//! WireGuard: `wg show all dump` (tab-separated; usually needs root) or a dump file the user
//! saved with `sudo wg show all dump > wg.txt`. Interfaces become networks, peers devices.
use crate::model::{Discovery, IdentKind, Layer, NetworkObservation, Observation, Source};
use std::net::IpAddr;
use std::time::Duration;

/// A peer counts as online when its last handshake is this recent (WireGuard re-keys every
/// two minutes on an active tunnel).
pub const ONLINE_WITHIN: i64 = 180;

pub const SUDO_HINT: &str = "sudo wg show all dump > ~/nodo-wg.txt";

/// Parse a dump. Interface lines have 5 fields, peer lines 9.
pub fn parse_dump(s: &str, now: i64) -> Discovery {
    let mut d = Discovery::default();
    for l in s.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        match f.len() {
            5 => {
                let iface = f[0];
                let mut n = NetworkObservation { key: format!("wg:{iface}"), name: iface.to_string(), layer: Some(Layer::WireGuard), ..Default::default() };
                n.facts.push(("public_key".into(), f[2].to_string()));
                if f[3] != "0" {
                    n.facts.push(("listen_port".into(), f[3].to_string()));
                }
                d.networks.push(n);
            }
            9 => {
                let (iface, key, endpoint, allowed, hs) = (f[0], f[1], f[3], f[4], f[5].parse::<i64>().unwrap_or(0));
                let mut o = Observation::new(Layer::WireGuard, Source::WireGuard).ident(IdentKind::WgPeer, key);
                o.network = Some(format!("wg:{iface}"));
                // host routes (/32, /128) are the peer's own addresses; wider ones are networks behind it
                let mut routed = Vec::new();
                for a in allowed.split(',').map(str::trim).filter(|a| !a.is_empty() && *a != "(none)") {
                    match a.split_once('/') {
                        Some((ip, "32" | "128")) => {
                            if let Ok(ip) = ip.parse::<IpAddr>() {
                                o.ips.push(ip);
                            }
                        }
                        _ => routed.push(a.to_string()),
                    }
                }
                o.name = Some(format!("{}…", &key[..key.len().min(8)]));
                o.online = Some(hs > 0 && now - hs <= ONLINE_WITHIN);
                if hs > 0 {
                    o.seen_at = Some(hs);
                }
                o = o.fact("endpoint", if endpoint == "(none)" { String::new() } else { endpoint.to_string() }).fact("allowed_ips", allowed.to_string()).fact("routes", routed.join(", ")).fact("rx_bytes", f[6].to_string()).fact("tx_bytes", f[7].to_string());
                if f[8] != "off" {
                    o = o.fact("keepalive", f[8].to_string());
                }
                d.devices.push(o);
            }
            _ => {}
        }
    }
    d
}

/// `wg` refused for lack of privileges.
pub fn needs_root(stderr: &str) -> bool {
    let e = stderr.to_lowercase();
    e.contains("operation not permitted") || e.contains("permission denied") || e.contains("unable to access interface")
}

pub fn discover(now: i64) -> Discovery {
    let Some(wg) = crate::cmd::find("wg", &["/opt/homebrew/bin/wg", "/usr/local/bin/wg", "/usr/bin/wg"]) else {
        return Discovery { notes: vec![(Source::WireGuard, i18n::t!("core.wg.not_found"))], ..Default::default() };
    };
    match crate::cmd::run(&wg, &["show", "all", "dump"], Duration::from_secs(10)) {
        Ok(o) if o.ok && !needs_root(&o.stderr) => parse_dump(&o.stdout, now),
        Ok(o) if needs_root(&o.stderr) || o.stdout.trim().is_empty() && !o.ok => Discovery { notes: vec![(Source::WireGuard, i18n::t!("core.wg.needs_root", command = SUDO_HINT))], ..Default::default() },
        Ok(o) => Discovery { notes: vec![(Source::WireGuard, i18n::t!("core.wg.failed", detail = o.stderr.trim()))], ..Default::default() },
        Err(e) => Discovery { notes: vec![(Source::WireGuard, i18n::t!("core.wg.failed", detail = format!("{e:?}")))], ..Default::default() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = "wg0\tcHJpdmF0ZQ==\tSERVERPUBKEY=\t51820\toff
wg0\tPEERONEKEYAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\t(none)\t203.0.113.7:51820\t10.8.0.2/32,192.168.50.0/24\t1795000000\t12345\t67890\t25
wg0\tPEERTWOKEYBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=\t(none)\t(none)\t10.8.0.3/32,fd00::3/128\t0\t0\t0\toff
utun5\tcHJpdmF0ZQ==\tCLIENTPUB=\t0\toff
";

    #[test]
    fn dump() {
        let d = parse_dump(DUMP, 1_795_000_100);
        assert_eq!(d.networks.iter().map(|n| n.key.as_str()).collect::<Vec<_>>(), ["wg:wg0", "wg:utun5"]);
        assert!(d.networks[0].facts.contains(&("listen_port".into(), "51820".into())));
        assert_eq!(d.devices.len(), 2);
        let a = &d.devices[0];
        assert_eq!(a.ips, vec!["10.8.0.2".parse::<IpAddr>().unwrap()]);
        assert_eq!(a.online, Some(true));
        assert!(a.facts.contains(&("routes".into(), "192.168.50.0/24".into())));
        assert!(a.facts.contains(&("endpoint".into(), "203.0.113.7:51820".into())));
        assert_eq!(a.idents[0].kind, IdentKind::WgPeer);
        let b = &d.devices[1];
        assert_eq!(b.ips.len(), 2);
        assert_eq!(b.online, Some(false));
        assert_eq!(b.seen_at, None);
        assert!(needs_root("Unable to access interface: Operation not permitted"));
        assert!(!needs_root(""));
    }
}
