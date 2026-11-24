//! An open map: the file plus its contents in memory, and every change written back at once
//! (no "save" step; each change is its own small transaction).
use crate::db::{MapData, Result, Store};
use crate::identity::{self, IngestReport};
use crate::model::*;
use std::net::IpAddr;
use std::path::Path;

pub struct Doc {
    pub store: Store,
    pub data: MapData,
    /// The device that is the computer running Nodo, worked out at runtime
    /// ([`crate::this_computer::identify`]); never written to the file.
    pub this_computer: Option<DeviceId>,
}

/// What [`Doc::apply`] did, for the UI and the alerts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Applied {
    pub report: IngestReport,
    /// New devices with a LAN address (candidates for "new unknown device" alerts).
    pub new_lan: Vec<DeviceId>,
    /// LAN devices the map had before this discovery.
    pub lan_before: usize,
}

impl Doc {
    pub fn create(path: &Path) -> Result<Doc> {
        let store = Store::create(path)?;
        Ok(Doc { store, data: MapData::default(), this_computer: None })
    }

    pub fn open(path: &Path) -> Result<Doc> {
        let store = Store::open(path)?;
        let data = store.load()?;
        let mut doc = Doc { store, data, this_computer: None };
        // maps written before addresses were classified by what they are
        let (mut devs, nets) = identity::reclassify(&mut doc.data);
        // maps that stored "this computer" as a fact (wrong once the file is synced elsewhere)
        for d in doc.data.devices.iter_mut().filter(|d| d.fact("this_computer").is_some()) {
            d.facts.retain(|(k, _)| k != "this_computer");
            devs.push(d.id);
        }
        doc.save_networks(&nets)?;
        doc.save_ids(&devs)?;
        Ok(doc)
    }

    /// Save As: copy the map to `dest` (checked, see [`crate::mapfile::copy_map`]) and carry
    /// on from there. The old file is left as it is; returns its path.
    pub fn save_as(&mut self, dest: &Path) -> std::result::Result<std::path::PathBuf, crate::mapfile::FileError> {
        crate::mapfile::copy_map(&self.store, dest)?;
        let store = Store::open(dest)?;
        let old = self.store.path().to_path_buf();
        self.store = store;
        Ok(old)
    }

    /// Rereads the map when the sync client put a new file at the path (see
    /// [`Store::refresh`]); true when it did.
    pub fn refresh(&mut self) -> Result<bool> {
        if !self.store.refresh()? {
            return Ok(false);
        }
        self.data = self.store.load()?;
        Ok(true)
    }

    pub fn title(&self) -> String {
        self.store.path().file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    }

    fn save_networks(&mut self, ids: &[NetworkId]) -> Result<()> {
        for id in ids {
            if let Some(n) = self.data.network(*id).cloned() {
                self.store.save_network(&n)?;
            }
        }
        Ok(())
    }

    fn save_ids(&mut self, ids: &[DeviceId]) -> Result<()> {
        for id in ids {
            if let Some(d) = self.data.device(*id).cloned() {
                self.store.save_device(&d)?;
            }
        }
        Ok(())
    }

    /// Merge a discovery into the map and write what changed, with history events.
    pub fn apply(&mut self, d: &Discovery, now: i64) -> Result<Applied> {
        let lan_before = self.data.devices.iter().filter(|x| x.layers().contains(&Layer::Lan)).count();
        let report = identity::ingest_as(&mut self.data, d, now, self.this_computer);
        // a new device for this computer (first run, or nothing matched before)
        if self.this_computer.is_none()
            && let Some(i) = d.devices.iter().position(|o| o.facts.iter().any(|(k, _)| k == "this_computer"))
            && let Some(id) = identity::find(&self.data.devices, &d.devices[i]).map(|x| self.data.devices[x].id)
        {
            self.this_computer = Some(id);
        }
        // networks first: addresses reference them
        let mut nets: Vec<Network> = report.networks.iter().filter_map(|id| self.data.network(*id).cloned()).collect();
        nets.sort_by_key(|n| n.parent.is_some());
        for n in &nets {
            self.store.save_network(n)?;
        }
        self.save_ids(&report.dirty())?;
        for id in &report.new {
            self.store.add_event(Some(*id), now, EventKind::Discovered, d.devices.iter().find_map(|o| o.source).map(|s| s.id()).unwrap_or_default())?;
        }
        for (id, what) in &report.ip_changes {
            self.store.add_event(Some(*id), now, EventKind::IpChanged, what)?;
        }
        let me = self.this_computer;
        let new_lan = report.new.iter().copied().filter(|id| Some(*id) != me && self.data.device(*id).is_some_and(|x| x.layers().contains(&Layer::Lan))).collect();
        Ok(Applied { report, new_lan, lan_before })
    }

    /// Replace a device with the user's edited copy.
    pub fn update_device(&mut self, d: Device) -> Result<()> {
        if let Some(x) = self.data.device_mut(d.id) {
            *x = d.clone();
        } else {
            self.data.devices.push(d.clone());
        }
        self.store.save_device(&d)
    }

    /// A device the user adds by hand (saved, monitored).
    pub fn add_manual(&mut self, name: &str, ip: Option<IpAddr>, kind: DeviceKind, now: i64) -> Result<DeviceId> {
        let id = self.data.next_device_id();
        let mut d = Device { id, name: name.to_string(), name_manual: !name.is_empty(), kind, kind_manual: kind != DeviceKind::Unknown, saved: true, monitor: true, first_seen: now, ..Default::default() };
        if let Some(ip) = ip {
            d.addrs.push(Addr { ip, layer: Layer::Lan, network: self.data.network_for(ip, Layer::Lan), source: Source::Manual, last_seen: 0 });
        }
        if d.name.is_empty() {
            d.name = identity::fallback_name(&d);
        }
        self.data.devices.push(d);
        // its layer comes from the address: tailnet, internet or a LAN subnet
        let (_, nets) = identity::reclassify(&mut self.data);
        self.save_networks(&nets)?;
        let d = self.data.device(id).cloned().unwrap_or_default();
        self.store.save_device(&d)?;
        self.store.add_event(Some(id), now, EventKind::Edited, "")?;
        Ok(id)
    }

    pub fn delete_device(&mut self, id: DeviceId) -> Result<()> {
        self.data.devices.retain(|d| d.id != id);
        self.store.delete_device(id)
    }

    pub fn merge(&mut self, keep: DeviceId, other: DeviceId, now: i64) -> Result<bool> {
        let other_name = self.data.device(other).map(|d| d.name.clone()).unwrap_or_default();
        if !identity::merge(&mut self.data, keep, other) {
            return Ok(false);
        }
        self.store.move_events(other, keep)?;
        self.store.delete_device(other)?;
        self.save_ids(&[keep])?;
        self.store.add_event(Some(keep), now, EventKind::Merged, &other_name)?;
        Ok(true)
    }

    pub fn split(&mut self, id: DeviceId, ips: &[IpAddr], idents: &[Ident], now: i64) -> Result<Option<DeviceId>> {
        let Some(new) = identity::split(&mut self.data, id, ips, idents, now) else { return Ok(None) };
        self.save_ids(&[id, new])?;
        self.store.add_event(Some(new), now, EventKind::Split, &id.to_string())?;
        Ok(Some(new))
    }

    /// Devices from a CSV import: matched like discovery, then saved with their inventory
    /// columns applied. Returns how many rows were imported.
    pub fn import(&mut self, rows: Vec<Observation>, now: i64) -> Result<usize> {
        let n = rows.len();
        let mut d = Discovery::default();
        let inv: Vec<Vec<(String, String)>> = rows.iter().map(|o| o.facts.iter().filter(|(k, _)| k.starts_with("inv:") || k.starts_with("field:")).cloned().collect()).collect();
        d.devices = rows
            .into_iter()
            .map(|mut o| {
                o.facts.retain(|(k, _)| !k.starts_with("inv:") && !k.starts_with("field:"));
                o
            })
            .collect();
        for (o, facts) in d.devices.clone().iter().zip(inv) {
            let one = Discovery { devices: vec![o.clone()], ..Default::default() };
            let a = self.apply(&one, now)?;
            let Some(id) = a.report.new.first().or(a.report.updated.first()).copied() else { continue };
            if let Some(dev) = self.data.device_mut(id) {
                dev.saved = true;
                dev.monitor = true;
                if o.name.is_some() {
                    dev.name_manual = true;
                }
                if o.kind.is_some_and(|k| k != DeviceKind::Unknown) {
                    dev.kind = o.kind.unwrap_or_default();
                    dev.kind_manual = true;
                }
                for (k, v) in facts {
                    match k.as_str() {
                        "inv:owner" => dev.owner = v,
                        "inv:location" => dev.location = v,
                        "inv:notes" => dev.notes = v,
                        "inv:tags" => dev.tags = v.split([';', ',']).map(str::trim).filter(|t| !t.is_empty()).map(str::to_string).collect(),
                        "inv:links" => {
                            dev.links = v
                                .split('|')
                                .filter_map(|l| {
                                    let l = l.trim();
                                    let (label, url) = l.rsplit_once(' ').unwrap_or(("", l));
                                    (!url.is_empty()).then(|| Link { label: label.trim().to_string(), url: url.to_string() })
                                })
                                .collect()
                        }
                        f => {
                            if let Some(name) = f.strip_prefix("field:") {
                                dev.fields.retain(|(k, _)| k != name);
                                dev.fields.push((name.to_string(), v));
                            }
                        }
                    }
                }
            }
            self.save_ids(&[id])?;
        }
        d.devices.clear();
        Ok(n)
    }

    /// Fold devices that are really this computer (its own address on a VM bridge seen by
    /// ARP or mDNS) into it. Returns how many were merged.
    pub fn absorb_into_this_computer(&mut self, me: &crate::this_computer::LocalIdentity, now: i64) -> Result<usize> {
        let Some(keep) = self.this_computer else { return Ok(0) };
        let mut n = 0;
        for other in crate::this_computer::absorb_targets(&self.data.devices, keep, me) {
            if self.merge(keep, other, now)? {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Name the guests on this computer's VM bridges after the runtime running them.
    pub fn label_vms(&mut self, me: &crate::this_computer::LocalIdentity, runtimes: &[crate::vms::Runtime]) -> Result<usize> {
        let bridges: Vec<crate::cidr::Cidr> = me.bridges().into_iter().map(|(_, c)| c).collect();
        let hosts: Vec<IpAddr> = me.bridges().into_iter().map(|(ip, _)| IpAddr::V4(ip)).collect();
        let names = crate::vms::label_guests(&self.data.devices, &bridges, &hosts, runtimes);
        for (id, name) in &names {
            if let Some(d) = self.data.device_mut(*id) {
                d.name = name.clone();
                // above the neighbour table's own guess, below mDNS / Tailscale names
                d.set_fact("name_rank", "3");
                if !d.kind_manual {
                    d.kind = DeviceKind::Vm;
                }
            }
        }
        self.save_ids(&names.iter().map(|(id, _)| *id).collect::<Vec<_>>())?;
        Ok(names.len())
    }

    pub fn rename_network(&mut self, id: NetworkId, name: &str) -> Result<()> {
        if let Some(n) = self.data.networks.iter_mut().find(|n| n.id == id) {
            n.name = name.to_string();
            let n = n.clone();
            self.store.save_network(&n)?;
        }
        Ok(())
    }

    /// Settings stored in the map file itself (alert thresholds, chosen AWS profile…).
    pub fn setting(&self, key: &str) -> Option<String> {
        self.store.meta(key).ok().flatten()
    }

    pub fn set_setting(&mut self, key: &str, value: &str) -> Result<()> {
        self.store.set_meta(key, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_as_carries_on_from_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tmp map.nodo");
        let mut doc = Doc::create(&path).unwrap();
        doc.add_manual("Router", Some("192.168.1.1".parse().unwrap()), DeviceKind::Router, 1).unwrap();
        let dest = dir.path().join("Casa.nodo");
        assert_eq!(doc.save_as(&dest).unwrap(), path);
        assert_eq!(doc.store.path(), dest);
        assert_eq!(doc.title(), "Casa");
        // later edits land in the new file only
        doc.add_manual("NAS", None, DeviceKind::Nas, 2).unwrap();
        assert_eq!(Doc::open(&dest).unwrap().data.devices.len(), 2);
        assert_eq!(Doc::open(&path).unwrap().data.devices.len(), 1);
    }

    #[test]
    fn apply_import_merge_persist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lab.nodo");
        let mut doc = Doc::create(&path).unwrap();
        let ts = crate::discover::tailscale::parse_status(crate::discover::tailscale::tests::STATUS).unwrap();
        let a = doc.apply(&ts, 1000).unwrap();
        assert_eq!(a.report.new.len(), 3);
        assert!(a.new_lan.is_empty(), "Tailscale nodes are not LAN devices");
        let csv = "name,ip,mac,type,owner,tags,Rack\nCore switch,192.168.1.2,00:11:22:33:44:55,switch,IT,core;lab,A1\n";
        assert_eq!(doc.import(crate::export::import_csv(csv).unwrap(), 1001).unwrap(), 1);
        let sw = doc.data.devices.iter().find(|d| d.name == "Core switch").unwrap().clone();
        assert!(sw.saved && sw.kind_manual);
        assert_eq!(sw.kind, DeviceKind::Switch);
        assert_eq!(sw.owner, "IT");
        assert_eq!(sw.tags, ["core", "lab"]);
        assert_eq!(sw.fields, vec![("Rack".to_string(), "A1".to_string())]);
        let pi = doc.data.devices.iter().find(|d| d.name == "pi").unwrap().id;
        assert!(doc.merge(sw.id, pi, 1002).unwrap());
        let id = doc.add_manual("Old printer", Some("192.168.1.250".parse().unwrap()), DeviceKind::Printer, 1003).unwrap();
        drop(doc);
        let doc = Doc::open(&path).unwrap();
        assert_eq!(doc.data.devices.len(), 4);
        assert_eq!(doc.data.networks.len(), 2, "the tailnet, and the subnet the typed addresses are in");
        assert!(doc.data.network_by_key("lan:192.168.1.0/24").is_some());
        let merged = doc.data.device(sw.id).unwrap();
        assert!(merged.ident(IdentKind::TailscaleNode).is_some());
        let ev = doc.store.events(Some(sw.id), 10).unwrap();
        assert!(ev.iter().any(|e| e.kind == EventKind::Merged && e.detail == "pi"));
        assert!(ev.iter().any(|e| e.kind == EventKind::Discovered), "pi's history moved over");
        assert_eq!(doc.data.device(id).unwrap().kind, DeviceKind::Printer);
        assert_eq!(doc.title(), "lab");
    }

    #[test]
    fn bridge_address_and_vm_guest() {
        use crate::this_computer::{IfaceKind, LocalIdentity, LocalIface};
        let dir = tempfile::tempdir().unwrap();
        let mut doc = Doc::create(&dir.path().join("vm.nodo")).unwrap();
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let me = LocalIdentity {
            hostname: Some("macbook-juan".into()),
            ifaces: vec![
                LocalIface { name: "en10".into(), kind: IfaceKind::Ethernet, mac: Some("00:e0:4c:53:63:88".into()), ips: vec![ip("192.168.40.7")], prefix: Some(24) },
                LocalIface { name: "bridge100".into(), kind: IfaceKind::Bridge, mac: Some("be:d0:74:00:00:64".into()), ips: vec![ip("192.168.64.1")], prefix: Some(24) },
            ],
            tailscale_node: None,
        };
        // an earlier ARP read saw the bridge address as its own device, and the VM
        let arp = |_: &str, m: &str| Observation::new(Layer::Lan, Source::Arp).ident(IdentKind::Mac, m);
        let mut stray = arp("", "be:d0:74:00:00:64");
        stray.ips.push(ip("192.168.64.1"));
        let mut vm = arp("", "aa:00:00:00:00:03");
        vm.ips.push(ip("192.168.64.3"));
        doc.apply(&Discovery { devices: vec![stray, vm], ..Default::default() }, 1).unwrap();
        assert_eq!(doc.data.devices.len(), 2);
        let net = crate::discover::netinfo::LocalNet { iface: "en10".into(), ip: "192.168.40.7".parse().unwrap(), prefix: 24, gateway: Some("192.168.40.1".parse().unwrap()), mac: None };
        let mut d = crate::discover::local(&net);
        d.networks.extend(crate::this_computer::bridge_networks(&me));
        d.devices = vec![crate::this_computer::observation(Some(&net), &me)];
        doc.apply(&d, 2).unwrap();
        let id = doc.this_computer.unwrap();
        assert_eq!(doc.absorb_into_this_computer(&me, 3).unwrap(), 1, "the stray bridge device folds into this computer");
        assert_eq!(doc.data.devices.len(), 2, "this computer and the VM");
        assert!(doc.data.device(id).unwrap().addrs.iter().any(|a| a.ip == ip("192.168.64.1")));
        let kiri = crate::vms::parse_ps("/Applications/Kiri.app/Contents/MacOS/kiri\n");
        assert_eq!(doc.label_vms(&me, &kiri).unwrap(), 1);
        let guest = doc.data.devices.iter().find(|d| d.id != id).unwrap();
        assert_eq!(guest.kind, DeviceKind::Vm);
        assert!(guest.name.contains("Kiri"));
        // the VM subnet hangs from this computer on the graph
        let g = crate::graph::build(&doc.data, &crate::graph::GraphOpts { this_computer: Some(id), ..Default::default() });
        let net = doc.data.network_by_key("lan:192.168.64.0/24").unwrap().id;
        let hub = g.index_of(crate::graph::NodeRef::Network(net)).unwrap();
        assert_eq!(g.nodes[hub].parent, g.index_of(crate::graph::NodeRef::Device(id)), "under this computer, not the gateway");
        let me_node = g.index_of(crate::graph::NodeRef::Device(id)).unwrap();
        assert!(!g.overlay.iter().any(|(d, h, _)| *d == me_node && *h == hub));
        let p = crate::layout::layout(&g, crate::layout::Mode::Radial);
        assert_eq!(p.pos.len(), g.nodes.len());
    }

    #[test]
    fn this_computer_is_not_stored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("home.nodo");
        let mut doc = Doc::create(&path).unwrap();
        let me = crate::this_computer::LocalIdentity { hostname: Some("pc-1".into()), ..Default::default() };
        let mut d = Discovery::default();
        d.devices.push(crate::this_computer::observation(None, &me));
        let a = doc.apply(&d, 10).unwrap();
        assert_eq!(a.report.new.len(), 1);
        assert!(a.new_lan.is_empty());
        let id = a.report.new[0];
        assert_eq!(doc.this_computer, Some(id), "known for this session");
        // a map written by an older version with the flag stored
        let mut dev = doc.data.device(id).unwrap().clone();
        dev.set_fact("this_computer", "1");
        doc.update_device(dev).unwrap();
        drop(doc);
        let doc = Doc::open(&path).unwrap();
        assert_eq!(doc.this_computer, None);
        assert!(doc.data.device(id).unwrap().fact("this_computer").is_none(), "the old flag is dropped on open");
    }
}
