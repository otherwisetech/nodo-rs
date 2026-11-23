//! `nodo`: Nodo's maps from a terminal or an agent, without the window. Same core as the app:
//! `nodo list --json`, `nodo show <device>`, `nodo scan --passive [--save]`.
mod background;
mod mcp;

use nodo_core::db::{MapData, Store};
use nodo_core::doc::Doc;
use nodo_core::model::*;
use std::path::PathBuf;
use std::process::ExitCode;

struct Args {
    cmd: String,
    rest: Vec<String>,
    map: Option<PathBuf>,
    json: bool,
    save: bool,
    flags: Vec<String>,
    aws_profile: Option<String>,
    regions: Vec<String>,
    opts: Vec<(String, String)>,
}

impl Args {
    fn opt(&self, k: &str) -> Option<&str> {
        self.opts.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
    }
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut a = Args { cmd: String::new(), rest: Vec::new(), map: None, json: false, save: false, flags: Vec::new(), aws_profile: None, regions: Vec::new(), opts: Vec::new() };
    while let Some(x) = it.next() {
        match x.as_str() {
            "--map" | "-m" => a.map = Some(PathBuf::from(it.next().ok_or_else(|| i18n::t!("cli.err.needs_value", flag = "--map"))?)),
            "--json" => a.json = true,
            "--save" => a.save = true,
            "--aws" => a.aws_profile = Some(it.next().ok_or_else(|| i18n::t!("cli.err.needs_value", flag = "--aws"))?),
            "--region" => a.regions.push(it.next().ok_or_else(|| i18n::t!("cli.err.needs_value", flag = "--region"))?),
            "--layout" | "-o" | "--output" | "--name" | "--device" | "--interval" | "--keyword" | "--slow" | "--cert-days" | "--expect" => {
                let k = x.trim_start_matches('-').to_string();
                a.opts.push((k, it.next().ok_or_else(|| i18n::t!("cli.err.needs_value", flag = x.clone()))?));
            }
            "-h" | "--help" => a.cmd = "help".into(),
            "-V" | "--version" => a.cmd = "version".into(),
            f if f.starts_with("--") => a.flags.push(f.trim_start_matches("--").to_string()),
            _ if a.cmd.is_empty() => a.cmd = x,
            _ => a.rest.push(x),
        }
    }
    Ok(a)
}

fn map_path(a: &Args) -> Result<PathBuf, String> {
    match &a.map {
        Some(p) => Ok(p.clone()),
        None => nodo_core::paths::recent().into_iter().next().ok_or_else(|| i18n::t!("cli.err.no_map")),
    }
}

fn load(a: &Args) -> Result<(PathBuf, MapData), String> {
    let p = map_path(a)?;
    let s = Store::open_read_only(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok((p, s.load().map_err(|e| e.to_string())?))
}

pub(crate) fn device_json(d: &Device) -> serde_json::Value {
    let mut v = serde_json::to_value(d).unwrap_or_default();
    if let Some(o) = v.as_object_mut() {
        o.insert("primary_ip".into(), d.primary_ip().map(|i| i.to_string()).into());
        o.insert("type".into(), d.kind.id().into());
    }
    v
}

/// A device by id, exact name, IP, MAC or host name (case-insensitive), else a unique
/// substring of the name.
pub(crate) fn find<'a>(m: &'a MapData, q: &str) -> Result<&'a Device, String> {
    let ql = q.to_lowercase();
    let mac = nodo_core::mac::normalize(q);
    let exact: Vec<&Device> = m
        .devices
        .iter()
        .filter(|d| d.id.to_string() == q || d.name.to_lowercase() == ql || d.ips().any(|i| i.to_string() == q) || mac.as_deref().is_some_and(|mm| d.macs().any(|x| x == mm)) || d.hostname.as_deref().is_some_and(|h| h.to_lowercase() == ql))
        .collect();
    if let [d] = exact.as_slice() {
        return Ok(d);
    }
    let fuzzy: Vec<&Device> = m.devices.iter().filter(|d| d.name.to_lowercase().contains(&ql)).collect();
    match fuzzy.as_slice() {
        [d] => Ok(d),
        [] if exact.is_empty() => Err(i18n::t!("cli.err.no_device", query = q)),
        _ => Err(i18n::t!("cli.err.ambiguous", query = q, n = exact.len().max(fuzzy.len()))),
    }
}

fn pad(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n >= w { format!("{} ", s.chars().take(w.saturating_sub(1)).collect::<String>()) } else { format!("{s}{}", " ".repeat(w - n)) }
}

fn cmd_list(a: &Args) -> Result<(), String> {
    let (path, m) = load(a)?;
    if a.json {
        let v = serde_json::json!({ "map": path.display().to_string(), "networks": m.networks, "devices": m.devices.iter().map(device_json).collect::<Vec<_>>() });
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(());
    }
    println!("{}", i18n::t!("cli.list.header", map = path.display(), n = m.devices.len()));
    println!("{}{}{}{}{}", pad("ID", 6), pad(&i18n::t!("cli.col.name"), 28), pad(&i18n::t!("cli.col.type"), 10), pad("IP", 18), i18n::t!("cli.col.seen")); // i18n-ok: column
    for d in &m.devices {
        let seen = if d.last_seen > 0 { nodo_core::time::format_utc(d.last_seen) } else { String::from("-") };
        println!("{}{}{}{}{}", pad(&d.id.to_string(), 6), pad(&d.name, 28), pad(d.kind.id(), 10), pad(&d.primary_ip().map(|i| i.to_string()).unwrap_or_default(), 18), seen);
    }
    Ok(())
}

fn cmd_show(a: &Args) -> Result<(), String> {
    let q = a.rest.first().ok_or_else(|| i18n::t!("cli.err.show_needs"))?;
    let (path, m) = load(a)?;
    let d = find(&m, q)?;
    if a.json {
        let mut v = device_json(d);
        if let (Some(o), Ok(s)) = (v.as_object_mut(), Store::open_read_only(&path)) {
            o.insert("events".into(), serde_json::to_value(s.events(Some(d.id), 50).unwrap_or_default()).unwrap_or_default());
        }
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(());
    }
    let line = |k: String, v: String| {
        if !v.is_empty() {
            println!("{} {v}", pad(&k, 14));
        }
    };
    println!("{} (#{})", d.name, d.id);
    line(i18n::t!("cli.col.type"), d.kind.id().to_string());
    for a in &d.addrs {
        line(a.layer.id().to_string(), a.ip.to_string());
    }
    for i in &d.idents {
        line(i.kind.id().to_string(), i.value.clone());
    }
    line(i18n::t!("cli.col.vendor"), d.vendor.clone().unwrap_or_default());
    line(i18n::t!("cli.col.hostname"), d.hostname.clone().unwrap_or_default());
    line(i18n::t!("cli.col.os"), d.os.clone().unwrap_or_default());
    line(i18n::t!("cli.col.model"), d.model.clone().unwrap_or_default());
    line(i18n::t!("cli.col.owner"), d.owner.clone());
    line(i18n::t!("cli.col.location"), d.location.clone());
    line(i18n::t!("cli.col.tags"), d.tags.join(", "));
    line(i18n::t!("cli.col.seen"), if d.last_seen > 0 { nodo_core::time::format_utc(d.last_seen) } else { String::new() });
    for s in &d.services {
        line(i18n::t!("cli.col.service"), match s.port {
            Some(p) => format!("{} :{p}", s.name),
            None => s.name.clone(),
        });
    }
    for (k, v) in d.facts.iter().filter(|(k, _)| k != "name_rank") {
        line(k.clone(), v.clone());
    }
    line(i18n::t!("cli.col.notes"), d.notes.clone());
    Ok(())
}

pub(crate) fn service_json(s: &nodo_core::services::ServiceCheck) -> serde_json::Value {
    serde_json::json!({ "id": s.id, "name": s.name, "device": s.device, "kind": s.spec.kind.id(), "target": s.spec.target, "interval_s": s.spec.interval_s, "keyword": s.spec.keyword, "expect_status": s.spec.expect_status, "slow_ms": s.spec.slow_ms, "cert_days": s.spec.cert_days, "enabled": s.enabled })
}

/// `nodo services list|add|check`.
fn cmd_services(a: &Args) -> Result<(), String> {
    use nodo_core::services::{CheckKind, DAY, ServiceCheck, Spec};
    let sub = a.rest.first().map(String::as_str).unwrap_or("list");
    let path = map_path(a)?;
    match sub {
        "list" | "ls" => {
            let s = Store::open_read_only(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let list = s.services().map_err(|e| e.to_string())?;
            let now = nodo_core::time::now();
            if a.json {
                let v: Vec<serde_json::Value> = list
                    .iter()
                    .map(|x| {
                        let h = s.service_hours(x.id, now - 31 * DAY).unwrap_or_default();
                        let mut j = service_json(x);
                        j["uptime"] = serde_json::json!({ "24h": nodo_core::services::uptime(&h, now, DAY), "7d": nodo_core::services::uptime(&h, now, 7 * DAY), "30d": nodo_core::services::uptime(&h, now, 30 * DAY) });
                        j["incidents"] = serde_json::to_value(s.incidents(Some(x.id), 20).unwrap_or_default()).unwrap_or_default();
                        j
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
                return Ok(());
            }
            println!("{}{}{}{}{}", pad("ID", 6), pad(&i18n::t!("cli.col.name"), 26), pad(&i18n::t!("cli.col.kind"), 7), pad(&i18n::t!("cli.col.target"), 40), i18n::t!("cli.col.uptime")); // i18n-ok: column
            for x in &list {
                let h = s.service_hours(x.id, now - 31 * DAY).unwrap_or_default();
                let up = |w: i64| nodo_core::services::uptime(&h, now, w).map(|u| format!("{:.2}%", u * 100.)).unwrap_or_else(|| "-".into()); // i18n-ok: number
                println!("{}{}{}{}{} / {} / {}", pad(&x.id.to_string(), 6), pad(&x.name, 26), pad(x.spec.kind.id(), 7), pad(&x.spec.target, 40), up(DAY), up(7 * DAY), up(30 * DAY));
            }
            Ok(())
        }
        "add" => {
            let kind = a.rest.get(1).and_then(|k| CheckKind::from_id(k)).ok_or_else(|| i18n::t!("cli.svc.add_needs"))?;
            let target = a.rest.get(2).ok_or_else(|| i18n::t!("cli.svc.add_needs"))?;
            let mut spec = Spec::new(kind, target);
            let num = |k: &str| a.opt(k).and_then(|v| v.parse::<u32>().ok());
            if let Some(i) = num("interval") {
                spec.interval_s = i.clamp(10, 86400);
            }
            spec.keyword = a.opt("keyword").map(str::to_string);
            spec.slow_ms = num("slow");
            if let Some(c) = num("cert-days") {
                spec.cert_days = Some(c);
            }
            spec.expect_status = a.opt("expect").and_then(|v| v.parse().ok());
            let mut doc = Doc::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let device = match a.opt("device") {
                Some(q) => Some(find(&doc.data, q)?.id),
                None => None,
            };
            let name = a.opt("name").map(str::to_string).unwrap_or_else(|| target.clone());
            let mut s = ServiceCheck { id: 0, name, device, spec, enabled: true };
            s.id = doc.store.save_service(&s).map_err(|e| e.to_string())?;
            if a.json {
                println!("{}", serde_json::to_string_pretty(&service_json(&s)).unwrap_or_default());
            } else {
                println!("{}", i18n::t!("cli.svc.added", id = s.id, name = s.name.clone()));
            }
            Ok(())
        }
        "check" => {
            let s = Store::open_read_only(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let list = s.services().map_err(|e| e.to_string())?;
            let pick: Vec<&ServiceCheck> = match a.rest.get(1) {
                Some(q) => list.iter().filter(|x| x.id.to_string() == *q || x.name.eq_ignore_ascii_case(q)).collect(),
                None => list.iter().collect(),
            };
            if pick.is_empty() {
                return Err(i18n::t!("cli.svc.none"));
            }
            let results: Vec<(&ServiceCheck, nodo_core::services::Outcome)> = std::thread::scope(|sc| {
                let hs: Vec<_> = pick.iter().map(|x| sc.spawn(move || (*x, nodo_core::services::check(&x.spec, &nodo_core::probe::SystemProber)))).collect();
                hs.into_iter().filter_map(|h| h.join().ok()).collect()
            });
            let failed = results.iter().any(|(_, o)| !o.ok);
            if a.json {
                let v: Vec<serde_json::Value> = results.iter().map(|(x, o)| serde_json::json!({ "service": service_json(x), "result": o })).collect();
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            } else {
                for (x, o) in &results {
                    let lat = o.latency_ms.map(|l| format!("{l:.0} ms")).unwrap_or_default(); // i18n-ok: unit
                    let cert = o.cert_days.map(|d| i18n::t!("cli.svc.cert", days = d)).unwrap_or_default();
                    println!("{} {}{}{} {}", if o.ok { "✓" } else { "✗" }, pad(&x.name, 26), pad(&lat, 10), o.detail, cert); // i18n-ok: marks
                }
            }
            if failed { Err(String::new()) } else { Ok(()) }
        }
        other => Err(i18n::t!("cli.err.unknown", command = format!("services {other}"))),
    }
}

/// `nodo today`: what the map says needs attention (the Today view's items, from the file).
fn cmd_today(a: &Args) -> Result<(), String> {
    let p = map_path(a)?;
    let s = Store::open_read_only(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    let m = s.load().map_err(|e| e.to_string())?;
    let (down, svcs) = nodo_core::feed::from_store(&s, &m);
    let uid = nodo_core::sync::known_map_uid(&s).unwrap_or_default();
    let events = nodo_core::feed::events(&uid, &down, &svcs, nodo_core::time::now());
    if a.json {
        println!("{}", serde_json::to_string_pretty(&events).unwrap_or_default());
    } else if events.is_empty() {
        println!("{}", i18n::t!("cli.today.none"));
    } else {
        for e in &events {
            println!("{}  {}", e.when.as_deref().unwrap_or("-"), e.title); // i18n-ok: layout
        }
    }
    Ok(())
}

/// `nodo sync`: publish the map's saved devices and sites to the shared registry (when on).
fn cmd_sync(a: &Args) -> Result<(), String> {
    let p = map_path(a)?;
    let doc = Doc::open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    let Doc { mut store, data, .. } = doc;
    match nodo_core::sync::sync_shared(&mut store, &data, &otherwise_link::Root::system()).map_err(|e| e.to_string())? {
        Some(r) => {
            if a.json {
                println!("{}", serde_json::json!({"pushed": r.pushed, "unlinked": r.unlinked, "repointed": r.repointed, "errors": r.errors}));
            } else {
                println!("{}", i18n::t!("cli.sync.done", pushed = r.pushed, unlinked = r.unlinked));
            }
        }
        None => println!("{}", if a.json { serde_json::json!({"enabled": false}).to_string() } else { i18n::t!("cli.sync.off") }),
    }
    Ok(())
}

fn cmd_mcp(a: &Args) -> Result<(), String> {
    let map = map_path(a)?;
    let mut srv = mcp::Server { map, writes: a.flags.iter().any(|f| f == "allow-writes") };
    srv.serve(std::io::stdin().lock(), std::io::stdout().lock()).map_err(|e| e.to_string())
}

fn cmd_scan(a: &Args) -> Result<(), String> {
    let mut d = Discovery::default();
    let any_source = a.flags.iter().any(|f| matches!(f.as_str(), "tailscale" | "wireguard")) || a.aws_profile.is_some();
    if a.flags.iter().any(|f| f == "passive") || !any_source {
        eprintln!("{}", i18n::t!("cli.scan.passive"));
        d.extend(nodo_core::discover::passive(Default::default()));
    }
    if a.flags.iter().any(|f| f == "tailscale") {
        d.extend(nodo_core::discover::tailscale::discover());
    }
    if a.flags.iter().any(|f| f == "wireguard") {
        d.extend(nodo_core::discover::wireguard::discover(nodo_core::time::now()));
    }
    if let Some(p) = &a.aws_profile {
        let regions = if a.regions.is_empty() { nodo_core::discover::aws::default_region(p).into_iter().collect() } else { a.regions.clone() };
        d.extend(nodo_core::discover::aws::discover(p, &regions));
    }
    for (src, note) in &d.notes {
        eprintln!("{}: {note}", src.id()); // i18n-ok: source prefix
    }
    if a.save {
        let path = map_path(a)?;
        let mut doc = Doc::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let r = doc.apply(&d, nodo_core::time::now()).map_err(|e| e.to_string())?;
        eprintln!("{}", i18n::t!("cli.scan.saved", map = path.display(), new = r.report.new.len(), updated = r.report.updated.len()));
    }
    if a.json {
        let devs: Vec<serde_json::Value> = d
            .devices
            .iter()
            .map(|o| {
                serde_json::json!({
                    "source": o.source.map(|s| s.id()), "layer": o.layer.map(|l| l.id()), "ips": o.ips.iter().map(|i| i.to_string()).collect::<Vec<_>>(),
                    "idents": o.idents, "name": o.name, "hostname": o.hostname, "vendor": o.vendor, "model": o.model, "os": o.os,
                    "type": o.kind.map(|k| k.id()), "online": o.online, "owner": o.owner, "services": o.services, "facts": o.facts,
                })
            })
            .collect();
        let nets: Vec<serde_json::Value> = d.networks.iter().map(|n| serde_json::json!({ "key": n.key, "name": n.name, "layer": n.layer.map(|l| l.id()), "cidr": n.cidr, "gateway": n.gateway.map(|g| g.to_string()) })).collect();
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "networks": nets, "observations": devs, "notes": d.notes.iter().map(|(s, n)| serde_json::json!({"source": s.id(), "note": n})).collect::<Vec<_>>() })).unwrap_or_default());
    } else {
        for o in &d.devices {
            let ips: Vec<String> = o.ips.iter().map(|i| i.to_string()).collect();
            let mac = o.idents.iter().find(|i| i.kind == IdentKind::Mac).map(|i| i.value.clone()).unwrap_or_default();
            println!("{}{}{}{}{}", pad(o.source.map(|s| s.id()).unwrap_or_default(), 11), pad(&ips.join(" "), 32), pad(&mac, 19), pad(o.name.as_deref().or(o.hostname.as_deref()).unwrap_or_default(), 28), o.vendor.as_deref().or(o.model.as_deref()).unwrap_or_default());
        }
    }
    Ok(())
}

/// `nodo export <svg|pdf|mermaid|csv|json> [--layout radial|subnet|network] [-o FILE]`.
fn cmd_export(a: &Args) -> Result<(), String> {
    let format = a.rest.first().map(String::as_str).unwrap_or("json");
    let (path, m) = load(a)?;
    let mode = nodo_core::layout::Mode::from_id(a.opt("layout").unwrap_or("radial"));
    let g = nodo_core::graph::build(&m, &Default::default());
    let p = nodo_core::layout::layout(&g, mode);
    let title = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let online = |id: DeviceId| m.device(id).and_then(|d| d.online);
    let bytes = match format {
        "svg" => nodo_core::render::to_svg(&nodo_core::render::scene(&m, &g, &p, &title, &online), false).into_bytes(),
        "pdf" => nodo_core::render::to_pdf(&nodo_core::render::scene(&m, &g, &p, &title, &online)),
        "mermaid" | "mmd" => nodo_core::export::to_mermaid(&m, &g).into_bytes(),
        "csv" => nodo_core::export::to_csv(&m).into_bytes(),
        "json" => nodo_core::export::to_json(&m).into_bytes(),
        other => return Err(i18n::t!("cli.err.format", format = other)),
    };
    match a.opt("o").or(a.opt("output")) {
        Some(out) => std::fs::write(out, bytes).map_err(|e| e.to_string()),
        None => {
            use std::io::Write as _;
            std::io::stdout().write_all(&bytes).map_err(|e| e.to_string())
        }
    }
}

/// `nodo map move <from> <to> [--force]`
fn cmd_map(a: &Args) -> Result<(), String> {
    let (Some("move"), Some(from), Some(to)) = (a.rest.first().map(String::as_str), a.rest.get(1), a.rest.get(2)) else { return Err(i18n::t!("cli.map.needs")) };
    let (from, mut to) = (PathBuf::from(from), PathBuf::from(to));
    if to.is_dir() {
        to = to.join(from.file_name().unwrap_or_default());
    }
    if to.extension().is_none() {
        to.set_extension(nodo_core::db::EXTENSION);
    }
    let host = nodo_core::mapfile::host();
    nodo_core::mapfile::move_file(&from, &to, &host, nodo_core::time::now(), a.flags.iter().any(|f| f == "force")).map_err(|e| e.to_string())?;
    nodo_core::paths::replace_recent(&from, &to);
    println!("{}", i18n::t!("cli.map.moved", from = from.display(), to = to.display()));
    Ok(())
}

fn main() -> ExitCode {
    let lang = std::env::var("LC_ALL").ok().or_else(|| std::env::var("LANG").ok()).and_then(|l| i18n::match_system(&l)).unwrap_or("en");
    i18n::set_locale(lang);
    nodo_core::shell_env::init();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("background") {
        return ExitCode::from(background::run(&argv[1..]) as u8);
    }
    let a = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let r = match a.cmd.as_str() {
        "list" | "ls" => cmd_list(&a),
        "show" => cmd_show(&a),
        "scan" => cmd_scan(&a),
        "services" | "svc" => cmd_services(&a),
        "export" => cmd_export(&a),
        "today" => cmd_today(&a),
        "sync" => cmd_sync(&a),
        "mcp" => cmd_mcp(&a),
        "map" => cmd_map(&a),
        "maps" => {
            for p in nodo_core::paths::recent() {
                println!("{}", p.display());
            }
            Ok(())
        }
        "version" => {
            println!("nodo {}", env!("CARGO_PKG_VERSION")); // i18n-ok: product name and version
            Ok(())
        }
        "" | "help" => {
            println!("{}", i18n::t!("cli.usage"));
            Ok(())
        }
        other => Err(i18n::t!("cli.err.unknown", command = other)),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("{e}");
            }
            ExitCode::FAILURE
        }
    }
}
