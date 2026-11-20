//! This computer's LAN: the default route's interface, its address and prefix, the gateway.
//! macOS `route -n get default` + `ifconfig <if>`, Linux `ip route` + `ip addr`, Windows
//! `route print -4`.
use crate::cidr::{Cidr, prefix_from_mask};
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalNet {
    pub iface: String,
    pub ip: Ipv4Addr,
    pub prefix: u8,
    pub gateway: Option<Ipv4Addr>,
    pub mac: Option<String>,
}

impl LocalNet {
    pub fn cidr(&self) -> Cidr {
        Cidr::v4(self.ip, self.prefix)
    }

    pub fn key(&self) -> String {
        format!("lan:{}", self.cidr())
    }
}

/// `route -n get default`: `gateway: 192.168.1.1` and `interface: en0`.
pub fn parse_route_get(s: &str) -> (Option<Ipv4Addr>, Option<String>) {
    let val = |k: &str| s.lines().find_map(|l| l.trim().strip_prefix(k).map(|v| v.trim().to_string()));
    (val("gateway:").and_then(|g| g.parse().ok()), val("interface:"))
}

/// `ifconfig en0`: `inet 192.168.1.23 netmask 0xffffff00 broadcast …` and `ether aa:bb:…`.
pub fn parse_ifconfig(s: &str) -> Option<(Ipv4Addr, u8, Option<String>)> {
    let mut ip = None;
    let mut mac = None;
    for l in s.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        match f.first() {
            Some(&"inet") if ip.is_none() => {
                let a: Ipv4Addr = f.get(1)?.parse().ok()?;
                let p = f.iter().position(|x| *x == "netmask").and_then(|i| f.get(i + 1)).and_then(|m| prefix_from_mask(m)).unwrap_or(24);
                if !a.is_loopback() {
                    ip = Some((a, p));
                }
            }
            Some(&"ether") => mac = f.get(1).and_then(|m| crate::mac::normalize(m)),
            _ => {}
        }
    }
    ip.map(|(a, p)| (a, p, mac))
}

/// `ip -4 route show default`: `default via 192.168.1.1 dev eth0 proto dhcp …`.
pub fn parse_ip_route(s: &str) -> (Option<Ipv4Addr>, Option<String>) {
    let l = s.lines().find(|l| l.starts_with("default")).unwrap_or_default();
    let f: Vec<&str> = l.split_whitespace().collect();
    let at = |k: &str| f.iter().position(|x| *x == k).and_then(|i| f.get(i + 1)).copied();
    (at("via").and_then(|g| g.parse().ok()), at("dev").map(str::to_string))
}

/// `ip -o -4 addr show dev eth0`: `2: eth0    inet 192.168.1.23/24 brd …`.
pub fn parse_ip_addr(s: &str) -> Option<(Ipv4Addr, u8)> {
    s.lines().find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        let i = f.iter().position(|x| *x == "inet")?;
        let (a, p) = f.get(i + 1)?.split_once('/')?;
        Some((a.parse().ok()?, p.parse().ok()?))
    })
}

/// `route print -4`: the `0.0.0.0 0.0.0.0 <gateway> <interface ip> <metric>` row with the
/// lowest metric, and the on-link row for that interface's network (its mask).
pub fn parse_route_print(s: &str) -> Option<LocalNet> {
    let rows: Vec<Vec<&str>> = s.lines().map(|l| l.split_whitespace().collect::<Vec<_>>()).filter(|f| f.len() == 5).collect();
    let default = rows.iter().filter(|f| f[0] == "0.0.0.0" && f[1] == "0.0.0.0").filter_map(|f| Some((f[4].parse::<u32>().ok()?, f[2].parse::<Ipv4Addr>().ok()?, f[3].parse::<Ipv4Addr>().ok()?))).min_by_key(|(m, _, _)| *m)?;
    let (_, gw, ip) = default;
    let prefix = rows
        .iter()
        .filter(|f| f[2].eq_ignore_ascii_case("On-link") && f[3] == ip.to_string() && f[1] != "255.255.255.255")
        .filter_map(|f| {
            let p = prefix_from_mask(f[1])?;
            let net: Ipv4Addr = f[0].parse().ok()?;
            (p > 0 && Cidr::v4(ip, p).addr == IpAddr::V4(net)).then_some(p)
        })
        .max()
        .unwrap_or(24);
    Some(LocalNet { iface: ip.to_string(), ip, prefix, gateway: Some(gw), mac: None })
}

/// The LAN behind the default route, if any.
pub fn detect() -> Option<LocalNet> {
    let t = Duration::from_secs(5);
    if cfg!(target_os = "macos") || cfg!(target_os = "freebsd") {
        let o = crate::cmd::run_named("route", &["-n", "get", "default"], t).ok()?;
        let (gw, iface) = parse_route_get(&o.stdout);
        let iface = iface?;
        let o = crate::cmd::run_named("ifconfig", &[&iface], t).ok()?;
        let (ip, prefix, mac) = parse_ifconfig(&o.stdout)?;
        return Some(LocalNet { iface, ip, prefix, gateway: gw, mac });
    }
    if cfg!(windows) {
        let o = crate::cmd::run_named("route", &["print", "-4"], t).ok()?;
        return parse_route_print(&o.stdout);
    }
    let o = crate::cmd::run_named("ip", &["-4", "route", "show", "default"], t).ok()?;
    let (gw, iface) = parse_ip_route(&o.stdout);
    let iface = iface?;
    let o = crate::cmd::run_named("ip", &["-o", "-4", "addr", "show", "dev", &iface], t).ok()?;
    let (ip, prefix) = parse_ip_addr(&o.stdout)?;
    let mac = std::fs::read_to_string(format!("/sys/class/net/{iface}/address")).ok().and_then(|m| crate::mac::normalize(&m));
    Some(LocalNet { iface, ip, prefix, gateway: gw, mac })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos() {
        let r = "   route to: default
destination: default
       mask: default
    gateway: 192.168.1.1
  interface: en0
      flags: <UP,GATEWAY,DONE,STATIC,PRCLONING,GLOBAL>
";
        assert_eq!(parse_route_get(r), (Some(Ipv4Addr::new(192, 168, 1, 1)), Some("en0".into())));
        let i = "en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
\toptions=6460<TSO4,TSO6,CHANNEL_IO,PARTIAL_CSUM,ZEROINVERT_CSUM>
\tether 3c:22:fb:aa:bb:cc
\tinet6 fe80::1c2b:1234:5678:9abc%en0 prefixlen 64 secured scopeid 0xb
\tinet 192.168.1.23 netmask 0xffffff00 broadcast 192.168.1.255
\tstatus: active
";
        assert_eq!(parse_ifconfig(i), Some((Ipv4Addr::new(192, 168, 1, 23), 24, Some("3c:22:fb:aa:bb:cc".into()))));
    }

    #[test]
    fn linux() {
        assert_eq!(parse_ip_route("default via 10.0.0.1 dev wlp2s0 proto dhcp src 10.0.0.42 metric 600\n"), (Some(Ipv4Addr::new(10, 0, 0, 1)), Some("wlp2s0".into())));
        assert_eq!(parse_ip_addr("3: wlp2s0    inet 10.0.0.42/22 brd 10.0.3.255 scope global dynamic noprefixroute wlp2s0\\       valid_lft 85000sec preferred_lft 85000sec\n"), Some((Ipv4Addr::new(10, 0, 0, 42), 22)));
        assert_eq!(parse_ip_route(""), (None, None));
    }

    #[test]
    fn windows() {
        let s = "===========================================================================
IPv4 Route Table
===========================================================================
Active Routes:
Network Destination        Netmask          Gateway       Interface  Metric
          0.0.0.0          0.0.0.0      192.168.1.1    192.168.1.10     25
          0.0.0.0          0.0.0.0      172.20.0.1     172.20.0.5      300
        127.0.0.0        255.0.0.0         On-link         127.0.0.1    331
      192.168.0.0    255.255.254.0         On-link      192.168.1.10    281
     192.168.1.10  255.255.255.255         On-link      192.168.1.10    281
===========================================================================
";
        let n = parse_route_print(s).unwrap();
        assert_eq!(n.ip, Ipv4Addr::new(192, 168, 1, 10));
        assert_eq!(n.gateway, Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(n.prefix, 23);
        assert_eq!(n.key(), "lan:192.168.0.0/23");
    }
}
