//! Sending files and text between computers running Nodo, over the LAN or Tailscale.
//!
//! Opt-in: a computer only listens when the user turns *Receive files* on. Peers find each other
//! by mDNS (`_nodo._tcp`) and by the addresses the map already knows (the Tailscale one first).
//! The channel is end-to-end encrypted and mutually authenticated ([`crypto`]); a new peer is
//! paired once by comparing a 6-digit code on both screens, then pinned. Transfers are chunked,
//! resumable and SHA-256 verified ([`transfer`]). Files land in `~/Downloads/Nodo`, never
//! overwriting, with the macOS quarantine attribute set.
pub mod crypto;
pub mod transfer;

use crypto::{Identity, PeerKey};
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use transfer::{Decision, Offer, Progress, Received, Sent};

pub const PORT: u16 = 47821;
pub const SERVICE: &str = "_nodo._tcp.local.";
/// Default largest file accepted.
pub const DEFAULT_MAX: u64 = 4 * 1024 * 1024 * 1024;

/// A file name from a peer, made safe: no directories, no `..`, no control characters.
pub fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let s: String = base.chars().filter(|c| !c.is_control() && !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|')).collect();
    let s = s.trim().trim_start_matches('.').trim().to_string();
    if s.is_empty() { "file".into() } else { s.chars().take(200).collect() } // i18n-ok: fallback file name
}

/// `dir/name`, or `name (1).ext`, `name (2).ext`… if taken. Never overwrites.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let name = safe_name(name);
    let p = dir.join(&name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    (1..).map(|i| dir.join(format!("{stem} ({i}){ext}"))).find(|p| !p.exists()).expect("some name is free")
}

/// macOS: mark a received file as downloaded (Gatekeeper asks before opening it).
pub fn quarantine(path: &Path, from: &str) {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let agent: String = from.chars().filter(|c| c.is_ascii_alphanumeric() || *c == ' ' || *c == '-').collect();
        let value = format!("0081;{t:x};Nodo ({agent});"); // i18n-ok: xattr value
        let (Ok(p), Ok(k)) = (std::ffi::CString::new(path.as_os_str().as_bytes()), std::ffi::CString::new("com.apple.quarantine")) else { return };
        // SAFETY: valid NUL-terminated strings and a buffer of the given length
        unsafe {
            libc::setxattr(p.as_ptr(), k.as_ptr(), value.as_ptr().cast(), value.len(), 0, 0);
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (path, from);
}

/// Zip a folder (stored, not compressed: fast and simple; CRC-32 from flate2) into `out`.
pub fn zip_folder(dir: &Path, out: &Path) -> io::Result<()> {
    let root = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "folder".into()); // i18n-ok: fallback name
    let mut files = Vec::new();
    fn walk(d: &Path, rel: &str, files: &mut Vec<(PathBuf, String)>) -> io::Result<()> {
        let mut entries: Vec<_> = std::fs::read_dir(d)?.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let path = e.path();
            let ft = e.file_type()?;
            if ft.is_symlink() {
                continue;
            }
            let r = format!("{rel}/{name}");
            if ft.is_dir() {
                walk(&path, &r, files)?;
            } else if ft.is_file() {
                files.push((path, r));
            }
        }
        Ok(())
    }
    walk(dir, &root, &mut files)?;
    let mut w = io::BufWriter::new(std::fs::File::create(out)?);
    let mut central = Vec::new();
    let mut offset: u64 = 0;
    let mut count = 0u16;
    for (path, name) in files {
        let data = std::fs::read(&path)?;
        if data.len() as u64 > u32::MAX as u64 || offset > u32::MAX as u64 || count == u16::MAX {
            return Err(io::Error::other("folder too large to zip"));
        }
        let mut crc = flate2::Crc::new();
        crc.update(&data);
        let (crc, size, nb) = (crc.sum(), data.len() as u32, name.as_bytes());
        let mut h = Vec::new();
        h.extend(0x04034b50u32.to_le_bytes());
        h.extend([20, 0, 0, 0x08, 0, 0, 0, 0, 0, 0]); // version, flags (UTF-8 names), stored, time, date
        h.extend(crc.to_le_bytes());
        h.extend(size.to_le_bytes());
        h.extend(size.to_le_bytes());
        h.extend((nb.len() as u16).to_le_bytes());
        h.extend(0u16.to_le_bytes());
        h.extend(nb);
        w.write_all(&h)?;
        w.write_all(&data)?;
        let mut c = Vec::new();
        c.extend(0x02014b50u32.to_le_bytes());
        c.extend([20, 0, 20, 0, 0, 0x08, 0, 0, 0, 0, 0, 0]);
        c.extend(crc.to_le_bytes());
        c.extend(size.to_le_bytes());
        c.extend(size.to_le_bytes());
        c.extend((nb.len() as u16).to_le_bytes());
        c.extend([0u8; 8]); // extra, comment, disk, internal attrs
        c.extend(0u32.to_le_bytes());
        c.extend((offset as u32).to_le_bytes());
        c.extend(nb);
        central.extend(c);
        offset += h.len() as u64 + data.len() as u64;
        count += 1;
    }
    w.write_all(&central)?;
    let mut e = Vec::new();
    e.extend(0x06054b50u32.to_le_bytes());
    e.extend([0u8; 4]);
    e.extend(count.to_le_bytes());
    e.extend(count.to_le_bytes());
    e.extend((central.len() as u32).to_le_bytes());
    e.extend((offset as u32).to_le_bytes());
    e.extend(0u16.to_le_bytes());
    w.write_all(&e)?;
    w.flush()
}

/// Paired peers (`peers.tsv` in the config dir: key, name per line).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Peers(pub Vec<(PeerKey, String)>);

impl Peers {
    pub fn parse(s: &str) -> Peers {
        Peers(s.lines().filter_map(|l| {
            let (k, n) = l.split_once('\t').unwrap_or((l, ""));
            Some((PeerKey::from_hex(k)?, n.trim().to_string()))
        }).collect())
    }

    pub fn to_text(&self) -> String {
        self.0.iter().map(|(k, n)| format!("{}\t{}\n", k.hex(), n.replace(['\t', '\n'], " "))).collect()
    }

    pub fn name(&self, k: &PeerKey) -> Option<&str> {
        self.0.iter().find(|(x, _)| x == k).map(|(_, n)| n.as_str())
    }

    pub fn add(&mut self, k: PeerKey, name: &str) {
        self.0.retain(|(x, _)| *x != k);
        self.0.push((k, name.to_string()));
    }

    pub fn remove(&mut self, k: &PeerKey) {
        self.0.retain(|(x, _)| x != k);
    }

    pub fn load(path: &Path) -> Peers {
        std::fs::read_to_string(path).map(|s| Peers::parse(&s)).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(path, self.to_text())
    }
}

/// What the receiver asks its UI. Each call may block (the user answers a prompt).
pub trait Host: Send + Sync + 'static {
    /// A peer seen for the first time: pair if both screens show `code`.
    fn pair(&self, key: &PeerKey, code: &str, addr: IpAddr) -> bool;
    /// Whether a paired peer counts as known (pairing is remembered here).
    fn known(&self, key: &PeerKey) -> Option<String>;
    fn decide(&self, from: &str, key: &PeerKey, offer: &Offer) -> Decision;
    fn started(&self, _from: &str, _offer: &Offer, _p: Arc<Progress>) {}
    fn finished(&self, r: Result<Received, String>);
}

/// A running receiver: the listener thread and the mDNS advertisement.
pub struct Receiver {
    stop: Arc<AtomicBool>,
    pub port: u16,
    mdns: Option<mdns_sd::ServiceDaemon>,
}

pub struct ReceiverConfig {
    pub dir: PathBuf,
    pub max_size: u64,
    /// Bind address; `0.0.0.0` in the app, loopback in tests.
    pub bind: SocketAddr,
    pub advertise: bool,
    pub name: String,
}

impl Receiver {
    pub fn start(cfg: ReceiverConfig, me: Arc<Identity>, host: Arc<dyn Host>) -> io::Result<Receiver> {
        let l = TcpListener::bind(cfg.bind)?;
        let port = l.local_addr()?.port();
        l.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        let cfg = Arc::new(cfg);
        let c2 = cfg.clone();
        std::thread::spawn(move || {
            while !st.load(Ordering::Relaxed) {
                match l.accept() {
                    Ok((sock, addr)) => {
                        let (me, host, cfg) = (me.clone(), host.clone(), c2.clone());
                        std::thread::spawn(move || {
                            let r = serve(sock, addr.ip(), &me, &*host, &cfg);
                            if let Err(e) = r {
                                host.finished(Err(e.to_string()));
                            }
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(150)),
                    Err(_) => std::thread::sleep(Duration::from_millis(500)),
                }
            }
        });
        let mdns = if cfg.advertise { advertise(&cfg.name, port).ok() } else { None };
        Ok(Receiver { stop, port, mdns })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(d) = self.mdns.take() {
            let _ = d.shutdown();
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve(sock: TcpStream, ip: IpAddr, me: &Identity, host: &dyn Host, cfg: &ReceiverConfig) -> io::Result<()> {
    sock.set_nonblocking(false)?;
    sock.set_read_timeout(Some(Duration::from_secs(300)))?;
    let mut s = crypto::accept(sock, me)?;
    let key = s.peer;
    if host.known(&key).is_none() && !host.pair(&key, &s.code, ip) {
        return Ok(());
    }
    std::fs::create_dir_all(&cfg.dir)?;
    let p = Progress::new();
    let p2 = p.clone();
    let r = transfer::receive(
        &mut s,
        &cfg.dir,
        cfg.max_size,
        |from, offer| {
            let d = host.decide(from, &key, offer);
            if d == Decision::Accept {
                host.started(from, offer, p2.clone());
            }
            d
        },
        &p,
    );
    host.finished(r.map_err(|e| e.to_string()));
    Ok(())
}

fn advertise(name: &str, port: u16) -> Result<mdns_sd::ServiceDaemon, mdns_sd::Error> {
    let d = mdns_sd::ServiceDaemon::new()?;
    let host = format!("{}.local.", crate::model::host_label(name).replace(' ', "-"));
    let info = mdns_sd::ServiceInfo::new(SERVICE, name, &host, "", port, &[("v", "1")][..])?.enable_addr_auto();
    d.register(info)?;
    Ok(d)
}

/// Nodos answering `_nodo._tcp` on the LAN: (instance name, addresses, port). Blocking for `wait`.
pub fn browse(wait: Duration) -> Vec<(String, Vec<IpAddr>, u16)> {
    let Ok(d) = mdns_sd::ServiceDaemon::new() else { return Vec::new() };
    let Ok(rx) = d.browse(SERVICE) else { return Vec::new() };
    let end = std::time::Instant::now() + wait;
    let mut out: Vec<(String, Vec<IpAddr>, u16)> = Vec::new();
    while let Some(left) = end.checked_duration_since(std::time::Instant::now()) {
        if let Ok(mdns_sd::ServiceEvent::ServiceResolved(i)) = rx.recv_timeout(left) {
            let name = i.fullname.trim_end_matches(SERVICE).trim_end_matches('.').to_string();
            let ips: Vec<IpAddr> = i.addresses.iter().map(|a| a.to_ip_addr()).collect();
            out.retain(|(n, _, _)| *n != name);
            out.push((name, ips, i.port));
        }
    }
    let _ = d.shutdown();
    out
}

/// What to send.
pub enum Payload {
    File { path: PathBuf, name: String, kind: transfer::Kind },
    Text(String),
}

/// Sender: try the addresses in order (Tailscale first, as the caller sorts them), handshake,
/// check or pair the peer key (`check` sees the key and the code and says whether to go on),
/// then send.
pub fn send_to(addrs: &[SocketAddr], me: &Identity, my_name: &str, payload: &Payload, p: &Progress, check: impl FnOnce(&PeerKey, &str) -> bool) -> io::Result<Sent> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no address");
    let mut sock = None;
    for a in addrs {
        match TcpStream::connect_timeout(a, Duration::from_secs(3)) {
            Ok(s) => {
                sock = Some(s);
                break;
            }
            Err(e) => last = e,
        }
    }
    let sock = sock.ok_or(last)?;
    sock.set_read_timeout(Some(Duration::from_secs(300)))?;
    let mut s = crypto::connect(sock, me)?;
    let (key, code) = (s.peer, s.code.clone());
    if !check(&key, &code) {
        return Ok(Sent::Cancelled);
    }
    send_on(&mut s, my_name, payload, p)
}

fn send_on<S: Read + Write>(s: &mut crypto::Secure<S>, my_name: &str, payload: &Payload, p: &Progress) -> io::Result<Sent> {
    match payload {
        Payload::File { path, name, kind } => {
            let offer = transfer::offer_for(path, name, *kind)?;
            transfer::send(s, my_name, &offer, Some(path), p)
        }
        Payload::Text(t) => transfer::send(s, my_name, &transfer::offer_text(t), None, p),
    }
}

/// `tailscale file cp <file> <target>:` (Taildrop). Errors are the CLI's own words.
pub fn taildrop(cli: &Path, file: &Path, target: &str) -> Result<(), String> {
    let f = file.to_string_lossy().into_owned();
    let t = format!("{target}:");
    match crate::cmd::run_env(cli, &["file", "cp", &f, &t], &[("TAILSCALE_BE_CLI", "1")], Duration::from_secs(3600)) {
        Ok(o) if o.ok => Ok(()),
        Ok(o) => Err(if o.stderr.trim().is_empty() { o.stdout.trim().to_string() } else { o.stderr.trim().to_string() }),
        Err(e) => Err(format!("{e:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use transfer::Kind;

    #[test]
    fn names_are_safe_and_unique() {
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name("C:\\Users\\x\\a?b.txt"), "ab.txt");
        assert_eq!(safe_name(".."), "file");
        assert_eq!(safe_name(".bashrc"), "bashrc");
        let d = tempfile::tempdir().unwrap();
        let a = unique_path(d.path(), "photo.jpg");
        assert_eq!(a.file_name().unwrap(), "photo.jpg");
        std::fs::write(&a, b"x").unwrap();
        let b = unique_path(d.path(), "photo.jpg");
        assert_eq!(b.file_name().unwrap(), "photo (1).jpg");
        std::fs::write(&b, b"x").unwrap();
        assert_eq!(unique_path(d.path(), "photo.jpg").file_name().unwrap(), "photo (2).jpg");
        std::fs::write(d.path().join("README"), b"x").unwrap();
        assert_eq!(unique_path(d.path(), "README").file_name().unwrap(), "README (1)");
    }

    #[test]
    fn zips_a_folder() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("proj");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.txt"), b"hello").unwrap();
        std::fs::write(src.join("sub/b.txt"), b"world!").unwrap();
        let out = d.path().join("proj.zip");
        zip_folder(&src, &out).unwrap();
        let z = std::fs::read(&out).unwrap();
        assert_eq!(&z[..4], &[0x50, 0x4b, 0x03, 0x04]);
        let end = &z[z.len() - 22..];
        assert_eq!(&end[..4], &[0x50, 0x4b, 0x05, 0x06]);
        assert_eq!(u16::from_le_bytes([end[10], end[11]]), 2, "two entries");
        let s = String::from_utf8_lossy(&z);
        assert!(s.contains("proj/a.txt") && s.contains("proj/sub/b.txt") && s.contains("hello"));
    }

    #[test]
    fn peers_round_trip() {
        let k = Identity::generate().public();
        let mut p = Peers::default();
        p.add(k, "studio");
        let q = Peers::parse(&p.to_text());
        assert_eq!(q.name(&k), Some("studio"));
        p.remove(&k);
        assert!(p.0.is_empty());
        assert!(Peers::parse("garbage\n\n").0.is_empty());
    }

    struct TestHost {
        pair: bool,
        accept: bool,
        known: Mutex<Vec<PeerKey>>,
        codes: Mutex<Vec<String>>,
        done: Mutex<Vec<Result<Received, String>>>,
    }

    impl Host for TestHost {
        fn pair(&self, key: &PeerKey, code: &str, _: IpAddr) -> bool {
            self.codes.lock().unwrap().push(code.to_string());
            if self.pair {
                self.known.lock().unwrap().push(*key);
            }
            self.pair
        }
        fn known(&self, key: &PeerKey) -> Option<String> {
            self.known.lock().unwrap().contains(key).then(|| "peer".into())
        }
        fn decide(&self, _: &str, _: &PeerKey, _: &Offer) -> Decision {
            if self.accept { Decision::Accept } else { Decision::Decline("no".into()) }
        }
        fn finished(&self, r: Result<Received, String>) {
            self.done.lock().unwrap().push(r);
        }
    }

    fn host(pair: bool, accept: bool) -> Arc<TestHost> {
        Arc::new(TestHost { pair, accept, known: Mutex::new(vec![]), codes: Mutex::new(vec![]), done: Mutex::new(vec![]) })
    }

    fn start(dir: &Path, h: Arc<TestHost>, max: u64) -> Receiver {
        let cfg = ReceiverConfig { dir: dir.to_path_buf(), max_size: max, bind: "127.0.0.1:0".parse().unwrap(), advertise: false, name: "test".into() };
        Receiver::start(cfg, Arc::new(Identity::generate()), h).unwrap()
    }

    fn wait(h: &TestHost, n: usize) {
        for _ in 0..200 {
            if h.done.lock().unwrap().len() >= n {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("receiver didn't finish");
    }

    #[test]
    fn loopback_transfer_pair_resume_verify() {
        let d = tempfile::tempdir().unwrap();
        let inbox = d.path().join("inbox");
        let src = d.path().join("big.bin");
        let data: Vec<u8> = (0..(transfer::CHUNK * 3 + 1234)).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(&src, &data).unwrap();
        let h = host(true, true);
        let r = start(&inbox, h.clone(), DEFAULT_MAX);
        let addr: SocketAddr = format!("127.0.0.1:{}", r.port).parse().unwrap();
        let me = Identity::generate();
        let payload = Payload::File { path: src.clone(), name: "big.bin".into(), kind: Kind::File };
        // pairing: the sender sees the same code the receiver confirmed
        let mut seen = String::new();
        let p = Progress::new();
        let out = send_to(&[addr], &me, "laptop", &payload, &p, |_, code| {
            seen = code.to_string();
            true
        })
        .unwrap();
        assert_eq!(out, Sent::Delivered);
        wait(&h, 1);
        assert_eq!(h.codes.lock().unwrap().as_slice(), [seen]);
        let got = inbox.join("big.bin");
        assert_eq!(std::fs::read(&got).unwrap(), data);
        assert!((p.fraction() - 1.).abs() < 1e-6);
        // resume: half of it already waiting; the second copy gets a unique name
        let offer = transfer::offer_for(&src, "big.bin", Kind::File).unwrap();
        let part = transfer::partial_path(&inbox, &offer.id);
        std::fs::create_dir_all(part.parent().unwrap()).unwrap();
        std::fs::write(&part, &data[..transfer::CHUNK + 10]).unwrap();
        let p = Progress::new();
        assert_eq!(send_to(&[addr], &me, "laptop", &payload, &p, |_, _| true).unwrap(), Sent::Delivered);
        wait(&h, 2);
        assert_eq!(h.codes.lock().unwrap().len(), 1, "paired once");
        assert_eq!(std::fs::read(inbox.join("big (1).bin")).unwrap(), data);
        assert!(!part.exists());
        // a corrupt partial is caught by the checksum and dropped
        std::fs::write(&part, vec![0u8; 100]).unwrap();
        let out = send_to(&[addr], &me, "laptop", &payload, &Progress::new(), |_, _| true).unwrap();
        assert!(matches!(out, Sent::Failed(_)));
        wait(&h, 3);
        assert!(matches!(h.done.lock().unwrap()[2], Ok(Received::Corrupt { .. })));
        assert!(!part.exists());
        // text
        let out = send_to(&[addr], &me, "laptop", &Payload::Text("hola 👋".into()), &Progress::new(), |_, _| true).unwrap();
        assert_eq!(out, Sent::Delivered);
        wait(&h, 4);
        assert!(matches!(&h.done.lock().unwrap()[3], Ok(Received::Text { text, from }) if text == "hola 👋" && from == "laptop"));
        // cancelled before the first chunk: nothing kept but the (empty) partial for a resume
        let p = Progress::new();
        p.cancel.store(true, Ordering::Relaxed);
        assert_eq!(send_to(&[addr], &me, "laptop", &payload, &p, |_, _| true).unwrap(), Sent::Cancelled);
        wait(&h, 5);
        assert!(matches!(h.done.lock().unwrap()[4], Ok(Received::Cancelled { .. })));
    }

    #[test]
    fn declines_unpaired_and_too_large() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("a.txt");
        std::fs::write(&src, b"0123456789").unwrap();
        let payload = Payload::File { path: src, name: "a.txt".into(), kind: Kind::File };
        let me = Identity::generate();
        // the receiver's user said the codes don't match: the connection just ends
        let h = host(false, true);
        let r = start(&d.path().join("in1"), h.clone(), DEFAULT_MAX);
        let addr: SocketAddr = format!("127.0.0.1:{}", r.port).parse().unwrap();
        assert!(send_to(&[addr], &me, "x", &payload, &Progress::new(), |_, _| true).is_err());
        // over the size limit
        let h = host(true, true);
        let r = start(&d.path().join("in2"), h.clone(), 5);
        let addr: SocketAddr = format!("127.0.0.1:{}", r.port).parse().unwrap();
        assert_eq!(send_to(&[addr], &me, "x", &payload, &Progress::new(), |_, _| true).unwrap(), Sent::Declined("too_large".into()));
        // the user declined
        let h = host(true, false);
        let r = start(&d.path().join("in3"), h.clone(), DEFAULT_MAX);
        let addr: SocketAddr = format!("127.0.0.1:{}", r.port).parse().unwrap();
        assert_eq!(send_to(&[addr], &me, "x", &payload, &Progress::new(), |_, _| true).unwrap(), Sent::Declined("no".into()));
        // the sender's user stopped at the code
        assert_eq!(send_to(&[addr], &me, "x", &payload, &Progress::new(), |_, _| false).unwrap(), Sent::Cancelled);
        assert!(!d.path().join("in3/a.txt").exists());
    }
}
