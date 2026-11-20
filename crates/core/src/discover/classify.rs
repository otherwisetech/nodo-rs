//! Guessing a device's type from what discovery saw: mDNS service types and TXT model, SSDP
//! device types, open ports, the MAC vendor and the OS name. The user's choice always wins.
use crate::model::{Device, DeviceKind};

/// The kind an mDNS service type suggests (`_ipp._tcp` → printer).
pub fn from_mdns(service: &str) -> Option<DeviceKind> {
    let s = service.trim_end_matches('.').trim_end_matches(".local").to_lowercase();
    let base = s.split('.').next().unwrap_or_default();
    Some(match base {
        "_ipp" | "_ipps" | "_printer" | "_pdl-datastream" | "_scanner" | "_uscan" => DeviceKind::Printer,
        "_googlecast" | "_airplay" | "_amzn-wplay" | "_roku" | "_viziocast" | "_androidtvremote2" => DeviceKind::Tv,
        "_raop" | "_spotify-connect" | "_sonos" | "_daap" => DeviceKind::Speaker,
        "_hap" | "_homekit" | "_homeassistant" | "_hue" | "_matter" | "_matterc" | "_esphomelib" | "_shelly" | "_nanoleafapi" => DeviceKind::Iot,
        "_afpovertcp" | "_adisk" | "_nfs" => DeviceKind::Nas,
        "_rtsp" | "_onvif" | "_axis-video" => DeviceKind::Camera,
        "_apple-mobdev2" => DeviceKind::Phone,
        _ => return None,
    })
}

/// `_device-info` TXT `model=MacBookPro18,3` and similar.
pub fn from_model(model: &str) -> Option<DeviceKind> {
    let m = model.to_lowercase();
    Some(if m.starts_with("macbook") {
        DeviceKind::Laptop
    } else if m.starts_with("imac") || m.starts_with("macmini") || m.starts_with("macpro") || m.starts_with("mac13") || m.starts_with("mac14") || m.starts_with("mac15") || m.starts_with("mac16") {
        DeviceKind::Desktop
    } else if m.starts_with("iphone") {
        DeviceKind::Phone
    } else if m.starts_with("ipad") {
        DeviceKind::Tablet
    } else if m.starts_with("appletv") {
        DeviceKind::Tv
    } else if m.starts_with("audioaccessory") || m.starts_with("homepod") {
        DeviceKind::Speaker
    } else if m.starts_with("timecapsule") || m.contains("diskstation") || m.contains("readynas") {
        DeviceKind::Nas
    } else if m.starts_with("airport") {
        DeviceKind::AccessPoint
    } else {
        return None;
    })
}

/// UPnP `urn:schemas-upnp-org:device:InternetGatewayDevice:1` → router.
pub fn from_ssdp(device_type: &str) -> Option<DeviceKind> {
    let t = device_type.to_lowercase();
    Some(if t.contains("internetgatewaydevice") || t.contains("wanconnectiondevice") {
        DeviceKind::Router
    } else if t.contains("wlanaccesspoint") {
        DeviceKind::AccessPoint
    } else if t.contains("mediarenderer") || t.contains("dial") || t.contains("tvdevice") {
        DeviceKind::Tv
    } else if t.contains("mediaserver") || t.contains("nas") {
        DeviceKind::Nas
    } else if t.contains("printer") {
        DeviceKind::Printer
    } else if t.contains("zoneplayer") || t.contains("speaker") {
        DeviceKind::Speaker
    } else if t.contains("camera") || t.contains("networkvideotransmitter") {
        DeviceKind::Camera
    } else {
        return None;
    })
}

/// From a set of open TCP ports (the active probe set).
pub fn from_ports(ports: &[u16]) -> Option<DeviceKind> {
    let has = |p: u16| ports.contains(&p);
    Some(if has(9100) || has(631) || has(515) {
        DeviceKind::Printer
    } else if has(554) && !has(445) {
        DeviceKind::Camera
    } else if has(3389) || (has(445) && has(135)) {
        DeviceKind::Desktop
    } else if has(445) && (has(80) || has(443) || has(5000) || has(5001)) {
        DeviceKind::Nas
    } else if has(5900) && has(22) {
        DeviceKind::Desktop
    } else if has(22) && !has(80) && !has(443) {
        DeviceKind::Server
    } else {
        return None;
    })
}

/// Rough kind by vendor for devices that only answered ARP.
pub fn from_vendor(vendor: &str) -> Option<DeviceKind> {
    let v = vendor.to_lowercase();
    const ROUTERS: &[&str] = &["ubiquiti", "mikrotik", "routerboard", "netgear", "tp-link", "asustek", "arris", "technicolor", "sagemcom", "huawei", "zte", "fritz", "avm audiovisuelles", "eero", "linksys", "cisco", "juniper", "fortinet", "draytek", "zyxel"];
    const IOT: &[&str] = &["espressif", "tuya", "shenzhen", "signify", "philips lighting", "ecobee", "nest", "ring", "wyze", "lifx", "shelly", "sonoff", "itead", "xiaomi", "ikea", "amazon technologies"];
    const NAS: &[&str] = &["synology", "qnap", "western digital", "buffalo", "asustor"];
    const PRINTERS: &[&str] = &["brother", "canon", "epson", "seiko epson", "lexmark", "kyocera", "xerox", "ricoh"];
    const CAMERAS: &[&str] = &["hikvision", "dahua", "axis communications", "reolink", "amcrest", "hangzhou"];
    const TVS: &[&str] = &["roku", "lg electronics", "vizio", "tcl", "hisense", "sony"];
    const SPEAKERS: &[&str] = &["sonos", "bose"];
    const PI: &[&str] = &["raspberry pi"];
    let any = |l: &[&str]| l.iter().any(|x| v.contains(x));
    Some(if any(NAS) {
        DeviceKind::Nas
    } else if any(PRINTERS) {
        DeviceKind::Printer
    } else if any(CAMERAS) {
        DeviceKind::Camera
    } else if any(SPEAKERS) {
        DeviceKind::Speaker
    } else if any(TVS) {
        DeviceKind::Tv
    } else if any(IOT) {
        DeviceKind::Iot
    } else if any(PI) {
        DeviceKind::Server
    } else if any(ROUTERS) {
        DeviceKind::Router
    } else if v.contains("vmware") || v.contains("parallels") || v.contains("xensource") || v.contains("qemu") {
        DeviceKind::Vm
    } else {
        return None;
    })
}

/// Tailscale's `OS` field.
pub fn from_os(os: &str) -> Option<DeviceKind> {
    Some(match os.to_lowercase().as_str() {
        "ios" | "android" => DeviceKind::Phone,
        "macos" | "windows" => DeviceKind::Laptop,
        "linux" | "freebsd" | "openbsd" | "illumos" => DeviceKind::Server,
        "tvos" => DeviceKind::Tv,
        _ => return None,
    })
}

/// Best guess for a device from everything it has. Order: mDNS model, mDNS/SSDP services,
/// gateway, ports, OS, vendor.
pub fn guess(d: &Device, is_gateway: bool) -> DeviceKind {
    if let Some(k) = d.model.as_deref().and_then(from_model) {
        return k;
    }
    let mut svc: Vec<DeviceKind> = d.services.iter().filter_map(|s| from_mdns(&s.name).or_else(|| from_ssdp(&s.name))).collect();
    // a printer that also does AirPlay is a printer; a TV that does Spotify is a TV
    svc.sort_by_key(|k| match k {
        DeviceKind::Router => 0,
        DeviceKind::Printer => 1,
        DeviceKind::Camera => 2,
        DeviceKind::Nas => 3,
        DeviceKind::Tv => 4,
        DeviceKind::Speaker => 5,
        _ => 6,
    });
    if is_gateway {
        return DeviceKind::Router;
    }
    let has = |names: &[&str]| d.services.iter().any(|s| names.iter().any(|n| s.name.starts_with(n)));
    // Macs and PCs announce AirPlay / screen sharing / file sharing too: a computer, not a TV
    let computer = has(&["_rfb.", "_smb.", "_workstation.", "_companion-link.", "_rdp.", "_sftp-ssh."]);
    match svc.first() {
        Some(DeviceKind::Tv | DeviceKind::Speaker) if computer => return DeviceKind::Desktop,
        Some(k) => return *k,
        None if computer => return DeviceKind::Desktop,
        None => {}
    }
    if has(&["_ssh."]) {
        return DeviceKind::Server;
    }
    let ports: Vec<u16> = d.services.iter().filter(|s| s.name.starts_with("tcp/")).filter_map(|s| s.port).collect();
    if let Some(k) = from_ports(&ports) {
        return k;
    }
    if d.ident(crate::model::IdentKind::AwsInstance).is_some() {
        return DeviceKind::Vm;
    }
    if let Some(k) = d.os.as_deref().and_then(from_os) {
        return k;
    }
    if let Some(k) = d.vendor.as_deref().and_then(from_vendor) {
        return k;
    }
    DeviceKind::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Service;

    #[test]
    fn kinds() {
        assert_eq!(from_mdns("_ipp._tcp.local."), Some(DeviceKind::Printer));
        assert_eq!(from_mdns("_googlecast._tcp"), Some(DeviceKind::Tv));
        assert_eq!(from_mdns("_ssh._tcp.local."), None);
        assert_eq!(from_model("MacBookPro18,3"), Some(DeviceKind::Laptop));
        assert_eq!(from_model("iPhone15,2"), Some(DeviceKind::Phone));
        assert_eq!(from_ssdp("urn:schemas-upnp-org:device:InternetGatewayDevice:1"), Some(DeviceKind::Router));
        assert_eq!(from_ports(&[22]), Some(DeviceKind::Server));
        assert_eq!(from_ports(&[80, 443, 445]), Some(DeviceKind::Nas));
        assert_eq!(from_ports(&[9100, 80]), Some(DeviceKind::Printer));
        assert_eq!(from_ports(&[554, 80]), Some(DeviceKind::Camera));
        assert_eq!(from_vendor("Synology Incorporated"), Some(DeviceKind::Nas));
        assert_eq!(from_vendor("Espressif Inc."), Some(DeviceKind::Iot));
        assert_eq!(from_os("iOS"), Some(DeviceKind::Phone));
        let mut d = Device { services: vec![Service { name: "_airplay._tcp".into(), port: Some(7000), label: None }, Service { name: "_ipp._tcp".into(), port: Some(631), label: None }], ..Default::default() };
        assert_eq!(guess(&d, false), DeviceKind::Printer);
        assert_eq!(guess(&d, true), DeviceKind::Router);
        d.model = Some("AppleTV11,1".into());
        assert_eq!(guess(&d, false), DeviceKind::Tv);
        let svc = |n: &str| Service { name: n.into(), port: Some(1), label: None };
        let mac = Device { services: vec![svc("_airplay._tcp"), svc("_ssh._tcp"), svc("_rfb._tcp")], ..Default::default() };
        assert_eq!(guess(&mac, false), DeviceKind::Desktop, "a Mac doing AirPlay is not a TV");
        assert_eq!(guess(&Device { services: vec![svc("_ssh._tcp")], ..Default::default() }, false), DeviceKind::Server);
        assert_eq!(guess(&Device::default(), false), DeviceKind::Unknown);
    }
}
