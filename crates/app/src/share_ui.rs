//! Sending files, folders and text to other computers running Nodo (`nodo_core::share`): the
//! opt-in receiver, the prompts (pair once with a code shown on both screens, accept / decline),
//! transfers with progress and cancel, history, Taildrop.
use crate::app::NodoApp;
use crate::dialogs::Dialog;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use nodo_core::model::{DeviceId, Layer};
use nodo_core::share::crypto::{Identity, PeerKey};
use nodo_core::share::transfer::{Decision, Kind, Offer, Progress, Received, Sent};
use nodo_core::share::{Host, Payload, Peers, Receiver, ReceiverConfig};
use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const ASK_TIMEOUT: Duration = Duration::from_secs(120);
const HISTORY_MAX: usize = 200;

/// A question for the user, answered from a dialog (dropping `reply` means no).
pub enum Ask {
    /// The receiver met a new peer.
    Pair { code: String, addr: IpAddr },
    /// The sender met a new peer.
    SendPair { device: String, code: String },
    Offer { from: String, offer: Offer },
}

pub struct AskReq {
    pub ask: Ask,
    pub reply: mpsc::Sender<bool>,
}

enum Event {
    Ask(AskReq),
    Started { from: String, offer: Offer, progress: Arc<Progress> },
    Received(Result<Received, String>),
    Sent { id: u64, result: Result<Sent, String> },
}

fn channel() -> &'static (mpsc::Sender<Event>, Mutex<mpsc::Receiver<Event>>) {
    static C: OnceLock<(mpsc::Sender<Event>, Mutex<mpsc::Receiver<Event>>)> = OnceLock::new();
    C.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        (tx, Mutex::new(rx))
    })
}

fn ask(a: Ask) -> bool {
    let (tx, rx) = mpsc::channel();
    if channel().0.send(Event::Ask(AskReq { ask: a, reply: tx })).is_err() {
        return false;
    }
    rx.recv_timeout(ASK_TIMEOUT).unwrap_or(false)
}

fn file(name: &str) -> Option<PathBuf> {
    crate::settings::config_dir().map(|d| d.join(name))
}

pub fn identity() -> Option<Arc<Identity>> {
    static ID: OnceLock<Option<Arc<Identity>>> = OnceLock::new();
    ID.get_or_init(|| Identity::load_or_create(&file("share-identity.key")?).ok().map(Arc::new)).clone()
}

fn peers() -> Peers {
    file("peers.tsv").map(|p| Peers::load(&p)).unwrap_or_default()
}

fn add_peer(k: PeerKey, name: &str) {
    if let Some(p) = file("peers.tsv") {
        let mut ps = Peers::load(&p);
        ps.add(k, name);
        let _ = ps.save(&p);
    }
}

fn my_name() -> String {
    nodo_core::discover::hostname().map(|h| nodo_core::model::host_label(&h)).unwrap_or_else(|| crate::APP_NAME.to_string())
}

struct AppHost;

impl Host for AppHost {
    fn pair(&self, key: &PeerKey, code: &str, addr: IpAddr) -> bool {
        let ok = ask(Ask::Pair { code: code.to_string(), addr });
        if ok {
            add_peer(*key, &addr.to_string());
        }
        ok
    }

    fn known(&self, key: &PeerKey) -> Option<String> {
        peers().name(key).map(str::to_string)
    }

    fn decide(&self, from: &str, key: &PeerKey, offer: &Offer) -> Decision {
        // the peer now has a name: remember it with the key
        if peers().name(key).is_some_and(|n| n.parse::<IpAddr>().is_ok()) {
            add_peer(*key, from);
        }
        if crate::settings::Settings::load().share_auto() || ask(Ask::Offer { from: from.to_string(), offer: offer.clone() }) {
            Decision::Accept
        } else {
            Decision::Decline("declined".into()) // i18n-ok: protocol reason
        }
    }

    fn started(&self, from: &str, offer: &Offer, p: Arc<Progress>) {
        let _ = channel().0.send(Event::Started { from: from.to_string(), offer: offer.clone(), progress: p });
    }

    fn finished(&self, r: Result<Received, String>) {
        let _ = channel().0.send(Event::Received(r));
    }
}

static RECEIVER: Mutex<Option<Receiver>> = Mutex::new(None);

/// Start or stop the receiver to match the settings. Errors are user-facing.
pub fn sync_receiver() -> Result<(), String> {
    let s = crate::settings::Settings::load();
    let mut r = RECEIVER.lock().map_err(|e| e.to_string())?;
    if !s.share_receive() {
        if let Some(mut x) = r.take() {
            x.stop();
        }
        return Ok(());
    }
    if r.is_some() {
        return Ok(());
    }
    let id = identity().ok_or_else(|| i18n::t!("app.share.no_identity"))?;
    let cfg = ReceiverConfig { dir: s.share_dir(), max_size: s.share_max_bytes(), bind: SocketAddr::from(([0, 0, 0, 0], nodo_core::share::PORT)), advertise: true, name: my_name() };
    *r = Some(Receiver::start(cfg, id, Arc::new(AppHost)).map_err(|e| i18n::t!("app.share.listen_failed", detail = e.to_string()))?);
    Ok(())
}

/// Stop and start again with the current settings.
pub fn restart_receiver() -> Result<(), String> {
    if let Ok(mut r) = RECEIVER.lock()
        && let Some(mut x) = r.take()
    {
        x.stop();
        // the listener thread notices within a poll interval and frees the port
        std::thread::sleep(Duration::from_millis(250));
    }
    sync_receiver()
}

pub fn receiving() -> bool {
    RECEIVER.lock().map(|r| r.is_some()).unwrap_or(false)
}

#[derive(Debug, Clone, PartialEq)]
pub enum TState {
    Running,
    Done(Option<PathBuf>),
    Failed(String),
    Declined,
    Cancelled,
}

pub struct Transfer {
    pub id: u64,
    pub incoming: bool,
    pub peer: String,
    pub name: String,
    pub size: u64,
    pub progress: Arc<Progress>,
    pub state: TState,
    pub at: i64,
}

#[derive(Default)]
pub struct ShareState {
    pub transfers: Vec<Transfer>,
    pub asks: VecDeque<AskReq>,
    next: u64,
    loop_on: bool,
}

/// One line of `transfers.tsv`: at, in/out, peer, name, size, state, path.
fn history_line(t: &Transfer) -> String {
    let st = match &t.state {
        TState::Running => "running",
        TState::Done(_) => "done",
        TState::Failed(_) => "failed",
        TState::Declined => "declined",
        TState::Cancelled => "cancelled",
    }; // i18n-ok: file format
    let path = match &t.state {
        TState::Done(Some(p)) => p.display().to_string(),
        _ => String::new(),
    };
    let clean = |s: &str| s.replace(['\t', '\n'], " ");
    format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\n", t.at, if t.incoming { "in" } else { "out" }, clean(&t.peer), clean(&t.name), t.size, st, clean(&path)) // i18n-ok: file format
}

fn save_history(t: &Transfer) {
    let Some(p) = file("transfers.tsv") else { return };
    let mut lines: Vec<String> = std::fs::read_to_string(&p).map(|s| s.lines().map(|l| format!("{l}\n")).collect()).unwrap_or_default();
    lines.push(history_line(t));
    let skip = lines.len().saturating_sub(HISTORY_MAX);
    let _ = std::fs::write(&p, lines[skip..].concat());
}

/// Saved history, newest first: (at, incoming, peer, name, size, state).
pub fn history() -> Vec<(i64, bool, String, String, u64, String)> {
    let Some(p) = file("transfers.tsv") else { return Vec::new() };
    let mut v: Vec<_> = std::fs::read_to_string(p)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            Some((f.first()?.parse().ok()?, *f.get(1)? == "in", f.get(2)?.to_string(), f.get(3)?.to_string(), f.get(4)?.parse().unwrap_or(0), f.get(5)?.to_string()))
        })
        .collect();
    v.reverse();
    v
}

pub fn size_label(b: u64) -> String {
    let f = b as f64;
    if f >= 1e9 {
        format!("{:.1} GB", f / 1e9)
    } else if f >= 1e6 {
        format!("{:.1} MB", f / 1e6)
    } else if f >= 1e3 {
        format!("{:.0} KB", f / 1e3)
    } else {
        format!("{b} B")
    } // i18n-ok: units
}

/// Where to try a device: Tailscale addresses first (WireGuard-encrypted all the way), then
/// LAN IPv4, then the rest; link-local left out.
pub fn targets(d: &nodo_core::model::Device) -> Vec<IpAddr> {
    let mut v: Vec<(u8, IpAddr)> = d
        .addrs
        .iter()
        .filter(|a| match a.ip {
            IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) != 0xfe80,
            IpAddr::V4(v4) => !v4.is_link_local(),
        })
        .map(|a| (match (a.layer, a.ip.is_ipv4()) {
            (Layer::Tailscale, true) => 0,
            (Layer::Tailscale, false) => 1,
            (Layer::Lan, true) => 2,
            _ => 3,
        }, a.ip))
        .collect();
    v.sort();
    v.dedup();
    v.into_iter().map(|(_, ip)| ip).collect()
}

impl NodoApp {
    /// Poll the share channel (prompts, progress, results) while the window is open.
    pub fn start_share(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = sync_receiver() {
            self.toast(e, cx);
        }
        if self.share.loop_on {
            return;
        }
        self.share.loop_on = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(300)).await;
                if this.update(cx, |this, cx| this.drain_share(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn drain_share(&mut self, cx: &mut Context<Self>) {
        let events: Vec<Event> = channel().1.try_lock().map(|rx| rx.try_iter().collect()).unwrap_or_default();
        let now = nodo_core::time::now();
        for e in events {
            match e {
                Event::Ask(a) => self.share.asks.push_back(a),
                Event::Started { from, offer, progress } => {
                    let id = self.next_transfer();
                    let name = if offer.kind == Kind::Text { i18n::t!("app.share.text") } else { offer.name.clone() };
                    self.share.transfers.push(Transfer { id, incoming: true, peer: from, name, size: offer.size, progress, state: TState::Running, at: now });
                }
                Event::Received(r) => self.received(r, cx),
                Event::Sent { id, result } => self.sent(id, result, cx),
            }
        }
        if self.dialog.is_none()
            && let Some(a) = self.share.asks.pop_front()
        {
            self.dialog = Some(Dialog::Ask(a));
            cx.notify();
        }
        if self.share.transfers.iter().any(|t| t.state == TState::Running) {
            cx.notify();
        }
    }

    fn next_transfer(&mut self) -> u64 {
        self.share.next += 1;
        self.share.next
    }

    fn finish_transfer(&mut self, pick: impl Fn(&Transfer) -> bool, state: TState) {
        if let Some(t) = self.share.transfers.iter_mut().rev().find(|t| t.state == TState::Running && pick(t)) {
            t.state = state;
            save_history(t);
        }
        // finished ones linger a little in the footer, then only in the history
        let now = nodo_core::time::now();
        self.share.transfers.retain(|t| t.state == TState::Running || now - t.at < 600);
        if self.share.transfers.len() > 8 {
            let n = self.share.transfers.len() - 8;
            self.share.transfers.drain(..n);
        }
    }

    fn received(&mut self, r: Result<Received, String>, cx: &mut Context<Self>) {
        let notify = |title: String, body: String| {
            if crate::settings::Settings::load().notify() {
                crate::discovery::notify(&title, &body);
            }
        };
        match r {
            Ok(Received::File { path, offer, from }) => {
                let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.finish_transfer(|t| t.incoming && t.name == offer.name, TState::Done(Some(path.clone())));
                let msg = i18n::t!("app.share.received", name = file, from = from);
                notify(i18n::t!("app.share.received_title"), msg.clone());
                self.toast(msg, cx);
            }
            Ok(Received::Text { text, from }) => {
                self.finish_transfer(|t| t.incoming && t.size == text.len() as u64, TState::Done(None));
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                let msg = i18n::t!("app.share.text_received", from = from);
                notify(i18n::t!("app.share.received_title"), msg.clone());
                self.toast(msg, cx);
            }
            Ok(Received::Declined { offer, from }) => {
                let id = self.next_transfer();
                let t = Transfer { id, incoming: true, peer: from, name: offer.name, size: offer.size, progress: Progress::new(), state: TState::Declined, at: nodo_core::time::now() };
                save_history(&t);
            }
            Ok(Received::Cancelled { offer, .. }) => self.finish_transfer(|t| t.incoming && t.name == offer.name, TState::Cancelled),
            Ok(Received::Corrupt { offer, .. }) => {
                self.finish_transfer(|t| t.incoming && t.name == offer.name, TState::Failed(i18n::t!("app.share.corrupt")));
                self.toast(i18n::t!("app.share.corrupt"), cx);
            }
            Err(e) => self.finish_transfer(|t| t.incoming, TState::Failed(e)),
        }
        cx.notify();
    }

    fn sent(&mut self, id: u64, r: Result<Sent, String>, cx: &mut Context<Self>) {
        let name = self.share.transfers.iter().find(|t| t.id == id).map(|t| t.peer.clone()).unwrap_or_default();
        let (state, msg) = match r {
            Ok(Sent::Delivered) => (TState::Done(None), i18n::t!("app.share.sent", to = name)),
            Ok(Sent::Declined(reason)) if reason == "too_large" => (TState::Declined, i18n::t!("app.share.too_large", to = name)),
            Ok(Sent::Declined(_)) => (TState::Declined, i18n::t!("app.share.declined", to = name)),
            Ok(Sent::Cancelled) => (TState::Cancelled, i18n::t!("app.share.cancelled")),
            Ok(Sent::Failed(d)) | Err(d) => (TState::Failed(d.clone()), i18n::t!("app.share.failed", to = name, detail = d)),
        };
        self.finish_transfer(|t| t.id == id, state);
        self.toast(msg, cx);
    }

    pub fn cancel_transfer(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(t) = self.share.transfers.iter().find(|t| t.id == id) {
            t.progress.cancel.store(true, Ordering::Relaxed);
        }
        cx.notify();
    }

    pub fn answer_ask(&mut self, yes: bool, cx: &mut Context<Self>) {
        if let Some(Dialog::Ask(a)) = self.dialog.take() {
            let _ = a.reply.send(yes);
        }
        cx.notify();
    }

    // ── sending ──────────────────────────────────────────────────────────────────────────────

    pub fn send_file_dialog(&mut self, id: DeviceId, folder: bool, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: !folder, directories: folder, multiple: !folder, prompt: Some(i18n::t!("app.share.pick").into()) });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                this.update(cx, |this, cx| this.send_paths(id, paths, cx)).ok();
            }
        })
        .detach();
    }

    /// Files as they are, folders zipped first.
    pub fn send_paths(&mut self, id: DeviceId, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        for p in paths {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if p.is_dir() {
                let zip = std::env::temp_dir().join("nodo-share").join(format!("{name}.zip"));
                let task = cx.background_executor().spawn({
                    let (p, zip) = (p.clone(), zip.clone());
                    async move {
                        if let Some(d) = zip.parent() {
                            std::fs::create_dir_all(d)?;
                        }
                        nodo_core::share::zip_folder(&p, &zip)
                    }
                });
                cx.spawn(async move |this, cx| {
                    let r = task.await;
                    this.update(cx, |this, cx| match r {
                        Ok(()) => this.send_payload(id, Payload::File { path: zip, name: format!("{name}.zip"), kind: Kind::Folder }, cx), // i18n-ok: file name
                        Err(e) => this.toast(i18n::t!("app.share.zip_failed", detail = e.to_string()), cx),
                    })
                    .ok();
                })
                .detach();
            } else if p.is_file() {
                self.send_payload(id, Payload::File { path: p, name, kind: Kind::File }, cx);
            }
        }
    }

    pub fn send_clipboard(&mut self, id: DeviceId, cx: &mut Context<Self>) {
        match cx.read_from_clipboard().and_then(|c| c.text()).filter(|t| !t.is_empty()) {
            Some(text) => self.send_payload(id, Payload::Text(text), cx),
            None => self.toast(i18n::t!("app.share.clipboard_empty"), cx),
        }
    }

    fn send_payload(&mut self, id: DeviceId, payload: Payload, cx: &mut Context<Self>) {
        let Some(d) = self.doc.as_ref().and_then(|doc| doc.data.device(id)).cloned() else { return };
        if self.this_computer() == Some(id) {
            self.toast(i18n::t!("app.share.to_self"), cx);
            return;
        }
        let Some(me) = identity() else {
            self.toast(i18n::t!("app.share.no_identity"), cx);
            return;
        };
        let ips = targets(&d);
        let host_label = d.hostname.as_deref().map(nodo_core::model::host_label).unwrap_or_else(|| d.name.to_lowercase());
        let tid = self.next_transfer();
        let (name, size) = match &payload {
            Payload::File { path, name, .. } => (name.clone(), std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)),
            Payload::Text(t) => (i18n::t!("app.share.text"), t.len() as u64),
        };
        let progress = Progress::new();
        self.share.transfers.push(Transfer { id: tid, incoming: false, peer: d.name.clone(), name, size, progress: progress.clone(), state: TState::Running, at: nodo_core::time::now() });
        let device = d.name.clone();
        std::thread::spawn(move || {
            // Nodos that announce themselves on the LAN, matched by address or host name
            let mut addrs: Vec<SocketAddr> = ips.iter().map(|ip| SocketAddr::new(*ip, nodo_core::share::PORT)).collect();
            for (n, found, port) in nodo_core::share::browse(Duration::from_millis(1200)) {
                if found.iter().any(|a| ips.contains(a)) || nodo_core::model::host_label(&n) == host_label {
                    for a in found {
                        let s = SocketAddr::new(a, port);
                        if !addrs.contains(&s) {
                            addrs.push(s);
                        }
                    }
                }
            }
            let result = nodo_core::share::send_to(&addrs, &me, &my_name(), &payload, &progress, |key, code| {
                if peers().name(key).is_some() {
                    return true;
                }
                let ok = ask(Ask::SendPair { device: device.clone(), code: code.to_string() });
                if ok {
                    add_peer(*key, &device);
                }
                ok
            });
            let _ = channel().0.send(Event::Sent { id: tid, result: result.map_err(|e| match e.kind() {
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::TimedOut | std::io::ErrorKind::NotFound => i18n::t!("app.share.unreachable"),
                _ => e.to_string(),
            }) });
        });
        cx.notify();
    }

    /// Taildrop: `tailscale file cp` to the device's tailnet address.
    pub fn send_taildrop(&mut self, id: DeviceId, cx: &mut Context<Self>) {
        let Some(d) = self.doc.as_ref().and_then(|doc| doc.data.device(id)).cloned() else { return };
        let Some(target) = d.addrs.iter().find(|a| a.layer == Layer::Tailscale && a.ip.is_ipv4()).map(|a| a.ip.to_string()) else {
            self.toast(i18n::t!("app.share.no_tailnet"), cx);
            return;
        };
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some(i18n::t!("app.share.pick").into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let task = cx.background_executor().spawn(async move {
                let cli = nodo_core::discover::tailscale::find_cli().ok_or_else(|| i18n::t!("core.tailscale.not_found"))?;
                nodo_core::share::taildrop(&cli, &path, &target)
            });
            let r = task.await;
            this.update(cx, |this, cx| match r {
                Ok(()) => this.toast(i18n::t!("app.share.taildrop_sent", to = d.name.clone()), cx),
                Err(e) => this.toast(i18n::t!("app.share.taildrop_failed", detail = e), cx),
            })
            .ok();
        })
        .detach();
    }

    pub fn open_transfers(&mut self, cx: &mut Context<Self>) {
        self.dialog = Some(Dialog::Transfers);
        cx.notify();
    }

    pub fn open_downloads(&mut self, cx: &mut Context<Self>) {
        let dir = crate::settings::Settings::load().share_dir();
        let _ = std::fs::create_dir_all(&dir);
        cx.open_with_system(&dir);
    }

    /// The running transfers, for the footer.
    pub fn render_share_footer(&self, t: &crate::theme::Tokens, cx: &mut Context<Self>) -> Option<Div> {
        let running: Vec<&Transfer> = self.share.transfers.iter().filter(|x| x.state == TState::Running).collect();
        if running.is_empty() {
            return receiving().then(|| div().id("share-on").flex_none().cursor_pointer().child(i18n::t!("app.share.receiving")).on_click(cx.listener(|this, _, _, cx| this.open_transfers(cx)))).map(|d| div().child(d));
        }
        let mut row = div().flex().flex_none().items_center().gap(px(10.));
        for x in running.into_iter().take(2) {
            let pct = (x.progress.fraction() * 100.).round();
            let label = if x.incoming { i18n::t!("app.share.receiving_one", name = x.name.clone(), pct = format!("{pct:.0}")) } else { i18n::t!("app.share.sending_one", name = x.name.clone(), to = x.peer.clone(), pct = format!("{pct:.0}")) };
            let id = x.id;
            row = row.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .max_w(px(320.))
                    .child(div().w(px(46.)).h(px(4.)).rounded(px(2.)).bg(t.border).child(div().h_full().rounded(px(2.)).bg(t.accent).w(px(46. * x.progress.fraction()))))
                    .child(div().truncate().child(label))
                    .when(!x.incoming, |d| d.child(div().id(("share-x", id as usize)).cursor_pointer().text_color(t.danger).child("×").on_click(cx.listener(move |this, _, _, cx| this.cancel_transfer(id, cx))))), // i18n-ok: symbol
            );
        }
        Some(row)
    }
}



#[cfg(test)]
mod tests {
    use nodo_core::model::{Addr, Device, Layer, Source};

    #[test]
    fn tailscale_first() {
        let a = |ip: &str, l: Layer| Addr { ip: ip.parse().unwrap(), layer: l, network: None, source: Source::Arp, last_seen: 0 };
        let d = Device { addrs: vec![a("fe80::1", Layer::Lan), a("192.168.40.22", Layer::Lan), a("fd7a:115c:a1e0::5", Layer::Tailscale), a("100.70.1.2", Layer::Tailscale)], ..Default::default() };
        let t: Vec<String> = super::targets(&d).iter().map(|i| i.to_string()).collect();
        assert_eq!(t, ["100.70.1.2", "fd7a:115c:a1e0::5", "192.168.40.22"]);
        assert_eq!(super::size_label(1_500_000), "1.5 MB");
    }
}
