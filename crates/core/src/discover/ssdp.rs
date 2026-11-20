//! SSDP / UPnP: one `M-SEARCH ssdp:all` to the multicast group, the answers for a few seconds,
//! then each device description (`LOCATION`, plain HTTP on a private address only) for its
//! friendly name, maker and model.
use crate::model::{Layer, Observation, Service, Source};
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

pub const M_SEARCH: &str = "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: ssdp:all\r\n\r\n";

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Response {
    pub from: Option<IpAddr>,
    pub location: Option<String>,
    pub server: Option<String>,
    pub st: Option<String>,
    pub usn: Option<String>,
}

/// Headers of one SSDP answer (`HTTP/1.1 200 OK` or a `NOTIFY`), case-insensitive.
pub fn parse_response(s: &str, from: Option<IpAddr>) -> Option<Response> {
    let mut lines = s.split("\r\n").flat_map(|l| l.split('\n'));
    let first = lines.next()?.trim();
    if !(first.starts_with("HTTP/1.1 200") || first.starts_with("NOTIFY")) {
        return None;
    }
    let mut r = Response { from, ..Default::default() };
    for l in lines {
        let Some((k, v)) = l.split_once(':') else { continue };
        let v = v.trim().to_string();
        match k.trim().to_ascii_uppercase().as_str() {
            "LOCATION" => r.location = Some(v),
            "SERVER" => r.server = Some(v),
            "ST" | "NT" => r.st = Some(v),
            "USN" => r.usn = Some(v),
            _ => {}
        }
    }
    Some(r)
}

/// What a UPnP device description says about the root device.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Description {
    pub friendly_name: Option<String>,
    pub manufacturer: Option<String>,
    pub model_name: Option<String>,
    pub model_number: Option<String>,
    pub device_type: Option<String>,
    pub presentation_url: Option<String>,
    pub udn: Option<String>,
}

/// First `<tag>…</tag>` in `xml`, entities decoded; namespaced tags (`<ns:tag>`) too.
pub fn xml_tag(xml: &str, tag: &str) -> Option<String> {
    let mut from = 0;
    while let Some(i) = xml[from..].find('<') {
        let start = from + i + 1;
        let end = start + xml[start..].find('>')?;
        let name = &xml[start..end];
        let local = name.split(':').next_back().unwrap_or(name);
        if local == tag {
            let close = xml[end + 1..].find("</")? + end + 1;
            let v = xml[end + 1..close].trim();
            return Some(v.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'")).filter(|s| !s.is_empty());
        }
        from = end;
    }
    None
}

pub fn parse_description(xml: &str) -> Description {
    // the root device comes first; embedded devices follow inside <deviceList>
    let root = xml.split("<deviceList").next().unwrap_or(xml);
    Description {
        friendly_name: xml_tag(root, "friendlyName"),
        manufacturer: xml_tag(root, "manufacturer"),
        model_name: xml_tag(root, "modelName"),
        model_number: xml_tag(root, "modelNumber"),
        device_type: xml_tag(root, "deviceType"),
        presentation_url: xml_tag(root, "presentationURL"),
        udn: xml_tag(root, "UDN"),
    }
}

/// One observation per answering address; `descriptions` maps LOCATION → parsed description.
pub fn to_observations(responses: &[Response], descriptions: &BTreeMap<String, Description>) -> Vec<Observation> {
    let mut by_ip: BTreeMap<IpAddr, Observation> = BTreeMap::new();
    for r in responses {
        let Some(ip) = r.from.or_else(|| r.location.as_deref().and_then(host_of)) else { continue };
        let o = by_ip.entry(ip).or_insert_with(|| {
            let mut o = Observation::new(Layer::Lan, Source::Ssdp);
            o.ips.push(ip);
            o
        });
        if let Some(st) = &r.st
            && st.starts_with("urn:")
        {
            let svc = Service { name: st.clone(), port: None, label: None };
            if !o.services.contains(&svc) {
                o.services.push(svc);
            }
        }
        if o.os.is_none()
            && let Some(s) = &r.server
        {
            o_fact(o, "ssdp_server", s);
        }
        if let Some((loc, d)) = r.location.as_ref().and_then(|l| descriptions.get(l).map(|d| (l, d))) {
            if o.name.is_none() {
                o.name = d.friendly_name.clone();
            }
            if o.vendor.is_none() {
                o.vendor = d.manufacturer.clone();
            }
            if o.model.is_none() {
                o.model = match (&d.model_name, &d.model_number) {
                    (Some(n), Some(num)) if !n.contains(num.as_str()) => Some(format!("{n} {num}")),
                    (Some(n), _) => Some(n.clone()),
                    _ => None,
                };
            }
            if let Some(t) = &d.device_type {
                if o.kind.is_none() {
                    o.kind = crate::discover::classify::from_ssdp(t);
                }
                let svc = Service { name: t.clone(), port: None, label: d.friendly_name.clone() };
                if !o.services.contains(&svc) {
                    o.services.push(svc);
                }
            }
            if let Some(u) = &d.presentation_url
                && !o.facts.iter().any(|(k, _)| k == "web_ui")
            {
                let url = if u.starts_with("http") { u.clone() } else { format!("{}{}", base_url(loc), if u.starts_with('/') { u.clone() } else { format!("/{u}") }) };
                o.facts.push(("web_ui".into(), url));
            }
        }
    }
    by_ip.into_values().collect()
}

fn o_fact(o: &mut Observation, k: &str, v: &str) {
    if !o.facts.iter().any(|(a, _)| a == k) {
        o.facts.push((k.to_string(), v.to_string()));
    }
}

/// `http://192.168.1.1:5000/rootDesc.xml` → `http://192.168.1.1:5000`.
pub fn base_url(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => format!("{scheme}://{}", rest.split(['/', '?']).next().unwrap_or_default()),
        None => url.to_string(),
    }
}

/// `http://192.168.1.1:5000/rootDesc.xml` → 192.168.1.1.
pub fn host_of(url: &str) -> Option<IpAddr> {
    let rest = url.split_once("://")?.1;
    let host = rest.split(['/', '?']).next()?;
    let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    host.trim_matches(['[', ']']).parse().ok()
}

/// Send one M-SEARCH and collect answers for `wait`. Blocking.
pub fn search(wait: Duration) -> Result<Vec<Response>, String> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(|e| e.to_string())?;
    sock.set_read_timeout(Some(Duration::from_millis(250))).map_err(|e| e.to_string())?;
    let _ = sock.set_multicast_ttl_v4(2);
    let group = SocketAddr::from((Ipv4Addr::new(239, 255, 255, 250), 1900));
    sock.send_to(M_SEARCH.as_bytes(), group).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + wait;
    let mut buf = [0u8; 4096];
    let mut out = Vec::new();
    while Instant::now() < deadline {
        if let Ok((n, from)) = sock.recv_from(&mut buf)
            && let Some(r) = parse_response(&String::from_utf8_lossy(&buf[..n]), Some(from.ip()))
        {
            out.push(r);
        }
    }
    Ok(out)
}

/// GET each distinct LOCATION on a private address (2 s timeout, 256 KB cap).
pub fn fetch_descriptions(responses: &[Response]) -> BTreeMap<String, Description> {
    let mut urls: Vec<String> = responses.iter().filter_map(|r| r.location.clone()).filter(|l| l.starts_with("http://") && host_of(l).is_some_and(crate::cidr::is_private)).collect();
    urls.sort();
    urls.dedup();
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(2))).max_redirects(0).build().into();
    let mut out = BTreeMap::new();
    for u in urls.into_iter().take(64) {
        if let Ok(mut resp) = agent.get(&u).call()
            && let Ok(body) = resp.body_mut().with_config().limit(256 * 1024).read_to_string()
        {
            out.insert(u, parse_description(&body));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESP: &str = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=120\r\nST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\nUSN: uuid:abcd::urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\nEXT:\r\nSERVER: Linux/5.4 UPnP/1.0 MiniUPnPd/2.2\r\nLOCATION: http://192.168.1.1:5000/rootDesc.xml\r\n\r\n";
    const DESC: &str = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
 <device>
  <deviceType>urn:schemas-upnp-org:device:InternetGatewayDevice:1</deviceType>
  <friendlyName>Home Router &amp; Wi-Fi</friendlyName>
  <manufacturer>ASUSTeK</manufacturer>
  <modelName>RT-AX86U</modelName>
  <modelNumber>3.0.0.4</modelNumber>
  <UDN>uuid:abcd</UDN>
  <presentationURL>/index.html</presentationURL>
  <deviceList><device><deviceType>urn:schemas-upnp-org:device:WANDevice:1</deviceType><friendlyName>WANDevice</friendlyName></device></deviceList>
 </device>
</root>"#;

    #[test]
    fn ssdp_and_description() {
        let r = parse_response(RESP, None).unwrap();
        assert_eq!(r.location.as_deref(), Some("http://192.168.1.1:5000/rootDesc.xml"));
        assert!(r.server.as_deref().unwrap().contains("MiniUPnPd"));
        assert_eq!(parse_response("garbage", None), None);
        let d = parse_description(DESC);
        assert_eq!(d.friendly_name.as_deref(), Some("Home Router & Wi-Fi"));
        assert_eq!(d.model_name.as_deref(), Some("RT-AX86U"));
        assert_eq!(xml_tag("<a:x>1</a:x>", "x").as_deref(), Some("1"));
        let mut descs = BTreeMap::new();
        descs.insert("http://192.168.1.1:5000/rootDesc.xml".to_string(), d);
        let tv = parse_response("NOTIFY * HTTP/1.1\r\nNT: urn:dial-multiscreen-org:service:dial:1\r\nLOCATION: http://192.168.1.60:8008/ssdp/device-desc.xml\r\n\r\n", Some("192.168.1.60".parse().unwrap())).unwrap();
        let o = to_observations(&[r, tv], &descs);
        assert_eq!(o.len(), 2);
        assert_eq!(o[0].ips, vec!["192.168.1.1".parse::<IpAddr>().unwrap()]);
        assert_eq!(o[0].kind, Some(crate::model::DeviceKind::Router));
        assert_eq!(o[0].name.as_deref(), Some("Home Router & Wi-Fi"));
        assert_eq!(o[0].model.as_deref(), Some("RT-AX86U 3.0.0.4"));
        assert!(o[0].facts.contains(&("web_ui".into(), "http://192.168.1.1:5000/index.html".into())));
        assert_eq!(host_of("http://[fe80::1]:80/x"), Some("fe80::1".parse().unwrap()));
    }
}
