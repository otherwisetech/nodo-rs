//! The terminal panel's state, kept free of gpui so it can be tested: where it is docked, how big,
//! whether it is shown, collapsed or maximized, and the tab strip.
use crate::settings::Settings;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dock {
    #[default]
    Bottom,
    Right,
}

impl Dock {
    pub fn id(self) -> &'static str {
        match self {
            Dock::Bottom => "bottom",
            Dock::Right => "right",
        }
    }

    pub fn parse(s: &str) -> Dock {
        if s.trim().eq_ignore_ascii_case("right") { Dock::Right } else { Dock::Bottom }
    }
}

/// Which terminal the device actions use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Prefer {
    #[default]
    Embedded,
    External,
}

impl Prefer {
    pub fn parse(s: Option<&str>) -> Prefer {
        if s == Some("external") { Prefer::External } else { Prefer::Embedded }
    }

    pub fn id(self) -> Option<String> {
        (self == Prefer::External).then(|| "external".into()) // i18n-ok: setting value
    }
}

/// What the window shows of the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    Hidden,
    /// One slim line: tab count and the active tab's last line.
    Bar,
    Docked(Dock),
    Maximized,
}

pub const BOTTOM_MIN: f32 = 140.;
pub const RIGHT_MIN: f32 = 320.;
/// What the map keeps at least, beside or above the panel.
pub const MAP_MIN: f32 = 220.;

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub dock: Dock,
    pub bottom: f32,
    pub right: f32,
    pub open: bool,
    pub collapsed: bool,
    pub maximized: bool,
}

impl Default for Layout {
    fn default() -> Self {
        Layout { dock: Dock::Bottom, bottom: 300., right: 560., open: false, collapsed: false, maximized: false }
    }
}

impl Layout {
    pub fn from_settings(s: &Settings) -> Layout {
        let d = Layout::default();
        let num = |v: &Option<String>, def: f32| v.as_deref().and_then(|v| v.parse::<f32>().ok()).filter(|v| v.is_finite() && *v > 0.).unwrap_or(def);
        Layout { dock: s.term_dock.as_deref().map(Dock::parse).unwrap_or_default(), bottom: num(&s.term_bottom, d.bottom), right: num(&s.term_right, d.right), collapsed: s.term_collapsed.as_deref() == Some("on"), ..d }
    }

    /// The remembered parts (not `open` / `maximized`: those are per session).
    pub fn store(&self, s: &mut Settings) {
        s.term_dock = Some(self.dock.id().into());
        s.term_bottom = Some(format!("{:.0}", self.bottom));
        s.term_right = Some(format!("{:.0}", self.right));
        s.term_collapsed = self.collapsed.then(|| "on".into()); // i18n-ok: setting value
    }

    pub fn shown(&self, tabs: usize) -> Shown {
        if !self.open || tabs == 0 {
            Shown::Hidden
        } else if self.maximized {
            Shown::Maximized
        } else if self.collapsed {
            Shown::Bar
        } else {
            Shown::Docked(self.dock)
        }
    }

    /// The docked panel's size along its axis, fitted to the window's `total` along it.
    pub fn size(&self, total: f32) -> f32 {
        match self.dock {
            Dock::Bottom => clamp(self.bottom, BOTTOM_MIN, total),
            Dock::Right => clamp(self.right, RIGHT_MIN, total),
        }
    }

    /// A splitter drag: `size` wanted, `total` available. Returns whether it changed.
    pub fn resize(&mut self, size: f32, total: f32) -> bool {
        let (slot, min) = match self.dock {
            Dock::Bottom => (&mut self.bottom, BOTTOM_MIN),
            Dock::Right => (&mut self.right, RIGHT_MIN),
        };
        let v = clamp(size, min, total).round();
        let changed = (*slot - v).abs() >= 1.;
        *slot = v;
        changed
    }

    pub fn set_dock(&mut self, dock: Dock) {
        self.dock = dock;
        self.collapsed = false;
        self.maximized = false;
        self.open = true;
    }

    /// ⌘J: hide, or show (expanded) when hidden or collapsed.
    pub fn toggle(&mut self) {
        if self.open && !self.collapsed {
            self.open = false;
            self.maximized = false;
        } else {
            self.open = true;
            self.collapsed = false;
        }
    }

    pub fn toggle_collapsed(&mut self) {
        self.collapsed = !self.collapsed;
        self.maximized = false;
        self.open = true;
    }

    pub fn toggle_maximized(&mut self) {
        self.maximized = !self.maximized;
        self.collapsed = false;
        self.open = true;
    }

    /// A tab was opened: show it.
    pub fn reveal(&mut self) {
        self.open = true;
        self.collapsed = false;
    }
}

fn clamp(v: f32, min: f32, total: f32) -> f32 {
    let max = (total - MAP_MIN).max(min);
    v.clamp(min, max)
}

/// An ordered strip with one active entry.
#[derive(Debug, Clone)]
pub struct Tabs<T> {
    pub items: Vec<T>,
    pub active: usize,
}

impl<T> Default for Tabs<T> {
    fn default() -> Self {
        Tabs { items: Vec::new(), active: 0 }
    }
}

impl<T> Tabs<T> {
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Added after the active tab, and made active.
    pub fn push(&mut self, t: T) -> usize {
        let at = if self.items.is_empty() { 0 } else { self.active + 1 };
        self.items.insert(at, t);
        self.active = at;
        at
    }

    pub fn active(&self) -> Option<&T> {
        self.items.get(self.active)
    }

    pub fn select(&mut self, ix: usize) {
        if ix < self.items.len() {
            self.active = ix;
        }
    }

    /// The tab to the right takes over, or the one to the left when it was the last.
    pub fn close(&mut self, ix: usize) -> Option<T> {
        if ix >= self.items.len() {
            return None;
        }
        let t = self.items.remove(ix);
        if ix < self.active || (ix == self.active && self.active >= self.items.len()) {
            self.active = self.active.saturating_sub(1);
        }
        Some(t)
    }

    pub fn next(&mut self, step: isize) {
        let n = self.items.len() as isize;
        if n > 0 {
            self.active = (self.active as isize + step).rem_euclid(n) as usize;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BOTTOM_MIN, Dock, Layout, MAP_MIN, Prefer, RIGHT_MIN, Shown, Tabs};
    use crate::settings::Settings;

    #[test]
    fn tabs_open_close_and_cycle() {
        let mut t: Tabs<&str> = Tabs::default();
        assert!(t.active().is_none());
        t.push("a");
        t.push("b");
        t.push("c");
        assert_eq!((t.items.clone(), t.active), (vec!["a", "b", "c"], 2));
        t.select(0);
        t.push("d");
        assert_eq!(t.items, vec!["a", "d", "b", "c"], "a new tab opens next to the active one");
        assert_eq!(t.active(), Some(&"d"));
        assert_eq!(t.close(1), Some("d"));
        assert_eq!(t.active(), Some(&"b"), "the tab to the right takes over");
        t.select(2);
        t.close(2);
        assert_eq!(t.active(), Some(&"b"), "closing the last one moves left");
        t.push("e");
        t.close(0);
        assert_eq!((t.items.clone(), t.active()), (vec!["b", "e"], Some(&"e")), "closing one before the active keeps it active");
        t.next(1);
        assert_eq!(t.active(), Some(&"b"), "wraps");
        t.next(-1);
        assert_eq!(t.active(), Some(&"e"));
        assert_eq!(t.close(9), None);
        t.close(0);
        t.close(0);
        assert!(t.is_empty() && t.active().is_none());
        t.next(1);
    }

    #[test]
    fn shown_follows_open_collapse_maximize() {
        let mut l = Layout::default();
        assert_eq!(l.shown(1), Shown::Hidden);
        l.reveal();
        assert_eq!(l.shown(0), Shown::Hidden, "no tabs: nothing to show");
        assert_eq!(l.shown(1), Shown::Docked(Dock::Bottom));
        l.toggle_collapsed();
        assert_eq!(l.shown(1), Shown::Bar);
        l.toggle();
        assert_eq!(l.shown(1), Shown::Docked(Dock::Bottom), "⌘J on the bar expands");
        l.toggle_maximized();
        assert_eq!(l.shown(1), Shown::Maximized);
        l.toggle();
        assert_eq!(l.shown(1), Shown::Hidden);
        l.toggle();
        assert_eq!(l.shown(1), Shown::Docked(Dock::Bottom), "shown again, not maximized");
        l.set_dock(Dock::Right);
        assert_eq!(l.shown(2), Shown::Docked(Dock::Right));
    }

    #[test]
    fn sizes_clamp_and_persist() {
        let mut l = Layout::default();
        assert!(l.resize(5000., 900.));
        assert_eq!(l.bottom, 900. - MAP_MIN);
        assert!(l.resize(10., 900.));
        assert_eq!(l.bottom, BOTTOM_MIN);
        assert!(!l.resize(10.4, 900.), "under a pixel is not a change");
        l.set_dock(Dock::Right);
        l.resize(700., 1400.);
        assert_eq!(l.size(1400.), 700.);
        assert_eq!(l.size(400.), RIGHT_MIN, "a narrow window still gets the minimum");
        l.collapsed = true;
        let mut s = Settings::default();
        l.store(&mut s);
        let back = Layout::from_settings(&s);
        assert_eq!((back.dock, back.bottom, back.right, back.collapsed, back.open), (Dock::Right, BOTTOM_MIN, 700., true, false));
        let junk = Settings { term_dock: Some("sideways".into()), term_bottom: Some("NaN".into()), term_right: Some("-4".into()), ..Default::default() };
        assert_eq!(Layout::from_settings(&junk), Layout::default());
        assert_eq!(Prefer::parse(Some("external")), Prefer::External);
        assert_eq!(Prefer::parse(None), Prefer::Embedded);
        assert_eq!(Prefer::External.id().as_deref(), Some("external"));
    }
}
