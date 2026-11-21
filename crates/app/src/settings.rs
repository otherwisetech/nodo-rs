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
    /// Embedded terminal panel: `bottom` / `right`, sizes in px, `on` when collapsed to a bar.
    pub term_dock: Option<String>,
    pub term_bottom: Option<String>,
    pub term_right: Option<String>,
    pub term_collapsed: Option<String>,
    /// Font size in pt.
    pub term_font: Option<String>,
    /// `external`: device actions open the terminal app instead of a tab.
    pub term_prefer: Option<String>,
    /// `on`: listen for files from other Nodos (off by default).
    pub share_receive: Option<String>,
    /// `on`: accept from paired devices without asking.
    pub share_auto: Option<String>,
    /// Where received files go (default `~/Downloads/Nodo`).
    pub share_dir: Option<String>,
    /// Largest file accepted, in MB.
    pub share_max_mb: Option<String>,
    /// `"off"`: don't publish alerts to Otherwise Apps' Today view.
    pub today_feed: Option<String>,
    /// The executable `otherwise-nodo://` was last registered for (Windows / Linux).
    pub link_registered: Option<String>,
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
            term_dock: st("term_dock"),
            term_bottom: st("term_bottom"),
            term_right: st("term_right"),
            term_collapsed: st("term_collapsed"),
            term_font: st("term_font"),
            term_prefer: st("term_prefer"),
            share_receive: st("share_receive"),
            share_auto: st("share_auto"),
            share_dir: st("share_dir"),
            share_max_mb: st("share_max_mb"),
            today_feed: st("today_feed"),
            link_registered: st("link_registered"),
        }
    }

    fn to_json(&self) -> String {
        let strings = [("language", &self.language), ("view", &self.view), ("layout", &self.layout), ("terminal", &self.terminal), ("github_stars", &self.github_stars), ("notify", &self.notify), ("auto_refresh", &self.auto_refresh), ("term_dock", &self.term_dock), ("term_bottom", &self.term_bottom), ("term_right", &self.term_right), ("term_collapsed", &self.term_collapsed), ("term_font", &self.term_font), ("term_prefer", &self.term_prefer), ("share_receive", &self.share_receive), ("share_auto", &self.share_auto), ("share_dir", &self.share_dir), ("share_max_mb", &self.share_max_mb), ("today_feed", &self.today_feed), ("link_registered", &self.link_registered)];
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

    pub fn today_feed(&self) -> bool {
        self.today_feed.as_deref() != Some("off")
    }

    pub fn auto_refresh(&self) -> bool {
        self.auto_refresh.as_deref() != Some("off")
    }

    pub fn share_receive(&self) -> bool {
        self.share_receive.as_deref() == Some("on")
    }

    pub fn share_auto(&self) -> bool {
        self.share_auto.as_deref() == Some("on")
    }

    pub fn share_dir(&self) -> std::path::PathBuf {
        match self.share_dir.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
            Some(d) => std::path::PathBuf::from(d),
            None => {
                let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(std::path::PathBuf::from).unwrap_or_default();
                home.join("Downloads").join("Nodo")
            }
        }
    }

    pub fn share_max_bytes(&self) -> u64 {
        self.share_max_mb.as_deref().and_then(|v| v.trim().parse::<u64>().ok()).filter(|v| *v > 0).map(|mb| mb * 1024 * 1024).unwrap_or(nodo_core::share::DEFAULT_MAX)
    }

    pub fn term_font(&self) -> f32 {
        self.term_font.as_deref().and_then(|v| v.parse::<f32>().ok()).filter(|v| v.is_finite()).unwrap_or(crate::terminal::FONT_DEFAULT).clamp(crate::terminal::FONT_MIN, crate::terminal::FONT_MAX)
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

/// The system's languages in order of preference: on macOS the Language & Region list
/// (`AppleLanguages`, what Finder-launched apps should follow; `LANG` is unset there and
/// `AppleLocale` is the region format, not the language), then `LC_ALL` / `LC_MESSAGES` /
/// `LANG`, then `AppleLocale`.
fn system_locales() -> Vec<String> {
    let mut out = Vec::new();
    let defaults = |key: &str| std::process::Command::new("defaults").args(["read", "-g", key]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    if cfg!(target_os = "macos") {
        out.extend(defaults("AppleLanguages").map(|s| parse_apple_languages(&s)).unwrap_or_default());
    }
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(var)
            && !v.is_empty()
            && v != "C"
            && v != "POSIX"
        {
            out.push(v);
        }
    }
    if cfg!(target_os = "macos")
        && let Some(v) = defaults("AppleLocale").map(|s| s.trim().to_string()).filter(|v| !v.is_empty())
    {
        out.push(v);
    }
    out
}

/// `defaults read -g AppleLanguages` → `["en-CO", "es-419"]`.
pub fn parse_apple_languages(s: &str) -> Vec<String> {
    s.trim().trim_start_matches('(').trim_end_matches(')').split([',', '\n']).map(|x| x.trim().trim_matches('"').trim().to_string()).filter(|x| !x.is_empty()).collect()
}

/// The first of `prefs` Nodo has a translation for.
pub fn pick_language(prefs: &[String]) -> Option<&'static str> {
    prefs.iter().find_map(|p| i18n::match_system(p))
}

/// Saved language, else the first bundled one of the system's preferred languages, else the
/// default.
pub fn initial_language(settings: &Settings) -> &'static str {
    let saved = settings.language.as_deref().and_then(|l| i18n::available().into_iter().map(|(c, _)| c).find(|c| *c == l));
    saved.or_else(|| pick_language(&system_locales())).unwrap_or(i18n::DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::Settings;

    #[test]
    fn system_language_order() {
        let l = super::parse_apple_languages("(\n    \"de-DE\",\n    \"es-419\",\n    en\n)\n");
        assert_eq!(l, ["de-DE", "es-419", "en"]);
        assert_eq!(super::pick_language(&l), Some("es"));
        assert_eq!(super::pick_language(&["en-CO".to_string(), "es-419".to_string()]), Some("en"));
        assert_eq!(super::pick_language(&["pt_BR.UTF-8".to_string()]), Some("pt"));
        assert_eq!(super::pick_language(&["ja-JP".to_string()]), None);
    }

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
            term_dock: Some("right".into()),
            term_bottom: Some("320".into()),
            term_right: Some("600".into()),
            term_collapsed: Some("on".into()),
            term_font: Some("14".into()),
            term_prefer: Some("external".into()),
            share_receive: Some("on".into()),
            share_auto: Some("on".into()),
            share_dir: Some("/tmp/inbox".into()),
            share_max_mb: Some("10".into()),
            today_feed: Some("off".into()),
            link_registered: Some("C:\\Nodo\\nodo-app.exe".into()),
        };
        assert_eq!(Settings::parse(&s.to_json()), s);
        assert!(s.share_receive() && !Settings::default().share_receive(), "receiving is opt-in");
        assert_eq!(s.share_max_bytes(), 10 * 1024 * 1024);
        assert_eq!(s.share_dir(), std::path::PathBuf::from("/tmp/inbox"));
        assert!(Settings::default().share_dir().ends_with("Downloads/Nodo"));
        assert!(!s.github_stars() && Settings::default().github_stars());
        assert!(!s.notify() && Settings::default().notify());
        assert!(!s.auto_refresh() && Settings::default().auto_refresh());
        assert!(!s.today_feed() && Settings::default().today_feed());
        assert_eq!(Settings::parse(&Settings::default().to_json()), Settings::default());
        assert_eq!(s.term_font(), 14.);
        assert_eq!(Settings { term_font: Some("200".into()), ..Default::default() }.term_font(), crate::terminal::FONT_MAX);
        assert_eq!(Settings::default().term_font(), crate::terminal::FONT_DEFAULT);
        assert_eq!(Settings::parse("garbage"), Settings::default());
        assert_eq!(Settings::parse("{\n  \"language\": \"fr\"\n}\n").language.as_deref(), Some("fr"));
    }
}
