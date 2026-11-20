//! The neighbour (ARP / NDP) table: who this computer has talked to on the LAN lately. Read
//! only, no packets sent: `arp -an` on macOS / BSD, `/proc/net/arp` or `ip neigh` on Linux,
//! `arp -a` on Windows.
use crate::model::{IdentKind, Layer, Observation, Source};
use std::net::IpAddr;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Neighbor {
    pub ip: IpAddr,
    pub mac: String,
    pub iface: Option<String>,
}

/// `? (192.168.1.1) at 0:11:22:33:44:55 on en0 ifscope [ethernet]` (macOS / BSD `arp -an`).
pub fn parse_bsd(s: &str) -> Vec<Neighbor> {
    s.lines()
        .filter_map(|l| {
            let ip = l.split_once('(')?.1.split_once(')')?.0.parse().ok()?;
            let rest = l.split_once(" at ")?.1;
            let mac = crate::mac::normalize(rest.split_whitespace().next()?)?;
            let iface = rest.split_once(" on ").and_then(|(_, r)| r.split_whitespace().next()).map(str::to_string);
            Some(Neighbor { ip, mac, iface })
        })
        .filter(|n| crate::mac::is_device(&n.mac))
        .collect()
}

/// `/proc/net/arp`: `IP address HW type Flags HW address Mask Device`; flags 0x0 = incomplete.
pub fn parse_proc(s: &str) -> Vec<Neighbor> {
    s.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 6 || f[2] == "0x0" {
                return None;
            }
            Some(Neighbor { ip: f[0].parse().ok()?, mac: crate::mac::normalize(f[3])?, iface: Some(f[5].to_string()) })
        })
        .filter(|n| crate::mac::is_device(&n.mac))
        .collect()
}

/// `ip neigh`: `192.168.1.1 dev eth0 lladdr 00:11:22:33:44:55 REACHABLE` (IPv4 and IPv6).
pub fn parse_ip_neigh(s: &str) -> Vec<Neighbor> {
    s.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let state = *f.last()?;
            if matches!(state, "FAILED" | "INCOMPLETE") {
                return None;
            }
            let at = |k: &str| f.iter().position(|x| *x == k).and_then(|i| f.get(i + 1)).copied();
            Some(Neighbor { ip: f.first()?.parse().ok()?, mac: crate::mac::normalize(at("lladdr")?)?, iface: at("dev").map(str::to_string) })
        })
        .filter(|n| crate::mac::is_device(&n.mac))
        .collect()
}

/// Windows `arp -a`: `Interface: 192.168.1.10 --- 0x4` then `192.168.1.1  00-11-22-33-44-55  dynamic`.
pub fn parse_windows(s: &str) -> Vec<Neighbor> {
    let mut iface = None;
    let mut out = Vec::new();
    for l in s.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.first().is_some_and(|w| w.ends_with(':')) && f.len() >= 2 {
            iface = Some(f[1].to_string());
            continue;
        }
        if f.len() >= 3
            && let (Ok(ip), Some(mac)) = (f[0].parse::<IpAddr>(), crate::mac::normalize(f[1]))
            && crate::mac::is_device(&mac)
        {
            out.push(Neighbor { ip, mac, iface: iface.clone() });
        }
    }
    out
}

/// IPv6 link-local entries duplicate the IPv4 ones for the same MAC; keep the table compact.
fn dedup(mut v: Vec<Neighbor>) -> Vec<Neighbor> {
    v.sort_by(|a, b| (a.mac.as_str(), !a.ip.is_ipv4(), a.ip).cmp(&(b.mac.as_str(), !b.ip.is_ipv4(), b.ip)));
    v.dedup_by(|a, b| a.ip == b.ip);
    v
}

pub fn to_observations(neighbors: &[Neighbor]) -> Vec<Observation> {
    // one observation per MAC, with all its addresses
    let mut out: Vec<Observation> = Vec::new();
    for n in neighbors {
        match out.iter_mut().find(|o| o.idents.iter().any(|i| i.kind == IdentKind::Mac && i.value == n.mac)) {
            Some(o) => {
                if !o.ips.contains(&n.ip) {
                    o.ips.push(n.ip);
                }
            }
            None => {
                let mut o = Observation::new(Layer::Lan, Source::Arp).ident(IdentKind::Mac, &n.mac);
                o.ips.push(n.ip);
                o.vendor = crate::mac::vendor(&n.mac).map(str::to_string);
                out.push(o);
            }
        }
    }
    out
}

/// Read this computer's neighbour table.
pub fn read() -> Result<Vec<Neighbor>, String> {
    let t = Duration::from_secs(8);
    if cfg!(target_os = "linux") {
        if let Ok(s) = std::fs::read_to_string("/proc/net/arp") {
            let mut v = parse_proc(&s);
            if let Ok(o) = crate::cmd::run_named("ip", &["neigh"], t) {
                v.extend(parse_ip_neigh(&o.stdout).into_iter().filter(|n| !n.ip.is_ipv4()));
            }
            return Ok(dedup(v));
        }
        let o = crate::cmd::run_named("ip", &["neigh"], t).map_err(|e| format!("{e:?}"))?;
        return Ok(dedup(parse_ip_neigh(&o.stdout)));
    }
    let arp = crate::cmd::find("arp", &[]).ok_or_else(|| "arp".to_string())?;
    if cfg!(windows) {
        let o = crate::cmd::run(&arp, &["-a"], t).map_err(|e| format!("{e:?}"))?;
        return Ok(dedup(parse_windows(&o.stdout)));
    }
    let o = crate::cmd::run(&arp, &["-an"], t).map_err(|e| format!("{e:?}"))?;
    let mut v = parse_bsd(&o.stdout);
    // NDP cache (IPv6 neighbours) on macOS
    if cfg!(target_os = "macos")
        && let Ok(o) = crate::cmd::run_named("ndp", &["-an"], t)
    {
        v.extend(parse_ndp(&o.stdout));
    }
    Ok(dedup(v))
}

/// macOS `ndp -an`: `fe80::1%en0  0:11:22:33:44:55  en0 23h59m58s S R`.
pub fn parse_ndp(s: &str) -> Vec<Neighbor> {
    s.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 3 {
                return None;
            }
            let ip = f[0].split('%').next()?.parse().ok()?;
            Some(Neighbor { ip, mac: crate::mac::normalize(f[1])?, iface: Some(f[2].to_string()) })
        })
        .filter(|n| crate::mac::is_device(&n.mac))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MACOS: &str = "? (192.168.1.1) at 0:11:22:33:44:55 on en0 ifscope [ethernet]
? (192.168.1.23) at b8:27:eb:a:b:c on en0 ifscope [ethernet]
? (192.168.1.40) at (incomplete) on en0 ifscope [ethernet]
? (192.168.1.255) at ff:ff:ff:ff:ff:ff on en0 ifscope [ethernet]
? (224.0.0.251) at 1:0:5e:0:0:fb on en0 ifscope permanent [ethernet]
";

    #[test]
    fn macos_arp() {
        let n = parse_bsd(MACOS);
        assert_eq!(n.len(), 2);
        assert_eq!(n[0], Neighbor { ip: "192.168.1.1".parse().unwrap(), mac: "00:11:22:33:44:55".into(), iface: Some("en0".into()) });
        assert_eq!(n[1].mac, "b8:27:eb:0a:0b:0c");
        let o = to_observations(&n);
        assert_eq!(o[1].vendor.as_deref(), Some("Raspberry Pi Foundation"));
        assert_eq!(o[1].source, Some(Source::Arp));
    }

    #[test]
    fn linux_proc_and_ip_neigh() {
        let proc_ = "IP address       HW type     Flags       HW address            Mask     Device
192.168.1.1      0x1         0x2         00:11:22:33:44:55     *        eth0
192.168.1.50     0x1         0x0         00:00:00:00:00:00     *        eth0
10.0.0.7         0x1         0x2         AA:BB:CC:DD:EE:01     *        wlan0
";
        let n = parse_proc(proc_);
        assert_eq!(n.len(), 2);
        assert_eq!(n[1].iface.as_deref(), Some("wlan0"));
        assert_eq!(n[1].mac, "aa:bb:cc:dd:ee:01");
        let ip = "192.168.1.1 dev eth0 lladdr 00:11:22:33:44:55 REACHABLE
192.168.1.9 dev eth0  FAILED
fe80::1 dev eth0 lladdr 00:11:22:33:44:55 router STALE
192.168.1.30 dev eth0 lladdr 3c:22:fb:01:02:03 DELAY
";
        let n = parse_ip_neigh(ip);
        assert_eq!(n.len(), 3);
        assert!(n[1].ip.is_ipv6());
        // one observation per MAC, IPv4 and IPv6 together
        let o = to_observations(&n);
        assert_eq!(o.len(), 2);
        assert_eq!(o[0].ips.len(), 2);
    }

    #[test]
    fn windows_arp() {
        let s = "
Interface: 192.168.1.10 --- 0x4
  Internet Address      Physical Address      Type
  192.168.1.1           00-11-22-33-44-55     dynamic
  192.168.1.20          b8-27-eb-00-00-01     dynamic
  192.168.1.255         ff-ff-ff-ff-ff-ff     static
  224.0.0.22            01-00-5e-00-00-16     static

Interface: 172.20.0.1 --- 0x11
  Internet Address      Physical Address      Type
  172.20.0.5            00-15-5d-01-02-03     dynamic
";
        let n = parse_windows(s);
        assert_eq!(n.len(), 3);
        assert_eq!(n[0].iface.as_deref(), Some("192.168.1.10"));
        assert_eq!(n[2].iface.as_deref(), Some("172.20.0.1"));
        assert_eq!(n[2].mac, "00:15:5d:01:02:03");
    }

    #[test]
    fn macos_ndp() {
        let s = "Neighbor                        Linklayer Address  Netif Expire    St Flgs Prbs
fe80::1%en0                     0:11:22:33:44:55     en0 23h59m58s S  R
fe80::aede:48ff:fe00:1122%en0   ac:de:48:0:11:22     en0 permanent R
fe80::99%en0                    (incomplete)         en0 expired   N
";
        let n = parse_ndp(s);
        assert_eq!(n.len(), 2);
        assert_eq!(n[0].ip, "fe80::1".parse::<IpAddr>().unwrap());
    }
}
