use alacritty_terminal::term::TermMode;
use gpui::Keystroke;
use std::borrow::Cow;

/// Which half of a click, or the motion between them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseKind {
    Press,
    Release,
    Drag,
}

/// What a click/release/drag means to an app that asked for mouse tracking (a TUI that runs its own
/// selection). `None` means the app wants no such report. Whether the *user* wants the
/// child to have the mouse at all is the caller's call, decided once at press time (shift keeps it
/// for our own selection): once the child owns a drag, every part of it must be delivered, or it is
/// left believing the button is still down.
pub fn mouse_bytes(mode: TermMode, kind: MouseKind, button: u8, col: usize, row: usize, alt: bool, ctrl: bool) -> Option<Vec<u8>> {
    if !mode.intersects(TermMode::MOUSE_MODE) {
        return None;
    }
    // motion is only reported to an app that asked for it; click-only mode (1000) gets press/release
    if kind == MouseKind::Drag && !mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) {
        return None;
    }
    let mut code = u32::from(button.min(2));
    if kind == MouseKind::Drag {
        code += 32;
    }
    if alt {
        code += 8;
    }
    if ctrl {
        code += 16;
    }
    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if kind == MouseKind::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{};{}{end}", col + 1, row + 1).into_bytes());
    }
    // X10 cannot address past column 223, so clamp rather than drop: a report the child is waiting
    // for (a release, above all) matters more than its exact column. Release is button 3, not a suffix.
    let (col, row) = (col.min(222), row.min(222));
    let code = if kind == MouseKind::Release { (code & !3) + 3 } else { code };
    Some(vec![0x1b, b'[', b'M', 32 + code as u8, 32 + col as u8 + 1, 32 + row as u8 + 1])
}

/// What a wheel of `lines` (positive = up) means to the app on the other side of the PTY: mouse
/// reports when it asked for mouse tracking, arrow keys under alternate-scroll on the alt screen
/// (Claude Code, less, vim…), otherwise None = scroll our own scrollback. Shift bypasses the app so
/// a user can always reach the terminal's history.
pub fn wheel_bytes(mode: TermMode, lines: i32, col: usize, row: usize, shift: bool) -> Option<Vec<u8>> {
    if lines == 0 || shift {
        return None;
    }
    let up = lines > 0;
    let count = lines.unsigned_abs() as usize;
    if mode.intersects(TermMode::MOUSE_MODE) {
        let button = if up { 64 } else { 65 };
        let mut out = Vec::new();
        for _ in 0..count {
            if mode.contains(TermMode::SGR_MOUSE) {
                out.extend_from_slice(format!("\x1b[<{button};{};{}M", col + 1, row + 1).as_bytes());
            } else {
                let (col, row) = (col.min(222), row.min(222));
                out.extend_from_slice(&[0x1b, b'[', b'M', 32 + button as u8, 32 + col as u8 + 1, 32 + row as u8 + 1]);
            }
        }
        return Some(out);
    }
    if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
        let key: &[u8] = match (up, mode.contains(TermMode::APP_CURSOR)) {
            (true, true) => b"\x1bOA",
            (true, false) => b"\x1b[A",
            (false, true) => b"\x1bOB",
            (false, false) => b"\x1b[B",
        };
        return Some(key.repeat(count));
    }
    None
}

/// ⌘-chords the child never sees as such — Ghostty's macOS defaults: ⌘← / ⌘→ to the line's start /
/// end as ^A / ^E, ⌘⌫ kills to the start as ^U. Readline, zsh and Claude Code all honour those
/// three, where Home/End reach fewer prompts.
pub fn platform_bytes(ks: &Keystroke) -> Option<&'static [u8]> {
    let m = &ks.modifiers;
    if !m.platform || m.control || m.alt || m.shift {
        return None;
    }
    match ks.key.as_str() {
        "left" => Some(b"\x01"),
        "right" => Some(b"\x05"),
        "backspace" => Some(b"\x15"),
        _ => None,
    }
}

/// Keystroke → bytes for the PTY. Follows xterm conventions (same table Zed uses).
pub fn to_bytes(ks: &Keystroke, mode: TermMode) -> Option<Cow<'static, [u8]>> {
    let m = &ks.modifiers;
    let ctrl = m.control;
    let alt = m.alt;
    let shift = m.shift;
    let plain = !ctrl && !alt && !shift && !m.platform;
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let key = ks.key.as_str();

    let fixed: Option<&'static [u8]> = match (key, ctrl, alt, shift) {
        ("tab", false, false, false) => Some(b"\x09"),
        ("tab", false, false, true) => Some(b"\x1b[Z"),
        ("escape", _, _, _) => Some(b"\x1b"),
        ("enter", false, false, false) => Some(b"\x0d"),
        ("enter", false, false, true) => Some(b"\x0a"),
        ("enter", false, true, false) => Some(b"\x1b\x0d"),
        ("backspace", false, false, _) => Some(b"\x7f"),
        ("backspace", true, false, false) => Some(b"\x08"),
        ("backspace", false, true, false) => Some(b"\x1b\x7f"),
        ("space", true, false, false) => Some(b"\x00"),
        ("home", false, false, false) => Some(if app_cursor { b"\x1bOH" } else { b"\x1b[H" }),
        ("end", false, false, false) => Some(if app_cursor { b"\x1bOF" } else { b"\x1b[F" }),
        ("up", false, false, false) => Some(if app_cursor { b"\x1bOA" } else { b"\x1b[A" }),
        ("down", false, false, false) => Some(if app_cursor { b"\x1bOB" } else { b"\x1b[B" }),
        ("right", false, false, false) => Some(if app_cursor { b"\x1bOC" } else { b"\x1b[C" }),
        ("left", false, false, false) => Some(if app_cursor { b"\x1bOD" } else { b"\x1b[D" }),
        // word motion as ⎋b / ⎋f rather than CSI 1;3 — zsh's emacs keymap and readline bind the
        // former out of the box, the latter only with an inputrc (Ghostty's macOS default too)
        ("left", false, true, false) => Some(b"\x1bb"),
        ("right", false, true, false) => Some(b"\x1bf"),
        ("insert", false, false, false) => Some(b"\x1b[2~"),
        ("delete", false, false, false) => Some(b"\x1b[3~"),
        ("pageup", false, false, false) => Some(b"\x1b[5~"),
        ("pagedown", false, false, false) => Some(b"\x1b[6~"),
        ("f1", false, false, false) => Some(b"\x1bOP"),
        ("f2", false, false, false) => Some(b"\x1bOQ"),
        ("f3", false, false, false) => Some(b"\x1bOR"),
        ("f4", false, false, false) => Some(b"\x1bOS"),
        ("f5", false, false, false) => Some(b"\x1b[15~"),
        ("f6", false, false, false) => Some(b"\x1b[17~"),
        ("f7", false, false, false) => Some(b"\x1b[18~"),
        ("f8", false, false, false) => Some(b"\x1b[19~"),
        ("f9", false, false, false) => Some(b"\x1b[20~"),
        ("f10", false, false, false) => Some(b"\x1b[21~"),
        ("f11", false, false, false) => Some(b"\x1b[23~"),
        ("f12", false, false, false) => Some(b"\x1b[24~"),
        _ => None,
    };
    if let Some(b) = fixed {
        return Some(Cow::Borrowed(b));
    }

    if ctrl && !alt {
        let c = key.chars().next()?;
        let byte = match c.to_ascii_lowercase() {
            'a'..='z' => Some(c.to_ascii_lowercase() as u8 - b'a' + 1),
            '@' => Some(0),
            '[' => Some(0x1b),
            '\\' => Some(0x1c),
            ']' => Some(0x1d),
            '^' => Some(0x1e),
            '_' => Some(0x1f),
            '?' => Some(0x7f),
            _ => None,
        };
        if let Some(b) = byte {
            return Some(Cow::Owned(vec![b]));
        }
    }

    if !plain {
        let code = 1 + (shift as u8) + (alt as u8) * 2 + (ctrl as u8) * 4;
        let seq = match key {
            "up" => Some(format!("\x1b[1;{code}A")),
            "down" => Some(format!("\x1b[1;{code}B")),
            "right" => Some(format!("\x1b[1;{code}C")),
            "left" => Some(format!("\x1b[1;{code}D")),
            "home" => Some(format!("\x1b[1;{code}H")),
            "end" => Some(format!("\x1b[1;{code}F")),
            "delete" => Some(format!("\x1b[3;{code}~")),
            "pageup" => Some(format!("\x1b[5;{code}~")),
            "pagedown" => Some(format!("\x1b[6;{code}~")),
            _ => None,
        };
        if let Some(s) = seq {
            return Some(Cow::Owned(s.into_bytes()));
        }
    }

    // printable text (option-x produces the composed char in key_char on macOS)
    let text = ks.key_char.clone().or_else(|| if key.chars().count() == 1 && !ctrl && !m.platform { Some(key.to_string()) } else { None })?;
    if alt && ks.key_char.is_none() {
        let mut v = vec![0x1b];
        v.extend_from_slice(text.as_bytes());
        return Some(Cow::Owned(v));
    }
    Some(Cow::Owned(text.into_bytes()))
}

#[cfg(test)]
mod wheel_tests {
    use super::{MouseKind, mouse_bytes, wheel_bytes};
    use alacritty_terminal::term::TermMode;

    #[test]
    fn mouse_reports_follow_the_terminal_mode() {
        let no = |m, k, c, r| mouse_bytes(m, k, 0, c, r, false, false);
        assert_eq!(no(TermMode::empty(), MouseKind::Press, 5, 9), None, "no tracking: the mouse is ours");
        let sgr = TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE;
        assert_eq!(no(sgr, MouseKind::Press, 5, 9).unwrap(), b"\x1b[<0;6;10M".to_vec());
        assert_eq!(no(sgr, MouseKind::Drag, 5, 9).unwrap(), b"\x1b[<32;6;10M".to_vec(), "drag sets the motion bit");
        assert_eq!(no(sgr, MouseKind::Release, 5, 9).unwrap(), b"\x1b[<0;6;10m".to_vec(), "SGR says release with a lowercase m");
        // shift is the caller's business now: a drag the child owns is always delivered
        // click-only mode (1000) hears press and release but not motion
        let click = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert!(no(click, MouseKind::Press, 0, 0).is_some());
        assert_eq!(no(click, MouseKind::Drag, 0, 0), None);
        // X10: release is button 3, and there is no way to address past column 223
        let x10 = TermMode::MOUSE_REPORT_CLICK;
        assert_eq!(no(x10, MouseKind::Press, 5, 9).unwrap(), vec![0x1b, b'[', b'M', 32, 38, 42]);
        assert_eq!(no(x10, MouseKind::Release, 5, 9).unwrap(), vec![0x1b, b'[', b'M', 35, 38, 42]);
        assert_eq!(no(x10, MouseKind::Press, 300, 9).unwrap(), vec![0x1b, b'[', b'M', 32, 255, 42], "clamped, not dropped");
        // alt (8) + ctrl (16) ride along in the button code
        assert_eq!(mouse_bytes(sgr, MouseKind::Press, 0, 0, 0, true, true).unwrap(), b"\x1b[<24;1;1M".to_vec());
    }

    #[test]
    fn wheel_follows_terminal_mode() {
        assert_eq!(wheel_bytes(TermMode::empty(), 3, 0, 0, false), None, "plain terminal: our scrollback");
        assert_eq!(wheel_bytes(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL, 2, 0, 0, false).unwrap(), b"\x1b[A\x1b[A".to_vec());
        assert_eq!(wheel_bytes(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL | TermMode::APP_CURSOR, -1, 0, 0, false).unwrap(), b"\x1bOB".to_vec());
        let sgr = TermMode::MOUSE_MOTION | TermMode::SGR_MOUSE;
        assert_eq!(wheel_bytes(sgr, 1, 4, 9, false).unwrap(), b"\x1b[<64;5;10M".to_vec());
        assert_eq!(wheel_bytes(sgr, -1, 0, 0, false).unwrap(), b"\x1b[<65;1;1M".to_vec());
        assert_eq!(wheel_bytes(TermMode::MOUSE_REPORT_CLICK, 1, 0, 0, false).unwrap(), vec![0x1b, b'[', b'M', 96, 33, 33]);
        assert_eq!(wheel_bytes(sgr, 1, 0, 0, true), None, "shift always reaches the terminal's own scrollback");
    }
}

#[cfg(test)]
mod tests {
    use super::{platform_bytes, to_bytes};
    use alacritty_terminal::term::TermMode;
    use gpui::{Keystroke, Modifiers};

    fn ks(key: &str, ch: Option<&str>, m: Modifiers) -> Keystroke {
        Keystroke { modifiers: m, key: key.into(), key_char: ch.map(String::from) }
    }

    #[test]
    fn mapping() {
        let none = Modifiers::default();
        assert_eq!(to_bytes(&ks("a", Some("a"), none), TermMode::empty()).unwrap().as_ref(), b"a");
        assert_eq!(to_bytes(&ks("enter", None, none), TermMode::empty()).unwrap().as_ref(), b"\r");
        assert_eq!(to_bytes(&ks("up", None, none), TermMode::empty()).unwrap().as_ref(), b"\x1b[A");
        assert_eq!(to_bytes(&ks("up", None, none), TermMode::APP_CURSOR).unwrap().as_ref(), b"\x1bOA");
        let ctrl = Modifiers { control: true, ..Default::default() };
        assert_eq!(to_bytes(&ks("c", None, ctrl), TermMode::empty()).unwrap().as_ref(), b"\x03");
        let shift = Modifiers { shift: true, ..Default::default() };
        assert_eq!(to_bytes(&ks("up", None, shift), TermMode::empty()).unwrap().as_ref(), b"\x1b[1;2A");
        let cmd = Modifiers { platform: true, ..Default::default() };
        assert!(to_bytes(&ks("v", None, cmd), TermMode::empty()).is_none());
        let alt = Modifiers { alt: true, ..Default::default() };
        assert_eq!(to_bytes(&ks("left", None, alt), TermMode::empty()).unwrap().as_ref(), b"\x1bb");
        assert_eq!(to_bytes(&ks("right", None, alt), TermMode::empty()).unwrap().as_ref(), b"\x1bf");
    }

    #[test]
    fn command_chords_are_ghosttys() {
        let cmd = Modifiers { platform: true, ..Default::default() };
        assert_eq!(platform_bytes(&ks("left", None, cmd)), Some(&b"\x01"[..]));
        assert_eq!(platform_bytes(&ks("right", None, cmd)), Some(&b"\x05"[..]));
        assert_eq!(platform_bytes(&ks("backspace", None, cmd)), Some(&b"\x15"[..]));
        assert_eq!(platform_bytes(&ks("v", None, cmd)), None, "paste stays paste");
        let cmd_shift = Modifiers { platform: true, shift: true, ..Default::default() };
        assert_eq!(platform_bytes(&ks("left", None, cmd_shift)), None);
        assert_eq!(platform_bytes(&ks("left", None, Modifiers::default())), None);
    }
}
