//! IPv4/IPv6 prefixes: parse, contain, enumerate hosts, private-range checks.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cidr {
    pub addr: IpAddr,
    pub prefix: u8,
}

impl Cidr {
    pub fn parse(s: &str) -> Option<Cidr> {
        let (a, p) = s.trim().split_once('/').unwrap_or((s.trim(), ""));
        let addr: IpAddr = a.parse().ok()?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = if p.is_empty() { max } else { p.parse().ok()? };
        (prefix <= max).then(|| Cidr { addr, prefix }.network())
    }

    pub fn v4(ip: Ipv4Addr, prefix: u8) -> Cidr {
        Cidr { addr: IpAddr::V4(ip), prefix: prefix.min(32) }.network()
    }

    /// The same prefix with the host bits cleared.
    pub fn network(self) -> Cidr {
        match self.addr {
            IpAddr::V4(a) => Cidr { addr: IpAddr::V4(Ipv4Addr::from(u32::from(a) & mask4(self.prefix))), prefix: self.prefix },
            IpAddr::V6(a) => Cidr { addr: IpAddr::V6(Ipv6Addr::from(u128::from(a) & mask6(self.prefix))), prefix: self.prefix },
        }
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip) {
            (IpAddr::V4(n), IpAddr::V4(i)) => u32::from(i) & mask4(self.prefix) == u32::from(n),
            (IpAddr::V6(n), IpAddr::V6(i)) => u128::from(i) & mask6(self.prefix) == u128::from(n),
            _ => false,
        }
    }

    /// Usable IPv4 host addresses (no network / broadcast for prefixes under /31); empty for
    /// IPv6 or anything larger than `limit` hosts.
    pub fn hosts(&self, limit: usize) -> Vec<Ipv4Addr> {
        let IpAddr::V4(n) = self.addr else { return Vec::new() };
        let size = 1u64 << (32 - self.prefix as u32);
        let (first, last) = if self.prefix >= 31 { (0, size - 1) } else { (1, size - 2) };
        if last + 1 - first > limit as u64 {
            return Vec::new();
        }
        let base = u32::from(n) as u64;
        (first..=last).map(|o| Ipv4Addr::from((base + o) as u32)).collect()
    }

    /// For IPv4 only: the broadcast address.
    pub fn broadcast(&self) -> Option<Ipv4Addr> {
        let IpAddr::V4(n) = self.addr else { return None };
        Some(Ipv4Addr::from(u32::from(n) | !mask4(self.prefix)))
    }
}

impl std::fmt::Display for Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

fn mask4(p: u8) -> u32 {
    if p == 0 { 0 } else { u32::MAX << (32 - p as u32) }
}

fn mask6(p: u8) -> u128 {
    if p == 0 { 0 } else { u128::MAX << (128 - p as u32) }
}

/// `0xffffff00` / `255.255.255.0` → 24.
pub fn prefix_from_mask(m: &str) -> Option<u8> {
    let m = m.trim();
    let n = if let Some(h) = m.strip_prefix("0x") { u32::from_str_radix(h, 16).ok()? } else { u32::from(m.parse::<Ipv4Addr>().ok()?) };
    let p = n.leading_ones();
    (n.count_ones() == p).then_some(p as u8)
}

/// RFC 1918 / ULA / link-local: the ranges scanned by default.
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => a.is_private() || a.is_link_local(),
        IpAddr::V6(a) => (a.segments()[0] & 0xfe00) == 0xfc00 || (a.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// 100.64.0.0/10, where Tailscale (and carrier NAT) addresses live.
pub fn is_cgnat(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V4(a) if a.octets()[0] == 100 && (a.octets()[1] & 0xc0) == 64)
}

/// The /24 a scan of `ip`'s subnet covers: the subnet itself when it is /24 or smaller.
pub fn scan_range(ip: Ipv4Addr, prefix: u8) -> Cidr {
    Cidr::v4(ip, prefix.max(24))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_contains_hosts() {
        let c = Cidr::parse("192.168.1.77/24").unwrap();
        assert_eq!(c.to_string(), "192.168.1.0/24");
        assert!(c.contains("192.168.1.254".parse().unwrap()));
        assert!(!c.contains("192.168.2.1".parse().unwrap()));
        assert!(!c.contains("::1".parse().unwrap()));
        let h = c.hosts(1024);
        assert_eq!(h.len(), 254);
        assert_eq!(h[0], Ipv4Addr::new(192, 168, 1, 1));
        assert_eq!(h[253], Ipv4Addr::new(192, 168, 1, 254));
        assert_eq!(c.broadcast(), Some(Ipv4Addr::new(192, 168, 1, 255)));
        assert!(Cidr::parse("10.0.0.0/8").unwrap().hosts(1024).is_empty(), "too big");
        assert_eq!(Cidr::parse("10.0.0.5").unwrap().prefix, 32);
        assert_eq!(Cidr::parse("10.0.0.0/33"), None);
        assert_eq!(Cidr::parse("fd7a:115c:a1e0::/48").unwrap().to_string(), "fd7a:115c:a1e0::/48");
        assert_eq!(prefix_from_mask("0xffffff00"), Some(24));
        assert_eq!(prefix_from_mask("255.255.252.0"), Some(22));
        assert_eq!(prefix_from_mask("255.0.255.0"), None);
        assert!(is_private("10.1.2.3".parse().unwrap()));
        assert!(is_private("fe80::1".parse().unwrap()));
        assert!(!is_private("8.8.8.8".parse().unwrap()));
        assert!(is_cgnat("100.101.102.103".parse().unwrap()));
        assert!(!is_cgnat("100.128.0.1".parse().unwrap()));
        assert_eq!(scan_range(Ipv4Addr::new(10, 0, 5, 9), 16).to_string(), "10.0.5.0/24");
        assert_eq!(scan_range(Ipv4Addr::new(10, 0, 5, 9), 26).to_string(), "10.0.5.0/26");
    }
}
