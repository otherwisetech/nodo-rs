//! Reaching a device from a terminal: which address to use (`ssh` prefers the tailnet), the SSH
//! user kept on the device, and the `ssh` argument list. Nothing here goes through a shell: the
//! app runs `ssh` with these arguments directly, and only the external-terminal path joins them
//! into one shell-quoted line. Device names never reach either; host and user are checked so a
//! value can't turn into an `ssh` option (`-oProxyCommand=…`).
use crate::model::{Device, Layer};
use std::net::IpAddr;

/// The custom field that holds the SSH user (`ssh user: pi`), matched case-insensitively and with
/// `_` for the space.
pub const SSH_USER_FIELD: &str = "ssh user";

/// MagicDNS suffixes Tailscale hands out; a tailnet network's own `magic_dns` fact adds to these.
const TS_SUFFIXES: &[&str] = &[".ts.net", ".beta.tailscale.net"];

pub fn is_user_field(key: &str) -> bool {
    let k = key.trim().replace('_', " ");
    k.eq_ignore_ascii_case(SSH_USER_FIELD)
}

pub fn ssh_user(d: &Device) -> Option<String> {
    d.fields.iter().find(|(k, _)| is_user_field(k)).map(|(_, v)| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// Set (or with an empty value, remove) the SSH user field, keeping the other fields' order.
pub fn set_ssh_user(fields: &mut Vec<(String, String)>, user: &str) {
    let user = user.trim();
    match fields.iter().position(|(k, _)| is_user_field(k)) {
        Some(i) if user.is_empty() => {
            fields.remove(i);
        }
        Some(i) => fields[i].1 = user.to_string(),
        None if user.is_empty() => {}
        None => fields.push((SSH_USER_FIELD.to_string(), user.to_string())),
    }
}

/// A user name `ssh` will take as one: POSIX portable characters, never starting with `-`.
pub fn valid_user(u: &str) -> bool {
    !u.is_empty() && u.len() <= 64 && !u.starts_with('-') && u.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '$'))
}

/// A DNS name or an IP literal, never starting with `-`.
pub fn valid_host(h: &str) -> bool {
    if h.parse::<IpAddr>().is_ok() {
        return true;
    }
    !h.is_empty() && h.len() <= 253 && !h.starts_with(['-', '.']) && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Where to reach `d`: its MagicDNS name or Tailscale address when it is on the tailnet, else its
/// LAN address, else whatever address it has. `suffixes` are the tailnets' own MagicDNS suffixes
/// (the `magic_dns` network fact), for tailnets not under `.ts.net`.
pub fn best_host(d: &Device, suffixes: &[String]) -> Option<String> {
    let on_tailnet = d.addrs.iter().any(|a| a.layer == Layer::Tailscale);
    if on_tailnet {
        if let Some(h) = d.hostname.as_deref().map(|h| h.trim_end_matches('.')).filter(|h| is_magic_dns(h, suffixes) && valid_host(h)) {
            return Some(h.to_string());
        }
        let ts = |v4: bool| d.addrs.iter().find(|a| a.layer == Layer::Tailscale && a.ip.is_ipv4() == v4).map(|a| a.ip);
        if let Some(ip) = ts(true).or_else(|| ts(false)) {
            return Some(ip.to_string());
        }
    }
    let lan = |v4: bool| d.addrs.iter().find(|a| a.layer == Layer::Lan && a.ip.is_ipv4() == v4 && !is_link_local(a.ip)).map(|a| a.ip);
    lan(true).or_else(|| lan(false)).or_else(|| d.primary_ip()).map(|ip| ip.to_string())
}

fn is_magic_dns(h: &str, suffixes: &[String]) -> bool {
    let h = h.to_ascii_lowercase();
    TS_SUFFIXES.iter().any(|s| h.ends_with(s)) || suffixes.iter().map(|s| s.trim_matches('.').to_ascii_lowercase()).filter(|s| !s.is_empty()).any(|s| h.ends_with(&format!(".{s}")))
}

fn is_link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_link_local(),
        IpAddr::V6(v) => (v.segments()[0] & 0xffc0) == 0xfe80,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshError {
    NoAddress,
    BadUser(String),
    BadHost(String),
}

/// `ssh` and its arguments for `user@host`.
pub fn ssh_argv(user: Option<&str>, host: &str) -> Result<(String, Vec<String>), SshError> {
    let host = host.trim();
    if !valid_host(host) {
        return Err(SshError::BadHost(host.to_string()));
    }
    let dest = match user.map(str::trim).filter(|u| !u.is_empty()) {
        Some(u) if !valid_user(u) => return Err(SshError::BadUser(u.to_string())),
        Some(u) => format!("{u}@{host}"),
        None => host.to_string(),
    };
    Ok(("ssh".to_string(), vec![dest]))
}

/// The SSH command for a device, from its user field and best address.
pub fn ssh_for(d: &Device, suffixes: &[String]) -> Result<(String, Vec<String>), SshError> {
    let host = best_host(d, suffixes).ok_or(SshError::NoAddress)?;
    ssh_argv(ssh_user(d).as_deref(), &host)
}

/// POSIX shell quoting: plain words as is, anything else in single quotes.
pub fn sh_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// One shell-safe command line, for showing and for terminals that only take a string.
pub fn command_line(program: &str, args: &[String]) -> String {
    std::iter::once(program).chain(args.iter().map(String::as_str)).map(sh_quote).collect::<Vec<_>>().join(" ")
}

/// Variables a shell opened "at" a device gets, so scripts can use them.
pub fn device_env(d: &Device, suffixes: &[String]) -> Vec<(String, String)> {
    let mut env = vec![("NODO_DEVICE_NAME".to_string(), d.name.clone())];
    if let Some(ip) = d.primary_ip() {
        env.push(("NODO_DEVICE_IP".into(), ip.to_string()));
    }
    if let Some(h) = best_host(d, suffixes) {
        env.push(("NODO_DEVICE_HOST".into(), h));
    }
    if let Some(u) = ssh_user(d).filter(|u| valid_user(u)) {
        env.push(("NODO_DEVICE_USER".into(), u));
    }
    if let Some(m) = d.macs().next() {
        env.push(("NODO_DEVICE_MAC".into(), m.to_string()));
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Addr, Source};

    fn dev(addrs: &[(&str, Layer)], hostname: Option<&str>) -> Device {
        Device {
            id: 1,
            name: "pi".into(),
            hostname: hostname.map(str::to_string),
            addrs: addrs.iter().map(|(ip, l)| Addr { ip: ip.parse().unwrap(), layer: *l, network: None, source: Source::Arp, last_seen: 0 }).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn tailnet_first_then_lan() {
        let both = dev(&[("192.168.1.20", Layer::Lan), ("100.101.1.3", Layer::Tailscale), ("fd7a:115c:a1e0::3", Layer::Tailscale)], Some("pi.tail1234.ts.net."));
        assert_eq!(best_host(&both, &[]).as_deref(), Some("pi.tail1234.ts.net"), "MagicDNS name wins");
        let no_dns = dev(&[("192.168.1.20", Layer::Lan), ("fd7a:115c:a1e0::3", Layer::Tailscale), ("100.101.1.3", Layer::Tailscale)], Some("pi.local"));
        assert_eq!(best_host(&no_dns, &[]).as_deref(), Some("100.101.1.3"), "not a MagicDNS name: the IPv4 tailnet address");
        let headscale = dev(&[("100.64.0.9", Layer::Tailscale)], Some("box.hs.example.org"));
        assert_eq!(best_host(&headscale, &[]).as_deref(), Some("100.64.0.9"));
        assert_eq!(best_host(&headscale, &["hs.example.org.".into()]).as_deref(), Some("box.hs.example.org"), "the tailnet's own suffix");
        let lan = dev(&[("fe80::1", Layer::Lan), ("fd00::5", Layer::Lan), ("10.0.0.5", Layer::Lan)], Some("nas.tail1234.ts.net"));
        assert_eq!(best_host(&lan, &[]).as_deref(), Some("10.0.0.5"), "off the tailnet a ts.net name is not used");
        let v6 = dev(&[("fe80::1", Layer::Lan), ("fd00::5", Layer::Lan)], None);
        assert_eq!(best_host(&v6, &[]).as_deref(), Some("fd00::5"), "link-local skipped");
        let aws = dev(&[("172.31.0.4", Layer::Aws)], None);
        assert_eq!(best_host(&aws, &[]).as_deref(), Some("172.31.0.4"));
        assert_eq!(best_host(&dev(&[], None), &[]), None);
    }

    #[test]
    fn argv_never_carries_options_or_shell() {
        assert_eq!(ssh_argv(Some("pi"), "10.0.0.5").unwrap(), ("ssh".to_string(), vec!["pi@10.0.0.5".to_string()]));
        assert_eq!(ssh_argv(None, "fd00::5").unwrap().1, vec!["fd00::5".to_string()]);
        assert_eq!(ssh_argv(Some("  "), "nas.local").unwrap().1, vec!["nas.local".to_string()]);
        assert_eq!(ssh_argv(Some("-oProxyCommand=touch /tmp/x"), "h"), Err(SshError::BadUser("-oProxyCommand=touch /tmp/x".into())));
        assert_eq!(ssh_argv(Some("me;rm -rf ~"), "h"), Err(SshError::BadUser("me;rm -rf ~".into())));
        assert_eq!(ssh_argv(None, "-oProxyCommand=x"), Err(SshError::BadHost("-oProxyCommand=x".into())));
        assert_eq!(ssh_argv(None, "host name"), Err(SshError::BadHost("host name".into())));
        assert_eq!(ssh_argv(None, "$(reboot)"), Err(SshError::BadHost("$(reboot)".into())));
        assert!(valid_user("deploy_1") && valid_user("ana.m") && !valid_user("") && !valid_user("a b") && !valid_user("ü"));
    }

    #[test]
    fn device_names_and_fields() {
        // a hostile device name changes nothing in the command: only host and user go in
        let mut d = dev(&[("10.0.0.7", Layer::Lan)], Some("x"));
        d.name = "'; rm -rf / #".into();
        d.fields = vec![("Rack".into(), "A2".into()), ("SSH_User".into(), "admin".into())];
        assert_eq!(ssh_for(&d, &[]).unwrap().1, vec!["admin@10.0.0.7".to_string()]);
        let env = device_env(&d, &[]);
        assert!(env.contains(&("NODO_DEVICE_NAME".into(), "'; rm -rf / #".into())), "passed as a value, never through a shell");
        assert!(env.contains(&("NODO_DEVICE_IP".into(), "10.0.0.7".into())));
        assert!(env.contains(&("NODO_DEVICE_USER".into(), "admin".into())));
        set_ssh_user(&mut d.fields, "pi");
        assert_eq!(d.fields, vec![("Rack".to_string(), "A2".to_string()), ("SSH_User".to_string(), "pi".to_string())], "kept in place");
        set_ssh_user(&mut d.fields, "");
        assert_eq!(d.fields, vec![("Rack".to_string(), "A2".to_string())]);
        set_ssh_user(&mut d.fields, "root");
        assert_eq!(ssh_user(&d).as_deref(), Some("root"));
        assert_eq!(ssh_for(&dev(&[], None), &[]), Err(SshError::NoAddress));
    }

    #[test]
    fn quoting_for_external_terminals() {
        assert_eq!(command_line("ssh", &["pi@10.0.0.5".into()]), "ssh pi@10.0.0.5");
        assert_eq!(command_line("ping", &["-c".into(), "5".into(), "fd00::5".into()]), "ping -c 5 fd00::5");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_quote(""), "''");
        assert_eq!(sh_quote("a b"), "'a b'");
    }
}
