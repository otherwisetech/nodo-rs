//! Inventory and diagram exports (CSV, JSON, Mermaid) and the CSV import.
use crate::db::MapData;
use crate::graph::{Graph, NodeRef};
use crate::model::*;

pub const CSV_COLUMNS: &[&str] = &["name", "type", "ips", "macs", "vendor", "hostname", "os", "model", "owner", "location", "tags", "notes", "saved", "layers", "first_seen", "last_seen", "links"];

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) || s.starts_with(' ') || s.ends_with(' ') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

pub fn to_csv(map: &MapData) -> String {
    let mut out = CSV_COLUMNS.join(",");
    out.push('\n');
    for d in &map.devices {
        let ips: Vec<String> = d.ips().map(|i| i.to_string()).collect();
        let macs: Vec<&str> = d.macs().collect();
        let layers: Vec<&str> = d.layers().iter().map(|l| l.id()).collect();
        let links: Vec<String> = d.links.iter().map(|l| if l.label.is_empty() { l.url.clone() } else { format!("{} {}", l.label, l.url) }).collect();
        let row = [
            d.name.clone(),
            d.kind.id().to_string(),
            ips.join(" "),
            macs.join(" "),
            d.vendor.clone().unwrap_or_default(),
            d.hostname.clone().unwrap_or_default(),
            d.os.clone().unwrap_or_default(),
            d.model.clone().unwrap_or_default(),
            d.owner.clone(),
            d.location.clone(),
            d.tags.join(";"),
            d.notes.clone(),
            if d.saved { "yes".into() } else { "no".into() }, // i18n-ok: data value
            layers.join(" "),
            if d.first_seen > 0 { crate::time::format_rfc3339(d.first_seen) } else { String::new() },
            if d.last_seen > 0 { crate::time::format_rfc3339(d.last_seen) } else { String::new() },
            links.join(" | "),
        ];
        out.push_str(&row.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    out
}

/// RFC 4180 rows (quoted fields, doubled quotes, newlines inside quotes).
pub fn parse_csv(s: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut f = String::new();
    let mut q = false;
    let mut chars = s.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(c) = chars.next() {
        match (c, q) {
            ('"', true) if chars.peek() == Some(&'"') => {
                f.push('"');
                chars.next();
            }
            ('"', true) => q = false,
            ('"', false) if f.is_empty() => q = true,
            (',', false) => row.push(std::mem::take(&mut f)),
            ('\r', false) => {}
            ('\n', false) => {
                row.push(std::mem::take(&mut f));
                if !(row.len() == 1 && row[0].is_empty()) {
                    rows.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
            }
            (c, _) => f.push(c),
        }
    }
    if !f.is_empty() || !row.is_empty() {
        row.push(f);
        rows.push(row);
    }
    rows
}

/// Devices from a CSV with a header row (any of [`CSV_COLUMNS`], case-insensitive, in any
/// order; `ip` / `mac` singular accepted). Imported devices are saved. Unknown columns become
/// custom fields.
pub fn import_csv(s: &str) -> Result<Vec<Observation>, String> {
    let rows = parse_csv(s);
    let Some((head, body)) = rows.split_first() else { return Err(i18n::t!("core.import.empty")) };
    let cols: Vec<String> = head.iter().map(|h| h.trim().to_lowercase()).collect();
    let col = |names: &[&str]| cols.iter().position(|c| names.contains(&c.as_str()));
    let (name, kind, ips, macs) = (col(&["name", "device", "hostname"]), col(&["type", "kind"]), col(&["ips", "ip", "address", "ip address"]), col(&["macs", "mac", "mac address"]));
    if name.is_none() && ips.is_none() && macs.is_none() {
        return Err(i18n::t!("core.import.no_columns"));
    }
    let known = ["name", "device", "type", "kind", "ips", "ip", "address", "ip address", "macs", "mac", "mac address", "vendor", "hostname", "os", "model", "owner", "location", "tags", "notes", "saved", "layers", "first_seen", "last_seen", "links"];
    let mut out = Vec::new();
    for r in body {
        let get = |i: Option<usize>| i.and_then(|i| r.get(i)).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let mut o = Observation::new(Layer::Lan, Source::Import);
        for ip in get(ips).unwrap_or_default().split([' ', ',', ';']).filter(|x| !x.is_empty()) {
            if let Ok(ip) = ip.parse() {
                o.ips.push(ip);
            }
        }
        for m in get(macs).unwrap_or_default().split([' ', ',', ';']).filter(|x| !x.is_empty()) {
            if let Some(m) = crate::mac::normalize(m) {
                o = o.ident(IdentKind::Mac, &m);
            }
        }
        o.name = get(name);
        o.kind = get(kind).map(|k| DeviceKind::from_id(&k.to_lowercase()));
        o.vendor = get(col(&["vendor"]));
        o.hostname = get(col(&["hostname"])).filter(|_| name != col(&["hostname"]));
        o.os = get(col(&["os"]));
        o.model = get(col(&["model"]));
        if o.ips.is_empty() && o.idents.is_empty() && o.name.is_none() {
            continue;
        }
        // inventory columns travel as facts with an `inv:` prefix; Doc::import applies them
        for (key, names) in [("owner", &["owner"][..]), ("location", &["location"]), ("tags", &["tags"]), ("notes", &["notes"]), ("links", &["links"])] {
            if let Some(v) = get(col(names)) {
                o.facts.push((format!("inv:{key}"), v));
            }
        }
        for (i, c) in cols.iter().enumerate() {
            if !known.contains(&c.as_str())
                && let Some(v) = r.get(i).map(|v| v.trim()).filter(|v| !v.is_empty())
            {
                o.facts.push((format!("field:{}", head[i].trim()), v.to_string()));
            }
        }
        out.push(o);
    }
    Ok(out)
}

pub fn to_json(map: &MapData) -> String {
    let v = serde_json::json!({
        "format": "nodo-map",
        "version": 1,
        "exported": crate::time::format_rfc3339(crate::time::now()),
        "networks": map.networks,
        "devices": map.devices,
    });
    serde_json::to_string_pretty(&v).unwrap_or_default()
}

fn mermaid_id(r: NodeRef) -> String {
    match r {
        NodeRef::Root => "root".into(),
        NodeRef::Network(n) => format!("n{n}"),
        NodeRef::Device(d) => format!("d{d}"),
    }
}

fn mermaid_label(s: &str) -> String {
    s.replace('"', "'").replace(['[', ']', '(', ')', '{', '}', '<', '>', '|'], " ")
}

/// `flowchart LR` with a node per device and network, hub edges solid, overlay edges dotted.
pub fn to_mermaid(map: &MapData, g: &Graph) -> String {
    let mut out = String::from("flowchart LR\n");
    let label = |r: NodeRef| -> String {
        match r {
            NodeRef::Root => i18n::t!("core.graph.root"),
            NodeRef::Network(n) => map.network(n).map(|n| match &n.cidr {
                Some(c) if *c != n.name => format!("{} {c}", n.name),
                _ => n.name.clone(),
            }).unwrap_or_default(),
            NodeRef::Device(d) => map.device(d).map(|d| match d.primary_ip() {
                Some(ip) => format!("{}<br/>{ip}", mermaid_label(&d.name)),
                None => mermaid_label(&d.name),
            }).unwrap_or_default(),
        }
    };
    for n in &g.nodes {
        let id = mermaid_id(n.node);
        let text = label(n.node);
        let text = if matches!(n.node, NodeRef::Device(_)) { text } else { mermaid_label(&text) };
        let shape = match n.node {
            NodeRef::Network(_) => format!("{id}{{{{\"{text}\"}}}}"),
            NodeRef::Root => format!("{id}((\"{text}\"))"),
            NodeRef::Device(_) => format!("{id}[\"{text}\"]"),
        };
        out.push_str(&format!("  {shape}\n"));
    }
    for n in &g.nodes {
        if let Some(p) = n.parent {
            out.push_str(&format!("  {} --- {}\n", mermaid_id(g.nodes[p].node), mermaid_id(n.node)));
        }
    }
    for (d, h, _) in &g.overlay {
        out.push_str(&format!("  {} -.- {}\n", mermaid_id(g.nodes[*d].node), mermaid_id(g.nodes[*h].node)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_roundtrip_and_import() {
        let mut m = crate::graph::tests::sample();
        m.devices[1].notes = "Juan's, \"work\" laptop\nsecond line".into();
        m.devices[1].tags = vec!["work".into(), "mac".into()];
        m.devices[1].idents.push(Ident::new(IdentKind::Mac, "3c:22:fb:00:00:01"));
        let csv = to_csv(&m);
        let rows = parse_csv(&csv);
        assert_eq!(rows.len(), 1 + m.devices.len());
        assert_eq!(rows[0], CSV_COLUMNS);
        assert_eq!(rows[2][0], "laptop");
        assert_eq!(rows[2][2], "192.168.1.20 100.64.0.2");
        assert_eq!(rows[2][11], "Juan's, \"work\" laptop\nsecond line");
        let obs = import_csv(&csv).unwrap();
        assert_eq!(obs.len(), m.devices.len());
        assert_eq!(obs[1].ips.len(), 2);
        assert_eq!(obs[1].idents, vec![Ident::new(IdentKind::Mac, "3c:22:fb:00:00:01")]);
        assert!(obs[1].facts.contains(&("inv:tags".into(), "work;mac".into())));
        // a hand-made sheet: other column names, extra columns become custom fields
        let sheet = "\u{feff}Device,IP,MAC,Rack\r\nCore switch,10.0.0.2,00-11-22-33-44-55,A1\r\n,,,\r\nAP lobby,10.0.0.3,,B4\r\n";
        let obs = import_csv(sheet).unwrap();
        assert_eq!(obs.len(), 2);
        assert_eq!(obs[0].name.as_deref(), Some("Core switch"));
        assert_eq!(obs[0].idents[0].value, "00:11:22:33:44:55");
        assert!(obs[1].facts.contains(&("field:Rack".into(), "B4".into())));
        assert!(import_csv("foo,bar\n1,2\n").is_err());
        assert!(import_csv("").is_err());
    }

    #[test]
    fn mermaid_and_json() {
        let m = crate::graph::tests::sample();
        let g = crate::graph::build(&m, &crate::graph::GraphOpts::default());
        let s = to_mermaid(&m, &g);
        assert!(s.starts_with("flowchart LR\n"));
        assert!(s.contains("d1[\"router<br/>192.168.1.1\"]"));
        assert!(s.contains("n2{{\"tailnet 100.64.0.0/10\"}}"));
        assert!(s.contains("d1 --- d2"));
        assert!(s.contains("d2 -.- n2"), "the laptop's Tailscale overlay");
        let j: serde_json::Value = serde_json::from_str(&to_json(&m)).unwrap();
        assert_eq!(j["format"], "nodo-map");
        assert_eq!(j["devices"].as_array().unwrap().len(), 5);
        assert_eq!(j["devices"][1]["addrs"][1]["layer"], "tailscale");
    }
}
