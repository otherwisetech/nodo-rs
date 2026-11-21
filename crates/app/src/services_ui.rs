//! Monitored services in the app: the engine next to the device monitor (only while a map is
//! open), hourly buckets written to the map at most every 10 minutes, incidents and alerts, the
//! Status view, the add / edit dialog and the graph badge.
use crate::app::NodoApp;
use crate::dialogs::Dialog;
use crate::theme::{MONO, Tokens};
use crate::ui;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::IconName;
use gpui_component::button::Button;
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::Sizable as _;
use nodo_core::db::Incident;
use nodo_core::model::DeviceId;
use nodo_core::services::{CheckKind, DAY, Engine, Health, Hour, ServiceCheck, ServiceId, Spec, SvcEvent, SvcState};
use std::collections::HashMap;
use std::sync::Arc;

const FLUSH_EVERY: i64 = 10 * 60;

#[derive(Default)]
pub struct SvcUi {
    pub engine: Option<Engine>,
    pub list: Vec<ServiceCheck>,
    pub states: HashMap<ServiceId, SvcState>,
    /// Checks not yet written to the file, per service and hour.
    pending: HashMap<(ServiceId, i64), Hour>,
    last_flush: i64,
    /// Last 30 days from the file.
    pub hours: HashMap<ServiceId, Vec<Hour>>,
    pub incidents: Vec<Incident>,
}

pub struct ServiceForm {
    pub id: ServiceId,
    pub device: Option<DeviceId>,
    pub kind: CheckKind,
    pub name: Entity<InputState>,
    pub target: Entity<InputState>,
    pub interval: Entity<InputState>,
    pub keyword: Entity<InputState>,
    pub slow: Entity<InputState>,
    pub cert: Entity<InputState>,
    pub expect: Entity<InputState>,
}

pub fn kind_label(k: CheckKind) -> String {
    match k {
        CheckKind::Http => i18n::t!("app.svc.kind_http"),
        CheckKind::Tcp => i18n::t!("app.svc.kind_tcp"),
        CheckKind::Dns => i18n::t!("app.svc.kind_dns"),
        CheckKind::Icmp => i18n::t!("app.svc.kind_icmp"),
    }
}

fn preset_label(id: &str) -> String {
    match id {
        "website" => i18n::t!("app.svc.preset_website"),
        "api" => i18n::t!("app.svc.preset_api"),
        "keyword" => i18n::t!("app.svc.preset_keyword"),
        "ssh" => i18n::t!("app.svc.preset_ssh"),
        "dns" => i18n::t!("app.svc.preset_dns"),
        _ => i18n::t!("app.svc.preset_ping"),
    }
}

pub fn health_color(t: &Tokens, h: Option<Health>) -> Hsla {
    match h {
        Some(Health::Up) => t.up,
        Some(Health::Down) => t.danger,
        _ => t.ink_muted.opacity(0.6),
    }
}

fn pct(v: Option<f64>) -> String {
    v.map(|u| format!("{:.2}%", u * 100.)).unwrap_or_else(|| "–".into()) // i18n-ok: number
}

impl NodoApp {
    pub fn start_services(&mut self) {
        self.stop_services();
        let Some(doc) = &self.doc else { return };
        self.svc.list = doc.store.services().unwrap_or_default();
        self.reload_service_history();
        let e = Engine::start(self.svc.list.clone(), Arc::new(nodo_core::probe::SystemProber));
        e.pause(self.mon.paused);
        self.svc.engine = Some(e);
        self.svc.last_flush = nodo_core::time::now();
    }

    pub fn stop_services(&mut self) {
        self.flush_services(true);
        if let Some(e) = self.svc.engine.take() {
            e.stop();
        }
        self.svc.states.clear();
        self.svc.list.clear();
        self.svc.hours.clear();
        self.svc.incidents.clear();
    }

    pub fn pause_services(&self, on: bool) {
        if let Some(e) = &self.svc.engine {
            e.pause(on);
        }
    }

    fn reload_service_history(&mut self) {
        let Some(doc) = &self.doc else { return };
        let since = nodo_core::time::now() - 31 * DAY;
        self.svc.hours = self.svc.list.iter().map(|s| (s.id, doc.store.service_hours(s.id, since).unwrap_or_default())).collect();
        self.svc.incidents = doc.store.incidents(None, 100).unwrap_or_default();
    }

    /// Write the hour buckets: when an hour is over, every 10 minutes, or on close.
    fn flush_services(&mut self, force: bool) {
        let now = nodo_core::time::now();
        if self.svc.pending.is_empty() || (!force && now - self.svc.last_flush < FLUSH_EVERY && self.svc.pending.keys().all(|(_, h)| *h == Hour::of(now))) {
            return;
        }
        let rows: Vec<(ServiceId, Hour)> = self.svc.pending.drain().map(|((s, _), h)| (s, h)).collect();
        self.svc.last_flush = now;
        if let Some(doc) = self.doc.as_mut() {
            let _ = doc.store.add_service_hours(&rows);
        }
        self.reload_service_history();
    }

    pub fn drain_services(&mut self, cx: &mut Context<Self>) {
        let Some(e) = &self.svc.engine else { return };
        let events: Vec<SvcEvent> = e.events.try_iter().collect();
        if events.is_empty() {
            return;
        }
        self.svc.states = e.states.lock().map(|s| s.clone()).unwrap_or_default();
        let name = |this: &NodoApp, id: ServiceId| this.svc.list.iter().find(|s| s.id == id).map(|s| s.name.clone()).unwrap_or_default();
        let notify = |title: String, body: String| {
            if crate::settings::Settings::load().notify() {
                crate::discovery::notify(&title, &body);
            }
        };
        for ev in events {
            match ev {
                SvcEvent::Checked { service, at, outcome } => {
                    self.svc.pending.entry((service, Hour::of(at))).or_insert(Hour { hour: Hour::of(at), ..Default::default() }).add(&outcome);
                }
                SvcEvent::Down { service, at, detail } => {
                    if let Some(doc) = self.doc.as_mut() {
                        let _ = doc.store.open_incident(service, at, &detail);
                    }
                    let n = name(self, service);
                    let body = i18n::t!("app.svc.down_body", name = n.clone(), detail = detail);
                    notify(i18n::t!("app.svc.down_title"), body.clone());
                    self.toast(body, cx);
                    self.reload_service_history();
                }
                SvcEvent::Up { service, at, down_for } => {
                    if let Some(doc) = self.doc.as_mut() {
                        let _ = doc.store.close_incident(service, at);
                    }
                    let mins = down_for.unwrap_or(0).max(60) / 60;
                    let body = i18n::t!("app.svc.up_body", name = name(self, service), minutes = mins);
                    notify(i18n::t!("app.svc.up_title"), body);
                    self.reload_service_history();
                }
                SvcEvent::Slow { service, ms, .. } => notify(i18n::t!("app.svc.slow_title"), i18n::t!("app.svc.slow_body", name = name(self, service), ms = format!("{ms:.0}"))),
                SvcEvent::CertExpiring { service, days, .. } => notify(i18n::t!("app.svc.cert_title"), i18n::t!("app.svc.cert_body", name = name(self, service), days = days)),
            }
        }
        self.flush_services(false);
        cx.notify();
    }

    pub fn service_health(&self, id: ServiceId) -> Option<Health> {
        self.svc.states.get(&id).map(|s| s.health)
    }

    /// The worst health among a device's services, for its badge.
    pub fn device_services_health(&self, device: DeviceId) -> Option<Health> {
        let hs: Vec<Option<Health>> = self.svc.list.iter().filter(|s| s.device == Some(device) && s.enabled).map(|s| self.service_health(s.id)).collect();
        if hs.is_empty() {
            return None;
        }
        Some(if hs.contains(&Some(Health::Down)) {
            Health::Down
        } else if hs.iter().all(|h| *h == Some(Health::Up)) {
            Health::Up
        } else {
            Health::Unknown
        })
    }

    fn hours_of(&self, id: ServiceId) -> Vec<Hour> {
        let mem: Vec<Hour> = self.svc.pending.iter().filter(|((s, _), _)| *s == id).map(|(_, h)| *h).collect();
        nodo_core::services::merge_hours(self.svc.hours.get(&id).map(Vec::as_slice).unwrap_or_default(), &mem)
    }

    // ── dialog ───────────────────────────────────────────────────────────────────────────────

    pub fn open_service(&mut self, existing: Option<ServiceId>, preset: Option<Spec>, device: Option<DeviceId>, window: &mut Window, cx: &mut Context<Self>) {
        let cur = existing.and_then(|id| self.svc.list.iter().find(|s| s.id == id).cloned());
        let dev = cur.as_ref().and_then(|s| s.device).or(device);
        let mut spec = cur.as_ref().map(|s| s.spec.clone()).or(preset).unwrap_or_else(|| Spec::new(CheckKind::Http, ""));
        // a device's own address in place of the preset's example
        if cur.is_none()
            && let Some(ip) = dev.and_then(|d| self.doc.as_ref()?.data.device(d)?.primary_ip())
        {
            let host = if ip.is_ipv6() { format!("[{ip}]") } else { ip.to_string() };
            spec.target = match spec.kind {
                CheckKind::Http => format!("http://{host}/"),
                CheckKind::Tcp => format!("{host}:{}", spec.target.rsplit_once(':').map(|(_, p)| p).unwrap_or("22")),
                _ => ip.to_string(),
            };
        }
        let name = cur.as_ref().map(|s| s.name.clone()).unwrap_or_else(|| dev.and_then(|d| self.doc.as_ref()?.data.device(d).map(|x| x.name.clone())).unwrap_or_default());
        let mk = |v: String, ph: String, window: &mut Window, cx: &mut Context<Self>| cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(v));
        let opt = |v: Option<u32>| v.map(|x| x.to_string()).unwrap_or_default();
        let f = ServiceForm {
            id: cur.as_ref().map(|s| s.id).unwrap_or(0),
            device: dev,
            kind: spec.kind,
            name: mk(name, i18n::t!("app.svc.name_ph"), window, cx),
            target: mk(spec.target.clone(), i18n::t!("app.svc.target_ph"), window, cx),
            interval: mk(spec.interval_s.to_string(), String::new(), window, cx),
            keyword: mk(spec.keyword.clone().unwrap_or_default(), String::new(), window, cx),
            slow: mk(opt(spec.slow_ms), String::new(), window, cx),
            cert: mk(opt(spec.cert_days), String::new(), window, cx),
            expect: mk(spec.expect_status.map(|s| s.to_string()).unwrap_or_default(), String::new(), window, cx),
        };
        self.dialog = Some(Dialog::Service(f));
        cx.notify();
    }

    pub fn save_service(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Service(f)) = self.dialog.take() else { return };
        let v = |e: &Entity<InputState>| e.read(cx).value().trim().to_string();
        let num = |e: &Entity<InputState>| v(e).parse::<u32>().ok().filter(|x| *x > 0);
        let target = v(&f.target);
        if target.is_empty() {
            self.dialog = Some(Dialog::Service(f));
            return;
        }
        let mut spec = Spec::new(f.kind, &target);
        spec.interval_s = num(&f.interval).unwrap_or(60).clamp(10, 86400);
        spec.keyword = Some(v(&f.keyword)).filter(|k| !k.is_empty());
        spec.slow_ms = num(&f.slow);
        spec.cert_days = num(&f.cert).filter(|_| f.kind == CheckKind::Http);
        spec.expect_status = v(&f.expect).parse().ok();
        let name = Some(v(&f.name)).filter(|n| !n.is_empty()).unwrap_or_else(|| target.clone());
        let s = ServiceCheck { id: f.id, name, device: f.device, spec, enabled: true };
        let Some(doc) = self.doc.as_mut() else { return };
        match doc.store.save_service(&s) {
            Ok(id) => {
                self.svc.list = doc.store.services().unwrap_or_default();
                if let Some(e) = &self.svc.engine {
                    e.set_checks(self.svc.list.clone());
                    e.check_now(id);
                }
                self.reload_service_history();
                self.view.status = true;
            }
            Err(e) => self.toast(i18n::t!("app.error.save", detail = e), cx),
        }
        cx.notify();
    }

    pub fn delete_service(&mut self, id: ServiceId, cx: &mut Context<Self>) {
        let Some(doc) = self.doc.as_mut() else { return };
        let _ = doc.store.delete_service(id);
        self.svc.pending.retain(|(s, _), _| *s != id);
        self.svc.list = doc.store.services().unwrap_or_default();
        if let Some(e) = &self.svc.engine {
            e.set_checks(self.svc.list.clone());
        }
        self.reload_service_history();
        cx.notify();
    }

    pub fn check_service_now(&mut self, id: ServiceId, cx: &mut Context<Self>) {
        if let Some(e) = &self.svc.engine {
            e.check_now(id);
        }
        self.toast(i18n::t!("app.svc.checking"), cx);
    }

    pub(crate) fn service_form(&self, f: &ServiceForm, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let label = |s: String| div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).child(s);
        let hint = |s: String| div().text_size(px(11.5)).text_color(t.ink_muted).child(s);
        let mut kinds = ui::seg_group(t);
        for k in CheckKind::ALL {
            kinds = kinds.child(ui::seg(t, ("svc-kind", k as usize), kind_label(k), f.kind == k).on_click(cx.listener(move |this, _, _, cx| {
                if let Some(Dialog::Service(f)) = this.dialog.as_mut() {
                    f.kind = k;
                }
                cx.notify();
            })));
        }
        let target_hint = match f.kind {
            CheckKind::Http => i18n::t!("app.svc.target_http"),
            CheckKind::Tcp => i18n::t!("app.svc.target_tcp"),
            CheckKind::Dns => i18n::t!("app.svc.target_dns"),
            CheckKind::Icmp => i18n::t!("app.svc.target_icmp"),
        };
        let row = |a: AnyElement, b: AnyElement| div().flex().gap(px(10.)).child(div().flex_1().flex().flex_col().gap(px(4.)).child(a)).child(div().flex_1().flex().flex_col().gap(px(4.)).child(b));
        let field = |l: String, e: &Entity<InputState>| div().flex().flex_col().gap(px(4.)).child(label(l)).child(Input::new(e).small()).into_any_element();
        let mut b = div().flex().flex_col().gap(px(8.)).child(kinds).child(label(i18n::t!("app.svc.name"))).child(Input::new(&f.name).small()).child(label(i18n::t!("app.svc.target"))).child(Input::new(&f.target).small()).child(hint(target_hint));
        b = b.child(row(field(i18n::t!("app.svc.interval"), &f.interval), field(i18n::t!("app.svc.slow"), &f.slow)));
        match f.kind {
            CheckKind::Http => b = b.child(row(field(i18n::t!("app.svc.keyword"), &f.keyword), field(i18n::t!("app.svc.expect"), &f.expect))).child(row(field(i18n::t!("app.svc.cert"), &f.cert), div().into_any_element())),
            CheckKind::Dns => b = b.child(field(i18n::t!("app.svc.dns_expect"), &f.keyword)),
            _ => {}
        }
        if let Some(d) = f.device.and_then(|d| self.doc.as_ref()?.data.device(d)) {
            b = b.child(hint(i18n::t!("app.svc.for_device", name = d.name.clone())));
        }
        b.child(hint(i18n::t!("app.svc.runs_open"))).into_any_element()
    }

    // ── Status view ──────────────────────────────────────────────────────────────────────────

    pub(crate) fn add_service_menu(&self, id: &'static str, device: Option<DeviceId>, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        Button::new(id).outline().small().icon(IconName::Plus).label(i18n::t!("app.svc.add")).dropdown_menu(move |mut menu, _, _| {
            for (pid, spec) in nodo_core::services::presets() {
                let w = weak.clone();
                menu = menu.item(PopupMenuItem::new(preset_label(pid)).on_click(move |_, window, cx| {
                    let spec = spec.clone();
                    w.update(cx, |this, cx| this.open_service(None, Some(spec), device, window, cx)).ok();
                }));
            }
            let w = weak.clone();
            menu.separator().item(PopupMenuItem::new(i18n::t!("app.svc.custom")).on_click(move |_, window, cx| {
                w.update(cx, |this, cx| this.open_service(None, None, device, window, cx)).ok();
            }))
        })
    }

    pub(crate) fn render_status(&mut self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let now = nodo_core::time::now();
        let hover = if t.dark { t.border } else { t.canvas };
        let head = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .h(px(30.))
            .px(px(12.))
            .border_b_1()
            .border_color(t.border)
            .bg(t.surface)
            .text_size(px(11.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(t.ink_muted)
            .child(div().flex_1().min_w_0().child(i18n::t!("app.svc.col_service")))
            .child(div().w(px(90.)).flex_none().child(i18n::t!("app.svc.col_latency")))
            .child(div().w(px(130.)).flex_none().child(i18n::t!("app.svc.col_trend")))
            .child(div().w(px(70.)).flex_none().child(i18n::t!("app.svc.col_24h")))
            .child(div().w(px(70.)).flex_none().child(i18n::t!("app.svc.col_7d")))
            .child(div().w(px(70.)).flex_none().child(i18n::t!("app.svc.col_30d")))
            .child(div().w(px(80.)).flex_none().child(i18n::t!("app.svc.col_cert")))
            .child(div().w(px(84.)).flex_none());
        let mut list = div().id("svc-list").flex().flex_col().flex_1().min_h_0().overflow_y_scroll();
        let services = self.svc.list.clone();
        for (k, s) in services.iter().enumerate() {
            let st = self.svc.states.get(&s.id);
            let h = st.map(|x| x.health);
            let last = st.and_then(|x| x.last.clone());
            let hours = self.hours_of(s.id);
            let dev = s.device.and_then(|d| self.doc.as_ref()?.data.device(d)).map(|d| d.name.clone());
            let id = s.id;
            // the sparkline: recent checks this session, bars scaled to the slowest
            let recent: Vec<Option<f64>> = st.map(|x| x.recent.iter().copied().collect()).unwrap_or_default();
            let max = recent.iter().flatten().fold(1f64, |a, b| a.max(*b));
            let mut spark = div().w(px(130.)).flex_none().h(px(18.)).flex().items_end().gap(px(1.));
            for r in recent.iter().rev().take(32).rev() {
                spark = spark.child(match r {
                    Some(ms) => div().w(px(3.)).h(px((2. + 16. * (*ms / max)) as f32)).rounded(px(1.)).bg(t.accent.opacity(0.75)),
                    None => div().w(px(3.)).h(px(18.)).rounded(px(1.)).bg(t.danger.opacity(0.8)),
                });
            }
            let cert = last.as_ref().and_then(|o| o.cert_days).map(|d| (d, s.spec.cert_days.is_some_and(|w| d <= w as i64)));
            let detail = last.as_ref().map(|o| o.detail.clone()).unwrap_or_else(|| i18n::t!("app.svc.waiting"));
            list = list.child(
                div()
                    .id(("svc-row", k))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .min_h(px(46.))
                    .px(px(12.))
                    .border_b_1()
                    .border_color(t.border.opacity(0.5))
                    .text_size(px(12.5))
                    .hover(move |x| x.bg(hover))
                    .when(!s.enabled, |r| r.opacity(0.5))
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap(px(8.))
                            .child(div().size(px(9.)).flex_none().rounded_full().bg(health_color(t, h)))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .min_w_0()
                                    .child(div().flex().gap(px(6.)).items_center().child(div().truncate().font_weight(FontWeight::MEDIUM).child(s.name.clone())).child(ui::chip(kind_label(s.spec.kind), t.ink_muted, 10.)).when_some(dev, |d, n| d.child(div().text_size(px(11.)).text_color(t.ink_muted).truncate().child(n))))
                                    .child(div().flex().gap(px(6.)).text_size(px(11.)).text_color(t.ink_muted).child(div().font_family(MONO).truncate().child(s.spec.target.clone())).child(div().truncate().child(detail))),
                            ),
                    )
                    .child(div().w(px(90.)).flex_none().font_family(MONO).text_size(px(11.5)).when(last.as_ref().is_some_and(|o| o.slow), |d| d.text_color(t.danger)).child(last.as_ref().and_then(|o| o.latency_ms).map(|l| format!("{l:.0} ms")).unwrap_or_else(|| "–".into()))) // i18n-ok: unit
                    .child(spark)
                    .child(div().w(px(70.)).flex_none().font_family(MONO).text_size(px(11.5)).child(pct(nodo_core::services::uptime(&hours, now, DAY))))
                    .child(div().w(px(70.)).flex_none().font_family(MONO).text_size(px(11.5)).child(pct(nodo_core::services::uptime(&hours, now, 7 * DAY))))
                    .child(div().w(px(70.)).flex_none().font_family(MONO).text_size(px(11.5)).child(pct(nodo_core::services::uptime(&hours, now, 30 * DAY))))
                    .child(div().w(px(80.)).flex_none().text_size(px(11.5)).when(cert.is_some_and(|(_, warn)| warn), |d| d.text_color(t.danger)).child(cert.map(|(d, _)| i18n::t!("app.svc.days", n = d)).unwrap_or_default()))
                    .child(
                        div()
                            .w(px(84.))
                            .flex_none()
                            .flex()
                            .gap(px(2.))
                            .child(ui::icon_button(t, ("svc-now", k), IconName::Redo, true).size(px(26.)).on_click(cx.listener(move |this, _, _, cx| this.check_service_now(id, cx))))
                            .child(ui::icon_button(t, ("svc-edit", k), IconName::Settings, true).size(px(26.)).on_click(cx.listener(move |this, _, window, cx| this.open_service(Some(id), None, None, window, cx))))
                            .child(ui::icon_button(t, ("svc-del", k), IconName::Delete, true).size(px(26.)).on_click(cx.listener(move |this, _, _, cx| this.delete_service(id, cx)))),
                    ),
            );
        }
        if services.is_empty() {
            list = list.child(div().p(px(24.)).flex().flex_col().gap(px(10.)).items_start().child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(i18n::t!("app.svc.empty_title"))).child(div().max_w(px(520.)).text_size(px(12.5)).line_height(px(18.)).text_color(t.ink_muted).child(i18n::t!("app.svc.empty_body"))).child(self.add_service_menu("svc-add-empty", None, cx)));
        }
        // incidents
        let mut inc = div().flex().flex_col().gap(px(3.)).p(px(12.)).border_t_1().border_color(t.border).bg(t.surface).max_h(px(220.)).child(div().flex().items_center().justify_between().child(ui::section_title(t, i18n::t!("app.svc.incidents"))).child(self.add_service_menu("svc-add", None, cx)));
        let names: HashMap<ServiceId, String> = self.svc.list.iter().map(|s| (s.id, s.name.clone())).collect();
        if self.svc.incidents.is_empty() {
            inc = inc.child(div().text_size(px(12.)).text_color(t.ink_muted).child(i18n::t!("app.svc.no_incidents")));
        }
        for i in self.svc.incidents.iter().take(12) {
            let dur = i.end.unwrap_or(now) - i.start;
            let d = if dur >= 3600 { format!("{}h {}m", dur / 3600, dur % 3600 / 60) } else { format!("{}m", (dur / 60).max(1)) }; // i18n-ok: duration
            inc = inc.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(12.))
                    .child(div().size(px(7.)).rounded_full().bg(if i.end.is_none() { t.danger } else { t.ink_muted.opacity(0.6) }))
                    .child(div().w(px(180.)).truncate().font_weight(FontWeight::MEDIUM).child(names.get(&i.service).cloned().unwrap_or_default()))
                    .child(div().w(px(150.)).text_color(t.ink_muted).child(crate::util::local_time(i.start)))
                    .child(div().w(px(110.)).child(if i.end.is_none() { i18n::t!("app.svc.ongoing", d = d) } else { i18n::t!("app.svc.lasted", d = d) }))
                    .child(div().flex_1().min_w_0().truncate().text_color(t.ink_muted).child(i.cause.clone())),
            );
        }
        div().flex().flex_col().size_full().bg(t.surface).child(head).child(list).child(inc).into_any_element()
    }

    /// The device panel's services section.
    pub(crate) fn device_services(&self, t: &Tokens, id: DeviceId, cx: &mut Context<Self>) -> Div {
        let mut d = div().flex().flex_col().gap(px(4.)).child(div().flex().items_center().justify_between().child(div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink_muted).child(i18n::t!("app.svc.title"))).child(self.add_service_menu("svc-add-dev", Some(id), cx)));
        for s in self.svc.list.iter().filter(|s| s.device == Some(id)) {
            let st = self.svc.states.get(&s.id);
            let lat = st.and_then(|x| x.last.as_ref()).and_then(|o| o.latency_ms).map(|l| format!("{l:.0} ms")).unwrap_or_default(); // i18n-ok: unit
            d = d.child(div().flex().items_center().gap(px(6.)).text_size(px(12.)).child(div().size(px(7.)).rounded_full().bg(health_color(t, st.map(|x| x.health)))).child(div().flex_1().min_w_0().truncate().child(s.name.clone())).child(div().font_family(MONO).text_size(px(11.)).text_color(t.ink_muted).child(lat)));
        }
        d
    }
}
