use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gpui::{Hsla, Rgba};

/// Catppuccin Mocha for a dark background (same palettes as plate).
pub const ANSI16_DARK: [(u8, u8, u8); 16] = [
    (0x1e, 0x1e, 0x2e), (0xf3, 0x8b, 0xa8), (0xa6, 0xe3, 0xa1), (0xf9, 0xe2, 0xaf),
    (0x89, 0xb4, 0xfa), (0xf5, 0xc2, 0xe7), (0x94, 0xe2, 0xd5), (0xba, 0xc2, 0xde),
    (0x58, 0x5b, 0x70), (0xf3, 0x8b, 0xa8), (0xa6, 0xe3, 0xa1), (0xf9, 0xe2, 0xaf),
    (0x89, 0xb4, 0xfa), (0xf5, 0xc2, 0xe7), (0x94, 0xe2, 0xd5), (0xa6, 0xad, 0xc8),
];

/// Catppuccin Latte: the same hues, dark enough to read on a light background.
pub const ANSI16_LIGHT: [(u8, u8, u8); 16] = [
    (0x5c, 0x5f, 0x77), (0xd2, 0x0f, 0x39), (0x40, 0xa0, 0x2b), (0xdf, 0x8e, 0x1d),
    (0x1e, 0x66, 0xf5), (0xea, 0x76, 0xcb), (0x17, 0x92, 0x99), (0xac, 0xb0, 0xbe),
    (0x6c, 0x6f, 0x85), (0xd2, 0x0f, 0x39), (0x40, 0xa0, 0x2b), (0xdf, 0x8e, 0x1d),
    (0x1e, 0x66, 0xf5), (0xea, 0x76, 0xcb), (0x17, 0x92, 0x99), (0xbc, 0xc0, 0xcc),
];

/// The palette follows the terminal's background, not the theme name, so the Mixed appearance
/// (light app, dark terminal) gets the dark set.
pub fn is_light(bg: Hsla) -> bool {
    bg.l > 0.5
}

pub fn rgb(r: u8, g: u8, b: u8) -> Hsla {
    Rgba { r: r as f32 / 255., g: g as f32 / 255., b: b as f32 / 255., a: 1. }.into()
}

fn from_rgb(c: Rgb) -> Hsla {
    rgb(c.r, c.g, c.b)
}

pub fn indexed(i: usize, light: bool) -> Hsla {
    match i {
        0..=15 => {
            let (r, g, b) = if light { ANSI16_LIGHT[i] } else { ANSI16_DARK[i] };
            rgb(r, g, b)
        }
        16..=231 => {
            let i = i - 16;
            let step = |v: usize| if v == 0 { 0 } else { (55 + 40 * v) as u8 };
            rgb(step(i / 36), step((i / 6) % 6), step(i % 6))
        }
        _ => {
            let v = (8 + 10 * (i.saturating_sub(232))) as u8;
            rgb(v, v, v)
        }
    }
}

/// Resolve a cell color; the terminal's own palette overrides (OSC 4/10/11) win when set.
pub fn resolve(c: &Color, palette: &Colors, default_fg: Hsla, default_bg: Hsla) -> Hsla {
    let light = is_light(default_bg);
    let indexed = |i| indexed(i, light);
    match c {
        Color::Spec(rgb) => from_rgb(*rgb),
        Color::Indexed(i) => palette[*i as usize].map(from_rgb).unwrap_or_else(|| indexed(*i as usize)),
        Color::Named(n) => {
            if let Some(o) = palette[*n] {
                return from_rgb(o);
            }
            match n {
                NamedColor::Foreground | NamedColor::BrightForeground => default_fg,
                NamedColor::DimForeground => {
                    let mut c = default_fg;
                    c.a = 0.7;
                    c
                }
                NamedColor::Background => default_bg,
                NamedColor::Cursor => default_fg,
                NamedColor::Black => indexed(0),
                NamedColor::Red => indexed(1),
                NamedColor::Green => indexed(2),
                NamedColor::Yellow => indexed(3),
                NamedColor::Blue => indexed(4),
                NamedColor::Magenta => indexed(5),
                NamedColor::Cyan => indexed(6),
                NamedColor::White => indexed(7),
                NamedColor::BrightBlack => indexed(8),
                NamedColor::BrightRed => indexed(9),
                NamedColor::BrightGreen => indexed(10),
                NamedColor::BrightYellow => indexed(11),
                NamedColor::BrightBlue => indexed(12),
                NamedColor::BrightMagenta => indexed(13),
                NamedColor::BrightCyan => indexed(14),
                NamedColor::BrightWhite => indexed(15),
                NamedColor::DimBlack => indexed(0),
                NamedColor::DimRed => indexed(1),
                NamedColor::DimGreen => indexed(2),
                NamedColor::DimYellow => indexed(3),
                NamedColor::DimBlue => indexed(4),
                NamedColor::DimMagenta => indexed(5),
                NamedColor::DimCyan => indexed(6),
                NamedColor::DimWhite => indexed(7),
            }
        }
    }
}

/// What an OSC 4/10/11/12 query reports for a color the child hasn't overridden: the color we
/// actually paint, so a program sniffing the background picks the right light/dark scheme.
pub fn query_reply(idx: usize, default_fg: Hsla, default_bg: Hsla) -> Rgb {
    let c = if idx == NamedColor::Background as usize {
        default_bg
    } else if idx < 256 {
        indexed(idx, is_light(default_bg))
    } else {
        default_fg
    };
    let c = c.to_rgb();
    let to8 = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    Rgb { r: to8(c.r), g: to8(c.g), b: to8(c.b) }
}

#[cfg(test)]
mod tests {
    use super::{indexed, query_reply, resolve, rgb};
    use alacritty_terminal::term::color::Colors;
    use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
    use gpui::Hsla;

    #[test]
    fn palette_follows_background_brightness() {
        let red = Color::Named(NamedColor::Red);
        let palette = Colors::default();
        assert_eq!(resolve(&red, &palette, Hsla::black(), Hsla::white()), rgb(0xd2, 0x0f, 0x39), "light bg gets Latte");
        assert_eq!(resolve(&red, &palette, Hsla::white(), Hsla::black()), rgb(0xf3, 0x8b, 0xa8), "dark bg gets Mocha");
        assert_eq!(indexed(196, true), indexed(196, false), "the 256-color cube is absolute");
    }

    #[test]
    fn queries_report_the_painted_defaults() {
        let (fg, bg) = (Hsla::black(), Hsla::white());
        assert_eq!(query_reply(NamedColor::Background as usize, fg, bg), Rgb { r: 255, g: 255, b: 255 });
        assert_eq!(query_reply(NamedColor::Foreground as usize, fg, bg), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(query_reply(1, fg, bg), Rgb { r: 0xd2, g: 0x0f, b: 0x39 });
    }
}
