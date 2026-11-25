//! Nodo's side of the shared entity registry of the Otherwise apps (`otherwise_link::registry`),
//! only when the user turned it on in Otherwise Hub.
//!
//! - **Push**: every saved device of the map becomes a `device` card (MACs, host names, IPs,
//!   Tailscale node, vendor, model, OS), and every place named in a saved device's *Location*
//!   a `site` card. Refs are `device-<id>@<map uid>` / `site-<name>@<map uid>`, so two maps never
//!   claim each other's cards; a device another map already holds (same MAC) is updated without
//!   taking its ref. A card is pushed again only when what Nodo would send changed.
//! - **Pull**: only to keep the links right (merges repoint, a card deleted in Otherwise Hub
//!   detaches the device for good); Nodo's own data stays Nodo's.
//! - **Owners**: a device's *Owner* text is matched against the registry's people and companies
//!   ([`owner_card`]) so the app can label it and the card can point at it.
//!
//! Every call is one short transaction; the caller runs this off the UI thread.
use crate::db::{MapData, Store, StoreError};
use crate::model::{Device, IdentKind};
use otherwise_link::entity::{self as card, Entity as Card, Kind as CardKind};
use otherwise_link::registry::{Error as RegError, Op, Registry};
use otherwise_link::text::fold;
use otherwise_link::Root;
use serde_json::json;
use uuid::Uuid;

pub const APP: &str = "nodo";
/// `shared_id` of a link whose card was deleted in the registry: never pushed again.
pub const DETACHED: &str = "detached";
pub const DEVICE: &str = "device";
pub const SITE: &str = "site";
const UID_KEY: &str = "shared_uid";
const BATCH: usize = 500;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub pushed: usize,
    pub unlinked: usize,
    pub repointed: usize,
    pub errors: Vec<String>,
}

#[derive(Debug)]
pub enum SyncError {
    Store(StoreError),
    Registry(RegError),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Store(e) => write!(f, "{e}"),
            SyncError::Registry(e) => write!(f, "{}", i18n::t!("core.sync.error", detail = e)),
        }
    }
}

impl std::error::Error for SyncError {}

impl From<StoreError> for SyncError {
    fn from(e: StoreError) -> Self {
        SyncError::Store(e)
    }
}

impl From<RegError> for SyncError {
    fn from(e: RegError) -> Self {
        SyncError::Registry(e)
    }
}

pub type Result<T> = std::result::Result<T, SyncError>;

/// This map's id in refs, links and Today events: random, made once, kept in the file (so it
/// survives moving or syncing the file).
pub fn map_uid(store: &mut Store) -> std::result::Result<String, StoreError> {
    if let Some(u) = store.meta(UID_KEY)? {
        return Ok(u);
    }
    let u = Uuid::new_v4().simple().to_string()[..8].to_string();
    store.set_meta(UID_KEY, &u)?;
    Ok(u)
}

/// The uid without creating it (read-only opens).
pub fn known_map_uid(store: &Store) -> Option<String> {
    store.meta(UID_KEY).ok().flatten()
}

pub fn device_ref(id: i64, uid: &str) -> String {
    format!("device-{id}@{uid}")
}

pub fn site_ref(name: &str, uid: &str) -> String {
    format!("site-{}@{uid}", fold(name).replace(' ', "-"))
}

/// The device id in one of this map's refs.
pub fn parse_device_ref(r: &str, uid: &str) -> Option<i64> {
    let (id, u) = r.strip_prefix("device-")?.split_once('@')?;
    if u == uid { id.parse().ok() } else { None }
}

fn push_unique(v: &mut Vec<String>, s: String) {
    if !s.is_empty() && !v.contains(&s) {
        v.push(s);
    }
}

pub fn device_card(d: &Device, uid: &str, owner: Option<Uuid>) -> Card {
    let mut c = Card::new(CardKind::Device, d.name.trim()).with_ref(APP, device_ref(d.id, uid));
    let mut dev = card::Device { vendor: d.vendor.clone(), model: d.model.clone(), os: d.os.clone(), ..Default::default() };
    for i in &d.idents {
        match i.kind {
            IdentKind::Mac => {
                if let Some(m) = card::normalize_mac(&i.value) {
                    push_unique(&mut dev.macs, m);
                }
            }
            IdentKind::Mdns | IdentKind::Host => push_unique(&mut dev.hostnames, i.value.trim_end_matches('.').to_lowercase()),
            IdentKind::TailscaleNode => dev.tailscale_id = Some(i.value.clone()),
            IdentKind::AwsInstance | IdentKind::WgPeer => {}
        }
    }
    if let Some(h) = &d.hostname {
        push_unique(&mut dev.hostnames, h.trim().trim_end_matches('.').to_lowercase());
    }
    for a in &d.addrs {
        push_unique(&mut dev.ips, a.ip.to_string());
    }
    dev.hostnames.sort();
    dev.ips.sort();
    c.device = Some(dev);
    c.notes = (!d.notes.trim().is_empty()).then(|| d.notes.trim().to_string());
    c.tags = d.tags.clone();
    c.ext.insert(APP.into(), json!({"type": d.kind.id(), "owner": d.owner, "ownerEntity": owner, "location": d.location}));
    c
}

pub fn site_card(name: &str, uid: &str) -> Card {
    let mut c = Card::new(CardKind::Site, name.trim()).with_ref(APP, site_ref(name, uid));
    c.site = Some(card::Site::default());
    c
}

/// The person or company of the registry an owner text names (by name or alias, ignoring case
/// and accents), if exactly one does.
pub fn owner_card(reg: &Registry, owner: &str) -> Option<Card> {
    let q = fold(owner);
    if q.is_empty() {
        return None;
    }
    let hits: Vec<Card> = reg.search(owner, None, 20).ok()?.into_iter().filter(|c| matches!(c.kind, CardKind::Person | CardKind::Company)).filter(|c| fold(&c.name) == q || c.aliases.iter().any(|a| fold(a) == q)).collect();
    match hits.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// Syncs the map with the registry at `root` when the user enabled it; `Ok(None)` when not.
pub fn sync_shared(store: &mut Store, data: &MapData, root: &Root) -> Result<Option<Report>> {
    match Registry::open_shared(root) {
        Ok(mut reg) => sync(store, data, &mut reg).map(Some),
        Err(RegError::Disabled) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn sync(store: &mut Store, data: &MapData, reg: &mut Registry) -> Result<Report> {
    let uid = map_uid(store)?;
    let mut rep = Report::default();
    // pull first: a merge or delete made elsewhere decides where (or whether) we push
    loop {
        let batch = reg.pull(APP, BATCH)?;
        for s in &batch.items {
            match (s.change.op, s.change.into) {
                (Op::Merge, Some(into)) => rep.repointed += store.repoint_shared(&s.change.id.to_string(), &into.to_string())?,
                (Op::Delete, _) => rep.repointed += store.repoint_shared(&s.change.id.to_string(), DETACHED)?,
                _ => {}
            }
        }
        reg.ack(APP, batch.cursor)?;
        if !batch.more {
            break;
        }
    }
    let links = store.shared_links()?;
    let link = |kind: &str, local: &str| links.iter().find(|l| l.kind == kind && l.local == local);
    let mut wanted: Vec<(&str, String, Card)> = Vec::new();
    let mut sites: Vec<String> = Vec::new();
    for d in data.devices.iter().filter(|d| d.saved && !d.name.trim().is_empty()) {
        let owner = owner_card(reg, &d.owner).map(|c| c.id);
        wanted.push((DEVICE, d.id.to_string(), device_card(d, &uid, owner)));
        let loc = d.location.trim();
        if !loc.is_empty() && !sites.iter().any(|s| fold(s) == fold(loc)) {
            sites.push(loc.to_string());
        }
    }
    for s in &sites {
        wanted.push((SITE, s.clone(), site_card(s, &uid)));
    }
    for (kind, local, mut c) in wanted {
        let json = c.to_json();
        let prev = link(kind, &local);
        if prev.is_some_and(|l| l.shared_id == DETACHED || l.card == json) {
            continue;
        }
        let own_ref = c.refs.get(APP).cloned();
        if let Some(id) = prev.and_then(|l| Uuid::parse_str(&l.shared_id).ok()) {
            c.id = id;
            if reg.get(id)?.and_then(|now| now.refs.get(APP).cloned()).is_some_and(|r| Some(&r) != own_ref.as_ref()) {
                c.refs.remove(APP);
            }
        }
        match reg.upsert(APP, c) {
            Ok(up) => {
                store.set_shared_link(kind, &local, &up.id.to_string(), &json)?;
                rep.pushed += 1;
            }
            Err(e) => rep.errors.push(format!("{local}: {e}")), // i18n-ok: name: error
        }
    }
    // devices deleted or taken out of the inventory, places nobody is at any more
    for l in &links {
        let alive = match l.kind.as_str() {
            DEVICE => data.devices.iter().any(|d| d.saved && d.id.to_string() == l.local),
            _ => sites.contains(&l.local),
        };
        if alive {
            continue;
        }
        if let Ok(id) = Uuid::parse_str(&l.shared_id)
            && let Some(c) = reg.get(id)?
        {
            let mine = if l.kind == DEVICE { device_ref(l.local.parse().unwrap_or(-1), &uid) } else { site_ref(&l.local, &uid) };
            if c.refs.get(APP) == Some(&mine) {
                reg.unlink(APP, c.id)?;
            }
        }
        store.delete_shared_link(&l.kind, &l.local)?;
        rep.unlinked += 1;
    }
    Ok(rep)
}

/// The device of this map a shared card is, following merges.
pub fn device_for_card(store: &Store, reg: &Registry, id: Uuid) -> Option<i64> {
    let live = reg.resolve(id).ok().flatten().unwrap_or(id);
    let links = store.shared_links().ok()?;
    if let Some(l) = links.iter().find(|l| l.kind == DEVICE && (l.shared_id == live.to_string() || l.shared_id == id.to_string())) {
        return l.local.parse().ok();
    }
    let uid = known_map_uid(store)?;
    reg.get(live).ok().flatten()?.refs.get(APP).and_then(|r| parse_device_ref(r, &uid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Addr, Ident, Layer, Source};

    #[test]
    fn device_cards() {
        let d = Device {
            id: 4,
            name: "NAS".into(),
            saved: true,
            vendor: Some("Synology".into()),
            hostname: Some("nas.local.".into()),
            idents: vec![Ident::new(IdentKind::Mac, "AA-BB-CC-00-11-22"), Ident::new(IdentKind::TailscaleNode, "nABC123"), Ident::new(IdentKind::Mdns, "NAS.local.")],
            addrs: vec![Addr { ip: "192.168.1.20".parse().unwrap(), layer: Layer::Lan, network: None, source: Source::Arp, last_seen: 0 }],
            ..Default::default()
        };
        let mut c = device_card(&d, "u1", None);
        c.normalize().unwrap();
        let dev = c.device.clone().unwrap();
        assert_eq!(dev.macs, vec!["aa:bb:cc:00:11:22"]);
        assert_eq!(dev.hostnames, vec!["nas.local"]);
        assert_eq!(dev.ips, vec!["192.168.1.20"]);
        assert_eq!(c.natural_keys(), vec!["mac:aa:bb:cc:00:11:22".to_string(), "tailscale:nABC123".to_string()]);
        assert_eq!(parse_device_ref(&c.refs["nodo"], "u1"), Some(4));
        assert_eq!(parse_device_ref(&c.refs["nodo"], "u2"), None);
        assert_eq!(site_ref("Oficina Bogotá", "u1"), "site-oficina-bogota@u1");
    }
}
