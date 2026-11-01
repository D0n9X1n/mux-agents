//! Terminal text for the side bar and `list -Text` (#37): the color switches, the 16 named ANSI colors, display
//! columns (from the `unicode-width` crate) and cutting to a width on whole clusters.

use unicode_width::UnicodeWidthChar;

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

/// Columns of `text`, counted conservatively: the sum of `unicode-width`'s width of each character (UAX #11, not CJK:
/// Ambiguous is 1; combining marks, ZWJ and other zero-width characters 0), with a character followed by VS16
/// (U+FE0F) counted as at least 2. A ZWJ sequence is never collapsed: multiplexers differ on clustering (tmux 3.2a
/// draws each part), so the bar may cut early on one that clusters, never late. Agent text is cleaned first.
pub fn width(text: &str) -> usize {
    let mut chars = text.chars().peekable();
    let mut total = 0;
    while let Some(c) = chars.next() {
        let w = c.width().unwrap_or(0);
        total += if chars.peek() == Some(&'\u{fe0f}') {
            w.max(2)
        } else {
            w
        };
    }
    total
}

/// The byte ends of `text`'s clusters: a character with the zero-width ones after it, and a joiner with the character
/// it joins, so a cut never parts a mark, a selector or an emoji sequence from its base.
fn cluster_ends(text: &str) -> Vec<usize> {
    let mut ends = Vec::new();
    let mut prev: Option<char> = None;
    for (at, c) in text.char_indices() {
        let joins = c.width().unwrap_or(0) == 0 || prev == Some('\u{200d}');
        if prev.is_some() && !joins {
            ends.push(at);
        }
        prev = Some(c);
    }
    if !text.is_empty() {
        ends.push(text.len());
    }
    ends
}

/// The longest run of whole clusters at the start of `text` within `max` columns.
pub fn fit(text: &str, max: usize) -> &str {
    let mut kept = "";
    for end in cluster_ends(text) {
        if width(&text[..end]) > max {
            break;
        }
        kept = &text[..end];
    }
    kept
}

/// `text` within `max` columns: as is when it fits, else cut on whole clusters with `...` (or cut bare when `max` is
/// under 4).
pub fn cut(text: &str, max: usize) -> String {
    if width(text) <= max {
        return text.to_string();
    }
    let (budget, tail) = if max < 4 { (max, "") } else { (max - 3, "...") };
    format!("{}{tail}", fit(text, budget))
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
    fn widths_match_a_fixed_table() {
        // Columns written out by hand (UAX #11 and emoji presentation), never computed by the code under test.
        let table: &[(&str, usize)] = &[
            ("a", 1),
            ("abc", 3),
            ("\u{754c}", 2),
            ("\u{ac00}\u{ff21}\u{3042}", 6),
            // A vertical form and an emoji: two columns each.
            ("\u{fe10}", 2),
            ("\u{1f680}", 2),
            ("\u{1f600}", 2),
            // A text-default heart is 1; with VS16 it is an emoji, 2. A ZWJ sequence is counted part by part (2 + 0 + 2):
            // a multiplexer that does not cluster it (tmux 3.2a) draws each part, so the bar never under-counts.
            ("\u{2764}", 1),
            ("\u{2764}\u{fe0f}", 2),
            ("\u{1f469}\u{200d}\u{1f4bb}", 4),
            ("\u{2764}\u{fe0f}\u{200d}\u{1f525}", 4),
            // A combining mark and a zero-width space take no column.
            ("e\u{301}", 1),
            ("\u{301}", 0),
            ("\u{200b}", 0),
            // The bar's glyphs, and the Ambiguous `·` and `…`, are single width (not CJK).
            ("\u{25c9}\u{25cc}\u{2713}\u{2717}\u{2709}", 5),
            ("\u{b7}\u{2026}", 2),
        ];
        for &(text, want) in table {
            assert_eq!(width(text), want, "{text:?}");
        }
        assert_eq!(width(&"\u{fe10}".repeat(30)), 60);
        assert_eq!(width(&"\u{1f680}".repeat(30)), 60);
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
        // Two-column forms and emoji are cut whole, never split from their selector or joiner.
        assert_eq!(
            cut(&"\u{fe10}".repeat(30), 9),
            "\u{fe10}\u{fe10}\u{fe10}..."
        );
        assert_eq!(
            cut(&"\u{1f680}".repeat(30), 9),
            "\u{1f680}\u{1f680}\u{1f680}..."
        );
        assert_eq!(
            cut(&"\u{2764}\u{fe0f}".repeat(20), 8),
            "\u{2764}\u{fe0f}\u{2764}\u{fe0f}..."
        );
        let coder = "\u{1f469}\u{200d}\u{1f4bb}";
        // 4 columns each: one fits in 6, with `...`; a sequence is never split.
        assert_eq!(cut(&coder.repeat(10), 9), format!("{coder}..."));
        assert_eq!(width(&coder.repeat(16)), 64);
        assert_eq!(cut(&"a\u{200b}".repeat(10), 5), "a\u{200b}a\u{200b}...");
    }
}
