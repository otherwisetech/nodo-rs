//! The AI panel (otherwise-agent-panel), docked next to the map and toggled with ⌘J: the user's
//! own Claude Code, Codex or opencode with `nodo mcp --map <this window's map>` attached for that
//! session only (the user's agent configs are never written).
//!
//! The assistant reads. "Solo lectura" offers read tools only; the other modes start the server
//! with `--propose <folder>` (one per window), so `update_device` / `add_service` only leave a
//! checked proposal there (`nodo_core::agent`). This window turns each into an Aplicar /
//! Rechazar card and applies it, built the same way, only on Aplicar.
use crate::app::{NodoApp, Sel};
use crate::theme::Tokens;
use gpui::*;
use nodo_core::agent::{self, Proposal};
use nodo_core::model::EventKind;
use otherwise_agent_panel::ui::{AgentHost, AgentPanel, PanelStyle};
use otherwise_agent_panel::{McpServer, PendingChange, PermissionMode};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const APP_ID: &str = "nodo"; // i18n-ok: app id

const INSTRUCTIONS: &str = "This is Nodo, a map of the devices on the user's networks (LAN, Tailscale, AWS, WireGuard) that also watches saved devices and services (HTTP(S), TCP, DNS, ping). The MCP server \"nodo\" is the map open in this window: status (what is down now and since when), services (uptime, open incidents, certificate expiry), incidents (outages over a period), history, devices / device, network_map (networks and who is on each). Times are Unix seconds: convert them to local dates and durations for the user. Never invent devices or addresses; look them up. When update_device / add_service are offered they only propose: each becomes a card the user accepts or rejects in the app, so tell them to review the card instead of saying it is done. Answer in the user's language."; // i18n-ok: agent instructions (English)

const POLL: Duration = Duration::from_millis(800);

pub struct AgentState {
    pub panel: Option<WeakEntity<AgentPanel>>,
    pending: HashMap<String, Proposal>,
    seq: u32,
}

impl AgentState {
    pub fn new() -> AgentState {
        AgentState { panel: None, pending: HashMap::new(), seq: 0 }
    }
}

fn live_root() -> PathBuf {
    otherwise_link::paths::app_dir(APP_ID).join("assistant").join("live")
}

/// Where this window's sessions leave proposals (per process and window: each has its own map).
pub fn proposals_dir(window_key: u64) -> PathBuf {
    live_root().join(format!("{}-{window_key}", std::process::id())).join("proposals")
}

/// Removes this process's proposal folders (on quit).
pub fn remove_live_dirs() {
    let Ok(rd) = std::fs::read_dir(live_root()) else { return };
    let mine = format!("{}-", std::process::id());
    for e in rd.flatten() {
        if e.file_name().to_string_lossy().starts_with(&mine) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// The command line inside the bundle (`Contents/MacOS/nodo`), next to this binary.
pub fn bundled_cli() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let cli = exe.with_file_name(if cfg!(windows) { "nodo.exe" } else { "nodo" }); // i18n-ok: file name
    cli.is_file().then_some(cli)
}

/// `nodo mcp` on `map` for a session in `mode`.
pub fn mcp_server(cli: &Path, map: &Path, mode: PermissionMode, proposals: &Path, lang: &str) -> McpServer {
    let map = map.display().to_string();
    let dir = proposals.display().to_string();
    let mut args = vec!["mcp", "--map", map.as_str()]; // i18n-ok: command line
    if mode != PermissionMode::ReadOnly {
        args.extend(["--propose", dir.as_str()]); // i18n-ok: flag
    }
    let mut s = McpServer::new(APP_ID, cli, &args);
    s.env.push(("NODO_LANG".into(), lang.into())); // i18n-ok: env var
    s
}

pub fn sync_locale() {
    otherwise_agent_panel::i18n::set_locale(i18n::locale());
}

/// The panel in Nodo's colours.
pub fn style(t: &Tokens) -> PanelStyle {
    PanelStyle {
        background: t.canvas,
        surface: t.surface,
        raised: t.surface,
        border: t.border,
        ink: t.ink,
        muted: t.ink_muted,
        accent: t.accent,
        accent_soft: t.accent_soft,
        danger: t.danger,
        danger_soft: t.danger.opacity(0.12),
        ok: t.up,
        note: t.tag,
        note_soft: t.tag.opacity(0.14),
        terminal_bg: if t.dark { rgb(0x161514).into() } else { rgb(0xFFFFFF).into() },
        terminal_fg: t.ink,
        mono_font: "Menlo".into(), // i18n-ok: font name
        dark: t.dark,
    }
}

pub struct Host {
    pub app: WeakEntity<NodoApp>,
}

impl AgentHost for Host {
    fn app_id(&self) -> String {
        APP_ID.into()
    }

    fn app_name(&self) -> String {
        crate::APP_NAME.into()
    }

    fn mcp_servers(&self, mode: PermissionMode, cx: &App) -> Vec<McpServer> {
        let Some(app) = self.app.upgrade() else { return Vec::new() };
        let Some(cli) = bundled_cli() else { return Vec::new() };
        let key = app.entity_id().as_u64();
        let a = app.read(cx);
        let Some(doc) = a.doc.as_ref() else { return Vec::new() };
        // open on another computer: read it, but nothing would be applied here
        let mode = if a.file.elsewhere.is_some() { PermissionMode::ReadOnly } else { mode };
        vec![mcp_server(&cli, doc.store.path(), mode, &proposals_dir(key), i18n::locale())]
    }

    fn context(&self, cx: &App) -> Option<String> {
        Some(self.app.upgrade()?.read(cx).agent_context())
    }

    fn instructions(&self) -> Option<String> {
        Some(INSTRUCTIONS.into())
    }

    fn suggestions(&self) -> Vec<String> {
        vec![i18n::t!("app.assistant.suggest_down"), i18n::t!("app.assistant.suggest_incidents"), i18n::t!("app.assistant.suggest_certs"), i18n::t!("app.assistant.suggest_map")]
    }

    fn resolve_change(&self, id: &str, approve: bool, _window: &mut Window, cx: &mut App) -> Result<(), String> {
        self.app.update(cx, |a, cx| a.resolve_proposal(id, approve, cx)).map_err(|e| e.to_string())?
    }
}

impl NodoApp {
    fn agent_panel(&self) -> Option<Entity<AgentPanel>> {
        self.agent.panel.as_ref().and_then(|p| p.upgrade())
    }

    pub fn toggle_agent_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = self.agent_panel() {
            p.update(cx, |p, cx| p.toggle(window, cx));
        }
    }

    /// "Lo que estoy viendo" for the agent (English).
    pub fn agent_context(&self) -> String {
        let view = if self.view.status {
            "Status (monitored services and devices)"
        } else if self.view.list {
            "Device list"
        } else {
            "Network map"
        }; // i18n-ok: context for the agent (English)
        let mut v = json!({ "view": view, "layout": format!("{:?}", self.view.mode), "layers": self.view.layers });
        if let Some(doc) = &self.doc {
            v["map"] = json!(doc.title());
            match self.selected {
                Some(Sel::Device(id)) => {
                    if let Some(d) = doc.data.device(id) {
                        v["selected_device"] = json!({ "id": d.id, "name": d.name, "ip": d.primary_ip().map(|i| i.to_string()) });
                    }
                }
                Some(Sel::Network(id)) => {
                    if let Some(n) = doc.data.network(id) {
                        v["selected_network"] = json!({ "id": n.id, "name": n.name, "cidr": n.cidr });
                    }
                }
                None => {}
            }
        } else {
            v["map"] = Value::Null;
        }
        v.to_string()
    }

    /// Per render: the panel's colours.
    pub fn sync_agent_style(t: &Tokens, cx: &mut App) {
        if cx.try_global::<PanelStyle>().is_none_or(|s| s.dark != t.dark) {
            cx.set_global(style(t));
        }
    }

    /// Turns delivered proposals into cards while the window lives.
    pub fn start_agent_pump(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                if this.update(cx, |this, cx| this.agent_tick(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn agent_tick(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.agent_panel() else { return };
        let dir = proposals_dir(cx.entity_id().as_u64());
        if !dir.is_dir() {
            return;
        }
        for p in agent::collect(&dir) {
            let Some((title, lines)) = self.card_text(&p) else { continue };
            self.agent.seq += 1;
            let id = format!("p{}", self.agent.seq); // i18n-ok: card id
            self.agent.pending.insert(id.clone(), p);
            let change = PendingChange::new(id, title, lines.join("\n"));
            panel.update(cx, |p, cx| p.report_pending_change(change, cx));
        }
    }

    /// The card's title and detail lines; `None` when it no longer applies to this map.
    pub fn card_text(&self, p: &Proposal) -> Option<(String, Vec<String>)> {
        let data = &self.doc.as_ref()?.data;
        let mut lines = Vec::new();
        let title = match p.tool.as_str() {
            "update_device" => {
                let new = agent::edited_device(data, &p.args).ok()?;
                let old = data.device(new.id)?;
                let changes = [(i18n::t!("app.assistant.field_name"), &old.name, &new.name), (i18n::t!("app.assistant.field_owner"), &old.owner, &new.owner), (i18n::t!("app.assistant.field_location"), &old.location, &new.location), (i18n::t!("app.assistant.field_notes"), &old.notes, &new.notes)];
                for (label, a, b) in changes {
                    if a != b {
                        lines.push(i18n::t!("app.assistant.card_change", field = label, before = a, after = b));
                    }
                }
                if old.saved != new.saved {
                    lines.push(if new.saved { i18n::t!("app.assistant.card_save") } else { i18n::t!("app.assistant.card_unsave") });
                }
                i18n::t!("app.assistant.card_update", name = old.name)
            }
            "add_service" => {
                let s = agent::new_service(data, &p.args).ok()?;
                lines.push(format!("{} · {}", s.spec.kind.id(), s.spec.target)); // i18n-ok: kind · target
                if let Some(d) = s.device.and_then(|id| data.device(id)) {
                    lines.push(i18n::t!("app.assistant.card_device", name = d.name));
                }
                i18n::t!("app.assistant.card_service", name = s.name)
            }
            _ => return None,
        };
        if !p.reason.is_empty() {
            lines.push(i18n::t!("app.assistant.card_reason", reason = p.reason));
        }
        Some((title, lines))
    }

    /// Aplicar / Rechazar on a card.
    pub fn resolve_proposal(&mut self, id: &str, approve: bool, cx: &mut Context<Self>) -> Result<(), String> {
        let Some(p) = self.agent.pending.remove(id) else {
            return if approve { Err(i18n::t!("app.assistant.card_gone")) } else { Ok(()) };
        };
        if !approve {
            return Ok(());
        }
        if self.file.elsewhere.is_some() {
            return Err(i18n::t!("app.assistant.read_only"));
        }
        let doc = self.doc.as_mut().ok_or_else(|| i18n::t!("app.assistant.card_gone"))?;
        let name = match p.tool.as_str() {
            "update_device" => {
                let d = agent::edited_device(&doc.data, &p.args)?;
                let (id, name) = (d.id, d.name.clone());
                doc.update_device(d).map_err(|e| e.to_string())?;
                let _ = doc.store.add_event(Some(id), nodo_core::time::now(), EventKind::Edited, "assistant"); // i18n-ok: source tag
                self.retarget();
                self.touch(cx);
                name
            }
            "add_service" => {
                let s = agent::new_service(&doc.data, &p.args)?;
                let name = s.name.clone();
                let sid = doc.store.save_service(&s).map_err(|e| e.to_string())?;
                self.svc.list = doc.store.services().unwrap_or_default();
                if let Some(e) = &self.svc.engine {
                    e.set_checks(self.svc.list.clone());
                    e.check_now(sid);
                }
                cx.notify();
                name
            }
            _ => return Err(i18n::t!("app.assistant.card_gone")),
        };
        self.toast(i18n::t!("app.assistant.applied", name = name), cx);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // not `super::*`: gpui's glob brings its own `test` attribute
    use super::{PermissionMode, mcp_server, proposals_dir};
    use std::path::Path;

    #[test]
    fn modes_start_the_map_server_read_only_or_proposing() {
        let cli = Path::new("/Applications/Nodo.app/Contents/MacOS/nodo");
        let map = Path::new("/data/nodo/Casa.nodo");
        let dir = Path::new("/data/nodo/assistant/live/7-3/proposals");
        let ro = mcp_server(cli, map, PermissionMode::ReadOnly, dir, "es");
        assert_eq!(ro.name, "nodo");
        assert_eq!(ro.args, ["mcp", "--map", "/data/nodo/Casa.nodo"]);
        assert_eq!(ro.env, [("NODO_LANG".to_string(), "es".to_string())]);
        for mode in [PermissionMode::Ask, PermissionMode::Auto] {
            assert_eq!(mcp_server(cli, map, mode, dir, "en").args, ["mcp", "--map", "/data/nodo/Casa.nodo", "--propose", "/data/nodo/assistant/live/7-3/proposals"], "{mode:?}: proposals, never --allow-writes");
        }
        assert_ne!(proposals_dir(1), proposals_dir(2), "one folder per window");
    }
}
