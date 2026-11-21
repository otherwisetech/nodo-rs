//! Uptime monitoring of services, not only devices: HTTP(S) (status, latency, keyword, TLS
//! certificate expiry), TCP ports, DNS resolution and ICMP. A service may belong to a device
//! or stand alone. Like device monitoring it runs only while a map is open:
//!
//! - [`check`]: one check; [`evaluate`] is the pure part (status, keyword, slow, certificate).
//! - [`Tracker`]: pure state machine (two failures in a row before "down"; slow and
//!   certificate warnings once per episode) producing [`SvcEvent`]s.
//! - [`Hour`] buckets: what the map file keeps (checks, ups, latency per hour), for uptime over
//!   24 h / 7 d / 30 d and the latency sparkline; incidents are kept as rows with start / end.
//! - [`Engine`]: the thread that runs each service at its own interval.
use crate::model::DeviceId;
use crate::probe::Prober;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type ServiceId = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckKind {
    Http,
    Tcp,
    Dns,
    Icmp,
}

impl CheckKind {
    pub const ALL: [CheckKind; 4] = [CheckKind::Http, CheckKind::Tcp, CheckKind::Dns, CheckKind::Icmp];

    pub fn id(self) -> &'static str {
        match self {
            CheckKind::Http => "http",
            CheckKind::Tcp => "tcp",
            CheckKind::Dns => "dns",
            CheckKind::Icmp => "icmp",
        }
    }

    pub fn from_id(s: &str) -> Option<CheckKind> {
        CheckKind::ALL.into_iter().find(|k| k.id() == s.to_lowercase() || (s.eq_ignore_ascii_case("https") && *k == CheckKind::Http) || (s.eq_ignore_ascii_case("ping") && *k == CheckKind::Icmp))
    }
}

fn d_interval() -> u32 {
    60
}
fn d_timeout() -> u32 {
    8000
}

/// What to check and when it counts as a problem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spec {
    pub kind: CheckKind,
    /// URL (http / https), `host:port` (tcp), a host name (dns, icmp).
    pub target: String,
    #[serde(default = "d_interval")]
    pub interval_s: u32,
    #[serde(default = "d_timeout")]
    pub timeout_ms: u32,
    /// HTTP: text the body must contain. DNS: an address the name must resolve to.
    #[serde(default)]
    pub keyword: Option<String>,
    /// HTTP: the exact status expected (else any 2xx / 3xx).
    #[serde(default)]
    pub expect_status: Option<u16>,
    /// Slower than this is "slow" (alert once per episode).
    #[serde(default)]
    pub slow_ms: Option<u32>,
    /// HTTPS: warn when the certificate expires within this many days.
    #[serde(default)]
    pub cert_days: Option<u32>,
}

impl Spec {
    pub fn new(kind: CheckKind, target: &str) -> Spec {
        Spec { kind, target: target.trim().to_string(), interval_s: d_interval(), timeout_ms: d_timeout(), keyword: None, expect_status: None, slow_ms: None, cert_days: (kind == CheckKind::Http && target.trim().to_lowercase().starts_with("https://")).then_some(14) }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceCheck {
    pub id: ServiceId,
    pub name: String,
    #[serde(default)]
    pub device: Option<DeviceId>,
    pub spec: Spec,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

/// Ready-made services for websites and common ports.
pub fn presets() -> Vec<(&'static str, Spec)> {
    let mut site = Spec::new(CheckKind::Http, "https://example.com");
    site.slow_ms = Some(2000);
    let mut api = Spec::new(CheckKind::Http, "https://example.com/health");
    api.expect_status = Some(200);
    api.interval_s = 30;
    let mut kw = Spec::new(CheckKind::Http, "https://example.com");
    kw.keyword = Some("Example Domain".into());
    vec![("website", site), ("api", api), ("keyword", kw), ("ssh", Spec::new(CheckKind::Tcp, "host:22")), ("dns", Spec::new(CheckKind::Dns, "example.com")), ("ping", Spec::new(CheckKind::Icmp, "1.1.1.1"))]
}

/// One check's result.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Outcome {
    pub ok: bool,
    pub latency_ms: Option<f64>,
    pub status: Option<u16>,
    pub cert_days: Option<i64>,
    /// Why it failed (or what it found), in plain words.
    pub detail: String,
    /// Answered, but slower than `slow_ms`.
    pub slow: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

pub fn parse_url(s: &str) -> Option<Url> {
    let s = s.trim();
    let (tls, rest) = if let Some(r) = s.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = s.strip_prefix("http://") {
        (false, r)
    } else {
        (true, s)
    };
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    let (host, port) = if let Some(h) = hostport.strip_prefix('[') {
        let (h, p) = h.split_once(']')?;
        (h.to_string(), p.strip_prefix(':').and_then(|p| p.parse().ok()))
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), Some(p.parse().ok()?)),
            None => (hostport.to_string(), None),
        }
    };
    if host.is_empty() || host.contains(char::is_whitespace) {
        return None;
    }
    Some(Url { tls, host, port: port.unwrap_or(if tls { 443 } else { 80 }), path })
}

/// The pure part of an HTTP check.
pub fn evaluate(spec: &Spec, status: u16, body: &[u8], latency_ms: f64, cert_days: Option<i64>) -> Outcome {
    let mut o = Outcome { latency_ms: Some(latency_ms), status: Some(status), cert_days, ..Default::default() };
    let status_ok = match spec.expect_status {
        Some(s) => status == s,
        None => (200..400).contains(&status),
    };
    let kw_ok = spec.keyword.as_deref().filter(|k| !k.is_empty()).is_none_or(|k| body.windows(k.len()).any(|w| w == k.as_bytes()));
    o.ok = status_ok && kw_ok;
    o.detail = if !status_ok {
        i18n::t!("core.svc.status", status = status)
    } else if !kw_ok {
        i18n::t!("core.svc.keyword_missing", keyword = spec.keyword.clone().unwrap_or_default())
    } else {
        format!("HTTP {status}") // i18n-ok: protocol
    };
    o.slow = o.ok && spec.slow_ms.is_some_and(|s| latency_ms > s as f64);
    o
}

// ── TLS certificate expiry (a few lines of DER instead of an X.509 crate) ─────────────────

fn der(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = b.split_first()?;
    let (&l0, mut rest) = rest.split_first()?;
    let len = if l0 < 0x80 {
        l0 as usize
    } else {
        let n = (l0 & 0x7f) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let v = rest[..n].iter().fold(0usize, |a, &x| (a << 8) | x as usize);
        rest = &rest[n..];
        v
    };
    if rest.len() < len {
        return None;
    }
    Some((tag, &rest[..len], &rest[len..]))
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// UTCTime (`YYMMDDHHMMSSZ`) or GeneralizedTime (`YYYYMMDDHHMMSSZ`) → unix seconds.
pub fn parse_asn1_time(tag: u8, s: &[u8]) -> Option<i64> {
    let s = std::str::from_utf8(s).ok()?.trim_end_matches('Z');
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, off) = match tag {
        0x17 => {
            let yy = num(0, 2)?;
            (if yy < 50 { 2000 + yy } else { 1900 + yy }, 2)
        }
        0x18 => (num(0, 4)?, 4),
        _ => return None,
    };
    let (mo, d, h, mi) = (num(off, off + 2)?, num(off + 2, off + 4)?, num(off + 4, off + 6)?, num(off + 6, off + 8)?);
    let sec = num(off + 8, off + 10).unwrap_or(0);
    Some(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + sec)
}

/// A DER certificate's `notAfter` as unix seconds.
pub fn cert_not_after(cert: &[u8]) -> Option<i64> {
    let (_, c, _) = der(cert)?; // Certificate
    let (_, tbs, _) = der(c)?; // TBSCertificate
    let mut rest = tbs;
    let (tag, _, r) = der(rest)?;
    if tag == 0xa0 {
        rest = r; // [0] version
    }
    let (_, _, r) = der(rest)?; // serial
    let (_, _, r) = der(r)?; // signature algorithm
    let (_, _, r) = der(r)?; // issuer
    let (_, validity, _) = der(r)?;
    let (_, _, r) = der(validity)?; // notBefore
    let (tag, t, _) = der(r)?;
    parse_asn1_time(tag, t)
}

// ── running a check ───────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct Capture {
    inner: Arc<rustls::client::WebPkiServerVerifier>,
    leaf: Mutex<Option<Vec<u8>>>,
}

impl rustls::client::danger::ServerCertVerifier for Capture {
    fn verify_server_cert(&self, end: &rustls::pki_types::CertificateDer<'_>, inter: &[rustls::pki_types::CertificateDer<'_>], name: &rustls::pki_types::ServerName<'_>, ocsp: &[u8], now: rustls::pki_types::UnixTime) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if let Ok(mut l) = self.leaf.lock() {
            *l = Some(end.to_vec());
        }
        self.inner.verify_server_cert(end, inter, name, ocsp, now)
    }

    fn verify_tls12_signature(&self, m: &[u8], c: &rustls::pki_types::CertificateDer<'_>, d: &rustls::DigitallySignedStruct) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(m, c, d)
    }

    fn verify_tls13_signature(&self, m: &[u8], c: &rustls::pki_types::CertificateDer<'_>, d: &rustls::DigitallySignedStruct) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(m, c, d)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

const BODY_MAX: usize = 512 * 1024;

fn connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, String> {
    let addrs: Vec<SocketAddr> = (host, port).to_socket_addrs().map_err(|e| i18n::t!("core.svc.resolve", detail = e.to_string()))?.collect();
    let mut last = i18n::t!("core.svc.no_address");
    for a in addrs {
        match TcpStream::connect_timeout(&a, timeout) {
            Ok(s) => {
                let _ = s.set_read_timeout(Some(timeout));
                let _ = s.set_write_timeout(Some(timeout));
                return Ok(s);
            }
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

fn read_response(r: &mut impl Read) -> Result<(u16, Vec<u8>), String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        match r.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() >= BODY_MAX {
                    break;
                }
            }
            // a TLS peer that closes without close_notify, after sending everything
            Err(e) if !buf.is_empty() && matches!(e.kind(), std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::ConnectionReset) => break,
            Err(e) if !buf.is_empty() && matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => break,
            Err(e) => return Err(e.to_string()),
        }
    }
    let line = buf.split(|b| *b == b'\n').next().unwrap_or_default();
    let status = std::str::from_utf8(line).ok().and_then(|l| l.split_whitespace().nth(1)).and_then(|s| s.parse().ok()).ok_or_else(|| i18n::t!("core.svc.not_http"))?;
    let body = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| buf[i + 4..].to_vec()).unwrap_or_default();
    Ok((status, body))
}

fn http(spec: &Spec, now: i64) -> Outcome {
    let Some(u) = parse_url(&spec.target) else { return fail(i18n::t!("core.svc.bad_url")) };
    let timeout = Duration::from_millis(spec.timeout_ms.max(500) as u64);
    let start = Instant::now();
    let tcp = match connect(&u.host, u.port, timeout) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let req = format!("GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: Nodo/{}\r\nAccept: */*\r\nConnection: close\r\n\r\n", u.path, u.host, env!("CARGO_PKG_VERSION")); // i18n-ok: protocol
    let (res, cert) = if u.tls {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let roots = Arc::new(rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() });
        let inner = match rustls::client::WebPkiServerVerifier::builder_with_provider(roots, provider.clone()).build() {
            Ok(v) => v,
            Err(e) => return fail(e.to_string()),
        };
        let cap = Arc::new(Capture { inner, leaf: Mutex::new(None) });
        let cfg = match rustls::ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions() {
            Ok(b) => b.dangerous().with_custom_certificate_verifier(cap.clone()).with_no_client_auth(),
            Err(e) => return fail(e.to_string()),
        };
        let Ok(name) = rustls::pki_types::ServerName::try_from(u.host.clone()) else { return fail(i18n::t!("core.svc.bad_url")) };
        let conn = match rustls::ClientConnection::new(Arc::new(cfg), name) {
            Ok(c) => c,
            Err(e) => return fail(e.to_string()),
        };
        let mut s = rustls::StreamOwned::new(conn, tcp);
        let r = s.write_all(req.as_bytes()).map_err(|e| e.to_string()).and_then(|_| read_response(&mut s));
        let leaf = cap.leaf.lock().ok().and_then(|l| l.clone());
        (r, leaf)
    } else {
        let mut s = tcp;
        (s.write_all(req.as_bytes()).map_err(|e| e.to_string()).and_then(|_| read_response(&mut s)), None)
    };
    let ms = start.elapsed().as_secs_f64() * 1000.;
    let cert_days = cert.as_deref().and_then(cert_not_after).map(|t| (t - now).div_euclid(86400));
    match res {
        Ok((status, body)) => evaluate(spec, status, &body, ms, cert_days),
        Err(e) => {
            let mut o = fail(match cert_days {
                Some(d) if d < 0 => i18n::t!("core.svc.cert_expired", days = -d),
                _ => e,
            });
            o.cert_days = cert_days;
            o
        }
    }
}

fn fail(detail: String) -> Outcome {
    Outcome { ok: false, detail, ..Default::default() }
}

fn host_port(t: &str) -> Option<(String, u16)> {
    let t = t.trim();
    if let Some(h) = t.strip_prefix('[') {
        let (h, p) = h.split_once("]:")?;
        return Some((h.to_string(), p.parse().ok()?));
    }
    let (h, p) = t.rsplit_once(':')?;
    Some((h.to_string(), p.parse().ok()?))
}

/// Run one check now. Blocking, at most about the spec's timeout.
pub fn check(spec: &Spec, prober: &dyn Prober) -> Outcome {
    let timeout = Duration::from_millis(spec.timeout_ms.max(500) as u64);
    let slow = |mut o: Outcome| {
        o.slow = o.ok && o.latency_ms.zip(spec.slow_ms).is_some_and(|(l, s)| l > s as f64);
        o
    };
    match spec.kind {
        CheckKind::Http => http(spec, crate::time::now()),
        CheckKind::Tcp => {
            let Some((h, p)) = host_port(&spec.target) else { return fail(i18n::t!("core.svc.bad_target")) };
            let start = Instant::now();
            match connect(&h, p, timeout) {
                Ok(_) => slow(Outcome { ok: true, latency_ms: Some(start.elapsed().as_secs_f64() * 1000.), detail: i18n::t!("core.svc.open", port = p), ..Default::default() }),
                Err(e) => fail(e),
            }
        }
        CheckKind::Dns => {
            let start = Instant::now();
            match (spec.target.trim(), 0).to_socket_addrs() {
                Ok(a) => {
                    let ips: Vec<IpAddr> = a.map(|s| s.ip()).collect();
                    let ms = start.elapsed().as_secs_f64() * 1000.;
                    let want = spec.keyword.as_deref().map(str::trim).filter(|k| !k.is_empty());
                    let ok = !ips.is_empty() && want.is_none_or(|w| ips.iter().any(|ip| ip.to_string() == w));
                    let list = ips.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
                    slow(Outcome { ok, latency_ms: Some(ms), detail: if ok { list } else { i18n::t!("core.svc.dns_mismatch", got = list) }, ..Default::default() })
                }
                Err(e) => fail(i18n::t!("core.svc.resolve", detail = e.to_string())),
            }
        }
        CheckKind::Icmp => {
            let ip = spec.target.trim().parse::<IpAddr>().ok().or_else(|| (spec.target.trim(), 0).to_socket_addrs().ok()?.next().map(|s| s.ip()));
            let Some(ip) = ip else { return fail(i18n::t!("core.svc.no_address")) };
            match prober.ping(ip, timeout) {
                Some(ms) => slow(Outcome { ok: true, latency_ms: Some(ms), detail: ip.to_string(), ..Default::default() }),
                None => fail(i18n::t!("core.svc.no_reply")),
            }
        }
    }
}

// ── state, alerts, history ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Health {
    Unknown,
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SvcState {
    pub health: Health,
    pub since: i64,
    pub misses: u32,
    pub last: Option<Outcome>,
    pub last_at: Option<i64>,
    /// Recent latencies (None = failed), newest last, for the sparkline.
    pub recent: VecDeque<Option<f64>>,
    slow_alerted: bool,
    cert_alerted: bool,
}

pub const RECENT: usize = 40;

#[derive(Debug, Clone, PartialEq)]
pub enum SvcEvent {
    Checked { service: ServiceId, at: i64, outcome: Outcome },
    Down { service: ServiceId, at: i64, detail: String },
    Up { service: ServiceId, at: i64, down_for: Option<i64> },
    Slow { service: ServiceId, at: i64, ms: f64 },
    CertExpiring { service: ServiceId, at: i64, days: i64 },
}

#[derive(Debug, Default)]
pub struct Tracker {
    pub states: HashMap<ServiceId, SvcState>,
}

impl Tracker {
    pub fn record(&mut self, s: &ServiceCheck, o: Outcome, now: i64) -> Vec<SvcEvent> {
        let st = self.states.entry(s.id).or_insert(SvcState { health: Health::Unknown, since: now, misses: 0, last: None, last_at: None, recent: VecDeque::new(), slow_alerted: false, cert_alerted: false });
        let mut ev = Vec::new();
        st.recent.push_back(if o.ok { o.latency_ms } else { None });
        while st.recent.len() > RECENT {
            st.recent.pop_front();
        }
        if o.ok {
            st.misses = 0;
            if st.health == Health::Down {
                ev.push(SvcEvent::Up { service: s.id, at: now, down_for: Some(now - st.since) });
            }
            if st.health != Health::Up {
                st.health = Health::Up;
                st.since = now;
            }
            if o.slow && !st.slow_alerted {
                st.slow_alerted = true;
                ev.push(SvcEvent::Slow { service: s.id, at: now, ms: o.latency_ms.unwrap_or_default() });
            } else if !o.slow {
                st.slow_alerted = false;
            }
        } else {
            st.misses += 1;
            // one failure isn't an outage; the first check of a session that fails is
            if st.health != Health::Down && (st.misses >= crate::monitor::MISSES_FOR_DOWN || st.health == Health::Unknown) {
                st.health = Health::Down;
                st.since = now;
                ev.push(SvcEvent::Down { service: s.id, at: now, detail: o.detail.clone() });
            }
        }
        match (o.cert_days, s.spec.cert_days) {
            (Some(d), Some(warn)) if d <= warn as i64 => {
                if !st.cert_alerted {
                    st.cert_alerted = true;
                    ev.push(SvcEvent::CertExpiring { service: s.id, at: now, days: d });
                }
            }
            (Some(_), _) => st.cert_alerted = false,
            _ => {}
        }
        st.last = Some(o.clone());
        st.last_at = Some(now);
        ev.insert(0, SvcEvent::Checked { service: s.id, at: now, outcome: o });
        ev
    }
}

/// Checks per hour: what the map file keeps.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Hour {
    pub hour: i64,
    pub checks: u32,
    pub ups: u32,
    pub latency_sum: f64,
    pub latency_n: u32,
}

impl Hour {
    pub fn of(at: i64) -> i64 {
        at.div_euclid(3600) * 3600
    }

    pub fn add(&mut self, o: &Outcome) {
        self.checks += 1;
        if o.ok {
            self.ups += 1;
            if let Some(l) = o.latency_ms {
                self.latency_sum += l;
                self.latency_n += 1;
            }
        }
    }

    pub fn latency(&self) -> Option<f64> {
        (self.latency_n > 0).then(|| self.latency_sum / self.latency_n as f64)
    }
}

/// Share of checks that passed in the last `window` seconds, 0–1.
pub fn uptime(hours: &[Hour], now: i64, window: i64) -> Option<f64> {
    let from = Hour::of(now - window);
    let (c, u) = hours.iter().filter(|h| h.hour >= from).fold((0u64, 0u64), |(c, u), h| (c + h.checks as u64, u + h.ups as u64));
    (c > 0).then(|| u as f64 / c as f64)
}

pub const DAY: i64 = 86400;

/// Fold same-hour buckets together (what's in the file plus what's still in memory).
pub fn merge_hours(a: &[Hour], b: &[Hour]) -> Vec<Hour> {
    let mut m: Vec<Hour> = a.to_vec();
    for h in b {
        match m.iter_mut().find(|x| x.hour == h.hour) {
            Some(x) => {
                x.checks += h.checks;
                x.ups += h.ups;
                x.latency_sum += h.latency_sum;
                x.latency_n += h.latency_n;
            }
            None => m.push(*h),
        }
    }
    m.sort_by_key(|h| h.hour);
    m
}

// ── engine ────────────────────────────────────────────────────────────────────────────────

enum Cmd {
    Checks(Vec<ServiceCheck>),
    Now(ServiceId),
    Stop,
}

/// Runs each enabled service at its own interval on worker threads; events over a channel.
pub struct Engine {
    tx: Sender<Cmd>,
    paused: Arc<AtomicBool>,
    pub events: Receiver<SvcEvent>,
    pub states: Arc<Mutex<HashMap<ServiceId, SvcState>>>,
}

impl Engine {
    pub fn start(checks: Vec<ServiceCheck>, prober: Arc<dyn Prober>) -> Engine {
        let (tx, rx) = channel();
        let (etx, erx) = channel();
        let paused = Arc::new(AtomicBool::new(false));
        let states = Arc::new(Mutex::new(HashMap::new()));
        let (p, st) = (paused.clone(), states.clone());
        std::thread::spawn(move || run(checks, prober, rx, etx, p, st));
        Engine { tx, paused, events: erx, states }
    }

    pub fn set_checks(&self, c: Vec<ServiceCheck>) {
        let _ = self.tx.send(Cmd::Checks(c));
    }

    pub fn check_now(&self, id: ServiceId) {
        let _ = self.tx.send(Cmd::Now(id));
    }

    pub fn pause(&self, on: bool) {
        self.paused.store(on, Ordering::Relaxed);
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

const WORKERS: usize = 8;

fn run(mut checks: Vec<ServiceCheck>, prober: Arc<dyn Prober>, rx: Receiver<Cmd>, tx: Sender<SvcEvent>, paused: Arc<AtomicBool>, states: Arc<Mutex<HashMap<ServiceId, SvcState>>>) {
    let mut tracker = Tracker::default();
    let mut due: HashMap<ServiceId, Instant> = HashMap::new();
    let mut running: HashMap<ServiceId, ()> = HashMap::new();
    let (rtx, rrx) = channel::<(ServiceId, Outcome)>();
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Cmd::Stop) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            Ok(Cmd::Checks(c)) => {
                let ids: Vec<ServiceId> = c.iter().map(|x| x.id).collect();
                tracker.states.retain(|k, _| ids.contains(k));
                due.retain(|k, _| ids.contains(k));
                checks = c;
            }
            Ok(Cmd::Now(id)) => {
                due.insert(id, Instant::now());
            }
            Err(_) => {}
        }
        while let Ok((id, o)) = rrx.try_recv() {
            running.remove(&id);
            let Some(s) = checks.iter().find(|c| c.id == id) else { continue };
            for e in tracker.record(s, o, crate::time::now()) {
                if tx.send(e).is_err() {
                    return;
                }
            }
            if let Ok(mut st) = states.lock() {
                *st = tracker.states.clone();
            }
        }
        if paused.load(Ordering::Relaxed) {
            continue;
        }
        let now = Instant::now();
        for s in checks.iter().filter(|c| c.enabled) {
            if running.len() >= WORKERS {
                break;
            }
            if running.contains_key(&s.id) || due.get(&s.id).is_some_and(|t| *t > now) {
                continue;
            }
            due.insert(s.id, now + Duration::from_secs(s.spec.interval_s.clamp(10, 86400) as u64));
            running.insert(s.id, ());
            let (spec, id, p, rtx) = (s.spec.clone(), s.id, prober.clone(), rtx.clone());
            std::thread::spawn(move || {
                let _ = rtx.send((id, check(&spec, &*p)));
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// openssl req -x509 … -not_before 20260101000000Z -not_after 20270115120000Z (P-256)
    const CERT_HEX: &str = include_str!("../tests/fixtures/cert-2027-01-15.hex");

    fn unhex(s: &str) -> Vec<u8> {
        let s = s.trim();
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap()).collect()
    }

    #[test]
    fn certificate_expiry() {
        assert_eq!(cert_not_after(&unhex(CERT_HEX)), Some(1_800_014_400));
        assert_eq!(parse_asn1_time(0x17, b"270115120000Z"), Some(1_800_014_400));
        assert_eq!(parse_asn1_time(0x18, b"20260101000000Z"), Some(1_767_225_600));
        assert_eq!(parse_asn1_time(0x17, b"991231235959Z"), Some(946_684_799));
        assert_eq!(cert_not_after(b"\x30\x03\x02\x01"), None, "truncated");
        assert_eq!(cert_not_after(&[]), None);
    }

    #[test]
    fn urls_and_evaluation() {
        assert_eq!(parse_url("https://example.com"), Some(Url { tls: true, host: "example.com".into(), port: 443, path: "/".into() }));
        assert_eq!(parse_url("http://10.0.0.2:8080/health?x=1"), Some(Url { tls: false, host: "10.0.0.2".into(), port: 8080, path: "/health?x=1".into() }));
        assert_eq!(parse_url("http://[fd00::1]:81/").map(|u| (u.host, u.port)), Some(("fd00::1".into(), 81)));
        assert_eq!(parse_url("example.org/x").map(|u| u.tls), Some(true));
        assert_eq!(parse_url("http://a b/"), None);
        let mut s = Spec::new(CheckKind::Http, "https://x");
        assert_eq!(s.cert_days, Some(14));
        assert!(evaluate(&s, 200, b"", 10., None).ok);
        assert!(evaluate(&s, 301, b"", 10., None).ok);
        assert!(!evaluate(&s, 503, b"", 10., None).ok);
        s.expect_status = Some(204);
        assert!(!evaluate(&s, 200, b"", 10., None).ok);
        s.expect_status = None;
        s.keyword = Some("healthy".into());
        assert!(evaluate(&s, 200, b"{\"status\":\"healthy\"}", 10., None).ok);
        assert!(!evaluate(&s, 200, b"degraded", 10., None).ok);
        s.slow_ms = Some(100);
        assert!(evaluate(&s, 200, b"healthy", 250., None).slow);
        assert_eq!(CheckKind::from_id("https"), Some(CheckKind::Http));
        assert_eq!(CheckKind::from_id("ping"), Some(CheckKind::Icmp));
        assert!(presets().iter().all(|(_, p)| p.interval_s >= 10));
    }

    fn serve(responses: Vec<&'static str>) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for r in responses {
                let (mut c, _) = l.accept().unwrap();
                let mut buf = [0u8; 2048];
                let _ = c.read(&mut buf);
                let _ = c.write_all(r.as_bytes());
            }
        });
        port
    }

    struct NoPing;
    impl Prober for NoPing {
        fn ping(&self, _: IpAddr, _: Duration) -> Option<f64> {
            None
        }
        fn tcp(&self, _: IpAddr, _: u16, _: Duration) -> (crate::probe::Tcp, f64) {
            (crate::probe::Tcp::Silent, 0.)
        }
    }

    #[test]
    fn checks_against_a_local_server() {
        let port = serve(vec!["HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\nall healthy", "HTTP/1.1 500 Internal Server Error\r\n\r\noops", "HTTP/1.1 200 OK\r\n\r\nall healthy"]);
        let mut s = Spec::new(CheckKind::Http, &format!("http://127.0.0.1:{port}/health"));
        s.keyword = Some("healthy".into());
        let o = check(&s, &NoPing);
        assert!(o.ok, "{o:?}");
        assert_eq!(o.status, Some(200));
        assert!(o.latency_ms.is_some());
        assert!(!check(&s, &NoPing).ok);
        let tcp = Spec::new(CheckKind::Tcp, &format!("127.0.0.1:{port}"));
        assert!(check(&tcp, &NoPing).ok, "the port is open");
        // nothing listens there now
        let closed = { TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port() };
        assert!(!check(&Spec::new(CheckKind::Tcp, &format!("127.0.0.1:{closed}")), &NoPing).ok);
        assert!(!check(&Spec::new(CheckKind::Tcp, "no-port"), &NoPing).ok);
        let dns = Spec::new(CheckKind::Dns, "localhost");
        assert!(check(&dns, &NoPing).ok);
        assert!(!check(&Spec::new(CheckKind::Icmp, "127.0.0.1"), &NoPing).ok);
    }

    #[test]
    fn tracker_incidents_and_alerts() {
        let mut spec = Spec::new(CheckKind::Http, "https://x");
        spec.slow_ms = Some(500);
        let s = ServiceCheck { id: 1, name: "site".into(), device: None, spec, enabled: true };
        let up = |ms: f64| Outcome { ok: true, latency_ms: Some(ms), slow: ms > 500., cert_days: Some(30), ..Default::default() };
        let down = || Outcome { ok: false, detail: "HTTP 503".into(), ..Default::default() };
        let mut t = Tracker::default();
        let kinds = |e: Vec<SvcEvent>| e.into_iter().filter(|e| !matches!(e, SvcEvent::Checked { .. })).collect::<Vec<_>>();
        assert!(kinds(t.record(&s, up(100.), 0)).is_empty());
        assert!(kinds(t.record(&s, down(), 60)).is_empty(), "one failure isn't an outage");
        assert_eq!(kinds(t.record(&s, down(), 120)), vec![SvcEvent::Down { service: 1, at: 120, detail: "HTTP 503".into() }]);
        assert!(kinds(t.record(&s, down(), 180)).is_empty());
        assert_eq!(kinds(t.record(&s, up(100.), 300)), vec![SvcEvent::Up { service: 1, at: 300, down_for: Some(180) }]);
        assert_eq!(kinds(t.record(&s, up(900.), 360)), vec![SvcEvent::Slow { service: 1, at: 360, ms: 900. }]);
        assert!(kinds(t.record(&s, up(900.), 420)).is_empty(), "slow alerts once per episode");
        let mut exp = up(100.);
        exp.cert_days = Some(5);
        assert_eq!(kinds(t.record(&s, exp.clone(), 480)), vec![SvcEvent::CertExpiring { service: 1, at: 480, days: 5 }]);
        assert!(kinds(t.record(&s, exp, 540)).is_empty());
        assert_eq!(t.states[&1].recent.len(), 9);
        // a first check that fails is down at once
        let mut t2 = Tracker::default();
        assert!(matches!(kinds(t2.record(&s, down(), 0)).as_slice(), [SvcEvent::Down { .. }]));
    }

    #[test]
    fn uptime_over_windows() {
        let now = 30 * DAY;
        let mut hours = Vec::new();
        // 30 days of hourly buckets, 60 checks each; the last day lost 6 checks, 10 days ago 60
        for h in 0..(30 * 24) {
            let at = h * 3600;
            let mut b = Hour { hour: Hour::of(at), ..Default::default() };
            for k in 0..60 {
                let ok = !(h == 30 * 24 - 1 && k < 6) && h != 20 * 24;
                b.add(&Outcome { ok, latency_ms: Some(20.), ..Default::default() });
            }
            hours.push(b);
        }
        let d1 = uptime(&hours, now, DAY).unwrap();
        assert!((d1 - (1. - 6. / (24. * 60.))).abs() < 1e-9, "{d1}");
        let d30 = uptime(&hours, now, 30 * DAY).unwrap();
        assert!((d30 - (1. - 66. / (720. * 60.))).abs() < 1e-9, "{d30}");
        assert_eq!(uptime(&[], now, DAY), None);
        assert_eq!(hours[0].latency(), Some(20.));
        let merged = merge_hours(&hours[..2], &[Hour { hour: 0, checks: 1, ups: 1, latency_sum: 5., latency_n: 1 }, Hour { hour: 999 * 3600, checks: 1, ..Default::default() }]);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].checks, 61);
    }

    #[test]
    fn engine_runs_and_checks_now() {
        let port = serve(vec!["HTTP/1.1 200 OK\r\n\r\nok", "HTTP/1.1 200 OK\r\n\r\nok"]);
        let s = ServiceCheck { id: 7, name: "local".into(), device: None, spec: Spec::new(CheckKind::Http, &format!("http://127.0.0.1:{port}/")), enabled: true };
        let e = Engine::start(vec![s], Arc::new(NoPing));
        let ev = e.events.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(matches!(ev, SvcEvent::Checked { service: 7, ref outcome, .. } if outcome.ok));
        e.check_now(7);
        let ev = e.events.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(matches!(ev, SvcEvent::Checked { service: 7, .. }));
        e.stop();
    }
}
