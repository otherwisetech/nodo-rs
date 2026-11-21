//! The map: toolbar (graph / list, layout, layers, zoom), the graph canvas (rama's worktree-map
//! approach: edges painted on a canvas, nodes as positioned elements, drag or scroll to pan,
//! ⌘+scroll / pinch / ⌘± to zoom) and the sortable list.
use crate::app::{NodoApp, Sel, Sort, ZOOM_MAX, ZOOM_MIN};
use crate::discovery::Status;
use crate::theme::{MONO, Tokens};
use crate::ui::{self, icon};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_component::spinner::Spinner;
use gpui_component::tooltip::Tooltip;
use gpui_component::{IconName, Sizable as _};
use nodo_core::graph::NodeRef;
use nodo_core::layout::{Mode, NODE_W};
use nodo_core::model::{Device, DeviceId, Layer};
use std::sync::atomic::Ordering;

const CIRCLE: f32 = 46.;
/// From, to, colour, dashed (overlay), faint (offline or not matching the search).
type Edge = ((f32, f32), (f32, f32), Hsla, bool, bool);
const HUB: f32 = 54.;

pub fn layer_label(l: Layer) -> String {
    nodo_core::render::layer_name(l)
}

pub fn kind_label(k: nodo_core::model::DeviceKind) -> String {
    use nodo_core::model::DeviceKind as K;
    match k {
        K::Unknown => i18n::t!("app.kind.unknown"),
        K::Router => i18n::t!("app.kind.router"),
        K::Switch => i18n::t!("app.kind.switch"),
        K::AccessPoint => i18n::t!("app.kind.ap"),
        K::Server => i18n::t!("app.kind.server"),
        K::Nas => i18n::t!("app.kind.nas"),
        K::Camera => i18n::t!("app.kind.camera"),
        K::Printer => i18n::t!("app.kind.printer"),
        K::Tv => i18n::t!("app.kind.tv"),
        K::Speaker => i18n::t!("app.kind.speaker"),
        K::Phone => i18n::t!("app.kind.phone"),
        K::Tablet => i18n::t!("app.kind.tablet"),
        K::Laptop => i18n::t!("app.kind.laptop"),
        K::Desktop => i18n::t!("app.kind.desktop"),
        K::Iot => i18n::t!("app.kind.iot"),
        K::Vm => i18n::t!("app.kind.vm"),
        K::Vpn => i18n::t!("app.kind.vpn"),
    }
}

pub fn status_color(t: &Tokens, s: Status) -> Hsla {
    match s {
        Status::Up(_) => t.up,
        Status::Down => t.danger,
        Status::Offline | Status::Unknown => t.ink_muted.opacity(0.6),
    }
}

pub fn status_label(s: Status) -> String {
    match s {
        Status::Up(Some(ms)) => i18n::t!("app.status.up_ms", ms = format!("{ms:.0}")),
        Status::Up(None) => i18n::t!("app.status.up"),
        Status::Down => i18n::t!("app.status.down"),
        Status::Offline => i18n::t!("app.status.offline"),
        Status::Unknown => i18n::t!("app.status.unknown"),
    }
}

impl NodoApp {
    /// Refresh, Scan subnet…, Sources ▾ and Add device, in the top bar.
    pub(crate) fn render_discover_buttons(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let d = &self.disc;
        let refresh = ui::outline_button(t, "disc-refresh")
            .px(px(10.))
            .when(d.passive, |b| b.child(Spinner::new().small().color(t.accent)))
            .when(!d.passive, |b| b.child(icon(IconName::Redo, 14., t.ink)))
            .tooltip(|w, cx| Tooltip::new(i18n::t!("app.discover.refresh_tip")).build(w, cx))
            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)));
        let scan = match &d.scan {
            Some(p) => {
                let (done, total, found) = (p.done.load(Ordering::Relaxed), p.total.load(Ordering::Relaxed).max(1), p.found.load(Ordering::Relaxed));
                ui::outline_button(t, "disc-scan").child(Spinner::new().small().color(t.accent)).child(i18n::t!("app.scan.progress", done = done, total = total, found = found)).tooltip(|w, cx| Tooltip::new(i18n::t!("app.scan.cancel")).build(w, cx)).on_click(cx.listener(|this, _, _, cx| this.cancel_scan(cx)))
            }
            None => ui::outline_button(t, "disc-scan").child(icon(IconName::Network, 14., t.ink)).child(i18n::t!("app.discover.scan")).on_click(cx.listener(|this, _, _, cx| this.ask_scan(cx))),
        };
        let weak = cx.entity().downgrade();
        let busy_src = d.tailscale || d.aws || d.wg;
        let sources = Button::new("disc-sources").outline().icon(if busy_src { IconName::LoaderCircle } else { IconName::ChevronDown }).label(i18n::t!("app.discover.sources")).dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
            let item = |label: String, f: fn(&mut NodoApp, &mut Window, &mut Context<NodoApp>)| {
                let w = weak.clone();
                PopupMenuItem::new(label).on_click(move |_, window, cx| {
                    w.update(cx, |this, cx| f(this, window, cx)).ok();
                })
            };
            menu.item(item(i18n::t!("app.discover.tailscale"), |this, _, cx| this.discover_tailscale(true, cx)))
                .item(item(i18n::t!("app.discover.aws"), |this, window, cx| this.open_aws(window, cx)))
                .item(item(i18n::t!("app.discover.wireguard"), |this, _, cx| this.discover_wireguard(cx)))
                .separator()
                .item(item(i18n::t!("app.menu.import_wg"), |this, window, cx| this.import_wg_dump(window, cx)))
                .item(item(i18n::t!("app.menu.import_csv"), |this, window, cx| this.import_csv(window, cx)))
        });
        let add = ui::icon_button(t, "disc-add", IconName::Plus, true).tooltip(|w, cx| Tooltip::new(i18n::t!("app.discover.add")).build(w, cx)).on_click(cx.listener(|this, _, window, cx| this.open_add_device(window, cx)));
        div().flex().flex_none().items_center().gap(px(6.)).child(refresh).child(scan).child(sources).child(add)
    }

    pub(crate) fn render_map(&mut self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let toolbar = self.map_toolbar(t, cx);
        let empty = self.doc.as_ref().is_some_and(|d| d.data.devices.is_empty());
        let body = if empty {
            self.render_empty(t, cx)
        } else if self.view.list {
            self.map_list(t, cx)
        } else {
            self.map_canvas(t, cx)
        };
        div().flex().flex_col().size_full().bg(t.canvas).child(toolbar).child(div().flex_1().min_h_0().child(body)).into_any_element()
    }

    fn render_empty(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(12.))
            .text_color(t.ink_muted)
            .child(ui::kind_svg("lan", 40., t.accent))
            .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink).child(if self.disc.passive { i18n::t!("app.empty.listening") } else { i18n::t!("app.empty.title") }))
            .child(div().max_w(px(460.)).text_center().text_size(px(13.)).line_height(px(19.)).child(i18n::t!("app.empty.body")))
            .when(self.disc.passive, |d| d.child(Spinner::new().color(t.accent)))
            .child(div().flex().gap(px(8.)).child(ui::outline_button(t, "empty-scan").child(i18n::t!("app.discover.scan")).on_click(cx.listener(|this, _, _, cx| this.ask_scan(cx)))).child(ui::outline_button(t, "empty-ts").child(i18n::t!("app.discover.tailscale")).on_click(cx.listener(|this, _, _, cx| this.discover_tailscale(true, cx)))))
            .into_any_element()
    }

    fn map_toolbar(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let list = self.view.list;
        let mode = ui::seg_group(t)
            .child(ui::seg(t, "view-map", i18n::t!("app.view.map"), !list).on_click(cx.listener(|this, _, _, cx| this.set_list(false, cx))))
            .child(ui::seg(t, "view-list", i18n::t!("app.view.list"), list).on_click(cx.listener(|this, _, _, cx| this.set_list(true, cx))));
        let m = self.view.mode;
        let layout = ui::seg_group(t)
            .child(ui::seg(t, "lay-radial", i18n::t!("app.layout.radial"), m == Mode::Radial).on_click(cx.listener(|this, _, _, cx| this.set_mode(Mode::Radial, cx))))
            .child(ui::seg(t, "lay-subnet", i18n::t!("app.layout.subnet"), m == Mode::BySubnet).on_click(cx.listener(|this, _, _, cx| this.set_mode(Mode::BySubnet, cx))))
            .child(ui::seg(t, "lay-network", i18n::t!("app.layout.network"), m == Mode::ByNetwork).on_click(cx.listener(|this, _, _, cx| this.set_mode(Mode::ByNetwork, cx))));
        let counts = self.layer_counts();
        let mut layers = div().flex().flex_none().items_center().gap(px(4.));
        for l in Layer::ALL {
            let on = self.view.layers.contains(&l);
            let c = t.layer(l);
            let n = counts.get(&l).copied().unwrap_or(0);
            layers = layers.child(
                div()
                    .id(("layer", l as usize))
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .h(px(26.))
                    .px(px(8.))
                    .rounded(px(13.))
                    .border_1()
                    .border_color(if on { c.opacity(0.6) } else { t.border })
                    .bg(if on { c.opacity(0.10) } else { t.surface })
                    .text_size(px(12.))
                    .text_color(if on { t.ink } else { t.ink_muted })
                    .cursor_pointer()
                    .child(div().size(px(8.)).rounded_full().bg(if on { c } else { t.border_strong }))
                    .child(layer_label(l))
                    .when(n > 0, |d| d.child(div().text_color(t.ink_muted).child(n.to_string())))
                    .tooltip(|w, cx| Tooltip::new(i18n::t!("app.layer.toggle_tip")).build(w, cx))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_layer(l, cx))),
            );
        }
        let small = |id: &'static str, ic: IconName, tip: String| ui::icon_button(t, id, ic, true).size(px(28.)).rounded(px(7.)).tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx));
        let zoom = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .child(small("zoom-out", IconName::Minus, i18n::t!("app.map.zoom_out")).on_click(cx.listener(|this, _, _, cx| this.zoom_key(Some(1. / 1.2), cx))))
            .child(div().id("zoom-fit").w(px(44.)).text_center().text_size(px(11.5)).text_color(t.ink_muted).cursor_pointer().child(format!("{:.0}%", self.view.zoom * 100.)).tooltip(|w, cx| Tooltip::new(i18n::t!("app.map.zoom_reset")).build(w, cx)).on_click(cx.listener(|this, _, _, cx| this.zoom_key(None, cx))))
            .child(small("zoom-in", IconName::Plus, i18n::t!("app.map.zoom_in")).on_click(cx.listener(|this, _, _, cx| this.zoom_key(Some(1.2), cx))));
        let paused = self.mon.paused;
        let monitor = ui::outline_button(t, "monitor-toggle")
            .h(px(28.))
            .px(px(9.))
            .text_size(px(12.))
            .child(icon(if paused { IconName::Play } else { IconName::Pause }, 13., t.ink))
            .child(if paused { i18n::t!("app.monitor.resume") } else { i18n::t!("app.monitor.pause") })
            .on_click(cx.listener(|this, _, _, cx| this.toggle_monitor(cx)));
        let alerts = self.mon.alerts.len();
        div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .py(px(8.))
            .border_b_1()
            .border_color(t.border)
            .bg(t.surface)
            .child(mode)
            .when(!list, |d| d.child(layout))
            .child(layers)
            .when(list, |d| d.child(self.sort_menu(cx)))
            .child(div().flex_1())
            .when(alerts > 0, |d| {
                d.child(
                    div()
                        .id("alerts-pill")
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .h(px(26.))
                        .px(px(8.))
                        .rounded(px(13.))
                        .bg(t.danger.opacity(0.12))
                        .text_color(t.danger)
                        .text_size(px(12.))
                        .cursor_pointer()
                        .child(icon(IconName::Bell, 13., t.danger))
                        .child(alerts.to_string())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.selected = None;
                            cx.notify();
                        })),
                )
            })
            .child(monitor)
            .when(!list, |d| d.child(zoom))
    }

    fn sort_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let cur = self.view.sort;
        let label = |s: Sort| match s {
            Sort::Name => i18n::t!("app.list.col_name"),
            Sort::Kind => i18n::t!("app.list.col_type"),
            Sort::Ip => i18n::t!("app.list.col_ip"),
            Sort::Status => i18n::t!("app.list.col_status"),
            Sort::LastSeen => i18n::t!("app.list.col_seen"),
            Sort::Layer => i18n::t!("app.list.col_layers"),
            Sort::Owner => i18n::t!("app.list.col_owner"),
        };
        let weak = cx.entity().downgrade();
        Button::new("list-sort").ghost().small().icon(if self.view.sort_desc { IconName::SortDescending } else { IconName::SortAscending }).label(i18n::t!("app.list.sort_by", by = label(cur))).dropdown_menu(move |mut menu, _, _| {
            for s in [Sort::Name, Sort::Kind, Sort::Ip, Sort::Status, Sort::LastSeen, Sort::Layer, Sort::Owner] {
                let w = weak.clone();
                menu = menu.item(PopupMenuItem::new(label(s)).checked(s == cur).on_click(move |_, _, cx| {
                    w.update(cx, |this, cx| this.sort_by(s, cx)).ok();
                }));
            }
            menu
        })
    }

    fn sort_by(&mut self, s: Sort, cx: &mut Context<Self>) {
        if self.view.sort == s {
            self.view.sort_desc = !self.view.sort_desc;
        } else {
            self.view.sort = s;
            self.view.sort_desc = false;
        }
        cx.notify();
    }

    fn map_canvas(&mut self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let Some(c) = self.cached() else { return div().into_any_element() };
        let Some(doc) = self.doc.as_ref() else { return div().into_any_element() };
        let (zoom, pan) = (self.view.zoom, self.view.pan);
        let q = self.query(cx);
        let now = nodo_core::time::now();
        let matches: Vec<bool> = c
            .graph
            .nodes
            .iter()
            .map(|n| match n.node {
                NodeRef::Device(id) => q.is_empty() || doc.data.device(id).is_some_and(|d| d.haystack().contains(&q)),
                _ => q.is_empty(),
            })
            .collect();
        // edges: (from, to, colour, dashed, faint)
        let mut edges: Vec<Edge> = Vec::new();
        for (i, n) in c.graph.nodes.iter().enumerate() {
            let Some(p) = n.parent else { continue };
            let is_leaf = matches!(n.node, NodeRef::Device(_)) && !c.graph.nodes.iter().any(|x| x.parent == Some(i));
            if !c.placed.leaf_edges && is_leaf {
                continue;
            }
            let offline = match n.node {
                NodeRef::Device(id) => doc.data.device(id).is_some_and(|d| !crate::discovery::status_of(d, &self.mon.states, now).online()),
                _ => false,
            };
            let color = n.layer.map(|l| t.layer(l)).unwrap_or(t.border_strong);
            edges.push((c.placed.pos[p], c.placed.pos[i], color, false, offline || !matches[i]));
        }
        for (d, h, l) in &c.graph.overlay {
            edges.push((c.placed.pos[*d], c.placed.pos[*h], t.layer(*l), true, !matches[*d]));
        }
        let viewport = self.view.viewport.clone();
        let edge_canvas = canvas(
            move |b, _, _| viewport.set(b),
            move |b, _, window, _| {
                let ctr = b.center();
                let at = |(x, y): (f32, f32)| point(ctr.x + px(pan.0 + x * zoom), ctr.y + px(pan.1 + y * zoom));
                for (a, b2, color, dashed, faint) in &edges {
                    let (from, to) = (at(*a), at(*b2));
                    let mut pb = PathBuilder::stroke(px((if *dashed { 1.6 } else { 2.2 } * zoom).max(1.)));
                    if *dashed {
                        pb = pb.dash_array(&[px(6. * zoom.max(0.4)), px(5. * zoom.max(0.4))]);
                    }
                    pb.move_to(from);
                    pb.line_to(to);
                    if let Ok(path) = pb.build() {
                        window.paint_path(path, color.opacity(if *faint { 0.18 } else if *dashed { 0.55 } else { 0.7 }));
                    }
                }
            },
        )
        .absolute()
        .inset_0();
        let z = |v: f32| px(v * zoom);
        let mut layer = div().absolute().left(relative(0.5)).top(relative(0.5)).size_0();
        // frames (grouped layouts)
        for (k, f) in c.placed.frames.iter().enumerate() {
            let color = f.layer.map(|l| t.layer(l)).unwrap_or(t.border_strong);
            let label = match (f.hub.map(|h| c.graph.nodes[h].node), f.layer) {
                (Some(NodeRef::Network(n)), _) => doc.data.network(n).map(|n| n.name.clone()).unwrap_or_default(),
                (Some(_), _) => layer_label(Layer::Lan),
                (None, Some(l)) => layer_label(l),
                _ => String::new(),
            };
            layer = layer.child(
                div()
                    .id(("frame", k))
                    .absolute()
                    .left(z(f.x) + px(pan.0))
                    .top(z(f.y) + px(pan.1))
                    .w(z(f.w))
                    .h(z(f.h))
                    .rounded(z(16.))
                    .border(px(1.))
                    .border_color(color.opacity(0.45))
                    .bg(color.opacity(0.05))
                    .child(div().pl(z(12.)).pt(z(8.)).text_size(z(13.)).font_weight(FontWeight::SEMIBOLD).text_color(color).child(label)),
            );
        }
        for (i, n) in c.graph.nodes.iter().enumerate() {
            let pos = c.placed.pos[i];
            let el = match n.node {
                NodeRef::Device(id) => match doc.data.device(id) {
                    Some(d) => self.device_node(t, i, d, pos, n.layer, matches[i], now, cx),
                    None => continue,
                },
                NodeRef::Network(nid) => self.hub_node(t, i, nid, pos, n.layer, matches[i] || q.is_empty(), cx),
                NodeRef::Root => self.root_node(t, pos),
            };
            layer = layer.child(el);
        }
        let weak = cx.entity().downgrade();
        div()
            .id("map-canvas")
            .relative()
            .size_full()
            .overflow_hidden()
            .cursor(if self.view.drag.is_some() { CursorStyle::ClosedHand } else { CursorStyle::OpenHand })
            .on_mouse_down(MouseButton::Left, cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                this.view.drag = Some((ev.position, this.view.pan));
                this.view.moved = false;
                cx.notify();
            }))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                if let Some((start, pan)) = this.view.drag {
                    if ev.pressed_button != Some(MouseButton::Left) {
                        this.view.drag = None;
                    } else {
                        let (dx, dy) = (f32::from(ev.position.x - start.x), f32::from(ev.position.y - start.y));
                        this.view.moved |= dx.abs() + dy.abs() > 3.;
                        this.view.pan = (pan.0 + dx, pan.1 + dy);
                    }
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.view.drag = None;
                cx.notify();
            }))
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, cx| {
                let d = ev.delta.pixel_delta(px(16.));
                if ev.modifiers.platform || ev.modifiers.control {
                    this.view.zoom_by((1. + f32::from(d.y) / 300.).clamp(0.5, 2.), Some(ev.position));
                } else {
                    this.view.pan.0 += f32::from(d.x);
                    this.view.pan.1 += f32::from(d.y);
                }
                cx.notify();
            }))
            .on_pinch(move |ev: &PinchEvent, _, cx| {
                weak.update(cx, |this, cx| {
                    this.view.zoom_by(1. + ev.delta, Some(ev.position));
                    cx.notify();
                })
                .ok();
            })
            .on_click(cx.listener(|this, ev: &ClickEvent, _, cx| {
                // a click on empty space (not the end of a drag) clears the selection
                if ev.click_count() == 1 && !this.view.moved {
                    this.selected = None;
                    this.edit = None;
                    cx.notify();
                }
            }))
            .child(edge_canvas)
            .child(layer)
            .child(self.legend(t))
            .child(div().absolute().bottom(px(10.)).left(px(12.)).text_size(px(11.)).text_color(t.ink_muted).child(i18n::t!("app.map.hint", min = format!("{:.0}", ZOOM_MIN * 100.), max = format!("{:.0}", ZOOM_MAX * 100.))))
            .into_any_element()
    }

    fn legend(&self, t: &Tokens) -> Div {
        let row = |c: Hsla, label: String, dashed: bool| {
            div().flex().items_center().gap(px(6.)).child(if dashed { div().w(px(16.)).border_t_2().border_dashed().border_color(c) } else { div().w(px(16.)).h(px(2.)).bg(c) }).child(label)
        };
        let dot = |c: Hsla, label: String| div().flex().items_center().gap(px(6.)).child(div().size(px(8.)).rounded_full().bg(c)).child(label);
        let mut d = div().absolute().bottom(px(10.)).right(px(12.)).flex().flex_col().gap(px(3.)).p(px(8.)).rounded(px(8.)).bg(t.surface.opacity(0.92)).border_1().border_color(t.border).text_size(px(11.)).text_color(t.ink_muted);
        for l in Layer::ALL.into_iter().filter(|l| self.view.layers.contains(l)) {
            d = d.child(row(t.layer(l), layer_label(l), false));
        }
        d.child(row(t.ink_muted, i18n::t!("app.legend.overlay"), true)).child(dot(t.up, i18n::t!("app.status.up"))).child(dot(t.danger, i18n::t!("app.status.down"))).child(dot(t.ink_muted.opacity(0.6), i18n::t!("app.status.offline")))
    }

    fn root_node(&self, t: &Tokens, (x, y): (f32, f32)) -> AnyElement {
        let (zoom, pan) = (self.view.zoom, self.view.pan);
        let z = |v: f32| px(v * zoom);
        div()
            .absolute()
            .left(z(x - NODE_W / 2.) + px(pan.0))
            .top(z(y - HUB / 2.) + px(pan.1))
            .w(z(NODE_W))
            .flex()
            .flex_col()
            .items_center()
            .gap(z(4.))
            .child(div().size(z(HUB)).rounded_full().flex().items_center().justify_center().bg(t.surface).border(z(2.)).border_color(t.border_strong).shadow_md().child(ui::kind_svg("root", 26. * zoom, t.ink_muted)))
            .child(div().px(z(4.)).rounded(z(4.)).bg(t.canvas.opacity(0.85)).text_size(z(12.)).font_weight(FontWeight::SEMIBOLD).child(i18n::t!("core.graph.root")))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn hub_node(&self, t: &Tokens, i: usize, nid: nodo_core::model::NetworkId, (x, y): (f32, f32), layer: Option<Layer>, lit: bool, cx: &mut Context<Self>) -> AnyElement {
        let (zoom, pan) = (self.view.zoom, self.view.pan);
        let z = |v: f32| px(v * zoom);
        let Some(net) = self.doc.as_ref().and_then(|d| d.data.network(nid)) else { return div().into_any_element() };
        let color = layer.map(|l| t.layer(l)).unwrap_or(t.border_strong);
        let selected = self.selected == Some(Sel::Network(nid));
        let icon_name = match net.layer {
            Layer::Lan => "lan",
            Layer::Tailscale => "tailscale",
            Layer::Aws => "aws",
            Layer::WireGuard => "wireguard",
        };
        div()
            .id(("hub", i))
            .absolute()
            .left(z(x - NODE_W / 2.) + px(pan.0))
            .top(z(y - HUB / 2.) + px(pan.1))
            .w(z(NODE_W))
            .flex()
            .flex_col()
            .items_center()
            .gap(z(3.))
            .cursor_pointer()
            .when(!lit, |d| d.opacity(0.35))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().size(z(HUB)).rounded(z(14.)).flex().items_center().justify_center().bg(color.opacity(0.14)).border(z(if selected { 3. } else { 2. })).border_color(if selected { t.accent } else { color }).shadow_md().child(ui::kind_svg(icon_name, 26. * zoom, color)))
            .child(div().max_w(z(NODE_W)).truncate().px(z(4.)).rounded(z(4.)).bg(t.canvas.opacity(0.85)).text_size(z(12.)).font_weight(FontWeight::SEMIBOLD).child(net.name.clone()))
            .when_some(net.cidr.clone().filter(|c| *c != net.name), |d, c| d.child(div().text_size(z(10.5)).font_family(MONO).text_color(t.ink_muted).child(c)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.selected = Some(Sel::Network(nid));
                this.edit = None;
                cx.notify();
            }))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn device_node(&self, t: &Tokens, i: usize, d: &Device, (x, y): (f32, f32), layer: Option<Layer>, lit: bool, now: i64, cx: &mut Context<Self>) -> AnyElement {
        let (zoom, pan) = (self.view.zoom, self.view.pan);
        let z = |v: f32| px(v * zoom);
        let st = crate::discovery::status_of(d, &self.mon.states, now);
        let color = layer.map(|l| t.layer(l)).unwrap_or(t.border_strong);
        let selected = self.selected == Some(Sel::Device(d.id));
        let online = st.online();
        let ring = if online { color } else { t.border_strong };
        let id = d.id;
        let is_me = d.fact("this_computer").is_some();
        let circle = div()
            .relative()
            .size(z(CIRCLE))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(if selected { t.accent_soft } else { t.surface })
            .border(z(if selected { 3. } else { 2. }))
            .border_color(if selected { t.accent } else { ring })
            .shadow_md()
            .child(ui::kind_svg(d.kind.id(), 22. * zoom, if online { t.ink } else { t.ink_muted }))
            .child(div().absolute().top(z(-1.)).right(z(-1.)).size(z(12.)).rounded_full().border(z(2.)).border_color(t.surface).bg(crate::map_view::status_color(t, st)))
            .when(d.saved, |c| c.child(div().absolute().bottom(z(-2.)).right(z(-4.)).child(icon(IconName::Star, 11. * zoom, t.tag))));
        let menu = self.device_menu(d, cx);
        div()
            .id(("dev", i))
            .absolute()
            .left(z(x - NODE_W / 2.) + px(pan.0))
            .top(z(y - CIRCLE / 2.) + px(pan.1))
            .w(z(NODE_W))
            .flex()
            .flex_col()
            .items_center()
            .gap(z(3.))
            .cursor_pointer()
            .when(!lit, |n| n.opacity(0.22))
            .when(lit && !online, |n| n.opacity(0.55))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(circle)
            .child(div().max_w(z(NODE_W)).truncate().px(z(4.)).rounded(z(4.)).bg(t.canvas.opacity(0.85)).text_size(z(12.)).font_weight(if is_me { FontWeight::BOLD } else { FontWeight::MEDIUM }).child(d.name.clone()))
            .when(zoom >= 0.45, |n| n.child(div().px(z(4.)).rounded(z(4.)).bg(t.canvas.opacity(0.85)).text_size(z(10.5)).font_family(MONO).text_color(t.ink_muted).child(d.primary_ip().map(|i| i.to_string()).unwrap_or_default())))
            .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.select_device(id, cx);
                if ev.click_count() == 2 {
                    this.open_web(id, cx);
                }
            }))
            .context_menu(move |m, window, cx| menu.build(m, window, cx))
            .into_any_element()
    }

    pub(crate) fn device_menu(&self, d: &Device, cx: &mut Context<Self>) -> DeviceMenu {
        DeviceMenu { weak: cx.entity().downgrade(), id: d.id, has_ip: d.primary_ip().is_some(), has_mac: d.macs().next().is_some(), saved: d.saved }
    }

    fn map_list(&mut self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let Some(doc) = self.doc.as_ref() else { return div().into_any_element() };
        let q = self.query(cx);
        let now = nodo_core::time::now();
        let mut rows: Vec<&Device> = doc.data.devices.iter().filter(|d| q.is_empty() || d.haystack().contains(&q)).filter(|d| d.layers().iter().any(|l| self.view.layers.contains(l)) || d.addrs.is_empty()).collect();
        let st = |d: &Device| crate::discovery::status_of(d, &self.mon.states, now);
        let rank = |s: Status| match s {
            Status::Down => 0,
            Status::Up(_) => 1,
            Status::Offline => 2,
            Status::Unknown => 3,
        };
        match self.view.sort {
            Sort::Name => rows.sort_by_key(|d| d.name.to_lowercase()),
            Sort::Kind => rows.sort_by_key(|d| (kind_label(d.kind), d.name.to_lowercase())),
            Sort::Ip => rows.sort_by_key(|d| (d.primary_ip().is_none(), d.primary_ip())),
            Sort::Status => rows.sort_by_key(|d| (rank(st(d)), d.name.to_lowercase())),
            Sort::LastSeen => rows.sort_by_key(|d| std::cmp::Reverse(d.last_seen)),
            Sort::Layer => rows.sort_by_key(|d| (d.layers().first().copied(), d.name.to_lowercase())),
            Sort::Owner => rows.sort_by_key(|d| (d.owner.trim().is_empty(), d.owner.trim().to_lowercase(), d.name.to_lowercase())),
        }
        if self.view.sort_desc {
            rows.reverse();
        }
        let head = |id: &'static str, w: Option<f32>, s: String, sort: Sort, cx: &mut Context<Self>| {
            let on = self.view.sort == sort;
            let d = div().id(id).flex().items_center().gap(px(3.)).text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(if on { t.ink } else { t.ink_muted }).cursor_pointer().truncate().child(s).when(on, |d| d.child(icon(if self.view.sort_desc { IconName::ChevronDown } else { IconName::ChevronUp }, 11., t.ink))).on_click(cx.listener(move |this, _, _, cx| this.sort_by(sort, cx)));
            match w {
                Some(w) => d.w(px(w)).flex_none(),
                None => d.flex_1().min_w_0(),
            }
        };
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .h(px(30.))
            .px(px(12.))
            .border_b_1()
            .border_color(t.border)
            .bg(t.surface)
            .child(head("h-status", Some(90.), i18n::t!("app.list.col_status"), Sort::Status, cx))
            .child(head("h-name", None, i18n::t!("app.list.col_name"), Sort::Name, cx))
            .child(head("h-type", Some(110.), i18n::t!("app.list.col_type"), Sort::Kind, cx))
            .child(head("h-ip", Some(130.), i18n::t!("app.list.col_ip"), Sort::Ip, cx))
            .child(div().w(px(140.)).flex_none().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink_muted).child(i18n::t!("app.list.col_mac")))
            .child(div().w(px(150.)).flex_none().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink_muted).child(i18n::t!("app.list.col_vendor")))
            .child(head("h-layers", Some(150.), i18n::t!("app.list.col_layers"), Sort::Layer, cx))
            .child(head("h-seen", Some(110.), i18n::t!("app.list.col_seen"), Sort::LastSeen, cx));
        let hover = if t.dark { t.border } else { t.canvas };
        let mut list = div().id("dev-list").flex().flex_col().flex_1().min_h_0().overflow_y_scroll();
        let group = self.view.sort == Sort::Owner;
        for (k, d) in rows.iter().enumerate() {
            let owner = d.owner.trim();
            if group && (k == 0 || !rows[k - 1].owner.trim().eq_ignore_ascii_case(owner)) {
                let n = rows.iter().filter(|x| x.owner.trim().eq_ignore_ascii_case(owner)).count();
                let title = if owner.is_empty() { i18n::t!("app.list.no_owner") } else { owner.to_string() };
                list = list.child(div().flex().flex_none().items_center().gap(px(8.)).h(px(28.)).px(px(12.)).bg(t.canvas).border_b_1().border_color(t.border).text_size(px(11.5)).font_weight(FontWeight::SEMIBOLD).child(icon(IconName::User, 12., t.ink_muted)).child(title).child(div().text_color(t.ink_muted).font_weight(FontWeight::NORMAL).child(i18n::tn!("app.devices.count", n))));
            }
            let s = st(d);
            let selected = self.selected == Some(Sel::Device(d.id));
            let id = d.id;
            let menu = self.device_menu(d, cx);
            let layers = d.layers();
            list = list.child(
                div()
                    .id(("row", k))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(34.))
                    .px(px(12.))
                    .border_b_1()
                    .border_color(t.border.opacity(0.5))
                    .text_size(px(12.5))
                    .cursor_pointer()
                    .when(selected, |r| r.bg(t.accent_soft))
                    .when(!selected, |r| r.hover(move |x| x.bg(hover)))
                    .when(!s.online(), |r| r.text_color(t.ink_muted))
                    .child(div().w(px(90.)).flex_none().flex().items_center().gap(px(6.)).child(div().size(px(8.)).rounded_full().bg(status_color(t, s))).child(div().text_size(px(11.5)).truncate().child(status_label(s))))
                    .child(div().flex().flex_1().min_w_0().items_center().gap(px(6.)).child(ui::kind_svg(d.kind.id(), 15., t.ink_muted)).child(div().truncate().child(d.name.clone())).when(d.saved, |r| r.child(icon(IconName::Star, 11., t.tag))))
                    .child(div().w(px(110.)).flex_none().truncate().text_size(px(11.5)).child(kind_label(d.kind)))
                    .child(div().w(px(130.)).flex_none().truncate().font_family(MONO).text_size(px(11.5)).child(d.primary_ip().map(|i| i.to_string()).unwrap_or_default()))
                    .child(div().w(px(140.)).flex_none().truncate().font_family(MONO).text_size(px(11.)).text_color(t.ink_muted).child(d.macs().next().unwrap_or_default().to_string()))
                    .child(div().w(px(150.)).flex_none().truncate().text_size(px(11.5)).text_color(t.ink_muted).child(d.vendor.as_deref().map(nodo_core::mac::short_vendor).unwrap_or_default()))
                    .child(div().w(px(150.)).flex_none().flex().gap(px(3.)).overflow_hidden().children(layers.into_iter().map(|l| ui::chip(layer_label(l), t.layer(l), 10.5))))
                    .child(div().w(px(110.)).flex_none().text_size(px(11.5)).text_color(t.ink_muted).child(if d.last_seen > 0 { crate::util::ago(d.last_seen, now) } else { String::new() }))
                    .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                        this.select_device(id, cx);
                        if ev.click_count() == 2 {
                            this.open_web(id, cx);
                        }
                    }))
                    .context_menu(move |m, window, cx| menu.build(m, window, cx)),
            );
        }
        if rows.is_empty() {
            list = list.child(div().p(px(20.)).text_size(px(13.)).text_color(t.ink_muted).child(i18n::t!("app.search.none")));
        }
        div().flex().flex_col().size_full().bg(t.surface).child(header).child(list).into_any_element()
    }
}

/// What a device's right-click menu needs.
pub(crate) struct DeviceMenu {
    weak: WeakEntity<NodoApp>,
    id: DeviceId,
    has_ip: bool,
    has_mac: bool,
    saved: bool,
}

impl DeviceMenu {
    fn item(&self, label: String, enabled: bool, f: impl Fn(&mut NodoApp, DeviceId, &mut Window, &mut Context<NodoApp>) + 'static) -> PopupMenuItem {
        let (w, id) = (self.weak.clone(), self.id);
        PopupMenuItem::new(label).disabled(!enabled).on_click(move |_, window, cx| {
            w.update(cx, |this, cx| f(this, id, window, cx)).ok();
        })
    }

    pub fn build(&self, menu: PopupMenu, _window: &mut Window, _cx: &mut Context<PopupMenu>) -> PopupMenu {
        let ip = self.has_ip;
        menu.item(self.item(i18n::t!("app.action.ssh"), ip, |this, id, _, cx| this.ssh(id, cx)))
            .item(self.item(i18n::t!("app.action.web"), ip, |this, id, _, cx| this.open_web(id, cx)))
            .item(self.item(i18n::t!("app.action.vnc"), ip, |this, id, _, cx| this.open_vnc(id, cx)))
            .separator()
            .item(self.item(i18n::t!("app.action.copy_ip"), ip, |this, id, _, cx| this.copy_ip(id, cx)))
            .item(self.item(i18n::t!("app.action.copy_mac"), self.has_mac, |this, id, _, cx| this.copy_mac(id, cx)))
            .item(self.item(i18n::t!("app.action.wake"), self.has_mac, |this, id, _, cx| this.wake(id, cx)))
            .separator()
            .item(self.item(i18n::t!("app.action.ping"), ip, |this, id, _, cx| this.run_tool(id, nodo_core::tools::Tool::Ping, cx)))
            .item(self.item(i18n::t!("app.action.traceroute"), ip, |this, id, _, cx| this.run_tool(id, nodo_core::tools::Tool::Traceroute, cx)))
            .separator()
            .item(self.item(if self.saved { i18n::t!("app.action.unsave") } else { i18n::t!("app.action.save") }, true, |this, id, _, cx| this.toggle_saved(id, cx)))
            .item(self.item(i18n::t!("app.action.edit"), true, |this, id, window, cx| this.start_edit(id, window, cx)))
            .item(self.item(i18n::t!("app.action.merge"), true, |this, id, window, cx| this.open_merge(id, window, cx)))
    }
}
