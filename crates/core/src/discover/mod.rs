//! Discovery sources. Passive ones (neighbour table, mDNS, SSDP) run on refresh, and so does
//! Tailscale when installed (it only reads the local daemon's status); the active subnet scan,
//! AWS and WireGuard run only when the user asks.
pub mod arp;
pub mod aws;
pub mod classify;
pub mod mdns;
pub mod netinfo;
pub mod scan;
pub mod ssdp;
pub mod tailscale;
pub mod wireguard;

use crate::model::{Discovery, Layer, NetworkObservation, Source};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

/// Which passive sources to run.
#[derive(Debug, Clone, Copy)]
pub struct Passive {
    pub arp: bool,
    pub mdns: bool,
    pub ssdp: bool,
    pub wait: Duration,
}

impl Default for Passive {
    fn default() -> Self {
        Passive { arp: true, mdns: true, ssdp: true, wait: Duration::from_secs(4) }
    }
}

fn lan_network(net: &netinfo::LocalNet) -> NetworkObservation {
    NetworkObservation { key: net.key(), name: net.cidr().to_string(), layer: Some(Layer::Lan), cidr: Some(net.cidr().to_string()), gateway: net.gateway.map(IpAddr::V4), facts: vec![("interface".into(), net.iface.clone())], ..Default::default() }
}

/// The LAN network and this computer, from [`netinfo::detect`].
pub fn local(net: &netinfo::LocalNet) -> Discovery {
    let me = crate::this_computer::LocalIdentity { hostname: hostname(), ..Default::default() };
    Discovery { networks: vec![lan_network(net)], devices: vec![crate::this_computer::observation(Some(net), &me)], notes: Vec::new() }
}

/// This computer's host name, from the environment or `hostname`.
pub fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME").ok().or_else(|| std::env::var("HOSTNAME").ok()).filter(|h| !h.is_empty()).or_else(|| crate::cmd::run_named("hostname", &[], Duration::from_secs(3)).ok().map(|o| o.stdout.trim().to_string()).filter(|h| !h.is_empty()))
}

/// Run the passive sources; blocking (a few seconds for mDNS and SSDP, in parallel).
pub fn passive(opts: Passive) -> Discovery {
    passive_with(opts, netinfo::detect().as_ref(), &crate::this_computer::detect())
}

/// [`passive`] with this computer's LAN and interfaces already detected. This computer is
/// always reported, network or not.
pub fn passive_with(opts: Passive, net: Option<&netinfo::LocalNet>, me: &crate::this_computer::LocalIdentity) -> Discovery {
    let mut d = Discovery::default();
    if let Some(n) = net {
        d.networks.push(lan_network(n));
    }
    d.devices.push(crate::this_computer::observation(net, me));
    let mdns = opts.mdns.then(|| std::thread::spawn(move || mdns::browse(opts.wait)));
    let ssdp = opts.ssdp.then(|| {
        std::thread::spawn(move || {
            let r = ssdp::search(opts.wait.min(Duration::from_secs(3)))?;
            let desc = ssdp::fetch_descriptions(&r);
            Ok::<_, String>(ssdp::to_observations(&r, &desc))
        })
    });
    if opts.arp {
        match arp::read() {
            Ok(n) => d.devices.extend(arp::to_observations(&n)),
            Err(e) => d.notes.push((Source::Arp, i18n::t!("core.arp.failed", detail = e))),
        }
    }
    if let Some(h) = mdns {
        match h.join() {
            Ok(Ok(r)) => d.devices.extend(mdns::to_observations(&r)),
            Ok(Err(e)) => d.notes.push((Source::Mdns, i18n::t!("core.mdns.failed", detail = e))),
            Err(_) => {}
        }
    }
    if let Some(h) = ssdp {
        match h.join() {
            Ok(Ok(o)) => d.devices.extend(o),
            Ok(Err(e)) => d.notes.push((Source::Ssdp, i18n::t!("core.ssdp.failed", detail = e))),
            Err(_) => {}
        }
    }
    d
}

/// The active scan of the current /24, then a fresh read of the neighbour table (the sweep
/// fills it) so answering hosts get their MAC.
pub fn active(net: &netinfo::LocalNet, opts: &scan::ScanOptions, progress: Arc<scan::ScanProgress>) -> Result<Discovery, scan::ScanError> {
    let range = crate::cidr::scan_range(net.ip, net.prefix);
    let hosts: Vec<_> = range.hosts(1024).into_iter().filter(|h| *h != net.ip).collect();
    let found = scan::sweep(&hosts, opts, Arc::new(crate::probe::SystemProber), progress)?;
    let mut d = local(net);
    d.devices.extend(found);
    if let Ok(n) = arp::read() {
        d.devices.extend(arp::to_observations(&n));
    }
    Ok(d)
}
