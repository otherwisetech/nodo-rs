//! Nodo's background job for Otherwise Helper: `nodo background run monitor` (declared in the
//! bundle's `Contents/Resources/otherwise-background.json`) checks the most recent map's saved,
//! monitored devices and its enabled service checks every few minutes while the app is closed,
//! and says when one goes down or comes back.
//!
//! It only **reads** the map (`Store::open_read_only`); what it remembers between runs (up /
//! down per device and service) is its own small file, `<data root>/nodo/background.json`, keyed
//! by map. While the app is open it monitors the map itself and the helper doesn't run this.
//! The first run is a baseline: nothing is announced until a later run sees a change.
use crate::db::MapData;
use crate::model::DeviceId;
use crate::probe::Prober;
use crate::services::{ServiceCheck, ServiceId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const MONITOR: &str = "monitor";
pub const STATE_FILE: &str = "background.json";
const TIMEOUT: Duration = Duration::from_millis(1500);
/// A miss is probed once more after this, so one dropped packet isn't an outage.
pub const RETRY_AFTER: Duration = Duration::from_secs(2);
const CONCURRENCY: usize = 16;

/// Per map path: device / service id → up.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Memory {
    pub maps: BTreeMap<String, MapMemory>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MapMemory {
    pub devices: BTreeMap<DeviceId, bool>,
    pub services: BTreeMap<ServiceId, bool>,
}

pub fn state_path(root: &otherwise_link::Root) -> PathBuf {
    root.app_dir(crate::sync::APP).join(STATE_FILE)
}

impl Memory {
    pub fn load(path: &Path) -> Memory {
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        otherwise_link::fsutil::write_atomic(path, (serde_json::to_string_pretty(self).unwrap_or_default() + "\n").as_bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    DeviceDown { id: DeviceId, name: String },
    DeviceUp { id: DeviceId, name: String },
    ServiceDown { id: ServiceId, name: String, detail: String },
    ServiceUp { id: ServiceId, name: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Round {
    pub devices: usize,
    pub devices_down: usize,
    pub services: usize,
    pub services_down: usize,
    pub changes: Vec<Change>,
}

struct Target {
    id: DeviceId,
    name: String,
    ip: IpAddr,
    ports: Vec<u16>,
}

fn ports(d: &crate::model::Device) -> Vec<u16> {
    let mut p: Vec<u16> = d.services.iter().filter(|s| s.name.starts_with("tcp/") || s.name.starts_with('_')).filter_map(|s| s.port).filter(|p| *p > 0).collect();
    p.sort();
    p.dedup();
    if p.is_empty() {
        p = vec![22, 80, 443];
    }
    p
}

/// Probes `items` with at most [`CONCURRENCY`] threads.
fn parallel<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let mut out = Vec::with_capacity(items.len());
    for chunk in items.chunks(CONCURRENCY) {
        let rs: Vec<R> = std::thread::scope(|s| {
            let hs: Vec<_> = chunk.iter().map(|x| s.spawn(|| f(x))).collect();
            hs.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        out.extend(rs);
    }
    out
}

/// One monitoring round over `map`: saved, monitored devices with an address, and enabled
/// services. `memory` is updated; `changes` lists what moved since the last round.
pub fn round(key: &str, map: &MapData, services: &[ServiceCheck], memory: &mut Memory, prober: &dyn Prober, retry_after: Duration) -> Round {
    let targets: Vec<Target> = map.devices.iter().filter(|d| d.saved && d.monitor).filter_map(|d| Some(Target { id: d.id, name: d.name.clone(), ip: d.primary_ip()?, ports: ports(d) })).collect();
    let reach = |t: &Target| prober.reach(t.ip, &t.ports, TIMEOUT).is_some();
    let mut up: Vec<bool> = parallel(&targets, reach);
    let missed: Vec<usize> = up.iter().enumerate().filter(|(_, u)| !**u).map(|(i, _)| i).collect();
    if !missed.is_empty() {
        std::thread::sleep(retry_after);
        let again = parallel(&missed.iter().map(|&i| &targets[i]).collect::<Vec<_>>(), |t| reach(t));
        for (k, &i) in missed.iter().enumerate() {
            up[i] = again.get(k).copied().unwrap_or(false);
        }
    }
    let enabled: Vec<&ServiceCheck> = services.iter().filter(|s| s.enabled).collect();
    let outcomes = parallel(&enabled, |s| crate::services::check(&s.spec, prober));
    let mem = memory.maps.entry(key.to_string()).or_default();
    let mut r = Round { devices: targets.len(), services: enabled.len(), ..Default::default() };
    for (t, &ok) in targets.iter().zip(&up) {
        if !ok {
            r.devices_down += 1;
        }
        match mem.devices.insert(t.id, ok) {
            Some(true) if !ok => r.changes.push(Change::DeviceDown { id: t.id, name: t.name.clone() }),
            Some(false) if ok => r.changes.push(Change::DeviceUp { id: t.id, name: t.name.clone() }),
            _ => {}
        }
    }
    for (s, o) in enabled.iter().zip(&outcomes) {
        if !o.ok {
            r.services_down += 1;
        }
        match mem.services.insert(s.id, o.ok) {
            Some(true) if !o.ok => r.changes.push(Change::ServiceDown { id: s.id, name: s.name.clone(), detail: o.detail.clone() }),
            Some(false) if o.ok => r.changes.push(Change::ServiceUp { id: s.id, name: s.name.clone() }),
            _ => {}
        }
    }
    // forget what left the map
    let ids: std::collections::BTreeSet<DeviceId> = targets.iter().map(|t| t.id).collect();
    mem.devices.retain(|id, _| ids.contains(id));
    let sids: std::collections::BTreeSet<ServiceId> = enabled.iter().map(|s| s.id).collect();
    mem.services.retain(|id, _| sids.contains(id));
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Device, DeviceKind};
    use crate::probe::Tcp;
    use std::sync::Mutex;

    struct Fake {
        up: Mutex<Vec<IpAddr>>,
    }

    impl Prober for Fake {
        fn ping(&self, ip: IpAddr, _: Duration) -> Option<f64> {
            self.up.lock().unwrap().contains(&ip).then_some(3.0)
        }
        fn tcp(&self, _: IpAddr, _: u16, _: Duration) -> (Tcp, f64) {
            (Tcp::Silent, 0.0)
        }
    }

    fn device(id: DeviceId, name: &str, ip: &str) -> Device {
        let mut d = Device { id, name: name.into(), kind: DeviceKind::Server, saved: true, monitor: true, ..Default::default() };
        d.addrs.push(crate::model::Addr { ip: ip.parse().unwrap(), layer: crate::model::Layer::Lan, network: None, source: crate::model::Source::Arp, last_seen: 0 });
        d
    }

    #[test]
    fn baseline_then_down_and_up() {
        let map = MapData { networks: vec![], devices: vec![device(1, "NAS", "10.0.0.2"), device(2, "Printer", "10.0.0.3"), Device { id: 3, saved: false, monitor: true, ..device(3, "Guest", "10.0.0.9") }] };
        let net = Fake { up: Mutex::new(vec!["10.0.0.2".parse().unwrap(), "10.0.0.3".parse().unwrap()]) };
        let mut mem = Memory::default();
        let r = round("m", &map, &[], &mut mem, &net, Duration::ZERO);
        assert_eq!((r.devices, r.devices_down, r.changes.len()), (2, 0, 0), "baseline, unsaved devices left out");
        net.up.lock().unwrap().retain(|ip| ip.to_string() != "10.0.0.3");
        let r = round("m", &map, &[], &mut mem, &net, Duration::ZERO);
        assert_eq!(r.changes, [Change::DeviceDown { id: 2, name: "Printer".into() }]);
        let r = round("m", &map, &[], &mut mem, &net, Duration::ZERO);
        assert!(r.changes.is_empty() && r.devices_down == 1, "announced once");
        net.up.lock().unwrap().push("10.0.0.3".parse().unwrap());
        let r = round("m", &map, &[], &mut mem, &net, Duration::ZERO);
        assert_eq!(r.changes, [Change::DeviceUp { id: 2, name: "Printer".into() }]);
        // state round trip
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("nodo").join(STATE_FILE);
        mem.save(&p).unwrap();
        assert_eq!(Memory::load(&p), mem);
    }
}
