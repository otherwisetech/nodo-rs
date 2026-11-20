//! Positions for the graph, in map units with the centre node at (0, 0). Pure and
//! deterministic: siblings are ordered by their key, so the same map always looks the same.
//!
//! - **Radial**: hubs on a ring around the centre, each owning a sector sized by how many
//!   devices it holds; devices fill arcs further out inside their sector.
//! - **By subnet**: one box per hub holding its devices in a grid, boxes packed in rows under
//!   the centre.
//! - **By network**: one column per layer (LAN, Tailscale, AWS, WireGuard), each hub a
//!   section of its column.
use crate::graph::{Graph, NodeRef};
use crate::model::Layer;
use std::f32::consts::{FRAC_PI_2, TAU};

/// A node's footprint (icon, name and address under it).
pub const NODE_W: f32 = 132.;
pub const NODE_H: f32 = 92.;
/// Spacing along an arc and between rings.
const SLOT: f32 = 170.;
const STEP: f32 = 160.;
const FIRST_RING: f32 = 230.;
const CELL_W: f32 = 150.;
const CELL_H: f32 = 112.;
const BOX_PAD: f32 = 20.;
const BOX_HEAD: f32 = 120.;
const BOX_GAP: f32 = 48.;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    Radial,
    BySubnet,
    ByNetwork,
}

impl Mode {
    pub fn id(self) -> &'static str {
        match self {
            Mode::Radial => "radial",
            Mode::BySubnet => "subnet",
            Mode::ByNetwork => "network",
        }
    }

    pub fn from_id(s: &str) -> Mode {
        match s {
            "subnet" => Mode::BySubnet,
            "network" => Mode::ByNetwork,
            _ => Mode::Radial,
        }
    }
}

/// A labelled frame around a group of nodes (grouped modes).
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// The hub node it belongs to, or the layer for By network.
    pub hub: Option<usize>,
    pub layer: Option<Layer>,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Placed {
    /// Centre of each node, by graph index.
    pub pos: Vec<(f32, f32)>,
    pub frames: Vec<Frame>,
    /// Whether to draw an edge from every device to its hub (radial), or only hub links.
    pub leaf_edges: bool,
}

impl Placed {
    /// Bounding box (min x, min y, max x, max y) of every node and frame.
    pub fn bounds(&self) -> (f32, f32, f32, f32) {
        let mut b = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (x, y) in &self.pos {
            b = (b.0.min(x - NODE_W / 2.), b.1.min(y - NODE_H / 2.), b.2.max(x + NODE_W / 2.), b.3.max(y + NODE_H / 2.));
        }
        for f in &self.frames {
            b = (b.0.min(f.x), b.1.min(f.y), b.2.max(f.x + f.w), b.3.max(f.y + f.h));
        }
        if b.0 > b.2 { (0., 0., 0., 0.) } else { b }
    }
}

pub fn layout(g: &Graph, mode: Mode) -> Placed {
    let mut p = Placed { pos: vec![(0., 0.); g.nodes.len()], frames: Vec::new(), leaf_edges: mode == Mode::Radial };
    if g.nodes.is_empty() {
        return p;
    }
    let kids: Vec<Vec<usize>> = (0..g.nodes.len()).map(|i| g.children(i)).collect();
    match mode {
        Mode::Radial => {
            let weights = leaf_weights(&kids);
            radial(&kids, &weights, 0, -FRAC_PI_2, -FRAC_PI_2 + TAU, 0., &mut p.pos, true);
        }
        Mode::BySubnet => by_subnet(g, &kids, &mut p),
        Mode::ByNetwork => by_network(g, &kids, &mut p),
    }
    p
}

/// Devices under each node (a node without children counts as one).
fn leaf_weights(kids: &[Vec<usize>]) -> Vec<f32> {
    fn w(i: usize, kids: &[Vec<usize>], memo: &mut Vec<Option<f32>>) -> f32 {
        if let Some(v) = memo[i] {
            return v;
        }
        let v = if kids[i].is_empty() { 1. } else { kids[i].iter().map(|&k| w(k, kids, memo)).sum::<f32>().max(1.) };
        memo[i] = Some(v);
        v
    }
    let mut memo = vec![None; kids.len()];
    (0..kids.len()).map(|i| w(i, kids, &mut memo)).collect()
}

/// Smallest angle that keeps two nodes `SLOT` apart at radius `r`.
fn slot_angle(r: f32) -> f32 {
    2. * (SLOT / (2. * r.max(1.))).min(1.).asin()
}

#[allow(clippy::too_many_arguments)]
fn radial(kids: &[Vec<usize>], weights: &[f32], i: usize, a0: f32, a1: f32, r: f32, pos: &mut [(f32, f32)], root: bool) {
    let leaves: Vec<usize> = kids[i].iter().copied().filter(|&k| kids[k].is_empty()).collect();
    let hubs: Vec<usize> = kids[i].iter().copied().filter(|&k| !kids[k].is_empty()).collect();
    let total: f32 = leaves.len() as f32 + hubs.iter().map(|&h| weights[h]).sum::<f32>();
    if total == 0. {
        return;
    }
    let span = a1 - a0;
    let start = if root { FIRST_RING } else { r + STEP };
    let mut a = a0;
    // the hub's own devices, ring after ring
    if !leaves.is_empty() {
        let block = span * leaves.len() as f32 / total;
        let mut rr = start;
        let mut left = &leaves[..];
        while !left.is_empty() {
            let fit = (block / slot_angle(rr)).floor() as usize;
            if fit == 0 {
                rr += STEP;
                continue;
            }
            // a full circle has no ends: the last slot touches the first
            let n = fit.min(left.len());
            let gap = block / n as f32;
            for (k, &leaf) in left[..n].iter().enumerate() {
                let ang = a + gap * (k as f32 + 0.5);
                pos[leaf] = (rr * ang.cos(), rr * ang.sin());
            }
            left = &left[n..];
            rr += STEP;
        }
        a += block;
    }
    for &h in &hubs {
        let sub = span * weights[h] / total;
        let mut hr = start;
        while sub < slot_angle(hr) {
            hr += STEP / 2.;
        }
        let ang = a + sub / 2.;
        pos[h] = (hr * ang.cos(), hr * ang.sin());
        radial(kids, weights, h, a, a + sub, hr, pos, false);
        a += sub;
    }
}

/// (w, h) of a grid of `n` cells and the cell positions relative to its top-left.
fn grid(n: usize, max_cols: usize) -> (f32, f32, Vec<(f32, f32)>) {
    if n == 0 {
        return (CELL_W, 0., Vec::new());
    }
    let cols = ((n as f32 * 1.6).sqrt().ceil() as usize).clamp(1, max_cols.max(1)).min(n);
    let rows = n.div_ceil(cols);
    let cells = (0..n).map(|k| ((k % cols) as f32 * CELL_W + CELL_W / 2., (k / cols) as f32 * CELL_H + CELL_H / 2.)).collect();
    (cols as f32 * CELL_W, rows as f32 * CELL_H, cells)
}

fn layer_of(g: &Graph, i: usize) -> Layer {
    g.nodes[i].layer.unwrap_or(Layer::Lan)
}

/// Hub, its devices, box width and height, cell centres.
type SizedBox = (usize, Vec<usize>, f32, f32, Vec<(f32, f32)>);

fn by_subnet(g: &Graph, kids: &[Vec<usize>], p: &mut Placed) {
    // one box per node that has devices (the centre's own devices included)
    let mut boxes: Vec<(usize, Vec<usize>)> = Vec::new();
    let mut order: Vec<usize> = vec![0];
    order.extend((1..g.nodes.len()).filter(|&i| matches!(g.nodes[i].node, NodeRef::Network(_))));
    order.sort_by_key(|&i| (i != 0, layer_of(g, i), g.nodes[i].key.clone()));
    let mut bare_hubs = Vec::new();
    for &h in &order {
        let leaves: Vec<usize> = kids[h].iter().copied().filter(|&k| kids[k].is_empty() && !matches!(g.nodes[k].node, NodeRef::Network(_))).collect();
        if leaves.is_empty() && h != 0 {
            bare_hubs.push(h);
        } else if !leaves.is_empty() {
            boxes.push((h, leaves));
        }
    }
    // hubs holding only other hubs (a VPC) sit in a row under the centre
    let top = NODE_H + 70.;
    let row_w = bare_hubs.len() as f32 * CELL_W;
    for (k, &h) in bare_hubs.iter().enumerate() {
        p.pos[h] = (-row_w / 2. + CELL_W * (k as f32 + 0.5), top);
    }
    let y0 = if bare_hubs.is_empty() { NODE_H } else { top + CELL_H / 2. + 10. };
    let sized: Vec<SizedBox> = boxes
        .into_iter()
        .map(|(h, leaves)| {
            let (w, hh, cells) = grid(leaves.len(), 8);
            (h, leaves, w + BOX_PAD * 2., hh + BOX_PAD + BOX_HEAD, cells)
        })
        .collect();
    let area: f32 = sized.iter().map(|(_, _, w, h, _)| (w + BOX_GAP) * (h + BOX_GAP)).sum();
    let max_row = (area.sqrt() * 1.6).max(1400.);
    // shelf packing into rows, each row centred
    let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
    let mut row_w = 0.;
    for (k, (_, _, w, _, _)) in sized.iter().enumerate() {
        if row_w > 0. && row_w + w > max_row {
            rows.push(Vec::new());
            row_w = 0.;
        }
        rows.last_mut().unwrap().push(k);
        row_w += w + BOX_GAP;
    }
    let mut y = y0;
    for row in rows {
        let w: f32 = row.iter().map(|&k| sized[k].2).sum::<f32>() + BOX_GAP * (row.len().saturating_sub(1)) as f32;
        let h = row.iter().map(|&k| sized[k].3).fold(0., f32::max);
        let mut x = -w / 2.;
        for k in row {
            let (hub, leaves, bw, bh, cells) = &sized[k];
            p.frames.push(Frame { hub: Some(*hub), layer: g.nodes[*hub].layer, x, y, w: *bw, h: *bh });
            if *hub != 0 {
                p.pos[*hub] = (x + bw / 2., y + BOX_HEAD / 2.);
            }
            for (leaf, (cx, cy)) in leaves.iter().zip(cells) {
                p.pos[*leaf] = (x + BOX_PAD + cx, y + BOX_HEAD + cy);
            }
            x += bw + BOX_GAP;
        }
        y += h + BOX_GAP;
    }
    // the centre sits above everything; its own devices are the first box
    p.pos[0] = (0., 0.);
}

fn by_network(g: &Graph, kids: &[Vec<usize>], p: &mut Placed) {
    let mut col_x = 0.;
    let mut columns = Vec::new();
    for layer in Layer::ALL {
        // sections: the centre's own devices count as LAN
        let mut sections: Vec<(Option<usize>, Vec<usize>)> = Vec::new();
        let mut hubs: Vec<usize> = (1..g.nodes.len()).filter(|&i| matches!(g.nodes[i].node, NodeRef::Network(_)) && layer_of(g, i) == layer).collect();
        hubs.sort_by_key(|&i| (g.nodes[i].parent.is_some_and(|q| q != 0), g.nodes[i].key.clone()));
        if layer == Layer::Lan {
            let own: Vec<usize> = kids[0].iter().copied().filter(|&k| kids[k].is_empty() && !matches!(g.nodes[k].node, NodeRef::Network(_))).collect();
            if !own.is_empty() {
                sections.push((None, own));
            }
        }
        for h in hubs {
            let leaves: Vec<usize> = kids[h].iter().copied().filter(|&k| kids[k].is_empty() && !matches!(g.nodes[k].node, NodeRef::Network(_))).collect();
            sections.push((Some(h), leaves));
        }
        if sections.is_empty() {
            continue;
        }
        let w = sections.iter().map(|(_, l)| grid(l.len(), 5).0).fold(CELL_W * 2., f32::max) + BOX_PAD * 2.;
        let top = NODE_H + 60.;
        let mut y = top + BOX_HEAD * 0.6;
        for (hub, leaves) in &sections {
            let (gw, gh, cells) = grid(leaves.len(), 5);
            if let Some(h) = hub {
                p.pos[*h] = (col_x + w / 2., y + NODE_H / 2.);
                y += NODE_H + 24.;
            }
            for (leaf, (cx, cy)) in leaves.iter().zip(cells) {
                p.pos[*leaf] = (col_x + (w - gw) / 2. + cx, y + cy);
            }
            y += gh + 30.;
        }
        p.frames.push(Frame { hub: None, layer: Some(layer), x: col_x, y: top, w, h: y - top });
        columns.push(p.frames.len() - 1);
        col_x += w + BOX_GAP;
    }
    // centre the columns under the centre node
    let shift = (col_x - BOX_GAP).max(0.) / 2.;
    for f in &mut p.frames {
        f.x -= shift;
    }
    for i in 1..p.pos.len() {
        p.pos[i].0 -= shift;
    }
    p.pos[0] = (0., 0.);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{GNode, build};

    fn overlaps(p: &Placed) -> Vec<(usize, usize)> {
        let mut v = Vec::new();
        for i in 0..p.pos.len() {
            for j in i + 1..p.pos.len() {
                let (a, b) = (p.pos[i], p.pos[j]);
                if (a.0 - b.0).abs() < NODE_W && (a.1 - b.1).abs() < NODE_H {
                    v.push((i, j));
                }
            }
        }
        v
    }

    /// The centre with `lan` devices, a tailnet of `ts`, a VPC with subnets of `aws` each.
    fn big(lan: usize, ts: usize, aws: &[usize]) -> Graph {
        let mut g = Graph::default();
        let node = |n, parent: Option<usize>, layer, key: String| GNode { node: n, parent, layer, key };
        g.nodes.push(node(NodeRef::Root, None, None, String::new()));
        for k in 0..lan {
            g.nodes.push(node(NodeRef::Device(k as i64 + 1000), Some(0), Some(Layer::Lan), format!("1lan{k:03}")));
        }
        let t = g.nodes.len();
        g.nodes.push(node(NodeRef::Network(1), Some(0), Some(Layer::Tailscale), "0ts".into()));
        for k in 0..ts {
            g.nodes.push(node(NodeRef::Device(k as i64 + 2000), Some(t), Some(Layer::Tailscale), format!("1ts{k:03}")));
        }
        let vpc = g.nodes.len();
        g.nodes.push(node(NodeRef::Network(2), Some(0), Some(Layer::Aws), "0vpc".into()));
        for (s, n) in aws.iter().enumerate() {
            let sn = g.nodes.len();
            g.nodes.push(node(NodeRef::Network(10 + s as i64), Some(vpc), Some(Layer::Aws), format!("0sub{s}")));
            for k in 0..*n {
                g.nodes.push(node(NodeRef::Device((3000 + s * 100 + k) as i64), Some(sn), Some(Layer::Aws), format!("1i{s}{k:03}")));
            }
        }
        g
    }

    #[test]
    fn no_overlaps_in_any_mode() {
        for g in [big(3, 2, &[1]), big(60, 14, &[5, 9, 2]), big(200, 40, &[30, 1, 12, 7]), big(0, 1, &[]), big(1, 0, &[0])] {
            for mode in [Mode::Radial, Mode::BySubnet, Mode::ByNetwork] {
                let p = layout(&g, mode);
                assert_eq!(p.pos.len(), g.nodes.len());
                assert!(overlaps(&p).is_empty(), "{mode:?} with {} nodes: {:?}", g.nodes.len(), &overlaps(&p)[..overlaps(&p).len().min(5)]);
                assert!(p.pos.iter().all(|(x, y)| x.is_finite() && y.is_finite()));
            }
        }
    }

    #[test]
    #[allow(clippy::needless_range_loop)]
    fn deterministic_whatever_the_input_order() {
        let g = big(30, 6, &[4, 4]);
        let mut shuffled = g.clone();
        // reverse the device order but keep parents valid: move nodes by key
        let mut order: Vec<usize> = (0..g.nodes.len()).collect();
        order[1..].reverse();
        let remap: Vec<usize> = {
            let mut r = vec![0; order.len()];
            for (new, &old) in order.iter().enumerate() {
                r[old] = new;
            }
            r
        };
        shuffled.nodes = order.iter().map(|&old| GNode { parent: g.nodes[old].parent.map(|p| remap[p]), ..g.nodes[old].clone() }).collect();
        for mode in [Mode::Radial, Mode::BySubnet, Mode::ByNetwork] {
            let a = layout(&g, mode);
            let b = layout(&shuffled, mode);
            for old in 0..g.nodes.len() {
                let (pa, pb) = (a.pos[old], b.pos[remap[old]]);
                assert!((pa.0 - pb.0).abs() < 0.01 && (pa.1 - pb.1).abs() < 0.01, "{mode:?}: node {old} moved");
            }
            assert_eq!(layout(&g, mode), a, "same input, same output");
        }
    }

    #[test]
    fn sample_map_layouts() {
        let m = crate::graph::tests::sample();
        let g = build(&m, &crate::graph::GraphOpts::default());
        let p = layout(&g, Mode::Radial);
        assert_eq!(p.pos[0], (0., 0.));
        assert!(p.leaf_edges);
        let p = layout(&g, Mode::BySubnet);
        assert!(!p.leaf_edges);
        assert!(p.frames.len() >= 3, "centre's LAN, tailnet, subnet-a");
        let p = layout(&g, Mode::ByNetwork);
        assert_eq!(p.frames.iter().map(|f| f.layer).collect::<Vec<_>>(), vec![Some(Layer::Lan), Some(Layer::Tailscale), Some(Layer::Aws)]);
        let (x0, _, x1, _) = p.bounds();
        assert!(x0 < 0. && x1 > 0., "columns centred");
        assert_eq!(Mode::from_id(Mode::BySubnet.id()), Mode::BySubnet);
    }
}
