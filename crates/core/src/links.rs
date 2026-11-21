//! Deep links into Nodo (`otherwise://nodo/…` or `otherwise-nodo://…`, see otherwise-link's
//! SPEC), and the links Nodo builds into Vigía.
//!
//! | Path | Opens |
//! |------|-------|
//! | (empty) | a window |
//! | `device/<id>?map=<uid>` | that device (in the map with that uid; else the open one) |
//! | `service/<id>?map=<uid>` | that service in the Status view |
//! | `entity/<uuid>` | the device linked to that shared card |
//!
//! A map's uid lives in the map file (`meta.shared_uid`), so links survive moving the file.
use crate::model::{Device, DeviceId};
use crate::services::{CheckKind, ServiceCheck, ServiceId};
use otherwise_link::Link;
use uuid::Uuid;

pub const APP: &str = crate::sync::APP;
pub const BUNDLE_ID: &str = "io.github.otherwisetech.nodo-app";
pub const SCHEME: &str = "otherwise-nodo";
pub const DESKTOP_FILE: &str = "io.github.otherwisetech.nodo-app.desktop";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Focus,
    Device { id: DeviceId, map: Option<String> },
    Service { id: ServiceId, map: Option<String> },
    Shared(Uuid),
    Unknown,
}

pub fn route(l: &Link) -> Route {
    if l.app != APP {
        return Route::Unknown;
    }
    let id = || l.at(1).and_then(|s| s.parse::<i64>().ok());
    let map = l.get("map").map(str::to_string).filter(|m| !m.is_empty());
    match l.at(0) {
        None => Route::Focus,
        Some("device") => id().map_or(Route::Unknown, |id| Route::Device { id, map }),
        Some("service") => id().map_or(Route::Unknown, |id| Route::Service { id, map }),
        Some("entity") => l.at(1).and_then(|s| Uuid::parse_str(s).ok()).map_or(Route::Unknown, Route::Shared),
        Some(_) => Route::Unknown,
    }
}

fn with_map(l: Link, map: Option<&str>) -> Link {
    match map {
        Some(m) => l.param("map", m),
        None => l,
    }
}

pub fn device_link(id: DeviceId, map: Option<&str>) -> Link {
    with_map(Link::new(APP).segment("device").segment(id.to_string()), map)
}

pub fn service_link(id: ServiceId, map: Option<&str>) -> Link {
    with_map(Link::new(APP).segment("service").segment(id.to_string()), map)
}

/// The host (and port, when not 443) an HTTPS check talks to.
pub fn tls_host(s: &ServiceCheck) -> Option<(String, u16)> {
    if s.spec.kind != CheckKind::Http {
        return None;
    }
    let u = crate::services::parse_url(&s.spec.target)?;
    u.tls.then_some((u.host, u.port))
}

/// The registrable domain of a host (`www.otherwisetech.com` → `otherwisetech.com`,
/// `shop.example.com.co` → `example.com.co`); None for an IP address.
pub fn domain_of(host: &str) -> Option<String> {
    let h = host.trim().trim_end_matches('.').to_lowercase();
    if h.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    let labels: Vec<&str> = h.split('.').filter(|x| !x.is_empty()).collect();
    if labels.len() < 2 {
        return None;
    }
    const SECOND: &[&str] = &["co", "com", "net", "org", "gov", "edu", "ac", "gob"];
    let n = labels.len();
    let keep = if n >= 3 && labels[n - 1].len() == 2 && SECOND.contains(&labels[n - 2]) { 3 } else { 2 };
    Some(labels[n.saturating_sub(keep)..].join("."))
}

/// Vigía's quick add, prefilled to track an HTTPS service's certificate (expiring `expires`,
/// `YYYY-MM-DD`, when known) under its domain.
pub fn vigia_certificate(s: &ServiceCheck, expires: Option<&str>, name: &str) -> Option<Link> {
    let (host, port) = tls_host(s)?;
    let target = if port == 443 { host.clone() } else { format!("{host}:{port}") };
    let mut l = Link::new("vigia").segment("new").param("kind", "certificate").param("name", name).param("host", target).param("check", "1");
    if let Some(d) = domain_of(&host) {
        l = l.param("owner", d).param("owner_kind", "digital");
    }
    if let Some(e) = expires {
        l = l.param("expires", e);
    }
    Some(l)
}

/// Vigía's quick add, prefilled with a device's warranty (name, vendor / model as provider,
/// the owner when known).
pub fn vigia_warranty(d: &Device, name: &str) -> Link {
    let mut l = Link::new("vigia").segment("new").param("kind", "warranty").param("name", name);
    let provider = [d.vendor.as_deref(), d.model.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" ");
    if !provider.trim().is_empty() {
        l = l.param("provider", provider.trim());
    }
    if !d.owner.trim().is_empty() {
        l = l.param("owner", d.owner.trim());
    }
    if let Some(serial) = d.fields.iter().find(|(k, _)| k.eq_ignore_ascii_case("serial")).map(|(_, v)| v.clone()) {
        l = l.param("reference", serial);
    }
    l
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::Spec;

    #[test]
    fn routes() {
        let r = |s: &str| route(&Link::parse(s).unwrap());
        assert_eq!(r("otherwise://nodo"), Route::Focus);
        assert_eq!(r("otherwise-nodo://device/7?map=ab12"), Route::Device { id: 7, map: Some("ab12".into()) });
        assert_eq!(r("otherwise://nodo/service/3"), Route::Service { id: 3, map: None });
        let u = Uuid::new_v4();
        assert_eq!(r(&format!("otherwise://nodo/entity/{u}")), Route::Shared(u));
        assert_eq!(r("otherwise://nodo/device/x"), Route::Unknown);
        assert_eq!(r("otherwise://vigia/device/1"), Route::Unknown);
        assert_eq!(device_link(7, Some("ab12")).to_url(), "otherwise://nodo/device/7?map=ab12");
    }

    #[test]
    fn links_into_vigia() {
        let s = ServiceCheck { id: 1, name: "Site".into(), device: None, spec: Spec::new(CheckKind::Http, "https://www.otherwisetech.com/health"), enabled: true };
        let l = vigia_certificate(&s, Some("2027-01-15"), "SSL www.otherwisetech.com").unwrap();
        assert_eq!(l.to_url(), "otherwise://vigia/new?kind=certificate&name=SSL%20www.otherwisetech.com&host=www.otherwisetech.com&check=1&owner=otherwisetech.com&owner_kind=digital&expires=2027-01-15");
        let plain = ServiceCheck { spec: Spec::new(CheckKind::Http, "http://example.com"), ..s.clone() };
        assert!(vigia_certificate(&plain, None, "x").is_none());
        let port = ServiceCheck { spec: Spec::new(CheckKind::Http, "https://10.0.0.5:8443"), ..s };
        assert_eq!(vigia_certificate(&port, None, "x").unwrap().get("host"), Some("10.0.0.5:8443"));
        assert_eq!(domain_of("shop.example.com.co").as_deref(), Some("example.com.co"));
        assert_eq!(domain_of("192.168.1.1"), None);
        let d = Device { name: "MacBook".into(), vendor: Some("Apple".into()), model: Some("MacBook Pro".into()), owner: "Otherwise SAS".into(), fields: vec![("Serial".into(), "C02X".into())], ..Default::default() };
        let w = vigia_warranty(&d, "MacBook");
        assert_eq!((w.get("kind"), w.get("provider"), w.get("owner"), w.get("reference")), (Some("warranty"), Some("Apple MacBook Pro"), Some("Otherwise SAS"), Some("C02X")));
    }
}
