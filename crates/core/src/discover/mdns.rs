//! mDNS / Bonjour: browse a fixed list of service types for a few seconds and turn each
//! resolved instance into an observation (host name, addresses, services, TXT model).
use crate::model::{IdentKind, Layer, Observation, Service, Source};
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Service types browsed. `_device-info` carries the Apple model in TXT.
pub const SERVICE_TYPES: &[&str] = &[
    "_http._tcp.local.",
    "_https._tcp.local.",
    "_ssh._tcp.local.",
    "_sftp-ssh._tcp.local.",
    "_smb._tcp.local.",
    "_afpovertcp._tcp.local.",
    "_adisk._tcp.local.",
    "_ipp._tcp.local.",
    "_ipps._tcp.local.",
    "_printer._tcp.local.",
    "_pdl-datastream._tcp.local.",
    "_scanner._tcp.local.",
    "_airplay._tcp.local.",
    "_raop._tcp.local.",
    "_googlecast._tcp.local.",
    "_spotify-connect._tcp.local.",
    "_sonos._tcp.local.",
    "_hap._tcp.local.",
    "_homeassistant._tcp.local.",
    "_hue._tcp.local.",
    "_matter._tcp.local.",
    "_esphomelib._tcp.local.",
    "_rfb._tcp.local.",
    "_rdp._tcp.local.",
    "_workstation._tcp.local.",
    "_device-info._tcp.local.",
    "_companion-link._tcp.local.",
    "_apple-mobdev2._tcp.local.",
    "_rtsp._tcp.local.",
    "_amzn-wplay._tcp.local.",
];

/// What we keep of one resolved mDNS instance (the crate's struct, minus its internals).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Record {
    /// `_ipp._tcp.local.`
    pub service_type: String,
    /// `Office Printer._ipp._tcp.local.`
    pub fullname: String,
    /// `office-printer.local.`
    pub host: String,
    pub port: u16,
    pub addresses: Vec<IpAddr>,
    pub txt: Vec<(String, String)>,
}

impl Record {
    fn txt(&self, k: &str) -> Option<&str> {
        self.txt.iter().find(|(a, _)| a.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str()).filter(|v| !v.is_empty())
    }

    /// `Office Printer._ipp._tcp.local.` → `Office Printer` (with `\.` / `\\` unescaped).
    pub fn instance(&self) -> String {
        let suffix = format!(".{}", self.service_type);
        let raw = self.fullname.strip_suffix(&suffix).unwrap_or(&self.fullname);
        raw.replace("\\.", ".").replace("\\\\", "\\")
    }
}

/// One observation per host: every instance on the same `.local` host joins it.
pub fn to_observations(records: &[Record]) -> Vec<Observation> {
    let mut by_host: BTreeMap<String, Observation> = BTreeMap::new();
    for r in records {
        let host = r.host.trim_end_matches('.').to_lowercase();
        if host.is_empty() {
            continue;
        }
        let o = by_host.entry(host.clone()).or_insert_with(|| {
            let mut o = Observation::new(Layer::Lan, Source::Mdns).ident(IdentKind::Mdns, &host).ident(IdentKind::Host, &host);
            o.hostname = Some(host.clone());
            o
        });
        for ip in &r.addresses {
            if !o.ips.contains(ip) && !ip.is_loopback() {
                o.ips.push(*ip);
            }
        }
        let ty = r.service_type.trim_end_matches('.').trim_end_matches(".local").to_string();
        if ty == "_device-info._tcp" {
            if let Some(m) = r.txt("model") {
                o.model = Some(m.to_string());
            }
        } else {
            let svc = Service { name: ty.clone(), port: Some(r.port), label: Some(r.instance()) };
            if !o.services.contains(&svc) {
                o.services.push(svc);
            }
            // a friendly instance name beats the host name (printers, TVs, speakers)
            let friendly = matches!(ty.as_str(), "_ipp._tcp" | "_ipps._tcp" | "_printer._tcp" | "_googlecast._tcp" | "_airplay._tcp" | "_hap._tcp" | "_sonos._tcp" | "_smb._tcp" | "_companion-link._tcp" | "_homeassistant._tcp");
            if friendly && o.name.is_none() {
                o.name = Some(r.txt("fn").map(str::to_string).unwrap_or_else(|| r.instance()));
            }
        }
        if o.model.is_none()
            && let Some(m) = r.txt("md").or_else(|| r.txt("model")).or_else(|| r.txt("ty"))
        {
            o.model = Some(m.to_string());
        }
        if o.vendor.is_none()
            && let Some(m) = r.txt("manufacturer").or_else(|| r.txt("usb_MFG"))
        {
            o.vendor = Some(m.to_string());
        }
        if o.kind.is_none() {
            o.kind = crate::discover::classify::from_mdns(&ty);
        }
    }
    by_host.into_values().filter(|o| !o.ips.is_empty()).collect()
}

/// Browse for `wait`, then stop. Blocking; run it off the UI thread.
pub fn browse(wait: Duration) -> Result<Vec<Record>, String> {
    let daemon = mdns_sd::ServiceDaemon::new().map_err(|e| e.to_string())?;
    let receivers: Vec<_> = SERVICE_TYPES.iter().filter_map(|t| daemon.browse(t).ok()).collect();
    let deadline = Instant::now() + wait;
    let mut out = Vec::new();
    while Instant::now() < deadline {
        let mut any = false;
        for rx in &receivers {
            while let Ok(ev) = rx.try_recv() {
                any = true;
                if let mdns_sd::ServiceEvent::ServiceResolved(s) = ev {
                    out.push(Record {
                        service_type: s.ty_domain.clone(),
                        fullname: s.fullname.clone(),
                        host: s.host.clone(),
                        port: s.port,
                        addresses: s.addresses.iter().map(|a| a.to_ip_addr()).collect(),
                        txt: s.txt_properties.iter().map(|p| (p.key().to_string(), p.val_str().to_string())).collect(),
                    });
                }
            }
        }
        if !any {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    for t in SERVICE_TYPES {
        let _ = daemon.stop_browse(t);
    }
    let _ = daemon.shutdown();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(ty: &str, full: &str, host: &str, port: u16, ip: &str, txt: &[(&str, &str)]) -> Record {
        Record { service_type: ty.into(), fullname: full.into(), host: host.into(), port, addresses: vec![ip.parse().unwrap()], txt: txt.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect() }
    }

    #[test]
    fn records_to_devices() {
        let r = vec![
            rec("_ipp._tcp.local.", "Office Printer._ipp._tcp.local.", "BRW1234.local.", 631, "192.168.1.30", &[("ty", "Brother HL-L2350DW"), ("usb_MFG", "Brother")]),
            rec("_http._tcp.local.", "BRW1234._http._tcp.local.", "BRW1234.local.", 80, "192.168.1.30", &[]),
            rec("_device-info._tcp.local.", "Juan\\'s MacBook._device-info._tcp.local.", "juans-macbook.local.", 0, "192.168.1.23", &[("model", "MacBookPro18,3")]),
            rec("_ssh._tcp.local.", "juans-macbook._ssh._tcp.local.", "juans-macbook.local.", 22, "fe80::1", &[]),
            rec("_googlecast._tcp.local.", "Chromecast-abc._googlecast._tcp.local.", "abc.local.", 8009, "192.168.1.60", &[("fn", "Living Room TV"), ("md", "Chromecast")]),
            rec("_hap._tcp.local.", "Lamp\\.1._hap._tcp.local.", "", 80, "192.168.1.70", &[]),
        ];
        let o = to_observations(&r);
        assert_eq!(o.len(), 3, "the record without a host is dropped; one per host");
        let pr = o.iter().find(|o| o.hostname.as_deref() == Some("brw1234.local")).unwrap();
        assert_eq!(pr.name.as_deref(), Some("Office Printer"));
        assert_eq!(pr.kind, Some(crate::model::DeviceKind::Printer));
        assert_eq!(pr.model.as_deref(), Some("Brother HL-L2350DW"));
        assert_eq!(pr.vendor.as_deref(), Some("Brother"));
        assert_eq!(pr.services.len(), 2);
        assert!(pr.idents.iter().any(|i| i.kind == IdentKind::Mdns && i.value == "brw1234.local"));
        let mac = o.iter().find(|o| o.hostname.as_deref() == Some("juans-macbook.local")).unwrap();
        assert_eq!(mac.model.as_deref(), Some("MacBookPro18,3"));
        assert_eq!(mac.ips.len(), 2);
        let tv = o.iter().find(|o| o.hostname.as_deref() == Some("abc.local")).unwrap();
        assert_eq!(tv.name.as_deref(), Some("Living Room TV"));
        assert_eq!(rec("_hap._tcp.local.", "Lamp\\.1._hap._tcp.local.", "x", 0, "1.1.1.1", &[]).instance(), "Lamp.1");
    }
}
