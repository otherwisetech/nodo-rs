//! The family's tokens come from otherwise-ui (github.com/otherwisetech/otherwise-ui, by tag):
//! Inter, the type scale, the warm neutrals and status colours, with Nodo's ocean-blue accent
//! (`otherwise_ui::accents::NODO`, set in main.rs). `Tokens` derefs to `otherwise_ui::Theme`
//! (so `&Tokens` passes wherever the crate wants `&Theme`) and adds the map's own colours: the
//! network layers, tags and "up".
use gpui::{App, Hsla, Window};
pub use otherwise_ui::tokens::{MONO, SANS};

#[derive(Debug, Clone, Copy)]
pub struct Tokens {
    pub ui: otherwise_ui::Theme,
    /// device tags (the family's warn amber)
    pub tag: Hsla,
    /// a device or service that answers (the family's ok green)
    pub up: Hsla,
}

impl std::ops::Deref for Tokens {
    type Target = otherwise_ui::Theme;
    fn deref(&self) -> &otherwise_ui::Theme {
        &self.ui
    }
}

impl Tokens {
    pub fn new(ui: otherwise_ui::Theme) -> Self {
        Tokens { ui, tag: ui.warn, up: ui.ok }
    }

    /// Colour of a network layer (edges, chips, frames); same hues as the exports.
    pub fn layer(&self, l: nodo_core::model::Layer) -> Hsla {
        let (r, g, b) = nodo_core::render::layer_color(l);
        let mut h = otherwise_ui::theme::hex(((r as u32) << 16) | ((g as u32) << 8) | b as u32);
        if !self.dark {
            h.l *= 0.92;
        }
        h
    }
}

pub fn tokens(cx: &App) -> Tokens {
    Tokens::new(otherwise_ui::theme(cx))
}

/// Follows the system's light / dark appearance and sets the family font on gpui-component.
pub fn apply(window: Option<&mut Window>, cx: &mut App) {
    otherwise_ui::apply(otherwise_ui::Appearance::System, window, cx);
}
