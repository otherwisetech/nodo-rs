//! The terminal panel in the window: tabs (local shells, SSH sessions, ping / traceroute), docked
//! at the bottom or on the right with a splitter, collapsible to a bar, maximizable; and the
//! device actions that open tabs.
use super::model::Spawn;
use super::state::{Dock, Layout, Prefer, Shown, Tabs};
use super::view::{TermEvent, TermStyle, TerminalView};
use crate::app::NodoApp;
use crate::theme::{MONO, Tokens};
use crate::ui::{self, icon};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::IconName;
use gpui_component::tooltip::Tooltip;
use nodo_core::model::DeviceId;
use nodo_core::tools::Tool;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

actions!(nodo_terminal, [NewTerminal, CloseTerminalTab, ToggleTerminal, ToggleTerminalMaximize, NextTerminalTab, PrevTerminalTab]);

#[cfg(target_os = "macos")]
mod keys {
    pub const NEW: &str = "cmd-t";
    pub const CLOSE: &str = "cmd-w";
    // ⌘J is the AI panel's, as in every Otherwise app
    pub const TOGGLE: &str = "ctrl-`";
    pub const MAXIMIZE: &str = "cmd-shift-enter";
    pub const NEXT: &[&str] = &["cmd-}", "cmd-shift-]"];
    pub const PREV: &[&str] = &["cmd-{", "cmd-shift-["];
    pub const NEW_LABEL: &str = "⌘T"; // i18n-ok: shortcut
    pub const TOGGLE_LABEL: &str = "⌃`"; // i18n-ok: shortcut
    pub const CLOSE_LABEL: &str = "⌘W"; // i18n-ok: shortcut
    pub const MAXIMIZE_LABEL: &str = "⌘⇧↩"; // i18n-ok: shortcut
}
#[cfg(not(target_os = "macos"))]
mod keys {
    // ctrl-t / ctrl-w / ctrl-j belong to the shell
    pub const NEW: &str = "ctrl-shift-t";
    pub const CLOSE: &str = "ctrl-shift-w";
    pub const TOGGLE: &str = "ctrl-shift-j";
    pub const MAXIMIZE: &str = "ctrl-shift-enter";
    pub const NEXT: &[&str] = &["ctrl-pagedown"];
    pub const PREV: &[&str] = &["ctrl-pageup"];
    pub const NEW_LABEL: &str = "Ctrl+Shift+T"; // i18n-ok: shortcut
    pub const TOGGLE_LABEL: &str = "Ctrl+Shift+J"; // i18n-ok: shortcut
    pub const CLOSE_LABEL: &str = "Ctrl+Shift+W"; // i18n-ok: shortcut
    pub const MAXIMIZE_LABEL: &str = "Ctrl+Shift+Enter"; // i18n-ok: shortcut
}

pub fn bind_keys(cx: &mut App) {
    let ctx = Some(crate::app::CONTEXT);
    let term = Some(super::view::KEY_CONTEXT);
    let mut b = vec![KeyBinding::new(keys::NEW, NewTerminal, ctx), KeyBinding::new(keys::CLOSE, CloseTerminalTab, ctx), KeyBinding::new(keys::TOGGLE, ToggleTerminal, ctx), KeyBinding::new(keys::MAXIMIZE, ToggleTerminalMaximize, ctx)];
    b.extend(keys::NEXT.iter().map(|k| KeyBinding::new(k, NextTerminalTab, ctx)));
    b.extend(keys::PREV.iter().map(|k| KeyBinding::new(k, PrevTerminalTab, ctx)));
    // Keys the app (or gpui-component's Root) binds that the child must get: NoAction in the
    // deeper terminal context out-ranks them and the keystroke falls through to the view.
    let mut through = vec!["escape", "tab", "shift-tab"];
    if cfg!(not(target_os = "macos")) {
        through.extend(["ctrl-n", "ctrl-o", "ctrl-f", "ctrl-r", "ctrl-l", "ctrl-1", "ctrl-2", "ctrl-3", "ctrl-=", "ctrl-+", "ctrl--", "ctrl-0", "ctrl-,"]);
    }
    b.extend(through.into_iter().map(|k| KeyBinding::new(k, NoAction, term)));
    cx.bind_keys(b);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabKind {
    Shell,
    Ssh,
    Tool,
}

pub struct TermTab {
    pub id: u64,
    pub view: Entity<TerminalView>,
    /// The device's name, or "Shell".
    pub label: String,
    pub command: String,
    pub device: Option<DeviceId>,
    pub kind: TabKind,
    _sub: Subscription,
}

pub struct TermPanel {
    pub layout: Layout,
    pub tabs: Tabs<TermTab>,
    next_id: u64,
    save_gen: u64,
    /// The room the docked panel had along its axis at the last paint.
    last_total: Rc<Cell<f32>>,
}

impl TermPanel {
    pub fn load() -> TermPanel {
        TermPanel { layout: Layout::from_settings(&crate::settings::Settings::load()), tabs: Tabs::default(), next_id: 1, save_gen: 0, last_total: Rc::new(Cell::new(0.)) }
    }
}

/// Splitter drag payload: where in the handle it was grabbed.
#[derive(Clone, Default)]
struct SplitDrag(Rc<Cell<Pixels>>);

impl Render for SplitDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

fn home() -> std::path::PathBuf {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

/// The user's login shell, started as one.
fn login_shell() -> (String, Vec<String>) {
    if cfg!(windows) {
        return (std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into()), Vec::new());
    }
    (crate::launch::user_shell(), vec!["-l".into()])
}

pub fn sync_style(cx: &mut App) {
    let font = crate::settings::Settings::load().term_font();
    let t = crate::theme::tokens(cx);
    cx.set_global(TermStyle::from_tokens(&t, font));
}

impl NodoApp {
    // ── tabs ─────────────────────────────────────────────────────────────────────────────────

    pub fn open_tab(&mut self, spec: Spawn, label: String, device: Option<DeviceId>, kind: TabKind, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(TerminalView::new);
        let sub = cx.subscribe(&view, |_, _, _: &TermEvent, cx| cx.notify());
        view.update(cx, |v, cx| v.run(&spec, window, cx));
        let id = self.term.next_id;
        self.term.next_id += 1;
        let command = nodo_core::remote::command_line(&spec.program, &spec.args);
        self.term.tabs.push(TermTab { id, view, label, command, device, kind, _sub: sub });
        self.term.layout.reveal();
        self.save_term_layout(cx);
        cx.notify();
    }

    pub fn new_local_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (program, args) = login_shell();
        self.open_tab(Spawn { program, args, cwd: home(), env: Vec::new() }, i18n::t!("app.term.shell"), None, TabKind::Shell, window, cx);
    }

    pub fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        // dropping the tab drops its `Terminal`, which ends the child's process group
        if let Some(t) = self.term.tabs.close(ix) {
            t.view.update(cx, |v, _| v.terminal = None);
        }
        self.focus_active_tab(window, cx);
        cx.notify();
    }

    fn select_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.term.tabs.select(ix);
        self.focus_active_tab(window, cx);
        cx.notify();
    }

    fn focus_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.term.tabs.active() {
            Some(t) if self.term.layout.shown(1) != Shown::Hidden => {
                let v = t.view.clone();
                v.update(cx, |v, cx| {
                    v.unseen_bell = false;
                    v.focus(window, cx);
                });
            }
            _ => self.focus_app(window, cx),
        }
    }

    pub fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.term.tabs.is_empty() {
            self.new_local_shell(window, cx);
            return;
        }
        self.term.layout.toggle();
        self.after_layout(window, cx);
    }

    fn after_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_term_layout(cx);
        if matches!(self.term.layout.shown(self.term.tabs.len()), Shown::Docked(_) | Shown::Maximized) {
            self.focus_active_tab(window, cx);
        } else {
            self.focus_app(window, cx);
        }
        cx.notify();
    }

    /// A map was closed: its device ids mean nothing in the next one.
    pub fn forget_term_devices(&mut self) {
        for t in &mut self.term.tabs.items {
            t.device = None;
        }
    }

    /// Devices with a session still running, for the graph's badges.
    pub fn has_session(&self, id: DeviceId, cx: &App) -> bool {
        self.term.tabs.items.iter().any(|t| t.device == Some(id) && t.view.read(cx).is_running())
    }

    /// Remember dock and sizes, a moment after the last change (a splitter drag is many).
    fn save_term_layout(&mut self, cx: &mut Context<Self>) {
        self.term.save_gen += 1;
        let gen_ = self.term.save_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(400)).await;
            this.update(cx, |this, _| {
                if this.term.save_gen == gen_ {
                    let l = this.term.layout.clone();
                    crate::settings::Settings::update(|s| l.store(s));
                }
            })
            .ok();
        })
        .detach();
    }

    // ── device actions ───────────────────────────────────────────────────────────────────────

    fn magic_suffixes(&self) -> Vec<String> {
        let Some(doc) = &self.doc else { return Vec::new() };
        doc.data.networks.iter().flat_map(|n| n.facts.iter().filter(|(k, _)| k == "magic_dns").map(|(_, v)| v.clone())).collect()
    }

    fn ssh_command(&mut self, id: DeviceId, cx: &mut Context<Self>) -> Option<(String, (String, Vec<String>))> {
        let d = self.doc.as_ref()?.data.device(id)?.clone();
        match nodo_core::remote::ssh_for(&d, &self.magic_suffixes()) {
            Ok(c) => Some((d.name.clone(), c)),
            Err(e) => {
                let msg = match e {
                    nodo_core::remote::SshError::NoAddress => i18n::t!("app.term.no_host", name = d.name.clone()),
                    nodo_core::remote::SshError::BadUser(u) => i18n::t!("app.term.bad_user", user = u),
                    nodo_core::remote::SshError::BadHost(h) => i18n::t!("app.term.bad_host", host = h),
                };
                self.toast(msg, cx);
                None
            }
        }
    }

    /// SSH into the device, in a tab or in the terminal app as the setting says.
    pub fn ssh(&mut self, id: DeviceId, window: &mut Window, cx: &mut Context<Self>) {
        // no SSH into the computer we're on: a local shell
        if self.this_computer() == Some(id) {
            self.shell_here(id, window, cx);
            return;
        }
        if Prefer::parse(crate::settings::Settings::load().term_prefer.as_deref()) == Prefer::External {
            self.ssh_external(id, cx);
            return;
        }
        let Some((name, (program, args))) = self.ssh_command(id, cx) else { return };
        self.open_tab(Spawn { program, args, cwd: home(), env: Vec::new() }, name, Some(id), TabKind::Ssh, window, cx);
    }

    pub fn ssh_external(&mut self, id: DeviceId, cx: &mut Context<Self>) {
        let Some((_, (program, args))) = self.ssh_command(id, cx) else { return };
        self.launch(nodo_core::remote::command_line(&program, &args), cx);
    }

    pub fn run_tool(&mut self, id: DeviceId, tool: Tool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.doc.as_ref().and_then(|doc| doc.data.device(id)).cloned() else { return };
        let Some(ip) = d.primary_ip() else { return };
        let (program, args) = nodo_core::tools::command(tool, ip);
        self.open_tab(Spawn { program, args, cwd: home(), env: Vec::new() }, d.name.clone(), Some(id), TabKind::Tool, window, cx);
    }

    /// A login shell with `NODO_DEVICE_*` set to the device's address, name and SSH user.
    pub fn shell_here(&mut self, id: DeviceId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.doc.as_ref().and_then(|doc| doc.data.device(id)).cloned() else { return };
        let (program, args) = login_shell();
        // on this computer, a plain local shell (no NODO_DEVICE_* pointing back at itself)
        if self.this_computer() == Some(id) {
            self.open_tab(Spawn { program, args, cwd: home(), env: Vec::new() }, i18n::t!("app.term.local_shell"), Some(id), TabKind::Shell, window, cx);
            return;
        }
        let env = nodo_core::remote::device_env(&d, &self.magic_suffixes());
        self.open_tab(Spawn { program, args, cwd: home(), env }, d.name.clone(), Some(id), TabKind::Shell, window, cx);
    }

    // ── rendering ────────────────────────────────────────────────────────────────────────────

    /// The window body with the panel placed around it.
    pub(crate) fn render_with_terminal(&mut self, body: AnyElement, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        match self.term.layout.shown(self.term.tabs.len()) {
            Shown::Hidden => body,
            Shown::Bar => div().flex().flex_col().size_full().child(div().flex_1().min_h_0().child(body)).child(self.render_term_bar(t, cx)).into_any_element(),
            Shown::Maximized => self.render_term_box(t, cx).into_any_element(),
            Shown::Docked(dock) => self.render_docked(body, dock, t, cx),
        }
    }

    fn render_docked(&mut self, body: AnyElement, dock: Dock, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let bottom = dock == Dock::Bottom;
        let total = self.term.last_total.get();
        let size = self.term.layout.size(if total > 0. { total } else { 2000. });
        let grip_color = t.border_strong;
        let accent = t.accent;
        let handle = div()
            .id("term-split")
            .group("term-split")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .bg(t.surface)
            .border_color(t.border)
            .when(bottom, |d| d.h(px(6.)).w_full().border_t_1().cursor_row_resize())
            .when(!bottom, |d| d.w(px(6.)).h_full().border_l_1().cursor_col_resize())
            .on_mouse_down(MouseButton::Left, |_, _, cx| gpui_component::GlobalState::suppress_text_selection(cx))
            .on_drag(SplitDrag::default(), move |d, offset, _, cx| {
                cx.stop_propagation();
                d.0.set(if bottom { offset.y } else { offset.x });
                cx.new(|_| d.clone())
            })
            .child(div().rounded_full().bg(grip_color).group_hover("term-split", move |d| d.bg(accent)).when(bottom, |d| d.w(px(36.)).h(px(3.))).when(!bottom, |d| d.h(px(36.)).w(px(3.))));
        let last_total = self.term.last_total.clone();
        let container = div()
            .size_full()
            .flex()
            .when(bottom, |d| d.flex_col())
            .on_drag_move(cx.listener(move |this, e: &DragMoveEvent<SplitDrag>, _, cx| {
                let grab = e.drag(cx).0.get();
                let (size, total) = if bottom {
                    (e.bounds.bottom() - (e.event.position.y - grab), e.bounds.size.height)
                } else {
                    (e.bounds.right() - (e.event.position.x - grab), e.bounds.size.width)
                };
                if this.term.layout.resize(f32::from(size), f32::from(total)) {
                    this.save_term_layout(cx);
                    cx.notify();
                }
            }))
            // remember how much room there is, so the size is clamped before the first drag
            .child(
                canvas(
                    move |b, _, _| {
                        last_total.set(f32::from(if bottom { b.size.height } else { b.size.width }));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            // gpui doesn't clip by default: a squeezed map would paint over the terminal
            .child(div().flex_1().min_h_0().min_w_0().overflow_hidden().child(body))
            .child(handle);
        let panel = self.render_term_box(t, cx).flex_none().when(bottom, |d| d.h(px(size)).w_full()).when(!bottom, |d| d.w(px(size)).h_full());
        container.child(panel).into_any_element()
    }

    fn render_term_box(&mut self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let view = self.term.tabs.active().map(|tab| tab.view.clone());
        div().flex().flex_col().min_h_0().min_w_0().bg(t.surface).child(self.render_term_header(t, cx)).child(div().flex_1().min_h_0().children(view))
    }

    fn render_term_header(&mut self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = self.term.layout.clone();
        let maximized = layout.maximized;
        let mut strip = div().id("term-tabs").flex().flex_1().min_w_0().items_center().gap(px(2.)).overflow_x_scroll();
        for (ix, tab) in self.term.tabs.items.iter().enumerate() {
            let active = ix == self.term.tabs.active;
            let v = tab.view.read(cx);
            let running = v.is_running();
            let bell = v.unseen_bell && !active;
            let title = v.title();
            let exit = v.exit_code();
            let label = match (tab.kind, &title) {
                (TabKind::Shell, Some(ti)) if tab.device.is_none() => ti.clone(),
                _ => tab.label.clone(),
            };
            let tip = {
                let mut lines = vec![tab.command.clone()];
                if let Some(ti) = title.filter(|ti| *ti != label) {
                    lines.push(ti);
                }
                if let Some(c) = exit {
                    lines.push(i18n::t!("app.term.exited", code = c));
                }
                lines.join("\n")
            };
            let ic = match tab.kind {
                TabKind::Shell => IconName::SquareTerminal,
                TabKind::Ssh => IconName::Globe,
                TabKind::Tool => IconName::Inspector,
            };
            let dot = if bell { Some(t.tag) } else if !running { Some(t.ink_muted) } else { None };
            let hover = t.canvas;
            let ink = if active { t.ink } else { t.ink_muted };
            strip = strip.child(
                div()
                    .id(("term-tab", tab.id as usize))
                    .group("term-tab")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(5.))
                    .h(px(26.))
                    .pl(px(8.))
                    .pr(px(3.))
                    .max_w(px(220.))
                    .rounded(px(7.))
                    .text_size(px(12.))
                    .text_color(ink)
                    .cursor_pointer()
                    .when(active, |d| d.bg(t.canvas).font_weight(FontWeight::MEDIUM))
                    .when(!active, move |d| d.hover(move |s| s.bg(hover)))
                    .child(icon(ic, 12., ink))
                    .child(div().min_w_0().truncate().when(!running, |d| d.line_through()).child(label))
                    .children(dot.map(|c| div().flex_none().size(px(6.)).rounded_full().bg(c)))
                    .child(
                        div()
                            .id(("term-tab-close", tab.id as usize))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(18.))
                            .rounded(px(5.))
                            .hover(move |s| s.bg(hover))
                            .child(icon(IconName::Close, 11., t.ink_muted))
                            .tooltip(|window, cx| Tooltip::new(i18n::t!("app.term.close_tab", keys = keys::CLOSE_LABEL)).build(window, cx))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab(ix, window, cx);
                            })),
                    )
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, window, cx| this.select_tab(ix, window, cx))),
            );
        }
        let small = |id: &'static str, ic: IconName, tip: String| {
            let hover = t.canvas;
            div().id(id).flex().flex_none().items_center().justify_center().size(px(26.)).rounded(px(7.)).cursor_pointer().hover(move |s| s.bg(hover)).child(icon(ic, 14., t.ink_muted)).tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        };
        strip = strip.child(small("term-new", IconName::Plus, i18n::t!("app.term.new", keys = keys::NEW_LABEL)).on_click(cx.listener(|this, _, window, cx| this.new_local_shell(window, cx))));
        let seg = |id: &'static str, ic: IconName, label: String, dock: Dock| {
            let on = layout.dock == dock && !maximized;
            ui::seg(t, id, String::new(), on).h(px(22.)).px(px(7.)).child(icon(ic, 12., if on { t.ink } else { t.ink_muted })).child(label).on_click(cx.listener(move |this, _, window, cx| {
                this.term.layout.set_dock(dock);
                this.after_layout(window, cx);
            }))
        };
        let docks = ui::seg_group(t).child(seg("term-dock-bottom", IconName::PanelBottom, i18n::t!("app.term.dock_bottom"), Dock::Bottom)).child(seg("term-dock-right", IconName::PanelRight, i18n::t!("app.term.dock_right"), Dock::Right));
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .h(px(34.))
            .px(px(6.))
            .border_b_1()
            .border_t_1()
            .border_color(t.border)
            .bg(t.surface)
            .child(strip)
            .child(docks)
            .child(small("term-collapse", IconName::ChevronDown, i18n::t!("app.term.collapse")).on_click(cx.listener(|this, _, window, cx| {
                this.term.layout.toggle_collapsed();
                this.after_layout(window, cx);
            })))
            .child(small("term-max", if maximized { IconName::Minimize } else { IconName::Maximize }, if maximized { i18n::t!("app.term.restore", keys = keys::MAXIMIZE_LABEL) } else { i18n::t!("app.term.maximize", keys = keys::MAXIMIZE_LABEL) }).on_click(cx.listener(|this, _, window, cx| {
                this.term.layout.toggle_maximized();
                this.after_layout(window, cx);
            })))
            .child(small("term-hide", IconName::Close, i18n::t!("app.term.hide", keys = keys::TOGGLE_LABEL)).on_click(cx.listener(|this, _, window, cx| {
                this.term.layout.open = false;
                this.term.layout.maximized = false;
                this.after_layout(window, cx);
            })))
    }

    fn render_term_bar(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let n = self.term.tabs.len();
        let (label, last) = match self.term.tabs.active() {
            Some(tab) => (tab.label.clone(), tab.view.read(cx).last_line().unwrap_or_default()),
            None => (String::new(), String::new()),
        };
        let hover = t.canvas;
        div()
            .id("term-bar")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .h(px(30.))
            .px(px(10.))
            .border_t_1()
            .border_color(t.border)
            .bg(t.surface)
            .text_size(px(12.))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .child(icon(IconName::SquareTerminal, 13., t.ink_muted))
            .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(i18n::t!("app.term.title")))
            .child(div().flex_none().text_color(t.ink_muted).child(i18n::tn!("app.term.tabs", n)))
            .child(div().flex_none().child(label))
            .child(div().flex_1().min_w_0().truncate().font_family(MONO).text_size(px(11.)).text_color(t.ink_muted).child(last))
            .child(icon(IconName::ChevronUp, 14., t.ink_muted))
            .tooltip(|window, cx| Tooltip::new(i18n::t!("app.term.expand", keys = keys::TOGGLE_LABEL)).build(window, cx))
            .on_click(cx.listener(|this, _, window, cx| {
                this.term.layout.toggle_collapsed();
                this.after_layout(window, cx);
            }))
    }

    /// Footer entry while the panel is hidden but tabs still run.
    pub(crate) fn render_term_footer(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let n = self.term.tabs.len();
        if n == 0 || self.term.layout.shown(n) != Shown::Hidden {
            return None;
        }
        let ink = t.ink;
        Some(
            div()
                .id("footer-term")
                .flex()
                .flex_none()
                .items_center()
                .gap(px(4.))
                .cursor_pointer()
                .hover(move |s| s.text_color(ink))
                .child(icon(IconName::SquareTerminal, 11., t.ink_muted))
                .child(i18n::tn!("app.term.tabs", n))
                .tooltip(|window, cx| Tooltip::new(i18n::t!("app.term.show", keys = keys::TOGGLE_LABEL)).build(window, cx))
                .on_click(cx.listener(|this, _, window, cx| this.toggle_terminal(window, cx))),
        )
    }

    pub(crate) fn on_terminal_actions(&self, d: Div, cx: &mut Context<Self>) -> Div {
        d.on_action(cx.listener(|this, _: &NewTerminal, window, cx| this.new_local_shell(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleTerminal, window, cx| this.toggle_terminal(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleTerminalMaximize, window, cx| {
                if this.term.tabs.is_empty() {
                    return;
                }
                this.term.layout.toggle_maximized();
                this.after_layout(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseTerminalTab, window, cx| {
                if this.term.layout.shown(this.term.tabs.len()) == Shown::Hidden {
                    cx.propagate();
                    return;
                }
                let ix = this.term.tabs.active;
                this.close_tab(ix, window, cx);
            }))
            .on_action(cx.listener(|this, _: &NextTerminalTab, window, cx| {
                this.term.tabs.next(1);
                this.focus_active_tab(window, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &PrevTerminalTab, window, cx| {
                this.term.tabs.next(-1);
                this.focus_active_tab(window, cx);
                cx.notify();
            }))
    }
}
