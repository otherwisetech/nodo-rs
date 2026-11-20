//! `nodo`: Nodo's maps from a terminal or an agent, without the window. Same core as the app:
//! `nodo list --json`, `nodo show <device>`, `nodo scan --passive [--save]`.
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
            "--layout" | "-o" | "--output" => {
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

fn device_json(d: &Device) -> serde_json::Value {
    let mut v = serde_json::to_value(d).unwrap_or_default();
    if let Some(o) = v.as_object_mut() {
        o.insert("primary_ip".into(), d.primary_ip().map(|i| i.to_string()).into());
        o.insert("type".into(), d.kind.id().into());
    }
    v
}

/// A device by id, exact name, IP, MAC or host name (case-insensitive), else a unique
/// substring of the name.
fn find<'a>(m: &'a MapData, q: &str) -> Result<&'a Device, String> {
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
                    "type": o.kind.map(|k| k.id()), "online": o.online, "services": o.services, "facts": o.facts,
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

fn main() -> ExitCode {
    let lang = std::env::var("LC_ALL").ok().or_else(|| std::env::var("LANG").ok()).and_then(|l| i18n::match_system(&l)).unwrap_or("en");
    i18n::set_locale(lang);
    nodo_core::shell_env::init();
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
        "export" => cmd_export(&a),
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
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
