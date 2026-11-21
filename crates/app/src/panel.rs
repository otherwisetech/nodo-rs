//! Side panel: the selected device (status, addresses, identities, services, inventory, quick
//! actions, history) or network, or the map's summary and alerts; and the inline edit form.
use crate::app::{NodoApp, Sel};
use crate::dialogs::Dialog;
use crate::discovery::AlertKind;
use crate::map_view::{kind_label, layer_label, status_color, status_label};
use crate::theme::{MONO, Tokens};
use crate::ui::{self, icon};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::Button;
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{IconName, Sizable as _};
use nodo_core::model::{Device, DeviceId, DeviceKind, EventKind, IdentKind, Layer, Link, Source};
use nodo_core::tools::Tool;
use std::net::IpAddr;

pub const PANEL_W: f32 = 340.;

pub struct EditForm {
    pub id: DeviceId,
    pub name: Entity<InputState>,
    pub kind: DeviceKind,
    pub owner: Entity<InputState>,
    pub location: Entity<InputState>,
    pub tags: Entity<InputState>,
    pub ssh_user: Entity<InputState>,
    pub links: Entity<TextareaState>,
    pub fields: Entity<TextareaState>,
    pub notes: Entity<TextareaState>,
}

/// `label url` per line (or just a URL) → links.
pub fn parse_links(s: &str) -> Vec<Link> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| match l.rsplit_once(char::is_whitespace) {
            Some((label, url)) if url.contains("://") => Link { label: label.trim().to_string(), url: url.to_string() },
            _ => Link { label: String::new(), url: l.to_string() },
        })
        .collect()
}

/// `key: value` per line → custom fields.
pub fn parse_fields(s: &str) -> Vec<(String, String)> {
    s.lines().filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).filter(|(k, _)| !k.is_empty()).collect()
}

fn event_label(k: EventKind, detail: &str) -> String {
    match k {
        EventKind::Discovered => i18n::t!("app.event.discovered", source = detail),
        EventKind::IpChanged => i18n::t!("app.event.ip_changed", detail = detail),
        EventKind::Up => if detail.is_empty() { i18n::t!("app.event.up") } else { detail.to_string() },
        EventKind::Down => i18n::t!("app.event.down"),
        EventKind::AlertDown => i18n::t!("app.event.alert_down", minutes = detail),
        EventKind::AlertNew => i18n::t!("app.event.alert_new"),
        EventKind::Merged => i18n::t!("app.event.merged", name = detail),
        EventKind::Split => i18n::t!("app.event.split"),
        EventKind::Edited => i18n::t!("app.event.edited"),
        EventKind::Scan => i18n::t!("app.event.scan", n = detail),
    }
}

fn fact_label(k: &str) -> Option<String> {
    Some(match k {
        "web_ui" => i18n::t!("app.fact.web_ui"),
        "exit_node" => i18n::t!("app.fact.exit_node"),
        "path" => i18n::t!("app.fact.path"),
        "tailscale_tags" => i18n::t!("app.fact.tags"),
        "tailscale_user" => i18n::t!("app.fact.user"),
        "instance_id" => i18n::t!("app.fact.instance_id"),
        "instance_type" => i18n::t!("app.fact.instance_type"),
        "state" => i18n::t!("app.fact.state"),
        "az" => i18n::t!("app.fact.az"),
        "public_ip" => i18n::t!("app.fact.public_ip"),
        "key_name" => i18n::t!("app.fact.key_name"),
        "security_groups" => i18n::t!("app.fact.security_groups"),
        "region" => i18n::t!("app.fact.region"),
        "profile" => i18n::t!("app.fact.profile"),
        "client_cidr" => i18n::t!("app.fact.client_cidr"),
        "vpn_protocol" => i18n::t!("app.fact.vpn_protocol"),
        "endpoint" => i18n::t!("app.fact.endpoint"),
        "allowed_ips" => i18n::t!("app.fact.allowed_ips"),
        "routes" => i18n::t!("app.fact.routes"),
        "keepalive" => i18n::t!("app.fact.keepalive"),
        "ssdp_server" => i18n::t!("app.fact.server"),
        "this_computer" => i18n::t!("app.fact.this_computer"),
        "rx_bytes" | "tx_bytes" | "name_rank" => return None,
        _ => return None,
    })
}

/// A CLI source's name, as its layer is called.
fn source_label(s: Source) -> String {
    match s {
        Source::Tailscale => layer_label(Layer::Tailscale),
        Source::Aws => layer_label(Layer::Aws),
        Source::WireGuard => layer_label(Layer::WireGuard),
        other => other.id().to_string(),
    }
}

impl NodoApp {
    fn device(&self, id: DeviceId) -> Option<&Device> {
        self.doc.as_ref()?.data.device(id)
    }

    fn ip_of(&self, id: DeviceId) -> Option<IpAddr> {
        self.device(id)?.primary_ip()
    }

    // ── quick actions ────────────────────────────────────────────────────────────────────────

    /// Open the user's terminal running `command`; the exact command line is shown the first
    /// time a terminal + command pair is used.
    pub fn launch(&mut self, command: String, cx: &mut Context<Self>) {
        self.detect_tools(cx);
        let s = crate::settings::Settings::load();
        let terms = if self.terminals.is_empty() { crate::launch::detect_terminals() } else { self.terminals.as_ref().clone() };
        let Some(term) = crate::launch::pick_terminal(s.terminal.as_deref(), &terms) else {
            self.toast(i18n::t!("app.launch.no_terminal"), cx);
            return;
        };
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from).unwrap_or_default();
        let plan = crate::launch::plan(term, &home, Some(&command), &crate::launch::user_shell());
        let key = crate::launch::approval_key(term, Some(&command));
        if s.launch_ok.contains(&key) {
            self.spawn_launch(plan, cx);
        } else {
            self.dialog = Some(Dialog::Launch { launch: plan, key });
        }
        cx.notify();
    }

    pub fn spawn_launch(&mut self, plan: crate::launch::Launch, cx: &mut Context<Self>) {
        match plan.spawn() {
            Ok(()) => {
                if let Some(c) = &plan.copy {
                    cx.write_to_clipboard(ClipboardItem::new_string(c.clone()));
                    self.toast(i18n::t!("app.launch.command_copied", command = c), cx);
                }
            }
            Err(e) => self.toast(i18n::t!("app.launch.failed", detail = e), cx),
        }
    }

    pub fn open_web(&mut self, id: DeviceId, cx: &mut Context<Self>) {
        let Some(d) = self.device(id) else { return };
        let url = d.links.first().map(|l| l.url.clone()).or_else(|| d.fact("web_ui").map(str::to_string)).or_else(|| d.primary_ip().map(|ip| if ip.is_ipv6() { format!("http://[{ip}]") } else { format!("http://{ip}") }));
        if let Some(u) = url {
            cx.open_url(&u);
        }
    }

    pub fn open_vnc(&mut self, id: DeviceId, cx: &mut Context<Self>) {
        if let Some(ip) = self.ip_of(id) {
            cx.open_url(&format!("vnc://{ip}"));
        }
    }

    pub fn copy_ip(&mut self, id: DeviceId, cx: &mut Context<Self>) {
        if let Some(ip) = self.ip_of(id) {
            self.copy(ip.to_string(), cx);
        }
    }

    pub fn copy_mac(&mut self, id: DeviceId, cx: &mut Context<Self>) {
        if let Some(m) = self.device(id).and_then(|d| d.macs().next().map(str::to_string)) {
            self.copy(m, cx);
        }
    }

    pub fn wake(&mut self, id: DeviceId, cx: &mut Context<Self>) {
        let Some(d) = self.device(id) else { return };
        let Some(mac) = d.macs().next().map(str::to_string) else { return };
        // the subnet's broadcast address when we know the LAN network
        let bcast = d.addrs.iter().filter(|a| a.layer == Layer::Lan).find_map(|a| self.doc.as_ref()?.data.network(a.network?)?.cidr.as_deref().and_then(nodo_core::cidr::Cidr::parse)?.broadcast());
        match nodo_core::wol::wake(&mac, bcast) {
            Ok(()) => self.toast(i18n::t!("app.action.wake_sent", mac = mac), cx),
            Err(e) => self.toast(i18n::t!("app.launch.failed", detail = e), cx),
        }
    }

    pub fn toggle_saved(&mut self, id: DeviceId, cx: &mut Context<Self>) {
        let Some(mut d) = self.device(id).cloned() else { return };
        d.saved = !d.saved;
        d.monitor = d.saved;
        self.save_device(d, cx);
    }

    pub fn toggle_device_monitor(&mut self, id: DeviceId, cx: &mut Context<Self>) {
        let Some(mut d) = self.device(id).cloned() else { return };
        d.monitor = !d.monitor;
        self.save_device(d, cx);
    }

    pub fn save_device(&mut self, d: Device, cx: &mut Context<Self>) {
        let Some(doc) = self.doc.as_mut() else { return };
        if let Err(e) = doc.update_device(d) {
            self.toast(i18n::t!("app.error.save", detail = e), cx);
        }
        self.retarget();
        self.touch(cx);
    }

    pub fn start_edit(&mut self, id: DeviceId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.device(id).cloned() else { return };
        self.selected = Some(Sel::Device(id));
        let mk = |v: String, window: &mut Window, cx: &mut Context<Self>| cx.new(|cx| InputState::new(window, cx).default_value(v));
        let area = |v: String, ph: String, window: &mut Window, cx: &mut Context<Self>| cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 8).placeholder(ph).default_value(v));
        let links = d.links.iter().map(|l| if l.label.is_empty() { l.url.clone() } else { format!("{} {}", l.label, l.url) }).collect::<Vec<_>>().join("\n");
        let fields = d.fields.iter().filter(|(k, _)| !nodo_core::remote::is_user_field(k)).map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n");
        let name = mk(d.name.clone(), window, cx);
        name.update(cx, |s, cx| s.focus(window, cx));
        self.edit = Some(EditForm {
            id,
            name,
            kind: d.kind,
            owner: mk(d.owner.clone(), window, cx),
            location: mk(d.location.clone(), window, cx),
            tags: mk(d.tags.join(", "), window, cx),
            ssh_user: cx.new(|cx| InputState::new(window, cx).placeholder(i18n::t!("app.edit.ssh_user_ph")).default_value(nodo_core::remote::ssh_user(&d).unwrap_or_default())),
            links: area(links, i18n::t!("app.edit.links_ph"), window, cx),
            fields: area(fields, i18n::t!("app.edit.fields_ph"), window, cx),
            notes: area(d.notes.clone(), String::new(), window, cx),
        });
        cx.notify();
    }

    pub fn save_edit(&mut self, cx: &mut Context<Self>) {
        let Some(f) = self.edit.take() else { return };
        let Some(mut d) = self.device(f.id).cloned() else { return };
        let val = |e: &Entity<InputState>| e.read(cx).value().trim().to_string();
        let name = val(&f.name);
        if !name.is_empty() && name != d.name {
            d.name = name;
            d.name_manual = true;
        }
        if f.kind != d.kind {
            d.kind = f.kind;
            d.kind_manual = true;
        }
        d.owner = val(&f.owner);
        d.location = val(&f.location);
        d.tags = val(&f.tags).split(',').map(str::trim).filter(|t| !t.is_empty()).map(str::to_string).collect();
        d.links = parse_links(&f.links.read(cx).value());
        d.fields = parse_fields(&f.fields.read(cx).value()).into_iter().filter(|(k, _)| !nodo_core::remote::is_user_field(k)).collect();
        nodo_core::remote::set_ssh_user(&mut d.fields, &val(&f.ssh_user));
        d.notes = f.notes.read(cx).value().trim().to_string();
        d.saved = true;
        if let Some(doc) = self.doc.as_mut() {
            let _ = doc.store.add_event(Some(d.id), nodo_core::time::now(), EventKind::Edited, "");
        }
        self.save_device(d, cx);
    }

    // ── rendering ────────────────────────────────────────────────────────────────────────────

    pub(crate) fn render_panel(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let panel = div().id("side-panel").flex().flex_col().flex_none().w(px(PANEL_W)).h_full().bg(t.surface).border_l_1().border_color(t.border).overflow_y_scroll();
        match self.selected {
            Some(Sel::Device(id)) => match self.device(id).cloned() {
                Some(_) if self.edit.as_ref().is_some_and(|e| e.id == id) => panel.child(self.render_edit(t, cx)).into_any_element(),
                Some(d) => panel.child(self.render_device(t, &d, cx)).into_any_element(),
                None => panel.child(self.render_summary(t, cx)).into_any_element(),
            },
            Some(Sel::Network(n)) => panel.child(self.render_network(t, n, cx)).into_any_element(),
            None => panel.child(self.render_summary(t, cx)).into_any_element(),
        }
    }

    fn render_summary(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let mut s = div().flex().flex_col().gap(px(10.)).p(px(16.));
        let Some(doc) = &self.doc else { return s };
        let now = nodo_core::time::now();
        let total = doc.data.devices.len();
        let up = doc.data.devices.iter().filter(|d| self.status(d).online()).count();
        let saved = doc.data.devices.iter().filter(|d| d.saved).count();
        s = s
            .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(doc.title()))
            .child(div().text_size(px(13.)).child(i18n::tn!("app.devices.count", total)))
            .child(div().text_size(px(13.)).text_color(t.ink_muted).child(i18n::t!("app.summary.online", up = up, total = total)))
            .child(div().text_size(px(13.)).text_color(t.ink_muted).child(i18n::tn!("app.summary.saved", saved)));
        let counts = self.layer_counts();
        for l in Layer::ALL {
            if let Some(n) = counts.get(&l) {
                s = s.child(div().flex().items_center().gap(px(6.)).text_size(px(12.5)).child(div().size(px(8.)).rounded_full().bg(t.layer(l))).child(layer_label(l)).child(div().text_color(t.ink_muted).child(n.to_string())));
            }
        }
        if let Some(at) = self.disc.last_refresh {
            s = s.child(div().text_size(px(12.)).text_color(t.ink_muted).child(i18n::t!("app.summary.refreshed", ago = crate::util::ago(at, now))));
        }
        if let Some(n) = &self.disc.local {
            s = s.child(div().text_size(px(12.)).text_color(t.ink_muted).child(i18n::t!("app.summary.lan", cidr = n.cidr(), iface = n.iface.clone())));
        }
        if !self.mon.alerts.is_empty() {
            s = s.child(div().pt(px(6.)).child(ui::section_title(t, i18n::t!("app.alert.title"))));
            for (k, a) in self.mon.alerts.iter().take(12).enumerate() {
                let id = a.device;
                let text = match a.kind {
                    AlertKind::Down { minutes } => i18n::t!("app.alert.down_body", name = a.name.clone(), minutes = minutes),
                    AlertKind::New => i18n::t!("app.alert.new_body", name = a.name.clone()),
                };
                let color = if matches!(a.kind, AlertKind::Down { .. }) { t.danger } else { t.tag };
                let hover = t.canvas;
                s = s.child(div().id(("alert", k)).flex().flex_col().gap(px(1.)).p(px(6.)).rounded(px(6.)).cursor_pointer().hover(move |x| x.bg(hover)).child(div().flex().items_center().gap(px(6.)).text_size(px(12.5)).child(icon(IconName::Bell, 12., color)).child(text)).child(div().text_size(px(11.)).text_color(t.ink_muted).child(crate::util::ago(a.at, now))).on_click(cx.listener(move |this, _, _, cx| {
                    this.select_device(id, cx);
                    this.center_on(id);
                })));
            }
            s = s.child(ui::outline_button(t, "alerts-clear").h(px(28.)).text_size(px(12.)).child(i18n::t!("app.alert.clear")).on_click(cx.listener(|this, _, _, cx| {
                this.mon.alerts.clear();
                cx.notify();
            })));
        }
        let ran: Vec<Source> = self.disc.runs.iter().map(|(s, _)| *s).collect();
        let others: Vec<&(Source, String)> = self.disc.notes.iter().filter(|(s, _)| !ran.contains(s)).collect();
        if !self.disc.runs.is_empty() || !others.is_empty() {
            s = s.child(div().pt(px(6.)).child(ui::section_title(t, i18n::t!("app.summary.notes"))));
            for (src, run) in &self.disc.runs {
                let name = source_label(*src);
                let ok = (run.devices > 0 || run.error.is_none()).then(|| i18n::tn!("app.source.ok", run.devices, source = name.clone(), time = crate::util::clock(run.at, now)));
                let bad = run.error.is_some();
                let mut row = div().flex().flex_col().gap(px(2.)).p(px(8.)).rounded(px(6.)).bg(if bad { t.tag.opacity(0.10) } else { t.up.opacity(0.08) }).text_size(px(12.)).line_height(px(17.));
                row = match ok {
                    Some(line) => row.child(div().flex().items_center().gap(px(6.)).child(div().size(px(7.)).rounded_full().bg(if bad { t.tag } else { t.up })).child(line)),
                    None => row.child(div().font_weight(FontWeight::SEMIBOLD).child(i18n::t!("app.source.failed", source = name.clone(), time = crate::util::clock(run.at, now)))),
                };
                if let Some(e) = &run.error {
                    row = row.child(div().text_color(t.ink_muted).child(e.clone()));
                }
                s = s.child(row);
            }
            for (src, n) in others {
                s = s.child(div().flex().flex_col().gap(px(2.)).p(px(8.)).rounded(px(6.)).bg(t.tag.opacity(0.10)).text_size(px(12.)).line_height(px(17.)).child(div().font_weight(FontWeight::SEMIBOLD).child(src.id().to_string())).child(n.clone()));
            }
        }
        s.child(div().pt(px(8.)).text_size(px(12.)).line_height(px(17.)).text_color(t.ink_muted).child(i18n::t!("app.summary.hint")))
    }

    fn render_network(&self, t: &Tokens, id: nodo_core::model::NetworkId, cx: &mut Context<Self>) -> Div {
        let mut s = div().flex().flex_col().gap(px(10.)).p(px(16.));
        let Some(doc) = &self.doc else { return s };
        let Some(n) = doc.data.network(id) else { return s };
        let kv = |k: String, v: String| div().flex().flex_col().gap(px(2.)).child(div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink_muted).child(k)).child(div().text_size(px(12.5)).line_height(px(17.)).child(v));
        let devices = doc.data.devices.iter().filter(|d| d.addrs.iter().any(|a| a.network == Some(id))).count();
        s = s.child(div().flex().items_center().gap(px(8.)).child(div().size(px(10.)).rounded_full().bg(t.layer(n.layer))).child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(self.network_name(n)))).child(ui::chip(layer_label(n.layer), t.layer(n.layer), 11.));
        if let Some(c) = &n.cidr {
            s = s.child(kv(i18n::t!("app.net.cidr"), c.clone()));
        }
        if let Some(g) = n.gateway {
            s = s.child(kv(i18n::t!("app.net.gateway"), g.to_string()));
        }
        if let Some(p) = n.parent.and_then(|p| doc.data.network(p)) {
            s = s.child(kv(i18n::t!("app.net.parent"), p.name.clone()));
        }
        s = s.child(kv(i18n::t!("app.net.devices"), devices.to_string()));
        for (k, v) in &n.facts {
            if !v.is_empty() && k != "bridge" {
                s = s.child(kv(k.replace('_', " "), v.clone()));
            }
        }
        s.child(ui::outline_button(t, "net-rename").w_full().h(px(30.)).text_size(px(12.5)).child(icon(IconName::Replace, 14., t.ink)).child(i18n::t!("app.net.rename")).on_click(cx.listener(move |this, _, window, cx| this.open_rename_network(id, window, cx))))
    }

    fn render_device(&self, t: &Tokens, d: &Device, cx: &mut Context<Self>) -> Div {
        let st = self.status(d);
        let id = d.id;
        let now = nodo_core::time::now();
        let kv = |k: String, v: String| div().flex().flex_col().gap(px(2.)).child(div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink_muted).child(k)).child(div().text_size(px(12.5)).line_height(px(17.)).child(v));
        let mono = |v: String| div().font_family(MONO).text_size(px(11.5)).child(v);
        let is_me = self.this_computer() == Some(id);
        let mut info = div().flex().flex_col().gap(px(10.)).p(px(16.)).border_b_1().border_color(t.border);
        if is_me {
            info = info.child(div().flex().items_center().gap(px(5.)).text_size(px(11.5)).font_weight(FontWeight::SEMIBOLD).text_color(t.accent).child(ui::kind_svg("pin", 12., t.accent)).child(i18n::t!("app.fact.this_computer")));
        }
        info = info.child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(div().size(px(40.)).flex_none().rounded_full().flex().items_center().justify_center().bg(t.canvas).border_2().border_color(if is_me { t.accent } else { status_color(t, st) }).child(ui::kind_svg(d.kind.id(), 20., t.ink)))
                .child(div().flex().flex_col().min_w_0().child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).line_height(px(20.)).child(d.name.clone())).child(div().text_size(px(12.)).text_color(t.ink_muted).child(kind_label(d.kind)))),
        );
        let mut chips = div().flex().flex_wrap().gap(px(4.)).child(ui::chip(status_label(st), status_color(t, st), 11.));
        if is_me {
            chips = chips.child(ui::chip(crate::map_view::me_badge(), t.accent, 11.));
        }
        for l in d.layers() {
            chips = chips.child(ui::chip(layer_label(l), t.layer(l), 11.));
        }
        if d.saved {
            chips = chips.child(ui::chip(i18n::t!("app.device.saved"), t.tag, 11.));
        }
        if d.monitor {
            chips = chips.child(ui::chip(i18n::t!("app.device.monitored"), t.accent, 11.));
        }
        info = info.child(chips);
        if let Some(s) = self.mon.states.get(&id) {
            let mut parts = Vec::new();
            if let Some(u) = s.uptime() {
                parts.push(i18n::t!("app.device.uptime", pct = format!("{:.0}", u * 100.)));
            }
            if let Some(ls) = s.last_seen {
                parts.push(i18n::t!("app.device.answered", ago = crate::util::ago(ls, now)));
            }
            if !parts.is_empty() {
                info = info.child(div().text_size(px(12.)).text_color(t.ink_muted).child(parts.join(" · "))); // i18n-ok: separator
            }
        }
        // addresses and identities
        let mut addrs = div().flex().flex_col().gap(px(3.));
        for a in &d.addrs {
            let net = a.network.and_then(|n| self.doc.as_ref()?.data.network(n)).map(|n| self.network_name(n)).unwrap_or_default();
            addrs = addrs.child(div().flex().items_center().gap(px(6.)).child(div().size(px(7.)).rounded_full().bg(t.layer(a.layer))).child(mono(a.ip.to_string())).child(div().text_size(px(11.)).text_color(t.ink_muted).truncate().child(net)));
        }
        if !d.addrs.is_empty() {
            info = info.child(div().flex().flex_col().gap(px(4.)).child(div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink_muted).child(i18n::t!("app.device.addresses"))).child(addrs));
        }
        // this computer's own interfaces, as the OS reports them right now
        if is_me && let Some(me) = &self.disc.me {
            use nodo_core::this_computer::IfaceKind as K;
            let mut rows = div().flex().flex_col().gap(px(4.));
            for i in me.sorted_ifaces().into_iter().filter(|i| i.kind != K::Other) {
                let (label, color) = match i.kind {
                    K::Wifi => (i18n::t!("app.iface.wifi"), t.layer(Layer::Lan)),
                    K::Ethernet => (i18n::t!("app.iface.ethernet"), t.layer(Layer::Lan)),
                    K::Tailscale => (i18n::t!("app.iface.tailscale"), t.layer(Layer::Tailscale)),
                    K::Vpn => (i18n::t!("app.iface.vpn"), t.layer(Layer::WireGuard)),
                    K::Bridge => (i18n::t!("app.iface.bridge"), t.ink_muted),
                    K::Other => (i18n::t!("app.iface.other"), t.ink_muted),
                };
                let ips: Vec<String> = i.ips.iter().map(|ip| ip.to_string()).collect();
                rows = rows.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .child(div().flex().items_center().gap(px(6.)).child(div().size(px(7.)).rounded_full().bg(color)).child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).child(label)).child(div().text_size(px(11.)).font_family(MONO).text_color(t.ink_muted).child(i.name.clone())))
                        .children(ips.into_iter().map(|ip| div().pl(px(13.)).child(mono(ip)))),
                );
            }
            info = info.child(div().flex().flex_col().gap(px(4.)).child(div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink_muted).child(i18n::t!("app.me.interfaces"))).child(rows));
        }
        for m in d.macs() {
            let vendor = nodo_core::mac::vendor(m).map(str::to_string).or_else(|| nodo_core::mac::is_random(m).then(|| i18n::t!("app.device.random_mac"))).unwrap_or_default();
            info = info.child(kv(i18n::t!("app.device.mac"), format!("{m}  {vendor}")));
        }
        for i in d.idents.iter().filter(|i| !matches!(i.kind, IdentKind::Mac | IdentKind::Host)) {
            let k = match i.kind {
                IdentKind::TailscaleNode => i18n::t!("app.ident.tailscale"),
                IdentKind::AwsInstance => i18n::t!("app.ident.aws"),
                IdentKind::WgPeer => i18n::t!("app.ident.wg"),
                _ => i18n::t!("app.ident.mdns"),
            };
            info = info.child(kv(k, i.value.clone()));
        }
        for (k, v) in [(i18n::t!("app.device.vendor"), d.vendor.clone()), (i18n::t!("app.device.model"), d.model.clone()), (i18n::t!("app.device.os"), d.os.clone()), (i18n::t!("app.device.hostname"), d.hostname.clone())] {
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                info = info.child(kv(k, v));
            }
        }
        for (k, v) in &d.facts {
            if let Some(label) = fact_label(k).filter(|_| !v.is_empty() && k != "this_computer") {
                info = info.child(kv(label, v.clone()));
            }
        }
        info = info.child(self.device_services(t, id, cx));
        if !d.services.is_empty() {
            let svc: Vec<String> = d.services.iter().map(|s| match (&s.label, s.port) {
                (Some(l), Some(p)) if !l.is_empty() => format!("{} :{p} — {l}", s.name), // i18n-ok: service line
                (_, Some(p)) => format!("{} :{p}", s.name),
                _ => s.name.clone(),
            }).collect();
            info = info.child(div().flex().flex_col().gap(px(2.)).child(div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink_muted).child(i18n::t!("app.device.services"))).children(svc.into_iter().take(14).map(|s| div().font_family(MONO).text_size(px(11.)).text_color(t.ink_muted).truncate().child(s))));
        }
        // inventory
        let owner = match self.owner_label(&d.owner) {
            Some(kind) => i18n::t!("app.link.owner_labeled", owner = d.owner, kind = kind),
            None => d.owner.clone(),
        };
        for (k, v) in [(i18n::t!("app.edit.owner"), owner), (i18n::t!("app.edit.location"), d.location.clone()), (i18n::t!("app.edit.tags"), d.tags.join(", ")), (i18n::t!("app.edit.notes"), d.notes.clone())] {
            if !v.is_empty() {
                info = info.child(kv(k, v));
            }
        }
        for (k, v) in &d.fields {
            info = info.child(kv(k.clone(), v.clone()));
        }
        for (n, l) in d.links.iter().enumerate() {
            let url = l.url.clone();
            info = info.child(div().id(("link", n)).text_size(px(12.5)).text_color(t.accent).cursor_pointer().hover(|s| s.underline()).child(if l.label.is_empty() { l.url.clone() } else { l.label.clone() }).on_click(move |_, _, cx| cx.open_url(&url)));
        }
        let seen = if d.last_seen > 0 { crate::util::ago(d.last_seen, now) } else { i18n::t!("app.device.never") };
        info = info.child(div().text_size(px(11.5)).text_color(t.ink_muted).child(i18n::t!("app.device.seen", seen = seen, first = if d.first_seen > 0 { crate::util::local_time(d.first_seen) } else { String::new() })));

        // actions
        let has_ip = d.primary_ip().is_some();
        let has_mac = d.macs().next().is_some();
        let act = |eid: &'static str, ic: IconName, label: String, enabled: bool| ui::outline_button(t, eid).flex_1().justify_center().h(px(30.)).px(px(6.)).text_size(px(12.)).child(icon(ic, 13., t.ink)).child(label).when(!enabled, |b| b.opacity(0.45).cursor_default());
        let row = || div().flex().gap(px(6.));
        let mut actions = div().flex().flex_col().gap(px(6.)).p(px(16.)).border_b_1().border_color(t.border);
        actions = actions
            .child(row().child(act("a-ssh", IconName::SquareTerminal, if is_me { i18n::t!("app.term.local_shell") } else { i18n::t!("app.action.ssh_short") }, has_ip || is_me).when(has_ip || is_me, |b| b.on_click(cx.listener(move |this, _, window, cx| this.ssh(id, window, cx))))).child(act("a-web", IconName::Globe, i18n::t!("app.action.web_short"), has_ip).when(has_ip, |b| b.on_click(cx.listener(move |this, _, _, cx| this.open_web(id, cx))))).child(act("a-vnc", IconName::Frame, i18n::t!("app.action.vnc_short"), has_ip).when(has_ip, |b| b.on_click(cx.listener(move |this, _, _, cx| this.open_vnc(id, cx))))))
            .child(row().child(act("a-ip", IconName::Copy, i18n::t!("app.action.copy_ip_short"), has_ip).when(has_ip, |b| b.on_click(cx.listener(move |this, _, _, cx| this.copy_ip(id, cx))))).child(act("a-mac", IconName::Copy, i18n::t!("app.action.copy_mac_short"), has_mac).when(has_mac, |b| b.on_click(cx.listener(move |this, _, _, cx| this.copy_mac(id, cx))))).child(act("a-wol", IconName::Sun, i18n::t!("app.action.wake_short"), has_mac).when(has_mac, |b| b.on_click(cx.listener(move |this, _, _, cx| this.wake(id, cx))))))
            .child(row().child(act("a-ping", IconName::Inspector, i18n::t!("app.action.ping"), has_ip).when(has_ip, |b| b.on_click(cx.listener(move |this, _, window, cx| this.run_tool(id, Tool::Ping, window, cx))))).child(act("a-trace", IconName::Map, i18n::t!("app.action.traceroute"), has_ip).when(has_ip, |b| b.on_click(cx.listener(move |this, _, window, cx| this.run_tool(id, Tool::Traceroute, window, cx))))));
        let saved = d.saved;
        let monitored = d.monitor;
        let weak = cx.entity().downgrade();
        let more = Button::new("a-more").outline().small().icon(IconName::Ellipsis).label(i18n::t!("app.action.more")).dropdown_menu(move |menu, _, _| {
            let item = |label: String, f: fn(&mut NodoApp, DeviceId, &mut Window, &mut Context<NodoApp>)| {
                let w = weak.clone();
                PopupMenuItem::new(label).on_click(move |_, window, cx| {
                    w.update(cx, |this, cx| f(this, id, window, cx)).ok();
                })
            };
            menu.item(item(i18n::t!("app.action.shell_here"), |this, id, window, cx| this.shell_here(id, window, cx)))
                .item(item(i18n::t!("app.link.warranty_in_vigia"), |this, id, _, cx| this.warranty_in_vigia(id, cx)))
                .item(item(i18n::t!("app.action.ssh_external"), |this, id, _, cx| this.ssh_external(id, cx)))
                .separator()
                .item(item(i18n::t!("app.share.send_file"), |this, id, _, cx| this.send_file_dialog(id, false, cx)))
                .item(item(i18n::t!("app.share.send_folder"), |this, id, _, cx| this.send_file_dialog(id, true, cx)))
                .item(item(i18n::t!("app.share.send_clipboard"), |this, id, _, cx| this.send_clipboard(id, cx)))
                .item(item(i18n::t!("app.share.send_taildrop"), |this, id, _, cx| this.send_taildrop(id, cx)))
                .item(item(i18n::t!("app.share.history"), |this, _, _, cx| this.open_transfers(cx)))
                .separator()
                .item(item(if monitored { i18n::t!("app.action.unmonitor") } else { i18n::t!("app.action.monitor") }, |this, id, _, cx| this.toggle_device_monitor(id, cx)))
                .item(item(i18n::t!("app.action.merge"), |this, id, window, cx| this.open_merge(id, window, cx)))
                .item(item(i18n::t!("app.action.split"), |this, id, _, cx| this.open_split(id, cx)))
                .separator()
                .item(item(i18n::t!("app.action.delete"), |this, id, _, cx| this.ask_delete(id, cx)))
        });
        actions = actions.child(
            row()
                .child(ui::filled_button(t, "a-save", if saved { t.border_strong } else { t.accent }).flex_1().justify_center().h(px(30.)).text_size(px(12.)).child(icon(IconName::Star, 13., t.on_fill())).child(if saved { i18n::t!("app.action.unsave") } else { i18n::t!("app.action.save") }).on_click(cx.listener(move |this, _, _, cx| this.toggle_saved(id, cx))))
                .child(ui::outline_button(t, "a-edit").h(px(30.)).text_size(px(12.)).child(i18n::t!("app.action.edit")).on_click(cx.listener(move |this, _, window, cx| this.start_edit(id, window, cx))))
                .child(more),
        );
        // history
        let mut hist = div().flex().flex_col().gap(px(4.)).p(px(16.)).child(ui::section_title(t, i18n::t!("app.device.history")));
        let events = self.doc.as_ref().and_then(|doc| doc.store.events(Some(id), 30).ok()).unwrap_or_default();
        if events.is_empty() {
            hist = hist.child(div().text_size(px(12.)).text_color(t.ink_muted).child(i18n::t!("app.device.no_history")));
        }
        for e in events {
            let c = match e.kind {
                EventKind::Down | EventKind::AlertDown => t.danger,
                EventKind::Up => t.up,
                EventKind::AlertNew => t.tag,
                _ => t.ink_muted,
            };
            hist = hist.child(div().flex().gap(px(8.)).text_size(px(11.5)).child(div().flex_none().w(px(104.)).text_color(t.ink_muted).child(crate::util::local_time(e.at))).child(div().flex_1().min_w_0().text_color(c).child(event_label(e.kind, &e.detail))));
        }
        div().flex().flex_col().child(info).child(actions).child(hist)
    }

    fn render_edit(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let Some(f) = &self.edit else { return div() };
        let label = |s: String| div().text_size(px(11.5)).font_weight(FontWeight::MEDIUM).text_color(t.ink_muted).child(s);
        let field = |s: String, e: &Entity<InputState>| div().flex().flex_col().gap(px(3.)).child(label(s)).child(Input::new(e).small());
        let area = |s: String, e: &Entity<TextareaState>| div().flex().flex_col().gap(px(3.)).child(label(s)).child(div().rounded(px(6.)).border_1().border_color(t.border).bg(t.canvas).px(px(6.)).py(px(4.)).text_size(px(12.5)).child(Textarea::new(e)));
        let weak = cx.entity().downgrade();
        let cur = f.kind;
        let kind = Button::new("edit-kind").outline().small().label(kind_label(cur)).dropdown_menu(move |mut menu, _, _| {
            for k in DeviceKind::ALL {
                let w = weak.clone();
                menu = menu.item(PopupMenuItem::new(kind_label(k)).checked(k == cur).on_click(move |_, _, cx| {
                    w.update(cx, |this, cx| {
                        if let Some(f) = this.edit.as_mut() {
                            f.kind = k;
                        }
                        cx.notify();
                    })
                    .ok();
                }));
            }
            menu
        });
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .p(px(16.))
            .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(i18n::t!("app.edit.title")))
            .child(field(i18n::t!("app.edit.name"), &f.name))
            .child(div().flex().flex_col().gap(px(3.)).child(label(i18n::t!("app.edit.kind"))).child(kind))
            .child(field(i18n::t!("app.edit.owner"), &f.owner))
            .child(field(i18n::t!("app.edit.location"), &f.location))
            .child(field(i18n::t!("app.edit.tags"), &f.tags))
            .child(field(i18n::t!("app.edit.ssh_user"), &f.ssh_user))
            .child(area(i18n::t!("app.edit.links"), &f.links))
            .child(area(i18n::t!("app.edit.fields"), &f.fields))
            .child(area(i18n::t!("app.edit.notes"), &f.notes))
            .child(div().text_size(px(11.5)).text_color(t.ink_muted).child(i18n::t!("app.edit.saves_hint")))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(ui::outline_button(t, "edit-cancel").child(i18n::t!("app.dialog.cancel")).on_click(cx.listener(|this, _, _, cx| {
                        this.edit = None;
                        cx.notify();
                    })))
                    .child(ui::filled_button(t, "edit-save", t.accent).child(i18n::t!("app.dialog.save")).on_click(cx.listener(|this, _, _, cx| this.save_edit(cx)))),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_fields, parse_links};
    use nodo_core::model::Link;

    #[test]
    fn links_and_fields() {
        let l = parse_links("Admin https://10.0.0.1:8443\nhttp://nas.local\n\n  DSM  web  http://x  ");
        assert_eq!(l, vec![Link { label: "Admin".into(), url: "https://10.0.0.1:8443".into() }, Link { label: String::new(), url: "http://nas.local".into() }, Link { label: "DSM  web".into(), url: "http://x".into() }]);
        assert_eq!(parse_fields("Rack: A2\nssh user: pi\nnope\n: empty"), vec![("Rack".to_string(), "A2".to_string()), ("ssh user".to_string(), "pi".to_string())]);
    }
}
