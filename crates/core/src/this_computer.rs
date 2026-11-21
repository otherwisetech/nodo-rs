//! Which device in the map is the computer Nodo runs on. Worked out at runtime from this
//! computer's interfaces (MACs and addresses, Tailscale's included), its host name and its
//! Tailscale node id, so a map in a synced folder opened on another computer marks that one.
//! Only a hint ("last seen as this computer") is kept in the map, to break ties.
use crate::discover::netinfo::LocalNet;
use crate::model::{Device, DeviceId, Discovery, IdentKind, Layer, Observation, Source};
use std::net::IpAddr;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IfaceKind {
    Wifi,
    Ethernet,
    Tailscale,
    Vpn,
    Bridge,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalIface {
    pub name: String,
    pub kind: IfaceKind,
    pub mac: Option<String>,
    /// Without loopback and link-local addresses.
    pub ips: Vec<IpAddr>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalIdentity {
    pub hostname: Option<String>,
    pub ifaces: Vec<LocalIface>,
    pub tailscale_node: Option<String>,
}

fn is_tailscale_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64,
        IpAddr::V6(v6) => v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

fn usable(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_link_local() && !v4.is_unspecified(),
        IpAddr::V6(v6) => !v6.is_loopback() && (v6.segments()[0] & 0xffc0) != 0xfe80 && !v6.is_unspecified(),
    }
}

/// A kind from the interface name alone (Linux names, macOS fallbacks).
fn kind_from_name(name: &str, ips: &[IpAddr]) -> IfaceKind {
    let n = name.to_lowercase();
    if n.starts_with("tailscale") || ((n.starts_with("utun") || n.starts_with("tun")) && ips.iter().any(is_tailscale_ip)) {
        IfaceKind::Tailscale
    } else if n.starts_with("wl") || n.starts_with("wifi") {
        IfaceKind::Wifi
    } else if n.starts_with("eth") || n.starts_with("en") {
        IfaceKind::Ethernet
    } else if n.starts_with("wg") || n.starts_with("tun") || n.starts_with("utun") || n.starts_with("ppp") || n.starts_with("ipsec") {
        IfaceKind::Vpn
    } else if n.starts_with("bridge") || n.starts_with("br") || n.starts_with("docker") || n.starts_with("virbr") || n.starts_with("veth") || n.starts_with("vmenet") || n.starts_with("vnic") {
        IfaceKind::Bridge
    } else {
        IfaceKind::Other
    }
}

/// macOS `networksetup -listallhardwareports`: (device, port name) pairs.
pub fn parse_hardware_ports(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut port: Option<String> = None;
    for l in s.lines() {
        if let Some(p) = l.strip_prefix("Hardware Port:") {
            port = Some(p.trim().to_string());
        } else if let Some(d) = l.strip_prefix("Device:")
            && let Some(p) = port.take()
        {
            out.push((d.trim().to_string(), p));
        }
    }
    out
}

fn kind_from_port(port: &str) -> IfaceKind {
    let p = port.to_lowercase();
    if p.contains("wi-fi") || p.contains("airport") || p.contains("wlan") {
        IfaceKind::Wifi
    } else if p.contains("bridge") {
        IfaceKind::Bridge
    } else if p.contains("ethernet") || p.contains("lan") {
        IfaceKind::Ethernet
    } else {
        IfaceKind::Other
    }
}

/// Full `ifconfig` output (macOS / BSD): every interface with an address, loopback left out.
/// `ports` from [`parse_hardware_ports`] names Wi-Fi and Ethernet.
pub fn parse_ifconfig_all(s: &str, ports: &[(String, String)]) -> Vec<LocalIface> {
    let mut out: Vec<LocalIface> = Vec::new();
    let mut cur: Option<(String, Option<String>, Vec<IpAddr>)> = None;
    let flush = |c: Option<(String, Option<String>, Vec<IpAddr>)>, out: &mut Vec<LocalIface>| {
        let Some((name, mac, ips)) = c else { return };
        if ips.is_empty() || name.starts_with("lo") {
            return;
        }
        let kind = match ports.iter().find(|(d, _)| *d == name) {
            Some((_, p)) if !ips.iter().any(is_tailscale_ip) => kind_from_port(p),
            _ => kind_from_name(&name, &ips),
        };
        out.push(LocalIface { name, kind, mac, ips });
    };
    for l in s.lines() {
        if !l.starts_with([' ', '\t']) && !l.is_empty() {
            flush(cur.take(), &mut out);
            let name = l.split(':').next().unwrap_or_default().trim().to_string();
            cur = Some((name, None, Vec::new()));
            continue;
        }
        let Some((_, mac, ips)) = cur.as_mut() else { continue };
        let f: Vec<&str> = l.split_whitespace().collect();
        match f.first() {
            Some(&"ether") => *mac = f.get(1).and_then(|m| crate::mac::normalize(m)),
            Some(&"inet") | Some(&"inet6") => {
                if let Some(ip) = f.get(1).and_then(|a| a.split('%').next()).and_then(|a| a.parse::<IpAddr>().ok()).filter(usable)
                    && !ips.contains(&ip)
                {
                    ips.push(ip);
                }
            }
            _ => {}
        }
    }
    flush(cur, &mut out);
    out
}

/// Linux `ip -o addr show`: `2: wlp2s0    inet 10.0.0.42/22 brd …` one address per line.
/// MACs come separately (`/sys/class/net/<if>/address`).
pub fn parse_ip_addr_all(s: &str) -> Vec<LocalIface> {
    let mut by: Vec<(String, Vec<IpAddr>)> = Vec::new();
    for l in s.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        let (Some(name), Some(i)) = (f.get(1), f.iter().position(|x| *x == "inet" || *x == "inet6")) else { continue };
        let name = name.trim_end_matches(':').split('@').next().unwrap_or_default().to_string();
        let Some(ip) = f.get(i + 1).and_then(|a| a.split('/').next()).and_then(|a| a.parse::<IpAddr>().ok()).filter(usable) else { continue };
        match by.iter_mut().find(|(n, _)| *n == name) {
            Some((_, v)) if !v.contains(&ip) => v.push(ip),
            Some(_) => {}
            None => by.push((name, vec![ip])),
        }
    }
    by.into_iter().filter(|(n, _)| n != "lo").map(|(name, ips)| LocalIface { kind: kind_from_name(&name, &ips), name, mac: None, ips }).collect()
}

impl LocalIdentity {
    /// MACs of the Wi-Fi and Ethernet interfaces in use (with an address): what neighbours see.
    pub fn macs(&self) -> Vec<String> {
        let mut v: Vec<String> = self.ifaces.iter().filter(|i| matches!(i.kind, IfaceKind::Wifi | IfaceKind::Ethernet)).filter_map(|i| i.mac.clone()).collect();
        v.dedup();
        v
    }

    pub fn ips(&self) -> impl Iterator<Item = IpAddr> + '_ {
        self.ifaces.iter().flat_map(|i| i.ips.iter().copied())
    }

    pub fn host_label(&self) -> Option<String> {
        self.hostname.as_deref().map(crate::model::host_label).filter(|h| !crate::identity::generic_host(h))
    }

    /// Fill in the Tailscale node id and addresses from a `tailscale status` discovery (its
    /// Self node).
    pub fn with_tailscale(&mut self, d: &Discovery) {
        let Some(me) = d.devices.iter().find(|o| o.source == Some(Source::Tailscale) && o.facts.iter().any(|(k, _)| k == "this_computer")) else { return };
        if let Some(id) = me.idents.iter().find(|i| i.kind == IdentKind::TailscaleNode) {
            self.tailscale_node = Some(id.value.clone());
        }
        let missing: Vec<IpAddr> = me.ips.iter().copied().filter(|ip| !self.ips().any(|x| x == *ip)).collect();
        if missing.is_empty() {
            return;
        }
        match self.ifaces.iter_mut().find(|i| i.kind == IfaceKind::Tailscale) {
            Some(i) => i.ips.extend(missing),
            None => self.ifaces.push(LocalIface { name: "tailscale".into(), kind: IfaceKind::Tailscale, mac: None, ips: missing }), // i18n-ok: interface id
        }
    }

    /// The addresses to show in one line: LAN IPv4 first, then the Tailscale IPv4.
    pub fn summary_ips(&self) -> Vec<IpAddr> {
        let mut v: Vec<IpAddr> = Vec::new();
        for want in [IfaceKind::Ethernet, IfaceKind::Wifi, IfaceKind::Tailscale] {
            v.extend(self.ifaces.iter().filter(|i| i.kind == want).flat_map(|i| i.ips.iter().copied()).filter(|ip| ip.is_ipv4()));
        }
        v.dedup();
        v
    }

    /// Wi-Fi, Ethernet and Tailscale first, bridges and the rest after.
    pub fn sorted_ifaces(&self) -> Vec<&LocalIface> {
        let mut v: Vec<&LocalIface> = self.ifaces.iter().collect();
        v.sort_by_key(|i| (i.kind, i.name.clone()));
        v
    }
}

/// This computer's interfaces and host name. Blocking (a few short commands).
pub fn detect() -> LocalIdentity {
    let t = Duration::from_secs(5);
    let hostname = crate::discover::hostname();
    let mut ifaces = Vec::new();
    if cfg!(target_os = "macos") || cfg!(target_os = "freebsd") {
        let ports = crate::cmd::run_named("networksetup", &["-listallhardwareports"], t).map(|o| parse_hardware_ports(&o.stdout)).unwrap_or_default();
        if let Ok(o) = crate::cmd::run_named("ifconfig", &[], t) {
            ifaces = parse_ifconfig_all(&o.stdout, &ports);
        }
    } else if cfg!(windows) {
        if let Some(n) = crate::discover::netinfo::detect() {
            ifaces.push(LocalIface { name: n.iface.clone(), kind: IfaceKind::Ethernet, mac: n.mac.clone(), ips: vec![IpAddr::V4(n.ip)] });
        }
    } else if let Ok(o) = crate::cmd::run_named("ip", &["-o", "addr", "show"], t) {
        ifaces = parse_ip_addr_all(&o.stdout);
        for i in &mut ifaces {
            i.mac = std::fs::read_to_string(format!("/sys/class/net/{}/address", i.name)).ok().and_then(|m| crate::mac::normalize(&m)).filter(|m| m != "00:00:00:00:00:00");
        }
    }
    LocalIdentity { hostname, ifaces, tailscale_node: None }
}

/// This computer as passive discovery reports it: its LAN address (the default route's, else
/// the first Wi-Fi / Ethernet IPv4), MACs and host name. Sent even with no network at all, so
/// the map always has it.
pub fn observation(net: Option<&LocalNet>, me: &LocalIdentity) -> Observation {
    let mut o = Observation::new(Layer::Lan, Source::Arp);
    match net {
        Some(n) => o.ips.push(IpAddr::V4(n.ip)),
        None => {
            if let Some(ip) = me.ifaces.iter().filter(|i| matches!(i.kind, IfaceKind::Wifi | IfaceKind::Ethernet)).flat_map(|i| i.ips.iter()).find(|ip| ip.is_ipv4()) {
                o.ips.push(*ip);
            }
        }
    }
    let mut macs: Vec<String> = net.and_then(|n| n.mac.clone()).into_iter().collect();
    macs.extend(me.macs());
    for m in &macs {
        o = o.ident(IdentKind::Mac, m);
    }
    o.vendor = macs.iter().find_map(|m| crate::mac::vendor(m)).map(str::to_string);
    if let Some(h) = me.hostname.clone().or_else(crate::discover::hostname) {
        o = o.ident(IdentKind::Host, &h);
        o.name = Some(crate::model::host_label(&h));
    }
    o.online = Some(true);
    o.facts.push(("this_computer".into(), "1".into()));
    o
}

/// The map's device that is this computer, if one matches well enough. A device with a
/// different Tailscale node is another machine; a LAN address only counts when the MACs don't
/// say otherwise (DHCP hands addresses around). `hint` (last time's answer) breaks ties.
pub fn identify(devices: &[Device], me: &LocalIdentity, hint: Option<DeviceId>) -> Option<DeviceId> {
    let macs = me.macs();
    let label = me.host_label();
    let mut scored: Vec<(u32, DeviceId)> = Vec::new();
    for d in devices {
        let node = d.ident(IdentKind::TailscaleNode);
        if let (Some(a), Some(b)) = (node, me.tailscale_node.as_deref())
            && a != b
        {
            continue;
        }
        let mut score = 0;
        if node.is_some() && node == me.tailscale_node.as_deref() {
            score += 100;
        }
        let dev_macs: Vec<&str> = d.macs().collect();
        let mac_hit = dev_macs.iter().any(|m| macs.iter().any(|x| x == m));
        if mac_hit {
            score += 50;
        }
        let mac_conflict = !dev_macs.is_empty() && !macs.is_empty() && !mac_hit;
        for a in &d.addrs {
            if !usable(&a.ip) || !me.ips().any(|x| x == a.ip) {
                continue;
            }
            let ts = a.layer == Layer::Tailscale || is_tailscale_ip(&a.ip);
            if (ts && node.is_some() && me.tailscale_node.is_none()) || (!ts && mac_conflict) {
                continue;
            }
            score += if a.ip.is_ipv4() { 10 } else { 5 };
        }
        if let Some(l) = &label
            && !mac_conflict
            && (d.idents.iter().any(|i| i.kind == IdentKind::Host && i.value == *l) || d.hostname.as_deref().map(crate::model::host_label).as_ref() == Some(l))
        {
            score += 5;
        }
        if score > 0 {
            scored.push((score, d.id));
        }
    }
    let best = scored.iter().map(|(s, _)| *s).max()?;
    let top: Vec<DeviceId> = scored.iter().filter(|(s, _)| *s == best).map(|(_, id)| *id).collect();
    match top.as_slice() {
        [one] => Some(*one),
        many => hint.filter(|h| many.contains(h)),
    }
}

/// Map setting holding the hint, per host name (a synced map keeps one per computer).
pub fn hint_key(me: &LocalIdentity) -> String {
    format!("this_computer:{}", me.host_label().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Addr, Ident};

    const IFCONFIG: &str = "lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST> mtu 16384
\tinet 127.0.0.1 netmask 0xff000000
\tinet6 ::1 prefixlen 128
\tinet6 fe80::1%lo0 prefixlen 64 scopeid 0x1
en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
\tether bc:d0:74:59:bc:5c
\tinet6 fe80::1c2b:1234:5678:9abc%en0 prefixlen 64 secured scopeid 0xb
\tinet 192.168.40.9 netmask 0xffffff00 broadcast 192.168.40.255
\tstatus: active
en4: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
\tether b6:d5:04:60:56:b4
\tstatus: inactive
en10: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
\tether 00:e0:4c:53:63:88
\tinet 192.168.40.7 netmask 0xffffff00 broadcast 192.168.40.255
\tstatus: active
bridge100: flags=8a63<UP,BROADCAST,SMART,RUNNING,ALLMULTI,SIMPLEX,MULTICAST> mtu 1500
\tether be:d0:74:00:00:64
\tinet 192.168.64.1 netmask 0xffffff00 broadcast 192.168.64.255
utun4: flags=8051<UP,POINTOPOINT,RUNNING,MULTICAST> mtu 1280
\tinet 100.81.184.80 --> 100.81.184.80 netmask 0xffffffff
\tinet6 fd7a:115c:a1e0::3a01:b850 prefixlen 48
";
    const PORTS: &str = "
Hardware Port: USB 10/100/1000 LAN
Device: en10
Ethernet Address: 00:e0:4c:53:63:88

Hardware Port: Wi-Fi
Device: en0
Ethernet Address: bc:d0:74:59:bc:5c

Hardware Port: Thunderbolt Bridge
Device: bridge0
Ethernet Address: 36:b6:fb:c0:8d:00
";

    fn mac_me() -> LocalIdentity {
        let ports = parse_hardware_ports(PORTS);
        LocalIdentity { hostname: Some("macbook-juan.local".into()), ifaces: parse_ifconfig_all(IFCONFIG, &ports), tailscale_node: None }
    }

    fn dev(id: DeviceId, ips: &[(&str, Layer)], idents: &[(IdentKind, &str)]) -> Device {
        Device {
            id,
            name: format!("d{id}"),
            addrs: ips.iter().map(|(ip, l)| Addr { ip: ip.parse().unwrap(), layer: *l, network: None, source: Source::Arp, last_seen: 0 }).collect(),
            idents: idents.iter().map(|(k, v)| Ident::new(*k, v)).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn parses_every_interface() {
        let me = mac_me();
        let names: Vec<(&str, IfaceKind)> = me.ifaces.iter().map(|i| (i.name.as_str(), i.kind)).collect();
        assert_eq!(names, vec![("en0", IfaceKind::Wifi), ("en10", IfaceKind::Ethernet), ("bridge100", IfaceKind::Bridge), ("utun4", IfaceKind::Tailscale)], "no loopback, no interface without an address");
        assert_eq!(me.macs(), vec!["bc:d0:74:59:bc:5c".to_string(), "00:e0:4c:53:63:88".to_string()], "bridges don't count");
        assert_eq!(me.ifaces[3].ips.len(), 2);
        assert!(me.ifaces[0].ips.iter().all(|ip| ip.is_ipv4()), "link-local left out");
        assert_eq!(me.summary_ips().iter().map(|i| i.to_string()).collect::<Vec<_>>(), ["192.168.40.7", "192.168.40.9", "100.81.184.80"]);
        let linux = parse_ip_addr_all("1: lo    inet 127.0.0.1/8 scope host lo\n3: wlp2s0    inet 10.0.0.42/22 brd 10.0.3.255 scope global wlp2s0\n4: tailscale0    inet 100.64.0.5/32 scope global tailscale0\n4: tailscale0    inet6 fd7a:115c:a1e0::5/128 scope global\n5: docker0    inet 172.17.0.1/16 scope global docker0\n");
        assert_eq!(linux.iter().map(|i| (i.name.as_str(), i.kind, i.ips.len())).collect::<Vec<_>>(), vec![("wlp2s0", IfaceKind::Wifi, 1), ("tailscale0", IfaceKind::Tailscale, 2), ("docker0", IfaceKind::Bridge, 1)]);
    }

    #[test]
    fn identifies_across_interfaces() {
        let mut me = mac_me();
        // known only by its Wi-Fi MAC (Ethernet is new), and its Tailscale node by address
        let devices = vec![
            dev(1, &[("192.168.40.1", Layer::Lan)], &[(IdentKind::Mac, "00:11:22:33:44:01")]),
            dev(2, &[("192.168.40.50", Layer::Lan)], &[(IdentKind::Mac, "bc:d0:74:59:bc:5c")]),
            dev(3, &[("100.81.184.80", Layer::Tailscale)], &[(IdentKind::TailscaleNode, "nMe")]),
        ];
        // without the node id a Tailscale address of a known node can't be proven ours
        assert_eq!(identify(&devices, &me, None), Some(2));
        me.tailscale_node = Some("nMe".into());
        assert_eq!(identify(&devices, &me, None), Some(3), "the Tailscale node is the strongest proof");
        // the same machine merged: one device
        let merged = vec![devices[0].clone(), dev(4, &[("192.168.40.7", Layer::Lan), ("100.81.184.80", Layer::Tailscale)], &[(IdentKind::Mac, "00:e0:4c:53:63:88"), (IdentKind::TailscaleNode, "nMe")])];
        assert_eq!(identify(&merged, &me, None), Some(4));
        // Tailscale discovery fills the node id in
        let mut fresh = mac_me();
        fresh.with_tailscale(&crate::discover::tailscale::parse_status(crate::discover::tailscale::tests::STATUS).unwrap());
        assert_eq!(fresh.tailscale_node.as_deref(), Some("nSelf1CNTRL"));
        assert!(fresh.ips().any(|ip| ip.to_string() == "100.101.1.2"));
    }

    #[test]
    fn map_from_another_computer_marks_this_one() {
        // made on the laptop (192.168.40.7, its own MAC and node); now opened on the studio,
        // which has since been given the laptop's old LAN address
        let laptop = dev(1, &[("192.168.40.7", Layer::Lan), ("100.81.184.80", Layer::Tailscale)], &[(IdentKind::Mac, "00:e0:4c:53:63:88"), (IdentKind::TailscaleNode, "nLaptop"), (IdentKind::Host, "macbook-juan")]);
        let studio = dev(2, &[("192.168.40.20", Layer::Lan), ("100.90.1.2", Layer::Tailscale)], &[(IdentKind::Mac, "aa:bb:cc:00:00:20"), (IdentKind::TailscaleNode, "nStudio")]);
        let me = LocalIdentity {
            hostname: Some("studio".into()),
            ifaces: vec![
                LocalIface { name: "en0".into(), kind: IfaceKind::Ethernet, mac: Some("aa:bb:cc:00:00:20".into()), ips: vec!["192.168.40.7".parse().unwrap()] },
                LocalIface { name: "utun3".into(), kind: IfaceKind::Tailscale, mac: None, ips: vec!["100.90.1.2".parse().unwrap()] },
            ],
            tailscale_node: Some("nStudio".into()),
        };
        assert_eq!(identify(&[laptop.clone(), studio], &me, Some(1)), Some(2), "a stale hint doesn't win");
        // the studio isn't in the map yet: nothing is marked, the laptop least of all
        assert_eq!(identify(std::slice::from_ref(&laptop), &me, Some(1)), None);
    }

    #[test]
    fn no_match_and_ties() {
        let me = mac_me();
        assert_eq!(identify(&[], &me, None), None);
        let others = vec![dev(1, &[("192.168.40.1", Layer::Lan)], &[(IdentKind::Mac, "00:11:22:33:44:01")]), dev(2, &[("10.0.0.5", Layer::Lan)], &[])];
        assert_eq!(identify(&others, &me, Some(1)), None, "the hint alone proves nothing");
        // two devices only known by the host name: the hint picks
        let twins = vec![dev(1, &[], &[(IdentKind::Host, "macbook-juan")]), dev(2, &[], &[(IdentKind::Host, "macbook-juan")])];
        assert_eq!(identify(&twins, &me, None), None);
        assert_eq!(identify(&twins, &me, Some(2)), Some(2));
        assert_eq!(hint_key(&me), "this_computer:macbook-juan");
    }

    #[test]
    fn observation_without_a_network() {
        let me = mac_me();
        let o = observation(None, &me);
        assert_eq!(o.ips, vec!["192.168.40.9".parse::<IpAddr>().unwrap()], "first Wi-Fi / Ethernet IPv4");
        assert_eq!(o.idents.iter().filter(|i| i.kind == IdentKind::Mac).count(), 2);
        assert_eq!(o.name.as_deref(), Some("macbook-juan"));
        let bare = observation(None, &LocalIdentity { hostname: Some("pc-1".into()), ..Default::default() });
        assert!(bare.ips.is_empty() && bare.facts.iter().any(|(k, _)| k == "this_computer"));
        let mut m = crate::db::MapData::default();
        let r = crate::identity::ingest(&mut m, &Discovery { devices: vec![bare], ..Default::default() }, 1);
        assert_eq!(r.new.len(), 1, "created even with no address and no other source");
        assert!(m.devices[0].fact("this_computer").is_none(), "never stored as a flag");
    }
}
