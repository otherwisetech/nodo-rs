//! The graph as a static picture for export: a [`Scene`] (positions, labels, colours) drawn as
//! SVG (also the source of the app's PNG) or as a one-page vector PDF, with no dependencies.
use crate::db::MapData;
use crate::graph::{Graph, NodeRef};
use crate::layout::{NODE_W, Placed};
use crate::model::{DeviceId, DeviceKind, Layer};

pub type Rgb = (u8, u8, u8);

/// Edge / accent colour of each layer (the app uses the same hues).
pub fn layer_color(l: Layer) -> Rgb {
    match l {
        Layer::Lan => (0x2F, 0x9B, 0xEA),
        Layer::Tailscale => (0x8E, 0x5B, 0xE0),
        Layer::Aws => (0xF0, 0x9A, 0x20),
        Layer::WireGuard => (0xE5, 0x48, 0x4D),
        Layer::Internet => (0x5B, 0x8A, 0x72),
    }
}

/// Two or three letters for a device type, drawn inside its circle.
pub fn kind_glyph(k: DeviceKind) -> &'static str {
    match k {
        DeviceKind::Unknown => "?",
        DeviceKind::Router => "RT",
        DeviceKind::Switch => "SW",
        DeviceKind::AccessPoint => "AP",
        DeviceKind::Server => "SV",
        DeviceKind::Nas => "NAS",
        DeviceKind::Camera => "CAM",
        DeviceKind::Printer => "PR",
        DeviceKind::Tv => "TV",
        DeviceKind::Speaker => "SPK",
        DeviceKind::Phone => "PH",
        DeviceKind::Tablet => "TAB",
        DeviceKind::Laptop => "LT",
        DeviceKind::Desktop => "PC",
        DeviceKind::Iot => "IoT",
        DeviceKind::Vm => "VM",
        DeviceKind::Vpn => "VPN",
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneNode {
    pub x: f32,
    pub y: f32,
    pub label: String,
    pub sub: String,
    pub glyph: String,
    pub color: Rgb,
    pub dim: bool,
    pub hub: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneEdge {
    pub a: (f32, f32),
    pub b: (f32, f32),
    pub color: Rgb,
    pub dashed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneFrame {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub label: String,
    pub color: Rgb,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scene {
    pub title: String,
    pub nodes: Vec<SceneNode>,
    pub edges: Vec<SceneEdge>,
    pub frames: Vec<SceneFrame>,
}

const NEUTRAL: Rgb = (0x8A, 0x86, 0x7E);
const R: f32 = 22.;

/// Build the scene. `online(id)` says whether a device is up (None = unknown).
pub fn scene(map: &MapData, g: &Graph, p: &Placed, title: &str, online: &dyn Fn(DeviceId) -> Option<bool>) -> Scene {
    let mut s = Scene { title: title.to_string(), ..Default::default() };
    for f in &p.frames {
        let label = match (f.hub.map(|h| g.nodes[h].node), f.layer) {
            (Some(NodeRef::Network(n)), _) => map.network(n).map(|n| n.name.clone()).unwrap_or_default(),
            (Some(NodeRef::Root), _) | (Some(NodeRef::Device(_)), _) => i18n::t!("core.layer.lan"),
            (None, Some(l)) => layer_name(l),
            _ => String::new(),
        };
        s.frames.push(SceneFrame { x: f.x, y: f.y, w: f.w, h: f.h, label, color: f.layer.map(layer_color).unwrap_or(NEUTRAL) });
    }
    for (i, n) in g.nodes.iter().enumerate() {
        if let Some(par) = n.parent {
            let hub_link = !matches!(n.node, NodeRef::Device(_)) || g.nodes.iter().any(|c| c.parent == Some(i));
            if p.leaf_edges || hub_link {
                s.edges.push(SceneEdge { a: p.pos[par], b: p.pos[i], color: n.layer.map(layer_color).unwrap_or(NEUTRAL), dashed: n.layer == Some(Layer::Internet) && matches!(n.node, NodeRef::Network(_)) });
            }
        }
    }
    for (d, h, l) in &g.overlay {
        s.edges.push(SceneEdge { a: p.pos[*d], b: p.pos[*h], color: layer_color(*l), dashed: true });
    }
    for (i, n) in g.nodes.iter().enumerate() {
        let (x, y) = p.pos[i];
        let node = match n.node {
            NodeRef::Root => SceneNode { x, y, label: i18n::t!("core.graph.root"), sub: String::new(), glyph: "◎".into(), color: NEUTRAL, dim: false, hub: true }, // i18n-ok: symbol
            NodeRef::Network(id) => {
                let net = map.network(id);
                SceneNode { x, y, label: net.map(|n| n.name.clone()).unwrap_or_default(), sub: net.and_then(|n| n.cidr.clone()).filter(|c| net.is_some_and(|n| n.name != *c)).unwrap_or_default(), glyph: net.map(|n| layer_glyph(n.layer)).unwrap_or("").into(), color: n.layer.map(layer_color).unwrap_or(NEUTRAL), dim: false, hub: true }
            }
            NodeRef::Device(id) => {
                let Some(d) = map.device(id) else { continue };
                let up = online(id).or(d.online);
                SceneNode { x, y, label: d.name.clone(), sub: d.primary_ip().map(|ip| ip.to_string()).unwrap_or_default(), glyph: kind_glyph(d.kind).into(), color: n.layer.map(layer_color).unwrap_or(NEUTRAL), dim: up == Some(false) || up.is_none() && d.saved, hub: false }
            }
        };
        s.nodes.push(node);
    }
    s
}

pub fn layer_name(l: Layer) -> String {
    match l {
        Layer::Lan => i18n::t!("core.layer.lan"),
        Layer::Tailscale => i18n::t!("core.layer.tailscale"),
        Layer::Aws => i18n::t!("core.layer.aws"),
        Layer::WireGuard => i18n::t!("core.layer.wireguard"),
        Layer::Internet => i18n::t!("core.layer.internet"),
    }
}

fn layer_glyph(l: Layer) -> &'static str {
    match l {
        Layer::Lan => "LAN",
        Layer::Tailscale => "TS",
        Layer::Aws => "AWS",
        Layer::WireGuard => "WG",
        Layer::Internet => "NET",
    }
}

fn bounds(s: &Scene) -> (f32, f32, f32, f32) {
    let mut b = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for n in &s.nodes {
        b = (b.0.min(n.x - NODE_W / 2.), b.1.min(n.y - R - 4.), b.2.max(n.x + NODE_W / 2.), b.3.max(n.y + R + 40.));
    }
    for f in &s.frames {
        b = (b.0.min(f.x), b.1.min(f.y), b.2.max(f.x + f.w), b.3.max(f.y + f.h));
    }
    if b.0 > b.2 { (0., 0., 100., 100.) } else { (b.0 - 40., b.1 - 70., b.2 + 40., b.3 + 40.) }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn hex((r, g, b): Rgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Cut a label to what fits under a node.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { format!("{}…", s.chars().take(max - 1).collect::<String>()) }
}

pub fn to_svg(s: &Scene, dark: bool) -> String {
    let (x0, y0, x1, y1) = bounds(s);
    let (bg, ink, muted, surface) = if dark { ("#141413", "#F2F0EA", "#A19C93", "#1C1B1A") } else { ("#F7F6F3", "#1C1B19", "#6B675F", "#FFFFFF") };
    let mut o = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{x0:.0} {y0:.0} {:.0} {:.0}\" width=\"{:.0}\" height=\"{:.0}\" font-family=\"Inter, Helvetica, Arial, sans-serif\">\n", x1 - x0, y1 - y0, x1 - x0, y1 - y0);
    o.push_str(&format!("<rect x=\"{x0:.0}\" y=\"{y0:.0}\" width=\"{:.0}\" height=\"{:.0}\" fill=\"{bg}\"/>\n", x1 - x0, y1 - y0));
    o.push_str(&format!("<text x=\"{:.0}\" y=\"{:.0}\" font-size=\"20\" font-weight=\"600\" fill=\"{ink}\">{}</text>\n", x0 + 32., y0 + 40., esc(&s.title)));
    for f in &s.frames {
        o.push_str(&format!("<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" rx=\"16\" fill=\"{}\" fill-opacity=\"0.06\" stroke=\"{}\" stroke-opacity=\"0.45\"/>\n", f.x, f.y, f.w, f.h, hex(f.color), hex(f.color)));
        o.push_str(&format!("<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"13\" font-weight=\"600\" fill=\"{}\">{}</text>\n", f.x + 14., f.y + 22., hex(f.color), esc(&f.label)));
    }
    for e in &s.edges {
        o.push_str(&format!("<line x1=\"{:.1}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"{}\" stroke-width=\"2\" stroke-opacity=\"0.7\"{}/>\n", e.a.0, e.a.1, e.b.0, e.b.1, hex(e.color), if e.dashed { " stroke-dasharray=\"6 5\"" } else { "" }));
    }
    for n in &s.nodes {
        let op = if n.dim { "0.4" } else { "1" };
        let r = if n.hub { R + 4. } else { R };
        o.push_str(&format!("<g opacity=\"{op}\">"));
        o.push_str(&format!("<circle cx=\"{:.1}\" cy=\"{:.1}\" r=\"{r}\" fill=\"{surface}\" stroke=\"{}\" stroke-width=\"{}\"/>", n.x, n.y, hex(n.color), if n.hub { 3 } else { 2 }));
        o.push_str(&format!("<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"11\" font-weight=\"600\" text-anchor=\"middle\" fill=\"{}\">{}</text>", n.x, n.y + 4., hex(n.color), esc(&n.glyph)));
        o.push_str(&format!("<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"12\" font-weight=\"600\" text-anchor=\"middle\" fill=\"{ink}\" stroke=\"{bg}\" stroke-width=\"4\" paint-order=\"stroke\">{}</text>", n.x, n.y + r + 16., esc(&clip(&n.label, 22))));
        if !n.sub.is_empty() {
            o.push_str(&format!("<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"10.5\" text-anchor=\"middle\" fill=\"{muted}\" stroke=\"{bg}\" stroke-width=\"4\" paint-order=\"stroke\">{}</text>", n.x, n.y + r + 30., esc(&n.sub)));
        }
        o.push_str("</g>\n");
    }
    o.push_str("</svg>\n");
    o
}

/// PDF string literal in WinAnsi (Latin-1 range kept, anything else becomes '?').
fn pdf_text(s: &str) -> String {
    let mut o = String::from("(");
    for c in s.chars() {
        match c {
            '(' | ')' | '\\' => {
                o.push('\\');
                o.push(c);
            }
            '…' => o.push_str("\\205"),
            '→' => o.push_str("->"),
            c if (c as u32) < 0x80 && !c.is_control() => o.push(c),
            c if (0xA0..=0xFF).contains(&(c as u32)) => o.push_str(&format!("\\{:03o}", c as u32)),
            _ => o.push('?'),
        }
    }
    o.push(')');
    o
}

/// One page, vector, Helvetica; sized to the scene (scaled down to fit 1600 pt).
pub fn to_pdf(s: &Scene) -> Vec<u8> {
    let (x0, y0, x1, y1) = bounds(s);
    let (w, h) = (x1 - x0, y1 - y0);
    let k = (1600. / w.max(h)).min(1.);
    let (pw, ph) = (w * k, h * k);
    // map units → PDF points (origin bottom-left)
    let tx = |x: f32| (x - x0) * k;
    let ty = |y: f32| ph - (y - y0) * k;
    let rgb = |(r, g, b): Rgb| format!("{:.3} {:.3} {:.3}", r as f32 / 255., g as f32 / 255., b as f32 / 255.);
    let mut c = String::new();
    c.push_str(&format!("0.969 0.965 0.953 rg 0 0 {pw:.1} {ph:.1} re f\n"));
    let text = |c: &mut String, x: f32, y: f32, size: f32, bold: bool, color: Rgb, t: &str, center: bool| {
        let width = t.chars().count() as f32 * size * 0.52;
        let x = if center { x - width / 2. } else { x };
        c.push_str(&format!("BT /{} {size:.1} Tf {} rg {x:.1} {y:.1} Td {} Tj ET\n", if bold { "F2" } else { "F1" }, rgb(color), pdf_text(t)));
    };
    text(&mut c, 32. * k, ph - 40. * k, 20. * k, true, (0x1C, 0x1B, 0x19), &s.title, false);
    for f in &s.frames {
        c.push_str(&format!("{} RG 1 w {:.1} {:.1} {:.1} {:.1} re S\n", rgb(f.color), tx(f.x), ty(f.y + f.h), f.w * k, f.h * k));
        text(&mut c, tx(f.x + 14.), ty(f.y + 22.), 13. * k, true, f.color, &f.label, false);
    }
    for e in &s.edges {
        c.push_str(&format!("{} RG {:.2} w {} {:.1} {:.1} m {:.1} {:.1} l S [] 0 d\n", rgb(e.color), 2. * k, if e.dashed { format!("[{:.1} {:.1}] 0 d", 6. * k, 5. * k) } else { String::new() }, tx(e.a.0), ty(e.a.1), tx(e.b.0), ty(e.b.1)));
    }
    for n in &s.nodes {
        let r = (if n.hub { R + 4. } else { R }) * k;
        let (cx, cy) = (tx(n.x), ty(n.y));
        // a circle from four Béziers
        let m = 0.5523 * r;
        let col = if n.dim { blend(n.color) } else { n.color };
        c.push_str(&format!(
            "1 1 1 rg {} RG {:.2} w {:.1} {:.1} m {:.1} {:.1} {:.1} {:.1} {:.1} {:.1} c {:.1} {:.1} {:.1} {:.1} {:.1} {:.1} c {:.1} {:.1} {:.1} {:.1} {:.1} {:.1} c {:.1} {:.1} {:.1} {:.1} {:.1} {:.1} c b\n",
            rgb(col),
            2. * k,
            cx + r, cy,
            cx + r, cy + m, cx + m, cy + r, cx, cy + r,
            cx - m, cy + r, cx - r, cy + m, cx - r, cy,
            cx - r, cy - m, cx - m, cy - r, cx, cy - r,
            cx + m, cy - r, cx + r, cy - m, cx + r, cy
        ));
        text(&mut c, cx, cy - 4. * k, 11. * k, true, col, &n.glyph, true);
        let ink = if n.dim { (0xA0, 0x9C, 0x94) } else { (0x1C, 0x1B, 0x19) };
        text(&mut c, cx, cy - r - 16. * k, 12. * k, true, ink, &clip(&n.label, 22), true);
        if !n.sub.is_empty() {
            text(&mut c, cx, cy - r - 30. * k, 10.5 * k, false, (0x6B, 0x67, 0x5F), &n.sub, true);
        }
    }
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw:.1} {ph:.1}] /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> /Contents 4 0 R >>"),
        format!("<< /Length {} >>\nstream\n{c}endstream", c.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>".to_string(),
        format!("<< /Title {} /Producer (Nodo) >>", pdf_text(&s.title)),
    ];
    let mut out = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R /Info {} 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1, objs.len()).as_bytes());
    out
}

fn blend((r, g, b): Rgb) -> Rgb {
    let m = |c: u8| ((c as u16 + 2 * 0xD8) / 3) as u8;
    (m(r), m(g), m(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_and_pdf() {
        i18n::set_locale("en");
        let m = crate::graph::tests::sample();
        let g = crate::graph::build(&m, &crate::graph::GraphOpts::default());
        let p = crate::layout::layout(&g, crate::layout::Mode::Radial);
        let s = scene(&m, &g, &p, "Home <map>", &|id| Some(id != 3));
        assert_eq!(s.nodes.len(), g.nodes.len());
        assert!(s.nodes.iter().find(|n| n.label == "phone").unwrap().dim, "offline is greyed");
        assert_eq!(s.edges.iter().filter(|e| e.dashed).count(), 1);
        let svg = to_svg(&s, false);
        assert!(svg.starts_with("<svg") && svg.trim_end().ends_with("</svg>"));
        assert!(svg.contains("Home &lt;map&gt;"));
        assert!(svg.contains(">192.168.1.20<"));
        let pdf = to_pdf(&s);
        assert!(pdf.starts_with(b"%PDF-1.4"));
        assert!(pdf.ends_with(b"%%EOF\n"));
        let text = String::from_utf8_lossy(&pdf);
        // startxref points at the xref table (byte offset)
        let xref_at: usize = text.rsplit("startxref\n").next().unwrap().lines().next().unwrap().parse().unwrap();
        assert_eq!(&pdf[xref_at..xref_at + 4], b"xref");
        assert!(text.contains("(laptop) Tj"));
        assert_eq!(pdf_text("a(b)\\ é→"), "(a\\(b\\)\\\\ \\351->)");
    }
}
