//! Where a map file lives: safe copy / move (`VACUUM INTO`, then integrity check and reopen
//! before anything is deleted), cloud-sync folders (OneDrive, iCloud Drive, Dropbox, Google
//! Drive, Box), the small lock file that says "open on another computer", and the conflict
//! copies sync clients leave next to the map. Cloud detection, conflict names and the lock file
//! format are otherwise-link's (`cloud`, `lock`), shared with every Otherwise app, so Otherwise
//! Apps sees an open map before it moves the data folder.
use crate::db::{Store, StoreError};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum FileError {
    /// Source and destination are the same file.
    Same,
    /// The destination's folder doesn't exist.
    NoFolder(PathBuf),
    /// Another computer has the map open (fresh lock).
    Locked { host: String },
    Store(StoreError),
    Io(std::io::Error),
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileError::Same => write!(f, "{}", i18n::t!("core.file.same")),
            FileError::NoFolder(p) => write!(f, "{}", i18n::t!("core.file.no_folder", path = p.display())),
            FileError::Locked { host } => write!(f, "{}", i18n::t!("core.file.locked", host = host)),
            FileError::Store(e) => write!(f, "{e}"),
            FileError::Io(e) => write!(f, "{}", i18n::t!("core.file.io", detail = e)),
        }
    }
}

impl std::error::Error for FileError {}

impl From<StoreError> for FileError {
    fn from(e: StoreError) -> Self {
        FileError::Store(e)
    }
}

impl From<std::io::Error> for FileError {
    fn from(e: std::io::Error) -> Self {
        FileError::Io(e)
    }
}

fn sibling(path: &Path, name: String) -> PathBuf {
    path.with_file_name(name)
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn same_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Copy the open map to `dest`: snapshot into a hidden temporary file next to `dest`, check
/// it (integrity, application id), then rename it into place. On any failure `dest` is left
/// as it was and the temporary file is removed.
pub fn copy_map(store: &Store, dest: &Path) -> Result<(), FileError> {
    if same_file(store.path(), dest) {
        return Err(FileError::Same);
    }
    let dir = dest.parent().filter(|d| !d.as_os_str().is_empty()).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    if !dir.is_dir() {
        return Err(FileError::NoFolder(dir));
    }
    let tmp = sibling(dest, format!(".{}.nodo-copy", file_name(dest)));
    let _ = std::fs::remove_file(&tmp);
    let r = (|| {
        store.copy_into(&tmp)?;
        Store::verify(&tmp)?;
        let _ = std::fs::remove_file(crate::db::journal(dest));
        std::fs::rename(&tmp, dest)?;
        Ok(())
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

/// [`copy_map`] for a map that isn't open.
pub fn copy_file(from: &Path, to: &Path) -> Result<(), FileError> {
    let store = Store::open_read_only(from)?;
    copy_map(&store, to)
}

/// The map file and what Nodo keeps beside it (rollback journal, lock).
pub fn remove_map(path: &Path) -> std::io::Result<()> {
    std::fs::remove_file(path)?;
    let _ = std::fs::remove_file(crate::db::journal(path));
    let _ = std::fs::remove_file(lock_path(path));
    let _ = std::fs::remove_file(legacy_lock_path(path));
    Ok(())
}

/// `nodo map move`: copy, verify, and only then delete the original. Refuses while another
/// Nodo has the map open (fresh lock), unless `force`.
pub fn move_file(from: &Path, to: &Path, host: &str, now: i64, force: bool) -> Result<(), FileError> {
    if !force && let LockState::Elsewhere(l) | LockState::Ours(l) = assess(read_lock(from).as_ref(), host, now) {
        return Err(FileError::Locked { host: l.host });
    }
    copy_file(from, to)?;
    Store::verify(to)?;
    remove_map(from)?;
    Ok(())
}

// ── cloud-sync folders ───────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cloud {
    OneDrive,
    ICloud,
    Dropbox,
    GoogleDrive,
    Box,
    /// Another provider under `~/Library/CloudStorage`.
    Other,
}

impl Cloud {
    pub fn name(self) -> String {
        match self {
            Cloud::OneDrive => "OneDrive".into(),        // i18n-ok: product name
            Cloud::ICloud => "iCloud Drive".into(),      // i18n-ok: product name
            Cloud::Dropbox => "Dropbox".into(),          // i18n-ok: product name
            Cloud::GoogleDrive => "Google Drive".into(), // i18n-ok: product name
            Cloud::Box => "Box".into(),                  // i18n-ok: product name
            Cloud::Other => i18n::t!("core.file.cloud_other"),
        }
    }
}

impl From<otherwise_link::cloud::Provider> for Cloud {
    fn from(p: otherwise_link::cloud::Provider) -> Cloud {
        use otherwise_link::cloud::Provider as P;
        match p {
            P::OneDrive => Cloud::OneDrive,
            P::ICloud => Cloud::ICloud,
            P::Dropbox => Cloud::Dropbox,
            P::GoogleDrive => Cloud::GoogleDrive,
            P::Box => Cloud::Box,
            P::Other => Cloud::Other,
        }
    }
}

/// Which sync client owns the folder `path` is in ([`otherwise_link::cloud::detect`]: macOS
/// File Provider folders, iCloud's `Mobile Documents`, `~/Dropbox`, `~/OneDrive - Company`,
/// `%OneDrive%`, Google Drive's `My Drive`…).
pub fn cloud_of(path: &Path) -> Option<Cloud> {
    otherwise_link::cloud::detect(path).map(|c| c.provider.into())
}

/// [`cloud_of`] judged against a given home folder (tests).
pub fn cloud_in(path: &Path, home: &Path) -> Option<Cloud> {
    otherwise_link::cloud::detect_in(path, home, &[]).map(|c| c.provider.into())
}

// ── "open on another computer" ───────────────────────────────────────────────────────────────

/// How often an open map refreshes its lock.
pub const HEARTBEAT_SECS: u64 = 60;
/// A lock not refreshed for this long is left over from a crash or a computer that slept.
pub const STALE_SECS: i64 = 5 * 60;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LockInfo {
    pub host: String,
    pub pid: u32,
    pub started: i64,
    pub heartbeat: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockState {
    Free,
    /// Taken by this computer (another window, or this one).
    Ours(LockInfo),
    /// Taken by another computer, but its heartbeat stopped.
    Stale(LockInfo),
    /// Another computer has it open right now.
    Elsewhere(LockInfo),
}

/// This computer's name as every Otherwise app writes it into locks.
pub fn host() -> String {
    otherwise_link::lock::host_name().to_string()
}

/// `Home network.nodo.lock.json` next to the map (otherwise-link's lock file), so it syncs with
/// it.
pub fn lock_path(map: &Path) -> PathBuf {
    otherwise_link::lock::lock_path(map)
}

/// Nodo 0.2.1's own lock, `.Home network.nodo.lock`: still read, so a computer that hasn't
/// updated yet is seen.
fn legacy_lock_path(map: &Path) -> PathBuf {
    sibling(map, format!(".{}.lock", file_name(map)))
}

pub fn read_lock(map: &Path) -> Option<LockInfo> {
    let new = otherwise_link::lock::read(map).map(|i| LockInfo { host: i.host, pid: i.pid, started: i.since, heartbeat: i.heartbeat });
    let old = std::fs::read_to_string(legacy_lock_path(map)).ok().and_then(|s| serde_json::from_str::<LockInfo>(&s).ok()).map(|mut l| {
        // 0.2.1 named this computer by its network host name
        if crate::discover::hostname().is_some_and(|h| h.eq_ignore_ascii_case(&l.host)) {
            l.host = host();
        }
        l
    });
    match (new, old) {
        (Some(a), Some(b)) => Some(if b.heartbeat > a.heartbeat { b } else { a }),
        (a, b) => a.or(b),
    }
}

pub fn assess(lock: Option<&LockInfo>, host: &str, now: i64) -> LockState {
    match lock {
        None => LockState::Free,
        Some(l) if l.host.eq_ignore_ascii_case(host) => LockState::Ours(l.clone()),
        Some(l) if now - l.heartbeat > STALE_SECS => LockState::Stale(l.clone()),
        Some(l) => LockState::Elsewhere(l.clone()),
    }
}

/// This Nodo's lock on a map while it is open. Dropping it removes the file if it is still
/// ours.
#[derive(Debug)]
pub struct MapLock {
    map: PathBuf,
    path: PathBuf,
    pub info: LockInfo,
}

impl MapLock {
    /// Take the lock (after the caller has looked at [`assess`] and decided to go ahead).
    pub fn take(map: &Path, host: &str, now: i64) -> std::io::Result<MapLock> {
        let info = LockInfo { host: host.to_string(), pid: std::process::id(), started: now, heartbeat: now };
        let l = MapLock { map: map.to_path_buf(), path: lock_path(map), info };
        l.write()?;
        let _ = std::fs::remove_file(legacy_lock_path(map));
        Ok(l)
    }

    fn write(&self) -> std::io::Result<()> {
        let i = otherwise_link::lock::LockInfo { app: "nodo".into(), host: self.info.host.clone(), pid: self.info.pid, scope: otherwise_link::lock::Scope::Process, since: self.info.started, heartbeat: self.info.heartbeat, ..Default::default() }; // i18n-ok: app id
        otherwise_link::fsutil::write_atomic(&self.path, (serde_json::to_string_pretty(&i).unwrap_or_default() + "\n").as_bytes())
    }

    /// Refresh the heartbeat. If another computer took the map over meanwhile its lock is left
    /// alone and returned.
    pub fn beat(&mut self, now: i64) -> std::io::Result<LockState> {
        let state = assess(read_lock(&self.map).as_ref(), &self.info.host, now);
        if let LockState::Elsewhere(_) = state {
            return Ok(state);
        }
        self.info.heartbeat = now;
        self.write()?;
        Ok(state)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for MapLock {
    fn drop(&mut self) {
        let ours = otherwise_link::lock::read(&self.map).is_some_and(|l| l.host == self.info.host && l.pid == self.info.pid);
        if ours {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

// ── conflict copies ──────────────────────────────────────────────────────────────────────────

/// Whether `candidate` looks like a sync client's conflict copy of the map named `map`:
/// Dropbox `Home network (Juan's conflicted copy 2026-11-20).nodo`, Syncthing
/// `Home network.sync-conflict-….nodo`, iCloud `Home network 2.nodo`, Google Drive
/// `Home network (1).nodo`, OneDrive `Home network-MacBook-Pro.nodo` (only checked when the
/// map is in OneDrive, as `-something` is also an ordinary name). otherwise-link's rules.
pub fn is_conflict_copy(map: &str, candidate: &str, onedrive: bool) -> bool {
    otherwise_link::cloud::is_conflict_name(map, candidate, onedrive.then_some(otherwise_link::cloud::Provider::OneDrive))
}

/// Conflict copies of `map` in its folder, sorted by name.
pub fn conflict_copies(map: &Path) -> Vec<PathBuf> {
    let name = file_name(map);
    let onedrive = cloud_of(map) == Some(Cloud::OneDrive);
    let Some(dir) = map.parent() else { return Vec::new() };
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_file() && is_conflict_copy(&name, &file_name(p), onedrive)).collect()).unwrap_or_default();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Doc;

    fn map_with_device(dir: &Path) -> PathBuf {
        let p = dir.join("Home network.nodo");
        let mut doc = Doc::create(&p).unwrap();
        doc.add_manual("NAS", Some("192.168.1.10".parse().unwrap()), crate::model::DeviceKind::Nas, 1).unwrap();
        p
    }

    #[test]
    fn copy_keeps_everything_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let src = map_with_device(dir.path());
        std::fs::create_dir(dir.path().join("OneDrive")).unwrap();
        let dest = dir.path().join("OneDrive").join("Casa.nodo");
        let doc = Doc::open(&src).unwrap();
        copy_map(&doc.store, &dest).unwrap();
        Store::verify(&dest).unwrap();
        let copy = Doc::open(&dest).unwrap();
        assert_eq!(copy.data.devices.len(), 1);
        assert_eq!(copy.data.devices[0].name, "NAS");
        assert_eq!(copy.store.version().unwrap(), crate::db::latest_version());
        assert_eq!(copy.store.journal_mode().unwrap(), "delete");
        // no temporary file left behind
        assert_eq!(std::fs::read_dir(dir.path().join("OneDrive")).unwrap().count(), 1);
        assert!(src.exists());
        assert!(matches!(copy_map(&doc.store, &src), Err(FileError::Same)));
    }

    #[test]
    fn failed_copy_leaves_original_and_destination() {
        let dir = tempfile::tempdir().unwrap();
        let src = map_with_device(dir.path());
        let doc = Doc::open(&src).unwrap();
        let missing = dir.path().join("nope").join("x.nodo");
        assert!(matches!(copy_map(&doc.store, &missing), Err(FileError::NoFolder(_))));
        // a destination that already exists stays untouched when the copy fails
        let dest = dir.path().join("dest.nodo");
        std::fs::write(&dest, b"keep me").unwrap();
        let tmp = dir.path().join(".dest.nodo.nodo-copy");
        std::fs::create_dir(&tmp).unwrap(); // VACUUM INTO can't write here
        std::fs::write(tmp.join("x"), b"").unwrap();
        assert!(copy_map(&doc.store, &dest).is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"keep me");
        assert!(Doc::open(&src).is_ok());
    }

    #[test]
    fn move_verifies_then_deletes_and_keeps_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let src = map_with_device(dir.path());
        let dest = dir.path().join("moved.nodo");
        move_file(&src, &dest, "pc", 100, false).unwrap();
        assert!(!src.exists());
        assert_eq!(Doc::open(&dest).unwrap().data.devices.len(), 1);
        // not a map: nothing is deleted
        let junk = dir.path().join("junk.nodo");
        std::fs::write(&junk, b"not sqlite at all, just text that is long enough").unwrap();
        assert!(move_file(&junk, &dir.path().join("junk2.nodo"), "pc", 100, false).is_err());
        assert!(junk.exists());
        assert!(!dir.path().join("junk2.nodo").exists());
        // open on another computer: refused unless forced
        let _lock = MapLock::take(&dest, "studio", 100).unwrap();
        assert!(matches!(move_file(&dest, &src, "laptop", 130, false), Err(FileError::Locked { host }) if host == "studio"));
        assert!(dest.exists());
        move_file(&dest, &src, "laptop", 130, true).unwrap();
        assert!(src.exists() && !dest.exists() && !lock_path(&dest).exists());
    }

    #[test]
    fn cloud_folders() {
        let c = |s: &str| {
            let p = Path::new(s);
            let parts: Vec<_> = p.components().collect();
            let i = parts.iter().position(|c| matches!(c.as_os_str().to_str(), Some("Users" | "home"))).unwrap_or(parts.len());
            let home: PathBuf = parts.iter().take(i + 2).collect();
            cloud_in(p, &home).or_else(|| cloud_in(p, Path::new("/nobody")))
        };
        assert_eq!(c("/Users/juan/Library/CloudStorage/OneDrive-Personal/Maps/Home.nodo"), Some(Cloud::OneDrive));
        assert_eq!(c("/Users/juan/Library/CloudStorage/OneDrive-OtherwiseTechnologies/Home.nodo"), Some(Cloud::OneDrive));
        assert_eq!(c("/Users/juan/Library/CloudStorage/GoogleDrive-juan@example.com/My Drive/Home.nodo"), Some(Cloud::GoogleDrive));
        assert_eq!(c("/Users/juan/Library/CloudStorage/Dropbox/Home.nodo"), Some(Cloud::Dropbox));
        assert_eq!(c("/Users/juan/Library/CloudStorage/Box-Box/Home.nodo"), Some(Cloud::Box));
        assert_eq!(c("/Users/juan/Library/CloudStorage/pCloud/Home.nodo"), Some(Cloud::Other));
        assert_eq!(c("/Users/juan/Library/Mobile Documents/com~apple~CloudDocs/Home.nodo"), Some(Cloud::ICloud));
        assert_eq!(c("/Users/juan/Dropbox/Home.nodo"), Some(Cloud::Dropbox));
        assert_eq!(c("/home/juan/Dropbox (Personal)/maps/Home.nodo"), Some(Cloud::Dropbox));
        assert_eq!(c("C:\\Users\\juan\\OneDrive - Otherwise\\Home.nodo").or(c("C:/Users/juan/OneDrive - Otherwise/Home.nodo")), Some(Cloud::OneDrive));
        assert_eq!(c("/Users/juan/OneDrive/Home.nodo"), Some(Cloud::OneDrive));
        assert_eq!(c("/Users/juan/iCloudDrive/Home.nodo"), Some(Cloud::ICloud));
        assert_eq!(c("/Users/juan/Documents/Home.nodo"), None);
        assert_eq!(c("/private/tmp/x/Home network.nodo"), None);
        // the file's own name doesn't make it a cloud folder
        assert_eq!(c("/Users/juan/Documents/Dropbox"), None);
    }

    #[test]
    fn lock_heartbeat_and_stale() {
        let dir = tempfile::tempdir().unwrap();
        let map = dir.path().join("Home network.nodo");
        assert_eq!(assess(read_lock(&map).as_ref(), "laptop", 0), LockState::Free);
        let mut mine = MapLock::take(&map, "laptop", 1000).unwrap();
        assert_eq!(mine.path(), dir.path().join("Home network.nodo.lock.json"));
        // the shared format: Otherwise Apps sees it as "open in Nodo"
        assert_eq!(otherwise_link::lock::read(&map).unwrap().app, "nodo");
        assert!(matches!(assess(read_lock(&map).as_ref(), "laptop", 1010), LockState::Ours(_)));
        assert!(matches!(assess(read_lock(&map).as_ref(), "studio", 1010), LockState::Elsewhere(l) if l.host == "laptop"));
        // heartbeat moves forward, so it never goes stale while open
        mine.beat(1000 + STALE_SECS + 10).unwrap();
        assert_eq!(read_lock(&map).unwrap().heartbeat, 1000 + STALE_SECS + 10);
        assert!(matches!(assess(read_lock(&map).as_ref(), "studio", 1000 + STALE_SECS + 20), LockState::Elsewhere(_)));
        // no heartbeat for longer than STALE_SECS: stale
        assert!(matches!(assess(read_lock(&map).as_ref(), "studio", 1000 + 3 * STALE_SECS), LockState::Stale(_)));
        // another computer took it over: our heartbeat leaves its lock alone
        let theirs = LockInfo { host: "studio".into(), pid: 7, started: 5000, heartbeat: 5000 };
        // a computer still on 0.2.1 writes the old lock file
        std::fs::write(legacy_lock_path(&map), serde_json::to_string(&theirs).unwrap()).unwrap();
        assert!(matches!(mine.beat(5010).unwrap(), LockState::Elsewhere(_)));
        assert_eq!(read_lock(&map).unwrap(), theirs);
        std::fs::remove_file(legacy_lock_path(&map)).unwrap();
        let shared = otherwise_link::lock::LockInfo { app: "nodo".into(), host: "studio".into(), pid: 7, since: 5000, heartbeat: 5000, ..Default::default() };
        std::fs::write(lock_path(&map), serde_json::to_string(&shared).unwrap()).unwrap();
        assert!(matches!(mine.beat(5010).unwrap(), LockState::Elsewhere(_)));
        assert_eq!(read_lock(&map).unwrap(), theirs);
        // dropping ours doesn't remove theirs
        drop(mine);
        assert!(lock_path(&map).exists());
        std::fs::remove_file(lock_path(&map)).unwrap();
        let mine = MapLock::take(&map, "laptop", 1).unwrap();
        drop(mine);
        assert!(!lock_path(&map).exists());
    }

    #[test]
    fn conflict_names() {
        let m = "Home network.nodo";
        assert!(is_conflict_copy(m, "Home network (Juan's conflicted copy 2026-11-20).nodo", false));
        assert!(is_conflict_copy(m, "Home network.sync-conflict-20261120-101010-ABCDEF.nodo", false));
        assert!(is_conflict_copy(m, "Home network 2.nodo", false));
        assert!(is_conflict_copy(m, "Home network (1).nodo", false));
        assert!(is_conflict_copy(m, "Home network-MacBook-Pro.nodo", true));
        assert!(!is_conflict_copy(m, "Home network-MacBook-Pro.nodo", false));
        assert!(!is_conflict_copy(m, "Home network.nodo", true));
        assert!(!is_conflict_copy(m, "Home network 2.csv", false));
        assert!(!is_conflict_copy(m, "Home networks.nodo", true));
        assert!(!is_conflict_copy(m, ".Home network.nodo.lock", true));
        assert!(!is_conflict_copy(m, "Office.nodo", true));
        let dir = tempfile::tempdir().unwrap();
        let map = dir.path().join(m);
        for f in [m, "Home network 2.nodo", "Home network (Ana's conflicted copy).nodo", "Other.nodo", ".Home network.nodo.lock"] {
            std::fs::write(dir.path().join(f), b"").unwrap();
        }
        let names: Vec<String> = conflict_copies(&map).iter().map(|p| file_name(p)).collect();
        assert_eq!(names, ["Home network (Ana's conflicted copy).nodo", "Home network 2.nodo"]);
    }
}
