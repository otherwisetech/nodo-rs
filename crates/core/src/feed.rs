//! Nodo's part of the Otherwise apps' Today view (`today/nodo.json` in the shared folder):
//! devices down, services down or slow, certificates expiring. Alerts carry a short expiry
//! (Nodo rewrites them while it watches, so a quit Nodo leaves nothing stale behind);
//! certificate events link to Vigía's quick add, prefilled, to track the renewal there.
use crate::db::{MapData, Store};
use crate::model::DeviceId;
use crate::services::ServiceCheck;
use otherwise_link::today::{Event, EventKind, Severity};

/// How far ahead an expiring certificate shows (or the service's own warning, if longer).
pub const CERT_HORIZON_DAYS: i64 = 30;
/// Alerts vanish this long after the last rewrite.
const ALERT_TTL: i64 = 2 * 3600;
/// `meta` key holding a service's certificate expiry (Unix seconds), written when it changes.
pub const CERT_META: &str = "svc_cert:";

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceDown {
    pub id: DeviceId,
    pub name: String,
    pub since: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServiceStatus {
    pub check: ServiceCheck,
    /// Down since then.
    pub down_since: Option<i64>,
    pub detail: String,
    /// Answering, but slower than its threshold.
    pub slow_ms: Option<f64>,
    /// The certificate's end of validity (Unix seconds), when known.
    pub cert_until: Option<i64>,
}

fn day(secs: i64) -> String {
    let (y, m, d) = crate::time::civil(secs.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn events(map_uid: &str, devices: &[DeviceDown], services: &[ServiceStatus], now: i64) -> Vec<Event> {
    // on the hour, so rewriting every few seconds doesn't change the file
    let alert_until = crate::time::format_rfc3339((now / 3600 + 1) * 3600 + ALERT_TTL);
    let mut out = Vec::new();
    for d in devices {
        out.push(
            Event::new(format!("{map_uid}-device-{}-down", d.id), EventKind::Alert, i18n::t!("core.today.device_down", name = d.name))
                .when(crate::time::format_rfc3339(d.since))
                .severity(Severity::High)
                .link(&crate::links::device_link(d.id, Some(map_uid)))
                .expires(alert_until.clone()),
        );
    }
    for s in services.iter().filter(|s| s.check.enabled) {
        let c = &s.check;
        let link = crate::links::service_link(c.id, Some(map_uid));
        if let Some(since) = s.down_since {
            let mut e = Event::new(format!("{map_uid}-svc-{}-down", c.id), EventKind::Alert, i18n::t!("core.today.service_down", name = c.name)).when(crate::time::format_rfc3339(since)).severity(Severity::High).link(&link).expires(alert_until.clone());
            if !s.detail.trim().is_empty() {
                e = e.body(s.detail.trim());
            }
            out.push(e);
        } else if let Some(ms) = s.slow_ms {
            out.push(Event::new(format!("{map_uid}-svc-{}-slow", c.id), EventKind::Alert, i18n::t!("core.today.service_slow", name = c.name, ms = format!("{ms:.0}"))).severity(Severity::Normal).link(&link).expires(alert_until.clone()));
        }
        if let (Some(until), Some((host, _))) = (s.cert_until, crate::links::tls_host(c)) {
            let days = (until - now).div_euclid(86_400);
            let horizon = CERT_HORIZON_DAYS.max(c.spec.cert_days.unwrap_or(0) as i64);
            if days > horizon {
                continue;
            }
            let expiry = day(until);
            let title = if days < 0 { i18n::t!("core.today.cert_expired", host = host) } else { i18n::tn!("core.today.cert_expiring", days, host = host) };
            let track = crate::links::vigia_certificate(c, Some(&expiry), &i18n::t!("core.today.cert_item", host = host)).unwrap_or(link.clone());
            let severity = match days {
                d if d < 0 => Severity::Critical,
                0..=3 => Severity::High,
                4..=14 => Severity::Normal,
                _ => Severity::Low,
            };
            out.push(Event::new(format!("{map_uid}-svc-{}-cert-{expiry}", c.id), EventKind::Due, title).when(expiry.clone()).severity(severity).body(i18n::t!("core.today.track_in_vigia")).link(&track).expires(day(until.max(now) + 7 * 86_400)));
        }
    }
    out
}

/// The same from the map file alone (the CLI and `nodo mcp`, without a running monitor):
/// saved, monitored devices whose last transition is "down", services with an open incident,
/// certificate dates the app recorded.
pub fn from_store(store: &Store, data: &MapData) -> (Vec<DeviceDown>, Vec<ServiceStatus>) {
    let mut down = Vec::new();
    for d in data.devices.iter().filter(|d| d.saved && d.monitor) {
        if let Some((at, false)) = store.transitions(d.id, 0).ok().and_then(|t| t.last().copied()) {
            down.push(DeviceDown { id: d.id, name: d.name.clone(), since: at });
        }
    }
    let open = store.incidents(None, 1000).unwrap_or_default();
    let services = store
        .services()
        .unwrap_or_default()
        .into_iter()
        .map(|c| {
            let inc = open.iter().find(|i| i.service == c.id && i.end.is_none());
            let cert_until = store.meta(&format!("{CERT_META}{}", c.id)).ok().flatten().and_then(|v| v.parse().ok());
            ServiceStatus { down_since: inc.map(|i| i.start), detail: inc.map(|i| i.cause.clone()).unwrap_or_default(), slow_ms: None, cert_until, check: c }
        })
        .collect();
    (down, services)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{CheckKind, Spec};

    fn svc(id: i64, url: &str) -> ServiceCheck {
        ServiceCheck { id, name: format!("svc{id}"), device: None, spec: Spec::new(CheckKind::Http, url), enabled: true }
    }

    #[test]
    fn alerts_and_certificates() {
        let now = crate::time::days_from_civil(2026, 11, 21) * 86_400 + 8 * 3600;
        let st = |c, down: Option<i64>, slow: Option<f64>, cert: Option<i64>| ServiceStatus { check: c, down_since: down, detail: "HTTP 503".into(), slow_ms: slow, cert_until: cert };
        let v = events(
            "ab12",
            &[DeviceDown { id: 3, name: "NAS".into(), since: now - 600 }],
            &[st(svc(1, "https://api.example.com"), Some(now - 60), None, Some(now + 90 * 86_400)), st(svc(2, "https://otherwisetech.com"), None, Some(2300.), Some(now + 12 * 86_400)), st(svc(3, "https://old.example.org"), None, None, Some(now - 86_400)), st(svc(4, "http://plain.example"), None, None, Some(now))],
            now,
        );
        let ids: Vec<&str> = v.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["ab12-device-3-down", "ab12-svc-1-down", "ab12-svc-2-slow", "ab12-svc-2-cert-2026-12-03", "ab12-svc-3-cert-2026-11-20"]);
        assert_eq!(v[0].link.as_deref(), Some("otherwise://nodo/device/3?map=ab12"));
        assert_eq!(v[1].body.as_deref(), Some("HTTP 503"));
        assert_eq!((v[0].kind, v[3].kind, v[3].severity, v[4].severity), (EventKind::Alert, EventKind::Due, Severity::Normal, Severity::Critical));
        assert!(v[3].link.as_deref().unwrap().starts_with("otherwise://vigia/new?kind=certificate"));
        assert!(v[3].link.as_deref().unwrap().contains("expires=2026-12-03"));
        assert_eq!(v[0].expires.as_deref(), Some("2026-11-21T11:00:00Z"));
        assert_eq!(events("ab12", &[DeviceDown { id: 3, name: "NAS".into(), since: now - 600 }], &[], now + 1800)[0].expires, v[0].expires, "stable within the hour");
    }

    #[test]
    fn from_the_map_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::create(&dir.path().join("m.nodo")).unwrap();
        let d = crate::model::Device { id: 1, name: "Router".into(), saved: true, monitor: true, ..Default::default() };
        s.save_device(&d).unwrap();
        s.add_event(Some(1), 100, crate::model::EventKind::Up, "").unwrap();
        s.add_event(Some(1), 200, crate::model::EventKind::Down, "").unwrap();
        let mut c = svc(0, "https://example.com");
        c.id = s.save_service(&c).unwrap();
        s.open_incident(c.id, 150, "timeout").unwrap();
        s.set_meta(&format!("{CERT_META}{}", c.id), "999").unwrap();
        let data = s.load().unwrap();
        let (down, svcs) = from_store(&s, &data);
        assert_eq!(down, vec![DeviceDown { id: 1, name: "Router".into(), since: 200 }]);
        assert_eq!((svcs[0].down_since, svcs[0].detail.as_str(), svcs[0].cert_until), (Some(150), "timeout", Some(999)));
    }
}
