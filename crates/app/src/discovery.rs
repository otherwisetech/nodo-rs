//! Running discovery off the UI thread and folding results into the open map; the monitor
//! engine while a map is open (events polled once a second); alerts in the app and as system
//! notifications.
use crate::app::NodoApp;
use crate::dialogs::Dialog;
use gpui::*;
use nodo_core::discover::netinfo::LocalNet;
use nodo_core::discover::scan::{ScanOptions, ScanProgress};
use nodo_core::model::{Device, DeviceId, Discovery, EventKind, Layer, Source};
use nodo_core::monitor::{Config, Engine, Health, MonitorEvent, State, Target};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// Passive refresh while a map is open (when the setting allows).
const AUTO_REFRESH: Duration = Duration::from_secs(5 * 60);
/// A device seen by passive discovery this recently counts as online.
pub const SEEN_FRESH: i64 = 15 * 60;
/// `last_seen` from the monitor is written to the file at most this often per device.
const SEEN_WRITE_EVERY: i64 = 10 * 60;

#[derive(Default)]
pub struct DiscState {
    pub passive: bool,
    pub scan: Option<Arc<ScanProgress>>,
    pub tailscale: bool,
    pub aws: bool,
    pub wg: bool,
    /// Problems from the last runs, per source (newest replaces older for the same source).
    pub notes: Vec<(Source, String)>,
    pub last_refresh: Option<i64>,
    pub local: Option<LocalNet>,
    /// This computer's interfaces, host name and Tailscale node (from the last refresh).
    pub me: Option<nodo_core::this_computer::LocalIdentity>,
    /// The last run of each CLI source, for the Sources panel.
    pub runs: Vec<(Source, SourceRun)>,
    /// Tailscale's CLI is installed (looked up once); it then runs with every refresh.
    pub tailscale_found: Option<bool>,
    auto_gen: u64,
}

/// "Tailscale: 12 devices · updated 10:42" or what went wrong.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceRun {
    pub at: i64,
    pub devices: usize,
    pub error: Option<String>,
}

impl SourceRun {
    pub fn of(d: &Discovery, src: Source, at: i64) -> SourceRun {
        let error = d.notes.iter().filter(|(s, _)| *s == src).map(|(_, n)| n.clone()).next();
        SourceRun { at, devices: d.devices.iter().filter(|o| o.source == Some(src)).count(), error }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AlertKind {
    Down { minutes: i64 },
    New,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    pub at: i64,
    pub device: DeviceId,
    pub kind: AlertKind,
    pub name: String,
}

#[derive(Default)]
pub struct MonState {
    pub engine: Option<Engine>,
    pub states: HashMap<DeviceId, State>,
    pub paused: bool,
    pub alerts: Vec<Alert>,
    loop_gen: u64,
    seen_written: HashMap<DeviceId, i64>,
}

/// What the UI shows for a device.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Status {
    Up(Option<f64>),
    Down,
    Offline,
    Unknown,
}

impl Status {
    pub fn online(self) -> bool {
        matches!(self, Status::Up(_))
    }
}

/// Monitor state first, then what the source says (Tailscale, AWS, WireGuard), then how
/// recently passive discovery saw it.
pub fn status_of(d: &Device, states: &HashMap<DeviceId, State>, now: i64) -> Status {
    if let Some(s) = states.get(&d.id) {
        match s.health {
            Health::Up => return Status::Up(s.latency_ms),
            Health::Down => return Status::Down,
            Health::Unknown => {}
        }
    }
    match d.online {
        Some(true) => Status::Up(None),
        Some(false) => Status::Offline,
        None if d.last_seen > 0 && now - d.last_seen < SEEN_FRESH => Status::Up(None),
        None if d.last_seen > 0 || d.saved => Status::Offline,
        None => Status::Unknown,
    }
}

/// Ports the monitor may try when ping gets no answer: what discovery found open, else the
/// usual ones.
pub fn probe_ports(d: &Device) -> Vec<u16> {
    let mut p: Vec<u16> = d.services.iter().filter(|s| s.name.starts_with("tcp/") || s.name.starts_with('_')).filter_map(|s| s.port).filter(|p| *p > 0).collect();
    p.sort();
    p.dedup();
    if p.is_empty() {
        p = vec![22, 80, 443];
    }
    p.truncate(3);
    p
}

/// System notification (macOS Notification Center, Linux notify-send); nothing on Windows.
pub fn notify(title: &str, body: &str) {
    if !crate::settings::Settings::load().notify() {
        return;
    }
    let (program, args): (&str, Vec<String>) = if cfg!(target_os = "macos") {
        ("osascript", vec!["-e".into(), format!("display notification {} with title {}", crate::launch::applescript_string(body), crate::launch::applescript_string(title))])
    } else if cfg!(target_os = "linux") {
        ("notify-send", vec![title.to_string(), body.to_string()])
    } else {
        return;
    };
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
    let _ = crate::launch::Launch { program: program.into(), args, cwd: home, copy: None }.spawn();
}

impl NodoApp {
    pub fn status(&self, d: &Device) -> Status {
        status_of(d, &self.mon.states, nodo_core::time::now())
    }

    fn record_run(&mut self, src: Source, d: &Discovery) {
        let run = SourceRun::of(d, src, nodo_core::time::now());
        self.disc.runs.retain(|(s, _)| *s != src);
        self.disc.runs.push((src, run));
        self.disc.runs.sort_by_key(|(s, _)| s.id());
    }

    fn note(&mut self, notes: Vec<(Source, String)>, ran: &[Source]) {
        self.disc.notes.retain(|(s, _)| !ran.contains(s));
        self.disc.notes.extend(notes);
    }

    /// Fold a discovery into the map; alerts for new LAN devices; retarget the monitor.
    pub fn apply(&mut self, d: Discovery, ran: &[Source], cx: &mut Context<Self>) {
        self.note(d.notes.clone(), ran);
        let Some(doc) = self.doc.as_mut() else { return };
        let now = nodo_core::time::now();
        match doc.apply(&d, now) {
            Ok(a) => {
                let cfg = self.monitor_config();
                let alerts = nodo_core::monitor::new_device_alerts(&a.new_lan, a.lan_before, &cfg);
                for id in alerts {
                    let name = self.doc.as_ref().and_then(|doc| doc.data.device(id)).map(|d| d.name.clone()).unwrap_or_default();
                    if let Some(doc) = self.doc.as_mut() {
                        let _ = doc.store.add_event(Some(id), now, EventKind::AlertNew, "");
                    }
                    self.mon.alerts.insert(0, Alert { at: now, device: id, kind: AlertKind::New, name: name.clone() });
                    notify(&i18n::t!("app.alert.new_title"), &i18n::t!("app.alert.new_body", name = name));
                }
                let n = a.report.new.len();
                if n > 0 && !ran.contains(&Source::Arp) {
                    self.toast(i18n::tn!("app.discover.found", n), cx);
                }
                self.retarget();
                self.touch(cx);
                if !self.view.fitted {
                    self.view.fitted = true;
                    self.fit();
                }
            }
            Err(e) => self.toast(i18n::t!("app.error.save", detail = e), cx),
        }
    }

    /// Passive sources: neighbour table, mDNS, SSDP.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.disc.passive || self.doc.is_none() {
            return;
        }
        self.disc.passive = true;
        let task = cx.background_executor().spawn(async move {
            let net = nodo_core::discover::netinfo::detect();
            let me = nodo_core::this_computer::detect();
            let d = nodo_core::discover::passive_with(Default::default(), net.as_ref(), &me);
            (net, me, d)
        });
        cx.spawn(async move |this, cx| {
            let (net, mut me, d) = task.await;
            this.update(cx, |this, cx| {
                this.disc.passive = false;
                this.disc.local = net;
                // the Tailscale side comes from the last `tailscale status`
                if let Some(old) = this.disc.me.take() {
                    me.tailscale_node = old.tailscale_node.clone();
                    for i in old.ifaces.into_iter().filter(|i| i.kind == nodo_core::this_computer::IfaceKind::Tailscale) {
                        if !me.ifaces.iter().any(|x| x.kind == i.kind) {
                            me.ifaces.push(i);
                        }
                    }
                }
                this.disc.me = Some(me);
                this.reidentify();
                this.disc.last_refresh = Some(nodo_core::time::now());
                this.apply(d, &[Source::Arp, Source::Mdns, Source::Ssdp], cx);
                // after the LAN devices are in, so tailnet nodes can join them
                this.auto_tailscale(cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Tailscale only reads the local daemon's status, so it rides along with every refresh
    /// (on open, ⌘R, every few minutes) once its CLI is found; problems go to the Sources
    /// panel, not toasts.
    fn auto_tailscale(&mut self, cx: &mut Context<Self>) {
        match self.disc.tailscale_found {
            Some(true) => self.discover_tailscale(false, cx),
            Some(false) => {}
            None => {
                self.disc.tailscale_found = Some(false);
                let task = cx.background_executor().spawn(async move { nodo_core::discover::tailscale::find_cli().is_some() });
                cx.spawn(async move |this, cx| {
                    let found = task.await;
                    this.update(cx, |this, cx| {
                        this.disc.tailscale_found = Some(found);
                        if found {
                            this.discover_tailscale(false, cx);
                        }
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    pub fn start_auto_refresh(&mut self, cx: &mut Context<Self>) {
        self.disc.auto_gen += 1;
        let generation = self.disc.auto_gen;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AUTO_REFRESH).await;
                let go = this.update(cx, |this, cx| {
                    if this.disc.auto_gen != generation || this.doc.is_none() {
                        return false;
                    }
                    if crate::settings::Settings::load().auto_refresh() {
                        this.refresh(cx);
                    }
                    true
                });
                if !matches!(go, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// "Scan subnet…": confirm first, with the range and the warning.
    pub fn ask_scan(&mut self, cx: &mut Context<Self>) {
        if self.doc.is_none() || self.disc.scan.is_some() {
            return;
        }
        let task = cx.background_executor().spawn(async move { nodo_core::discover::netinfo::detect() });
        cx.spawn(async move |this, cx| {
            let net = task.await;
            this.update(cx, |this, cx| {
                match net {
                    Some(n) => {
                        this.disc.local = Some(n.clone());
                        this.dialog = Some(Dialog::Scan(n));
                    }
                    None => this.toast(i18n::t!("app.scan.no_lan"), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn start_scan(&mut self, net: LocalNet, cx: &mut Context<Self>) {
        let progress = Arc::new(ScanProgress::default());
        self.disc.scan = Some(progress.clone());
        let p = progress.clone();
        let task = cx.background_executor().spawn(async move { nodo_core::discover::active(&net, &ScanOptions::default(), p) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            this.update(cx, |this, cx| {
                this.disc.scan = None;
                match r {
                    Ok(d) => {
                        let found = d.devices.iter().filter(|o| o.source == Some(Source::Scan)).count();
                        if let Some(doc) = this.doc.as_mut() {
                            let _ = doc.store.add_event(None, nodo_core::time::now(), EventKind::Scan, &found.to_string());
                        }
                        this.apply(d, &[Source::Scan], cx);
                        this.toast(i18n::tn!("app.scan.done", found), cx);
                    }
                    Err(nodo_core::discover::scan::ScanError::NotPrivate) => this.toast(i18n::t!("app.scan.not_private"), cx),
                    Err(nodo_core::discover::scan::ScanError::TooLarge) => this.toast(i18n::t!("app.scan.too_large"), cx),
                }
            })
            .ok();
        })
        .detach();
        // redraw the progress while it runs
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let running = this.update(cx, |this, cx| {
                    cx.notify();
                    this.disc.scan.is_some()
                });
                if !matches!(running, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    pub fn cancel_scan(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = &self.disc.scan {
            p.cancel.store(true, Ordering::Relaxed);
        }
        cx.notify();
    }

    /// `manual`: from the Sources menu, so problems also show as a toast.
    pub fn discover_tailscale(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.disc.tailscale || self.doc.is_none() {
            return;
        }
        self.disc.tailscale = true;
        let task = cx.background_executor().spawn(async move { nodo_core::discover::tailscale::discover() });
        cx.spawn(async move |this, cx| {
            let d = task.await;
            this.update(cx, |this, cx| {
                this.disc.tailscale = false;
                if !d.devices.is_empty() {
                    this.disc.tailscale_found = Some(true);
                }
                if manual && !d.notes.is_empty() {
                    let msg = d.notes[0].1.clone();
                    this.toast(msg, cx);
                }
                this.record_run(Source::Tailscale, &d);
                if let Some(me) = this.disc.me.as_mut() {
                    me.with_tailscale(&d);
                }
                this.reidentify();
                this.apply(d, &[Source::Tailscale], cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn run_aws(&mut self, profile: String, regions: Vec<String>, cx: &mut Context<Self>) {
        if self.disc.aws || self.doc.is_none() || profile.is_empty() {
            return;
        }
        if let Some(doc) = self.doc.as_mut() {
            let _ = doc.set_setting("aws_profile", &profile);
            let _ = doc.set_setting("aws_regions", &regions.join(","));
        }
        self.disc.aws = true;
        let task = cx.background_executor().spawn(async move {
            let regions = if regions.is_empty() { nodo_core::discover::aws::default_region(&profile).into_iter().collect() } else { regions };
            if regions.is_empty() {
                return Discovery { notes: vec![(Source::Aws, i18n::t!("app.aws.no_region"))], ..Default::default() };
            }
            nodo_core::discover::aws::discover(&profile, &regions)
        });
        cx.spawn(async move |this, cx| {
            let d = task.await;
            this.update(cx, |this, cx| {
                this.disc.aws = false;
                if !d.notes.is_empty() {
                    let msg = d.notes[0].1.clone();
                    this.toast(msg, cx);
                }
                this.record_run(Source::Aws, &d);
                this.apply(d, &[Source::Aws], cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn discover_wireguard(&mut self, cx: &mut Context<Self>) {
        if self.disc.wg || self.doc.is_none() {
            return;
        }
        self.disc.wg = true;
        let task = cx.background_executor().spawn(async move { nodo_core::discover::wireguard::discover(nodo_core::time::now()) });
        cx.spawn(async move |this, cx| {
            let d = task.await;
            this.update(cx, |this, cx| {
                this.disc.wg = false;
                if d.devices.is_empty() && d.networks.is_empty() {
                    let note = d.notes.first().map(|n| n.1.clone()).unwrap_or_else(|| i18n::t!("app.wg.none"));
                    this.dialog = Some(Dialog::WireGuard { note });
                }
                this.record_run(Source::WireGuard, &d);
                this.apply(d, &[Source::WireGuard], cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    // ── monitor ──────────────────────────────────────────────────────────────────────────────

    pub fn monitor_config(&self) -> Config {
        let get = |k: &str| self.doc.as_ref().and_then(|d| d.setting(k));
        let num = |k: &str, def: u64| get(k).and_then(|v| v.parse::<u64>().ok()).unwrap_or(def);
        Config {
            interval: Duration::from_secs(num("monitor_interval", 30).max(5)),
            timeout: Duration::from_millis(1500),
            down_after: Duration::from_secs(num("alert_down_minutes", 5) * 60),
            alert_down: get("alert_down").as_deref() != Some("off"),
            alert_new: get("alert_new").as_deref() != Some("off"),
        }
    }

    /// Devices watched: the ones marked for monitoring that have an address.
    pub fn targets(&self) -> Vec<Target> {
        let Some(doc) = &self.doc else { return Vec::new() };
        doc.data.devices.iter().filter(|d| d.monitor).filter_map(|d| Some(Target { device: d.id, ip: d.primary_ip()?, ports: probe_ports(d), saved: d.saved })).collect()
    }

    pub fn retarget(&mut self) {
        let t = self.targets();
        if let Some(e) = &self.mon.engine {
            e.set_targets(t);
        }
    }

    pub fn start_monitor(&mut self, cx: &mut Context<Self>) {
        self.stop_monitor();
        let engine = Engine::start(self.monitor_config(), self.targets(), Arc::new(nodo_core::probe::SystemProber), 16);
        engine.pause(self.mon.paused);
        self.mon.engine = Some(engine);
        self.mon.loop_gen += 1;
        let generation = self.mon.loop_gen;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let go = this.update(cx, |this, cx| {
                    if this.mon.loop_gen != generation {
                        return false;
                    }
                    this.drain_monitor(cx);
                    true
                });
                if !matches!(go, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    pub fn stop_monitor(&mut self) {
        if let Some(e) = self.mon.engine.take() {
            e.stop();
        }
        self.mon.loop_gen += 1;
        self.mon.states.clear();
    }

    pub fn toggle_monitor(&mut self, cx: &mut Context<Self>) {
        self.mon.paused = !self.mon.paused;
        if let Some(e) = &self.mon.engine {
            e.pause(self.mon.paused);
        }
        self.toast(if self.mon.paused { i18n::t!("app.monitor.paused_toast") } else { i18n::t!("app.monitor.resumed_toast") }, cx);
    }

    fn drain_monitor(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = &self.mon.engine else { return };
        let events: Vec<MonitorEvent> = engine.events.try_iter().collect();
        if events.is_empty() {
            return;
        }
        let snapshot = engine.snapshot.lock().map(|s| s.clone()).unwrap_or_default();
        let mut dirty = Vec::new();
        for e in events {
            let Some(doc) = self.doc.as_mut() else { return };
            match e {
                MonitorEvent::Up { device, at, latency_ms, down_for } => {
                    // the first answer of a session is not news; a recovery is
                    if let Some(secs) = down_for {
                        let _ = doc.store.add_event(Some(device), at, EventKind::Up, &i18n::t!("app.event.back_after", minutes = secs / 60, ms = format!("{latency_ms:.0}")));
                    }
                }
                MonitorEvent::Down { device, at } => {
                    let _ = doc.store.add_event(Some(device), at, EventKind::Down, "");
                }
                MonitorEvent::AlertDown { device, at, minutes } => {
                    let name = doc.data.device(device).map(|d| d.name.clone()).unwrap_or_default();
                    let _ = doc.store.add_event(Some(device), at, EventKind::AlertDown, &minutes.to_string());
                    self.mon.alerts.insert(0, crate::discovery::Alert { at, device, kind: AlertKind::Down { minutes }, name: name.clone() });
                    notify(&i18n::t!("app.alert.down_title"), &i18n::t!("app.alert.down_body", name = name, minutes = minutes));
                }
                MonitorEvent::Round { at } => {
                    for (id, st) in &snapshot {
                        if st.health == Health::Up
                            && let Some(d) = doc.data.device_mut(*id)
                        {
                            d.last_seen = d.last_seen.max(at);
                            let last = self.mon.seen_written.get(id).copied().unwrap_or(0);
                            if at - last >= SEEN_WRITE_EVERY {
                                self.mon.seen_written.insert(*id, at);
                                dirty.push(*id);
                            }
                        }
                    }
                }
            }
        }
        if let Some(doc) = self.doc.as_mut() {
            for id in dirty {
                if let Some(d) = doc.data.device(id).cloned() {
                    let _ = doc.store.save_device(&d);
                }
            }
        }
        self.mon.alerts.truncate(50);
        self.mon.states = snapshot;
        cx.notify();
    }

    /// "Monitoring · 12 devices" / "Paused" in the footer.
    pub fn monitor_status(&self, t: &crate::theme::Tokens) -> Option<Div> {
        self.doc.as_ref()?;
        let n = self.targets().len();
        let down = self.mon.states.values().filter(|s| s.health == Health::Down).count();
        let text = if self.mon.paused { i18n::t!("app.monitor.paused") } else { i18n::tn!("app.monitor.watching", n) };
        let color = if self.mon.paused { t.tag } else if down > 0 { t.danger } else { t.up };
        Some(div().flex().flex_none().items_center().gap(px(5.)).child(div().size(px(7.)).rounded_full().bg(color)).child(text).when(down > 0 && !self.mon.paused, |d| d.child(div().text_color(t.danger).child(i18n::tn!("app.monitor.down_count", down)))))
    }

    /// Devices per layer, for the layer chips.
    pub fn layer_counts(&self) -> HashMap<Layer, usize> {
        let mut m = HashMap::new();
        if let Some(doc) = &self.doc {
            for d in &doc.data.devices {
                for l in d.layers() {
                    *m.entry(l).or_insert(0) += 1;
                }
            }
        }
        m
    }
}

use gpui::prelude::FluentBuilder as _;

#[cfg(test)]
mod tests {
    // not `super::*`: gpui's glob exports its own `test` attribute
    use super::{SEEN_FRESH, Status, probe_ports, status_of};
    use nodo_core::model::{Device, Service};
    use nodo_core::monitor::{Config, Target};
    use std::collections::HashMap;

    #[test]
    fn status_rules() {
        let now = 10_000;
        let mut d = Device { id: 1, last_seen: now - 60, ..Default::default() };
        let mut states = HashMap::new();
        assert_eq!(status_of(&d, &states, now), Status::Up(None), "seen a minute ago");
        d.last_seen = now - SEEN_FRESH - 1;
        assert_eq!(status_of(&d, &states, now), Status::Offline);
        d.online = Some(true);
        assert_eq!(status_of(&d, &states, now), Status::Up(None), "the source says it is up");
        let mut m = nodo_core::monitor::Monitor::new(Config::default());
        let t = Target { device: 1, ip: "10.0.0.1".parse().unwrap(), ports: vec![], saved: true };
        m.record(&t, None, now);
        m.record(&t, None, now + 30);
        states = m.states().clone();
        assert_eq!(status_of(&d, &states, now), Status::Down, "the monitor wins");
        assert_eq!(status_of(&Device::default(), &HashMap::new(), now), Status::Unknown);
        assert_eq!(status_of(&Device { saved: true, ..Default::default() }, &HashMap::new(), now), Status::Offline);
    }

    #[test]
    fn ports_to_probe() {
        let d = Device { services: vec![Service { name: "tcp/443".into(), port: Some(443), label: None }, Service { name: "_ssh._tcp".into(), port: Some(22), label: None }, Service { name: "urn:x".into(), port: None, label: None }], ..Default::default() };
        assert_eq!(probe_ports(&d), vec![22, 443]);
        assert_eq!(probe_ports(&Device::default()), vec![22, 80, 443]);
    }
}
