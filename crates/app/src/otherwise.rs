//! Nodo in the Otherwise apps (otherwise-link): deep links in, the Today feed out, the shared
//! entity registry (opt-in), and links into Vigía.
//!
//! Several windows can each have a map open, but Nodo has one Today file: every window keeps
//! its map's events in [`FEEDS`] and the union is written when it changes. A window's entry
//! goes away with the window ([`FeedSlot`]'s `Drop`) or when its map closes.
use crate::app::{NodoApp, Sel};
use gpui::*;
use nodo_core::feed::{self, DeviceDown, ServiceStatus};
use nodo_core::links::Route;
use otherwise_link::today::Event;
use otherwise_link::{Link, Root};
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

/// Links from the OS (macOS Apple events arrive without an `App`, maybe before a window).
pub static INBOUND: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[derive(Default)]
struct Feeds {
    by_map: BTreeMap<String, Vec<Event>>,
    written: String,
}

static FEEDS: Mutex<Feeds> = Mutex::new(Feeds { by_map: BTreeMap::new(), written: String::new() });

fn publish_all(f: &mut Feeds) {
    let root = Root::system();
    if !crate::settings::Settings::load().today_feed() {
        if f.written != "off" && otherwise_link::today::clear(&root, nodo_core::sync::APP).is_ok() {
            f.written = "off".into();
        }
        return;
    }
    let all: Vec<Event> = f.by_map.values().flatten().cloned().collect();
    let sig = serde_json::to_string(&all).unwrap_or_default();
    if sig != f.written && otherwise_link::today::publish(&root, nodo_core::sync::APP, &all).is_ok() {
        f.written = sig;
    }
}

fn set_feed(uid: &str, events: Option<Vec<Event>>) {
    let mut f = FEEDS.lock().unwrap_or_else(|e| e.into_inner());
    match events {
        Some(v) => {
            f.by_map.insert(uid.to_string(), v);
        }
        None => {
            f.by_map.remove(uid);
        }
    }
    publish_all(&mut f);
}

/// A window's place in the Today feed; leaving it (map closed, window gone) removes its events.
#[derive(Default)]
pub struct FeedSlot(Option<String>);

impl FeedSlot {
    pub fn clear(&mut self) {
        if let Some(uid) = self.0.take() {
            set_feed(&uid, None);
        }
    }
}

impl Drop for FeedSlot {
    fn drop(&mut self) {
        self.clear();
    }
}

#[derive(Default)]
pub struct LinkState {
    /// The open map's uid (links, refs, Today ids).
    pub uid: Option<String>,
    pub feed: FeedSlot,
    ticks: u64,
    synced_generation: Option<u64>,
    syncing: bool,
    /// Owner text → (kind, name) of the registry's matching person / company.
    pub owners: HashMap<String, (otherwise_link::Kind, String)>,
    /// Certificate dates already written to the map file, per service.
    certs: HashMap<i64, i64>,
}

/// The user's own copy of a map that has `uid`, among the recent ones.
pub fn recent_map_with_uid(uid: &str) -> Option<std::path::PathBuf> {
    nodo_core::paths::recent().into_iter().find(|p| nodo_core::db::Store::open_read_only(p).ok().and_then(|s| nodo_core::sync::known_map_uid(&s)).as_deref() == Some(uid))
}

/// Makes the OS send `otherwise-nodo://` links here (see Vigía's twin): Windows once per
/// executable path, Linux once the menu entry is installed; macOS through Info.plist.
pub fn register_scheme() {
    use nodo_core::links::{DESKTOP_FILE, SCHEME};
    let Ok(exe) = std::env::current_exe() else { return };
    let exe_s = exe.display().to_string();
    if crate::settings::Settings::load().link_registered.as_deref() == Some(exe_s.as_str()) {
        return;
    }
    let done = if cfg!(windows) {
        otherwise_link::register::register_windows(SCHEME, &exe, crate::APP_NAME).is_ok()
    } else if cfg!(target_os = "linux") {
        let installed = otherwise_link::paths::home().join(".local/share/applications").join(DESKTOP_FILE);
        installed.exists() && otherwise_link::register::register_linux(DESKTOP_FILE, &[SCHEME]).is_ok()
    } else {
        false
    };
    if done {
        crate::settings::Settings::update(|s| s.link_registered = Some(exe_s));
    }
}

/// Opens `l` in Nodo's own ways; the launcher handles `otherwise://` when Vigía is missing.
pub fn open_external(l: &Link) -> Result<(), String> {
    otherwise_link::open::open_link(l).map(|_| ()).map_err(|e| e.to_string())
}

impl NodoApp {
    /// The open map's uid, made on first use (one `meta` row in the file).
    pub fn map_uid(&mut self) -> Option<String> {
        if self.link.uid.is_none()
            && let Some(doc) = self.doc.as_mut()
        {
            self.link.uid = nodo_core::sync::map_uid(&mut doc.store).ok();
        }
        self.link.uid.clone()
    }

    /// The map was closed (or replaced): its Today events go, its uid is forgotten.
    pub fn leave_map(&mut self) {
        self.link.feed.clear();
        self.link.uid = None;
        self.link.synced_generation = None;
        self.link.owners.clear();
        self.link.certs.clear();
    }

    /// Once a second while the map is watched: the Today feed every few seconds, certificate
    /// dates into the file when they change, the registry after edits and every 5 minutes.
    pub fn integration_tick(&mut self, cx: &mut Context<Self>) {
        self.link.ticks += 1;
        if self.link.ticks % 5 == 1 {
            self.publish_today();
        }
        let edited = self.link.synced_generation != Some(self.generation);
        if (edited && self.link.ticks.is_multiple_of(10)) || self.link.ticks.is_multiple_of(300) {
            self.sync_registry(cx);
        }
    }

    fn live_status(&mut self) -> (Vec<DeviceDown>, Vec<ServiceStatus>) {
        let Some(doc) = self.doc.as_mut() else { return (vec![], vec![]) };
        let down = doc
            .data
            .devices
            .iter()
            .filter(|d| d.saved)
            .filter_map(|d| self.mon.states.get(&d.id).filter(|s| s.health == nodo_core::monitor::Health::Down).map(|s| DeviceDown { id: d.id, name: d.name.clone(), since: s.since }))
            .collect();
        let mut services = Vec::new();
        for c in &self.svc.list {
            let st = self.svc.states.get(&c.id);
            let last = st.and_then(|s| s.last.as_ref());
            let cert_until = match (last.and_then(|o| o.cert_days), st.and_then(|s| s.last_at)) {
                (Some(days), Some(at)) => Some(at + days * 86_400),
                _ => doc.store.meta(&format!("{}{}", feed::CERT_META, c.id)).ok().flatten().and_then(|v| v.parse().ok()),
            };
            if let Some(until) = cert_until
                && self.link.certs.get(&c.id).is_none_or(|prev| (prev - until).abs() >= 86_400)
            {
                let _ = doc.store.set_meta(&format!("{}{}", feed::CERT_META, c.id), &until.to_string());
                self.link.certs.insert(c.id, until);
            }
            let down = st.filter(|s| s.health == nodo_core::services::Health::Down);
            services.push(ServiceStatus {
                check: c.clone(),
                down_since: down.map(|s| s.since),
                detail: down.and_then(|s| s.last.as_ref()).map(|o| o.detail.clone()).unwrap_or_default(),
                slow_ms: last.filter(|o| o.ok && o.slow).and_then(|o| o.latency_ms),
                cert_until,
            });
        }
        (down, services)
    }

    pub fn publish_today(&mut self) {
        let Some(uid) = self.map_uid() else { return };
        let (down, services) = self.live_status();
        let events = feed::events(&uid, &down, &services, nodo_core::time::now());
        self.link.feed.0 = Some(uid.clone());
        set_feed(&uid, Some(events));
    }

    /// Publishes the map's saved devices and sites to the shared registry in the background
    /// (nothing happens unless the user turned it on in Otherwise Apps), and learns which
    /// owners are people or companies there.
    pub fn sync_registry(&mut self, cx: &mut Context<Self>) {
        if self.link.syncing {
            return;
        }
        let Some(doc) = self.doc.as_ref() else { return };
        let path = doc.store.path().to_path_buf();
        let data = doc.data.clone();
        self.link.syncing = true;
        self.link.synced_generation = Some(self.generation);
        let task = cx.background_executor().spawn(async move {
            let mut reg = match otherwise_link::registry::Registry::open_shared(&Root::system()) {
                Ok(r) => r,
                Err(_) => return None,
            };
            let mut store = nodo_core::db::Store::open(&path).ok()?;
            let r = nodo_core::sync::sync(&mut store, &data, &mut reg);
            let mut owners = HashMap::new();
            for d in &data.devices {
                if !d.owner.trim().is_empty()
                    && !owners.contains_key(&d.owner)
                    && let Some(c) = nodo_core::sync::owner_card(&reg, &d.owner)
                {
                    owners.insert(d.owner.clone(), (c.kind, c.name));
                }
            }
            Some((r.map_err(|e| e.to_string()), owners))
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            this.update(cx, |this, cx| {
                this.link.syncing = false;
                if let Some((res, owners)) = r {
                    if let Err(e) = res {
                        eprintln!("{e}");
                    }
                    this.link.owners = owners;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The registry's person / company an owner text names, as "Company" / "Person".
    pub fn owner_label(&self, owner: &str) -> Option<String> {
        self.link.owners.get(owner).map(|(k, _)| match k {
            otherwise_link::Kind::Company => i18n::t!("app.link.owner_company"),
            _ => i18n::t!("app.link.owner_person"),
        })
    }

    /// The map this window shows has `uid`.
    pub fn has_map(&mut self, uid: &str) -> bool {
        self.map_uid().as_deref() == Some(uid)
    }

    /// A deep link (`otherwise://nodo/…`, from another app or the Today view).
    pub fn open_link(&mut self, l: &Link, window: &mut Window, cx: &mut Context<Self>) {
        window.activate_window();
        match nodo_core::links::route(l) {
            Route::Focus => {}
            Route::Device { id, .. } => {
                if self.doc.as_ref().is_some_and(|d| d.data.device(id).is_some()) {
                    self.view.status = false;
                    self.select_device(id, cx);
                } else {
                    self.toast(i18n::t!("app.link.not_found"), cx);
                }
            }
            Route::Service { id, .. } => {
                if self.svc.list.iter().any(|s| s.id == id) {
                    self.view.status = true;
                    let dev = self.svc.list.iter().find(|s| s.id == id).and_then(|s| s.device);
                    if let Some(d) = dev {
                        self.selected = Some(Sel::Device(d));
                    }
                    cx.notify();
                } else {
                    self.toast(i18n::t!("app.link.not_found"), cx);
                }
            }
            Route::Shared(uuid) => {
                let found = self.doc.as_ref().and_then(|doc| {
                    let reg = otherwise_link::registry::Registry::open_shared(&Root::system()).ok()?;
                    nodo_core::sync::device_for_card(&doc.store, &reg, uuid)
                });
                match found {
                    Some(id) => {
                        self.view.status = false;
                        self.select_device(id, cx);
                    }
                    None => self.toast(i18n::t!("app.link.not_found"), cx),
                }
            }
            Route::Unknown => self.toast(i18n::t!("app.link.unknown", url = l.to_url()), cx),
        }
    }

    /// Vigía's new-item form, prefilled with this device's warranty.
    pub fn warranty_in_vigia(&mut self, id: nodo_core::model::DeviceId, cx: &mut Context<Self>) {
        let Some(d) = self.doc.as_ref().and_then(|doc| doc.data.device(id)).cloned() else { return };
        let name = i18n::t!("app.link.warranty_name", name = d.name);
        self.open_in_vigia(nodo_core::links::vigia_warranty(&d, &name), cx);
    }

    /// Vigía's new-item form, prefilled with an HTTPS service's certificate.
    pub fn certificate_in_vigia(&mut self, id: nodo_core::services::ServiceId, cx: &mut Context<Self>) {
        let Some(c) = self.svc.list.iter().find(|s| s.id == id).cloned() else { return };
        let Some((host, _)) = nodo_core::links::tls_host(&c) else { return };
        let st = self.svc.states.get(&id);
        let expires = match (st.and_then(|s| s.last.as_ref()).and_then(|o| o.cert_days), st.and_then(|s| s.last_at)) {
            (Some(days), Some(at)) => {
                let (y, m, d) = nodo_core::time::civil((at + days * 86_400).div_euclid(86_400));
                Some(format!("{y:04}-{m:02}-{d:02}"))
            }
            _ => None,
        };
        if let Some(l) = nodo_core::links::vigia_certificate(&c, expires.as_deref(), &i18n::t!("core.today.cert_item", host = host)) {
            self.open_in_vigia(l, cx);
        }
    }

    /// "Track in Vigía" / "Add warranty in Vigía".
    pub fn open_in_vigia(&mut self, l: Link, cx: &mut Context<Self>) {
        if let Err(e) = open_external(&l) {
            self.toast(i18n::t!("app.link.vigia_failed", detail = e), cx);
        }
    }
}

/// Every 250 ms: hands queued links to the window showing their map (opening the map from the
/// recent ones when no window has it), else to the first window.
pub fn pump(cx: &mut App, retry: &mut Vec<(Link, u32)>) {
    let fresh = std::mem::take(&mut *INBOUND.lock().unwrap_or_else(|e| e.into_inner()));
    retry.extend(fresh.iter().filter_map(|u| Link::parse(u).ok()).map(|l| (l, 0)));
    if retry.is_empty() {
        return;
    }
    let windows: Vec<(AnyWindowHandle, Entity<NodoApp>)> = cx.windows().into_iter().filter_map(|w| w.downcast::<gpui_component::Root>().and_then(|r| r.read(cx).ok().and_then(|root| root.view().clone().downcast::<NodoApp>().ok())).map(|v| (w, v))).collect();
    let mut keep = Vec::new();
    for (l, tries) in std::mem::take(retry) {
        let want = match nodo_core::links::route(&l) {
            Route::Device { map: Some(m), .. } | Route::Service { map: Some(m), .. } => Some(m),
            _ => None,
        };
        let any = |cx: &App| windows.iter().find(|(_, v)| v.read(cx).doc.is_some()).or(windows.first()).cloned();
        let mut target = match &want {
            Some(uid) => windows.iter().find(|(_, v)| v.update(cx, |this, _| this.has_map(uid))).cloned(),
            None => any(cx),
        };
        if target.is_none() && tries == 0 {
            // the map isn't open: open it from the recent ones and deliver on a later round
            match want.as_deref().and_then(recent_map_with_uid) {
                Some(path) => {
                    crate::open_window(Some(path), false, cx);
                    keep.push((l, 1));
                    continue;
                }
                None => target = any(cx),
            }
        }
        match target {
            Some((w, v)) => {
                let _ = w.update(cx, |_, window, cx| v.update(cx, |this, cx| this.open_link(&l, window, cx)));
            }
            None if windows.is_empty() && tries == 0 => {
                crate::open_window(None, false, cx);
                keep.push((l, 1));
            }
            None if tries < 40 => keep.push((l, tries + 1)),
            None => {}
        }
    }
    *retry = keep;
}
