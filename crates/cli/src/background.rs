//! `nodo background manifest | run monitor`: Otherwise Helper's job (logic in
//! `nodo_core::background`). Reads the most recent map only; one report line on stdout;
//! notifications go to `<data root>/nodo/outbox/`.
use nodo_core::background::{self as bg, Change, Memory};
use otherwise_link::background::{Job, Label, Manifest, Notification, Report};
use otherwise_link::today::Severity;
use otherwise_link::Root;

fn label(key: &str) -> Label {
    let mut l = Label::en(key, &i18n::tr_in("en", key));
    for lang in ["es", "pt", "fr"] {
        l = l.with(lang, &i18n::tr_in(lang, key));
    }
    l
}

/// What the bundle ships as `Contents/Resources/otherwise-background.json`.
pub fn manifest() -> Manifest {
    let mut m = Job::every(bg::MONITOR, label("cli.bg.monitor_label"), "5m", &["background", "run", bg::MONITOR]); // i18n-ok: arguments
    m.exe = Some("nodo".into()); // i18n-ok: binary name
    // the app monitors the map itself while it is open
    m.while_open = false;
    m.network = true;
    m.timeout_secs = Some(120);
    m.memory_mb = Some(300);
    Manifest { schema: 1, app: nodo_core::sync::APP.into(), jobs: vec![m] }
}

pub fn run(args: &[String]) -> i32 {
    if let Some(l) = std::env::var(otherwise_link::background::ENV_LANG).ok().filter(|l| !l.is_empty()) {
        i18n::set_locale(&l);
    }
    let (report, code) = match (args.first().map(String::as_str), args.get(1).map(String::as_str)) {
        (Some("manifest"), _) => {
            print!("{}", manifest().to_json());
            return 0;
        }
        (Some("run"), Some(bg::MONITOR)) => monitor(),
        _ => (Report::status(i18n::t!("cli.bg.unknown_job")), 2),
    };
    println!("{}", report.line());
    code
}

fn monitor() -> (Report, i32) {
    let Some(path) = nodo_core::paths::recent().into_iter().next() else { return (Report::idle(), 0) };
    let store = match nodo_core::db::Store::open_read_only(&path) {
        Ok(s) => s,
        Err(e) => return (Report::status(i18n::t!("cli.bg.failed", error = e)), 1),
    };
    let (map, services) = match (store.load(), store.services()) {
        (Ok(m), Ok(s)) => (m, s),
        (Err(e), _) | (_, Err(e)) => return (Report::status(i18n::t!("cli.bg.failed", error = e)), 1),
    };
    let uid = nodo_core::sync::known_map_uid(&store);
    drop(store);
    if !map.devices.iter().any(|d| d.saved && d.monitor) && !services.iter().any(|s| s.enabled) {
        return (Report::idle(), 0);
    }
    let root = Root::system();
    let state = bg::state_path(&root);
    let mut mem = Memory::load(&state);
    let key = path.display().to_string();
    let r = bg::round(&key, &map, &services, &mut mem, &nodo_core::probe::SystemProber, bg::RETRY_AFTER);
    let _ = mem.save(&state);
    let name = uid;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    for c in &r.changes {
        let n = match c {
            Change::DeviceDown { id, name: d } => Notification::new(format!("dev-{id}-down-{now}"), i18n::t!("cli.bg.device_down", device = d)).severity(Severity::High).link(&nodo_core::links::device_link(*id, name.as_deref())),
            Change::DeviceUp { id, name: d } => Notification::new(format!("dev-{id}-up-{now}"), i18n::t!("cli.bg.device_up", device = d)).link(&nodo_core::links::device_link(*id, name.as_deref())),
            Change::ServiceDown { id, name: s, detail } => Notification::new(format!("svc-{id}-down-{now}"), i18n::t!("cli.bg.service_down", service = s)).body(detail.clone()).severity(Severity::High).link(&nodo_core::links::service_link(*id, name.as_deref())),
            Change::ServiceUp { id, name: s } => Notification::new(format!("svc-{id}-up-{now}"), i18n::t!("cli.bg.service_up", service = s)).link(&nodo_core::links::service_link(*id, name.as_deref())),
        };
        let _ = otherwise_link::background::notify(&root, nodo_core::sync::APP, &n);
    }
    let down = r.devices_down + r.services_down;
    let status = if down > 0 { i18n::tn!("cli.bg.down_status", down) } else { i18n::tn!("cli.bg.up_status", r.devices + r.services) };
    (Report::status(status), 0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn manifest_is_valid_and_translated() {
        let m = super::manifest();
        let back = otherwise_link::background::Manifest::parse(&m.to_json(), "nodo").unwrap();
        assert_eq!(back.jobs.len(), 1);
        let j = &back.jobs[0];
        assert!(j.exe.as_deref() == Some("nodo") && !j.while_open && j.network);
        assert_ne!(j.label.get("es"), j.label.get("en"));
    }
}
