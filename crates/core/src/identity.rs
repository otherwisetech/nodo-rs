//! Turning observations into devices: which device an observation belongs to (by MAC,
//! Tailscale node, AWS instance, WireGuard key, mDNS name, then IP, then an unambiguous host
//! name), merging what it says into the device, and the user's own merge / split.
use crate::db::MapData;
use crate::model::*;
use std::net::IpAddr;

/// Kinds compared for conflicts: a device and an observation that both carry one of these but
/// share no value are different machines, whatever their IPs say.
const EXCLUSIVE: [IdentKind; 5] = [IdentKind::Mac, IdentKind::TailscaleNode, IdentKind::AwsInstance, IdentKind::WgPeer, IdentKind::Mdns];

fn conflicts(d: &Device, idents: &[Ident]) -> bool {
    EXCLUSIVE.iter().any(|k| {
        let a: Vec<&str> = d.idents.iter().filter(|i| i.kind == *k).map(|i| i.value.as_str()).collect();
        let b: Vec<&str> = idents.iter().filter(|i| i.kind == *k).map(|i| i.value.as_str()).collect();
        !a.is_empty() && !b.is_empty() && !a.iter().any(|x| b.contains(x))
    })
}

/// Index of the device `o` describes, if any.
pub fn find(devices: &[Device], o: &Observation) -> Option<usize> {
    // 1. a strong identity, strongest first
    let mut best: Option<(u8, usize)> = None;
    for id in o.idents.iter().filter(|i| i.kind != IdentKind::Host) {
        if let Some(i) = devices.iter().position(|d| d.idents.contains(id)) {
            let s = id.kind.strength();
            if best.is_none_or(|(b, _)| s > b) {
                best = Some((s, i));
            }
        }
    }
    if let Some((_, i)) = best {
        return Some(i);
    }
    // 2. the device holding most of its addresses on the same layer (IPv4 counts most;
    //    link-local IPv6 like fe80::1 is shared by routers and loopbacks, so it counts least),
    //    unless the identities disagree (IP reassigned)
    let weight = |ip: &IpAddr| match ip {
        IpAddr::V4(_) => 4,
        // never enough on its own: routers and macOS's loopback both use fe80::1
        IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80 => 0,
        IpAddr::V6(_) => 2,
    };
    let mut best: Option<(u32, usize)> = None;
    for (i, d) in devices.iter().enumerate() {
        let score: u32 = o.ips.iter().filter(|ip| d.addrs.iter().any(|a| a.ip == **ip && a.layer == o.layer())).map(weight).sum();
        if score > 0 && !conflicts(d, &o.idents) && best.is_none_or(|(b, _)| score > b) {
            best = Some((score, i));
        }
    }
    if let Some((_, i)) = best {
        return Some(i);
    }
    let unique = |hits: Vec<usize>| if let [one] = hits.as_slice() { Some(*one) } else { None };
    let hinted = |d: &Device| d.addrs.iter().any(|a| a.layer == Layer::Lan && o.lan_hints.contains(&a.ip));
    // 3. this computer seen by another source (its Tailscale node and its LAN interface); with
    //    hints, only if they agree (a synced map may have been made on another computer)
    if o.facts.iter().any(|(k, _)| k == "this_computer")
        && let Some(i) = unique(devices.iter().enumerate().filter(|(_, d)| d.fact("this_computer").is_some() && (o.lan_hints.is_empty() || hinted(d)) && !conflicts(d, &o.idents)).map(|(i, _)| i).collect())
    {
        return Some(i);
    }
    // 4. the one LAN device holding an address the source says the machine has
    if let Some(i) = unique(devices.iter().enumerate().filter(|(_, d)| hinted(d) && !conflicts(d, &o.idents)).map(|(i, _)| i).collect()) {
        return Some(i);
    }
    // 5. a host name only one device has (joins a LAN machine with its Tailscale node)
    for id in o.idents.iter().filter(|i| i.kind == IdentKind::Host && !generic_host(&i.value)) {
        let hits: Vec<usize> = devices.iter().enumerate().filter(|(_, d)| d.idents.contains(id) && !conflicts(d, &o.idents)).map(|(i, _)| i).collect();
        if let [one] = hits.as_slice() {
            return Some(*one);
        }
    }
    None
}

/// Names too common to identify anything.
pub(crate) fn generic_host(h: &str) -> bool {
    matches!(h, "" | "localhost" | "raspberrypi" | "android" | "iphone" | "ipad" | "ubuntu" | "debian" | "unknown" | "esp32" | "esp8266" | "router" | "gateway")
}

/// How much a source's name counts; a better-ranked name replaces a worse one.
fn name_rank(src: Source) -> u8 {
    match src {
        Source::Manual | Source::Import => 9,
        Source::Aws => 7,
        Source::Tailscale => 6,
        Source::Mdns => 5,
        Source::Ssdp => 4,
        Source::WireGuard => 2,
        _ => 1,
    }
}

/// What changed in one ingest.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IngestReport {
    pub new: Vec<DeviceId>,
    pub updated: Vec<DeviceId>,
    pub networks: Vec<NetworkId>,
    /// (device, "old → new")
    pub ip_changes: Vec<(DeviceId, String)>,
    /// Devices that lost an address to another device (also need saving).
    pub touched: Vec<DeviceId>,
}

impl IngestReport {
    /// Every device id to write back.
    pub fn dirty(&self) -> Vec<DeviceId> {
        let mut v: Vec<DeviceId> = self.new.iter().chain(&self.updated).chain(&self.touched).copied().collect();
        v.sort();
        v.dedup();
        v
    }
}

/// Networks first (so addresses can point at them), then devices.
pub fn ingest(map: &mut MapData, d: &Discovery, now: i64) -> IngestReport {
    let mut rep = IngestReport::default();
    for n in &d.networks {
        let id = upsert_network(map, n);
        if !rep.networks.contains(&id) {
            rep.networks.push(id);
        }
    }
    // parents may come after children in the list
    for n in &d.networks {
        if let Some(pk) = &n.parent_key {
            let parent = map.network_by_key(pk).map(|p| p.id);
            if let Some(net) = map.networks.iter_mut().find(|x| x.key == n.key) {
                net.parent = parent;
            }
        }
    }
    for o in &d.devices {
        ingest_one(map, o, now, &mut rep);
    }
    let (devs, nets) = reclassify(map);
    rep.updated.extend(devs);
    rep.networks.extend(nets);
    rep.updated.retain(|id| !rep.new.contains(id));
    rep.updated.sort();
    rep.updated.dedup();
    rep
}

fn upsert_network(map: &mut MapData, n: &NetworkObservation) -> NetworkId {
    if let Some(x) = map.networks.iter_mut().find(|x| x.key == n.key) {
        if x.cidr.is_none() || n.cidr.is_some() {
            x.cidr = n.cidr.clone().or(x.cidr.take());
        }
        if n.gateway.is_some() {
            x.gateway = n.gateway;
        }
        // AWS / Tailscale names follow the source; a LAN name may have been edited by the user
        if x.layer != Layer::Lan && !n.name.is_empty() {
            x.name = n.name.clone();
        }
        for (k, v) in &n.facts {
            match x.facts.iter_mut().find(|(a, _)| a == k) {
                Some(f) => f.1 = v.clone(),
                None => x.facts.push((k.clone(), v.clone())),
            }
        }
        return x.id;
    }
    let id = map.next_network_id();
    map.networks.push(Network { id, name: n.name.clone(), layer: n.layer.unwrap_or(Layer::Lan), cidr: n.cidr.clone(), gateway: n.gateway, parent: None, key: n.key.clone(), facts: n.facts.clone() });
    id
}

fn ingest_one(map: &mut MapData, o: &Observation, now: i64, rep: &mut IngestReport) {
    if o.ips.is_empty() && o.idents.is_empty() {
        return;
    }
    let layer = o.layer();
    let src = o.source();
    let seen = o.seen_at.unwrap_or(now);
    let network_of = |map: &MapData, ip: IpAddr| -> Option<NetworkId> {
        // the source's own network when it contains the address (or has no CIDR); else by CIDR
        let own = o.network.as_deref().and_then(|k| map.network_by_key(k));
        match own {
            Some(n) if n.cidr.as_deref().and_then(crate::cidr::Cidr::parse).is_none_or(|c| c.contains(ip)) => Some(n.id),
            // an IPv4 outside the source's IPv4 subnet is elsewhere (a VM bridge): `reclassify`
            // finds its subnet; an IPv6 on a network known by its IPv4 prefix stays with it
            _ => map.network_for(ip, layer).or(own.filter(|n| n.cidr.as_deref().and_then(crate::cidr::Cidr::parse).is_some_and(|c| c.addr.is_ipv4() != ip.is_ipv4())).map(|n| n.id)),
        }
    };
    let found = find(&map.devices, o);
    // an address-less record only adds to a device we already know
    if found.is_none() && o.ips.is_empty() && !matches!(src, Source::Manual | Source::Import) {
        return;
    }
    let idx = match found {
        Some(i) => {
            rep.updated.push(map.devices[i].id);
            i
        }
        None => {
            let id = map.next_device_id();
            map.devices.push(Device { id, first_seen: seen, kind: DeviceKind::Unknown, ..Default::default() });
            rep.new.push(id);
            map.devices.len() - 1
        }
    };
    let dev_id = map.devices[idx].id;
    let strong = matches!(src, Source::Tailscale | Source::Aws | Source::WireGuard) || o.idents.iter().any(|i| matches!(i.kind, IdentKind::Mac | IdentKind::TailscaleNode | IdentKind::AwsInstance | IdentKind::WgPeer));
    // an observation without a hardware identity (mDNS, SSDP, scan) can't prove an address
    // moved: leave addresses other devices hold alone
    let ips: Vec<IpAddr> = o.ips.iter().copied().filter(|ip| strong || !map.devices.iter().enumerate().any(|(j, d)| j != idx && d.addrs.iter().any(|a| a.ip == *ip && a.layer == layer))).collect();
    // the same address on the same layer elsewhere belonged to another machine: take it away
    for ip in &ips {
        for (j, other) in map.devices.iter_mut().enumerate() {
            if j != idx && other.addrs.iter().any(|a| a.ip == *ip && a.layer == layer) {
                other.addrs.retain(|a| !(a.ip == *ip && a.layer == layer));
                rep.touched.push(other.id);
            }
        }
    }
    let nets: Vec<Option<NetworkId>> = ips.iter().map(|ip| network_of(map, *ip)).collect();
    let d = &mut map.devices[idx];
    // authoritative sources replace the device's addresses on their layer; ARP with a MAC
    // replaces its LAN IPv4 on the same network (DHCP gave it a new one)
    let authoritative = matches!(src, Source::Tailscale | Source::Aws | Source::WireGuard);
    let has_mac = o.idents.iter().any(|i| i.kind == IdentKind::Mac);
    let before: Vec<IpAddr> = d.addrs.iter().filter(|a| a.layer == layer && a.ip.is_ipv4()).map(|a| a.ip).collect();
    if authoritative && !ips.is_empty() {
        d.addrs.retain(|a| a.layer != layer || ips.contains(&a.ip));
    } else if src == Source::Arp && has_mac {
        let new_v4: Vec<(IpAddr, Option<NetworkId>)> = ips.iter().zip(&nets).filter(|(ip, _)| ip.is_ipv4()).map(|(ip, n)| (*ip, *n)).collect();
        if !new_v4.is_empty() {
            d.addrs.retain(|a| !(a.layer == layer && a.ip.is_ipv4() && new_v4.iter().any(|(_, n)| *n == a.network) && !new_v4.iter().any(|(ip, _)| *ip == a.ip)));
        }
    }
    for (ip, net) in ips.iter().zip(&nets) {
        match d.addrs.iter_mut().find(|a| a.ip == *ip && a.layer == layer) {
            Some(a) => {
                a.last_seen = a.last_seen.max(seen);
                if net.is_some() {
                    a.network = *net;
                }
            }
            None => d.addrs.push(Addr { ip: *ip, layer, network: *net, source: src, last_seen: seen }),
        }
    }
    let after: Vec<IpAddr> = d.addrs.iter().filter(|a| a.layer == layer && a.ip.is_ipv4()).map(|a| a.ip).collect();
    let gone: Vec<String> = before.iter().filter(|ip| !after.contains(ip)).map(|ip| ip.to_string()).collect();
    let added: Vec<String> = after.iter().filter(|ip| !before.contains(ip)).map(|ip| ip.to_string()).collect();
    if !gone.is_empty() && !added.is_empty() {
        rep.ip_changes.push((dev_id, format!("{} → {}", gone.join(", "), added.join(", ")))); // i18n-ok: addresses
    }
    for id in &o.idents {
        if !d.idents.contains(id) {
            d.idents.push(id.clone());
        }
    }
    d.idents.sort();
    // names
    let rank: u8 = d.fact("name_rank").and_then(|r| r.parse().ok()).unwrap_or(0);
    let candidate = o.name.clone().filter(|n| !n.trim().is_empty()).map(|n| (n, name_rank(src))).or_else(|| o.hostname.as_deref().map(host_label).filter(|h| !h.is_empty() && !generic_host(h)).map(|h| (h, 1)));
    if !d.name_manual
        && let Some((n, r)) = candidate
        && (r >= rank || d.name.is_empty())
    {
        d.name = n;
        d.set_fact("name_rank", &r.to_string());
    }
    if let Some(h) = &o.hostname {
        d.hostname = Some(h.clone());
    }
    // the source's owner until the user types one (`owner_auto` remembers what we filled in)
    if let Some(owner) = o.owner.as_deref().filter(|w| !w.is_empty())
        && (d.owner.is_empty() || d.fact("owner_auto") == Some(d.owner.as_str()))
    {
        d.owner = owner.to_string();
        d.set_fact("owner_auto", owner);
    }
    if let Some(os) = &o.os {
        d.os = Some(os.clone());
    }
    if let Some(m) = &o.model {
        d.model = Some(m.clone());
    }
    if let Some(v) = &o.vendor {
        d.vendor = Some(v.clone());
    } else if d.vendor.is_none() {
        let v = d.macs().find_map(crate::mac::vendor).map(str::to_string);
        d.vendor = v;
    }
    for s in &o.services {
        if !d.services.contains(s) {
            d.services.push(s.clone());
        }
    }
    d.services.sort();
    for (k, v) in &o.facts {
        d.set_fact(k, v);
    }
    if o.online.is_some() {
        d.online = o.online;
    }
    if o.online != Some(false) {
        d.last_seen = d.last_seen.max(seen);
    }
    if d.name.is_empty() {
        d.name = fallback_name(d);
    }
    let gw = map.is_gateway(&map.devices[idx]);
    let d = &mut map.devices[idx];
    if !d.kind_manual {
        let g = crate::discover::classify::guess(d, gw);
        d.kind = match (o.kind, g) {
            (Some(DeviceKind::Vpn), _) => DeviceKind::Vpn,
            (_, k) if k != DeviceKind::Unknown => k,
            (Some(k), _) => k,
            _ => d.kind,
        };
    }
}

/// "Synology 192.168.1.10", or the address alone.
pub const INTERNET_KEY: &str = "internet";

/// What the addresses themselves say, over what the source assumed: an address recorded on the
/// LAN that is a tailnet address moves to the Tailscale layer, a public one the user typed or
/// imported moves to the Internet layer (a neighbour-table or mDNS sighting is on-link, so a
/// public address seen there stays local), and a private IPv4 outside every known LAN subnet
/// gets its own subnet (a VM or container bridge is not the home LAN). Returns the devices and
/// networks it changed. Idempotent; runs after each discovery and when a map is opened.
pub fn reclassify(map: &mut MapData) -> (Vec<DeviceId>, Vec<NetworkId>) {
    use crate::cidr::{AddrClass, Cidr, classify};
    let mut devs = Vec::new();
    let mut nets = Vec::new();
    for i in 0..map.devices.len() {
        let mut changed = false;
        for k in 0..map.devices[i].addrs.len() {
            let a = map.devices[i].addrs[k].clone();
            if a.layer != Layer::Lan {
                continue;
            }
            let typed = matches!(a.source, Source::Manual | Source::Import);
            let (layer, network) = match classify(a.ip) {
                AddrClass::Tailnet => (Layer::Tailscale, map.network_for(a.ip, Layer::Tailscale)),
                AddrClass::Public if typed => (Layer::Internet, Some(internet_network(map, &mut nets))),
                _ => {
                    let fits = a.network.and_then(|n| map.network(n)).is_some_and(|n| n.layer == Layer::Lan && n.cidr.as_deref().and_then(Cidr::parse).is_none_or(|c| c.contains(a.ip) || c.addr.is_ipv4() != a.ip.is_ipv4()));
                    // no subnet to guess (IPv6, link-local): leave it where the source put it
                    let net = if fits { a.network } else { map.network_for(a.ip, Layer::Lan).or_else(|| subnet_network(map, a.ip, &mut nets)).or(a.network) };
                    (Layer::Lan, net)
                }
            };
            if layer != a.layer || network != a.network {
                let x = &mut map.devices[i].addrs[k];
                x.layer = layer;
                x.network = network;
                changed = true;
            }
        }
        if changed {
            let d = &mut map.devices[i];
            let mut seen = Vec::new();
            d.addrs.retain(|a| {
                let k = (a.ip, a.layer);
                let fresh = !seen.contains(&k);
                seen.push(k);
                fresh
            });
            devs.push(d.id);
        }
    }
    (devs, nets)
}

fn internet_network(map: &mut MapData, created: &mut Vec<NetworkId>) -> NetworkId {
    if let Some(n) = map.network_by_key(INTERNET_KEY) {
        return n.id;
    }
    let id = map.next_network_id();
    map.networks.push(Network { id, name: i18n::t!("core.net.internet"), layer: Layer::Internet, cidr: None, gateway: None, parent: None, key: INTERNET_KEY.into(), facts: Vec::new() });
    created.push(id);
    id
}

/// The private IPv4 subnet `ip` is in when no known LAN network holds it: a known VM bridge, else
/// its /24. Not for link-local or IPv6 (no subnet to guess).
fn subnet_network(map: &mut MapData, ip: IpAddr, created: &mut Vec<NetworkId>) -> Option<NetworkId> {
    let IpAddr::V4(v4) = ip else { return None };
    if !v4.is_private() {
        return None;
    }
    let bridge = crate::cidr::vm_bridge(ip);
    let cidr = bridge.unwrap_or_else(|| crate::cidr::Cidr::v4(v4, 24)).to_string();
    let key = format!("lan:{cidr}");
    if let Some(n) = map.network_by_key(&key) {
        return Some(n.id);
    }
    let id = map.next_network_id();
    let (name, facts) = match bridge {
        Some(_) => (i18n::t!("core.net.vm_bridge", cidr = cidr.clone()), vec![("bridge".to_string(), "vm".to_string())]),
        None => (cidr.clone(), Vec::new()),
    };
    map.networks.push(Network { id, name, layer: Layer::Lan, cidr: Some(cidr), gateway: None, parent: None, key, facts });
    created.push(id);
    Some(id)
}

pub fn fallback_name(d: &Device) -> String {
    let ip = d.primary_ip().map(|i| i.to_string()).unwrap_or_default();
    match d.vendor.as_deref().map(crate::mac::short_vendor) {
        Some(v) if !v.is_empty() && !ip.is_empty() => format!("{v} {ip}"),
        _ if !ip.is_empty() => ip,
        _ => d.idents.first().map(|i| i.value.clone()).unwrap_or_else(|| "?".into()),
    }
}

/// Fold `other` into `keep` (the user's merge). The kept device's own inventory fields win;
/// lists are joined. Returns false if either is missing or they are the same.
pub fn merge(map: &mut MapData, keep: DeviceId, other: DeviceId) -> bool {
    if keep == other {
        return false;
    }
    let Some(oi) = map.devices.iter().position(|d| d.id == other) else { return false };
    if map.device(keep).is_none() {
        return false;
    }
    let o = map.devices.remove(oi);
    let k = map.device_mut(keep).expect("checked");
    for a in o.addrs {
        if !k.addrs.iter().any(|x| x.ip == a.ip && x.layer == a.layer) {
            k.addrs.push(a);
        }
    }
    for i in o.idents {
        if !k.idents.contains(&i) {
            k.idents.push(i);
        }
    }
    k.idents.sort();
    for s in o.services {
        if !k.services.contains(&s) {
            k.services.push(s);
        }
    }
    for t in o.tags {
        if !k.tags.contains(&t) {
            k.tags.push(t);
        }
    }
    for l in o.links {
        if !k.links.contains(&l) {
            k.links.push(l);
        }
    }
    for (f, v) in o.fields {
        if !k.fields.iter().any(|(x, _)| *x == f) {
            k.fields.push((f, v));
        }
    }
    for (f, v) in o.facts {
        if k.fact(&f).is_none() {
            k.facts.push((f, v));
        }
    }
    let fill = |a: &mut String, b: String| {
        if a.is_empty() {
            *a = b;
        }
    };
    fill(&mut k.owner, o.owner);
    fill(&mut k.location, o.location);
    if k.notes.is_empty() {
        k.notes = o.notes;
    } else if !o.notes.is_empty() {
        k.notes = format!("{}\n{}", k.notes, o.notes);
    }
    k.vendor = k.vendor.take().or(o.vendor);
    k.hostname = k.hostname.take().or(o.hostname);
    k.os = k.os.take().or(o.os);
    k.model = k.model.take().or(o.model);
    k.saved |= o.saved;
    k.monitor |= o.monitor;
    k.first_seen = if k.first_seen == 0 { o.first_seen } else if o.first_seen == 0 { k.first_seen } else { k.first_seen.min(o.first_seen) };
    k.last_seen = k.last_seen.max(o.last_seen);
    if k.kind == DeviceKind::Unknown {
        k.kind = o.kind;
    }
    true
}

/// Move the given addresses and identities of `id` to a new device (the user's split, when
/// two machines were wrongly joined). Host-name identities stay behind so the next discovery
/// doesn't join them again. Returns the new device's id.
pub fn split(map: &mut MapData, id: DeviceId, ips: &[IpAddr], idents: &[Ident], now: i64) -> Option<DeviceId> {
    let new_id = map.next_device_id();
    let d = map.device_mut(id)?;
    let moved_addrs: Vec<Addr> = d.addrs.iter().filter(|a| ips.contains(&a.ip)).cloned().collect();
    let moved_ids: Vec<Ident> = d.idents.iter().filter(|i| idents.contains(i) && i.kind != IdentKind::Host).cloned().collect();
    if moved_addrs.is_empty() && moved_ids.is_empty() {
        return None;
    }
    d.addrs.retain(|a| !ips.contains(&a.ip));
    d.idents.retain(|i| !moved_ids.contains(i));
    let mut n = Device { id: new_id, addrs: moved_addrs, idents: moved_ids, first_seen: now, last_seen: d.last_seen, kind: DeviceKind::Unknown, ..Default::default() };
    let v = n.macs().find_map(crate::mac::vendor).map(str::to_string);
    n.vendor = v;
    n.name = fallback_name(&n);
    map.devices.push(n);
    Some(new_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arp(ip: &str, mac: &str) -> Observation {
        let mut o = Observation::new(Layer::Lan, Source::Arp).ident(IdentKind::Mac, mac);
        o.ips.push(ip.parse().unwrap());
        o
    }

    fn lan_net() -> NetworkObservation {
        NetworkObservation { key: "lan:192.168.1.0/24".into(), name: "192.168.1.0/24".into(), layer: Some(Layer::Lan), cidr: Some("192.168.1.0/24".into()), gateway: Some("192.168.1.1".parse().unwrap()), ..Default::default() }
    }

    #[test]
    fn addresses_are_placed_by_what_they_are() {
        let mut m = MapData::default();
        let mut on_lan = arp("192.168.1.30", "aa:00:00:00:00:01");
        on_lan.network = Some("lan:192.168.1.0/24".into());
        // ARP from the host's VM bridge, tagged with the home LAN by the runner
        let mut vm = arp("192.168.64.3", "aa:00:00:00:00:02");
        vm.network = Some("lan:192.168.1.0/24".into());
        let other = arp("10.9.8.7", "aa:00:00:00:00:03");
        let mut typed = Observation::new(Layer::Lan, Source::Manual);
        typed.name = Some("KVM 8".into());
        typed.ips.push("2.24.118.14".parse().unwrap());
        let mut cgnat = Observation::new(Layer::Lan, Source::Import);
        cgnat.ips.push("100.79.32.16".parse().unwrap());
        // a public address in the neighbour table is on-link: it stays on the LAN
        let onlink = arp("203.0.113.5", "aa:00:00:00:00:04");
        ingest(&mut m, &Discovery { networks: vec![lan_net()], devices: vec![on_lan, vm, other, typed, cgnat, onlink], notes: vec![] }, 100);
        fn addr(m: &MapData, ip: &str) -> Addr {
            m.devices.iter().flat_map(|d| &d.addrs).find(|a| a.ip == ip.parse::<IpAddr>().unwrap()).cloned().unwrap()
        }
        fn net(m: &MapData, ip: &str) -> Option<(Layer, String, String)> {
            addr(m, ip).network.and_then(|n| m.network(n)).map(|n| (n.layer, n.key.clone(), n.name.clone()))
        }
        assert_eq!(net(&m, "192.168.1.30").unwrap().1, "lan:192.168.1.0/24");
        let (l, key, name) = net(&m, "192.168.64.3").unwrap();
        assert_eq!((l, key.as_str()), (Layer::Lan, "lan:192.168.64.0/24"), "its own subnet, not the home LAN");
        assert!(name.contains("192.168.64.0/24"));
        assert_eq!(m.network_by_key("lan:192.168.64.0/24").unwrap().facts, vec![("bridge".to_string(), "vm".to_string())]);
        assert_eq!(net(&m, "10.9.8.7").unwrap().1, "lan:10.9.8.0/24", "an unknown private subnet gets its /24");
        assert_eq!(addr(&m, "2.24.118.14").layer, Layer::Internet);
        assert_eq!(net(&m, "2.24.118.14").unwrap().1, INTERNET_KEY);
        assert_eq!(addr(&m, "100.79.32.16").layer, Layer::Tailscale);
        assert_eq!(addr(&m, "203.0.113.5").layer, Layer::Lan);
        // idempotent, and the next refresh doesn't move the VM back onto the LAN
        assert_eq!(reclassify(&mut m), (vec![], vec![]));
        let mut again = arp("192.168.64.3", "aa:00:00:00:00:02");
        again.network = Some("lan:192.168.1.0/24".into());
        let r = ingest(&mut m, &Discovery { devices: vec![again], ..Default::default() }, 200);
        assert!(r.networks.is_empty());
        assert_eq!(net(&m, "192.168.64.3").unwrap().1, "lan:192.168.64.0/24");
    }

    #[test]
    fn maps_from_before_are_fixed() {
        // what older versions wrote: everything on the LAN network
        let mut m = MapData::default();
        m.networks.push(Network { id: 1, name: "home".into(), layer: Layer::Lan, cidr: Some("192.168.40.0/24".into()), gateway: Some("192.168.40.1".parse().unwrap()), parent: None, key: "lan:192.168.40.0/24".into(), facts: vec![] });
        let addr = |ip: &str, net: Option<NetworkId>, source: Source| Addr { ip: ip.parse().unwrap(), layer: Layer::Lan, network: net, source, last_seen: 0 };
        m.devices.push(Device { id: 1, name: "KVM 8".into(), saved: true, addrs: vec![addr("2.24.118.14", None, Source::Manual)], ..Default::default() });
        m.devices.push(Device { id: 2, name: "vm".into(), addrs: vec![addr("192.168.64.3", Some(1), Source::Arp)], ..Default::default() });
        m.devices.push(Device { id: 3, name: "tv".into(), addrs: vec![addr("192.168.40.20", Some(1), Source::Arp), addr("fe80::1", Some(1), Source::Arp), addr("fd00::20", Some(1), Source::Mdns)], ..Default::default() });
        let (devs, nets) = reclassify(&mut m);
        assert_eq!(devs, vec![1, 2]);
        assert_eq!(nets.len(), 2, "internet and the VM bridge");
        assert_eq!(m.devices[0].layers(), vec![Layer::Internet]);
        assert_eq!(m.devices[2].addrs.iter().map(|a| a.network).collect::<Vec<_>>(), vec![Some(1); 3], "IPv6 and link-local stay with the LAN they were seen on");
    }

    #[test]
    fn same_mac_new_ip_is_the_same_device() {
        let mut m = MapData::default();
        let r = ingest(&mut m, &Discovery { networks: vec![lan_net()], devices: vec![arp("192.168.1.23", "b8:27:eb:00:00:01")], notes: vec![] }, 100);
        assert_eq!(r.new.len(), 1);
        assert_eq!(m.devices[0].name, "Raspberry Pi Foundation 192.168.1.23");
        assert_eq!(m.devices[0].addrs[0].network, Some(1));
        // DHCP hands out a new address: same device, old address gone, change recorded
        let r = ingest(&mut m, &Discovery { devices: vec![arp("192.168.1.45", "b8:27:eb:00:00:01")], ..Default::default() }, 200);
        assert!(r.new.is_empty());
        assert_eq!(m.devices.len(), 1);
        assert_eq!(m.devices[0].ips().collect::<Vec<_>>(), vec!["192.168.1.45".parse::<IpAddr>().unwrap()]);
        assert_eq!(r.ip_changes, vec![(1, "192.168.1.23 → 192.168.1.45".to_string())]);
        assert_eq!(m.devices[0].last_seen, 200);
        assert_eq!(m.devices[0].first_seen, 100);
    }

    #[test]
    fn reassigned_ip_goes_to_a_new_device() {
        let mut m = MapData::default();
        ingest(&mut m, &Discovery { networks: vec![lan_net()], devices: vec![arp("192.168.1.50", "00:11:22:33:44:55")], notes: vec![] }, 1);
        let r = ingest(&mut m, &Discovery { devices: vec![arp("192.168.1.50", "66:77:88:99:aa:bb")], ..Default::default() }, 2);
        assert_eq!(r.new, vec![2]);
        assert_eq!(r.touched, vec![1]);
        assert!(m.devices[0].addrs.is_empty(), "old device lost the address");
        assert_eq!(m.devices[1].ips().count(), 1);
    }

    #[test]
    fn sources_join_by_ip_mdns_and_host() {
        let mut m = MapData::default();
        let mut md = Observation::new(Layer::Lan, Source::Mdns).ident(IdentKind::Mdns, "pi.local").ident(IdentKind::Host, "pi.local");
        md.ips.push("192.168.1.23".parse().unwrap());
        md.services.push(Service { name: "_ssh._tcp".into(), port: Some(22), label: None });
        let mut ts = Observation::new(Layer::Tailscale, Source::Tailscale).ident(IdentKind::TailscaleNode, "nPi").ident(IdentKind::Host, "pi");
        ts.ips.push("100.101.1.3".parse().unwrap());
        ts.name = Some("pi".into());
        ts.os = Some("linux".into());
        ingest(&mut m, &Discovery { networks: vec![lan_net()], devices: vec![arp("192.168.1.23", "b8:27:eb:00:00:01"), md, ts], notes: vec![] }, 10);
        assert_eq!(m.devices.len(), 1, "ARP + mDNS by IP, Tailscale by the unique host name");
        let d = &m.devices[0];
        assert_eq!(d.layers(), vec![Layer::Lan, Layer::Tailscale]);
        assert_eq!(d.name, "pi");
        assert_eq!(d.kind, DeviceKind::Server);
        // a second "pi" host makes the name ambiguous: no join
        let mut other = Observation::new(Layer::Tailscale, Source::Tailscale).ident(IdentKind::TailscaleNode, "nOther").ident(IdentKind::Host, "pi");
        other.ips.push("100.101.1.9".parse().unwrap());
        ingest(&mut m, &Discovery { devices: vec![other], ..Default::default() }, 11);
        assert_eq!(m.devices.len(), 2);
        // a strong identity beats an IP: the same node seen with a new address
        let mut again = Observation::new(Layer::Tailscale, Source::Tailscale).ident(IdentKind::TailscaleNode, "nPi");
        again.ips.push("100.101.1.33".parse().unwrap());
        ingest(&mut m, &Discovery { devices: vec![again], ..Default::default() }, 12);
        let ts_ips: Vec<String> = m.devices[0].addrs.iter().filter(|a| a.layer == Layer::Tailscale).map(|a| a.ip.to_string()).collect();
        assert_eq!(ts_ips, ["100.101.1.33"], "Tailscale is authoritative for its layer");
    }

    #[test]
    fn mdns_with_a_shared_link_local_address_joins_the_right_device() {
        let mut m = MapData::default();
        // the router answers NDP for fe80::1; this Mac's Bonjour record also lists fe80::1 (lo0)
        let mut router = arp("192.168.40.1", "2c:0b:ab:61:f6:7e");
        router.ips.push("fe80::1".parse().unwrap());
        let mac = arp("192.168.40.7", "00:e0:4c:53:63:88");
        ingest(&mut m, &Discovery { networks: vec![], devices: vec![router, mac], notes: vec![] }, 1);
        let mut md = Observation::new(Layer::Lan, Source::Mdns).ident(IdentKind::Mdns, "macbook.local");
        md.ips = vec!["fe80::1".parse().unwrap(), "192.168.40.7".parse().unwrap(), "192.168.64.1".parse().unwrap()];
        md.name = Some("MacBook Pro".into());
        ingest(&mut m, &Discovery { devices: vec![md], ..Default::default() }, 2);
        assert_eq!(m.devices.len(), 2);
        assert_eq!(m.devices[1].name, "MacBook Pro", "joined by its IPv4, not the shared link-local");
        assert!(m.devices[0].ips().any(|i| i.to_string() == "fe80::1"), "the router keeps fe80::1");
        assert!(!m.devices[1].ips().any(|i| i.to_string() == "fe80::1"), "a MAC-less record can't take it");
        assert!(m.devices[1].ips().any(|i| i.to_string() == "192.168.64.1"));
    }

    #[test]
    fn manual_names_and_kinds_stick() {
        let mut m = MapData::default();
        ingest(&mut m, &Discovery { networks: vec![lan_net()], devices: vec![arp("192.168.1.1", "00:11:22:33:44:55")], notes: vec![] }, 1);
        assert_eq!(m.devices[0].kind, DeviceKind::Router, "the network's gateway");
        m.devices[0].name = "Main router".into();
        m.devices[0].name_manual = true;
        m.devices[0].kind = DeviceKind::Switch;
        m.devices[0].kind_manual = true;
        let mut s = Observation::new(Layer::Lan, Source::Ssdp);
        s.ips.push("192.168.1.1".parse().unwrap());
        s.name = Some("RT-AX86U".into());
        ingest(&mut m, &Discovery { devices: vec![s], ..Default::default() }, 2);
        assert_eq!(m.devices[0].name, "Main router");
        assert_eq!(m.devices[0].kind, DeviceKind::Switch);
    }

    #[test]
    fn merge_and_split() {
        let mut m = MapData::default();
        ingest(&mut m, &Discovery { networks: vec![lan_net()], devices: vec![arp("192.168.1.10", "00:11:22:33:44:01"), arp("192.168.1.11", "00:11:22:33:44:02")], notes: vec![] }, 1);
        m.devices[0].owner = "Ana".into();
        m.devices[1].tags = vec!["lab".into()];
        m.devices[1].saved = true;
        assert!(merge(&mut m, 1, 2));
        assert!(!merge(&mut m, 1, 2), "already gone");
        assert_eq!(m.devices.len(), 1);
        let d = &m.devices[0];
        assert_eq!(d.addrs.len(), 2);
        assert_eq!(d.macs().count(), 2);
        assert_eq!(d.tags, ["lab"]);
        assert!(d.saved);
        assert_eq!(d.owner, "Ana");
        // merged devices are found by either MAC afterwards
        assert_eq!(find(&m.devices, &arp("192.168.1.99", "00:11:22:33:44:02")), Some(0));
        let mac2 = Ident::new(IdentKind::Mac, "00:11:22:33:44:02");
        let new_id = split(&mut m, 1, &["192.168.1.11".parse().unwrap()], std::slice::from_ref(&mac2), 5).unwrap();
        assert_eq!(m.devices.len(), 2);
        assert_eq!(m.device(new_id).unwrap().idents, vec![mac2]);
        assert_eq!(m.device(1).unwrap().addrs.len(), 1);
        assert_eq!(split(&mut m, 1, &[], &[], 5), None);
        // the next ARP read keeps them apart
        let r = ingest(&mut m, &Discovery { devices: vec![arp("192.168.1.11", "00:11:22:33:44:02")], ..Default::default() }, 6);
        assert_eq!(r.updated, vec![new_id]);
    }

    #[test]
    fn tailnet_joins_lan_devices_and_fills_owners() {
        let mut m = MapData::default();
        let lan = NetworkObservation { key: "lan:192.168.50.0/24".into(), name: "192.168.50.0/24".into(), layer: Some(Layer::Lan), cidr: Some("192.168.50.0/24".into()), ..Default::default() };
        // this computer as passive discovery sees it (its LAN host name differs from Tailscale's)
        let mut me = arp("192.168.50.7", "00:11:22:33:44:07").ident(IdentKind::Host, "ana-mbp");
        me.facts.push(("this_computer".into(), "1".into()));
        // the computer's VM bridge, another LAN "device" holding one of the self endpoints
        let bridge = arp("192.168.64.1", "be:00:00:00:00:01");
        let studio = arp("192.168.50.20", "00:11:22:33:44:20");
        let tv = arp("192.168.50.51", "00:11:22:33:44:51");
        ingest(&mut m, &Discovery { networks: vec![lan], devices: vec![me, bridge, studio, tv], notes: vec![] }, 10);
        assert_eq!(m.devices.len(), 4);
        let ts = crate::discover::tailscale::parse_status(crate::discover::tailscale::tests::STATUS_1_104).unwrap();
        let r = ingest(&mut m, &ts, 20);
        assert_eq!(r.new.len(), 3, "desktop, phone, vps are new; this computer and the studio join LAN devices");
        assert_eq!(m.devices.len(), 7);
        let me = m.devices.iter().find(|d| d.fact("this_computer").is_some()).unwrap();
        assert_eq!(me.layers(), vec![Layer::Lan, Layer::Tailscale], "this computer appears once, on both networks");
        assert!(me.ident(IdentKind::Mac).is_some() && me.ident(IdentKind::TailscaleNode) == Some("nSelfAAAA"));
        assert_eq!(me.owner, "Ana P");
        let studio = m.devices.iter().find(|d| d.ident(IdentKind::TailscaleNode) == Some("nStudioCCC")).unwrap();
        assert!(studio.addrs.iter().any(|a| a.ip.to_string() == "192.168.50.20"), "the direct LAN endpoint joins it to the LAN device");
        let bridge = m.devices.iter().find(|d| d.addrs.iter().any(|a| a.ip.to_string() == "192.168.64.1")).unwrap();
        assert!(bridge.ident(IdentKind::TailscaleNode).is_none());
        let owners: Vec<&str> = m.devices.iter().filter(|d| d.layers().contains(&Layer::Tailscale)).map(|d| d.owner.as_str()).collect();
        assert_eq!(owners.len(), 5);
        assert!(owners.contains(&"Example Office") && owners.contains(&"ops@example.net"));
        // an owner the user typed stays; the source's own keeps following the tailnet
        let id = m.devices.iter().find(|d| d.ident(IdentKind::TailscaleNode) == Some("nDeskBBBB")).unwrap().id;
        m.device_mut(id).unwrap().owner = "Reception".into();
        let again = crate::discover::tailscale::parse_status(&crate::discover::tailscale::tests::STATUS_1_104.replace("\"DisplayName\": \"Ana P\"", "\"DisplayName\": \"Ana Pérez\"").replace("\"DisplayName\": \"Example Office\"", "\"DisplayName\": \"Office\"")).unwrap();
        let r = ingest(&mut m, &again, 30);
        assert!(r.new.is_empty(), "a second run updates the same devices");
        assert_eq!(m.device(id).unwrap().owner, "Reception");
        assert_eq!(m.devices.iter().find(|d| d.fact("this_computer").is_some()).unwrap().owner, "Ana Pérez");
    }
}
