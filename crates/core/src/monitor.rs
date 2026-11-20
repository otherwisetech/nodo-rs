//! Reachability monitoring. Two parts, so a menu-bar or background monitor can reuse them later:
//!
//! - [`Monitor`]: a pure state machine. Feed it probe results with a timestamp; it tracks
//!   up / down per device (two misses in a row before "down", so one lost packet doesn't
//!   flap), latency, uptime, last seen, and says when to alert (a saved device down for N
//!   minutes). No clock, no I/O: tested with synthetic results.
//! - [`Engine`]: a thread that probes the targets every interval with a [`Prober`] and sends
//!   the state machine's events over a channel. Pause / resume / retarget / stop through the
//!   handle. The app runs one while a map is open; nothing runs when the app is closed.
use crate::model::DeviceId;
use crate::probe::Prober;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Misses in a row before a device counts as down.
pub const MISSES_FOR_DOWN: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    pub interval: Duration,
    pub timeout: Duration,
    /// A saved device down this long raises an alert (once per outage).
    pub down_after: Duration,
    pub alert_down: bool,
    pub alert_new: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config { interval: Duration::from_secs(30), timeout: Duration::from_millis(1500), down_after: Duration::from_secs(5 * 60), alert_down: true, alert_new: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Unknown,
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub health: Health,
    /// When the current health began.
    pub since: i64,
    pub last_seen: Option<i64>,
    pub latency_ms: Option<f64>,
    pub misses: u32,
    pub checks: u64,
    pub answered: u64,
    alerted: bool,
}

impl State {
    fn new(now: i64) -> State {
        State { health: Health::Unknown, since: now, last_seen: None, latency_ms: None, misses: 0, checks: 0, answered: 0, alerted: false }
    }

    /// Share of checks answered this session, 0–1.
    pub fn uptime(&self) -> Option<f64> {
        (self.checks > 0).then(|| self.answered as f64 / self.checks as f64)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MonitorEvent {
    /// First answer, or back after being down (`down_for` seconds).
    Up { device: DeviceId, at: i64, latency_ms: f64, down_for: Option<i64> },
    Down { device: DeviceId, at: i64 },
    /// A saved device has been down `minutes`.
    AlertDown { device: DeviceId, at: i64, minutes: i64 },
    /// One probe round finished (for the UI to refresh latencies).
    Round { at: i64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub device: DeviceId,
    pub ip: IpAddr,
    /// Ports known to answer, tried when ping doesn't.
    pub ports: Vec<u16>,
    /// Saved devices raise "down" alerts; others are just tracked.
    pub saved: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Monitor {
    pub cfg: Config,
    states: HashMap<DeviceId, State>,
}

impl Monitor {
    pub fn new(cfg: Config) -> Monitor {
        Monitor { cfg, states: HashMap::new() }
    }

    pub fn state(&self, device: DeviceId) -> Option<&State> {
        self.states.get(&device)
    }

    pub fn states(&self) -> &HashMap<DeviceId, State> {
        &self.states
    }

    pub fn forget(&mut self, device: DeviceId) {
        self.states.remove(&device);
    }

    /// One probe result for `t` at `now`: `Some(ms)` answered, `None` missed.
    pub fn record(&mut self, t: &Target, result: Option<f64>, now: i64) -> Vec<MonitorEvent> {
        let st = self.states.entry(t.device).or_insert_with(|| State::new(now));
        st.checks += 1;
        let mut ev = Vec::new();
        match result {
            Some(ms) => {
                st.answered += 1;
                st.misses = 0;
                st.latency_ms = Some(ms);
                st.last_seen = Some(now);
                if st.health != Health::Up {
                    let down_for = (st.health == Health::Down).then(|| now - st.since);
                    st.health = Health::Up;
                    st.since = now;
                    st.alerted = false;
                    ev.push(MonitorEvent::Up { device: t.device, at: now, latency_ms: ms, down_for });
                }
            }
            None => {
                st.misses += 1;
                st.latency_ms = None;
                if st.health != Health::Down && st.misses >= MISSES_FOR_DOWN {
                    st.health = Health::Down;
                    // the outage began at the last answer, if we saw one
                    st.since = st.last_seen.unwrap_or(now);
                    ev.push(MonitorEvent::Down { device: t.device, at: now });
                }
                if st.health == Health::Down && t.saved && self.cfg.alert_down && !st.alerted && now - st.since >= self.cfg.down_after.as_secs() as i64 {
                    st.alerted = true;
                    ev.push(MonitorEvent::AlertDown { device: t.device, at: now, minutes: (now - st.since) / 60 });
                }
            }
        }
        ev
    }
}

/// Which newly discovered devices deserve a "new unknown device on the LAN" alert. The very
/// first discovery of a map (no LAN devices before) is a baseline, not a stream of alerts.
pub fn new_device_alerts(new_lan: &[DeviceId], lan_devices_before: usize, cfg: &Config) -> Vec<DeviceId> {
    if !cfg.alert_new || lan_devices_before == 0 {
        return Vec::new();
    }
    new_lan.to_vec()
}

enum Cmd {
    Targets(Vec<Target>),
    Config(Config),
    Stop,
}

/// Handle to a running [`Engine`]; dropping it stops the thread.
pub struct Engine {
    tx: Sender<Cmd>,
    paused: Arc<AtomicBool>,
    pub events: Receiver<MonitorEvent>,
    /// Latest per-device state, shared with the thread.
    pub snapshot: Arc<Mutex<HashMap<DeviceId, State>>>,
}

impl Engine {
    /// Start probing `targets` every `cfg.interval`, at most `concurrency` at a time.
    pub fn start(cfg: Config, targets: Vec<Target>, prober: Arc<dyn Prober>, concurrency: usize) -> Engine {
        let (tx, rx) = channel::<Cmd>();
        let (etx, erx) = channel::<MonitorEvent>();
        let paused = Arc::new(AtomicBool::new(false));
        let snapshot = Arc::new(Mutex::new(HashMap::new()));
        let (p, snap) = (paused.clone(), snapshot.clone());
        std::thread::spawn(move || run(cfg, targets, prober, concurrency.max(1), rx, etx, p, snap));
        Engine { tx, paused, events: erx, snapshot }
    }

    pub fn set_targets(&self, t: Vec<Target>) {
        let _ = self.tx.send(Cmd::Targets(t));
    }

    pub fn set_config(&self, c: Config) {
        let _ = self.tx.send(Cmd::Config(c));
    }

    pub fn pause(&self, on: bool) {
        self.paused.store(on, Ordering::Relaxed);
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

#[allow(clippy::too_many_arguments)]
fn run(mut cfg: Config, mut targets: Vec<Target>, prober: Arc<dyn Prober>, concurrency: usize, rx: Receiver<Cmd>, tx: Sender<MonitorEvent>, paused: Arc<AtomicBool>, snapshot: Arc<Mutex<HashMap<DeviceId, State>>>) {
    let mut mon = Monitor::new(cfg);
    loop {
        if !paused.load(Ordering::Relaxed) && !targets.is_empty() {
            let results = probe_all(&targets, &prober, cfg.timeout, concurrency);
            let now = crate::time::now();
            for (t, r) in targets.iter().zip(results) {
                for e in mon.record(t, r, now) {
                    if tx.send(e).is_err() {
                        return;
                    }
                }
            }
            *snapshot.lock().unwrap() = mon.states().clone();
            if tx.send(MonitorEvent::Round { at: now }).is_err() {
                return;
            }
        }
        // wait for the next round, reacting to commands meanwhile
        let deadline = std::time::Instant::now() + cfg.interval;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            match rx.recv_timeout(left) {
                Ok(Cmd::Stop) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                Ok(Cmd::Targets(t)) => {
                    let keep: Vec<DeviceId> = t.iter().map(|x| x.device).collect();
                    for d in targets.iter().map(|x| x.device).filter(|d| !keep.contains(d)).collect::<Vec<_>>() {
                        mon.forget(d);
                    }
                    targets = t;
                    // probe new targets right away
                    break;
                }
                Ok(Cmd::Config(c)) => {
                    cfg = c;
                    mon.cfg = c;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
            }
        }
    }
}

fn probe_all(targets: &[Target], prober: &Arc<dyn Prober>, timeout: Duration, concurrency: usize) -> Vec<Option<f64>> {
    let results = Arc::new(Mutex::new(vec![None; targets.len()]));
    let next = Arc::new(Mutex::new(0usize));
    let targets = Arc::new(targets.to_vec());
    let workers: Vec<_> = (0..concurrency.min(targets.len()))
        .map(|_| {
            let (results, next, targets, prober) = (results.clone(), next.clone(), targets.clone(), prober.clone());
            std::thread::spawn(move || {
                loop {
                    let i = {
                        let mut n = next.lock().unwrap();
                        let i = *n;
                        *n += 1;
                        i
                    };
                    let Some(t) = targets.get(i) else { break };
                    let r = prober.reach(t.ip, &t.ports, timeout);
                    results.lock().unwrap()[i] = r;
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    Arc::try_unwrap(results).map(|m| m.into_inner().unwrap()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::Tcp;

    fn target(device: DeviceId, saved: bool) -> Target {
        Target { device, ip: "192.168.1.10".parse().unwrap(), ports: vec![22], saved }
    }

    #[test]
    fn state_machine() {
        let cfg = Config { down_after: Duration::from_secs(300), ..Default::default() };
        let mut m = Monitor::new(cfg);
        let t = target(1, true);
        assert_eq!(m.record(&t, Some(2.0), 0), vec![MonitorEvent::Up { device: 1, at: 0, latency_ms: 2.0, down_for: None }]);
        assert!(m.record(&t, Some(3.0), 30).is_empty());
        // one miss is not an outage
        assert!(m.record(&t, None, 60).is_empty());
        assert_eq!(m.state(1).unwrap().health, Health::Up);
        assert_eq!(m.record(&t, None, 90), vec![MonitorEvent::Down { device: 1, at: 90 }]);
        assert_eq!(m.state(1).unwrap().since, 30, "the outage starts at the last answer");
        assert!(m.record(&t, None, 300).is_empty(), "270 s down: not yet");
        assert_eq!(m.record(&t, None, 330), vec![MonitorEvent::AlertDown { device: 1, at: 330, minutes: 5 }]);
        assert!(m.record(&t, None, 600).is_empty(), "alerts once per outage");
        assert_eq!(m.record(&t, Some(4.0), 630), vec![MonitorEvent::Up { device: 1, at: 630, latency_ms: 4.0, down_for: Some(600) }]);
        let s = m.state(1).unwrap();
        assert_eq!(s.checks, 8);
        assert_eq!(s.answered, 3);
        assert_eq!(s.last_seen, Some(630));
        assert!((s.uptime().unwrap() - 3. / 8.).abs() < 1e-9);
        // unsaved devices go down but never alert
        let u = target(2, false);
        m.record(&u, None, 0);
        m.record(&u, None, 30);
        assert!(m.record(&u, None, 10_000).is_empty());
        assert_eq!(m.state(2).unwrap().health, Health::Down);
        // a device never seen goes down after two misses, counted from then
        assert_eq!(m.state(2).unwrap().since, 30);
    }

    #[test]
    fn new_device_alert_policy() {
        let cfg = Config::default();
        assert!(new_device_alerts(&[5, 6], 0, &cfg).is_empty(), "first scan is the baseline");
        assert_eq!(new_device_alerts(&[5], 12, &cfg), vec![5]);
        assert!(new_device_alerts(&[5], 12, &Config { alert_new: false, ..cfg }).is_empty());
    }

    struct Flaky;
    impl Prober for Flaky {
        fn ping(&self, ip: IpAddr, _: Duration) -> Option<f64> {
            ip.to_string().ends_with(".1").then_some(1.0)
        }
        fn tcp(&self, _: IpAddr, _: u16, _: Duration) -> (Tcp, f64) {
            (Tcp::Silent, 0.)
        }
    }

    #[test]
    fn engine_rounds_pause_and_stop() {
        let cfg = Config { interval: Duration::from_millis(20), timeout: Duration::from_millis(5), ..Default::default() };
        let targets = vec![Target { device: 1, ip: "10.0.0.1".parse().unwrap(), ports: vec![], saved: true }, Target { device: 2, ip: "10.0.0.2".parse().unwrap(), ports: vec![], saved: true }];
        let e = Engine::start(cfg, targets, Arc::new(Flaky), 4);
        let mut ups = 0;
        let mut downs = 0;
        let mut rounds = 0;
        while rounds < 3 {
            match e.events.recv_timeout(Duration::from_secs(5)).expect("engine alive") {
                MonitorEvent::Up { device: 1, .. } => ups += 1,
                MonitorEvent::Down { device: 2, .. } => downs += 1,
                MonitorEvent::Round { .. } => rounds += 1,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!((ups, downs), (1, 1));
        assert_eq!(e.snapshot.lock().unwrap()[&1].health, Health::Up);
        e.pause(true);
        assert!(e.paused());
        while e.events.recv_timeout(Duration::from_millis(100)).is_ok() {}
        assert!(e.events.recv_timeout(Duration::from_millis(80)).is_err(), "no rounds while paused");
        e.stop();
    }
}
