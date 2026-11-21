//! Tailscale: `tailscale status --json` from the installed CLI (the macOS app bundle's binary
//! too). Peers with names, owners, addresses, OS, online, exit node, tags and direct / relayed
//! path.
use crate::model::{Discovery, IdentKind, Layer, NetworkObservation, Observation, Source};
use serde_json::Value;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

/// Where the CLI lives besides PATH. The macOS app (App Store or standalone) ships the CLI as
/// its own main binary.
pub const EXTRA_PATHS: &[&str] = &[
    "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    "/usr/local/bin/tailscale",
    "/opt/homebrew/bin/tailscale",
    "/usr/bin/tailscale",
    "/usr/sbin/tailscale",
    "/snap/bin/tailscale",
    "C:\\Program Files\\Tailscale\\tailscale.exe",
];

const TIMEOUT: Duration = Duration::from_secs(15);

pub fn find_cli() -> Option<PathBuf> {
    crate::cmd::find("tailscale", EXTRA_PATHS).or_else(|| {
        // an app installed for this user only
        let home = std::env::var_os("HOME")?;
        let p = PathBuf::from(home).join("Applications/Tailscale.app/Contents/MacOS/Tailscale");
        p.is_file().then_some(p)
    })
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
    // the OS host name ("MacBook Pro de Juan" → macbook-pro-de-juan) can differ from the
    // MagicDNS name and is what mDNS / DHCP see on the LAN
    if let Some(h) = &host {
        o = o.ident(IdentKind::Host, &h.split_whitespace().collect::<Vec<_>>().join("-"));
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
        && let Some(u) = users.get(uid.to_string())
    {
        let us = |k: &str| u.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
        if let Some(login) = us("LoginName") {
            o.facts.push(("tailscale_user".into(), login));
        }
        o.owner = us("DisplayName").or_else(|| us("LoginName"));
    }
    // endpoints on a private LAN (this computer's own, or a peer reached directly) point at the
    // LAN device the node is
    let endpoint_ip = |a: &str| a.parse::<std::net::SocketAddr>().ok().map(|s| s.ip());
    let mut eps: Vec<String> = n.get("Addrs").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
    eps.extend(s("CurAddr"));
    for ip in eps.iter().filter_map(|a| endpoint_ip(a)) {
        if matches!(ip, IpAddr::V4(v4) if v4.is_private()) && !o.lan_hints.contains(&ip) {
            o.lan_hints.push(ip);
        }
    }
    if let Some(ls) = s("LastSeen").filter(|l| !l.starts_with("0001-"))
        && let Some(t) = crate::time::parse_rfc3339(&ls)
    {
        o.seen_at = Some(t);
    }
    o
}

/// Run the CLI and parse it. Errors are user-facing (translated) notes.
pub fn discover() -> Discovery {
    let Some(cli) = find_cli() else {
        return note(i18n::t!("core.tailscale.not_found"));
    };
    // the macOS app's binary only acts as the CLI when it thinks it runs in a terminal; started
    // from Finder (no TERM) it tries to open a second GUI and prints nothing
    let out = crate::cmd::run_env(&cli, &["status", "--json"], &[("TAILSCALE_BE_CLI", "1")], TIMEOUT);
    from_output(out)
}

fn note(msg: String) -> Discovery {
    Discovery { notes: vec![(Source::Tailscale, msg)], ..Default::default() }
}

/// What a `tailscale status --json` run gave, as a discovery or a note.
pub fn from_output(out: Result<crate::cmd::Output, crate::cmd::CmdError>) -> Discovery {
    use crate::cmd::CmdError;
    let out = match out {
        Ok(o) => o,
        Err(CmdError::NotFound) => return note(i18n::t!("core.tailscale.not_found")),
        Err(CmdError::Timeout) => return note(i18n::t!("core.tailscale.timeout", secs = TIMEOUT.as_secs())),
        Err(CmdError::Failed(e)) => return note(i18n::t!("core.tailscale.failed", detail = e)),
    };
    // `status --json` prints the status even when the backend is stopped (with a non-zero exit)
    let body = out.stdout.trim();
    if !body.starts_with('{') {
        let detail = [out.stderr.trim(), body].into_iter().find(|s| !s.is_empty()).unwrap_or_default().lines().next().unwrap_or_default().to_string();
        return note(i18n::t!("core.tailscale.failed", detail = detail));
    }
    let mut d = match parse_status(body) {
        Ok(d) => d,
        Err(e) => return note(i18n::t!("core.tailscale.bad_output", detail = e)),
    };
    if let Some(state) = d.networks.first().and_then(|n| n.facts.iter().find(|(k, _)| k == "backend_state")).map(|(_, v)| v.clone())
        && state != "Running"
    {
        d.notes.push((Source::Tailscale, i18n::t!("core.tailscale.not_running", state = state)));
    }
    d
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

    /// Shaped like 1.104's output (fake names, keys and addresses): three users, peers keyed by
    /// node key, `Addrs` only on Self, a spaced OS host name, a peer reached directly on the LAN.
    pub const STATUS_1_104: &str = r#"{
  "Version": "1.104.7-t0000000-g0000000",
  "TUN": false,
  "BackendState": "Running",
  "HaveNodeKey": true,
  "AuthURL": "",
  "TailscaleIPs": ["100.80.0.1", "fd7a:115c:a1e0::1"],
  "Self": {"ID": "nSelfAAAA", "PublicKey": "nodekey:0001", "HostName": "Laptop de Ana", "DNSName": "laptop.tailabc12.ts.net.", "OS": "macOS", "UserID": 1001, "TailscaleIPs": ["100.80.0.1", "fd7a:115c:a1e0::1"], "AllowedIPs": ["100.80.0.1/32"], "Addrs": ["198.51.100.7:9749", "192.168.50.7:41641", "192.168.64.1:41641"], "CurAddr": "", "Relay": "mia", "PeerRelay": "", "RxBytes": 0, "TxBytes": 0, "Created": "2025-01-01T00:00:00Z", "LastWrite": "0001-01-01T00:00:00Z", "LastSeen": "0001-01-01T00:00:00Z", "LastHandshake": "0001-01-01T00:00:00Z", "Online": true, "ExitNode": false, "ExitNodeOption": false, "Active": false, "PeerAPIURL": ["http://100.80.0.1:12345"], "Capabilities": [], "CapMap": {}, "InNetworkMap": true, "InMagicSock": false, "InEngine": false},
  "Health": [],
  "MagicDNSSuffix": "tailabc12.ts.net",
  "CurrentTailnet": {"Name": "ana@example.com", "StableID": "T000CNTRL", "MagicDNSSuffix": "tailabc12.ts.net", "MagicDNSEnabled": true},
  "CertDomains": null,
  "Peer": {
    "nodekey:0002": {"ID": "nDeskBBBB", "PublicKey": "nodekey:0002", "HostName": "DESKTOP-AB12CD3", "DNSName": "desktop-ab12cd3.tailabc12.ts.net.", "OS": "windows", "UserID": 1002, "TailscaleIPs": ["100.80.0.2", "fd7a:115c:a1e0::2"], "Addrs": null, "CurAddr": "", "Relay": "mia", "PeerRelay": "", "LastSeen": "2026-11-20T09:00:00.1Z", "Online": true, "ExitNode": false, "ExitNodeOption": false, "Active": false, "InNetworkMap": true},
    "nodekey:0003": {"ID": "nStudioCCC", "PublicKey": "nodekey:0003", "HostName": "Studio de Ana", "DNSName": "studio.tailabc12.ts.net.", "OS": "macOS", "UserID": 1001, "TailscaleIPs": ["100.80.0.3", "fd7a:115c:a1e0::3"], "Addrs": null, "CurAddr": "192.168.50.20:41641", "Relay": "mia", "PeerRelay": "", "LastSeen": "0001-01-01T00:00:00Z", "Online": true, "ExitNode": false, "ExitNodeOption": true, "Active": true, "InNetworkMap": true},
    "nodekey:0004": {"ID": "nPhoneDDDD", "PublicKey": "nodekey:0004", "HostName": "localhost", "DNSName": "phone-ana.tailabc12.ts.net.", "OS": "iOS", "UserID": 1001, "TailscaleIPs": ["100.80.0.4", "fd7a:115c:a1e0::4"], "Addrs": null, "CurAddr": "", "Relay": "", "PeerRelay": "", "LastSeen": "2026-10-09T00:16:14.1Z", "Online": false, "ExitNode": false, "ExitNodeOption": false, "Active": false, "InNetworkMap": true},
    "nodekey:0005": {"ID": "nVpsEEEE", "PublicKey": "nodekey:0005", "HostName": "srv123", "DNSName": "vps.tailabc12.ts.net.", "OS": "linux", "UserID": 1003, "TailscaleIPs": ["100.80.0.5"], "Addrs": null, "CurAddr": "203.0.113.9:41641", "Relay": "nyc", "PeerRelay": "", "Tags": ["tag:server"], "LastSeen": "2026-05-05T03:38:17.1Z", "Online": false, "ExitNode": false, "ExitNodeOption": false, "Active": false, "InNetworkMap": true}
  },
  "User": {
    "1001": {"ID": 1001, "LoginName": "ana@example.com", "DisplayName": "Ana P", "ProfilePicURL": ""},
    "1002": {"ID": 1002, "LoginName": "office@example.org", "DisplayName": "Example Office", "ProfilePicURL": ""},
    "1003": {"ID": 1003, "LoginName": "ops@example.net", "DisplayName": "", "ProfilePicURL": ""}
  },
  "ClientVersion": null
}"#;

    #[test]
    fn status_json_1_104() {
        let d = parse_status(STATUS_1_104).unwrap();
        assert_eq!(d.networks[0].key, "tailscale:ana@example.com");
        assert_eq!(d.devices.len(), 5, "self + 4 peers");
        let me = &d.devices[0];
        assert_eq!(me.name.as_deref(), Some("laptop"));
        assert_eq!(me.owner.as_deref(), Some("Ana P"));
        assert!(me.idents.contains(&crate::model::Ident::new(IdentKind::Host, "laptop-de-ana")), "{:?}", me.idents);
        let hints: Vec<String> = me.lan_hints.iter().map(|i| i.to_string()).collect();
        assert_eq!(hints, ["192.168.50.7", "192.168.64.1"], "private endpoints only");
        let by = |n: &str| d.devices.iter().find(|o| o.name.as_deref() == Some(n)).unwrap();
        assert_eq!(by("desktop-ab12cd3").owner.as_deref(), Some("Example Office"));
        assert!(by("desktop-ab12cd3").lan_hints.is_empty());
        let studio = by("studio");
        assert_eq!(studio.lan_hints, vec!["192.168.50.20".parse::<IpAddr>().unwrap()]);
        assert!(studio.facts.contains(&("path".into(), "direct 192.168.50.20:41641".into())));
        assert!(studio.facts.contains(&("exit_node".into(), "offered".into())));
        assert_eq!(studio.seen_at, None, "0001-01-01 is no date");
        let phone = by("phone-ana");
        assert_eq!(phone.online, Some(false));
        assert!(!phone.facts.iter().any(|(k, _)| k == "path"), "empty Relay / CurAddr");
        let vps = by("vps");
        assert_eq!(vps.owner.as_deref(), Some("ops@example.net"), "login when there is no display name");
        assert!(vps.lan_hints.is_empty(), "public endpoint");
        assert_eq!(vps.ips.len(), 1);
    }

    fn out(stdout: &str, stderr: &str, ok: bool) -> Result<crate::cmd::Output, crate::cmd::CmdError> {
        Ok(crate::cmd::Output { ok, stdout: stdout.into(), stderr: stderr.into() })
    }

    #[test]
    fn run_results() {
        let d = from_output(out(STATUS_1_104, "", true));
        assert_eq!(d.devices.len(), 5);
        assert!(d.notes.is_empty());
        // the macOS app binary started without TERM tries to open its GUI instead
        let d = from_output(out("", "The Tailscale GUI failed to start: (Tailscale.CLIError error 3.)\n", true));
        assert!(d.devices.is_empty());
        assert_eq!(d.notes.len(), 1);
        assert!(d.notes[0].1.contains("GUI failed to start"), "the error is shown, not swallowed: {}", d.notes[0].1);
        let stopped = STATUS_1_104.replace(r#""BackendState": "Running""#, r#""BackendState": "Stopped""#);
        let d = from_output(out(&stopped, "", false));
        assert_eq!(d.notes.len(), 1);
        assert!(d.notes[0].1.contains("Stopped"));
        assert_eq!(from_output(Err(crate::cmd::CmdError::Timeout)).notes.len(), 1);
        assert!(from_output(out("{not json", "", true)).notes[0].1.contains(':'));
    }
}
