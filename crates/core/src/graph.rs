//! The map as a tree for drawing: the LAN gateway (or a virtual root) in the centre, networks as
//! hubs (a LAN subnet, the tailnet, each VPC and its subnets, each WireGuard interface) and
//! devices under the hub of their first visible layer. A device on more layers gets overlay
//! edges to its other hubs.
use crate::db::MapData;
use crate::model::{DeviceId, Layer, NetworkId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeRef {
    /// No gateway known (or LAN hidden): the map's own centre.
    Root,
    Network(NetworkId),
    Device(DeviceId),
}

#[derive(Debug, Clone, PartialEq)]
pub struct GNode {
    pub node: NodeRef,
    pub parent: Option<usize>,
    pub layer: Option<Layer>,
    /// Stable sort key; layouts order siblings by it so positions don't depend on load order.
    pub key: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Graph {
    pub nodes: Vec<GNode>,
    /// (device node, hub node, layer) for a device's other layers.
    pub overlay: Vec<(usize, usize, Layer)>,
}

impl Graph {
    pub fn index_of(&self, r: NodeRef) -> Option<usize> {
        self.nodes.iter().position(|n| n.node == r)
    }

    pub fn children(&self, i: usize) -> Vec<usize> {
        let mut c: Vec<usize> = (0..self.nodes.len()).filter(|&j| self.nodes[j].parent == Some(i)).collect();
        c.sort_by(|a, b| self.nodes[*a].key.cmp(&self.nodes[*b].key));
        c
    }
}

#[derive(Debug, Clone)]
pub struct GraphOpts {
    pub layers: Vec<Layer>,
}

impl Default for GraphOpts {
    fn default() -> Self {
        GraphOpts { layers: Layer::ALL.to_vec() }
    }
}

fn layer_rank(l: Layer) -> u8 {
    Layer::ALL.iter().position(|x| *x == l).unwrap_or(9) as u8
}

/// The LAN network whose gateway device we know, else the first LAN network.
pub fn primary_lan(map: &MapData) -> Option<(NetworkId, Option<DeviceId>)> {
    let lans: Vec<_> = map.networks.iter().filter(|n| n.layer == Layer::Lan).collect();
    for n in &lans {
        if let Some(gw) = n.gateway
            && let Some(d) = map.devices.iter().find(|d| d.addrs.iter().any(|a| a.ip == gw && a.layer == Layer::Lan))
        {
            return Some((n.id, Some(d.id)));
        }
    }
    lans.first().map(|n| (n.id, None))
}

pub fn build(map: &MapData, opts: &GraphOpts) -> Graph {
    let visible = |l: Layer| opts.layers.contains(&l);
    let mut g = Graph::default();
    let (primary, gw_dev) = match primary_lan(map) {
        Some((n, d)) if visible(Layer::Lan) => (Some(n), d),
        _ => (None, None),
    };
    g.nodes.push(GNode { node: gw_dev.map(NodeRef::Device).unwrap_or(NodeRef::Root), parent: None, layer: gw_dev.map(|_| Layer::Lan), key: String::new() });
    // hubs: parents (VPCs) before children (subnets)
    let mut nets: Vec<_> = map.networks.iter().filter(|n| visible(n.layer) && Some(n.id) != primary).collect();
    nets.sort_by_key(|n| (n.parent.is_some(), layer_rank(n.layer), n.name.to_lowercase(), n.id));
    for n in &nets {
        let parent = n.parent.and_then(|p| g.index_of(NodeRef::Network(p))).unwrap_or(0);
        g.nodes.push(GNode { node: NodeRef::Network(n.id), parent: Some(parent), layer: Some(n.layer), key: format!("0{}{}", layer_rank(n.layer), n.name.to_lowercase()) });
    }
    let hub_for = |g: &Graph, net: Option<NetworkId>| -> usize {
        match net {
            Some(id) if Some(id) != primary => g.index_of(NodeRef::Network(id)).unwrap_or(0),
            _ => 0,
        }
    };
    let mut devices: Vec<_> = map.devices.iter().filter(|d| Some(d.id) != gw_dev).collect();
    devices.sort_by_key(|d| (d.name.to_lowercase(), d.id));
    for d in devices {
        let mut addrs: Vec<_> = d.addrs.iter().filter(|a| visible(a.layer)).collect();
        if addrs.is_empty() && !(d.addrs.is_empty() && d.saved) {
            continue;
        }
        addrs.sort_by_key(|a| (layer_rank(a.layer), !a.ip.is_ipv4()));
        let first = addrs.first();
        let parent = hub_for(&g, first.and_then(|a| a.network));
        let me = g.nodes.len();
        g.nodes.push(GNode { node: NodeRef::Device(d.id), parent: Some(parent), layer: first.map(|a| a.layer), key: format!("1{}{:010}", d.name.to_lowercase(), d.id) });
        let mut linked = vec![parent];
        for a in addrs.iter().skip(1) {
            let h = hub_for(&g, a.network);
            if !linked.contains(&h) {
                linked.push(h);
                g.overlay.push((me, h, a.layer));
            }
        }
    }
    g
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::*;

    pub fn sample() -> MapData {
        let net = |id, key: &str, layer, cidr: &str, parent| Network { id, name: key.into(), layer, cidr: Some(cidr.into()), gateway: (id == 1).then(|| "192.168.1.1".parse().unwrap()), parent, key: key.into(), facts: vec![] };
        let networks = vec![net(1, "lan", Layer::Lan, "192.168.1.0/24", None), net(2, "tailnet", Layer::Tailscale, "100.64.0.0/10", None), net(3, "vpc", Layer::Aws, "10.0.0.0/16", None), net(4, "subnet-a", Layer::Aws, "10.0.1.0/24", Some(3))];
        let dev = |id, name: &str, addrs: Vec<(&str, Layer, NetworkId)>| Device { id, name: name.into(), addrs: addrs.into_iter().map(|(ip, layer, n)| Addr { ip: ip.parse().unwrap(), layer, network: Some(n), source: Source::Arp, last_seen: 0 }).collect(), ..Default::default() };
        let devices = vec![
            dev(1, "router", vec![("192.168.1.1", Layer::Lan, 1)]),
            dev(2, "laptop", vec![("192.168.1.20", Layer::Lan, 1), ("100.64.0.2", Layer::Tailscale, 2)]),
            dev(3, "phone", vec![("100.64.0.3", Layer::Tailscale, 2)]),
            dev(4, "web-1", vec![("10.0.1.10", Layer::Aws, 4)]),
            Device { id: 5, name: "old nas".into(), saved: true, ..Default::default() },
        ];
        MapData { networks, devices }
    }

    #[test]
    fn tree_shape() {
        let m = sample();
        let g = build(&m, &GraphOpts::default());
        assert_eq!(g.nodes[0].node, NodeRef::Device(1), "the gateway is the centre");
        let idx = |r| g.index_of(r).unwrap();
        assert_eq!(g.nodes[idx(NodeRef::Device(2))].parent, Some(0));
        assert_eq!(g.nodes[idx(NodeRef::Device(3))].parent, Some(idx(NodeRef::Network(2))));
        assert_eq!(g.nodes[idx(NodeRef::Network(4))].parent, Some(idx(NodeRef::Network(3))));
        assert_eq!(g.nodes[idx(NodeRef::Device(4))].parent, Some(idx(NodeRef::Network(4))));
        assert_eq!(g.nodes[idx(NodeRef::Device(5))].parent, Some(0), "saved device without addresses");
        assert_eq!(g.overlay, vec![(idx(NodeRef::Device(2)), idx(NodeRef::Network(2)), Layer::Tailscale)]);
        assert!(g.index_of(NodeRef::Network(1)).is_none(), "the gateway stands for its LAN");
        // LAN hidden: virtual root, the laptop hangs from the tailnet
        let g = build(&m, &GraphOpts { layers: vec![Layer::Tailscale] });
        assert_eq!(g.nodes[0].node, NodeRef::Root);
        assert_eq!(g.nodes.iter().filter(|n| matches!(n.node, NodeRef::Device(_))).count(), 3, "laptop, phone, and the saved one");
        assert_eq!(g.nodes[g.index_of(NodeRef::Device(2)).unwrap()].parent, g.index_of(NodeRef::Network(2)));
        assert!(g.overlay.is_empty());
    }
}
