//! Tiny app settings file (`settings.json` in Nodo's config dir, shared with the CLI): a flat
//! JSON object of strings and string lists, parsed by hand. Per-map settings (alert thresholds,
//! AWS profile) live in the map file itself.
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    pub language: Option<String>,
    /// `"list"` or the graph.
    pub view: Option<String>,
    /// `layout::Mode::id`: radial, subnet, network.
    pub layout: Option<String>,
    /// Layer ids hidden on the graph.
    pub hidden_layers: Vec<String>,
    /// Preferred terminal (`launch::Term::id`).
    pub terminal: Option<String>,
    /// Terminal + command pairs the user has already seen and approved once.
    pub launch_ok: Vec<String>,
    /// `"off"`: don't fetch the repo's star count from api.github.com.
    pub github_stars: Option<String>,
    /// `"off"`: alerts stay in the app, no system notification.
    pub notify: Option<String>,
    /// `"off"`: no passive refresh every few minutes while a map is open.
    pub auto_refresh: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Val {
    S(String),
    L(Vec<String>),
}

fn dir() -> Option<PathBuf> {
    nodo_core::paths::config_dir()
}

pub fn config_dir() -> Option<PathBuf> {
    dir()
}

fn file() -> Option<PathBuf> {
    dir().map(|d| d.join("settings.json"))
}

impl Settings {
    pub fn load() -> Settings {
        file().and_then(|f| std::fs::read_to_string(f).ok()).map(|s| Settings::parse(&s)).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(f) = file() else { return };
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(f, self.to_json());
    }

    /// Load, change, save: other windows may have saved in between.
    pub fn update(f: impl FnOnce(&mut Settings)) -> Settings {
        let mut s = Settings::load();
        f(&mut s);
        s.save();
        s
    }

    fn parse(s: &str) -> Settings {
        let m = parse_object(s).unwrap_or_default();
        let st = |k: &str| match m.get(k) {
            Some(Val::S(v)) if !v.is_empty() => Some(v.clone()),
            _ => None,
        };
        let list = |k: &str| match m.get(k) {
            Some(Val::L(v)) => v.clone(),
            _ => Vec::new(),
        };
        Settings {
            language: st("language"),
            view: st("view"),
            layout: st("layout"),
            hidden_layers: list("hidden_layers"),
            terminal: st("terminal"),
            launch_ok: list("launch_ok"),
            github_stars: st("github_stars"),
            notify: st("notify"),
            auto_refresh: st("auto_refresh"),
        }
    }

    fn to_json(&self) -> String {
        let strings = [("language", &self.language), ("view", &self.view), ("layout", &self.layout), ("terminal", &self.terminal), ("github_stars", &self.github_stars), ("notify", &self.notify), ("auto_refresh", &self.auto_refresh)];
        let lists = [("hidden_layers", &self.hidden_layers), ("launch_ok", &self.launch_ok)];
        let mut fields: Vec<String> = strings.iter().filter_map(|(k, v)| v.as_ref().map(|v| format!("  \"{k}\": {}", quote(v)))).collect();
        fields.extend(lists.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| format!("  \"{k}\": [{}]", v.iter().map(|x| quote(x)).collect::<Vec<_>>().join(", "))));
        if fields.is_empty() { "{}\n".into() } else { format!("{{\n{}\n}}\n", fields.join(",\n")) }
    }

    pub fn github_stars(&self) -> bool {
        self.github_stars.as_deref() != Some("off")
    }

    pub fn notify(&self) -> bool {
        self.notify.as_deref() != Some("off")
    }

    pub fn auto_refresh(&self) -> bool {
        self.auto_refresh.as_deref() != Some("off")
    }
}

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A flat object of strings and arrays of strings; anything else is skipped or fails.
fn parse_object(s: &str) -> Option<BTreeMap<String, Val>> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let ws = |i: &mut usize| {
        while *i < c.len() && c[*i].is_whitespace() {
            *i += 1;
        }
    };
    fn string(c: &[char], i: &mut usize) -> Option<String> {
        if c.get(*i) != Some(&'"') {
            return None;
        }
        *i += 1;
        let mut out = String::new();
        loop {
            let ch = *c.get(*i)?;
            *i += 1;
            match ch {
                '"' => return Some(out),
                '\\' => {
                    let e = *c.get(*i)?;
                    *i += 1;
                    match e {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        'u' => {
                            let hex: String = c.get(*i..*i + 4)?.iter().collect();
                            *i += 4;
                            out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                        }
                        other => out.push(other),
                    }
                }
                ch => out.push(ch),
            }
        }
    }
    let mut m = BTreeMap::new();
    ws(&mut i);
    if c.get(i) != Some(&'{') {
        return None;
    }
    i += 1;
    loop {
        ws(&mut i);
        match c.get(i) {
            Some('}') => return Some(m),
            Some(',') => {
                i += 1;
                continue;
            }
            _ => {}
        }
        let k = string(&c, &mut i)?;
        ws(&mut i);
        if c.get(i) != Some(&':') {
            return None;
        }
        i += 1;
        ws(&mut i);
        let v = if c.get(i) == Some(&'[') {
            i += 1;
            let mut l = Vec::new();
            loop {
                ws(&mut i);
                match c.get(i) {
                    Some(']') => {
                        i += 1;
                        break;
                    }
                    Some(',') => i += 1,
                    _ => l.push(string(&c, &mut i)?),
                }
            }
            Val::L(l)
        } else {
            Val::S(string(&c, &mut i)?)
        };
        m.insert(k, v);
    }
}

fn system_locale() -> Option<String> {
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(var)
            && !v.is_empty()
            && v != "C"
            && v != "POSIX"
        {
            return Some(v);
        }
    }
    if cfg!(target_os = "macos") {
        let out = std::process::Command::new("defaults").args(["read", "-g", "AppleLocale"]).output().ok()?;
        let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !v.is_empty() {
            return Some(v);
        }
    }
    None
}

/// Saved language, else the system one if bundled, else the default.
pub fn initial_language(settings: &Settings) -> &'static str {
    let saved = settings.language.as_deref().and_then(|l| i18n::available().into_iter().map(|(c, _)| c).find(|c| *c == l));
    saved.or_else(|| system_locale().as_deref().and_then(i18n::match_system)).unwrap_or(i18n::DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::Settings;

    #[test]
    fn roundtrip() {
        let s = Settings { language: Some("en".into()), ..Default::default() };
        assert_eq!(Settings::parse(&s.to_json()), s);
        let s = Settings {
            language: Some("pt".into()),
            view: Some("list".into()),
            layout: Some("subnet".into()),
            hidden_layers: vec!["aws".into(), "wireguard".into()],
            terminal: Some("ghostty".into()),
            launch_ok: vec!["ghostty|ssh 10.0.0.2".into(), "kitty|ssh \"x\"".into()],
            github_stars: Some("off".into()),
            notify: Some("off".into()),
            auto_refresh: Some("off".into()),
        };
        assert_eq!(Settings::parse(&s.to_json()), s);
        assert!(!s.github_stars() && Settings::default().github_stars());
        assert!(!s.notify() && Settings::default().notify());
        assert!(!s.auto_refresh() && Settings::default().auto_refresh());
        assert_eq!(Settings::parse(&Settings::default().to_json()), Settings::default());
        assert_eq!(Settings::parse("garbage"), Settings::default());
        assert_eq!(Settings::parse("{\n  \"language\": \"fr\"\n}\n").language.as_deref(), Some("fr"));
    }
}
