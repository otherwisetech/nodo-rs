//! Where Nodo keeps its own small files (settings, recent maps, star cache), shared by the app
//! and the `nodo` CLI. Maps themselves live wherever the user saved them.
use std::path::{Path, PathBuf};

pub fn config_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::var_os("APPDATA").filter(|v| !v.is_empty()).map(|d| PathBuf::from(d).join("Nodo"));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        return home.map(|h| h.join("Library/Application Support/Nodo"));
    }
    std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| home.map(|h| h.join(".config"))).map(|d| d.join("nodo"))
}

const RECENT_MAX: usize = 10;

fn recent_file() -> Option<PathBuf> {
    config_dir().map(|d| d.join("recent.txt"))
}

pub fn parse_recent(s: &str) -> Vec<PathBuf> {
    s.lines().map(str::trim).filter(|l| !l.is_empty()).map(PathBuf::from).collect()
}

/// A recently opened map; `exists` is false when the file was moved or deleted (shown greyed
/// with "Locate…").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recent {
    pub path: PathBuf,
    pub exists: bool,
}

pub fn entries_from(list: Vec<PathBuf>, exists: impl Fn(&Path) -> bool) -> Vec<Recent> {
    list.into_iter().take(RECENT_MAX).map(|path| Recent { exists: exists(&path), path }).collect()
}

fn load_raw() -> Vec<PathBuf> {
    recent_file().and_then(|f| std::fs::read_to_string(f).ok()).map(|s| parse_recent(&s)).unwrap_or_default()
}

fn save_raw(list: &[PathBuf]) {
    let Some(f) = recent_file() else { return };
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(f, list.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n"));
}

/// Every remembered map, most recent first, missing ones included.
pub fn recent_entries() -> Vec<Recent> {
    entries_from(load_raw(), Path::exists)
}

/// Recently opened maps that still exist, most recent first.
pub fn recent() -> Vec<PathBuf> {
    recent_entries().into_iter().filter(|r| r.exists).map(|r| r.path).collect()
}

/// `path` moved to the front.
pub fn with_added(mut list: Vec<PathBuf>, path: &Path) -> Vec<PathBuf> {
    list.retain(|p| p != path);
    list.insert(0, path.to_path_buf());
    list.truncate(RECENT_MAX);
    list
}

/// `old` replaced by `new` in place (a moved or located map keeps its spot).
pub fn with_replaced(list: Vec<PathBuf>, old: &Path, new: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for p in list {
        let p = if p == old { new.to_path_buf() } else { p };
        if !out.contains(&p) {
            out.push(p);
        }
    }
    if !out.iter().any(|p| p == new) {
        out.insert(0, new.to_path_buf());
    }
    out.truncate(RECENT_MAX);
    out
}

pub fn add_recent(path: &Path) {
    save_raw(&with_added(load_raw(), path));
}

pub fn replace_recent(old: &Path, new: &Path) {
    save_raw(&with_replaced(load_raw(), old, new));
}

pub fn remove_recent(path: &Path) {
    let mut list = load_raw();
    list.retain(|p| p != path);
    save_raw(&list);
}

pub fn clear_recent() {
    if let Some(f) = recent_file() {
        let _ = std::fs::remove_file(f);
    }
}

/// System temporary folders: never a good default home for a map.
pub fn is_temp(path: &Path) -> bool {
    let tmp = std::env::temp_dir();
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let p = canon(path);
    path.starts_with(&tmp) || p.starts_with(canon(&tmp)) || ["/tmp", "/private/tmp", "/var/folders", "/private/var/folders", "/var/tmp"].iter().any(|t| p.starts_with(t)) // i18n-ok: paths
}

/// Where a new map is offered: the folder of the most recent map that still exists and isn't
/// in a temporary folder, else `fallback` (Documents).
pub fn default_dir(recent: &[Recent], fallback: PathBuf) -> PathBuf {
    recent.iter().filter(|r| r.exists).filter_map(|r| r.path.parent()).find(|d| d.is_dir() && !is_temp(d)).map(Path::to_path_buf).unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_maps_stay_listed() {
        let list = parse_recent("/a/one.nodo\n\n/b/two.nodo\n");
        let e = entries_from(list, |p| p.ends_with("one.nodo"));
        assert_eq!(e, vec![Recent { path: "/a/one.nodo".into(), exists: true }, Recent { path: "/b/two.nodo".into(), exists: false }]);
    }

    #[test]
    fn add_replace() {
        let list: Vec<PathBuf> = vec!["/a.nodo".into(), "/b.nodo".into(), "/c.nodo".into()];
        assert_eq!(with_added(list.clone(), Path::new("/c.nodo")), [PathBuf::from("/c.nodo"), "/a.nodo".into(), "/b.nodo".into()]);
        assert_eq!(with_replaced(list.clone(), Path::new("/b.nodo"), Path::new("/x/b.nodo")), [PathBuf::from("/a.nodo"), "/x/b.nodo".into(), "/c.nodo".into()]);
        // replacing with one already listed doesn't duplicate it
        assert_eq!(with_replaced(list.clone(), Path::new("/b.nodo"), Path::new("/a.nodo")), [PathBuf::from("/a.nodo"), "/c.nodo".into()]);
        assert_eq!(with_replaced(list, Path::new("/zzz.nodo"), Path::new("/new.nodo"))[0], PathBuf::from("/new.nodo"));
    }

    #[test]
    fn new_maps_avoid_temp_folders() {
        let docs = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let tmp_map = std::env::temp_dir().join("x.nodo");
        let real = docs.path().join("Home.nodo");
        let fallback = PathBuf::from("/fallback");
        assert!(is_temp(&tmp_map));
        assert!(!is_temp(&real));
        let r = |p: &Path, exists| Recent { path: p.to_path_buf(), exists };
        assert_eq!(default_dir(&[r(&tmp_map, true)], fallback.clone()), fallback);
        assert_eq!(default_dir(&[r(&tmp_map, true), r(&real, true)], fallback.clone()), docs.path());
        assert_eq!(default_dir(&[r(&real, false)], fallback.clone()), fallback);
    }
}
