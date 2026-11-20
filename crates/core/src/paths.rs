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

/// Recently opened maps, most recent first, only those that still exist.
pub fn recent() -> Vec<PathBuf> {
    recent_file().and_then(|f| std::fs::read_to_string(f).ok()).map(|s| parse_recent(&s).into_iter().filter(|p| p.exists()).take(RECENT_MAX).collect()).unwrap_or_default()
}

pub fn add_recent(path: &Path) {
    let Some(f) = recent_file() else { return };
    let mut list = recent();
    list.retain(|p| p != path);
    list.insert(0, path.to_path_buf());
    list.truncate(RECENT_MAX);
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(f, list.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n"));
}

pub fn clear_recent() {
    if let Some(f) = recent_file() {
        let _ = std::fs::remove_file(f);
    }
}
