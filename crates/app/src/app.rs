//! Window state: welcome screen → an open map (graph or list | side panel) → or an error.
use crate::theme::{SANS, Tokens, tokens};
use crate::ui::{self, icon};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{IconName, Sizable as _, Theme};
use nodo_core::doc::Doc;
use nodo_core::graph::{Graph, GraphOpts};
use nodo_core::layout::{Mode, Placed};
use nodo_core::model::{DeviceId, Layer, NetworkId};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(crate) const CONTEXT: &str = "NodoApp";
#[cfg(target_os = "macos")]
pub(crate) const SEARCH_KEYS: &str = "⌘F"; // i18n-ok: shortcut
#[cfg(not(target_os = "macos"))]
pub(crate) const SEARCH_KEYS: &str = "Ctrl+F"; // i18n-ok: shortcut
const TOAST_FOR: Duration = Duration::from_secs(4);
pub(crate) const RECENT_ACTIONS: usize = 10;
pub(crate) const ZOOM_MIN: f32 = 0.15;
pub(crate) const ZOOM_MAX: f32 = 2.5;

actions!(
    nodo,
    [
        NewMap,
        OpenMap,
        CloseMap,
        ClearRecent,
        ImportCsv,
        ImportWgDump,
        ExportPng,
        ExportPdf,
        ExportSvg,
        ExportMermaid,
        ExportCsv,
        ExportJson,
        ToggleList,
        LayoutRadial,
        LayoutSubnet,
        LayoutNetwork,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        FocusSearch,
        Escape,
        Refresh,
        ScanSubnet,
        DiscoverTailscale,
        DiscoverAws,
        DiscoverWireGuard,
        AddDevice,
        ToggleMonitor,
        ShowAbout,
        OpenSettings,
        OpenRecent0,
        OpenRecent1,
        OpenRecent2,
        OpenRecent3,
        OpenRecent4,
        OpenRecent5,
        OpenRecent6,
        OpenRecent7,
        OpenRecent8,
        OpenRecent9
    ]
);

pub(crate) fn recent_action(i: usize, name: String) -> MenuItem {
    match i {
        0 => MenuItem::action(name, OpenRecent0),
        1 => MenuItem::action(name, OpenRecent1),
        2 => MenuItem::action(name, OpenRecent2),
        3 => MenuItem::action(name, OpenRecent3),
        4 => MenuItem::action(name, OpenRecent4),
        5 => MenuItem::action(name, OpenRecent5),
        6 => MenuItem::action(name, OpenRecent6),
        7 => MenuItem::action(name, OpenRecent7),
        8 => MenuItem::action(name, OpenRecent8),
        _ => MenuItem::action(name, OpenRecent9),
    }
}

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-n", NewMap, Some(CONTEXT)),
        KeyBinding::new("secondary-o", OpenMap, Some(CONTEXT)),
        KeyBinding::new("secondary-f", FocusSearch, Some(CONTEXT)),
        KeyBinding::new("escape", Escape, Some(CONTEXT)),
        KeyBinding::new("secondary-r", Refresh, Some(CONTEXT)),
        KeyBinding::new("secondary-l", ToggleList, Some(CONTEXT)),
        KeyBinding::new("secondary-1", LayoutRadial, Some(CONTEXT)),
        KeyBinding::new("secondary-2", LayoutSubnet, Some(CONTEXT)),
        KeyBinding::new("secondary-3", LayoutNetwork, Some(CONTEXT)),
        KeyBinding::new("secondary-=", ZoomIn, Some(CONTEXT)),
        KeyBinding::new("secondary-+", ZoomIn, Some(CONTEXT)),
        KeyBinding::new("secondary--", ZoomOut, Some(CONTEXT)),
        KeyBinding::new("secondary-0", ZoomReset, Some(CONTEXT)),
        KeyBinding::new("secondary-,", OpenSettings, Some(CONTEXT)),
    ]);
}

pub(crate) fn sync_theme(window: Option<&mut Window>, cx: &mut App) {
    Theme::sync_system_appearance(window, cx);
    Theme::global_mut(cx).font_family = SANS.into();
}

pub enum Phase {
    Welcome,
    Ready,
    Failed { path: PathBuf, message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sel {
    Device(DeviceId),
    Network(NetworkId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Name,
    Kind,
    Ip,
    Status,
    LastSeen,
    Layer,
    /// Grouped by owner (tailnet user or typed), with a header per owner.
    Owner,
}

/// How the map is shown: graph or list, layout, layers, pan / zoom.
pub struct ViewState {
    pub list: bool,
    pub mode: Mode,
    pub layers: Vec<Layer>,
    pub zoom: f32,
    pub pan: (f32, f32),
    pub drag: Option<(Point<Pixels>, (f32, f32))>,
    /// The pointer moved since the button went down: the click that follows isn't a click.
    pub moved: bool,
    pub viewport: Rc<Cell<Bounds<Pixels>>>,
    pub sort: Sort,
    pub sort_desc: bool,
    /// Pan / zoom chosen to fit the map once it first has nodes.
    pub fitted: bool,
}

impl ViewState {
    fn from_settings() -> ViewState {
        let s = crate::settings::Settings::load();
        ViewState {
            list: s.view.as_deref() == Some("list"),
            mode: Mode::from_id(s.layout.as_deref().unwrap_or_default()),
            layers: Layer::ALL.into_iter().filter(|l| !s.hidden_layers.iter().any(|h| h == l.id())).collect(),
            zoom: 1.0,
            pan: (0., 0.),
            drag: None,
            moved: false,
            viewport: Rc::new(Cell::new(Bounds::default())),
            sort: Sort::Name,
            sort_desc: false,
            fitted: false,
        }
    }

    pub fn zoom_by(&mut self, factor: f32, around: Option<Point<Pixels>>) {
        let z = (self.zoom * factor).clamp(ZOOM_MIN, ZOOM_MAX);
        let b = self.viewport.get();
        let c = b.center();
        let p = around.unwrap_or(c);
        let (px_, py_) = (f32::from(p.x - c.x), f32::from(p.y - c.y));
        let wx = (px_ - self.pan.0) / self.zoom;
        let wy = (py_ - self.pan.1) / self.zoom;
        self.pan = (px_ - wx * z, py_ - wy * z);
        self.zoom = z;
    }
}

/// The graph and its layout for the current data, layers and mode.
pub struct Cached {
    pub generation: u64,
    pub layers: Vec<Layer>,
    pub mode: Mode,
    pub graph: Graph,
    pub placed: Placed,
}

pub struct NodoApp {
    pub phase: Phase,
    pub doc: Option<Doc>,
    /// Bumped on every change to the map's data; invalidates `cache`.
    pub generation: u64,
    cache: Option<Rc<Cached>>,
    pub view: ViewState,
    pub selected: Option<Sel>,
    pub search: Entity<InputState>,
    pub disc: crate::discovery::DiscState,
    pub mon: crate::discovery::MonState,
    pub output: Option<crate::panel::Output>,
    pub dialog: Option<crate::dialogs::Dialog>,
    pub edit: Option<crate::panel::EditForm>,
    pub about: bool,
    toast: Option<(String, Instant)>,
    pub logo: Arc<Image>,
    focus: FocusHandle,
    placeholder_locale: &'static str,
    pub terminals: Rc<Vec<crate::launch::Term>>,
    detected: bool,
    pub recent: Vec<PathBuf>,
    _subs: Vec<Subscription>,
}

impl NodoApp {
    pub fn new(initial: Option<PathBuf>, new_map: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        sync_theme(Some(window), cx);
        let search = cx.new(|cx| InputState::new(window, cx));
        let subs = vec![
            window.observe_window_appearance(|window, cx| sync_theme(Some(window), cx)),
            cx.subscribe_in(&search, window, |this, _, ev: &InputEvent, _, cx| match ev {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => this.select_first_match(cx),
                _ => {}
            }),
        ];
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let mut this = NodoApp {
            phase: Phase::Welcome,
            doc: None,
            generation: 0,
            cache: None,
            view: ViewState::from_settings(),
            selected: None,
            search,
            disc: Default::default(),
            mon: Default::default(),
            output: None,
            dialog: None,
            edit: None,
            about: false,
            toast: None,
            logo: Arc::new(Image::from_bytes(ImageFormat::Png, crate::ICON_PNG.to_vec())),
            focus,
            placeholder_locale: "",
            terminals: Rc::new(Vec::new()),
            detected: false,
            recent: nodo_core::paths::recent(),
            _subs: subs,
        };
        crate::star::refresh(crate::settings::Settings::load().github_stars(), cx);
        if let Some(p) = initial {
            this.open_path(p, window, cx);
        } else if new_map {
            this.new_map_dialog(window, cx);
        }
        this
    }

    // ── files ────────────────────────────────────────────────────────────────────────────────

    pub fn new_map_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.doc.is_some() {
            crate::open_window(None, true, cx);
            return;
        }
        let dir = documents_dir();
        let name = format!("{}.{}", i18n::t!("app.file.default_name"), nodo_core::db::EXTENSION);
        let rx = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = rx.await {
                this.update_in(cx, |this, window, cx| this.create_at(path, window, cx)).ok();
            }
        })
        .detach();
    }

    pub fn create_at(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let path = if path.extension().is_none() { path.with_extension(nodo_core::db::EXTENSION) } else { path };
        match Doc::create(&path) {
            Ok(doc) => self.loaded(doc, window, cx),
            Err(e) => self.phase = Phase::Failed { path, message: e.to_string() },
        }
        cx.notify();
    }

    pub fn open_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some(i18n::t!("app.file.open_prompt").into()) });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(p) = paths.into_iter().next()
            {
                this.update_in(cx, |this, window, cx| this.open_path(p, window, cx)).ok();
            }
        })
        .detach();
    }

    /// Open a map here, or in a new window if this one already has one.
    pub fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(d) = &self.doc {
            if d.store.path() != path {
                crate::open_window(Some(path), false, cx);
            }
            return;
        }
        // `nodo-app new-map.nodo` with a file that doesn't exist yet makes it
        if !path.exists() && path.extension().is_some_and(|e| e == nodo_core::db::EXTENSION) {
            self.create_at(path, window, cx);
            return;
        }
        match Doc::open(&path) {
            Ok(doc) => self.loaded(doc, window, cx),
            Err(e) => self.phase = Phase::Failed { path, message: e.to_string() },
        }
        cx.notify();
    }

    fn loaded(&mut self, doc: Doc, window: &mut Window, cx: &mut Context<Self>) {
        nodo_core::paths::add_recent(doc.store.path());
        self.recent = nodo_core::paths::recent();
        crate::set_app_menus(cx);
        window.set_window_title(&format!("{} — {}", doc.title(), crate::APP_NAME));
        self.doc = Some(doc);
        self.phase = Phase::Ready;
        self.selected = None;
        self.view.fitted = false;
        self.generation += 1;
        self.start_monitor(cx);
        // passive discovery is harmless: run it right away, then every few minutes
        self.refresh(cx);
        self.start_auto_refresh(cx);
        self.focus.focus(window, cx);
    }

    pub fn close_map(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_monitor();
        self.doc = None;
        self.phase = Phase::Welcome;
        self.selected = None;
        self.dialog = None;
        self.edit = None;
        self.output = None;
        self.cache = None;
        self.disc = Default::default();
        self.recent = nodo_core::paths::recent();
        window.set_window_title(crate::APP_NAME);
        cx.notify();
    }

    fn open_recent(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = nodo_core::paths::recent().get(i).cloned() {
            self.open_path(p, window, cx);
        }
    }

    /// The data changed: rebuild the graph on next render.
    pub fn touch(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        cx.notify();
    }

    pub fn cached(&mut self) -> Option<Rc<Cached>> {
        let doc = self.doc.as_ref()?;
        if let Some(c) = &self.cache
            && c.generation == self.generation
            && c.layers == self.view.layers
            && c.mode == self.view.mode
        {
            return Some(c.clone());
        }
        let graph = nodo_core::graph::build(&doc.data, &GraphOpts { layers: self.view.layers.clone() });
        let placed = nodo_core::layout::layout(&graph, self.view.mode);
        let c = Rc::new(Cached { generation: self.generation, layers: self.view.layers.clone(), mode: self.view.mode, graph, placed });
        self.cache = Some(c.clone());
        Some(c)
    }

    // ── small actions ────────────────────────────────────────────────────────────────────────

    pub fn toast(&mut self, text: String, cx: &mut Context<Self>) {
        let at = Instant::now();
        self.toast = Some((text, at));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_FOR).await;
            this.update(cx, |this, cx| {
                if this.toast.as_ref().is_some_and(|(_, t)| *t == at) {
                    this.toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn copy(&mut self, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.toast(i18n::t!("app.toast.copied", text = text), cx);
    }

    pub fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().trim().to_lowercase()
    }

    fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Enter in the search field: select (and centre) the first match.
    fn select_first_match(&mut self, cx: &mut Context<Self>) {
        let q = self.query(cx);
        let Some(doc) = &self.doc else { return };
        if let Some(d) = doc.data.devices.iter().find(|d| !q.is_empty() && d.haystack().contains(&q)) {
            let id = d.id;
            self.select_device(id, cx);
            self.center_on(id);
        }
    }

    /// Pan so the device is in the middle of the canvas.
    pub fn center_on(&mut self, id: DeviceId) {
        let Some(c) = self.cached() else { return };
        if let Some(i) = c.graph.index_of(nodo_core::graph::NodeRef::Device(id)) {
            let (x, y) = c.placed.pos[i];
            self.view.pan = (-x * self.view.zoom, -y * self.view.zoom);
        }
    }

    pub fn select_device(&mut self, id: DeviceId, cx: &mut Context<Self>) {
        if self.selected != Some(Sel::Device(id)) {
            self.edit = None;
        }
        self.selected = Some(Sel::Device(id));
        cx.notify();
    }

    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.about {
            self.about = false;
        } else if self.dialog.is_some() {
            self.dialog = None;
        } else if self.edit.is_some() {
            self.edit = None;
        } else if !self.search.read(cx).value().is_empty() {
            self.search.update(cx, |s, cx| s.set_value("", window, cx));
            self.focus.focus(window, cx);
        } else {
            self.selected = None;
        }
        cx.notify();
    }

    fn set_language(&mut self, code: &'static str, cx: &mut Context<Self>) {
        if i18n::set_locale(code) {
            crate::settings::Settings::update(|s| s.language = Some(code.to_string()));
            gpui_component::set_locale(code);
            crate::set_app_menus(cx);
            cx.refresh_windows();
        }
    }

    pub fn set_github_stars(&mut self, on: bool, cx: &mut Context<Self>) {
        crate::settings::Settings::update(|s| s.github_stars = (!on).then(|| "off".into())); // i18n-ok: setting value
        crate::star::refresh(on, cx);
        cx.refresh_windows();
    }

    pub fn set_list(&mut self, list: bool, cx: &mut Context<Self>) {
        self.view.list = list;
        crate::settings::Settings::update(|s| s.view = Some(if list { "list" } else { "map" }.into())); // i18n-ok: setting value
        cx.notify();
    }

    pub fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.view.mode = mode;
        self.view.fitted = false;
        crate::settings::Settings::update(|s| s.layout = Some(mode.id().into()));
        cx.notify();
    }

    pub fn toggle_layer(&mut self, l: Layer, cx: &mut Context<Self>) {
        if self.view.layers.contains(&l) {
            self.view.layers.retain(|x| *x != l);
        } else {
            self.view.layers.push(l);
            self.view.layers.sort();
        }
        let hidden: Vec<String> = Layer::ALL.into_iter().filter(|x| !self.view.layers.contains(x)).map(|x| x.id().to_string()).collect();
        crate::settings::Settings::update(|s| s.hidden_layers = hidden);
        cx.notify();
    }

    pub fn zoom_key(&mut self, factor: Option<f32>, cx: &mut Context<Self>) {
        if self.view.list || self.doc.is_none() {
            return;
        }
        match factor {
            Some(f) => self.view.zoom_by(f, None),
            None => self.fit(),
        }
        cx.notify();
    }

    /// Zoom and pan so the whole graph fits the canvas.
    pub fn fit(&mut self) {
        let Some(c) = self.cached() else { return };
        let (x0, y0, x1, y1) = c.placed.bounds();
        let b = self.view.viewport.get();
        let (w, h) = (f32::from(b.size.width).max(200.), f32::from(b.size.height).max(200.));
        let z = ((w - 60.) / (x1 - x0).max(1.)).min((h - 60.) / (y1 - y0).max(1.)).clamp(ZOOM_MIN, 1.2);
        self.view.zoom = z;
        self.view.pan = (-(x0 + x1) / 2. * z, -(y0 + y1) / 2. * z);
    }

    /// Terminals installed (needs the login-shell PATH), once.
    pub fn detect_tools(&mut self, cx: &mut Context<Self>) {
        if self.detected {
            return;
        }
        self.detected = true;
        let task = cx.background_executor().spawn(async move { crate::launch::detect_terminals() });
        cx.spawn(async move |this, cx| {
            let terms = task.await;
            this.update(cx, |this, cx| {
                this.terminals = Rc::new(terms);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn show_about(&mut self, cx: &mut Context<Self>) {
        self.about = true;
        cx.notify();
    }

    fn sync_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let loc = i18n::locale();
        if self.placeholder_locale != loc {
            self.placeholder_locale = loc;
            let text = i18n::t!("app.search.placeholder", keys = SEARCH_KEYS);
            self.search.update(cx, |s, cx| s.set_placeholder(text, window, cx));
        }
    }

    // ── rendering ────────────────────────────────────────────────────────────────────────────

    fn render_topbar(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let ready = matches!(self.phase, Phase::Ready);
        let mut left = div().flex().items_center().gap(px(10.)).min_w_0().flex_shrink_1();
        left = left.child(div().flex().flex_none().items_center().justify_center().size(px(30.)).rounded(px(8.)).bg(t.brand).child(ui::logo_mark(17., gpui::white())));
        let title = self.doc.as_ref().map(|d| d.title()).unwrap_or_else(|| crate::APP_NAME.to_string());
        left = left.child(div().min_w(px(40.)).flex_shrink_1().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).truncate().child(title));
        let current = i18n::locale();
        let weak = cx.entity().downgrade();
        let stars_on = crate::star::enabled(cx);
        let lang = Button::new("lang").ghost().icon(IconName::Globe).with_size(px(34.)).tooltip(i18n::t!("common.language")).dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
            menu = menu.label(i18n::t!("common.language"));
            for (code, name) in i18n::available() {
                let w = weak.clone();
                menu = menu.item(PopupMenuItem::new(name).checked(code == current).on_click(move |_, _, cx| {
                    w.update(cx, |this, cx| this.set_language(code, cx)).ok();
                }));
            }
            let w = weak.clone();
            menu = menu.separator().item(PopupMenuItem::new(i18n::t!("app.star.setting")).checked(stars_on).on_click(move |_, _, cx| {
                w.update(cx, |this, cx| this.set_github_stars(!stars_on, cx)).ok();
            }));
            menu = menu.label(i18n::t!("app.star.setting_hint"));
            let w = weak.clone();
            menu = menu.separator().item(PopupMenuItem::new(i18n::t!("app.menu.settings")).on_click(move |_, window, cx| {
                w.update(cx, |this, cx| this.open_settings(window, cx)).ok();
            }));
            let w = weak.clone();
            menu.item(PopupMenuItem::new(i18n::t!("app.about.menu", app = crate::APP_NAME)).on_click(move |_, _, cx| {
                w.update(cx, |this, cx| this.show_about(cx)).ok();
            }))
        });
        let search = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .w(px(420.))
            .min_w(px(160.))
            .flex_shrink(1.)
            .h(px(34.))
            .pl(px(10.))
            .pr(px(4.))
            .rounded(px(10.))
            .border_1()
            .border_color(t.border)
            .bg(t.canvas)
            .child(icon(IconName::Search, 14., t.ink_muted))
            .child(div().flex_1().min_w_0().text_size(px(13.)).child(Input::new(&self.search).appearance(false).small()))
            .children(self.match_count(cx).map(|(n, total)| div().flex_none().pr(px(6.)).text_size(px(11.)).text_color(if n == 0 { t.danger } else { t.ink_muted }).child(i18n::t!("app.search.count", n = n, total = total))));
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(52.))
            .px(px(12.))
            .border_b_1()
            .border_color(t.border)
            .bg(t.surface)
            .child(left)
            .child(div().flex_1())
            .when(ready, |d| d.child(search).child(self.render_discover_buttons(t, cx)))
            .when(!ready, |d| {
                d.child(ui::outline_button(t, "top-open").child(icon(IconName::FolderOpen, 15., t.ink)).child(i18n::t!("app.file.open")).on_click(cx.listener(|this, _, window, cx| this.open_dialog(window, cx))))
            })
            .child(lang)
    }

    fn match_count(&self, cx: &App) -> Option<(usize, usize)> {
        let q = self.query(cx);
        let doc = self.doc.as_ref()?;
        if q.is_empty() {
            return None;
        }
        Some((doc.data.devices.iter().filter(|d| d.haystack().contains(&q)).count(), doc.data.devices.len()))
    }

    fn render_welcome(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let recent = self.recent.iter().enumerate().map(|(i, p)| {
            let p2 = p.clone();
            let name = p.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
            let hover = t.canvas;
            div()
                .id(("recent", i))
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(10.))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .child(ui::logo_mark(15., t.ink_muted))
                .child(div().flex().flex_col().min_w_0().child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child(name)).child(div().text_size(px(11.)).text_color(t.ink_muted).truncate().child(p.display().to_string())))
                .on_click(cx.listener(move |this, _, window, cx| this.open_path(p2.clone(), window, cx)))
        });
        let feature = |ic: &str, title: String, body: String| div().flex().gap(px(10.)).w(px(250.)).child(ui::kind_svg(ic, 18., t.accent)).child(div().flex().flex_col().gap(px(2.)).child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(title)).child(div().text_size(px(12.)).line_height(px(17.)).text_color(t.ink_muted).child(body)));
        div()
            .id("welcome")
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(22.))
            .size_full()
            .overflow_y_scroll()
            .py(px(30.))
            .child(img(self.logo.clone()).size(px(112.)))
            .child(div().flex().flex_col().items_center().gap(px(6.)).child(div().text_size(px(26.)).font_weight(FontWeight::SEMIBOLD).child(i18n::t!("app.welcome.title"))).child(div().max_w(px(560.)).text_center().text_size(px(14.)).line_height(px(20.)).text_color(t.ink_muted).child(i18n::t!("app.welcome.subtitle"))))
            .child(
                div()
                    .flex()
                    .gap(px(10.))
                    .child(ui::filled_button(t, "new-big", t.accent).child(icon(IconName::Plus, 15., t.on_fill())).child(i18n::t!("app.file.new")).on_click(cx.listener(|this, _, window, cx| this.new_map_dialog(window, cx))))
                    .child(ui::outline_button(t, "open-big").child(icon(IconName::FolderOpen, 15., t.ink)).child(i18n::t!("app.file.open")).on_click(cx.listener(|this, _, window, cx| this.open_dialog(window, cx)))),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .max_w(px(820.))
                    .gap(px(18.))
                    .child(feature("lan", i18n::t!("app.welcome.f_lan"), i18n::t!("app.welcome.f_lan_body")))
                    .child(feature("tailscale", i18n::t!("app.welcome.f_overlay"), i18n::t!("app.welcome.f_overlay_body")))
                    .child(feature("server", i18n::t!("app.welcome.f_monitor"), i18n::t!("app.welcome.f_monitor_body"))),
            )
            .when(!self.recent.is_empty(), |d| d.child(div().flex().flex_col().w(px(460.)).gap(px(2.)).child(div().px(px(12.)).pb(px(4.)).child(ui::section_title(t, i18n::t!("app.welcome.recent")))).children(recent)))
            .child(div().max_w(px(520.)).text_center().text_size(px(11.5)).line_height(px(16.)).text_color(t.ink_muted).child(i18n::t!("app.welcome.privacy")))
    }

    fn render_failed(&self, t: &Tokens, path: &Path, message: &str, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(14.))
            .size_full()
            .child(icon(IconName::TriangleAlert, 32., t.danger))
            .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(i18n::t!("app.error.title")))
            .child(div().text_size(px(13.)).text_color(t.ink_muted).child(path.display().to_string()))
            .child(div().max_w(px(520.)).text_center().text_size(px(13.)).child(message.to_string()))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(ui::outline_button(t, "back").child(i18n::t!("app.error.back")).on_click(cx.listener(|this, _, window, cx| {
                        this.phase = Phase::Welcome;
                        window.set_window_title(crate::APP_NAME);
                        cx.notify();
                    })))
                    .child(ui::filled_button(t, "choose", t.accent).child(i18n::t!("app.file.open")).on_click(cx.listener(|this, _, window, cx| this.open_dialog(window, cx)))),
            )
    }

    /// Slim bar at the bottom of every screen: where you are on the left, About on the right.
    fn render_footer(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let left = match &self.doc {
            Some(d) => {
                let n = d.data.devices.len();
                format!("{} · {}", d.store.path().display(), i18n::tn!("app.devices.count", n)) // i18n-ok: separator
            }
            None => String::new(),
        };
        let ink = t.ink;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .h(px(26.))
            .px(px(12.))
            .border_t_1()
            .border_color(t.border)
            .bg(t.surface)
            .text_size(px(11.))
            .text_color(t.ink_muted)
            .child(div().flex_1().min_w_0().truncate().child(left))
            .children(self.monitor_status(t))
            .child(crate::star::footer_link(t, cx))
            .child(
                div()
                    .id("footer-about")
                    .flex_none()
                    .cursor_pointer()
                    .hover(move |s| s.text_color(ink).underline())
                    .child(i18n::t!("app.about.footer", app = crate::APP_NAME, version = crate::VERSION, company = crate::COMPANY))
                    .on_click(cx.listener(|this, _, _, cx| this.show_about(cx))),
            )
    }

    fn render_about(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let accent = t.accent;
        let card = div()
            .id("about-card")
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(12.))
            .w(px(420.))
            .px(px(32.))
            .pt(px(32.))
            .pb(px(26.))
            .rounded(px(18.))
            .border_1()
            .border_color(t.border)
            .bg(t.surface)
            .shadow_lg()
            // clicks inside the card must not reach the backdrop
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().absolute().top(px(10.)).right(px(10.)).child(ui::icon_button(t, "about-close", IconName::Close, true).on_click(cx.listener(|this, _, _, cx| {
                this.about = false;
                cx.notify();
            }))))
            .child(img(self.logo.clone()).size(px(88.)))
            .child(div().flex().items_baseline().gap(px(8.)).child(div().text_size(px(22.)).font_weight(FontWeight::SEMIBOLD).child(crate::APP_NAME)).child(div().text_size(px(13.)).text_color(t.ink_muted).child(crate::VERSION)))
            .child(div().text_center().text_size(px(13.)).line_height(px(19.)).text_color(t.ink_muted).child(i18n::t!("app.about.description")))
            .child(div().w_full().h(px(1.)).my(px(4.)).bg(t.border))
            .child(div().flex().flex_col().items_center().gap(px(2.)).child(div().text_size(px(13.)).child(i18n::t!("app.about.by", company = crate::COMPANY))).child(div().text_size(px(12.)).text_color(t.ink_muted).child(i18n::t!("app.about.place"))))
            .child(ui::filled_button(t, "about-site", t.accent).mt(px(4.)).child(i18n::t!("app.about.visit")).on_click(|_, _, cx| cx.open_url(crate::COMPANY_URL)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .child(div().id("about-source").text_size(px(12.)).text_color(accent).cursor_pointer().hover(|s| s.underline()).child(i18n::t!("app.about.source")).on_click(|_, _, cx| cx.open_url(crate::REPO_URL)))
                    .child(crate::star::about_button(t, cx)),
            )
            .child(crate::star::about_ask(t))
            .child(div().text_size(px(11.)).text_color(t.ink_muted).child(i18n::t!("app.about.license")));
        ui::backdrop(t, "about-backdrop")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.about = false;
                    cx.notify();
                }),
            )
            .child(card)
    }

    fn render_toast(&self, t: &Tokens) -> Option<impl IntoElement> {
        let (text, _) = self.toast.as_ref()?;
        Some(div().absolute().bottom(px(36.)).left_0().right_0().flex().justify_center().child(div().max_w(px(640.)).px(px(16.)).py(px(10.)).rounded(px(12.)).bg(t.ink).text_color(t.on_fill()).text_size(px(13.)).child(text.clone())))
    }
}

/// `~/Documents` if it exists, else home.
pub fn documents_dir() -> PathBuf {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default();
    let docs = home.join("Documents");
    if docs.is_dir() { docs } else { home }
}

impl Render for NodoApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_placeholder(window, cx);
        let t = tokens(cx);
        let body: AnyElement = match &self.phase {
            Phase::Welcome => self.render_welcome(&t, cx).into_any_element(),
            Phase::Failed { path, message } => {
                let (path, message) = (path.clone(), message.clone());
                self.render_failed(&t, &path, &message, cx).into_any_element()
            }
            Phase::Ready => {
                let center = self.render_map(&t, cx);
                let right = self.render_panel(&t, cx);
                div().flex().size_full().min_h_0().child(div().flex().flex_col().flex_1().min_w_0().h_full().child(div().flex_1().min_h_0().child(center)).children(self.render_output(&t, cx))).child(right).into_any_element()
            }
        };
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &NewMap, window, cx| this.new_map_dialog(window, cx)))
            .on_action(cx.listener(|this, _: &OpenMap, window, cx| this.open_dialog(window, cx)))
            .on_action(cx.listener(|this, _: &CloseMap, window, cx| this.close_map(window, cx)))
            .on_action(cx.listener(|this, _: &ClearRecent, _, cx| {
                nodo_core::paths::clear_recent();
                this.recent.clear();
                crate::set_app_menus(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenRecent0, window, cx| this.open_recent(0, window, cx)))
            .on_action(cx.listener(|this, _: &OpenRecent1, window, cx| this.open_recent(1, window, cx)))
            .on_action(cx.listener(|this, _: &OpenRecent2, window, cx| this.open_recent(2, window, cx)))
            .on_action(cx.listener(|this, _: &OpenRecent3, window, cx| this.open_recent(3, window, cx)))
            .on_action(cx.listener(|this, _: &OpenRecent4, window, cx| this.open_recent(4, window, cx)))
            .on_action(cx.listener(|this, _: &OpenRecent5, window, cx| this.open_recent(5, window, cx)))
            .on_action(cx.listener(|this, _: &OpenRecent6, window, cx| this.open_recent(6, window, cx)))
            .on_action(cx.listener(|this, _: &OpenRecent7, window, cx| this.open_recent(7, window, cx)))
            .on_action(cx.listener(|this, _: &OpenRecent8, window, cx| this.open_recent(8, window, cx)))
            .on_action(cx.listener(|this, _: &OpenRecent9, window, cx| this.open_recent(9, window, cx)))
            .on_action(cx.listener(|this, _: &ImportCsv, window, cx| this.import_csv(window, cx)))
            .on_action(cx.listener(|this, _: &ImportWgDump, window, cx| this.import_wg_dump(window, cx)))
            .on_action(cx.listener(|this, _: &ExportPng, window, cx| this.export(crate::export_ui::Format::Png, window, cx)))
            .on_action(cx.listener(|this, _: &ExportPdf, window, cx| this.export(crate::export_ui::Format::Pdf, window, cx)))
            .on_action(cx.listener(|this, _: &ExportSvg, window, cx| this.export(crate::export_ui::Format::Svg, window, cx)))
            .on_action(cx.listener(|this, _: &ExportMermaid, window, cx| this.export(crate::export_ui::Format::Mermaid, window, cx)))
            .on_action(cx.listener(|this, _: &ExportCsv, window, cx| this.export(crate::export_ui::Format::Csv, window, cx)))
            .on_action(cx.listener(|this, _: &ExportJson, window, cx| this.export(crate::export_ui::Format::Json, window, cx)))
            .on_action(cx.listener(|this, _: &ToggleList, _, cx| this.set_list(!this.view.list, cx)))
            .on_action(cx.listener(|this, _: &LayoutRadial, _, cx| this.set_mode(Mode::Radial, cx)))
            .on_action(cx.listener(|this, _: &LayoutSubnet, _, cx| this.set_mode(Mode::BySubnet, cx)))
            .on_action(cx.listener(|this, _: &LayoutNetwork, _, cx| this.set_mode(Mode::ByNetwork, cx)))
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| this.zoom_key(Some(1.2), cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| this.zoom_key(Some(1. / 1.2), cx)))
            .on_action(cx.listener(|this, _: &ZoomReset, _, cx| this.zoom_key(None, cx)))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| this.focus_search(window, cx)))
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.escape(window, cx)))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.refresh(cx)))
            .on_action(cx.listener(|this, _: &ScanSubnet, _, cx| this.ask_scan(cx)))
            .on_action(cx.listener(|this, _: &DiscoverTailscale, _, cx| this.discover_tailscale(true, cx)))
            .on_action(cx.listener(|this, _: &DiscoverAws, window, cx| this.open_aws(window, cx)))
            .on_action(cx.listener(|this, _: &DiscoverWireGuard, _, cx| this.discover_wireguard(cx)))
            .on_action(cx.listener(|this, _: &AddDevice, window, cx| this.open_add_device(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleMonitor, _, cx| this.toggle_monitor(cx)))
            .on_action(cx.listener(|this, _: &ShowAbout, _, cx| this.show_about(cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.canvas)
            .text_color(t.ink)
            .font_family(SANS)
            .child(self.render_topbar(&t, cx))
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_footer(&t, cx))
            .children(self.render_toast(&t))
            .children(self.render_dialog(&t, cx))
            .when(self.about, |d| d.child(self.render_about(&t, cx)))
    }
}
