//! Active scan, only when the user asks ("Scan subnet"): a TCP probe of a small port set plus
//! one ping per address of the current /24, a few hosts at a time and rate-limited, private
//! ranges only unless the caller explicitly allows otherwise. Open ports hint the device type.
use crate::model::{Layer, Observation, Service, Source};
use crate::probe::{Prober, Tcp};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// SSH, HTTP, HTTPS, SMB, RDP, VNC, alt-HTTP, raw printing, RTSP.
pub const PORTS: &[u16] = &[22, 80, 443, 445, 3389, 5900, 8080, 9100, 554];

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub concurrency: usize,
    pub timeout: Duration,
    /// Connection attempts per second, across all workers.
    pub rate: u32,
    pub ports: Vec<u16>,
    pub allow_public: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions { concurrency: 32, timeout: Duration::from_millis(600), rate: 400, ports: PORTS.to_vec(), allow_public: false }
    }
}

#[derive(Debug)]
pub enum ScanError {
    /// Refused: a public range without `allow_public`.
    NotPrivate,
    TooLarge,
}

/// Shared progress for the UI; `cancel` stops it between hosts.
#[derive(Default)]
pub struct ScanProgress {
    pub done: AtomicUsize,
    pub total: AtomicUsize,
    pub found: AtomicUsize,
    pub cancel: AtomicBool,
}

/// Spreads attempts evenly: at most `rate` per second overall.
struct Pacer {
    next: Mutex<Instant>,
    gap: Duration,
}

impl Pacer {
    fn wait(&self) {
        let at = {
            let mut n = self.next.lock().unwrap();
            let now = Instant::now();
            let at = (*n).max(now);
            *n = at + self.gap;
            at
        };
        let now = Instant::now();
        if at > now {
            std::thread::sleep(at - now);
        }
    }
}

/// Probe `hosts`; one observation per host that answered anything.
pub fn sweep(hosts: &[Ipv4Addr], opts: &ScanOptions, prober: Arc<dyn Prober>, progress: Arc<ScanProgress>) -> Result<Vec<Observation>, ScanError> {
    if hosts.len() > 4096 {
        return Err(ScanError::TooLarge);
    }
    if !opts.allow_public && hosts.iter().any(|h| !crate::cidr::is_private(IpAddr::V4(*h))) {
        return Err(ScanError::NotPrivate);
    }
    progress.total.store(hosts.len(), Ordering::Relaxed);
    let queue = Arc::new(Mutex::new(hosts.iter().rev().copied().collect::<Vec<_>>()));
    let results = Arc::new(Mutex::new(Vec::new()));
    let pacer = Arc::new(Pacer { next: Mutex::new(Instant::now()), gap: Duration::from_secs_f64(1. / opts.rate.max(1) as f64) });
    let workers: Vec<_> = (0..opts.concurrency.clamp(1, 128))
        .map(|_| {
            let (queue, results, pacer, prober, progress, opts) = (queue.clone(), results.clone(), pacer.clone(), prober.clone(), progress.clone(), opts.clone());
            std::thread::spawn(move || {
                loop {
                    if progress.cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let Some(host) = queue.lock().unwrap().pop() else { break };
                    if let Some(o) = probe_host(host, &opts, prober.as_ref(), &pacer, &progress) {
                        progress.found.fetch_add(1, Ordering::Relaxed);
                        results.lock().unwrap().push(o);
                    }
                    progress.done.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    let mut out = std::mem::take(&mut *results.lock().unwrap());
    out.sort_by_key(|o: &Observation| o.ips.first().copied());
    Ok(out)
}

fn probe_host(host: Ipv4Addr, opts: &ScanOptions, prober: &dyn Prober, pacer: &Pacer, progress: &ScanProgress) -> Option<Observation> {
    let ip = IpAddr::V4(host);
    pacer.wait();
    let ping = prober.ping(ip, opts.timeout);
    let mut open = Vec::new();
    let mut alive = ping.is_some();
    let mut best = ping;
    for &p in &opts.ports {
        if progress.cancel.load(Ordering::Relaxed) {
            return None;
        }
        pacer.wait();
        let (r, ms) = prober.tcp(ip, p, opts.timeout);
        match r {
            Tcp::Open => {
                open.push(p);
                alive = true;
                best = Some(best.map_or(ms, |b| b.min(ms)));
            }
            Tcp::Refused => {
                alive = true;
                best = Some(best.map_or(ms, |b| b.min(ms)));
            }
            Tcp::Silent => {}
        }
    }
    if !alive {
        return None;
    }
    let mut o = Observation::new(Layer::Lan, Source::Scan);
    o.ips.push(ip);
    o.online = Some(true);
    o.latency_ms = best;
    o.services = open.iter().map(|&p| Service { name: format!("tcp/{p}"), port: Some(p), label: None }).collect(); // i18n-ok: service id
    o.kind = crate::discover::classify::from_ports(&open);
    if open.contains(&80) || open.contains(&443) || open.contains(&8080) {
        let url = if open.contains(&443) { format!("https://{host}") } else if open.contains(&80) { format!("http://{host}") } else { format!("http://{host}:8080") };
        o.facts.push(("web_ui".into(), url));
    }
    Some(o)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// .1 is a router (80, 443), .5 answers ping only, .9 has SSH and refuses the rest, the
    /// rest is silent. Counts calls to check the rate limit is not bypassed.
    struct Fake {
        calls: AtomicUsize,
    }
    impl Prober for Fake {
        fn ping(&self, ip: IpAddr, _: Duration) -> Option<f64> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            (ip.to_string().ends_with(".5") || ip.to_string().ends_with(".1")).then_some(1.5)
        }
        fn tcp(&self, ip: IpAddr, port: u16, _: Duration) -> (Tcp, f64) {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let s = ip.to_string();
            if s.ends_with(".1") && (port == 80 || port == 443) {
                (Tcp::Open, 1.0)
            } else if s.ends_with(".9") {
                if port == 22 { (Tcp::Open, 3.0) } else { (Tcp::Refused, 2.0) }
            } else {
                (Tcp::Silent, 0.0)
            }
        }
    }

    #[test]
    fn sweep_finds_and_classifies() {
        let hosts = crate::cidr::Cidr::parse("192.168.7.0/28").unwrap().hosts(256);
        let fake = Arc::new(Fake { calls: AtomicUsize::new(0) });
        let progress = Arc::new(ScanProgress::default());
        let opts = ScanOptions { concurrency: 4, rate: 100_000, timeout: Duration::from_millis(1), ..Default::default() };
        let found = sweep(&hosts, &opts, fake.clone(), progress.clone()).unwrap();
        assert_eq!(found.iter().map(|o| o.ips[0].to_string()).collect::<Vec<_>>(), ["192.168.7.1", "192.168.7.5", "192.168.7.9"]);
        assert_eq!(found[0].services.len(), 2);
        assert!(found[0].facts.contains(&("web_ui".into(), "https://192.168.7.1".into())));
        assert_eq!(found[2].kind, Some(crate::model::DeviceKind::Server));
        assert_eq!(found[2].latency_ms, Some(2.0));
        assert_eq!(progress.done.load(Ordering::Relaxed), 14);
        assert_eq!(fake.calls.load(Ordering::Relaxed), 14 * (1 + PORTS.len()));
    }

    #[test]
    fn refuses_public_and_respects_cancel_and_rate() {
        let fake: Arc<dyn Prober> = Arc::new(Fake { calls: AtomicUsize::new(0) });
        let p = Arc::new(ScanProgress::default());
        assert!(matches!(sweep(&[Ipv4Addr::new(8, 8, 8, 8)], &ScanOptions::default(), fake.clone(), p.clone()), Err(ScanError::NotPrivate)));
        p.cancel.store(true, Ordering::Relaxed);
        let hosts = crate::cidr::Cidr::parse("10.0.0.0/24").unwrap().hosts(256);
        assert!(sweep(&hosts, &ScanOptions::default(), fake.clone(), p.clone()).unwrap().is_empty());
        // 20 attempts at 200/s take at least ~95 ms whatever the concurrency
        let p = Arc::new(ScanProgress::default());
        let opts = ScanOptions { concurrency: 16, rate: 200, ports: vec![22], timeout: Duration::from_millis(1), allow_public: false };
        let t = Instant::now();
        sweep(&hosts[..10], &opts, fake, p).unwrap();
        assert!(t.elapsed() >= Duration::from_millis(90), "rate limit holds: {:?}", t.elapsed());
    }
}
