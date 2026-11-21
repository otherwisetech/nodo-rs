//! A terminal tab's surface: input, mouse, scrolling, and the grid painted with GPUI text runs.
//! From plate's `TerminalView` / `TerminalElement`.
use super::model::{Dims, Frame, Spawn, Terminal};
use alacritty_terminal::index::{Column, Line, Point as TermPoint, Side};
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::vte::ansi::CursorShape;
use gpui::*;
use std::path::PathBuf;
use std::time::Duration;

/// Key context of the terminal surface; bindings scoped to it out-rank the app's (see `bind_keys`).
pub const KEY_CONTEXT: &str = "NodoTerminal";

pub const FONT_MIN: f32 = 9.;
pub const FONT_MAX: f32 = 24.;
pub const FONT_DEFAULT: f32 = 12.5;

/// Font size and colours every terminal paints with; set from settings and the theme.
#[derive(Debug, Clone, Copy)]
pub struct TermStyle {
    pub font_size: f32,
    pub fg: Hsla,
    pub bg: Hsla,
    pub selection: Hsla,
}

impl Global for TermStyle {}

impl TermStyle {
    pub fn from_tokens(t: &crate::theme::Tokens, font_size: f32) -> TermStyle {
        let bg: Hsla = if t.dark { rgb(0x161514).into() } else { rgb(0xFFFFFF).into() };
        TermStyle { font_size: font_size.clamp(FONT_MIN, FONT_MAX), fg: t.ink, bg, selection: t.accent.opacity(if t.dark { 0.38 } else { 0.24 }) }
    }
}

fn style(cx: &App) -> TermStyle {
    cx.try_global::<TermStyle>().copied().unwrap_or_else(|| TermStyle::from_tokens(&crate::theme::tokens(cx), FONT_DEFAULT))
}

/// Title, exit or bell changed: the tab strip and the graph's badges re-render.
pub enum TermEvent {
    Changed,
}

impl EventEmitter<TermEvent> for TerminalView {}

pub struct TerminalView {
    pub terminal: Option<Terminal>,
    focus: FocusHandle,
    cell: (Pixels, Pixels),
    pub last_error: Option<String>,
    /// Bells not yet seen; cleared when the tab is shown.
    pub unseen_bell: bool,
    scroll_remainder: f32,
    origin: Point<Pixels>,
    drag: Option<Drag>,
    last_report: Option<(usize, usize)>,
    hitbox: Option<HitboxId>,
    focused: bool,
    seen: (String, Option<i32>, u32),
}

/// Who owns the button that is down: our selection (anchored at a pixel) or a child that asked
/// for mouse tracking.
enum Drag {
    Ours(Point<Pixels>),
    Child,
}

impl TerminalView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(33)).await;
                let alive = this.update(cx, |this, cx| this.tick(cx));
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        TerminalView { terminal: None, focus: cx.focus_handle(), cell: (px(8.), px(16.)), last_error: None, unseen_bell: false, scroll_remainder: 0.0, origin: point(px(0.), px(0.)), drag: None, last_report: None, hitbox: None, focused: false, seen: (String::new(), None, 0) }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.terminal.as_mut() else { return };
        if t.take_dirty() {
            t.pump();
            cx.notify();
        }
        let now = (t.title.clone(), t.exited, t.bells);
        if now != self.seen {
            if now.2 != self.seen.2 && !self.focused {
                self.unseen_bell = true;
            }
            self.seen = now;
            cx.emit(TermEvent::Changed);
        }
        // only the focused tab may write the clipboard: a background tab printing OSC 52 must
        // not replace what the user last copied
        if let Some(text) = t.take_clipboard()
            && self.focused
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub fn run(&mut self, spec: &Spawn, window: &mut Window, cx: &mut Context<Self>) {
        let dims = self.terminal.as_ref().map(|t| t.dims).unwrap_or(Dims { cols: 100, rows: 24 });
        self.terminal = None;
        let cell = (f32::from(self.cell.0) as u16, f32::from(self.cell.1) as u16);
        match Terminal::spawn(spec, dims, cell) {
            Ok(mut t) => {
                let s = style(cx);
                t.set_default_colors(s.fg, s.bg);
                self.terminal = Some(t);
                self.last_error = None;
            }
            Err(e) => self.last_error = Some(format!("{e:#}")),
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    pub fn is_running(&self) -> bool {
        self.terminal.as_ref().is_some_and(|t| t.exited.is_none())
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.terminal.as_ref().and_then(|t| t.exited)
    }

    pub fn title(&self) -> Option<String> {
        self.terminal.as_ref().map(|t| t.title.clone()).filter(|t| !t.is_empty())
    }

    pub fn last_line(&self) -> Option<String> {
        self.terminal.as_ref().and_then(|t| t.last_line())
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = &self.terminal else { return };
        let ks = &ev.keystroke;
        if ks.modifiers.platform {
            match ks.key.as_str() {
                "v" => {
                    if let Some(item) = cx.read_from_clipboard() {
                        if let Some(paths) = item.entries().iter().find_map(|e| match e {
                            ClipboardEntry::ExternalPaths(p) => Some(p.paths()),
                            _ => None,
                        }) {
                            let text = paths_to_paste(paths);
                            if !text.is_empty() {
                                t.paste(&text);
                            }
                        } else if let Some(text) = item.text() {
                            t.paste(&text);
                        }
                    }
                    cx.stop_propagation();
                }
                "c" => {
                    if let Some(text) = t.selection_text() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                    cx.stop_propagation();
                }
                "a" => {
                    t.select_all();
                    cx.stop_propagation();
                    cx.notify();
                }
                "k" => {
                    t.clear_scrollback();
                    // ^L redraws the prompt at the top, like Terminal.app's Clear to Start
                    t.input(&b"\x0c"[..]);
                    cx.stop_propagation();
                    cx.notify();
                }
                _ => {
                    if let Some(bytes) = super::keys::platform_bytes(ks) {
                        t.input(bytes);
                        cx.stop_propagation();
                        cx.notify();
                    }
                }
            }
            return;
        }
        // Linux / Windows: ctrl-shift-c / v are copy and paste, ctrl-c stays the child's
        if cfg!(not(target_os = "macos")) && ks.modifiers.control && ks.modifiers.shift && !ks.modifiers.alt {
            match ks.key.to_lowercase().as_str() {
                "c" => {
                    if let Some(text) = t.selection_text() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                    cx.stop_propagation();
                    return;
                }
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
                        t.paste(&text);
                    }
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        if ks.modifiers.shift && (ks.key == "pageup" || ks.key == "pagedown") {
            let page = t.dims.rows.max(1) as i32 - 1;
            t.scroll(if ks.key == "pageup" { page } else { -page });
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if let Some(bytes) = super::keys::to_bytes(ks, t.mode()) {
            t.input(bytes);
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = &self.terminal else { return };
        let lines = match ev.delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => f32::from(p.y) / f32::from(self.cell.1).max(1.0),
        };
        let n = scroll_lines(&mut self.scroll_remainder, lines);
        if n == 0 {
            return;
        }
        let col = ((f32::from(ev.position.x - self.origin.x) / f32::from(self.cell.0).max(1.0)).floor().max(0.0)) as usize;
        let row = ((f32::from(ev.position.y - self.origin.y) / f32::from(self.cell.1).max(1.0)).floor().max(0.0)) as usize;
        match super::keys::wheel_bytes(t.mode(), n, col, row, ev.modifiers.shift) {
            Some(bytes) => t.write(bytes),
            None => t.scroll(n),
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Tell the child about a click it asked to hear about; `true` if it took it.
    fn report_mouse(&mut self, kind: super::keys::MouseKind, pos: Point<Pixels>, mods: Modifiers) -> bool {
        let Some(t) = &self.terminal else { return false };
        let off = t.display_offset();
        let (point, _) = grid_point(pos, self.origin, self.cell, t.dims, off);
        let cell = (point.column.0, (point.line.0 + off as i32).max(0) as usize);
        if kind == super::keys::MouseKind::Drag && self.last_report == Some(cell) {
            return true;
        }
        let Some(bytes) = super::keys::mouse_bytes(t.mode(), kind, 0, cell.0, cell.1, mods.alt, mods.control) else {
            return false;
        };
        t.write(bytes);
        self.last_report = Some(cell);
        true
    }

    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.unseen_bell = false;
        if self.terminal.is_none() || ev.button != MouseButton::Left {
            return;
        }
        // shift keeps the mouse for our own selection even when the child tracks it
        if !ev.modifiers.shift && self.report_mouse(super::keys::MouseKind::Press, ev.position, ev.modifiers) {
            self.drag = Some(Drag::Child);
            self.capture(window);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let Some(t) = &self.terminal else { return };
        let (point, side) = grid_point(ev.position, self.origin, self.cell, t.dims, t.display_offset());
        let ty = match ev.click_count {
            2 => SelectionType::Semantic,
            n if n >= 3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        t.start_selection(ty, point, side);
        self.drag = Some(Drag::Ours(ev.position));
        self.capture(window);
        cx.notify();
    }

    /// gpui reports a captured hitbox as hovered wherever the pointer goes, so a drag that leaves
    /// the terminal still gets its moves and its release.
    fn capture(&self, window: &mut Window) {
        if let Some(id) = self.hitbox {
            window.capture_pointer(id);
        }
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        if ev.pressed_button != Some(MouseButton::Left) {
            return;
        }
        match self.drag {
            Some(Drag::Child) => {
                self.report_mouse(super::keys::MouseKind::Drag, ev.position, ev.modifiers);
            }
            Some(Drag::Ours(_)) => {
                let Some(t) = &self.terminal else { return };
                let (point, side) = grid_point(ev.position, self.origin, self.cell, t.dims, t.display_offset());
                t.update_selection(point, side);
                cx.notify();
            }
            None => {}
        }
    }

    fn on_mouse_up(&mut self, ev: &MouseUpEvent, cx: &mut Context<Self>) {
        if ev.button != MouseButton::Left {
            return;
        }
        let Some(drag) = self.drag.take() else { return };
        self.last_report = None;
        match drag {
            // owed whatever the modifiers say now, or the child thinks the button is still down
            Drag::Child => {
                self.report_mouse(super::keys::MouseKind::Release, ev.position, ev.modifiers);
            }
            Drag::Ours(anchor) => {
                let moved = (f32::from(ev.position.x - anchor.x)).abs() > 3.0 || (f32::from(ev.position.y - anchor.y)).abs() > 3.0;
                if ev.click_count <= 1 && !moved
                    && let Some(t) = &self.terminal
                {
                    t.clear_selection();
                }
            }
        }
        cx.notify();
    }

    fn on_file_drop(&mut self, paths: &ExternalPaths, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = &self.terminal else { return };
        let text = paths_to_paste(paths.paths());
        if !text.is_empty() {
            t.paste(&text);
            cx.notify();
        }
    }
}

/// Pixel position → grid point + side, clamped to the viewport; the display offset maps viewport
/// rows back into scrollback lines.
pub fn grid_point(pos: Point<Pixels>, origin: Point<Pixels>, cell: (Pixels, Pixels), dims: Dims, display_offset: usize) -> (TermPoint, Side) {
    let x = f32::from(pos.x - origin.x).max(0.0);
    let y = f32::from(pos.y - origin.y).max(0.0);
    let cw = f32::from(cell.0).max(1.0);
    let lh = f32::from(cell.1).max(1.0);
    let col = ((x / cw).floor() as usize).min(dims.cols.saturating_sub(1));
    let row = ((y / lh).floor() as usize).min(dims.rows.saturating_sub(1));
    let side = if (x / cw).fract() > 0.5 { Side::Right } else { Side::Left };
    (TermPoint::new(Line(row as i32 - display_offset as i32), Column(col)), side)
}

/// Dropped or pasted files: each path shell-quoted, space-separated, with a trailing space.
pub fn paths_to_paste(paths: &[PathBuf]) -> String {
    let mut text: String = paths.iter().filter_map(|p| p.to_str()).map(|p| format!(" {}", nodo_core::remote::sh_quote(p))).collect();
    if !text.is_empty() {
        text.push(' ');
    }
    text
}

/// Whole lines to scroll for this event, carrying the fraction into the next one.
pub fn scroll_lines(remainder: &mut f32, delta_lines: f32) -> i32 {
    if (*remainder > 0.0 && delta_lines < 0.0) || (*remainder < 0.0 && delta_lines > 0.0) {
        *remainder = 0.0;
    }
    *remainder += delta_lines;
    let n = remainder.trunc();
    *remainder -= n;
    n as i32
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("terminal-view")
            .track_focus(&self.focus)
            .key_context(KEY_CONTEXT)
            .size_full()
            .bg(style(cx).bg)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_drop(cx.listener(Self::on_file_drop))
            .child(TerminalElement { view: cx.entity() })
    }
}

pub struct TerminalElement {
    view: Entity<TerminalView>,
}

pub struct Prepared {
    frame: Option<Frame>,
    origin: Point<Pixels>,
    cell_w: Pixels,
    line_h: Pixels,
    font: Font,
    font_size: Pixels,
    style: TermStyle,
    focused: bool,
    message: Option<String>,
    hitbox: Hitbox,
}

impl IntoElement for TerminalElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepared;

    fn id(&self) -> Option<ElementId> {
        Some("nodo-terminal-grid".into())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) -> Prepared {
        let st = style(cx);
        let font = font(crate::theme::MONO);
        let font_size = px(st.font_size);
        let font_id = cx.text_system().resolve_font(&font);
        let cell_w = cx.text_system().advance(font_id, font_size, 'm').map(|s| s.width).unwrap_or(px(8.));
        let line_h = (font_size * 1.3).round();
        let pad = px(6.);
        let cols = ((f32::from(bounds.size.width - pad * 2.) / f32::from(cell_w)).floor() as usize).max(2);
        let rows = ((f32::from(bounds.size.height - pad * 2.) / f32::from(line_h)).floor() as usize).max(1);
        let focused = self.view.read(cx).focus.is_focused(window);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let hitbox_id = hitbox.id;
        // hitbox ids are fresh every frame: re-assert the capture for as long as the drag lives
        if self.view.read(cx).drag.is_some() {
            window.capture_pointer(hitbox_id);
        }
        let origin = bounds.origin + point(pad, pad);
        let (frame, message) = self.view.update(cx, |v, _| {
            v.cell = (cell_w, line_h);
            v.origin = origin;
            v.hitbox = Some(hitbox_id);
            v.focused = focused;
            if focused {
                v.unseen_bell = false;
            }
            let cell = (f32::from(cell_w) as u16, f32::from(line_h) as u16);
            match v.terminal.as_mut() {
                Some(t) => {
                    t.resize(Dims { cols, rows }, cell);
                    t.set_default_colors(st.fg, st.bg);
                    (Some(t.frame(st.fg, st.bg)), None)
                }
                None => (None, Some(v.last_error.as_ref().map(|e| i18n::t!("app.term.failed", detail = e)).unwrap_or_default())),
            }
        });
        Prepared { frame, origin, cell_w, line_h, font, font_size, style: st, focused, message, hitbox }
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), p: &mut Prepared, window: &mut Window, cx: &mut App) {
        let view = self.view.clone();
        let hb = p.hitbox.clone();
        window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hb.is_hovered(window) {
                view.update(cx, |v, cx| v.on_mouse_move(ev, cx));
            }
        });
        let view = self.view.clone();
        let hb = p.hitbox.clone();
        window.on_mouse_event(move |ev: &MouseUpEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hb.is_hovered(window) {
                view.update(cx, |v, cx| v.on_mouse_up(ev, cx));
            }
        });
        let (fg0, bg0) = (p.style.fg, p.style.bg);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            window.paint_quad(fill(bounds, bg0));
            let Some(frame) = &p.frame else {
                if let Some(msg) = p.message.as_ref().filter(|m| !m.is_empty()) {
                    let run = TextRun { len: msg.len(), font: p.font.clone(), color: fg0, background_color: None, underline: None, strikethrough: None };
                    let line = window.text_system().shape_line(msg.clone().into(), p.font_size, &[run], None);
                    let _ = line.paint(p.origin, p.line_h, TextAlign::Left, None, window, cx);
                }
                return;
            };
            let cell_at = |col: usize, row: usize| point(p.origin.x + p.cell_w * col as f32, p.origin.y + p.line_h * row as f32);
            for (row_ix, row) in frame.cells.iter().enumerate() {
                for (col_ix, c) in row.iter().enumerate() {
                    let Some(c) = c else { continue };
                    let b = Bounds::new(cell_at(col_ix, row_ix), size(p.cell_w, p.line_h));
                    if let Some(bg) = c.bg {
                        window.paint_quad(fill(b, bg));
                    }
                    if c.selected {
                        window.paint_quad(fill(b, p.style.selection));
                    }
                }
            }
            for (row_ix, row) in frame.cells.iter().enumerate() {
                let mut col_ix = 0;
                while col_ix < row.len() {
                    let Some(c) = &row[col_ix] else {
                        col_ix += 1;
                        continue;
                    };
                    if c.c == ' ' && !c.underline {
                        col_ix += 1;
                        continue;
                    }
                    let start = col_ix;
                    let mut text = String::new();
                    let (fg, bold, italic, ul) = (c.fg, c.bold, c.italic, c.underline);
                    while col_ix < row.len() {
                        match &row[col_ix] {
                            Some(n) if n.fg == fg && n.bold == bold && n.italic == italic && n.underline == ul => {
                                text.push(if n.c == '\0' { ' ' } else { n.c });
                                col_ix += 1;
                            }
                            None => {
                                text.push(' ');
                                col_ix += 1;
                            }
                            _ => break,
                        }
                    }
                    if !ul {
                        let trimmed = text.trim_end().len();
                        text.truncate(trimmed);
                    }
                    if text.is_empty() {
                        continue;
                    }
                    let run = TextRun {
                        len: text.len(),
                        font: Font { weight: if bold { FontWeight::BOLD } else { FontWeight::NORMAL }, style: if italic { FontStyle::Italic } else { FontStyle::Normal }, ..p.font.clone() },
                        color: fg,
                        background_color: None,
                        underline: ul.then(|| UnderlineStyle { thickness: px(1.), color: Some(fg), wavy: false }),
                        strikethrough: None,
                    };
                    let line = window.text_system().shape_line(text.into(), p.font_size, &[run], Some(p.cell_w));
                    let _ = line.paint(cell_at(start, row_ix), p.line_h, TextAlign::Left, None, window, cx);
                }
            }
            if let Some((col, row, shape)) = frame.cursor {
                let origin = cell_at(col, row);
                let mut color = fg0;
                color.a = if p.focused { 0.85 } else { 0.4 };
                let b = match shape {
                    CursorShape::Beam => Bounds::new(origin, size(px(2.), p.line_h)),
                    CursorShape::Underline => Bounds::new(origin + point(px(0.), p.line_h - px(2.)), size(p.cell_w, px(2.))),
                    _ => Bounds::new(origin, size(p.cell_w, p.line_h)),
                };
                if p.focused && matches!(shape, CursorShape::Block) {
                    window.paint_quad(fill(b, color));
                    if let Some(Some(c)) = frame.cells.get(row).and_then(|r| r.get(col))
                        && c.c != ' '
                    {
                        let run = TextRun { len: c.c.len_utf8(), font: p.font.clone(), color: bg0, background_color: None, underline: None, strikethrough: None };
                        let line = window.text_system().shape_line(c.c.to_string().into(), p.font_size, &[run], Some(p.cell_w));
                        let _ = line.paint(origin, p.line_h, TextAlign::Left, None, window, cx);
                    }
                } else if matches!(shape, CursorShape::Block | CursorShape::HollowBlock) {
                    let mut q = fill(b, Hsla::transparent_black());
                    q.border_widths = Edges::all(px(1.));
                    q.border_color = color;
                    window.paint_quad(q);
                } else {
                    window.paint_quad(fill(b, color));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{grid_point, paths_to_paste, scroll_lines};
    use crate::terminal::model::Dims;
    use alacritty_terminal::index::Side;
    use gpui::{Pixels, point, px};
    use std::path::PathBuf;

    #[test]
    fn pixels_map_to_grid_points() {
        let origin = point(px(10.), px(20.));
        let cell: (Pixels, Pixels) = (px(8.), px(16.));
        let dims = Dims { cols: 80, rows: 24 };
        let (p, side) = grid_point(point(px(35.), px(52.)), origin, cell, dims, 0);
        assert_eq!((p.line.0, p.column.0, side), (2, 3, Side::Left));
        let (p, _) = grid_point(point(px(35.), px(52.)), origin, cell, dims, 10);
        assert_eq!(p.line.0, -8, "scrolled up 10: viewport row 2 is history line -8");
        let (p, _) = grid_point(point(px(9999.), px(9999.)), origin, cell, dims, 0);
        assert_eq!((p.line.0, p.column.0), (23, 79));
    }

    #[test]
    fn paste_paths_and_wheel() {
        assert_eq!(paths_to_paste(&[PathBuf::from("/tmp/a b.png"), PathBuf::from("/tmp/c.png")]), " '/tmp/a b.png' /tmp/c.png ");
        assert_eq!(paths_to_paste(&[]), "");
        let mut rem = 0.0;
        let total: i32 = (0..12).map(|_| scroll_lines(&mut rem, 0.2)).sum();
        assert_eq!(total, 2);
        assert_eq!(scroll_lines(&mut rem, -1.0), -1, "reversal drops the leftover");
    }
}
