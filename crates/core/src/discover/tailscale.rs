//! Tailscale: `tailscale status --json` from the installed CLI (the macOS app bundle's binary
//! too). Peers with names, addresses, OS, online, exit node, tags and direct / relayed path.
use crate::model::{Discovery, IdentKind, Layer, NetworkObservation, Observation, Source};
use serde_json::Value;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

/// Where the CLI lives besides PATH.
pub const EXTRA_PATHS: &[&str] = &[
    "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    "/usr/local/bin/tailscale",
    "/opt/homebrew/bin/tailscale",
    "/usr/bin/tailscale",
    "/snap/bin/tailscale",
    "C:\\Program Files\\Tailscale\\tailscale.exe",
];

pub fn find_cli() -> Option<PathBuf> {
    crate::cmd::find("tailscale", EXTRA_PATHS)
}

/// The tailnet as a network plus one observation per node (this computer included).
pub fn parse_status(json: &str) -> Result<Discovery, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let suffix = v.get("MagicDNSSuffix").and_then(Value::as_str).unwrap_or_default().to_string();
    let tailnet = v.pointer("/CurrentTailnet/Name").and_then(Value::as_str).map(str::to_string).filter(|s| !s.is_empty()).unwrap_or_else(|| suffix.clone());
    let key = format!("tailscale:{}", if tailnet.is_empty() { "tailnet" } else { &tailnet });
    let mut d = Discovery::default();
    let state = v.get("BackendState").and_then(Value::as_str).unwrap_or_default();
    let mut net = NetworkObservation { key: key.clone(), name: if tailnet.is_empty() { "Tailscale".into() } else { tailnet.clone() }, layer: Some(Layer::Tailscale), cidr: Some("100.64.0.0/10".into()), ..Default::default() }; // i18n-ok: product name
    net.facts.push(("backend_state".into(), state.to_string()));
    if !suffix.is_empty() {
        net.facts.push(("magic_dns".into(), suffix.clone()));
    }
    d.networks.push(net);
    let users = v.get("User").cloned().unwrap_or(Value::Null);
    if let Some(s) = v.get("Self") {
        d.devices.push(node(s, &key, &users, true));
    }
    if let Some(peers) = v.get("Peer").and_then(Value::as_object) {
        let mut keys: Vec<&String> = peers.keys().collect();
        keys.sort();
        for k in keys {
            d.devices.push(node(&peers[k], &key, &users, false));
        }
    }
    Ok(d)
}

fn node(n: &Value, net_key: &str, users: &Value, is_self: bool) -> Observation {
    let s = |k: &str| n.get(k).and_then(Value::as_str).map(str::to_string).filter(|v| !v.is_empty());
    let b = |k: &str| n.get(k).and_then(Value::as_bool).unwrap_or(false);
    let mut o = Observation::new(Layer::Tailscale, Source::Tailscale);
    if let Some(id) = s("ID") {
        o = o.ident(IdentKind::TailscaleNode, &id);
    }
    let host = s("HostName").filter(|h| !h.eq_ignore_ascii_case("localhost"));
    let dns = s("DNSName").map(|d| d.trim_end_matches('.').to_string());
    o.name = dns.as_deref().and_then(|d| d.split('.').next()).map(str::to_string).or(host.clone());
    if let Some(n) = o.name.clone() {
        o = o.ident(IdentKind::Host, &n);
    }
    o.hostname = dns.clone().or(host);
    o.os = s("OS");
    o.kind = o.os.as_deref().and_then(crate::discover::classify::from_os);
    o.ips = n.get("TailscaleIPs").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str()?.parse::<IpAddr>().ok()).collect()).unwrap_or_default();
    o.online = Some(is_self || b("Online"));
    o.network = Some(net_key.to_string());
    if is_self {
        o.facts.push(("this_computer".into(), "1".into()));
    }
    if b("ExitNode") {
        o.facts.push(("exit_node".into(), "active".into()));
    } else if b("ExitNodeOption") {
        o.facts.push(("exit_node".into(), "offered".into()));
    }
    let tags: Vec<String> = n.get("Tags").and_then(Value::as_array).map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect()).unwrap_or_default();
    if !tags.is_empty() {
        o.facts.push(("tailscale_tags".into(), tags.join(", ")));
    }
    if !is_self {
        let cur = s("CurAddr");
        let relay = s("Relay");
        match (cur, relay) {
            (Some(c), _) => o.facts.push(("path".into(), format!("direct {c}"))), // i18n-ok: data value
            (None, Some(r)) => o.facts.push(("path".into(), format!("relay {r}"))), // i18n-ok: data value
            _ => {}
        }
    }
    if let Some(uid) = n.get("UserID").and_then(Value::as_i64)
        && let Some(login) = users.get(uid.to_string()).and_then(|u| u.get("LoginName")).and_then(Value::as_str)
    {
        o.facts.push(("tailscale_user".into(), login.to_string()));
    }
    if let Some(ls) = s("LastSeen").filter(|l| !l.starts_with("0001-"))
        && let Some(t) = crate::time::parse_rfc3339(&ls)
    {
        o.seen_at = Some(t);
    }
    o
}

/// Run the CLI and parse it. Errors are user-facing (translated).
pub fn discover() -> Discovery {
    let Some(cli) = find_cli() else {
        return Discovery { notes: vec![(Source::Tailscale, i18n::t!("core.tailscale.not_found"))], ..Default::default() };
    };
    match crate::cmd::run(&cli, &["status", "--json"], Duration::from_secs(10)) {
        Ok(out) if !out.stdout.trim().is_empty() => match parse_status(&out.stdout) {
            Ok(d) => d,
            Err(e) => Discovery { notes: vec![(Source::Tailscale, i18n::t!("core.tailscale.bad_output", detail = e))], ..Default::default() },
        },
        Ok(out) => Discovery { notes: vec![(Source::Tailscale, i18n::t!("core.tailscale.failed", detail = out.stderr.trim()))], ..Default::default() },
        Err(e) => Discovery { notes: vec![(Source::Tailscale, i18n::t!("core.tailscale.failed", detail = format!("{e:?}")))], ..Default::default() },
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const STATUS: &str = r#"{
  "Version": "1.76.1-t",
  "BackendState": "Running",
  "TailscaleIPs": ["100.101.1.2", "fd7a:115c:a1e0::1"],
  "Self": {"ID": "nSelf1CNTRL", "PublicKey": "nodekey:aa", "HostName": "Juans-MBP", "DNSName": "juans-mbp.tail1234.ts.net.", "OS": "macOS", "UserID": 7, "TailscaleIPs": ["100.101.1.2", "fd7a:115c:a1e0::1"], "Online": true, "ExitNode": false, "ExitNodeOption": false, "Relay": "mia", "CurAddr": "", "LastSeen": "0001-01-01T00:00:00Z"},
  "MagicDNSSuffix": "tail1234.ts.net",
  "CurrentTailnet": {"Name": "example.com", "MagicDNSSuffix": "tail1234.ts.net", "MagicDNSEnabled": true},
  "Peer": {
    "nodekey:bb": {"ID": "nPi2CNTRL", "HostName": "pi", "DNSName": "pi.tail1234.ts.net.", "OS": "linux", "UserID": 7, "TailscaleIPs": ["100.101.1.3"], "Online": true, "ExitNode": true, "ExitNodeOption": true, "Tags": ["tag:server", "tag:home"], "Relay": "mia", "CurAddr": "203.0.113.5:41641", "LastSeen": "2026-11-20T10:00:00Z"},
    "nodekey:cc": {"ID": "nPhone3CNTRL", "HostName": "localhost", "DNSName": "juans-iphone.tail1234.ts.net.", "OS": "iOS", "UserID": 7, "TailscaleIPs": ["100.101.1.4"], "Online": false, "Relay": "mia", "CurAddr": "", "LastSeen": "2026-11-19T22:30:00Z"}
  },
  "User": {"7": {"ID": 7, "LoginName": "juan@example.com", "DisplayName": "Juan"}}
}"#;

    #[test]
    fn status_json() {
        let d = parse_status(STATUS).unwrap();
        assert_eq!(d.networks.len(), 1);
        assert_eq!(d.networks[0].key, "tailscale:example.com");
        assert_eq!(d.devices.len(), 3);
        let me = &d.devices[0];
        assert_eq!(me.name.as_deref(), Some("juans-mbp"));
        assert!(me.facts.contains(&("this_computer".into(), "1".into())));
        assert_eq!(me.ips.len(), 2);
        let pi = &d.devices[1];
        assert_eq!(pi.idents[0].value, "nPi2CNTRL");
        assert_eq!(pi.kind, Some(crate::model::DeviceKind::Server));
        assert!(pi.facts.contains(&("exit_node".into(), "active".into())));
        assert!(pi.facts.contains(&("path".into(), "direct 203.0.113.5:41641".into())));
        assert!(pi.facts.contains(&("tailscale_tags".into(), "tag:server, tag:home".into())));
        assert!(pi.facts.contains(&("tailscale_user".into(), "juan@example.com".into())));
        assert_eq!(pi.online, Some(true));
        assert_eq!(pi.seen_at, crate::time::parse_rfc3339("2026-11-20T10:00:00Z"));
        let phone = &d.devices[2];
        assert_eq!(phone.name.as_deref(), Some("juans-iphone"), "DNS name beats a 'localhost' host name");
        assert_eq!(phone.online, Some(false));
        assert!(phone.facts.contains(&("path".into(), "relay mia".into())));
        assert!(parse_status("not json").is_err());
    }
}
