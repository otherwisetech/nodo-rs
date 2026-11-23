//! `nodo mcp`: a stdio MCP server (JSON-RPC 2.0, one message per line) over one map, proxied
//! by Otherwise Apps' `otherwise-hub` as `nodo.*` and started by the app's AI panel for one
//! session. Read-only unless started with `--allow-writes` (the hub passes it only when the user
//! enabled write tools) or `--propose <folder>` (the panel: write tools only check the change
//! and leave it there for the app to show as a card); writes only edit a device's labels or add
//! a service, each recorded in the device's history. The map is read fresh for every call, so
//! the server sees what the app wrote.
use nodo_core::db::Store;
use nodo_core::doc::Doc;
use nodo_core::model::{Device, EventKind};
use nodo_core::services::DAY;
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::PathBuf;

pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

pub struct Server {
    pub map: PathBuf,
    pub writes: bool,
    /// The app's proposals folder: write tools become cards there instead of writing.
    pub propose: Option<PathBuf>,
}

const PROPOSE_NOTE: &str = " In this session it is a proposal: the user sees it as a card in Nodo and it is applied only if they accept it, so tell them to review the card instead of saying it is done."; // i18n-ok: for agents

const HELP: &str = "Nodo maps the devices on the user's networks (LAN, Tailscale, AWS, WireGuard) and watches saved devices and services (HTTP(S), TCP, DNS, ping). Times are Unix seconds. `status` says what is wrong now; device and service ids are per map."; // i18n-ok: for agents

struct Tool {
    name: &'static str,
    description: &'static str,
    schema: fn() -> Value,
    write: bool,
}

const TOOLS: &[Tool] = &[
    Tool { name: "incidents", description: "Service outages (incidents) in the last `days` (default 7): service, start, end (null while still down), duration in seconds and cause. Optional service id.", schema: || json!({"type": "object", "properties": {"days": {"type": "integer", "minimum": 1, "maximum": 365}, "service": {"type": "integer"}}}), write: false }, // i18n-ok: for agents
    Tool { name: "history", description: "Recent history of the map (devices appearing, going down / up, edits), newest first; optionally for one device.", schema: || json!({"type": "object", "properties": {"device": {"type": "string"}, "limit": {"type": "integer", "minimum": 1, "maximum": 500}}}), write: false }, // i18n-ok: for agents
    Tool { name: "network_map", description: "The map's structure: each network (LAN subnet, Tailscale tailnet, AWS VPC / subnet, WireGuard) with its CIDR, gateway and parent, and the devices on it (name, type, IP, online, saved); plus devices on no known network.", schema: || json!({"type": "object", "properties": {}}), write: false }, // i18n-ok: for agents
    Tool { name: "devices", description: "Devices of the map: id, name, type, primary IP, MACs, saved (in the inventory), online, owner, location. Filters: query (name, IP, MAC, host name), saved_only.", schema: || json!({"type": "object", "properties": {"query": {"type": "string"}, "saved_only": {"type": "boolean"}}}), write: false }, // i18n-ok: for agents
    Tool { name: "device", description: "One device (by id, name, IP, MAC or host name) with addresses, identities, services, facts, recent history and its shared registry card when linked.", schema: || json!({"type": "object", "properties": {"device": {"type": "string"}}, "required": ["device"]}), write: false }, // i18n-ok: for agents
    Tool { name: "services", description: "Monitored services with uptime (24 h, 7 d, 30 d), the open incident if down, and the TLS certificate's expiry when known.", schema: || json!({"type": "object", "properties": {}}), write: false }, // i18n-ok: for agents
    Tool { name: "status", description: "What needs attention: devices down, services down, certificates expiring, with counts.", schema: || json!({"type": "object", "properties": {}}), write: false }, // i18n-ok: for agents
    Tool { name: "update_device", description: "Change a device's name, owner, location, notes or whether it is saved in the inventory. Recorded in its history.", schema: || json!({"type": "object", "properties": {"device": {"type": "string"}, "name": {"type": "string"}, "owner": {"type": "string"}, "location": {"type": "string"}, "notes": {"type": "string"}, "saved": {"type": "boolean"}, "reason": {"type": "string", "description": "proposals: one short sentence for the user"}}, "required": ["device"]}), write: true }, // i18n-ok: for agents
    Tool { name: "add_service", description: "Add a monitored service: kind (http, tcp, dns, icmp) and target (URL, host:port or host name), optional name and device.", schema: || json!({"type": "object", "properties": {"kind": {"type": "string", "enum": ["http", "tcp", "dns", "icmp"]}, "target": {"type": "string"}, "name": {"type": "string"}, "device": {"type": "string"}, "reason": {"type": "string", "description": "proposals: one short sentence for the user"}}, "required": ["kind", "target"]}), write: true }, // i18n-ok: for agents
];

fn ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn summary(d: &Device, map: Option<&str>) -> Value {
    json!({"id": d.id, "name": d.name, "type": d.kind.id(), "primary_ip": d.primary_ip().map(|i| i.to_string()), "macs": d.macs().collect::<Vec<_>>(), "hostname": d.hostname, "saved": d.saved, "online": d.online, "owner": d.owner, "location": d.location, "vendor": d.vendor, "model": d.model, "last_seen": d.last_seen, "link": nodo_core::links::device_link(d.id, map).to_url()})
}

impl Server {
    fn offers(&self, t: &Tool) -> bool {
        !t.write || self.writes || self.propose.is_some()
    }

    pub fn tools(&self) -> Vec<Value> {
        let proposing = self.propose.is_some() && !self.writes;
        TOOLS
            .iter()
            .filter(|t| self.offers(t))
            .map(|t| {
                let description = if t.write && proposing { format!("{}{PROPOSE_NOTE}", t.description) } else { t.description.to_string() };
                json!({"name": t.name, "description": description, "inputSchema": (t.schema)(), "annotations": {"readOnlyHint": !t.write, "destructiveHint": false}})
            })
            .collect()
    }

    pub fn handle(&mut self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned()?;
        if msg.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(rpc_error(id, -32600, "invalid request"));
        }
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        Some(match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
                let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(PROTOCOL_VERSIONS[0]);
                ok(id, json!({"protocolVersion": version, "capabilities": {"tools": {"listChanged": false}}, "serverInfo": {"name": "nodo", "version": env!("CARGO_PKG_VERSION")}, "instructions": HELP}))
            }
            "ping" => ok(id, json!({})),
            "tools/list" => ok(id, json!({"tools": self.tools()})),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                if !TOOLS.iter().any(|t| t.name == name && self.offers(t)) {
                    return Some(rpc_error(id, -32602, &format!("unknown tool {name:?}"))); // i18n-ok: protocol
                }
                match self.call(name, &args) {
                    Ok(v) => ok(id, json!({"content": [{"type": "text", "text": v.to_string()}], "structuredContent": v, "isError": false})),
                    Err(e) => ok(id, json!({"content": [{"type": "text", "text": e}], "isError": true})),
                }
            }
            _ => rpc_error(id, -32601, &format!("method not found: {method}")), // i18n-ok: protocol
        })
    }

    fn call(&self, name: &str, a: &Value) -> Result<Value, String> {
        let arg = |k: &str| a.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
        let err = |e: &dyn std::fmt::Display| format!("{}: {e}", self.map.display()); // i18n-ok: path: error
        if TOOLS.iter().any(|t| t.name == name && t.write) {
            if !self.writes
                && let Some(dir) = &self.propose
            {
                let store = Store::open_read_only(&self.map).map_err(|e| err(&e))?;
                let data = store.load().map_err(|e| e.to_string())?;
                let preview = match name {
                    "update_device" => summary(&nodo_core::agent::edited_device(&data, a)?, None),
                    _ => crate::service_json(&nodo_core::agent::new_service(&data, a)?),
                };
                let p = nodo_core::agent::Proposal { tool: name.to_string(), args: a.clone(), reason: arg("reason").unwrap_or_default().to_string() };
                nodo_core::agent::deliver(dir, &p).map_err(|e| e.to_string())?;
                return Ok(json!({"status": "awaiting_approval", "preview": preview, "message": "Shown to the user as a card in Nodo; it is applied only if they accept it."})); // i18n-ok: for agents
            }
            let mut doc = Doc::open(&self.map).map_err(|e| err(&e))?;
            let now = nodo_core::time::now();
            return match name {
                "update_device" => {
                    let d = nodo_core::agent::edited_device(&doc.data, a)?;
                    let id = d.id;
                    doc.update_device(d).map_err(|e| e.to_string())?;
                    doc.store.add_event(Some(id), now, EventKind::Edited, "mcp").map_err(|e| e.to_string())?; // i18n-ok: source tag
                    Ok(summary(doc.data.device(id).ok_or_else(|| i18n::t!("cli.err.no_device", query = id))?, None))
                }
                _ => {
                    let mut s = nodo_core::agent::new_service(&doc.data, a)?;
                    s.id = doc.store.save_service(&s).map_err(|e| e.to_string())?;
                    Ok(crate::service_json(&s))
                }
            };
        }
        let store = Store::open_read_only(&self.map).map_err(|e| err(&e))?;
        let data = store.load().map_err(|e| e.to_string())?;
        let uid = nodo_core::sync::known_map_uid(&store);
        let uid = uid.as_deref();
        let now = nodo_core::time::now();
        match name {
            "incidents" => {
                let days = a.get("days").and_then(Value::as_i64).unwrap_or(7).clamp(1, 365);
                let since = now - days * DAY;
                let services = store.services().unwrap_or_default();
                let name_of = |id: i64| services.iter().find(|s| s.id == id).map(|s| s.name.clone()).unwrap_or_default();
                let v: Vec<Value> = store
                    .incidents(a.get("service").and_then(Value::as_i64), 2000)
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .filter(|i| i.end.unwrap_or(now) >= since)
                    .map(|i| json!({"id": i.id, "service": i.service, "service_name": name_of(i.service), "start": i.start, "end": i.end, "ongoing": i.end.is_none(), "duration_s": i.end.unwrap_or(now) - i.start, "cause": i.cause}))
                    .collect();
                Ok(json!({"since": since, "now": now, "count": v.len(), "incidents": v}))
            }
            "history" => {
                let limit = a.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, 500) as usize;
                let device = match arg("device") {
                    Some(q) => Some(crate::find(&data, q)?.id),
                    None => None,
                };
                let name_of = |id: Option<i64>| id.and_then(|id| data.device(id)).map(|d| d.name.clone());
                let v: Vec<Value> = store.events(device, limit).map_err(|e| e.to_string())?.into_iter().map(|e| json!({"at": e.at, "device": e.device, "device_name": name_of(e.device), "kind": e.kind, "detail": e.detail})).collect();
                Ok(json!({"now": now, "count": v.len(), "events": v}))
            }
            "network_map" => {
                let on = |d: &Device, n: i64| d.addrs.iter().any(|x| x.network == Some(n));
                let brief = |d: &Device| json!({"id": d.id, "name": d.name, "type": d.kind.id(), "ip": d.primary_ip().map(|i| i.to_string()), "online": d.online, "saved": d.saved});
                let nets: Vec<Value> = data
                    .networks
                    .iter()
                    .map(|n| {
                        let devs: Vec<Value> = data.devices.iter().filter(|d| on(d, n.id)).map(brief).collect();
                        json!({"id": n.id, "name": n.name, "layer": n.layer, "cidr": n.cidr, "gateway": n.gateway.map(|g| g.to_string()), "parent": n.parent, "device_count": devs.len(), "devices": devs})
                    })
                    .collect();
                let loose: Vec<Value> = data.devices.iter().filter(|d| !d.addrs.iter().any(|x| x.network.is_some())).map(brief).collect();
                Ok(json!({"networks": nets, "devices_without_network": loose, "devices": data.devices.len()}))
            }
            "devices" => {
                let q = arg("query").map(str::to_lowercase);
                let saved_only = a.get("saved_only").and_then(Value::as_bool).unwrap_or(false);
                let v: Vec<Value> = data.devices.iter().filter(|d| !saved_only || d.saved).filter(|d| q.as_deref().is_none_or(|q| d.haystack().contains(q))).map(|d| summary(d, uid)).collect();
                Ok(json!({"count": v.len(), "devices": v}))
            }
            "device" => {
                let d = crate::find(&data, arg("device").ok_or_else(|| i18n::t!("cli.err.show_needs"))?)?;
                let mut v = crate::device_json(d);
                v["events"] = serde_json::to_value(store.events(Some(d.id), 50).unwrap_or_default()).unwrap_or_default();
                v["link"] = json!(nodo_core::links::device_link(d.id, uid).to_url());
                let link = store.shared_links().unwrap_or_default().into_iter().find(|l| l.kind == nodo_core::sync::DEVICE && l.local == d.id.to_string());
                v["shared_id"] = json!(link.map(|l| l.shared_id).filter(|s| s != nodo_core::sync::DETACHED));
                if let Ok(reg) = otherwise_link::registry::Registry::open_shared(&otherwise_link::Root::system()) {
                    v["owner_entity"] = json!(nodo_core::sync::owner_card(&reg, &d.owner).map(|c| json!({"id": c.id, "kind": c.kind, "name": c.name})));
                }
                Ok(v)
            }
            "services" => {
                let (_, st) = nodo_core::feed::from_store(&store, &data);
                let v: Vec<Value> = st
                    .iter()
                    .map(|s| {
                        let h = store.service_hours(s.check.id, now - 31 * DAY).unwrap_or_default();
                        let mut j = crate::service_json(&s.check);
                        j["uptime"] = json!({"24h": nodo_core::services::uptime(&h, now, DAY), "7d": nodo_core::services::uptime(&h, now, 7 * DAY), "30d": nodo_core::services::uptime(&h, now, 30 * DAY)});
                        j["down_since"] = json!(s.down_since);
                        j["down_cause"] = json!((!s.detail.is_empty()).then_some(&s.detail));
                        j["cert_until"] = json!(s.cert_until);
                        j["link"] = json!(nodo_core::links::service_link(s.check.id, uid).to_url());
                        j
                    })
                    .collect();
                Ok(json!({"count": v.len(), "services": v}))
            }
            _ => {
                let (down, st) = nodo_core::feed::from_store(&store, &data);
                let events = nodo_core::feed::events(uid.unwrap_or("map"), &down, &st, now);
                Ok(json!({
                    "map": self.map.display().to_string(),
                    "devices": data.devices.len(),
                    "saved": data.devices.iter().filter(|d| d.saved).count(),
                    "devices_down": down.iter().map(|d| json!({"id": d.id, "name": d.name, "since": d.since})).collect::<Vec<_>>(),
                    "services": st.len(),
                    "services_down": st.iter().filter(|s| s.down_since.is_some()).map(|s| json!({"id": s.check.id, "name": s.check.name, "since": s.down_since, "cause": s.detail})).collect::<Vec<_>>(),
                    "attention": events,
                }))
            }
        }
    }

    pub fn serve(&mut self, input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let reply = match serde_json::from_str::<Value>(&line) {
                Ok(Value::Array(batch)) => {
                    let r: Vec<Value> = batch.iter().filter_map(|m| self.handle(m)).collect();
                    (!r.is_empty()).then_some(Value::Array(r))
                }
                Ok(m) => self.handle(&m),
                Err(_) => Some(rpc_error(Value::Null, -32700, "parse error")),
            };
            if let Some(r) = reply {
                writeln!(output, "{r}")?;
                output.flush()?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodo_core::services::{CheckKind, ServiceCheck, Spec};

    fn map() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("home.nodo");
        let mut s = Store::create(&p).unwrap();
        s.save_device(&Device { id: 1, name: "Router".into(), saved: true, monitor: true, ..Default::default() }).unwrap();
        s.save_device(&Device { id: 2, name: "Phone".into(), ..Default::default() }).unwrap();
        s.add_event(Some(1), 10, EventKind::Down, "").unwrap();
        let c = ServiceCheck { id: 0, name: "Website".into(), device: None, spec: Spec::new(CheckKind::Http, "https://example.com"), enabled: true };
        let id = s.save_service(&c).unwrap();
        s.open_incident(id, 20, "HTTP 503").unwrap();
        (dir, p)
    }

    fn call(srv: &mut Server, id: i64, name: &str, args: Value) -> Value {
        srv.handle(&json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": args}})).unwrap()
    }

    #[test]
    fn read_only_by_default() {
        let (_d, p) = map();
        let mut srv = Server { map: p, writes: false, propose: None };
        let input = [json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}), json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})].iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
        let mut out = Vec::new();
        srv.serve(std::io::Cursor::new(input), &mut out).unwrap();
        let lines: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2, "the notification gets no answer");
        let names: Vec<&str> = lines[1]["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["incidents", "history", "network_map", "devices", "device", "services", "status"]);
        assert!(lines[1]["result"]["tools"].as_array().unwrap().iter().all(|t| t["annotations"]["readOnlyHint"] == true));
        assert_eq!(call(&mut srv, 3, "devices", json!({"saved_only": true}))["result"]["structuredContent"]["count"], 1);
        assert_eq!(call(&mut srv, 4, "device", json!({"device": "router"}))["result"]["structuredContent"]["name"], "Router");
        let st = call(&mut srv, 5, "status", json!({}));
        assert_eq!(st["result"]["structuredContent"]["devices_down"][0]["name"], "Router");
        assert_eq!(st["result"]["structuredContent"]["services_down"][0]["cause"], "HTTP 503");
        assert_eq!(call(&mut srv, 6, "services", json!({}))["result"]["structuredContent"]["services"][0]["down_since"], 20);
        assert_eq!(call(&mut srv, 7, "update_device", json!({"device": "1", "name": "x"}))["error"]["code"], -32602);
        assert_eq!(call(&mut srv, 8, "device", json!({"device": "nothing"}))["result"]["isError"], true);
    }

    #[test]
    fn writes_only_with_the_flag() {
        let (_d, p) = map();
        let mut srv = Server { map: p.clone(), writes: true, propose: None };
        let list = srv.handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), TOOLS.len());
        let r = call(&mut srv, 2, "update_device", json!({"device": "Phone", "owner": "Ana", "saved": true}));
        assert_eq!((r["result"]["structuredContent"]["owner"].clone(), r["result"]["structuredContent"]["saved"].clone()), (json!("Ana"), json!(true)));
        let r = call(&mut srv, 3, "add_service", json!({"kind": "tcp", "target": "10.0.0.1:22", "name": "SSH"}));
        assert_eq!(r["result"]["structuredContent"]["kind"], "tcp");
        let s = Store::open_read_only(&p).unwrap();
        assert_eq!(s.services().unwrap().len(), 2);
        assert!(s.events(Some(2), 5).unwrap().iter().any(|e| e.kind == EventKind::Edited));
    }

    #[test]
    fn propose_mode_leaves_cards_and_never_writes() {
        let (d, p) = map();
        let dir = d.path().join("proposals");
        let mut srv = Server { map: p.clone(), writes: false, propose: Some(dir.clone()) };
        let list = srv.handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
        let upd = list["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "update_device").expect("offered as a proposal");
        assert!(upd["description"].as_str().unwrap().contains("proposal"));
        let r = call(&mut srv, 2, "update_device", json!({"device": "Phone", "owner": "Ana", "reason": "her phone"}));
        assert_eq!(r["result"]["structuredContent"]["status"], "awaiting_approval");
        assert_eq!(r["result"]["structuredContent"]["preview"]["owner"], "Ana");
        let r = call(&mut srv, 3, "add_service", json!({"kind": "icmp", "target": "10.0.0.1"}));
        assert_eq!(r["result"]["structuredContent"]["preview"]["kind"], "icmp");
        assert_eq!(call(&mut srv, 4, "update_device", json!({"device": "printer"}))["result"]["isError"], true);
        let s = Store::open_read_only(&p).unwrap();
        assert_eq!(s.services().unwrap().len(), 1, "nothing written");
        assert_eq!(s.load().unwrap().device(2).unwrap().owner, "");
        let got = nodo_core::agent::collect(&dir);
        assert_eq!(got.iter().map(|p| p.tool.as_str()).collect::<Vec<_>>(), ["update_device", "add_service"]);
        assert_eq!(got[0].reason, "her phone");
    }

    #[test]
    fn incidents_history_and_the_network_map() {
        let (_d, p) = map();
        {
            let mut s = Store::open(&p).unwrap();
            let mut data = s.load().unwrap();
            let net = nodo_core::model::Network { id: 1, name: "Casa".into(), layer: nodo_core::model::Layer::Lan, cidr: Some("192.168.1.0/24".into()), gateway: None, parent: None, key: "lan:192.168.1.0/24".into(), facts: vec![] };
            s.save_network(&net).unwrap();
            let mut r = data.devices.remove(0);
            r.addrs.push(nodo_core::model::Addr { ip: "192.168.1.1".parse().unwrap(), layer: nodo_core::model::Layer::Lan, network: Some(1), source: nodo_core::model::Source::Manual, last_seen: 0 });
            s.save_device(&r).unwrap();
        }
        let mut srv = Server { map: p, writes: false, propose: None };
        let inc = call(&mut srv, 1, "incidents", json!({"days": 36500}))["result"]["structuredContent"].clone();
        assert_eq!(inc["incidents"][0]["cause"], "HTTP 503");
        assert_eq!(inc["incidents"][0]["ongoing"], true);
        assert_eq!(inc["incidents"][0]["service_name"], "Website");
        let h = call(&mut srv, 2, "history", json!({"device": "router"}))["result"]["structuredContent"].clone();
        assert_eq!(h["events"][0]["device_name"], "Router");
        let m = call(&mut srv, 3, "network_map", json!({}))["result"]["structuredContent"].clone();
        assert_eq!(m["networks"][0]["cidr"], "192.168.1.0/24");
        assert_eq!(m["networks"][0]["devices"][0]["name"], "Router");
        assert_eq!(m["devices_without_network"][0]["name"], "Phone");
    }
}
