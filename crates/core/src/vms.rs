//! Best-effort names for the guests on this computer's VM / container bridges: which
//! virtualisation runtimes are running (from the process list and their config files), so
//! 192.168.64.3 on bridge100 reads "Kiri VM (Docker)" rather than an address.
use crate::cidr::Cidr;
use crate::model::{Device, DeviceId};
use std::net::IpAddr;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runtime {
    pub id: &'static str,
    /// Product name.
    pub name: &'static str,
    /// What its guest runs, when that's what it is for.
    pub engine: Option<&'static str>,
    /// The guest's address when the runtime's config says.
    pub ip: Option<IpAddr>,
}

/// (process-path fragment, id, product name, engine). Product names are not translated.
const KNOWN: &[(&str, &str, &str, Option<&str>)] = &[
    ("/Kiri.app/", "kiri", "Kiri", Some("Docker")),
    ("com.docker.backend", "docker", "Docker Desktop", None),
    ("/Docker.app/", "docker", "Docker Desktop", None),
    ("/OrbStack.app/", "orbstack", "OrbStack", None),
    ("colima", "colima", "Colima", Some("Docker")),
    ("limactl", "lima", "Lima", None),
    ("/UTM.app/", "utm", "UTM", None),
    ("prl_vm_app", "parallels", "Parallels", None),
    ("vmware-vmx", "vmware", "VMware", None),
    ("VBoxHeadless", "virtualbox", "VirtualBox", None),
    ("VirtualBoxVM", "virtualbox", "VirtualBox", None),
    ("multipassd", "multipass", "Multipass", None),
];

/// `ps -axo comm=` (one executable path per line) → the runtimes running.
pub fn parse_ps(s: &str) -> Vec<Runtime> {
    let mut out: Vec<Runtime> = Vec::new();
    for l in s.lines() {
        for (frag, id, name, engine) in KNOWN {
            if l.contains(frag) && !out.iter().any(|r| r.id == *id) {
                out.push(Runtime { id, name, engine: *engine, ip: None });
            }
        }
    }
    out
}

/// A guest address in a runtime's JSON config (`~/.kiri/vm.json`: `ip`, `ip_address`,
/// `address`…, at any depth).
pub fn parse_vm_json(s: &str) -> Option<IpAddr> {
    fn walk(v: &serde_json::Value) -> Option<IpAddr> {
        match v {
            serde_json::Value::Object(m) => {
                for (k, x) in m {
                    let k = k.to_lowercase();
                    if (k == "ip" || k.contains("ip_address") || k.contains("ipaddress") || k == "address" || k == "guest_ip")
                        && let Some(ip) = x.as_str().and_then(|s| s.split('/').next()?.parse().ok())
                    {
                        return Some(ip);
                    }
                }
                m.values().find_map(walk)
            }
            serde_json::Value::Array(a) => a.iter().find_map(walk),
            _ => None,
        }
    }
    walk(&serde_json::from_str(s).ok()?)
}

/// The runtimes running on this computer. Blocking (one `ps`, a few small files).
pub fn detect() -> Vec<Runtime> {
    if cfg!(windows) {
        return Vec::new();
    }
    let mut rts = crate::cmd::run_named("ps", &["-axo", "comm="], Duration::from_secs(5)).map(|o| parse_ps(&o.stdout)).unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from)
        && let Some(r) = rts.iter_mut().find(|r| r.id == "kiri")
    {
        r.ip = std::fs::read_to_string(home.join(".kiri/vm.json")).ok().and_then(|s| parse_vm_json(&s));
    }
    rts
}

/// "Kiri VM (Docker)", "UTM VM".
pub fn guest_label(r: &Runtime) -> String {
    match r.engine {
        Some(e) => i18n::t!("core.vm.guest_engine", runtime = r.name, engine = e),
        None => i18n::t!("core.vm.guest", runtime = r.name),
    }
}

/// New names for guests on `bridges` (this computer's VM subnets; `host_ips` are its own
/// addresses there): the runtime whose config names the address, else the only runtime
/// running. Devices the user named are left alone.
pub fn label_guests(devices: &[Device], bridges: &[Cidr], host_ips: &[IpAddr], runtimes: &[Runtime]) -> Vec<(DeviceId, String)> {
    let mut out = Vec::new();
    for d in devices.iter().filter(|d| !d.name_manual) {
        let Some(ip) = d.addrs.iter().map(|a| a.ip).find(|ip| ip.is_ipv4() && !host_ips.contains(ip) && bridges.iter().any(|c| c.contains(*ip))) else { continue };
        let r = runtimes.iter().find(|r| r.ip == Some(ip)).or(if runtimes.len() == 1 { runtimes.first() } else { None });
        if let Some(r) = r {
            let name = guest_label(r);
            if d.name != name {
                out.push((d.id, name));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Addr, Layer, Source};

    const PS: &str = "/sbin/launchd
/Applications/Kiri.app/Contents/MacOS/kiri
/opt/orbit/bin/orbit/orbit
/System/Library/Frameworks/Virtualization.framework/Versions/A/XPCServices/com.apple.Virtualization.VirtualMachine.xpc/Contents/MacOS/com.apple.Virtualization.VirtualMachine
";

    fn dev(id: DeviceId, ip: &str) -> Device {
        Device { id, name: ip.into(), addrs: vec![Addr { ip: ip.parse().unwrap(), layer: Layer::Lan, network: None, source: Source::Arp, last_seen: 0 }], ..Default::default() }
    }

    #[test]
    fn runtimes_from_ps() {
        let r = parse_ps(PS);
        assert_eq!(r.iter().map(|r| r.id).collect::<Vec<_>>(), ["kiri"], "orbit is not OrbStack");
        let many = parse_ps("/Applications/OrbStack.app/Contents/MacOS/OrbStack\n/usr/local/bin/limactl\n/Applications/UTM.app/Contents/MacOS/UTM\n/Applications/Docker.app/Contents/MacOS/com.docker.backend\n/usr/lib/virtualbox/VBoxHeadless\n/snap/multipass/current/bin/multipassd\n");
        assert_eq!(many.iter().map(|r| r.id).collect::<Vec<_>>(), ["orbstack", "lima", "utm", "docker", "virtualbox", "multipass"]);
        assert_eq!(parse_vm_json(r#"{"cpus":8,"memory_mib":10240}"#), None);
        assert_eq!(parse_vm_json(r#"{"cpus":8,"network":{"ip_address":"192.168.64.3/24"}}"#), Some("192.168.64.3".parse().unwrap()));
        assert_eq!(parse_vm_json("not json"), None);
    }

    #[test]
    fn labels_guests_on_this_computers_bridge() {
        let bridges = [Cidr::parse("192.168.64.0/24").unwrap()];
        let host = ["192.168.64.1".parse().unwrap()];
        let kiri = parse_ps(PS);
        let devices = vec![dev(1, "192.168.64.1"), dev(2, "192.168.64.3"), dev(3, "192.168.40.22")];
        let l = label_guests(&devices, &bridges, &host, &kiri);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].0, 2);
        assert!(l[0].1.contains("Kiri") && l[0].1.contains("Docker"), "{}", l[0].1);
        // two runtimes: only the one whose config names the address
        let mut two = parse_ps("/Applications/UTM.app/Contents/MacOS/UTM\n/Applications/Kiri.app/Contents/MacOS/kiri\n");
        assert!(label_guests(&devices, &bridges, &host, &two).is_empty());
        two[1].ip = Some("192.168.64.3".parse().unwrap());
        assert_eq!(label_guests(&devices, &bridges, &host, &two).len(), 1);
        // a name the user typed stays
        let mut named = devices.clone();
        named[1].name_manual = true;
        assert!(label_guests(&named, &bridges, &host, &kiri).is_empty());
        assert!(label_guests(&devices, &bridges, &host, &[]).is_empty());
    }
}
