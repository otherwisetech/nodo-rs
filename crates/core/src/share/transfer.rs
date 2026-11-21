//! The transfer protocol inside a [`super::crypto::Secure`] channel: offer → accept (with the
//! offset to resume from) or decline → chunks → done → verified result. JSON control messages,
//! raw chunks; the receiver checks the SHA-256 of the whole file before it keeps it.
use super::crypto::Secure;
use ring::digest;
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub const CHUNK: usize = 256 * 1024;
const TAG_MSG: u8 = 0;
const TAG_DATA: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    File,
    /// A folder sent as a zip.
    Folder,
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    /// Content hash: the same file offered again resumes.
    pub id: String,
    pub name: String,
    pub size: u64,
    pub sha256: String,
    pub kind: Kind,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Msg {
    Hello { name: String },
    Offer(Offer),
    Accept { offset: u64 },
    Decline { reason: String },
    Done,
    Cancel,
    Result { ok: bool, detail: String },
}

/// Shared with the UI: bytes so far, total, and a cancel switch.
#[derive(Debug, Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

impl Progress {
    pub fn new() -> Arc<Progress> {
        Arc::new(Progress::default())
    }

    pub fn fraction(&self) -> f32 {
        let t = self.total.load(Ordering::Relaxed);
        if t == 0 { 0. } else { self.done.load(Ordering::Relaxed) as f32 / t as f32 }
    }
}

fn bad(m: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m.into())
}

pub fn send_msg<S: Read + Write>(s: &mut Secure<S>, m: &Msg) -> io::Result<()> {
    let mut b = vec![TAG_MSG];
    b.extend(serde_json::to_vec(m).map_err(|e| bad(e.to_string()))?);
    s.send(&b)
}

pub fn recv_msg<S: Read + Write>(s: &mut Secure<S>) -> io::Result<Msg> {
    let b = s.recv()?;
    match b.split_first() {
        Some((&TAG_MSG, rest)) => serde_json::from_slice(rest).map_err(|e| bad(e.to_string())),
        _ => Err(bad("expected a message")),
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut c = digest::Context::new(&digest::SHA256);
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        c.update(&buf[..n]);
    }
    Ok(hex(c.finish().as_ref()))
}

/// The offer for a file on disk (`kind` Folder for a zip made from a folder).
pub fn offer_for(path: &Path, name: &str, kind: Kind) -> io::Result<Offer> {
    let size = std::fs::metadata(path)?.len();
    let sha = sha256_file(path)?;
    Ok(Offer { id: sha[..32].to_string(), name: name.to_string(), size, sha256: sha, kind, text: None })
}

pub fn offer_text(text: &str) -> Offer {
    let sha = hex(digest::digest(&digest::SHA256, text.as_bytes()).as_ref());
    Offer { id: sha[..32].to_string(), name: String::new(), size: text.len() as u64, sha256: sha, kind: Kind::Text, text: Some(text.to_string()) }
}

/// How a send ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sent {
    Delivered,
    Declined(String),
    Cancelled,
    Failed(String),
}

/// Sender side, after the handshake. `file` is None for text.
pub fn send<S: Read + Write>(s: &mut Secure<S>, my_name: &str, offer: &Offer, file: Option<&Path>, p: &Progress) -> io::Result<Sent> {
    send_msg(s, &Msg::Hello { name: my_name.to_string() })?;
    send_msg(s, &Msg::Offer(offer.clone()))?;
    let offset = match recv_msg(s)? {
        Msg::Accept { offset } => offset.min(offer.size),
        Msg::Decline { reason } => return Ok(Sent::Declined(reason)),
        m => return Err(bad(format!("unexpected {m:?}"))),
    };
    p.total.store(offer.size, Ordering::Relaxed);
    p.done.store(offset, Ordering::Relaxed);
    if let Some(path) = file {
        let mut f = std::fs::File::open(path)?;
        f.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; CHUNK];
        loop {
            if p.cancel.load(Ordering::Relaxed) {
                let _ = send_msg(s, &Msg::Cancel);
                return Ok(Sent::Cancelled);
            }
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let mut frame = Vec::with_capacity(n + 1);
            frame.push(TAG_DATA);
            frame.extend_from_slice(&buf[..n]);
            s.send(&frame)?;
            p.done.fetch_add(n as u64, Ordering::Relaxed);
        }
    }
    send_msg(s, &Msg::Done)?;
    match recv_msg(s)? {
        Msg::Result { ok: true, .. } => Ok(Sent::Delivered),
        Msg::Result { ok: false, detail } => Ok(Sent::Failed(detail)),
        m => Err(bad(format!("unexpected {m:?}"))),
    }
}

/// What the receiver decided about an offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Accept,
    Decline(String),
}

/// How a receive ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Received {
    File { path: PathBuf, offer: Offer, from: String },
    Text { text: String, from: String },
    Declined { offer: Offer, from: String },
    /// The sender stopped; the partial file stays for a resume.
    Cancelled { offer: Offer, from: String },
    Corrupt { offer: Offer, from: String },
}

/// Where partial files wait for a resume.
pub fn partial_path(dir: &Path, id: &str) -> PathBuf {
    let id: String = id.chars().filter(|c| c.is_ascii_hexdigit()).take(64).collect();
    dir.join(".nodo-partial").join(format!("{id}.part"))
}

/// Receiver side, after the handshake. `decide` sees the sender's name and the offer.
pub fn receive<S: Read + Write>(s: &mut Secure<S>, dir: &Path, max_size: u64, decide: impl FnOnce(&str, &Offer) -> Decision, p: &Progress) -> io::Result<Received> {
    let from = match recv_msg(s)? {
        Msg::Hello { name } => name.chars().filter(|c| !c.is_control()).take(80).collect::<String>(),
        m => return Err(bad(format!("unexpected {m:?}"))),
    };
    let offer = match recv_msg(s)? {
        Msg::Offer(o) => o,
        m => return Err(bad(format!("unexpected {m:?}"))),
    };
    if offer.size > max_size {
        send_msg(s, &Msg::Decline { reason: "too_large".into() })?;
        return Ok(Received::Declined { offer, from });
    }
    if let Decision::Decline(reason) = decide(&from, &offer) {
        send_msg(s, &Msg::Decline { reason })?;
        return Ok(Received::Declined { offer, from });
    }
    if offer.kind == Kind::Text {
        send_msg(s, &Msg::Accept { offset: 0 })?;
        let text = offer.text.clone().unwrap_or_default();
        let ok = hex(digest::digest(&digest::SHA256, text.as_bytes()).as_ref()) == offer.sha256;
        match recv_msg(s)? {
            Msg::Done => {}
            m => return Err(bad(format!("unexpected {m:?}"))),
        }
        send_msg(s, &Msg::Result { ok, detail: String::new() })?;
        return Ok(if ok { Received::Text { text, from } } else { Received::Corrupt { offer, from } });
    }
    let part = partial_path(dir, &offer.id);
    if let Some(d) = part.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if have > offer.size {
        std::fs::remove_file(&part)?;
        have = 0;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&part)?;
    send_msg(s, &Msg::Accept { offset: have })?;
    p.total.store(offer.size, Ordering::Relaxed);
    p.done.store(have, Ordering::Relaxed);
    let mut got = have;
    loop {
        let b = s.recv()?;
        match b.split_first() {
            Some((&TAG_DATA, data)) => {
                got += data.len() as u64;
                if got > offer.size {
                    drop(f);
                    let _ = std::fs::remove_file(&part);
                    return Err(bad("more data than offered"));
                }
                f.write_all(data)?;
                p.done.store(got, Ordering::Relaxed);
            }
            Some((&TAG_MSG, rest)) => match serde_json::from_slice::<Msg>(rest).map_err(|e| bad(e.to_string()))? {
                Msg::Done => break,
                Msg::Cancel => return Ok(Received::Cancelled { offer, from }),
                m => return Err(bad(format!("unexpected {m:?}"))),
            },
            _ => return Err(bad("bad frame")),
        }
    }
    f.flush()?;
    drop(f);
    if got != offer.size || sha256_file(&part)? != offer.sha256 {
        let _ = std::fs::remove_file(&part);
        send_msg(s, &Msg::Result { ok: false, detail: "checksum".into() })?;
        return Ok(Received::Corrupt { offer, from });
    }
    let name = if offer.kind == Kind::Folder && !offer.name.to_lowercase().ends_with(".zip") { format!("{}.zip", offer.name) } else { offer.name.clone() };
    let dest = super::unique_path(dir, &name);
    std::fs::rename(&part, &dest)?;
    super::quarantine(&dest, &from);
    send_msg(s, &Msg::Result { ok: true, detail: String::new() })?;
    Ok(Received::File { path: dest, offer, from })
}
