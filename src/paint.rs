//! Terminal text for the side bar and `list -Text` (#37): the color switches, the 16 named ANSI colors, display
//! columns (wide East Asian characters 2, combining marks 0) and cutting to a width.

use crate::ctx::Env;

/// Whether to color: `NO_COLOR` non-empty → no; `FORCE_COLOR` or `CLICOLOR_FORCE` non-empty → yes; `TERM=dumb` → no;
/// else only on a terminal. An empty value counts as unset.
pub fn color_on(env: &Env, tty: bool) -> bool {
    let set = |name: &str| env.get(name).is_some_and(|v| !v.is_empty());
    if set("NO_COLOR") {
        return false;
    }
    if set("FORCE_COLOR") || set("CLICOLOR_FORCE") {
        return true;
    }
    !dumb(env) && tty
}

/// `TERM=dumb`: no cursor or mode controls.
pub fn dumb(env: &Env) -> bool {
    env.get("TERM") == Some("dumb")
}

/// `MUX_AGENTS_ASCII=1`: ASCII glyphs.
pub fn ascii(env: &Env) -> bool {
    env.get("MUX_AGENTS_ASCII") == Some("1")
}

/// Agent text made safe to draw (S2.9): C0 (LF included), DEL and C1 removed, before any escape is added.
pub fn clean(text: &str) -> String {
    text.chars()
        .filter(|&c| !(c < ' ' || ('\u{7f}'..='\u{9f}').contains(&c)))
        .collect()
}

/// An SGR style: bold, dim and one of the 16 named colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub bold: bool,
    pub dim: bool,
    pub color: Option<Color>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    BrightBlue,
    BrightMagenta,
}

/// `text` in `style` when `color`, else as is.
pub fn paint(text: &str, style: Style, color: bool) -> String {
    let mut codes = Vec::new();
    if style.bold {
        codes.push("1");
    }
    if style.dim {
        codes.push("2");
    }
    if let Some(c) = style.color {
        codes.push(match c {
            Color::Red => "31",
            Color::Green => "32",
            Color::Yellow => "33",
            Color::Blue => "34",
            Color::Magenta => "35",
            Color::Cyan => "36",
            Color::BrightBlue => "94",
            Color::BrightMagenta => "95",
        });
    }
    if !color || codes.is_empty() || text.is_empty() {
        return text.to_string();
    }
    format!("\u{1b}[{}m{text}\u{1b}[0m", codes.join(";"))
}

/// The identity hues, in palette order: never a state color.
pub const PALETTE: [&str; 4] = ["blue", "magenta", "bright-blue", "bright-magenta"];

/// A palette name's color; none for any other text.
pub fn hue(name: &str) -> Option<Color> {
    match name {
        "blue" => Some(Color::Blue),
        "magenta" => Some(Color::Magenta),
        "bright-blue" => Some(Color::BrightBlue),
        "bright-magenta" => Some(Color::BrightMagenta),
        _ => None,
    }
}

/// The palette name for an agent: its recorded `color` when that is a palette name, else FNV-1a of its name.
pub fn identity(recorded: Option<&str>, name: &str) -> &'static str {
    if let Some(found) = recorded.and_then(|r| PALETTE.iter().find(|p| **p == r)) {
        return found;
    }
    let mut hash: u32 = 0x811c_9dc5;
    for byte in name.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    PALETTE[(hash % PALETTE.len() as u32) as usize]
}

// Zero-width: combining marks, zero-width spaces and joiners, variation selectors.
const ZERO: &[(u32, u32)] = &[
    (0x0300, 0x036f),
    (0x0483, 0x0489),
    (0x0591, 0x05bd),
    (0x0610, 0x061a),
    (0x064b, 0x065f),
    (0x0e31, 0x0e31),
    (0x0e34, 0x0e3a),
    (0x0e47, 0x0e4e),
    (0x1ab0, 0x1aff),
    (0x1dc0, 0x1dff),
    (0x200b, 0x200f),
    (0x20d0, 0x20ff),
    (0xfe00, 0xfe0f),
    (0xfe20, 0xfe2f),
    (0xe0100, 0xe01ef),
];

// Two columns: East Asian Wide and Fullwidth blocks, and emoji.
const WIDE: &[(u32, u32)] = &[
    (0x1100, 0x115f),
    (0x2e80, 0x303e),
    (0x3041, 0x33ff),
    (0x3400, 0x4dbf),
    (0x4e00, 0x9fff),
    (0xa000, 0xa4cf),
    (0xac00, 0xd7a3),
    (0xf900, 0xfaff),
    (0xfe30, 0xfe4f),
    (0xff00, 0xff60),
    (0xffe0, 0xffe6),
    (0x1f300, 0x1f64f),
    (0x1f900, 0x1f9ff),
    (0x20000, 0x3fffd),
];

/// Columns of one character.
pub fn char_width(c: char) -> usize {
    let code = u32::from(c);
    let within = |table: &[(u32, u32)]| table.iter().any(|&(lo, hi)| (lo..=hi).contains(&code));
    if c < ' ' || within(ZERO) {
        0
    } else if within(WIDE) {
        2
    } else {
        1
    }
}

/// Columns of `text`.
pub fn width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

/// `text` within `max` columns: as is when it fits, else cut with `...` (or cut bare when `max` is under 4).
pub fn cut(text: &str, max: usize) -> String {
    if width(text) <= max {
        return text.to_string();
    }
    let (budget, tail) = if max < 4 { (max, "") } else { (max - 3, "...") };
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = char_width(c);
        if used + w > budget {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push_str(tail);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Env {
        Env::from_pairs(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())))
    }

    #[test]
    fn color_precedence_truth_table() {
        // (NO_COLOR, FORCE_COLOR, CLICOLOR_FORCE, TERM, tty) → color. None = unset; "" = set empty.
        // (NO_COLOR, FORCE_COLOR, CLICOLOR_FORCE, TERM, tty, want)
        type Case = (
            Option<&'static str>,
            Option<&'static str>,
            Option<&'static str>,
            Option<&'static str>,
            bool,
            bool,
        );
        let cases: &[Case] = &[
            (None, None, None, None, false, false),
            (None, None, None, None, true, true),
            (None, None, None, Some("xterm"), true, true),
            (None, None, None, Some("dumb"), true, false),
            (None, None, None, Some("dumb"), false, false),
            (Some("1"), None, None, None, true, false),
            (Some("1"), Some("1"), None, None, true, false),
            (Some("1"), None, Some("1"), None, false, false),
            (Some(""), None, None, None, true, true),
            (Some(""), None, None, None, false, false),
            (None, Some("1"), None, None, false, true),
            (None, None, Some("1"), None, false, true),
            (None, Some("1"), None, Some("dumb"), false, true),
            (None, None, Some("1"), Some("dumb"), true, true),
            (None, Some(""), None, None, false, false),
            (None, None, Some(""), None, false, false),
            (None, Some(""), Some(""), None, true, true),
            (Some(""), Some("1"), None, None, false, true),
            (Some(""), Some(""), None, Some("dumb"), true, false),
            (None, Some("0"), None, None, false, true),
        ];
        for &(no, force, cli, term, tty, want) in cases {
            let mut pairs = Vec::new();
            for (name, value) in [
                ("NO_COLOR", no),
                ("FORCE_COLOR", force),
                ("CLICOLOR_FORCE", cli),
                ("TERM", term),
            ] {
                if let Some(value) = value {
                    pairs.push((name, value));
                }
            }
            assert_eq!(color_on(&env(&pairs), tty), want, "{pairs:?} tty={tty}");
        }
    }

    #[test]
    fn dumb_and_ascii_switches() {
        assert!(dumb(&env(&[("TERM", "dumb")])));
        assert!(!dumb(&env(&[("TERM", "xterm-256color")])));
        assert!(!dumb(&env(&[])));
        assert!(ascii(&env(&[("MUX_AGENTS_ASCII", "1")])));
        assert!(!ascii(&env(&[("MUX_AGENTS_ASCII", "0")])));
        assert!(!ascii(&env(&[("MUX_AGENTS_ASCII", "")])));
        assert!(!ascii(&env(&[])));
    }

    #[test]
    fn paint_sgr_only_when_color() {
        let bold = Style {
            bold: true,
            ..Style::default()
        };
        let badge = Style {
            bold: true,
            color: Some(Color::Blue),
            ..Style::default()
        };
        let dim = Style {
            dim: true,
            ..Style::default()
        };
        assert_eq!(paint("x", bold, true), "\u{1b}[1mx\u{1b}[0m");
        assert_eq!(paint("x", badge, true), "\u{1b}[1;34mx\u{1b}[0m");
        assert_eq!(paint("x", dim, true), "\u{1b}[2mx\u{1b}[0m");
        assert_eq!(paint("x", badge, false), "x");
        assert_eq!(paint("x", Style::default(), true), "x");
        assert_eq!(paint("", badge, true), "");
        let codes: Vec<String> = [
            Color::Red,
            Color::Green,
            Color::Yellow,
            Color::Blue,
            Color::Magenta,
            Color::Cyan,
            Color::BrightBlue,
            Color::BrightMagenta,
        ]
        .into_iter()
        .map(|c| {
            paint(
                "x",
                Style {
                    color: Some(c),
                    ..Style::default()
                },
                true,
            )
        })
        .collect();
        assert_eq!(
            codes,
            ["31", "32", "33", "34", "35", "36", "94", "95"]
                .map(|c| format!("\u{1b}[{c}mx\u{1b}[0m"))
        );
    }

    #[test]
    fn identity_hues_and_stable_hash() {
        assert_eq!(
            PALETTE.map(hue),
            [
                Some(Color::Blue),
                Some(Color::Magenta),
                Some(Color::BrightBlue),
                Some(Color::BrightMagenta)
            ]
        );
        assert_eq!(hue("green"), None);
        assert_eq!(identity(Some("magenta"), "x"), "magenta");
        // A missing or foreign color is FNV-1a 32 of the name, modulo the palette.
        let fnv = |name: &str| {
            let mut h: u32 = 0x811c_9dc5;
            for b in name.bytes() {
                h ^= u32::from(b);
                h = h.wrapping_mul(0x0100_0193);
            }
            PALETTE[(h % 4) as usize]
        };
        for name in ["worker", "add-subtract", "test-calc", "a", ""] {
            assert_eq!(identity(None, name), fnv(name), "{name}");
            assert_eq!(identity(Some("red"), name), fnv(name), "{name}");
        }
        let all: std::collections::HashSet<&str> = (0..40)
            .map(|n| identity(None, &format!("agent-{n}")))
            .collect();
        assert_eq!(all.len(), 4);
    }

    #[test]
    fn widths_count_columns() {
        assert_eq!(width("abc"), 3);
        assert_eq!(width("\u{754c}\u{754c}"), 4);
        assert_eq!(width("e\u{301}"), 1);
        assert_eq!(width("\u{ac00}\u{ff21}\u{3042}"), 6);
        assert_eq!(width("\u{1f600}"), 2);
        // The bar's glyphs are single width.
        assert_eq!(width("\u{25c9}\u{25cc}\u{2713}\u{2717}\u{2709}"), 5);
        assert_eq!(width("\u{200b}\u{fe0f}\u{20dd}"), 0);
    }

    #[test]
    fn cut_by_columns() {
        assert_eq!(cut("hello", 5), "hello");
        assert_eq!(cut("hello world", 8), "hello...");
        assert_eq!(cut("hello", 3), "hel");
        assert_eq!(cut("hello", 0), "");
        let wide: String = "\u{754c}".repeat(30);
        for max in 0..=40 {
            let got = cut(&wide, max);
            assert!(width(&got) <= max, "{max}: {got}");
        }
        assert_eq!(cut(&wide, 8), "\u{754c}\u{754c}...");
        assert_eq!(cut(&wide, 9), "\u{754c}\u{754c}\u{754c}...");
        // A combining mark stays with its base; none is left dangling after a cut.
        let marks = "e\u{301}".repeat(30);
        assert_eq!(cut(&marks, 5), "e\u{301}e\u{301}...");
        assert_eq!(width(&cut(&marks, 5)), 5);
        assert_eq!(cut(&marks, 30), marks);
    }
}
