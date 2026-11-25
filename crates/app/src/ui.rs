//! Small styled building blocks shared by the top bar, panel and screens.
use crate::theme::Tokens;
use gpui::*;
use gpui_component::IconName;
use otherwise_ui::tokens::{icon as icon_size, text};
pub use otherwise_ui::{filled_button, icon, outline_button, section_title};

/// The family's square icon-only button with one of gpui-component's icons.
pub fn icon_button(t: &Tokens, id: impl Into<ElementId>, name: IconName, enabled: bool) -> Stateful<Div> {
    otherwise_ui::icon_button(t, id, enabled).child(icon(name, icon_size::TOOLBAR, if enabled { t.ink } else { t.ink_faint }))
}

/// The app's own mark.
pub fn logo_mark(size: f32, color: Hsla) -> Svg {
    svg().path(crate::LOGO_SVG).flex_none().size(px(size)).text_color(color)
}

/// One of our own icons (`icons/kind/<name>.svg`).
pub fn kind_svg(name: &str, size: f32, color: Hsla) -> Svg {
    svg().path(SharedString::from(format!("icons/kind/{name}.svg"))).flex_none().size(px(size)).text_color(color) // i18n-ok: asset path
}

/// Small rounded label.
pub fn chip(text: impl Into<SharedString>, color: Hsla, size: f32) -> Div {
    div().flex_none().px(px(6.)).rounded(px(5.)).bg(color.opacity(0.14)).text_color(color).text_size(px(size)).child(text.into())
}

/// Segmented control of two or more options.
pub fn seg(t: &Tokens, id: impl Into<ElementId>, label: String, on: bool) -> Stateful<Div> {
    otherwise_ui::segment(t, id, on).child(label)
}

pub fn seg_group(t: &Tokens) -> Div {
    otherwise_ui::segmented(t)
}

pub fn code_block(t: &Tokens, id: impl Into<ElementId>, text: String) -> Stateful<Div> {
    div().id(id).max_h(px(160.)).overflow_y_scroll().p(px(8.)).rounded(px(6.)).bg(t.canvas).font_family(crate::theme::MONO).text_size(px(text::CAPTION)).line_height(px(16.)).child(text)
}

pub fn backdrop(t: &Tokens, id: &'static str) -> Stateful<Div> {
    div().id(id).absolute().inset_0().flex().items_center().justify_center().bg(t.backdrop)
}

