//! Where the map file lives: Save As / Move / Rename / Show in Finder, the recent maps' menu
//! (missing files, Locate…), and the cloud-sync safety net (lock file with heartbeat, "open
//! on another computer" notice, conflict copies). File work is in `nodo_core::mapfile`.
use crate::app::NodoApp;
use crate::dialogs::Dialog;
use crate::theme::Tokens;
use crate::ui;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::InputState;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::IconName;
use nodo_core::mapfile::{self, Cloud, LockInfo, LockState, MapLock};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Default)]
pub struct FileState {
    pub cloud: Option<Cloud>,
    pub lock: Option<MapLock>,
    /// Another computer had the map open when it was opened (or took it over since).
    pub elsewhere: Option<LockInfo>,
    pub conflicts: Vec<PathBuf>,
    /// The conflict notice was dismissed for these copies.
    pub conflicts_seen: Vec<PathBuf>,
    beat_gen: u64,
}

/// What happens to the original after the copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relocate {
    /// Save As: keep it.
    Copy,
    /// Move: ask, then delete it.
    Move,
    /// Rename: delete it right away (the user just asked for exactly that).
    Rename,
}

/// "Show in Finder" / "Show in Explorer" / "Show in folder".
pub fn reveal_label() -> String {
    if cfg!(target_os = "macos") {
        i18n::t!("app.file.reveal")
    } else if cfg!(windows) {
        i18n::t!("app.file.reveal_win")
    } else {
        i18n::t!("app.file.reveal_linux")
    }
}

fn host() -> String {
    nodo_core::discover::hostname().unwrap_or_default()
}

fn with_ext(path: PathBuf) -> PathBuf {
    if path.extension().is_none() { path.with_extension(nodo_core::db::EXTENSION) } else { path }
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

impl NodoApp {
    /// Where to offer a new or moved map: the open map's folder, else the last used one.
    pub fn save_dir(&self) -> PathBuf {
        if let Some(d) = self.doc.as_ref().and_then(|d| d.store.path().parent()).filter(|d| d.is_dir() && !nodo_core::paths::is_temp(d)) {
            return d.to_path_buf();
        }
        nodo_core::paths::default_dir(&self.recent, crate::app::documents_dir())
    }

    /// After a map is opened (or reopened from a new place): cloud folder, lock, conflicts.
    pub fn file_opened(&mut self, cx: &mut Context<Self>) {
        self.file.lock = None;
        self.file.elsewhere = None;
        let Some(path) = self.doc.as_ref().map(|d| d.store.path().to_path_buf()) else { return };
        self.file.cloud = mapfile::cloud_of(&path);
        if self.file.cloud.is_none() {
            self.file.conflicts.clear();
            return;
        }
        let now = nodo_core::time::now();
        match mapfile::assess(mapfile::read_lock(&path).as_ref(), &host(), now) {
            LockState::Elsewhere(l) => self.file.elsewhere = Some(l),
            _ => self.file.lock = MapLock::take(&path, &host(), now).ok(),
        }
        self.file.conflicts = mapfile::conflict_copies(&path);
        self.file.beat_gen += 1;
        let generation = self.file.beat_gen;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(mapfile::HEARTBEAT_SECS)).await;
                let go = this.update(cx, |this, cx| {
                    if this.file.beat_gen != generation || this.doc.is_none() {
                        return false;
                    }
                    this.heartbeat(cx);
                    true
                });
                if !matches!(go, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    fn heartbeat(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.doc.as_ref().map(|d| d.store.path().to_path_buf()) else { return };
        let now = nodo_core::time::now();
        if let Some(lock) = self.file.lock.as_mut()
            && let Ok(LockState::Elsewhere(l)) = lock.beat(now)
        {
            self.file.elsewhere = Some(l);
            cx.notify();
        }
        if self.file.lock.is_none()
            && let Some(l) = &self.file.elsewhere
            && !matches!(mapfile::assess(mapfile::read_lock(&path).as_ref(), &host(), now), LockState::Elsewhere(ref x) if x.host == l.host)
        {
            // the other computer closed it: take it
            self.file.elsewhere = None;
            self.file.lock = MapLock::take(&path, &host(), now).ok();
            cx.notify();
        }
        let c = mapfile::conflict_copies(&path);
        if c != self.file.conflicts {
            self.file.conflicts = c;
            cx.notify();
        }
    }

    pub fn take_over_lock(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.doc.as_ref().map(|d| d.store.path().to_path_buf()) else { return };
        self.file.elsewhere = None;
        self.file.lock = MapLock::take(&path, &host(), nodo_core::time::now()).ok();
        cx.notify();
    }

    /// Save As (`Copy`), Move (`Move`): pick the new place with the system dialog.
    pub fn relocate_dialog(&mut self, how: Relocate, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.doc.as_ref() else { return };
        let name = name_of(doc.store.path());
        let rx = cx.prompt_for_new_path(&self.save_dir(), Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = rx.await {
                this.update_in(cx, |this, window, cx| this.relocate(with_ext(path), how, window, cx)).ok();
            }
        })
        .detach();
    }

    /// Copy the open map to `dest` (checked) and carry on from there.
    pub fn relocate(&mut self, dest: PathBuf, how: Relocate, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.doc.as_mut() else { return };
        // our lock goes before the old file can be deleted, and comes back on the new one
        let lock = self.file.lock.take();
        match doc.save_as(&dest) {
            Ok(old) => {
                drop(lock);
                match how {
                    Relocate::Copy => {
                        nodo_core::paths::add_recent(&dest);
                        self.toast(i18n::t!("app.file.saved_as", path = dest.display()), cx);
                    }
                    Relocate::Move => {
                        nodo_core::paths::replace_recent(&old, &dest);
                        self.dialog = Some(Dialog::ConfirmMove { old, new: dest.clone() });
                    }
                    Relocate::Rename => {
                        nodo_core::paths::replace_recent(&old, &dest);
                        match mapfile::remove_map(&old) {
                            Ok(()) => self.toast(i18n::t!("app.file.renamed", name = name_of(&dest)), cx),
                            Err(e) => self.toast(i18n::t!("app.file.old_kept", path = old.display(), detail = e), cx),
                        }
                    }
                }
                self.recent = nodo_core::paths::recent_entries();
                crate::set_app_menus(cx);
                if let Some(d) = &self.doc {
                    window.set_window_title(&format!("{} — {}", d.title(), crate::APP_NAME));
                }
                self.file_opened(cx);
            }
            Err(e) => {
                self.file.lock = lock;
                self.toast(i18n::t!("app.file.copy_failed", detail = e), cx);
            }
        }
        cx.notify();
    }

    /// The copy was checked and reopened; now the original goes (Move's confirmation).
    pub fn confirm_move(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::ConfirmMove { old, new }) = self.dialog.take() else { return };
        match mapfile::remove_map(&old) {
            Ok(()) => self.toast(i18n::t!("app.file.moved", path = new.display()), cx),
            Err(e) => self.toast(i18n::t!("app.file.old_kept", path = old.display(), detail = e), cx),
        }
        self.recent = nodo_core::paths::recent_entries();
        crate::set_app_menus(cx);
        cx.notify();
    }

    pub fn open_rename_map(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.doc.as_ref() else { return };
        let stem = doc.title();
        let name = cx.new(|cx| InputState::new(window, cx).default_value(stem));
        self.dialog = Some(Dialog::RenameMap { name });
        cx.notify();
    }

    pub fn submit_rename_map(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Dialog::RenameMap { name }) = self.dialog.take() else { return };
        let v = name.read(cx).value().trim().to_string();
        let Some(cur) = self.doc.as_ref().map(|d| d.store.path().to_path_buf()) else { return };
        if v.is_empty() || v.contains(['/', '\\', ':']) || cur.file_stem().is_some_and(|s| s.to_string_lossy() == v) {
            cx.notify();
            return;
        }
        let dest = cur.with_file_name(format!("{v}.{}", nodo_core::db::EXTENSION));
        if dest.exists() {
            self.toast(i18n::t!("app.file.exists", name = name_of(&dest)), cx);
            return;
        }
        self.relocate(dest, Relocate::Rename, window, cx);
    }

    pub fn reveal_map(&mut self, _cx: &mut Context<Self>) {
        if let Some(d) = &self.doc {
            crate::util::reveal(d.store.path());
        }
    }

    // ── recent maps ──────────────────────────────────────────────────────────────────────────

    pub fn forget_recent(&mut self, path: &Path, cx: &mut Context<Self>) {
        nodo_core::paths::remove_recent(path);
        self.recent = nodo_core::paths::recent_entries();
        crate::set_app_menus(cx);
        cx.notify();
    }

    /// A recent map that was moved or deleted: find it, keep its place in the list, open it.
    pub fn locate_recent(&mut self, old: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some(i18n::t!("app.recent.locate_prompt", name = name_of(&old)).into()) });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(p) = paths.into_iter().next()
            {
                this.update_in(cx, |this, window, cx| {
                    nodo_core::paths::replace_recent(&old, &p);
                    this.recent = nodo_core::paths::recent_entries();
                    this.open_path(p, window, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// Move a map that isn't open (from the recent list): copy + check, then confirm deleting
    /// the original.
    pub fn move_recent(&mut self, from: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.doc.as_ref().is_some_and(|d| d.store.path() == from) {
            self.relocate_dialog(Relocate::Move, window, cx);
            return;
        }
        if let LockState::Elsewhere(l) = mapfile::assess(mapfile::read_lock(&from).as_ref(), &host(), nodo_core::time::now()) {
            self.toast(i18n::t!("app.file.open_elsewhere_short", host = l.host), cx);
            return;
        }
        let rx = cx.prompt_for_new_path(&self.save_dir(), Some(&name_of(&from)));
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(to))) = rx.await {
                let to = with_ext(to);
                this.update(cx, |this, cx| {
                    match mapfile::copy_file(&from, &to).and_then(|()| nodo_core::db::Store::verify(&to).map_err(Into::into)) {
                        Ok(()) => {
                            nodo_core::paths::replace_recent(&from, &to);
                            this.recent = nodo_core::paths::recent_entries();
                            crate::set_app_menus(cx);
                            this.dialog = Some(Dialog::ConfirmMove { old: from.clone(), new: to });
                        }
                        Err(e) => this.toast(i18n::t!("app.file.copy_failed", detail = e), cx),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    pub fn recent_menu(&self, path: PathBuf, exists: bool, cx: &mut Context<Self>) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let weak = cx.entity().downgrade();
        move |menu, _, _| {
            let item = |label: String, enabled: bool, f: fn(&mut NodoApp, PathBuf, &mut Window, &mut Context<NodoApp>)| {
                let (w, p) = (weak.clone(), path.clone());
                PopupMenuItem::new(label).disabled(!enabled).on_click(move |_, window, cx| {
                    w.update(cx, |this, cx| f(this, p.clone(), window, cx)).ok();
                })
            };
            let menu = if exists { menu.item(item(i18n::t!("app.file.open"), true, |this, p, window, cx| this.open_path(p, window, cx))) } else { menu.item(item(i18n::t!("app.recent.locate"), true, |this, p, window, cx| this.locate_recent(p, window, cx))) };
            menu.item(item(crate::mapfile_ui::reveal_label(), exists, |_, p, _, _| crate::util::reveal(&p)))
                .item(item(i18n::t!("app.recent.move"), exists, |this, p, window, cx| this.move_recent(p, window, cx)))
                .separator()
                .item(item(i18n::t!("app.recent.remove"), true, |this, p, _, cx| this.forget_recent(&p, cx)))
        }
    }

    // ── notices ──────────────────────────────────────────────────────────────────────────────

    /// "Open on another computer" and "conflict copies" bars above the map.
    pub fn render_file_notices(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let now = nodo_core::time::now();
        let bar = |id: &'static str, text: String| {
            div()
                .id(id)
                .flex()
                .flex_none()
                .items_center()
                .gap(px(10.))
                .px(px(12.))
                .py(px(8.))
                .border_b_1()
                .border_color(t.border)
                .bg(t.tag.opacity(0.12))
                .child(ui::icon(IconName::TriangleAlert, 15., t.tag))
                .child(div().flex_1().min_w_0().text_size(px(12.5)).line_height(px(18.)).child(text))
        };
        let small = |id: &'static str, label: String| ui::outline_button(t, id).h(px(28.)).text_size(px(12.)).child(label);
        let mut col = div().flex().flex_col().flex_none();
        let mut any = false;
        if let Some(l) = &self.file.elsewhere {
            any = true;
            col = col.child(
                bar("notice-lock", i18n::t!("app.file.open_elsewhere", host = l.host.clone(), ago = crate::util::ago(l.heartbeat, now)))
                    .child(small("notice-lock-take", i18n::t!("app.file.take_over")).on_click(cx.listener(|this, _, _, cx| this.take_over_lock(cx)))),
            );
        }
        let fresh: Vec<PathBuf> = self.file.conflicts.iter().filter(|p| !self.file.conflicts_seen.contains(p)).cloned().collect();
        if !fresh.is_empty() {
            any = true;
            let cloud = self.file.cloud.map(Cloud::name).unwrap_or_default();
            col = col.child(
                bar("notice-conflicts", i18n::tn!("app.file.conflicts", fresh.len(), cloud = cloud))
                    .child(small("notice-conflicts-review", i18n::t!("app.file.review")).on_click(cx.listener(|this, _, _, cx| {
                        this.dialog = Some(Dialog::Conflicts(this.file.conflicts.clone()));
                        cx.notify();
                    })))
                    .child(small("notice-conflicts-hide", i18n::t!("app.file.dismiss")).on_click(cx.listener(|this, _, _, cx| {
                        this.file.conflicts_seen = this.file.conflicts.clone();
                        cx.notify();
                    }))),
            );
        }
        any.then(|| col.into_any_element())
    }

    /// Footer chip "In OneDrive" while the map sits in a synced folder.
    pub fn render_cloud_footer(&self) -> Option<impl IntoElement> {
        let c = self.file.cloud?;
        Some(div().id("footer-cloud").flex_none().child(i18n::t!("app.file.in_cloud", cloud = c.name())).tooltip(|w, cx| gpui_component::tooltip::Tooltip::new(i18n::t!("app.file.cloud_tip")).build(w, cx)))
    }

    /// The conflict-copies dialog's body: each copy with Open / Show in Finder.
    pub fn conflicts_body(&self, list: &[PathBuf], t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let mut b = div().flex().flex_col().gap(px(8.)).child(div().text_size(px(13.)).line_height(px(19.)).text_color(t.ink_muted).child(i18n::t!("app.file.conflicts_body")));
        for (i, p) in list.iter().enumerate() {
            let (p1, p2) = (p.clone(), p.clone());
            let modified = std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| crate::util::local_time(d.as_secs() as i64)).unwrap_or_default();
            b = b.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex().flex_col().flex_1().min_w_0().child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).truncate().child(name_of(p))).child(div().text_size(px(11.)).text_color(t.ink_muted).child(modified)))
                    .child(ui::outline_button(t, ("conflict-open", i)).h(px(28.)).text_size(px(12.)).child(i18n::t!("app.file.open_copy")).on_click(cx.listener(move |_, _, _, cx| crate::open_window(Some(p1.clone()), false, cx))))
                    .child(ui::outline_button(t, ("conflict-reveal", i)).h(px(28.)).text_size(px(12.)).child(crate::mapfile_ui::reveal_label()).on_click(move |_, _, _| crate::util::reveal(&p2))),
            );
        }
        b.when(list.is_empty(), |b| b.child(div().text_size(px(13.)).child(i18n::t!("app.file.no_conflicts")))).into_any_element()
    }
}
