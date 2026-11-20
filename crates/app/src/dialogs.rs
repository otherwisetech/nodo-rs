//! Modal dialogs: scan confirmation (with the "networks you own" warning), AWS profile and
//! regions, WireGuard help, settings, add device, merge, split, delete, rename network, and
//! the terminal command shown before its first run.
use crate::app::NodoApp;
use crate::map_view::kind_label;
use crate::theme::Tokens;
use crate::ui::{self, code_block};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::Button;
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::spinner::Spinner;
use gpui_component::{IconName, Sizable as _};
use nodo_core::discover::netinfo::LocalNet;
use nodo_core::model::{DeviceId, DeviceKind, Ident, NetworkId};
use std::net::IpAddr;

pub struct AwsForm {
    pub profiles: Option<Vec<String>>,
    pub profile: Option<String>,
    pub regions: Entity<InputState>,
}

pub struct SettingsForm {
    pub interval: Entity<InputState>,
    pub down_minutes: Entity<InputState>,
    pub alert_down: bool,
    pub alert_new: bool,
    pub notify: bool,
    pub auto_refresh: bool,
    pub stars: bool,
    pub terminal: Option<String>,
}

pub struct AddForm {
    pub name: Entity<InputState>,
    pub ip: Entity<InputState>,
    pub kind: DeviceKind,
    _subs: Vec<Subscription>,
}

pub enum Dialog {
    Scan(LocalNet),
    Aws(AwsForm),
    WireGuard { note: String },
    Settings(SettingsForm),
    Add(AddForm),
    Merge { id: DeviceId, filter: Entity<InputState>, _sub: Subscription },
    Split { id: DeviceId, ips: Vec<(IpAddr, bool)>, idents: Vec<(Ident, bool)> },
    Delete { id: DeviceId, name: String },
    Rename { id: NetworkId, name: Entity<InputState> },
    /// The exact command, shown before running it the first time.
    Launch { launch: crate::launch::Launch, key: String },
}

/// Title, body, OK label, danger, ready, OK action.
type Parts = (String, AnyElement, String, bool, bool, fn(&mut NodoApp, &mut Window, &mut Context<NodoApp>));

impl NodoApp {
    pub fn open_aws(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.doc.is_none() {
            return;
        }
        let saved_profile = self.doc.as_ref().and_then(|d| d.setting("aws_profile"));
        let saved_regions = self.doc.as_ref().and_then(|d| d.setting("aws_regions")).unwrap_or_default();
        let regions = cx.new(|cx| InputState::new(window, cx).placeholder(i18n::t!("app.aws.regions_ph")).default_value(saved_regions));
        self.dialog = Some(Dialog::Aws(AwsForm { profiles: None, profile: saved_profile, regions }));
        let task = cx.background_executor().spawn(async move { nodo_core::discover::aws::list_profiles() });
        cx.spawn(async move |this, cx| {
            let p = task.await;
            this.update(cx, |this, cx| {
                if let Some(Dialog::Aws(f)) = this.dialog.as_mut() {
                    if f.profile.as_ref().is_none_or(|x| !p.contains(x)) {
                        f.profile = p.first().cloned();
                    }
                    f.profiles = Some(p);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn submit_aws(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Aws(f)) = self.dialog.take() else { return };
        let regions: Vec<String> = f.regions.read(cx).value().split([',', ' ']).map(str::trim).filter(|r| !r.is_empty()).map(str::to_string).collect();
        if let Some(p) = f.profile {
            self.run_aws(p, regions, cx);
        }
        cx.notify();
    }

    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.detect_tools(cx);
        let s = crate::settings::Settings::load();
        let cfg = self.monitor_config();
        let mk = |v: String, window: &mut Window, cx: &mut Context<Self>| cx.new(|cx| InputState::new(window, cx).default_value(v));
        let interval = mk(cfg.interval.as_secs().to_string(), window, cx);
        let down_minutes = mk((cfg.down_after.as_secs() / 60).to_string(), window, cx);
        self.dialog = Some(Dialog::Settings(SettingsForm { interval, down_minutes, alert_down: cfg.alert_down, alert_new: cfg.alert_new, notify: s.notify(), auto_refresh: s.auto_refresh(), stars: s.github_stars(), terminal: s.terminal.clone() }));
        cx.notify();
    }

    fn save_settings(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Settings(f)) = self.dialog.take() else { return };
        let num = |e: &Entity<InputState>, def: u64| e.read(cx).value().trim().parse::<u64>().unwrap_or(def);
        let (interval, minutes) = (num(&f.interval, 30).clamp(5, 3600), num(&f.down_minutes, 5).clamp(1, 1440));
        let off = |on: bool| if on { "on" } else { "off" }; // i18n-ok: setting value
        if let Some(doc) = self.doc.as_mut() {
            let _ = doc.set_setting("monitor_interval", &interval.to_string());
            let _ = doc.set_setting("alert_down_minutes", &minutes.to_string());
            let _ = doc.set_setting("alert_down", off(f.alert_down));
            let _ = doc.set_setting("alert_new", off(f.alert_new));
        }
        let stars_changed = f.stars != crate::settings::Settings::load().github_stars();
        crate::settings::Settings::update(|s| {
            s.notify = (!f.notify).then(|| "off".into()); // i18n-ok: setting value
            s.auto_refresh = (!f.auto_refresh).then(|| "off".into()); // i18n-ok: setting value
            s.terminal = f.terminal.clone();
        });
        if stars_changed {
            self.set_github_stars(f.stars, cx);
        }
        if let Some(e) = &self.mon.engine {
            e.set_config(self.monitor_config());
        }
        cx.notify();
    }

    pub fn open_add_device(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.doc.is_none() {
            return;
        }
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(i18n::t!("app.add.name_ph")));
        let ip = cx.new(|cx| InputState::new(window, cx).placeholder(i18n::t!("app.add.ip_ph")));
        name.update(cx, |s, cx| s.focus(window, cx));
        // re-check the fields as they are typed
        let on_change = |this: &mut NodoApp, _: &Entity<InputState>, ev: &InputEvent, _: &mut Window, cx: &mut Context<NodoApp>| match ev {
            InputEvent::Change => cx.notify(),
            InputEvent::PressEnter { .. } => this.submit_add(cx),
            _ => {}
        };
        let subs = vec![cx.subscribe_in(&name, window, on_change), cx.subscribe_in(&ip, window, on_change)];
        self.dialog = Some(Dialog::Add(AddForm { name, ip, kind: DeviceKind::Unknown, _subs: subs }));
        cx.notify();
    }

    fn submit_add(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Add(f)) = &self.dialog else { return };
        let name = f.name.read(cx).value().trim().to_string();
        let ip_text = f.ip.read(cx).value().trim().to_string();
        let ip = ip_text.parse::<IpAddr>().ok();
        if (ip.is_none() && !ip_text.is_empty()) || (name.is_empty() && ip.is_none()) {
            return;
        }
        let Some(Dialog::Add(f)) = self.dialog.take() else { return };
        let Some(doc) = self.doc.as_mut() else { return };
        match doc.add_manual(&name, ip, f.kind, nodo_core::time::now()) {
            Ok(id) => {
                self.retarget();
                self.touch(cx);
                self.select_device(id, cx);
            }
            Err(e) => self.toast(i18n::t!("app.error.save", detail = e), cx),
        }
    }

    pub fn open_merge(&mut self, id: DeviceId, window: &mut Window, cx: &mut Context<Self>) {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(i18n::t!("app.merge.filter_ph")));
        filter.update(cx, |s, cx| s.focus(window, cx));
        let sub = cx.subscribe_in(&filter, window, |_, _, ev: &InputEvent, _, cx| {
            if let InputEvent::Change = ev {
                cx.notify();
            }
        });
        self.dialog = Some(Dialog::Merge { id, filter, _sub: sub });
        cx.notify();
    }

    fn merge_into(&mut self, keep: DeviceId, other: DeviceId, cx: &mut Context<Self>) {
        self.dialog = None;
        let Some(doc) = self.doc.as_mut() else { return };
        match doc.merge(keep, other, nodo_core::time::now()) {
            Ok(true) => {
                self.mon.states.remove(&other);
                self.retarget();
                self.touch(cx);
                self.select_device(keep, cx);
                self.toast(i18n::t!("app.merge.done"), cx);
            }
            Ok(false) => {}
            Err(e) => self.toast(i18n::t!("app.error.save", detail = e), cx),
        }
    }

    pub fn open_split(&mut self, id: DeviceId, cx: &mut Context<Self>) {
        let Some(d) = self.doc.as_ref().and_then(|doc| doc.data.device(id)) else { return };
        let ips = d.addrs.iter().map(|a| (a.ip, false)).collect();
        let idents = d.idents.iter().filter(|i| i.kind != nodo_core::model::IdentKind::Host).map(|i| (i.clone(), false)).collect();
        self.dialog = Some(Dialog::Split { id, ips, idents });
        cx.notify();
    }

    fn submit_split(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Split { id, ips, idents }) = self.dialog.take() else { return };
        let ips: Vec<IpAddr> = ips.into_iter().filter(|(_, on)| *on).map(|(ip, _)| ip).collect();
        let idents: Vec<Ident> = idents.into_iter().filter(|(_, on)| *on).map(|(i, _)| i).collect();
        let Some(doc) = self.doc.as_mut() else { return };
        match doc.split(id, &ips, &idents, nodo_core::time::now()) {
            Ok(Some(new)) => {
                self.retarget();
                self.touch(cx);
                self.select_device(new, cx);
            }
            Ok(None) => {}
            Err(e) => self.toast(i18n::t!("app.error.save", detail = e), cx),
        }
    }

    pub fn ask_delete(&mut self, id: DeviceId, cx: &mut Context<Self>) {
        let name = self.doc.as_ref().and_then(|d| d.data.device(id)).map(|d| d.name.clone()).unwrap_or_default();
        self.dialog = Some(Dialog::Delete { id, name });
        cx.notify();
    }

    fn confirm_delete(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Delete { id, .. }) = self.dialog.take() else { return };
        if let Some(doc) = self.doc.as_mut()
            && let Err(e) = doc.delete_device(id)
        {
            self.toast(i18n::t!("app.error.save", detail = e), cx);
        }
        self.selected = None;
        self.mon.states.remove(&id);
        self.retarget();
        self.touch(cx);
    }

    pub fn open_rename_network(&mut self, id: NetworkId, window: &mut Window, cx: &mut Context<Self>) {
        let cur = self.doc.as_ref().and_then(|d| d.data.network(id)).map(|n| n.name.clone()).unwrap_or_default();
        let name = cx.new(|cx| InputState::new(window, cx).default_value(cur));
        name.update(cx, |s, cx| s.focus(window, cx));
        self.dialog = Some(Dialog::Rename { id, name });
        cx.notify();
    }

    fn submit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Rename { id, name }) = self.dialog.take() else { return };
        let v = name.read(cx).value().trim().to_string();
        if v.is_empty() {
            return;
        }
        if let Some(doc) = self.doc.as_mut()
            && let Err(e) = doc.rename_network(id, &v)
        {
            self.toast(i18n::t!("app.error.save", detail = e), cx);
        }
        self.touch(cx);
    }

    fn confirm_launch(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Launch { launch, key }) = self.dialog.take() else { return };
        crate::settings::Settings::update(|s| {
            if !s.launch_ok.contains(&key) {
                s.launch_ok.push(key);
            }
        });
        self.spawn_launch(launch, cx);
        cx.notify();
    }

    pub(crate) fn render_dialog(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = self.dialog.as_ref()?;
        let text = |s: String| div().text_size(px(13.)).line_height(px(19.)).text_color(t.ink_muted).child(s);
        let label = |s: String| div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).child(s);
        let (title, body, ok_label, danger, ready, ok): Parts = match d {
            Dialog::Scan(net) => {
                let range = nodo_core::cidr::scan_range(net.ip, net.prefix);
                let ports = nodo_core::discover::scan::PORTS.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ");
                let private = nodo_core::cidr::is_private(IpAddr::V4(net.ip));
                let b = div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(div().flex().gap(px(8.)).p(px(10.)).rounded(px(8.)).bg(t.tag.opacity(0.12)).child(crate::ui::icon(IconName::TriangleAlert, 16., t.tag)).child(div().flex_1().text_size(px(13.)).line_height(px(19.)).font_weight(FontWeight::MEDIUM).child(i18n::t!("app.scan.warning"))))
                    .child(text(i18n::t!("app.scan.body", range = range, iface = net.iface.clone())))
                    .child(code_block(t, "scan-ports", i18n::t!("app.scan.ports", ports = ports)))
                    .child(text(i18n::t!("app.scan.rate")))
                    .when(!private, |d| d.child(div().text_size(px(13.)).text_color(t.danger).child(i18n::t!("app.scan.not_private"))));
                (i18n::t!("app.scan.title"), b.into_any_element(), i18n::t!("app.scan.ok"), false, private, |this, _, cx| {
                    if let Some(Dialog::Scan(net)) = this.dialog.take() {
                        this.start_scan(net, cx);
                    }
                })
            }
            Dialog::Aws(f) => {
                let mut b = div().flex().flex_col().gap(px(10.)).child(text(i18n::t!("app.aws.body")));
                match &f.profiles {
                    None => b = b.child(div().flex().items_center().gap(px(8.)).child(Spinner::new().small().color(t.accent)).child(text(i18n::t!("app.aws.loading")))),
                    Some(p) if p.is_empty() => b = b.child(div().text_size(px(13.)).text_color(t.danger).child(i18n::t!("app.aws.no_profiles"))),
                    Some(p) => {
                        let weak = cx.entity().downgrade();
                        let list = p.clone();
                        let cur = f.profile.clone();
                        let pick = Button::new("aws-profile").outline().small().icon(IconName::User).label(cur.clone().unwrap_or_default()).dropdown_menu(move |mut menu, _, _| {
                            for name in &list {
                                let (w, n) = (weak.clone(), name.clone());
                                menu = menu.item(PopupMenuItem::new(name.clone()).checked(cur.as_deref() == Some(name.as_str())).on_click(move |_, _, cx| {
                                    w.update(cx, |this, cx| {
                                        if let Some(Dialog::Aws(f)) = this.dialog.as_mut() {
                                            f.profile = Some(n.clone());
                                        }
                                        cx.notify();
                                    })
                                    .ok();
                                }));
                            }
                            menu
                        });
                        b = b.child(div().flex().items_center().gap(px(8.)).child(label(i18n::t!("app.aws.profile"))).child(pick));
                    }
                }
                b = b.child(label(i18n::t!("app.aws.regions"))).child(Input::new(&f.regions)).child(div().text_size(px(11.5)).text_color(t.ink_muted).child(i18n::t!("app.aws.regions_hint"))).child(div().text_size(px(11.5)).text_color(t.ink_muted).child(i18n::t!("app.aws.privacy")));
                let ready = f.profile.is_some() && !self.disc.aws;
                (i18n::t!("app.aws.title"), b.into_any_element(), i18n::t!("app.aws.ok"), false, ready, |this, _, cx| this.submit_aws(cx))
            }
            Dialog::WireGuard { note } => {
                let b = div().flex().flex_col().gap(px(10.)).child(text(note.clone())).child(code_block(t, "wg-cmd", nodo_core::discover::wireguard::SUDO_HINT.to_string())).child(text(i18n::t!("app.wg.import_hint")));
                (i18n::t!("app.wg.title"), b.into_any_element(), i18n::t!("app.menu.import_wg"), false, true, |this, window, cx| {
                    this.dialog = None;
                    this.import_wg_dump(window, cx);
                })
            }
            Dialog::Settings(f) => {
                let hint = |s: String| div().text_size(px(11.5)).text_color(t.ink_muted).child(s);
                let check = |id: &'static str, l: String, on: bool, set: fn(&mut SettingsForm, bool)| {
                    Checkbox::new(id).small().label(l).checked(on).on_click(cx.listener(move |this, v: &bool, _, cx| {
                        if let Some(Dialog::Settings(f)) = this.dialog.as_mut() {
                            set(f, *v);
                        }
                        cx.notify();
                    }))
                };
                let terms = self.terminals.clone();
                let cur = f.terminal.clone();
                let picked = crate::launch::pick_terminal(cur.as_deref(), &terms);
                let weak = cx.entity().downgrade();
                let term = Button::new("set-terminal").outline().small().icon(IconName::SquareTerminal).label(picked.map(|t| t.label()).unwrap_or_else(|| i18n::t!("app.launch.no_terminal"))).dropdown_menu(move |mut menu, _, _| {
                    for tm in terms.iter().copied() {
                        let w = weak.clone();
                        menu = menu.item(PopupMenuItem::new(tm.label()).checked(Some(tm) == picked).on_click(move |_, _, cx| {
                            w.update(cx, |this, cx| {
                                if let Some(Dialog::Settings(f)) = this.dialog.as_mut() {
                                    f.terminal = Some(tm.id().to_string());
                                }
                                cx.notify();
                            })
                            .ok();
                        }));
                    }
                    menu
                });
                let b = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(ui::section_title(t, i18n::t!("app.settings.monitoring")))
                    .child(label(i18n::t!("app.settings.interval")))
                    .child(Input::new(&f.interval).small())
                    .child(label(i18n::t!("app.settings.down_minutes")))
                    .child(Input::new(&f.down_minutes).small())
                    .child(check("set-alert-down", i18n::t!("app.settings.alert_down"), f.alert_down, |f, v| f.alert_down = v))
                    .child(check("set-alert-new", i18n::t!("app.settings.alert_new"), f.alert_new, |f, v| f.alert_new = v))
                    .child(check("set-notify", i18n::t!("app.settings.notify"), f.notify, |f, v| f.notify = v))
                    .child(hint(i18n::t!("app.settings.map_hint")))
                    .child(ui::section_title(t, i18n::t!("app.settings.general")))
                    .child(check("set-auto", i18n::t!("app.settings.auto_refresh"), f.auto_refresh, |f, v| f.auto_refresh = v))
                    .child(check("set-stars", i18n::t!("app.star.setting"), f.stars, |f, v| f.stars = v))
                    .child(hint(i18n::t!("app.star.setting_hint")))
                    .child(div().flex().items_center().gap(px(8.)).child(label(i18n::t!("app.settings.terminal"))).child(term))
                    .child(hint(i18n::t!("app.settings.background_hint")));
                (i18n::t!("app.menu.settings"), b.into_any_element(), i18n::t!("app.dialog.save"), false, true, |this, _, cx| this.save_settings(cx))
            }
            Dialog::Add(f) => {
                let weak = cx.entity().downgrade();
                let cur = f.kind;
                let kind = Button::new("add-kind").outline().small().label(kind_label(cur)).dropdown_menu(move |mut menu, _, _| {
                    for k in DeviceKind::ALL {
                        let w = weak.clone();
                        menu = menu.item(PopupMenuItem::new(kind_label(k)).checked(k == cur).on_click(move |_, _, cx| {
                            w.update(cx, |this, cx| {
                                if let Some(Dialog::Add(f)) = this.dialog.as_mut() {
                                    f.kind = k;
                                }
                                cx.notify();
                            })
                            .ok();
                        }));
                    }
                    menu
                });
                let ip_text = f.ip.read(cx).value().trim().to_string();
                let ip_ok = ip_text.is_empty() || ip_text.parse::<IpAddr>().is_ok();
                let b = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(text(i18n::t!("app.add.body")))
                    .child(label(i18n::t!("app.edit.name")))
                    .child(Input::new(&f.name))
                    .child(label(i18n::t!("app.add.ip")))
                    .child(Input::new(&f.ip))
                    .when(!ip_ok, |d| d.child(div().text_size(px(12.)).text_color(t.danger).child(i18n::t!("app.add.bad_ip"))))
                    .child(div().flex().items_center().gap(px(8.)).child(label(i18n::t!("app.edit.kind"))).child(kind));
                let ready = ip_ok && (!f.name.read(cx).value().trim().is_empty() || !ip_text.is_empty());
                (i18n::t!("app.discover.add"), b.into_any_element(), i18n::t!("app.add.ok"), false, ready, |this, _, cx| this.submit_add(cx))
            }
            Dialog::Merge { id, filter, .. } => {
                let id = *id;
                let q = filter.read(cx).value().trim().to_lowercase();
                let doc = self.doc.as_ref()?;
                let me = doc.data.device(id)?;
                let mut list = div().id("merge-list").flex().flex_col().max_h(px(280.)).overflow_y_scroll();
                let hover = t.canvas;
                for (k, d) in doc.data.devices.iter().filter(|d| d.id != id && (q.is_empty() || d.haystack().contains(&q))).take(60).enumerate() {
                    let other = d.id;
                    list = list.child(
                        div()
                            .id(("merge", k))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .px(px(8.))
                            .py(px(6.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .child(crate::ui::kind_svg(d.kind.id(), 15., t.ink_muted))
                            .child(div().flex_1().min_w_0().truncate().text_size(px(13.)).child(d.name.clone()))
                            .child(div().text_size(px(11.5)).text_color(t.ink_muted).child(d.primary_ip().map(|i| i.to_string()).unwrap_or_default()))
                            .on_click(cx.listener(move |this, _, _, cx| this.merge_into(id, other, cx))),
                    );
                }
                let b = div().flex().flex_col().gap(px(10.)).child(text(i18n::t!("app.merge.body", name = me.name.clone()))).child(Input::new(filter)).child(list);
                (i18n::t!("app.action.merge"), b.into_any_element(), i18n::t!("app.dialog.close"), false, true, |this, _, cx| {
                    this.dialog = None;
                    cx.notify();
                })
            }
            Dialog::Split { ips, idents, .. } => {
                let mut b = div().flex().flex_col().gap(px(6.)).child(text(i18n::t!("app.split.body")));
                for (k, (ip, on)) in ips.iter().enumerate() {
                    b = b.child(Checkbox::new(("split-ip", k)).small().label(ip.to_string()).checked(*on).on_click(cx.listener(move |this, v: &bool, _, cx| {
                        if let Some(Dialog::Split { ips, .. }) = this.dialog.as_mut() {
                            ips[k].1 = *v;
                        }
                        cx.notify();
                    })));
                }
                for (k, (i, on)) in idents.iter().enumerate() {
                    b = b.child(Checkbox::new(("split-id", k)).small().label(format!("{} {}", i.kind.id(), i.value)).checked(*on).on_click(cx.listener(move |this, v: &bool, _, cx| {
                        if let Some(Dialog::Split { idents, .. }) = this.dialog.as_mut() {
                            idents[k].1 = *v;
                        }
                        cx.notify();
                    })));
                }
                let ready = ips.iter().any(|x| x.1) || idents.iter().any(|x| x.1);
                (i18n::t!("app.action.split"), b.into_any_element(), i18n::t!("app.split.ok"), false, ready, |this, _, cx| this.submit_split(cx))
            }
            Dialog::Delete { name, .. } => (i18n::t!("app.delete.title"), text(i18n::t!("app.delete.body", name = name.clone())).into_any_element(), i18n::t!("app.action.delete"), true, true, |this, _, cx| this.confirm_delete(cx)),
            Dialog::Rename { name, .. } => (i18n::t!("app.net.rename"), div().child(Input::new(name)).into_any_element(), i18n::t!("app.dialog.save"), false, true, |this, _, cx| this.submit_rename(cx)),
            Dialog::Launch { launch, .. } => {
                let mut b = div().flex().flex_col().gap(px(10.)).child(text(i18n::t!("app.launch.body"))).child(code_block(t, "launch-cmd", launch.display()));
                if let Some(c) = &launch.copy {
                    b = b.child(text(i18n::t!("app.launch.copy", command = c)));
                }
                b = b.child(div().text_size(px(12.)).text_color(t.ink_muted).child(i18n::t!("app.launch.once")));
                (i18n::t!("app.launch.title"), b.into_any_element(), i18n::t!("app.launch.ok"), false, true, |this, _, cx| this.confirm_launch(cx))
            }
        };
        let close = |this: &mut NodoApp, cx: &mut Context<NodoApp>| {
            this.dialog = None;
            cx.notify();
        };
        let ok_btn = ui::filled_button(t, "dialog-ok", if danger { t.danger } else { t.accent }).child(ok_label).when(!ready, |d| d.opacity(0.4).cursor_default()).when(ready, |d| d.on_click(cx.listener(move |this, _, window, cx| ok(this, window, cx))));
        let card = div()
            .id("dialog")
            .flex()
            .flex_col()
            .gap(px(14.))
            .w(px(520.))
            .max_h(relative(0.9))
            .overflow_y_scroll()
            .p(px(22.))
            .rounded(px(16.))
            .border_1()
            .border_color(t.border)
            .bg(t.surface)
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(title))
            .child(body)
            .child(div().flex().justify_end().gap(px(8.)).child(ui::outline_button(t, "dialog-cancel").child(i18n::t!("app.dialog.cancel")).on_click(cx.listener(move |this, _, _, cx| close(this, cx)))).child(ok_btn));
        Some(ui::backdrop(t, "dialog-backdrop").on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| close(this, cx))).child(card).into_any_element())
    }
}
