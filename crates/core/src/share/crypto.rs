//! The encrypted, mutually authenticated channel between two Nodos (all of it from `ring`).
//!
//! Each Nodo has a long-term Ed25519 identity. A connection does an ephemeral X25519 exchange;
//! each side signs the transcript with its identity, so each learns the other's public key.
//! Unknown keys are paired once: both screens show the same 6-digit code (derived from the
//! transcript, so a man in the middle gets a different one) and the user confirms; the key is
//! then pinned. Frames are ChaCha20-Poly1305 with a per-direction key and counter nonce.
use ring::rand::SecureRandom as _;
use ring::{aead, agreement, digest, hkdf, rand, signature};
use std::io::{self, Read, Write};

const MAGIC: &[u8; 8] = b"NODOSHR1"; // i18n-ok: protocol tag
const MAX_FRAME: usize = 1 << 20;

fn err(m: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m.to_string())
}

/// This Nodo's long-term key pair (PKCS#8, kept in the config dir).
pub struct Identity {
    pair: signature::Ed25519KeyPair,
    pkcs8: Vec<u8>,
}

impl Identity {
    pub fn generate() -> Identity {
        let rng = rand::SystemRandom::new();
        let doc = signature::Ed25519KeyPair::generate_pkcs8(&rng).expect("system randomness");
        Identity::from_pkcs8(doc.as_ref()).expect("fresh key parses")
    }

    pub fn from_pkcs8(b: &[u8]) -> Option<Identity> {
        signature::Ed25519KeyPair::from_pkcs8(b).ok().map(|pair| Identity { pair, pkcs8: b.to_vec() })
    }

    pub fn pkcs8(&self) -> &[u8] {
        &self.pkcs8
    }

    pub fn public(&self) -> PeerKey {
        let mut k = [0u8; 32];
        k.copy_from_slice(signature::KeyPair::public_key(&self.pair).as_ref());
        PeerKey(k)
    }

    /// Load from `path`, or make one and save it there (owner-only on Unix).
    pub fn load_or_create(path: &std::path::Path) -> io::Result<Identity> {
        if let Ok(b) = std::fs::read(path)
            && let Some(id) = Identity::from_pkcs8(&b)
        {
            return Ok(id);
        }
        let id = Identity::generate();
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(path, id.pkcs8())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(id)
    }
}

/// A peer's Ed25519 public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerKey(pub [u8; 32]);

impl PeerKey {
    pub fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(s: &str) -> Option<PeerKey> {
        let s = s.trim();
        if s.len() != 64 {
            return None;
        }
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        Some(PeerKey(k))
    }

    /// Short form for the UI and mDNS.
    pub fn fingerprint(&self) -> String {
        self.hex()[..16].to_string()
    }
}

fn read_frame(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let n = u32::from_be_bytes(len) as usize;
    if n > MAX_FRAME + 64 {
        return Err(err("frame too large"));
    }
    let mut b = vec![0u8; n];
    r.read_exact(&mut b)?;
    Ok(b)
}

fn write_frame(w: &mut impl Write, b: &[u8]) -> io::Result<()> {
    w.write_all(&(b.len() as u32).to_be_bytes())?;
    w.write_all(b)?;
    w.flush()
}

struct Len(usize);
impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn sha(parts: &[&[u8]]) -> [u8; 32] {
    let mut c = digest::Context::new(&digest::SHA256);
    for p in parts {
        c.update(p);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(c.finish().as_ref());
    out
}

struct Dir {
    key: aead::LessSafeKey,
    n: u64,
}

impl Dir {
    fn new(k: &[u8]) -> Dir {
        Dir { key: aead::LessSafeKey::new(aead::UnboundKey::new(&aead::CHACHA20_POLY1305, k).expect("32-byte key")), n: 0 }
    }

    fn nonce(&mut self) -> aead::Nonce {
        let mut b = [0u8; 12];
        b[4..].copy_from_slice(&self.n.to_be_bytes());
        self.n += 1;
        aead::Nonce::assume_unique_for_key(b)
    }
}

/// An authenticated, encrypted connection.
pub struct Secure<S> {
    io: S,
    tx: Dir,
    rx: Dir,
    pub peer: PeerKey,
    /// The 6-digit pairing code both sides see.
    pub code: String,
}

impl<S: Read + Write> Secure<S> {
    pub fn send(&mut self, plain: &[u8]) -> io::Result<()> {
        if plain.len() > MAX_FRAME {
            return Err(err("frame too large"));
        }
        let mut b = plain.to_vec();
        let n = self.tx.nonce();
        self.tx.key.seal_in_place_append_tag(n, aead::Aad::empty(), &mut b).map_err(|_| err("seal"))?;
        write_frame(&mut self.io, &b)
    }

    pub fn recv(&mut self) -> io::Result<Vec<u8>> {
        let mut b = read_frame(&mut self.io)?;
        let n = self.rx.nonce();
        let plain = self.rx.key.open_in_place(n, aead::Aad::empty(), &mut b).map_err(|_| err("bad frame (tampered or wrong key)"))?;
        let len = plain.len();
        b.truncate(len);
        Ok(b)
    }

    pub fn get_ref(&self) -> &S {
        &self.io
    }
}

fn code(h: &[u8]) -> String {
    let d = sha(&[b"nodo-pair-code", h]);
    let v = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % 1_000_000;
    format!("{:03} {:03}", v / 1000, v % 1000)
}

fn keys(shared: &[u8], h: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, h).extract(shared);
    let mut okm = [0u8; 64];
    prk.expand(&[b"nodo-share keys"], Len(64)).and_then(|o| o.fill(&mut okm)).expect("64 bytes");
    let (mut a, mut b) = ([0u8; 32], [0u8; 32]);
    a.copy_from_slice(&okm[..32]);
    b.copy_from_slice(&okm[32..]);
    (a, b)
}

fn ephemeral() -> io::Result<(agreement::EphemeralPrivateKey, [u8; 32])> {
    let rng = rand::SystemRandom::new();
    let k = agreement::EphemeralPrivateKey::generate(&agreement::X25519, &rng).map_err(|_| err("rng"))?;
    let mut p = [0u8; 32];
    p.copy_from_slice(k.compute_public_key().map_err(|_| err("key"))?.as_ref());
    Ok((k, p))
}

fn agree(k: agreement::EphemeralPrivateKey, peer: &[u8]) -> io::Result<Vec<u8>> {
    agreement::agree_ephemeral(k, &agreement::UnparsedPublicKey::new(&agreement::X25519, peer), |s| s.to_vec()).map_err(|_| err("key agreement"))
}

fn verify(pk: &[u8], msg: &[u8], sig: &[u8]) -> io::Result<()> {
    signature::UnparsedPublicKey::new(&signature::ED25519, pk).verify(msg, sig).map_err(|_| err("bad signature"))
}

/// Initiator side (the sender).
pub fn connect<S: Read + Write>(mut io: S, me: &Identity) -> io::Result<Secure<S>> {
    let (eph, i_pub) = ephemeral()?;
    write_frame(&mut io, &[MAGIC.as_slice(), &i_pub].concat())?;
    let m2 = read_frame(&mut io)?;
    if m2.len() != 32 + 32 + 64 {
        return Err(err("handshake"));
    }
    let (r_pub, r_id, r_sig) = (&m2[..32], &m2[32..64], &m2[64..]);
    let h = sha(&[MAGIC, &i_pub, r_pub, r_id]);
    verify(r_id, &h, r_sig)?;
    let shared = agree(eph, r_pub)?;
    let (k_ir, k_ri) = keys(&shared, &h);
    let mine = me.public();
    let h2 = sha(&[&h, &mine.0]);
    let mut peer = [0u8; 32];
    peer.copy_from_slice(r_id);
    let mut s = Secure { io, tx: Dir::new(&k_ir), rx: Dir::new(&k_ri), peer: PeerKey(peer), code: code(&h2) };
    s.send(&[mine.0.as_slice(), me.pair.sign(&h2).as_ref()].concat())?;
    Ok(s)
}

/// Responder side (the receiver).
pub fn accept<S: Read + Write>(mut io: S, me: &Identity) -> io::Result<Secure<S>> {
    let m1 = read_frame(&mut io)?;
    if m1.len() != 8 + 32 || &m1[..8] != MAGIC {
        return Err(err("not a Nodo"));
    }
    let i_pub = &m1[8..];
    let (eph, r_pub) = ephemeral()?;
    let mine = me.public();
    let h = sha(&[MAGIC, i_pub, &r_pub, &mine.0]);
    write_frame(&mut io, &[r_pub.as_slice(), &mine.0, me.pair.sign(&h).as_ref()].concat())?;
    let shared = agree(eph, i_pub)?;
    let (k_ir, k_ri) = keys(&shared, &h);
    let mut s = Secure { io, tx: Dir::new(&k_ri), rx: Dir::new(&k_ir), peer: PeerKey([0; 32]), code: String::new() };
    let m3 = s.recv()?;
    if m3.len() != 32 + 64 {
        return Err(err("handshake"));
    }
    let h2 = sha(&[&h, &m3[..32]]);
    verify(&m3[..32], &h2, &m3[32..])?;
    s.peer.0.copy_from_slice(&m3[..32]);
    s.code = code(&h2);
    Ok(s)
}

/// Random bytes as hex (transfer ids).
pub fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    let _ = rand::SystemRandom::new().fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    #[test]
    fn handshake_pairs_and_encrypts() {
        let (a, b) = (Identity::generate(), Identity::generate());
        let (a_pub, b_pub) = (a.public(), b.public());
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let (c, _) = l.accept().unwrap();
            let mut s = accept(c, &b).unwrap();
            let got = s.recv().unwrap();
            s.send(&got).unwrap();
            (s.peer, s.code.clone())
        });
        let mut s = connect(TcpStream::connect(addr).unwrap(), &a).unwrap();
        s.send(b"hello").unwrap();
        assert_eq!(s.recv().unwrap(), b"hello");
        let (peer, code) = t.join().unwrap();
        assert_eq!(peer, a_pub, "the receiver learns the sender's key");
        assert_eq!(s.peer, b_pub);
        assert_eq!(s.code, code, "both screens show the same code");
        assert_eq!(code.len(), 7);
        assert_eq!(PeerKey::from_hex(&a_pub.hex()), Some(a_pub));
        let again = Identity::from_pkcs8(a.pkcs8()).unwrap();
        assert_eq!(again.public(), a_pub);
    }

    #[test]
    fn tampering_is_detected() {
        let (a, b) = (Identity::generate(), Identity::generate());
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let (c, _) = l.accept().unwrap();
            let mut s = accept(c, &b).unwrap();
            s.recv().map(|_| ())
        });
        let s = connect(TcpStream::connect(addr).unwrap(), &a).unwrap();
        // a forged frame on the wire
        let mut raw = s.get_ref().try_clone().unwrap();
        write_frame(&mut raw, &[0u8; 40]).unwrap();
        assert!(t.join().unwrap().is_err());
        // not a Nodo at all
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let b = Identity::generate();
        let t = std::thread::spawn(move || accept(l.accept().unwrap().0, &b).map(|_| ()));
        let mut c = TcpStream::connect(addr).unwrap();
        write_frame(&mut c, b"GET / HTTP/1.1\r\n\r\n").unwrap();
        assert!(t.join().unwrap().is_err());
    }
}
