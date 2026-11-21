//! The map file: one SQLite database the user picks (File → New network map… / Open…). Plain
//! rollback journal (no WAL) so the single file can live in a synced folder; an integrity check
//! and the application id are verified on open; the schema is migrated forward with
//! `PRAGMA user_version`. One file can hold several networks.
use crate::model::*;
use rusqlite::{Connection, OptionalExtension, params};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

/// `NODO` in ASCII, in the SQLite header so other tools can tell what the file is.
pub const APPLICATION_ID: i32 = 0x4E4F_444F;
/// Suggested extension for map files.
pub const EXTENSION: &str = "nodo";

/// Schema steps; `MIGRATIONS[i]` takes the file from version `i` to `i + 1`. Never edit a
/// shipped step: add a new one.
pub const MIGRATIONS: &[&str] = &[
    // 1: networks, devices, addresses, identities, events, meta
    "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
     CREATE TABLE networks (
       id INTEGER PRIMARY KEY, key TEXT NOT NULL UNIQUE, name TEXT NOT NULL, layer TEXT NOT NULL,
       cidr TEXT, gateway TEXT, parent INTEGER REFERENCES networks(id) ON DELETE SET NULL,
       facts TEXT NOT NULL DEFAULT '[]');
     CREATE TABLE devices (
       id INTEGER PRIMARY KEY, name TEXT NOT NULL, name_manual INTEGER NOT NULL DEFAULT 0,
       kind TEXT NOT NULL DEFAULT 'unknown', kind_manual INTEGER NOT NULL DEFAULT 0,
       owner TEXT NOT NULL DEFAULT '', location TEXT NOT NULL DEFAULT '', notes TEXT NOT NULL DEFAULT '',
       tags TEXT NOT NULL DEFAULT '[]', links TEXT NOT NULL DEFAULT '[]', fields TEXT NOT NULL DEFAULT '[]',
       saved INTEGER NOT NULL DEFAULT 0, monitor INTEGER NOT NULL DEFAULT 0,
       vendor TEXT, hostname TEXT, os TEXT, model TEXT,
       services TEXT NOT NULL DEFAULT '[]', facts TEXT NOT NULL DEFAULT '[]',
       first_seen INTEGER NOT NULL DEFAULT 0, last_seen INTEGER NOT NULL DEFAULT 0, online INTEGER);
     CREATE TABLE addresses (
       device INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE, ip TEXT NOT NULL, layer TEXT NOT NULL,
       network INTEGER REFERENCES networks(id) ON DELETE SET NULL, source TEXT NOT NULL, last_seen INTEGER NOT NULL DEFAULT 0,
       PRIMARY KEY (device, ip, layer));
     CREATE TABLE identities (
       device INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE, kind TEXT NOT NULL, value TEXT NOT NULL,
       PRIMARY KEY (device, kind, value));
     CREATE TABLE events (
       id INTEGER PRIMARY KEY, device INTEGER REFERENCES devices(id) ON DELETE CASCADE,
       at INTEGER NOT NULL, kind TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '');
     CREATE INDEX events_device ON events(device, at);",
    // 2: look devices up by address and identity value
    "CREATE INDEX addresses_ip ON addresses(ip);
     CREATE INDEX identities_value ON identities(kind, value);",
    // 3: monitored services (HTTP / TCP / DNS / ICMP), hourly results, incidents
    "CREATE TABLE services (
       id INTEGER PRIMARY KEY, name TEXT NOT NULL, device INTEGER REFERENCES devices(id) ON DELETE SET NULL,
       spec TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1);
     CREATE TABLE service_hours (
       service INTEGER NOT NULL REFERENCES services(id) ON DELETE CASCADE, hour INTEGER NOT NULL,
       checks INTEGER NOT NULL DEFAULT 0, ups INTEGER NOT NULL DEFAULT 0,
       latency_sum REAL NOT NULL DEFAULT 0, latency_n INTEGER NOT NULL DEFAULT 0,
       PRIMARY KEY (service, hour));
     CREATE TABLE service_incidents (
       id INTEGER PRIMARY KEY, service INTEGER NOT NULL REFERENCES services(id) ON DELETE CASCADE,
       start INTEGER NOT NULL, end INTEGER, cause TEXT NOT NULL DEFAULT '');
     CREATE INDEX service_incidents_service ON service_incidents(service, start);",
];

/// One outage of a service: from `start` until `end` (None while still down).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Incident {
    pub id: i64,
    pub service: crate::services::ServiceId,
    pub start: i64,
    pub end: Option<i64>,
    pub cause: String,
}

pub fn latest_version() -> i64 {
    MIGRATIONS.len() as i64
}

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    /// The file is a database, but not a Nodo map.
    NotNodo,
    /// Made by a newer Nodo.
    TooNew(i64),
    /// `PRAGMA integrity_check` failed.
    Corrupt(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Sqlite(rusqlite::Error::SqliteFailure(c, _)) if c.code == rusqlite::ErrorCode::NotADatabase => write!(f, "{}", i18n::t!("core.db.not_database")),
            StoreError::Sqlite(e) => write!(f, "{}", i18n::t!("core.db.error", detail = e)),
            StoreError::NotNodo => write!(f, "{}", i18n::t!("core.db.not_nodo")),
            StoreError::TooNew(v) => write!(f, "{}", i18n::t!("core.db.too_new", version = v)),
            StoreError::Corrupt(d) => write!(f, "{}", i18n::t!("core.db.corrupt", detail = d)),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Sqlite(e)
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// The whole map in memory (maps are hundreds of devices, not millions). Ids are allocated
/// here and written as is.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MapData {
    pub networks: Vec<Network>,
    pub devices: Vec<Device>,
}

impl MapData {
    pub fn next_device_id(&self) -> DeviceId {
        self.devices.iter().map(|d| d.id).max().unwrap_or(0) + 1
    }

    pub fn next_network_id(&self) -> NetworkId {
        self.networks.iter().map(|n| n.id).max().unwrap_or(0) + 1
    }

    pub fn device(&self, id: DeviceId) -> Option<&Device> {
        self.devices.iter().find(|d| d.id == id)
    }

    pub fn device_mut(&mut self, id: DeviceId) -> Option<&mut Device> {
        self.devices.iter_mut().find(|d| d.id == id)
    }

    pub fn network(&self, id: NetworkId) -> Option<&Network> {
        self.networks.iter().find(|n| n.id == id)
    }

    pub fn network_by_key(&self, key: &str) -> Option<&Network> {
        self.networks.iter().find(|n| n.key == key)
    }

    /// The most specific network of `layer` whose CIDR holds `ip`.
    pub fn network_for(&self, ip: IpAddr, layer: Layer) -> Option<NetworkId> {
        self.networks.iter().filter(|n| n.layer == layer).filter_map(|n| Some((crate::cidr::Cidr::parse(n.cidr.as_deref()?)?, n.id))).filter(|(c, _)| c.contains(ip)).max_by_key(|(c, _)| c.prefix).map(|(_, id)| id)
    }

    /// Whether `d` is the gateway of one of its networks.
    pub fn is_gateway(&self, d: &Device) -> bool {
        d.addrs.iter().any(|a| self.networks.iter().any(|n| n.gateway == Some(a.ip)))
    }
}

pub struct Store {
    conn: Connection,
    path: PathBuf,
}

fn to_json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".into())
}

fn from_json<T: serde::de::DeserializeOwned + Default>(s: &str) -> T {
    serde_json::from_str(s).unwrap_or_default()
}

impl Store {
    /// A new, empty map at `path` (an existing file there is replaced: the save dialog has
    /// already asked).
    pub fn create(path: &Path) -> Result<Store> {
        if path.exists() {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(journal(path));
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "application_id", APPLICATION_ID)?;
        let mut s = Store { conn, path: path.to_path_buf() };
        s.configure()?;
        s.migrate()?;
        s.set_meta("created", &crate::time::now().to_string())?;
        Ok(s)
    }

    /// Open an existing map: integrity check, application id, migrations.
    pub fn open(path: &Path) -> Result<Store> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_URI | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        let check: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if check != "ok" {
            return Err(StoreError::Corrupt(check));
        }
        let app: i32 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if app != APPLICATION_ID {
            return Err(StoreError::NotNodo);
        }
        if version > latest_version() {
            return Err(StoreError::TooNew(version));
        }
        let mut s = Store { conn, path: path.to_path_buf() };
        s.configure()?;
        s.migrate()?;
        Ok(s)
    }

    /// Read-only, for the CLI's `list` / `show` (no migration, no lock beyond reading).
    pub fn open_read_only(path: &Path) -> Result<Store> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        let app: i32 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        if app != APPLICATION_ID {
            return Err(StoreError::NotNodo);
        }
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > latest_version() {
            return Err(StoreError::TooNew(version));
        }
        Ok(Store { conn, path: path.to_path_buf() })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn configure(&mut self) -> Result<()> {
        // a single file (plus a short-lived -journal) plays well with Dropbox / iCloud / Syncthing
        self.conn.pragma_update(None, "journal_mode", "DELETE")?;
        self.conn.pragma_update(None, "foreign_keys", "ON")?;
        self.conn.pragma_update(None, "synchronous", "FULL")?;
        self.conn.busy_timeout(std::time::Duration::from_secs(3))?;
        Ok(())
    }

    pub fn version(&self) -> Result<i64> {
        Ok(self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    fn migrate(&mut self) -> Result<()> {
        let from = self.version()?;
        for (i, step) in MIGRATIONS.iter().enumerate().skip(from.max(0) as usize) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(step)?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
        }
        Ok(())
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set_meta(&mut self, key: &str, value: &str) -> Result<()> {
        self.conn.execute("INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![key, value])?;
        Ok(())
    }

    pub fn load(&self) -> Result<MapData> {
        let mut networks = Vec::new();
        let mut st = self.conn.prepare("SELECT id, key, name, layer, cidr, gateway, parent, facts FROM networks ORDER BY id")?;
        let rows = st.query_map([], |r| {
            Ok(Network {
                id: r.get(0)?,
                key: r.get(1)?,
                name: r.get(2)?,
                layer: Layer::from_id(&r.get::<_, String>(3)?).unwrap_or(Layer::Lan),
                cidr: r.get(4)?,
                gateway: r.get::<_, Option<String>>(5)?.and_then(|g| g.parse().ok()),
                parent: r.get(6)?,
                facts: from_json(&r.get::<_, String>(7)?),
            })
        })?;
        for n in rows {
            networks.push(n?);
        }
        let mut devices = Vec::new();
        let mut st = self.conn.prepare("SELECT id, name, name_manual, kind, kind_manual, owner, location, notes, tags, links, fields, saved, monitor, vendor, hostname, os, model, services, facts, first_seen, last_seen, online FROM devices ORDER BY id")?;
        let rows = st.query_map([], |r| {
            Ok(Device {
                id: r.get(0)?,
                name: r.get(1)?,
                name_manual: r.get(2)?,
                kind: DeviceKind::from_id(&r.get::<_, String>(3)?),
                kind_manual: r.get(4)?,
                owner: r.get(5)?,
                location: r.get(6)?,
                notes: r.get(7)?,
                tags: from_json(&r.get::<_, String>(8)?),
                links: from_json(&r.get::<_, String>(9)?),
                fields: from_json(&r.get::<_, String>(10)?),
                saved: r.get(11)?,
                monitor: r.get(12)?,
                vendor: r.get(13)?,
                hostname: r.get(14)?,
                os: r.get(15)?,
                model: r.get(16)?,
                services: from_json(&r.get::<_, String>(17)?),
                facts: from_json(&r.get::<_, String>(18)?),
                first_seen: r.get(19)?,
                last_seen: r.get(20)?,
                online: r.get(21)?,
                addrs: Vec::new(),
                idents: Vec::new(),
            })
        })?;
        for d in rows {
            devices.push(d?);
        }
        let mut st = self.conn.prepare("SELECT device, ip, layer, network, source, last_seen FROM addresses ORDER BY device, layer, ip")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<i64>>(3)?, r.get::<_, String>(4)?, r.get::<_, i64>(5)?)))?;
        for row in rows {
            let (dev, ip, layer, network, source, last_seen) = row?;
            if let (Some(d), Ok(ip)) = (devices.iter_mut().find(|d| d.id == dev), ip.parse()) {
                d.addrs.push(Addr { ip, layer: Layer::from_id(&layer).unwrap_or(Layer::Lan), network, source: Source::from_id(&source), last_seen });
            }
        }
        let mut st = self.conn.prepare("SELECT device, kind, value FROM identities ORDER BY device, kind, value")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
        for row in rows {
            let (dev, kind, value) = row?;
            if let (Some(d), Some(kind)) = (devices.iter_mut().find(|d| d.id == dev), IdentKind::from_id(&kind)) {
                d.idents.push(Ident { kind, value });
            }
        }
        Ok(MapData { networks, devices })
    }

    pub fn save_network(&mut self, n: &Network) -> Result<()> {
        self.conn.execute(
            "INSERT INTO networks (id, key, name, layer, cidr, gateway, parent, facts) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET key = excluded.key, name = excluded.name, layer = excluded.layer, cidr = excluded.cidr,
               gateway = excluded.gateway, parent = excluded.parent, facts = excluded.facts",
            params![n.id, n.key, n.name, n.layer.id(), n.cidr, n.gateway.map(|g| g.to_string()), n.parent, to_json(&n.facts)],
        )?;
        Ok(())
    }

    pub fn delete_network(&mut self, id: NetworkId) -> Result<()> {
        self.conn.execute("DELETE FROM networks WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Insert or replace one device with its addresses and identities, atomically.
    pub fn save_device(&mut self, d: &Device) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO devices (id, name, name_manual, kind, kind_manual, owner, location, notes, tags, links, fields, saved, monitor, vendor, hostname, os, model, services, facts, first_seen, last_seen, online)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, name_manual = excluded.name_manual, kind = excluded.kind, kind_manual = excluded.kind_manual,
               owner = excluded.owner, location = excluded.location, notes = excluded.notes, tags = excluded.tags, links = excluded.links, fields = excluded.fields,
               saved = excluded.saved, monitor = excluded.monitor, vendor = excluded.vendor, hostname = excluded.hostname, os = excluded.os, model = excluded.model,
               services = excluded.services, facts = excluded.facts, first_seen = excluded.first_seen, last_seen = excluded.last_seen, online = excluded.online",
            params![
                d.id,
                d.name,
                d.name_manual,
                d.kind.id(),
                d.kind_manual,
                d.owner,
                d.location,
                d.notes,
                to_json(&d.tags),
                to_json(&d.links),
                to_json(&d.fields),
                d.saved,
                d.monitor,
                d.vendor,
                d.hostname,
                d.os,
                d.model,
                to_json(&d.services),
                to_json(&d.facts),
                d.first_seen,
                d.last_seen,
                d.online
            ],
        )?;
        tx.execute("DELETE FROM addresses WHERE device = ?1", [d.id])?;
        for a in &d.addrs {
            tx.execute("INSERT OR REPLACE INTO addresses (device, ip, layer, network, source, last_seen) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![d.id, a.ip.to_string(), a.layer.id(), a.network, a.source.id(), a.last_seen])?;
        }
        tx.execute("DELETE FROM identities WHERE device = ?1", [d.id])?;
        for i in &d.idents {
            tx.execute("INSERT OR IGNORE INTO identities (device, kind, value) VALUES (?1, ?2, ?3)", params![d.id, i.kind.id(), i.value])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_device(&mut self, id: DeviceId) -> Result<()> {
        self.conn.execute("DELETE FROM devices WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Move a device's history onto another one (after a merge).
    pub fn move_events(&mut self, from: DeviceId, to: DeviceId) -> Result<()> {
        self.conn.execute("UPDATE events SET device = ?2 WHERE device = ?1", params![from, to])?;
        Ok(())
    }

    pub fn add_event(&mut self, device: Option<DeviceId>, at: i64, kind: EventKind, detail: &str) -> Result<i64> {
        self.conn.execute("INSERT INTO events (device, at, kind, detail) VALUES (?1, ?2, ?3, ?4)", params![device, at, kind.id(), detail])?;
        Ok(self.conn.last_insert_rowid())
    }

    /// A device's history (or the whole map's with `None`), newest first.
    pub fn events(&self, device: Option<DeviceId>, limit: usize) -> Result<Vec<Event>> {
        let sql = if device.is_some() { "SELECT id, device, at, kind, detail FROM events WHERE device = ?1 ORDER BY at DESC, id DESC LIMIT ?2" } else { "SELECT id, device, at, kind, detail FROM events WHERE ?1 IS NULL ORDER BY at DESC, id DESC LIMIT ?2" };
        let mut st = self.conn.prepare(sql)?;
        let rows = st.query_map(params![device, limit as i64], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, device, at, kind, detail) = row?;
            if let Some(kind) = EventKind::from_id(&kind) {
                out.push(Event { id, device, at, kind, detail });
            }
        }
        Ok(out)
    }

    pub fn services(&self) -> Result<Vec<crate::services::ServiceCheck>> {
        let mut st = self.conn.prepare("SELECT id, name, device, spec, enabled FROM services ORDER BY name COLLATE NOCASE, id")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, String>(3)?, r.get::<_, bool>(4)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, name, device, spec, enabled) = row?;
            if let Ok(spec) = serde_json::from_str(&spec) {
                out.push(crate::services::ServiceCheck { id, name, device, spec, enabled });
            }
        }
        Ok(out)
    }

    /// Insert (`id` 0) or update; returns the id.
    pub fn save_service(&mut self, s: &crate::services::ServiceCheck) -> Result<i64> {
        let spec = serde_json::to_string(&s.spec).unwrap_or_default();
        if s.id == 0 {
            self.conn.execute("INSERT INTO services (name, device, spec, enabled) VALUES (?1, ?2, ?3, ?4)", params![s.name, s.device, spec, s.enabled])?;
            return Ok(self.conn.last_insert_rowid());
        }
        self.conn.execute("INSERT INTO services (id, name, device, spec, enabled) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(id) DO UPDATE SET name = ?2, device = ?3, spec = ?4, enabled = ?5", params![s.id, s.name, s.device, spec, s.enabled])?;
        Ok(s.id)
    }

    pub fn delete_service(&mut self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM services WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Add a batch of checks to the hour buckets (one small transaction).
    pub fn add_service_hours(&mut self, rows: &[(i64, crate::services::Hour)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for (svc, h) in rows {
            tx.execute(
                "INSERT INTO service_hours (service, hour, checks, ups, latency_sum, latency_n) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(service, hour) DO UPDATE SET checks = checks + ?3, ups = ups + ?4, latency_sum = latency_sum + ?5, latency_n = latency_n + ?6",
                params![svc, h.hour, h.checks, h.ups, h.latency_sum, h.latency_n],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn service_hours(&self, service: i64, since: i64) -> Result<Vec<crate::services::Hour>> {
        let mut st = self.conn.prepare("SELECT hour, checks, ups, latency_sum, latency_n FROM service_hours WHERE service = ?1 AND hour >= ?2 ORDER BY hour")?;
        let rows = st.query_map(params![service, since], |r| Ok(crate::services::Hour { hour: r.get(0)?, checks: r.get(1)?, ups: r.get(2)?, latency_sum: r.get(3)?, latency_n: r.get(4)? }))?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn open_incident(&mut self, service: i64, start: i64, cause: &str) -> Result<i64> {
        self.conn.execute("INSERT INTO service_incidents (service, start, cause) VALUES (?1, ?2, ?3)", params![service, start, cause])?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Close the service's open incident, if any.
    pub fn close_incident(&mut self, service: i64, end: i64) -> Result<()> {
        self.conn.execute("UPDATE service_incidents SET end = ?2 WHERE service = ?1 AND end IS NULL", params![service, end])?;
        Ok(())
    }

    /// Newest first; all services with `None`.
    pub fn incidents(&self, service: Option<i64>, limit: usize) -> Result<Vec<Incident>> {
        let mut st = self.conn.prepare("SELECT id, service, start, end, cause FROM service_incidents WHERE (?1 IS NULL OR service = ?1) ORDER BY start DESC, id DESC LIMIT ?2")?;
        let rows = st.query_map(params![service, limit as i64], |r| Ok(Incident { id: r.get(0)?, service: r.get(1)?, start: r.get(2)?, end: r.get(3)?, cause: r.get(4)? }))?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Up/down transitions of a device since `since`, oldest first (for uptime).
    pub fn transitions(&self, device: DeviceId, since: i64) -> Result<Vec<(i64, bool)>> {
        let mut st = self.conn.prepare("SELECT at, kind FROM events WHERE device = ?1 AND at >= ?2 AND kind IN ('up', 'down') ORDER BY at, id")?;
        let rows = st.query_map(params![device, since], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)? == "up")))?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }
}

fn journal(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push("-journal");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: DeviceId) -> Device {
        Device {
            id,
            name: "NAS".into(),
            kind: DeviceKind::Nas,
            tags: vec!["home".into()],
            links: vec![Link { label: "DSM".into(), url: "https://192.168.1.10:5001".into() }],
            fields: vec![("Rack".into(), "A2".into())],
            saved: true,
            monitor: true,
            vendor: Some("Synology Incorporated".into()),
            addrs: vec![Addr { ip: "192.168.1.10".parse().unwrap(), layer: Layer::Lan, network: Some(1), source: Source::Arp, last_seen: 5 }],
            idents: vec![Ident::new(IdentKind::Mac, "00:11:32:aa:bb:cc")],
            services: vec![Service { name: "_smb._tcp".into(), port: Some(445), label: None }],
            facts: vec![("web_ui".into(), "http://192.168.1.10".into())],
            first_seen: 1,
            last_seen: 5,
            online: Some(true),
            ..Default::default()
        }
    }

    #[test]
    fn create_save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("home.nodo");
        let mut s = Store::create(&path).unwrap();
        assert_eq!(s.version().unwrap(), latest_version());
        let net = Network { id: 1, name: "Home".into(), layer: Layer::Lan, cidr: Some("192.168.1.0/24".into()), gateway: Some("192.168.1.1".parse().unwrap()), parent: None, key: "lan:192.168.1.0/24".into(), facts: vec![] };
        s.save_network(&net).unwrap();
        let d = device(1);
        s.save_device(&d).unwrap();
        s.add_event(Some(1), 10, EventKind::Down, "").unwrap();
        s.add_event(Some(1), 20, EventKind::Up, "3 ms").unwrap();
        s.set_meta("down_minutes", "5").unwrap();
        drop(s);
        // journal_mode=DELETE: nothing but the file itself is left behind
        let names: Vec<String> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["home.nodo"]);
        let s = Store::open(&path).unwrap();
        let m = s.load().unwrap();
        assert_eq!(m.networks, vec![net]);
        assert_eq!(m.devices, vec![d]);
        let ev = s.events(Some(1), 10).unwrap();
        assert_eq!(ev[0].kind, EventKind::Up);
        assert_eq!(ev[0].detail, "3 ms");
        assert_eq!(s.transitions(1, 0).unwrap(), vec![(10, false), (20, true)]);
        assert_eq!(s.meta("down_minutes").unwrap().as_deref(), Some("5"));
        assert_eq!(m.network_for("192.168.1.77".parse().unwrap(), Layer::Lan), Some(1));
        assert!(m.is_gateway(&Device { addrs: vec![Addr { ip: "192.168.1.1".parse().unwrap(), layer: Layer::Lan, network: None, source: Source::Arp, last_seen: 0 }], ..Default::default() }));
    }

    #[test]
    fn migrates_old_files_and_refuses_foreign_ones() {
        let dir = tempfile::tempdir().unwrap();
        // a version-1 file with one device, as the first release wrote it
        let p = dir.path().join("v1.nodo");
        {
            let c = Connection::open(&p).unwrap();
            c.pragma_update(None, "application_id", APPLICATION_ID).unwrap();
            c.execute_batch(MIGRATIONS[0]).unwrap();
            c.pragma_update(None, "user_version", 1).unwrap();
            c.execute("INSERT INTO devices (id, name) VALUES (7, 'old printer')", []).unwrap();
        }
        let s = Store::open(&p).unwrap();
        assert_eq!(s.version().unwrap(), latest_version());
        let has_index: i64 = s.conn.query_row("SELECT count(*) FROM sqlite_master WHERE name = 'addresses_ip'", [], |r| r.get(0)).unwrap();
        assert_eq!(has_index, 1);
        assert_eq!(s.load().unwrap().devices[0].name, "old printer");
        drop(s);
        // from a newer Nodo
        let p2 = dir.path().join("future.nodo");
        {
            let c = Connection::open(&p2).unwrap();
            c.pragma_update(None, "application_id", APPLICATION_ID).unwrap();
            c.pragma_update(None, "user_version", 99).unwrap();
        }
        assert!(matches!(Store::open(&p2), Err(StoreError::TooNew(99))));
        // some other SQLite database
        let p3 = dir.path().join("other.db");
        Connection::open(&p3).unwrap().execute_batch("CREATE TABLE t (x)").unwrap();
        assert!(matches!(Store::open(&p3), Err(StoreError::NotNodo)));
        // not a database at all
        let p4 = dir.path().join("notes.txt");
        std::fs::write(&p4, "hello, this is not sqlite at all, just some text that is long enough").unwrap();
        assert!(Store::open(&p4).is_err());
        // version 3: services, hourly buckets (added up), incidents
        let mut s = Store::open(&p).unwrap();
        let mut svc = crate::services::ServiceCheck { id: 0, name: "Site".into(), device: Some(7), spec: crate::services::Spec::new(crate::services::CheckKind::Http, "https://example.com"), enabled: true };
        svc.id = s.save_service(&svc).unwrap();
        svc.name = "Website".into();
        assert_eq!(s.save_service(&svc).unwrap(), svc.id);
        assert_eq!(s.services().unwrap(), vec![svc.clone()]);
        let h = crate::services::Hour { hour: 3600, checks: 2, ups: 1, latency_sum: 40., latency_n: 1 };
        s.add_service_hours(&[(svc.id, h), (svc.id, h)]).unwrap();
        assert_eq!(s.service_hours(svc.id, 0).unwrap(), vec![crate::services::Hour { hour: 3600, checks: 4, ups: 2, latency_sum: 80., latency_n: 2 }]);
        s.open_incident(svc.id, 100, "HTTP 503").unwrap();
        s.close_incident(svc.id, 160).unwrap();
        let inc = s.incidents(None, 10).unwrap();
        assert_eq!((inc[0].start, inc[0].end, inc[0].cause.as_str()), (100, Some(160), "HTTP 503"));
        s.delete_device(7).unwrap();
        assert_eq!(s.services().unwrap()[0].device, None, "the service outlives its device");
        s.delete_service(svc.id).unwrap();
        assert!(s.incidents(Some(svc.id), 10).unwrap().is_empty() && s.service_hours(svc.id, 0).unwrap().is_empty());
        drop(s);
        // creating over an existing file starts empty
        let s = Store::create(&p).unwrap();
        assert!(s.load().unwrap().devices.is_empty());
    }
}
