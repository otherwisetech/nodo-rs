//! What the AI panel's assistant may change, as proposals. `nodo mcp --propose <folder>` turns
//! its write tools (`update_device`, `add_service`) into a checked [`Proposal`] dropped in that
//! folder; the app shows it as a card and applies it, built the same way, only when the user
//! accepts. `--allow-writes` (Otherwise Hub's hub) uses the same builders and saves at once.
use crate::db::MapData;
use crate::model::Device;
use crate::services::{CheckKind, ServiceCheck, Spec};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const WRITE_TOOLS: &[&str] = &["update_device", "add_service"];

fn arg<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty())
}

/// A device by id, name, IP, MAC or host name; else the one device whose name contains `q`.
pub fn find_device<'a>(m: &'a MapData, q: &str) -> Result<&'a Device, String> {
    let ql = q.to_lowercase();
    let mac = crate::mac::normalize(q);
    let exact: Vec<&Device> = m
        .devices
        .iter()
        .filter(|d| d.id.to_string() == q || d.name.to_lowercase() == ql || d.ips().any(|i| i.to_string() == q) || mac.as_deref().is_some_and(|mm| d.macs().any(|x| x == mm)) || d.hostname.as_deref().is_some_and(|h| h.to_lowercase() == ql))
        .collect();
    if let [d] = exact.as_slice() {
        return Ok(d);
    }
    let fuzzy: Vec<&Device> = m.devices.iter().filter(|d| d.name.to_lowercase().contains(&ql)).collect();
    match fuzzy.as_slice() {
        [d] => Ok(d),
        [] if exact.is_empty() => Err(i18n::t!("cli.err.no_device", query = q)),
        _ => Err(i18n::t!("cli.err.ambiguous", query = q, n = exact.len().max(fuzzy.len()))),
    }
}

/// `update_device`'s arguments → the device as it would be saved (not saved).
pub fn edited_device(m: &MapData, a: &Value) -> Result<Device, String> {
    let mut d = find_device(m, arg(a, "device").unwrap_or_default())?.clone();
    if let Some(v) = arg(a, "name") {
        d.name = v.into();
        d.name_manual = true;
    }
    for (k, f) in [("owner", &mut d.owner), ("location", &mut d.location), ("notes", &mut d.notes)] {
        if let Some(v) = a.get(k).and_then(Value::as_str) {
            *f = v.trim().to_string();
        }
    }
    if let Some(s) = a.get("saved").and_then(Value::as_bool) {
        d.saved = s;
    }
    Ok(d)
}

/// `add_service`'s arguments → the check it would add (id 0, not saved).
pub fn new_service(m: &MapData, a: &Value) -> Result<ServiceCheck, String> {
    let kind = arg(a, "kind").and_then(CheckKind::from_id).ok_or_else(|| i18n::t!("cli.svc.add_needs"))?;
    let target = arg(a, "target").ok_or_else(|| i18n::t!("cli.svc.add_needs"))?;
    let device = match arg(a, "device") {
        Some(q) => Some(find_device(m, q)?.id),
        None => None,
    };
    Ok(ServiceCheck { id: 0, name: arg(a, "name").unwrap_or(target).to_string(), device, spec: Spec::new(kind, target), enabled: true })
}

/// A write tool call waiting for the user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    pub tool: String,
    pub args: Value,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// Drops `p` in `dir` (written beside it first, then renamed, so the app never reads half a file).
pub fn deliver(dir: &Path, p: &Proposal) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    let name = format!("{stamp:032}-{}.json", std::process::id());
    let tmp = dir.join(format!(".{name}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec(p).map_err(std::io::Error::other)?)?;
    let path = dir.join(name);
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Takes every delivered proposal out of `dir`, oldest first; unreadable files are dropped.
pub fn collect(dir: &Path) -> Vec<Proposal> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json") && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))).collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|p| {
            let v = std::fs::read(&p).ok();
            let _ = std::fs::remove_file(&p);
            serde_json::from_slice(&v?).ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map() -> MapData {
        let mut m = MapData::default();
        m.devices.push(Device { id: 1, name: "Router".into(), ..Default::default() });
        m.devices.push(Device { id: 2, name: "Phone Ana".into(), ..Default::default() });
        m.devices.push(Device { id: 3, name: "Phone Luis".into(), ..Default::default() });
        m
    }

    #[test]
    fn edits_are_built_without_saving() {
        let m = map();
        let d = edited_device(&m, &json!({"device": "router", "name": "Main router", "location": " Office ", "saved": true})).unwrap();
        assert_eq!((d.id, d.name.as_str(), d.location.as_str(), d.saved, d.name_manual), (1, "Main router", "Office", true, true));
        assert_eq!(m.devices[0].name, "Router", "the map is untouched");
        assert!(edited_device(&m, &json!({"device": "phone"})).is_err(), "ambiguous");
        assert!(edited_device(&m, &json!({"device": "printer"})).is_err());
    }

    #[test]
    fn services_need_a_kind_and_a_target() {
        let m = map();
        let s = new_service(&m, &json!({"kind": "http", "target": "https://example.com", "device": "1"})).unwrap();
        assert_eq!((s.id, s.name.as_str(), s.device), (0, "https://example.com", Some(1)));
        assert!(new_service(&m, &json!({"kind": "smtp", "target": "x"})).is_err());
        assert!(new_service(&m, &json!({"kind": "tcp"})).is_err());
    }

    #[test]
    fn proposals_are_collected_once_in_order() {
        let d = tempfile::tempdir().unwrap();
        let a = Proposal { tool: "update_device".into(), args: json!({"device": "1", "name": "x"}), reason: String::new() };
        let b = Proposal { tool: "add_service".into(), args: json!({"kind": "icmp", "target": "10.0.0.1"}), reason: "watch it".into() };
        deliver(d.path(), &a).unwrap();
        deliver(d.path(), &b).unwrap();
        assert_eq!(collect(d.path()), vec![a, b]);
        assert!(collect(d.path()).is_empty());
    }
}
