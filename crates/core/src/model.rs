//! What a network map holds: networks (LAN subnets, a tailnet, AWS VPCs and subnets, WireGuard
//! interfaces) and devices with their addresses, identities, services and inventory fields.
use serde::{Deserialize, Serialize};
use std::net::IpAddr;

pub type DeviceId = i64;
pub type NetworkId = i64;

/// The overlay a network or address belongs to; each one is a toggleable layer on the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    Lan,
    Tailscale,
    Aws,
    WireGuard,
    /// Public addresses reached over the internet (a VPS, a remote server).
    Internet,
}

impl Layer {
    pub const ALL: [Layer; 5] = [Layer::Lan, Layer::Tailscale, Layer::Aws, Layer::WireGuard, Layer::Internet];

    pub fn id(self) -> &'static str {
        match self {
            Layer::Lan => "lan",
            Layer::Tailscale => "tailscale",
            Layer::Aws => "aws",
            Layer::WireGuard => "wireguard",
            Layer::Internet => "internet",
        }
    }

    pub fn from_id(s: &str) -> Option<Layer> {
        Layer::ALL.into_iter().find(|l| l.id() == s)
    }
}

/// Device type, for the icon and the filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    #[default]
    Unknown,
    Router,
    Switch,
    AccessPoint,
    Server,
    Nas,
    Camera,
    Printer,
    Tv,
    Speaker,
    Phone,
    Tablet,
    Laptop,
    Desktop,
    Iot,
    Vm,
    Vpn,
}

impl DeviceKind {
    pub const ALL: [DeviceKind; 17] = [
        DeviceKind::Unknown,
        DeviceKind::Router,
        DeviceKind::Switch,
        DeviceKind::AccessPoint,
        DeviceKind::Server,
        DeviceKind::Nas,
        DeviceKind::Camera,
        DeviceKind::Printer,
        DeviceKind::Tv,
        DeviceKind::Speaker,
        DeviceKind::Phone,
        DeviceKind::Tablet,
        DeviceKind::Laptop,
        DeviceKind::Desktop,
        DeviceKind::Iot,
        DeviceKind::Vm,
        DeviceKind::Vpn,
    ];

    pub fn id(self) -> &'static str {
        match self {
            DeviceKind::Unknown => "unknown",
            DeviceKind::Router => "router",
            DeviceKind::Switch => "switch",
            DeviceKind::AccessPoint => "ap",
            DeviceKind::Server => "server",
            DeviceKind::Nas => "nas",
            DeviceKind::Camera => "camera",
            DeviceKind::Printer => "printer",
            DeviceKind::Tv => "tv",
            DeviceKind::Speaker => "speaker",
            DeviceKind::Phone => "phone",
            DeviceKind::Tablet => "tablet",
            DeviceKind::Laptop => "laptop",
            DeviceKind::Desktop => "desktop",
            DeviceKind::Iot => "iot",
            DeviceKind::Vm => "vm",
            DeviceKind::Vpn => "vpn",
        }
    }

    pub fn from_id(s: &str) -> DeviceKind {
        DeviceKind::ALL.into_iter().find(|k| k.id() == s).unwrap_or_default()
    }
}

/// Where a piece of information came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Arp,
    Mdns,
    Ssdp,
    Scan,
    Tailscale,
    Aws,
    WireGuard,
    Manual,
    Import,
    Monitor,
}

impl Source {
    pub fn id(self) -> &'static str {
        match self {
            Source::Arp => "arp",
            Source::Mdns => "mdns",
            Source::Ssdp => "ssdp",
            Source::Scan => "scan",
            Source::Tailscale => "tailscale",
            Source::Aws => "aws",
            Source::WireGuard => "wireguard",
            Source::Manual => "manual",
            Source::Import => "import",
            Source::Monitor => "monitor",
        }
    }

    pub fn from_id(s: &str) -> Source {
        [Source::Arp, Source::Mdns, Source::Ssdp, Source::Scan, Source::Tailscale, Source::Aws, Source::WireGuard, Source::Manual, Source::Import, Source::Monitor].into_iter().find(|x| x.id() == s).unwrap_or(Source::Manual)
    }
}

/// Something that names one device across IP changes. `Mac`, `TailscaleNode`, `AwsInstance` and
/// `WgPeer` are strong (globally unique); `Mdns` (a `.local` host name) is strong on a LAN;
/// `Host` (a bare host name) is weak and only joins devices when it is unambiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdentKind {
    Mac,
    TailscaleNode,
    AwsInstance,
    WgPeer,
    Mdns,
    Host,
}

impl IdentKind {
    pub const ALL: [IdentKind; 6] = [IdentKind::Mac, IdentKind::TailscaleNode, IdentKind::AwsInstance, IdentKind::WgPeer, IdentKind::Mdns, IdentKind::Host];

    pub fn id(self) -> &'static str {
        match self {
            IdentKind::Mac => "mac",
            IdentKind::TailscaleNode => "tailscale",
            IdentKind::AwsInstance => "aws",
            IdentKind::WgPeer => "wg",
            IdentKind::Mdns => "mdns",
            IdentKind::Host => "host",
        }
    }

    pub fn from_id(s: &str) -> Option<IdentKind> {
        IdentKind::ALL.into_iter().find(|k| k.id() == s)
    }

    /// Higher wins when one observation matches several devices.
    pub fn strength(self) -> u8 {
        match self {
            IdentKind::TailscaleNode | IdentKind::AwsInstance | IdentKind::WgPeer => 4,
            IdentKind::Mac => 3,
            IdentKind::Mdns => 2,
            IdentKind::Host => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Ident {
    pub kind: IdentKind,
    pub value: String,
}

impl Ident {
    /// Normalised: MACs as `aa:bb:cc:dd:ee:ff`, host names lower case without `.local.`.
    pub fn new(kind: IdentKind, value: &str) -> Ident {
        let v = match kind {
            IdentKind::Mac => crate::mac::normalize(value).unwrap_or_else(|| value.to_lowercase()),
            IdentKind::Mdns => value.trim_end_matches('.').to_lowercase(),
            IdentKind::Host => host_label(value),
            _ => value.trim().to_string(),
        };
        Ident { kind, value: v }
    }
}

/// `Juans-MacBook.local.` / `juans-macbook.tail1234.ts.net` → `juans-macbook`.
pub fn host_label(s: &str) -> String {
    s.trim().trim_end_matches('.').split('.').next().unwrap_or_default().to_lowercase()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Addr {
    pub ip: IpAddr,
    pub layer: Layer,
    #[serde(default)]
    pub network: Option<NetworkId>,
    pub source: Source,
    #[serde(default)]
    pub last_seen: i64,
}

/// A service offered by the device (mDNS type, SSDP device type or an open port).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Service {
    /// `_ssh._tcp`, `urn:schemas-upnp-org:device:MediaRenderer:1`, `tcp/443`…
    pub name: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Device {
    pub id: DeviceId,
    /// Shown name: set by the user, else the best discovered name.
    pub name: String,
    /// The user named it; discovery no longer renames it.
    #[serde(default)]
    pub name_manual: bool,
    pub kind: DeviceKind,
    #[serde(default)]
    pub kind_manual: bool,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub location: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub links: Vec<Link>,
    /// User-defined fields, in order.
    #[serde(default)]
    pub fields: Vec<(String, String)>,
    /// In the inventory: kept (greyed) when offline, watched by the monitor, alerts when down.
    #[serde(default)]
    pub saved: bool,
    /// Reachability checks while the app is open (saved devices default to on).
    #[serde(default)]
    pub monitor: bool,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub addrs: Vec<Addr>,
    #[serde(default)]
    pub idents: Vec<Ident>,
    #[serde(default)]
    pub services: Vec<Service>,
    /// Discovery details by source: exit node, relay, instance type, security groups…
    #[serde(default)]
    pub facts: Vec<(String, String)>,
    #[serde(default)]
    pub first_seen: i64,
    #[serde(default)]
    pub last_seen: i64,
    /// Online according to the last discovery that knows (Tailscale, AWS state, WireGuard
    /// handshake); the monitor's own reachability lives in memory.
    #[serde(default)]
    pub online: Option<bool>,
}

impl Device {
    pub fn macs(&self) -> impl Iterator<Item = &str> {
        self.idents.iter().filter(|i| i.kind == IdentKind::Mac).map(|i| i.value.as_str())
    }

    pub fn ident(&self, kind: IdentKind) -> Option<&str> {
        self.idents.iter().find(|i| i.kind == kind).map(|i| i.value.as_str())
    }

    pub fn ips(&self) -> impl Iterator<Item = IpAddr> + '_ {
        self.addrs.iter().map(|a| a.ip)
    }

    pub fn layers(&self) -> Vec<Layer> {
        let mut l: Vec<Layer> = self.addrs.iter().map(|a| a.layer).collect();
        l.sort();
        l.dedup();
        l
    }

    /// The address used to reach it: LAN IPv4 first, then any IPv4, then anything.
    pub fn primary_ip(&self) -> Option<IpAddr> {
        let mut best: Option<(u8, IpAddr)> = None;
        for a in &self.addrs {
            let score = match (a.layer, a.ip.is_ipv4()) {
                (Layer::Lan, true) => 5,
                (Layer::Tailscale, true) => 4,
                (_, true) => 3,
                (Layer::Lan, false) => 2,
                _ => 1,
            };
            if best.is_none_or(|(s, _)| score > s) {
                best = Some((score, a.ip));
            }
        }
        best.map(|(_, ip)| ip)
    }

    pub fn fact(&self, key: &str) -> Option<&str> {
        self.facts.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn set_fact(&mut self, key: &str, value: &str) {
        match self.facts.iter_mut().find(|(k, _)| k == key) {
            Some(f) => f.1 = value.to_string(),
            None => self.facts.push((key.to_string(), value.to_string())),
        }
    }

    /// Lower-case text searched by ⌘F and the list filter.
    pub fn haystack(&self) -> String {
        let mut s = vec![self.name.clone(), self.kind.id().to_string(), self.owner.clone(), self.location.clone(), self.notes.clone()];
        s.extend(self.tags.iter().cloned());
        s.extend(self.addrs.iter().map(|a| a.ip.to_string()));
        s.extend(self.idents.iter().map(|i| i.value.clone()));
        s.extend([self.vendor.clone(), self.hostname.clone(), self.os.clone(), self.model.clone()].into_iter().flatten());
        s.extend(self.fields.iter().map(|(_, v)| v.clone()));
        s.join(" ").to_lowercase()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Network {
    pub id: NetworkId,
    pub name: String,
    pub layer: Layer,
    /// `192.168.1.0/24`, `10.0.0.0/16`, `100.64.0.0/10`…
    #[serde(default)]
    pub cidr: Option<String>,
    #[serde(default)]
    pub gateway: Option<IpAddr>,
    /// An AWS subnet's VPC.
    #[serde(default)]
    pub parent: Option<NetworkId>,
    /// Stable key from discovery so a rescan finds the same network: `lan:192.168.1.0/24`,
    /// `tailscale:<tailnet>`, `aws:<profile>:<region>:<vpc-id>`, `wg:<interface>`.
    pub key: String,
    #[serde(default)]
    pub facts: Vec<(String, String)>,
}

impl Network {
    pub fn fact(&self, key: &str) -> Option<&str> {
        self.facts.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

/// One entry of a device's history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub device: Option<DeviceId>,
    pub at: i64,
    pub kind: EventKind,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Discovered,
    IpChanged,
    Up,
    Down,
    AlertDown,
    AlertNew,
    Merged,
    Split,
    Edited,
    Scan,
}

impl EventKind {
    pub const ALL: [EventKind; 10] = [EventKind::Discovered, EventKind::IpChanged, EventKind::Up, EventKind::Down, EventKind::AlertDown, EventKind::AlertNew, EventKind::Merged, EventKind::Split, EventKind::Edited, EventKind::Scan];

    pub fn id(self) -> &'static str {
        match self {
            EventKind::Discovered => "discovered",
            EventKind::IpChanged => "ip_changed",
            EventKind::Up => "up",
            EventKind::Down => "down",
            EventKind::AlertDown => "alert_down",
            EventKind::AlertNew => "alert_new",
            EventKind::Merged => "merged",
            EventKind::Split => "split",
            EventKind::Edited => "edited",
            EventKind::Scan => "scan",
        }
    }

    pub fn from_id(s: &str) -> Option<EventKind> {
        EventKind::ALL.into_iter().find(|k| k.id() == s)
    }
}

/// What one discovery source saw of one device.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Observation {
    pub layer: Option<Layer>,
    pub source: Option<Source>,
    pub ips: Vec<IpAddr>,
    pub idents: Vec<Ident>,
    pub name: Option<String>,
    pub hostname: Option<String>,
    pub os: Option<String>,
    pub model: Option<String>,
    pub vendor: Option<String>,
    pub kind: Option<DeviceKind>,
    pub services: Vec<Service>,
    pub online: Option<bool>,
    pub latency_ms: Option<f64>,
    /// `Network::key` of the network it was seen on, when the source knows (VPC subnet,
    /// tailnet, WireGuard interface); LAN addresses are matched to subnets by CIDR.
    pub network: Option<String>,
    pub facts: Vec<(String, String)>,
    /// When the source last saw it (unix seconds), if it says; else "now".
    pub seen_at: Option<i64>,
    /// Who the device belongs to, per the source (a tailnet user); fills `Device::owner`
    /// unless the user typed one.
    pub owner: Option<String>,
    /// LAN addresses the source says this machine has (Tailscale endpoints): only used to find
    /// the LAN device it is, never stored as addresses.
    pub lan_hints: Vec<IpAddr>,
}

impl Observation {
    pub fn new(layer: Layer, source: Source) -> Observation {
        Observation { layer: Some(layer), source: Some(source), ..Default::default() }
    }

    pub fn layer(&self) -> Layer {
        self.layer.unwrap_or(Layer::Lan)
    }

    pub fn source(&self) -> Source {
        self.source.unwrap_or(Source::Manual)
    }

    pub fn ident(mut self, kind: IdentKind, value: &str) -> Self {
        if !value.trim().is_empty() {
            let id = Ident::new(kind, value);
            if !self.idents.contains(&id) {
                self.idents.push(id);
            }
        }
        self
    }

    pub fn fact(mut self, k: &str, v: impl Into<String>) -> Self {
        let v = v.into();
        if !v.is_empty() {
            self.facts.push((k.to_string(), v));
        }
        self
    }
}

/// A network discovered by a source (VPCs, subnets, a tailnet, a WireGuard interface).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NetworkObservation {
    pub key: String,
    pub name: String,
    pub layer: Option<Layer>,
    pub cidr: Option<String>,
    pub gateway: Option<IpAddr>,
    pub parent_key: Option<String>,
    pub facts: Vec<(String, String)>,
}

/// Everything a discovery run returns.
#[derive(Debug, Clone, Default)]
pub struct Discovery {
    pub networks: Vec<NetworkObservation>,
    pub devices: Vec<Observation>,
    /// Per-source problems worth showing (CLI not found, needs sudo, access denied).
    pub notes: Vec<(Source, String)>,
}

impl Discovery {
    pub fn extend(&mut self, other: Discovery) {
        self.networks.extend(other.networks);
        self.devices.extend(other.devices);
        self.notes.extend(other.notes);
    }
}
