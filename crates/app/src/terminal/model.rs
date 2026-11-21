//! One terminal: a PTY child, alacritty's grid and parser, and its event loop thread. Adapted
//! from plate's embedded terminal (same alacritty fork and GPUI rev), minus shell integration.
use alacritty_terminal::event::{Event, EventListener, Notify, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg, Notifier};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as TermPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{ClipboardType, Config, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use anyhow::{Context as _, Result};
use gpui::Hsla;
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long a closed tab's child gets to leave on SIGHUP before it is killed.
const CLOSE_GRACE: Duration = Duration::from_millis(1500);
const SCROLLBACK: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dims {
    pub cols: usize,
    pub rows: usize,
}

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// What to run in a tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spawn {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// On top of the login shell's environment.
    pub env: Vec<(String, String)>,
}

#[derive(Default)]
struct Shared {
    dirty: AtomicBool,
    events: Mutex<Vec<Event>>,
}

#[derive(Clone)]
struct Listener(Arc<Shared>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange => {}
            other => self.0.events.lock().unwrap().push(other),
        }
        self.0.dirty.store(true, Ordering::Release);
    }
}

#[derive(Clone, Debug)]
pub struct CellData {
    pub c: char,
    pub fg: Hsla,
    pub bg: Option<Hsla>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub selected: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub cells: Vec<Vec<Option<CellData>>>,
    pub cursor: Option<(usize, usize, CursorShape)>,
}

pub struct Terminal {
    term: Arc<FairMutex<Term<Listener>>>,
    notifier: Notifier,
    shared: Arc<Shared>,
    pub dims: Dims,
    /// OSC 0/2 title, empty when the child set none.
    pub title: String,
    pub exited: Option<i32>,
    /// Bells rung since the view last looked.
    pub bells: u32,
    #[cfg_attr(not(unix), allow(dead_code))]
    child_pid: u32,
    teardown: Option<EventLoopSender>,
    clipboard: Option<String>,
    default_colors: (Hsla, Hsla),
}

/// The environment a tab's child starts with: the login shell's (PATH from `.zprofile`, nvm,
/// Homebrew…), then the terminal's own variables, then the caller's.
pub fn child_env(extra: &[(String, String)]) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = nodo_core::shell_env::resolved().iter().cloned().collect();
    for (k, v) in [("TERM", "xterm-256color"), ("COLORTERM", "truecolor"), ("TERM_PROGRAM", "nodo")] {
        env.insert(k.into(), v.into());
    }
    env.insert("TERM_PROGRAM_VERSION".into(), crate::VERSION.into());
    for (k, v) in extra {
        env.insert(k.clone(), v.clone());
    }
    env
}

impl Terminal {
    pub fn spawn(spec: &Spawn, dims: Dims, cell: (u16, u16)) -> Result<Self> {
        let env = child_env(&spec.env);
        // resolve against the login shell's PATH, not the bare one an app gets from Finder
        let program = if spec.program.contains(['/', '\\']) { spec.program.clone() } else { nodo_core::cmd::find(&spec.program, &[]).map(|p| p.display().to_string()).unwrap_or_else(|| spec.program.clone()) };
        let options = tty::Options {
            shell: Some(tty::Shell::new(program.clone(), spec.args.clone())),
            working_directory: Some(spec.cwd.clone()),
            drain_on_exit: true,
            env,
            #[cfg(not(windows))]
            child_signal_mask: tty::SignalMask::current().ok(),
            #[cfg(windows)]
            escape_args: true,
        };
        let ws = WindowSize { num_lines: dims.rows as u16, num_cols: dims.cols as u16, cell_width: cell.0, cell_height: cell.1 };
        let pty = tty::new(&options, ws, 0).with_context(|| format!("pty for {program}"))?;
        let shared = Arc::new(Shared::default());
        let listener = Listener(shared.clone());
        let config = Config { scrolling_history: SCROLLBACK, ..Config::default() };
        let term = Arc::new(FairMutex::new(Term::new(config, &dims, listener.clone())));
        let child_pid = pty.child().id();
        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false).context("event loop")?;
        let notifier = Notifier(event_loop.channel());
        let teardown = event_loop.channel();
        event_loop.spawn();
        Ok(Terminal { term, notifier, shared, dims, title: String::new(), exited: None, bells: 0, child_pid, teardown: Some(teardown), clipboard: None, default_colors: (Hsla::white(), Hsla::black()) })
    }

    /// Text the child asked to put on the clipboard (OSC 52); write-only on purpose, reads would
    /// let a child exfiltrate the clipboard.
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.clipboard.take()
    }

    pub fn set_default_colors(&mut self, fg: Hsla, bg: Hsla) {
        self.default_colors = (fg, bg);
    }

    pub fn take_dirty(&self) -> bool {
        self.shared.dirty.swap(false, Ordering::AcqRel)
    }

    /// Drain queued PTY events: answer queries, track title, bell and exit.
    pub fn pump(&mut self) {
        let events: Vec<Event> = std::mem::take(&mut *self.shared.events.lock().unwrap());
        for e in events {
            match e {
                Event::PtyWrite(s) => self.notifier.notify(s.into_bytes()),
                Event::Title(t) => self.title = t,
                Event::ResetTitle => self.title.clear(),
                Event::Bell => self.bells += 1,
                Event::ColorRequest(idx, f) => {
                    let (fg, bg) = self.default_colors;
                    let rgb = self.term.lock().colors()[idx].unwrap_or_else(|| super::colors::query_reply(idx, fg, bg));
                    self.notifier.notify(f(rgb).into_bytes());
                }
                Event::TextAreaSizeRequest(f) => {
                    let ws = WindowSize { num_lines: self.dims.rows as u16, num_cols: self.dims.cols as u16, cell_width: 0, cell_height: 0 };
                    self.notifier.notify(f(ws).into_bytes());
                }
                // `;p` / `;s` (the X11 primary selection) would trample what the user last copied
                Event::ClipboardStore(ClipboardType::Clipboard, text) => self.clipboard = Some(text),
                Event::ChildExit(status) => self.exited = Some(status.code().unwrap_or(-1)),
                Event::Exit if self.exited.is_none() => self.exited = Some(0),
                _ => {}
            }
        }
    }

    /// Bytes the user typed or pasted: drops the selection and jumps to the bottom.
    pub fn input(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        {
            let mut term = self.term.lock();
            term.selection = None;
            term.scroll_display(Scroll::Bottom);
        }
        self.write(bytes);
    }

    /// Bytes the user did not type (mouse reports, alternate-scroll arrows): selection and
    /// viewport stay.
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        self.notifier.notify(bytes);
    }

    pub fn start_selection(&self, ty: SelectionType, point: TermPoint, side: Side) {
        self.term.lock().selection = Some(Selection::new(ty, point, side));
    }

    pub fn update_selection(&self, point: TermPoint, side: Side) {
        if let Some(sel) = self.term.lock().selection.as_mut() {
            sel.update(point, side);
        }
    }

    pub fn clear_selection(&self) {
        self.term.lock().selection = None;
    }

    pub fn select_all(&self) {
        let mut term = self.term.lock();
        let (top, bottom) = (term.grid().topmost_line(), term.grid().bottommost_line());
        let last = Column(term.grid().columns().saturating_sub(1));
        let mut sel = Selection::new(SelectionType::Simple, TermPoint::new(top, Column(0)), Side::Left);
        sel.update(TermPoint::new(bottom, last), Side::Right);
        term.selection = Some(sel);
    }

    pub fn selection_text(&self) -> Option<String> {
        self.term.lock().selection_to_string().filter(|s| !s.trim().is_empty())
    }

    pub fn display_offset(&self) -> usize {
        self.term.lock().grid().display_offset()
    }

    pub fn paste(&self, text: &str) {
        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        let clean = text.replace("\r\n", "\r").replace('\n', "\r");
        if bracketed {
            // a pasted end marker would let the text out of the bracket
            let clean = clean.replace("\x1b[201~", "");
            self.input(format!("\x1b[200~{clean}\x1b[201~").into_bytes());
        } else {
            self.input(clean.into_bytes());
        }
    }

    pub fn mode(&self) -> TermMode {
        *self.term.lock().mode()
    }

    pub fn scroll(&self, lines: i32) {
        self.term.lock().scroll_display(Scroll::Delta(lines));
    }

    pub fn clear_scrollback(&self) {
        self.term.lock().grid_mut().clear_history();
    }

    pub fn resize(&mut self, dims: Dims, cell: (u16, u16)) {
        if dims == self.dims || dims.cols == 0 || dims.rows == 0 {
            return;
        }
        self.dims = dims;
        self.term.lock().resize(dims);
        let ws = WindowSize { num_lines: dims.rows as u16, num_cols: dims.cols as u16, cell_width: cell.0, cell_height: cell.1 };
        let _ = self.notifier.0.send(Msg::Resize(ws));
    }

    /// The lowest non-blank line on screen, for the collapsed bar.
    pub fn last_line(&self) -> Option<String> {
        let term = self.term.lock();
        let grid = term.grid();
        (0..grid.screen_lines() as i32).rev().find_map(|y| {
            let row = &grid[Line(y)];
            let text: String = (0..grid.columns()).map(|c| row[Column(c)].c).collect();
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        })
    }

    pub fn frame(&self, default_fg: Hsla, default_bg: Hsla) -> Frame {
        let term = self.term.lock();
        let content = term.renderable_content();
        let (rows, cols) = (self.dims.rows, self.dims.cols);
        let mut cells: Vec<Vec<Option<CellData>>> = vec![vec![None; cols]; rows];
        let off = content.display_offset as i32;
        let palette = content.colors;
        let sel = content.selection;
        for ic in content.display_iter {
            let row = ic.point.line.0 + off;
            let col = ic.point.column.0;
            if row < 0 || row as usize >= rows || col >= cols {
                continue;
            }
            let cell = ic.cell;
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::HIDDEN) {
                continue;
            }
            let (mut fg, mut bg) = (super::colors::resolve(&cell.fg, palette, default_fg, default_bg), super::colors::resolve(&cell.bg, palette, default_fg, default_bg));
            let inverse = cell.flags.contains(Flags::INVERSE);
            if inverse {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::DIM) {
                fg.a *= 0.6;
            }
            let bg_is_default = matches!(cell.bg, Color::Named(NamedColor::Background)) && !inverse;
            cells[row as usize][col] = Some(CellData {
                c: cell.c,
                fg,
                bg: if bg_is_default { None } else { Some(bg) },
                bold: cell.flags.contains(Flags::BOLD),
                italic: cell.flags.contains(Flags::ITALIC),
                underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
                selected: sel.is_some_and(|r| r.contains(ic.point)),
            });
        }
        let cur = content.cursor;
        let crow = cur.point.line.0 + off;
        let cursor = (crow >= 0 && (crow as usize) < rows && cur.point.column.0 < cols && cur.shape != CursorShape::Hidden).then_some((cur.point.column.0, crow as usize, cur.shape));
        Frame { cells, cursor }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let Some(tx) = self.teardown.take() else { return };
        #[cfg(unix)]
        {
            // the pty gave the child its own session: signal the group so what it started
            // (an ssh ControlMaster, a ping) goes too. Synchronously, because on quit gpui exits
            // soon after and a signal left to a thread might never be sent.
            let group = -(self.child_pid as i32);
            // SAFETY: signalling a process group by id; a gone group yields ESRCH, ignored
            unsafe { libc::kill(group, libc::SIGHUP) };
            std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + CLOSE_GRACE;
                // SAFETY: signal 0 only probes for existence
                while std::time::Instant::now() < deadline && unsafe { libc::kill(group, 0) } == 0 {
                    std::thread::sleep(Duration::from_millis(25));
                }
                // SAFETY: as above
                unsafe { libc::kill(group, libc::SIGKILL) };
                let _ = tx.send(Msg::Shutdown);
            });
        }
        #[cfg(not(unix))]
        {
            let _ = tx.send(Msg::Shutdown);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{Dims, Frame, Spawn, Terminal};
    use gpui::Hsla;
    use std::time::{Duration, Instant};

    fn sh(script: &str) -> Spawn {
        Spawn { program: "/bin/sh".into(), args: vec!["-c".into(), script.into()], cwd: std::env::temp_dir(), env: vec![("NODO_TEST_VAR".into(), "it's here".into())] }
    }

    fn text_of(frame: &Frame) -> String {
        frame.cells.iter().map(|r| r.iter().map(|c| c.as_ref().map(|c| c.c).unwrap_or(' ')).collect::<String>().trim_end().to_string()).collect::<Vec<_>>().join("\n")
    }

    fn wait_for(t: &mut Terminal, needle: &str) -> Frame {
        let start = Instant::now();
        loop {
            t.take_dirty();
            t.pump();
            let f = t.frame(Hsla::white(), Hsla::black());
            if text_of(&f).contains(needle) {
                return f;
            }
            assert!(start.elapsed() < Duration::from_secs(5), "timed out waiting for {needle:?}; screen:\n{}", text_of(&f));
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    #[test]
    fn env_title_bell_and_colors() {
        let script = "printf '\\033]0;my title\\007\\007'; printf '\\033[1;31mRED\\033[0m %s\\n' \"$NODO_TEST_VAR\"; echo \"term=$TERM\"; sleep 3";
        let mut t = Terminal::spawn(&sh(script), Dims { cols: 40, rows: 5 }, (8, 16)).unwrap();
        let f = wait_for(&mut t, "term=xterm-256color");
        assert!(text_of(&f).contains("RED it's here"), "extra env reaches the child as a value");
        assert_eq!(t.title, "my title");
        assert_eq!(t.bells, 1);
        let red = f.cells[0][0].as_ref().unwrap();
        let plain = f.cells[0][4].as_ref().unwrap();
        assert!(red.bold && !plain.bold);
        assert_ne!(red.fg, plain.fg);
    }

    #[test]
    fn input_resize_selection_and_exit() {
        let spec = Spawn { program: "/bin/cat".into(), args: vec![], cwd: std::env::temp_dir(), env: vec![] };
        let mut t = Terminal::spawn(&spec, Dims { cols: 40, rows: 6 }, (8, 16)).unwrap();
        t.input(b"hello nodo\r".to_vec());
        wait_for(&mut t, "hello nodo");
        t.select_all();
        assert!(t.selection_text().unwrap().contains("hello nodo"));
        t.resize(Dims { cols: 20, rows: 4 }, (8, 16));
        t.input(b"after\r".to_vec());
        let f = wait_for(&mut t, "after");
        assert_eq!((f.cells[0].len(), f.cells.len()), (20, 4));
        assert!(t.selection_text().is_none(), "typing drops the selection");
        t.input(b"\x04".to_vec());
        let start = Instant::now();
        while t.exited.is_none() {
            t.pump();
            assert!(start.elapsed() < Duration::from_secs(5), "cat did not exit");
            std::thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(t.exited, Some(0));
    }

    #[test]
    fn scrollback() {
        let mut t = Terminal::spawn(&sh("seq 1 100; sleep 3"), Dims { cols: 40, rows: 6 }, (8, 16)).unwrap();
        wait_for(&mut t, "100");
        t.scroll(20);
        let up = t.frame(Hsla::white(), Hsla::black());
        assert!(t.display_offset() >= 20);
        assert!(!text_of(&up).contains("100"));
        t.clear_scrollback();
        assert_eq!(t.display_offset(), 0);
    }
}
