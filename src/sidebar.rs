//! The side bar (MuxAgents.ps1:1423-1591): `sidebar [-Close]` opens or closes one bar per session running
//! `<abs binary> watch`, and `watch` draws the session's board in it until its pane closes.

pub(crate) mod input;
mod keys;
mod pick;

use std::cmp::Reverse;
use std::io;
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value};

use crate::agents::board::{ENDED_MAX, board};
use crate::agents::spawn::{pane_env, utf8};
use crate::agents::view::{Focused, focus_hc};
use crate::agents::{lock, lock_key, pane_id, text};
use crate::caller::{Caller, resolve};
use crate::cli::Request;
use crate::ctx::{Ctx, Env};
use crate::error::{Res, refuse};
use crate::mux::{Output, native};
use crate::paint::{self, Color, Style};
use input::{Input, NoInput, Waited};
use keys::{Decoder, Event};
use pick::{Act, Pick, Target};

const TICK: Duration = Duration::from_millis(2000);
// How long a lone ESC may wait for the rest of a sequence before it counts as the Esc key.
const ESC_WAIT: Duration = Duration::from_millis(50);
// A minute of ticks with no answer from the multiplexer: a write to a pipe whose reader closed can still succeed.
const UNKNOWN_MAX: u32 = 30;
// The bar's own pane, its label and its size, read each tick (S2.1: the layout follows the actual pane).
const WATCH_PROBE: &str = "#{pane_id}|#{@agent-role}|#{pane_width}|#{pane_height}";
// The bar's own size, read again after the keeper moved or resized it, so the frame fits the pane as it is now.
const SIZE_FORMAT: &str = "#{pane_id}|#{pane_width}|#{pane_height}";
// What the bar's split answers: its pane, window and process (#47: the bar is that process, not its label).
const SPLIT_FORMAT: &str = "#{pane_id}|#{window_id}|#{pane_pid}";
// The proof read of the recorded pane: alive, and still the recorded process.
const PROOF_FORMAT: &str = "#{pane_id}|#{window_id}|#{pane_dead}|#{pane_pid}";
// The re-read after a conditional kill: a gone pane answers empty fields.
pub const GONE_FORMAT: &str = "#{pane_id}|#{pane_pid}";
// Each pane of the session and whether it carries the side-bar label, compared by the server: a label value with a
// newline or `|` cannot forge a line.
const LABEL_FORMAT: &str = "#{pane_id}|#{==:#{@agent-role},sidebar}";
// The bar is `@mux-agents-sidebar-percent` of its window, never narrower than `@mux-agents-sidebar-min` columns,
// both read where the bar's pane stands (pane, window, session, then `set -g`). Unset or not a whole number in
// range: 20% of the window, at least 40 columns.
const MIN_WIDTH: u32 = 40;
const PERCENT: u32 = 20;
#[cfg(test)]
const MIN_OPTION: &str = "@mux-agents-sidebar-min";
#[cfg(test)]
const PERCENT_OPTION: &str = "@mux-agents-sidebar-percent";
// The two options as format fields: the multiplexer replaces `|`, LF and CR in each value, so a value never adds
// a field or a row (it is then no number, and its default is used).
macro_rules! options {
    () => {
        "#{s/[|\n\r]/x/:@mux-agents-sidebar-min}|#{s/[|\n\r]/x/:@mux-agents-sidebar-percent}"
    };
}
// The caller's window width and the width options, read once by open.
const WINDOW_PROBE: &str = concat!("#{window_width}|", options!());
// The bar's window, one row per pane (S1.3), with the zoom flag (a zoomed window is left alone) and the width
// options as each pane reads them.
const LAYOUT: &str = concat!(
    "#{pane_id}|#{pane_active}|#{pane_at_right}|#{pane_at_top}|#{pane_at_bottom}|#{pane_width}|#{window_width}|",
    "#{pane_left}|#{pane_top}|#{window_zoomed_flag}|",
    options!()
);
// Repairs that may fail in a row before repair pauses, and how long a pause lasts when the layout does not change.
const REPAIR_TRIES: u32 = 3;
const REARM: Duration = Duration::from_secs(60);
// The bar's last line while repair is paused (drawn dim).
const PAUSED: &str = "layout: repair paused";
// `off` once `sidebar -Close` ran in the session; unset again by `sidebar`.
const OPTION: &str = "@mux-agents-sidebar";
// The session's bar as `<pane> <pid> <start>`, written only by `open` once the split's process is this binary
// `watch`, cleared by `sidebar -Close` once the bar is gone.
const RECORD: &str = "@mux-agents-sidebar-id";
// A new split's process may not have exec'd yet: it is read up to this many times, 50 ms apart.
const WATCH_TRIES: u32 = 20;

/// What `ensure` found: a bar it opened, the session's bar, or none because the session turned it off.
#[derive(Debug, PartialEq, Eq)]
pub enum Bar {
    Opened(String),
    Existing(String),
    Disabled,
}

/// The bar a spawn keeps open: under the session's sidebar lock, nothing when the session's option says `off`,
/// else the session's bar, found or opened beside the caller. A failed read or probe refuses and opens nothing.
pub fn ensure(ctx: &Ctx, caller: &Caller) -> Res<Bar> {
    lock(ctx, &lock_key("sidebar", caller, &caller.session), || {
        let read = native(
            ctx,
            &["show-options", "-qv", "-t", &caller.session, OPTION],
            false,
        )?;
        if read.stdout.trim_end_matches(['\r', '\n']) == "off" {
            return Ok(Bar::Disabled);
        }
        let (pane, _, opened) = find_or_open(ctx, caller)?;
        Ok(if opened {
            Bar::Opened(pane)
        } else {
            Bar::Existing(pane)
        })
    })
}

/// The session's bar as (pane, window, opened): the proven one, else a new one; refused while a pane of the session
/// carries the side-bar label without being the proven bar (a 0.2.1 bar, or a label rmux moved), so no second bar
/// opens beside one that cannot be proven.
fn find_or_open(ctx: &Ctx, caller: &Caller) -> Res<(String, String, bool)> {
    if let Some(bar) = prove(ctx, caller)? {
        return Ok((bar.pane, bar.window, false));
    }
    let labelled = labelled(ctx, caller)?;
    if !labelled.is_empty() {
        return refuse(1, "sidebar", unproven(&labelled));
    }
    let (pane, window) = open(ctx, caller)?;
    Ok((pane, window, true))
}

/// A board field made safe to draw: C0 (LF included), DEL and C1 removed, so no field moves the cursor or adds a line.
fn clean(value: Option<&Value>) -> String {
    paint::clean(&text(value))
}

/// Whole seconds as 59s, 59m, 23h or 1d; `?` when unknown.
pub fn format_age(seconds: Option<&Value>) -> String {
    let Some(s) = seconds.and_then(Value::as_i64) else {
        return "?".into();
    };
    match s {
        ..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

/// How the bar draws (S2.7, S2.8): SGR color, ASCII glyphs, and no cursor or mode controls on a dumb terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Look {
    pub color: bool,
    pub ascii: bool,
    pub dumb: bool,
}

impl Look {
    /// The switches from the environment, for output that is a terminal when `tty`.
    pub fn from_env(env: &Env, tty: bool) -> Look {
        Look {
            color: paint::color_on(env, tty),
            ascii: paint::ascii(env),
            dumb: paint::dumb(env),
        }
    }
}

/// The bar's pane in columns and rows, read each tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub width: usize,
    pub height: usize,
}

// Until the pane answers its size: the bar's default narrowest width and a common height.
const FALLBACK: Size = Size {
    width: MIN_WIDTH as usize,
    height: 24,
};
// Ended agents drawn before `+K more`.
const ENDED_SHOWN: usize = 3;
const PLAIN: Style = Style {
    bold: false,
    dim: false,
    reverse: false,
    color: None,
};
const BOLD: Style = Style {
    bold: true,
    ..PLAIN
};
const DIM: Style = Style { dim: true, ..PLAIN };
const BADGE: Style = Style {
    bold: true,
    color: Some(Color::Blue),
    ..PLAIN
};
const MAIL: &str = "\u{2709}";
const HINT: &str = "Enter: focus  Esc: clear";

/// Agent-supplied text as drawn: '◉', '◌' and '✦' become '•' (all one column under unicode-width 0.2.3), so only the
/// bar's own glyph starts a name line.
fn own_glyphs(text: &str) -> String {
    text.replace(['\u{25c9}', '\u{25cc}', '\u{2726}'], "\u{2022}")
}

const fn hued(color: Color) -> Style {
    Style {
        color: Some(color),
        ..PLAIN
    }
}

// Cleaned text in one style; a row is its spans, an empty row a blank line.
type Span = (String, Style);
type Row = Vec<Span>;

fn span(text: impl Into<String>, style: Style) -> Span {
    (text.into(), style)
}

fn row_width(row: &[Span]) -> usize {
    row.iter().map(|(text, _)| paint::width(text)).sum()
}

/// `row` within `max` columns: whole when it fits, else cut with `...` in the style of the span it cuts (bare under
/// 4 columns).
fn cut_row(row: Row, max: usize) -> Row {
    if row_width(&row) <= max {
        return row;
    }
    let (budget, tail) = if max < 4 { (max, "") } else { (max - 3, "...") };
    let mut out = Vec::new();
    let mut used = 0;
    for (text, style) in row {
        let w = paint::width(&text);
        if used + w <= budget {
            used += w;
            out.push((text, style));
            continue;
        }
        let kept = paint::fit(&text, budget - used);
        out.push((format!("{kept}{tail}"), style));
        return out;
    }
    out
}

/// A row as text, with SGR only when `color`; agent text was cleaned before it became a span.
fn render(row: &[Span], color: bool) -> String {
    row.iter()
        .map(|(text, style)| paint::paint(text, *style, color))
        .collect()
}

/// An agent's state as the bar draws it (S2.3): a live agent's activity as herdr names it, then the ended kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Blocked,
    Done,
    Working,
    Idle,
    Finished,
    Exit,
    Stopped,
    Other,
}

impl Kind {
    /// A live kind's section heading, as herdr names them.
    fn section(self) -> &'static str {
        match self {
            Kind::Blocked => "Needs you",
            Kind::Done => "Done",
            Kind::Idle => "Idle",
            _ => "Working",
        }
    }

    /// A live row's kind: its `activity`, else (a feed without it) `waiting`.
    fn live(row: &Map<String, Value>) -> Kind {
        match row.get("activity").and_then(Value::as_str) {
            Some("blocked") => Kind::Blocked,
            Some("done") => Kind::Done,
            Some("idle") => Kind::Idle,
            Some(_) => Kind::Working,
            None if row.get("waiting") == Some(&Value::Bool(true)) => Kind::Idle,
            None => Kind::Working,
        }
    }

    /// Single-width glyphs (UAX #11 Neutral), or ASCII; glyph and word tell every kind apart without color.
    fn glyph(self, ascii: bool) -> &'static str {
        match (self, ascii) {
            (Kind::Blocked, _) => "!",
            (Kind::Done, false) => "\u{2726}",
            (Kind::Done, true) => "@",
            (Kind::Working, false) => "\u{25c9}",
            (Kind::Working, true) => "*",
            (Kind::Idle, false) => "\u{25cc}",
            (Kind::Idle, true) => "o",
            (Kind::Finished, false) => "\u{2713}",
            (Kind::Finished, true) => "+",
            (Kind::Exit, false) => "\u{2717}",
            (Kind::Exit, true) => "x",
            (Kind::Stopped | Kind::Other, _) => "-",
        }
    }

    /// A live kind's word.
    fn word(self) -> &'static str {
        match self {
            Kind::Blocked => "needs you",
            Kind::Done => "done",
            Kind::Idle => "idle",
            _ => "working",
        }
    }

    fn style(self) -> Style {
        match self {
            Kind::Blocked => Style {
                bold: true,
                ..hued(Color::Yellow)
            },
            Kind::Done => Style {
                bold: true,
                ..hued(Color::Cyan)
            },
            Kind::Working => hued(Color::Green),
            Kind::Idle | Kind::Finished | Kind::Other => DIM,
            Kind::Exit => hued(Color::Red),
            Kind::Stopped => hued(Color::Yellow),
        }
    }

    /// The word's style: needs you is reversed, herdr's pill.
    fn word_style(self) -> Style {
        Style {
            reverse: self == Kind::Blocked,
            ..self.style()
        }
    }
}

/// The right side of an agent's line: its state word, then its age right-aligned.
fn right(kind: Kind, word: &str, age: &str, muted: bool) -> Row {
    let age_style = if muted { DIM } else { PLAIN };
    vec![
        span(word, kind.word_style()),
        span(format!(" {age:>4}"), age_style),
    ]
}

/// One agent's line in `cols` columns: `<glyph> <name><model><badge>` left, `right` right-aligned. Short of room the
/// model goes first, then the name is cut; with no room for the right side the left is cut alone.
fn agent_line(glyph: Span, name: Span, model: Row, badge: Row, right: Row, cols: usize) -> Row {
    let fixed = paint::width(&glyph.0) + 1 + row_width(&badge);
    let right_w = row_width(&right);
    let assemble = |name: Span, model: &[Span]| -> Row {
        let used = fixed + paint::width(&name.0) + row_width(model) + right_w;
        let mut row = vec![glyph.clone(), span(" ", PLAIN), name];
        row.extend(model.iter().cloned());
        row.extend(badge.iter().cloned());
        row.push(span(" ".repeat(cols - used), PLAIN));
        row.extend(right.iter().cloned());
        row
    };
    let name_w = paint::width(&name.0);
    if fixed + name_w + row_width(&model) + 2 + right_w <= cols {
        return assemble(name, &model);
    }
    if fixed + name_w + 2 + right_w <= cols {
        return assemble(name, &[]);
    }
    if fixed + 2 + right_w + 4 <= cols {
        let room = cols - fixed - 2 - right_w;
        return assemble((paint::cut(&name.0, room), name.1), &[]);
    }
    let mut row = vec![glyph, span(" ", PLAIN), name];
    row.extend(badge);
    cut_row(row, cols)
}

/// The one-column margin every content row starts with.
fn margin(mut row: Row) -> Row {
    row.insert(0, span(" ", PLAIN));
    row
}

/// A dim line under an agent's name: its doing line, `+K more`, `+J more running`.
fn indented(text: &str, cols: usize) -> Row {
    vec![span("   ", PLAIN), span(paint::cut(text, cols - 2), DIM)]
}

/// One live agent: its line, its doing line, its age (the oldest drop first) and what focusing it needs.
struct Live {
    kind: Kind,
    line: Row,
    // The line under a section heading, as herdr's compact status: the glyph and the time, no word (the heading
    // names the state), so the name keeps the room.
    compact: Row,
    doing: Option<Row>,
    age: Option<i64>,
    target: Target,
}

/// The rows as drawn, and each drawn running agent with its name line's row, top to bottom.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Laid {
    pub rows: Vec<Row>,
    pub agents: Vec<(Target, usize)>,
}

/// The drawn running agents, top to bottom (for keys).
fn targets(laid: &Laid) -> Vec<Target> {
    laid.agents
        .iter()
        .map(|(target, _)| target.clone())
        .collect()
}

/// The rows a board can draw, before the height decides which are kept.
struct Parts {
    // Between a section's name and its count: `·`, or `-` in ASCII.
    dot: &'static str,
    header: Row,
    live: Vec<Live>,
    ended: Vec<Row>,
    // Ended agents not among `ended` (lost ones included).
    ended_more: usize,
    // Whether the session has ended agents: the separator.
    ended_any: bool,
    nothing: bool,
    notes: Vec<Row>,
}

/// Which parts are kept (S2.1's overflow order, in reverse).
struct Shape {
    gaps: usize,
    // herdr's section headings above each state's first agent, when the live agents are in more than one state.
    sections: bool,
    doing: Vec<bool>,
    shown: Vec<bool>,
    live_more: bool,
    ended: usize,
    folded: bool,
    sep: bool,
    nothing: bool,
    notes: usize,
}

fn live_row(row: &Map<String, Value>, cols: usize, look: Look) -> Live {
    let kind = Kind::live(row);
    // An idle agent's whole row is muted, as herdr mutes it; its name keeps its hue.
    let muted = kind == Kind::Idle;
    let name = clean(row.get("name"));
    let hue = paint::hue(paint::identity(
        row.get("color").and_then(Value::as_str),
        &name,
    ));
    let model = own_glyphs(&clean(row.get("model")));
    let model = if model.is_empty() {
        Vec::new()
    } else {
        vec![span(format!(" [{model}]"), DIM)]
    };
    let unread = row.get("unread").and_then(Value::as_u64).unwrap_or(0);
    let badge = if unread > 0 {
        let mail = if look.ascii { "m" } else { MAIL };
        vec![span("  ", PLAIN), span(format!("{mail} {unread}"), BADGE)]
    } else {
        Vec::new()
    };
    // The time since the activity last changed, else since the agent started.
    let since = row
        .get("since")
        .filter(|since| !since.is_null())
        .or_else(|| row.get("age"));
    let glyph = span(kind.glyph(look.ascii), kind.style());
    let name_span = span(
        own_glyphs(&name),
        Style {
            color: hue,
            dim: muted,
            ..PLAIN
        },
    );
    let age = format_age(since);
    let line = agent_line(
        glyph.clone(),
        name_span.clone(),
        model.clone(),
        badge.clone(),
        right(kind, kind.word(), &age, muted),
        cols,
    );
    let age_style = if muted { DIM } else { PLAIN };
    let compact = agent_line(
        glyph,
        name_span,
        model,
        badge,
        vec![span(format!("{age:>4}"), age_style)],
        cols,
    );
    let doing = own_glyphs(&clean(row.get("doing")));
    let doing = doing.trim();
    Live {
        kind,
        line: margin(line),
        compact: margin(compact),
        doing: (!doing.is_empty()).then(|| indented(doing, cols)),
        age: row.get("age").and_then(Value::as_i64),
        target: Target {
            name,
            pane: clean(row.get("pane")),
        },
    }
}

/// `mux-agents` bold, then the counts right-aligned when they fit, as herdr counts its sections: `N need you`
/// (yellow), `N done`, `N working`, `N idle`, each only when not 0 (`0 running` when no agent lives), then `M ended`.
/// Short of room the counts lose their last part, then the next, then the title stands alone.
fn header(parts: &[(String, Style)], cols: usize) -> Row {
    let title = span("mux-agents", BOLD);
    let shown = |n: usize| -> Row {
        let mut row = Vec::new();
        for (at, (text, style)) in parts[..n].iter().enumerate() {
            if at > 0 {
                row.push(span(" / ", PLAIN));
            }
            row.push(span(text.clone(), *style));
        }
        row
    };
    let Some(counts) = (1..=parts.len())
        .rev()
        .map(shown)
        .find(|row| 10 + 2 + row_width(row) <= cols)
    else {
        return margin(vec![title]);
    };
    let width = row_width(&counts);
    let mut row = vec![title, span(" ".repeat(cols - 10 - width), PLAIN)];
    row.extend(counts);
    margin(row)
}

fn parts(
    board: Option<&Map<String, Value>>,
    notes: &[(&str, Style)],
    cols: usize,
    look: Look,
) -> Parts {
    let notes = notes
        .iter()
        .map(|(note, style)| margin(vec![span(paint::cut(&own_glyphs(note), cols), *style)]))
        .collect();
    let Some(board) = board else {
        return Parts {
            dot: "",
            header: header(&[("? running".into(), PLAIN)], cols),
            live: Vec::new(),
            ended: Vec::new(),
            ended_more: 0,
            ended_any: false,
            nothing: false,
            notes,
        };
    };
    let rows = board.get("agents").and_then(Value::as_array);
    let (mut live, mut ended, mut counted) = (Vec::new(), Vec::new(), 0);
    // Live agents by kind, in herdr's order: needs you, done, working, idle.
    let mut kinds = [0usize; 4];
    for row in rows.map_or(&[][..], Vec::as_slice) {
        let Some(row) = row.as_object() else {
            continue;
        };
        // Every printed field, state included, is cleaned first.
        let state = clean(row.get("state"));
        if state == "running" {
            let at = match Kind::live(row) {
                Kind::Blocked => 0,
                Kind::Done => 1,
                Kind::Working => 2,
                _ => 3,
            };
            kinds[at] += 1;
            live.push(live_row(row, cols, look));
            continue;
        }
        counted += 1;
        let (kind, word) = match state.as_str() {
            // A lost agent's pane is gone: counted, never drawn.
            "lost" => continue,
            "exited" => match row.get("exitCode").filter(|code| !code.is_null()) {
                Some(code) if clean(Some(code)) == "0" => (Kind::Finished, "done".to_string()),
                Some(code) => (Kind::Exit, format!("exit {}", clean(Some(code)))),
                None => (Kind::Exit, "exit ?".to_string()),
            },
            "stopped" => (Kind::Stopped, state.clone()),
            _ => (Kind::Other, state.clone()),
        };
        let line = agent_line(
            span(kind.glyph(look.ascii), kind.style()),
            span(own_glyphs(&clean(row.get("name"))), DIM),
            Vec::new(),
            Vec::new(),
            right(kind, &word, &format_age(row.get("age")), false),
            cols,
        );
        ended.push(margin(line));
    }
    let total = board
        .get("endedTotal")
        .and_then(Value::as_u64)
        .map_or(counted, |total| total as usize);
    ended.truncate(ENDED_SHOWN);
    let words = ["need you", "done", "working", "idle"];
    let styles = [hued(Color::Yellow), PLAIN, PLAIN, PLAIN];
    let mut counts: Vec<(String, Style)> = (0..4)
        .filter(|&at| kinds[at] > 0)
        .map(|at| (format!("{} {}", kinds[at], words[at]), styles[at]))
        .collect();
    if counts.is_empty() {
        counts.push(("0 running".into(), PLAIN));
    }
    counts.push((format!("{total} ended"), PLAIN));
    Parts {
        dot: if look.ascii { "-" } else { "\u{b7}" },
        header: header(&counts, cols),
        nothing: live.is_empty(),
        live,
        ended_more: total.saturating_sub(ended.len()),
        ended,
        ended_any: total > 0,
        notes,
    }
}

/// The kept parts as rows: blocks (header, each live agent, `+J more running`, `No agents running.`, the ended
/// section, the notes) with a blank line between the first `gaps` pairs of them; each shown live agent with its row.
fn assemble(parts: &Parts, shape: &Shape, cols: usize) -> Laid {
    let mut blocks: Vec<Vec<Row>> = vec![vec![parts.header.clone()]];
    // The block each shown live agent heads, and its name line's row within the block.
    let mut heads = Vec::new();
    let mut section = None;
    for (at, live) in parts.live.iter().enumerate() {
        if !shape.shown[at] {
            continue;
        }
        let mut block = Vec::new();
        // The first shown agent of a state brings its heading, counting the state's live agents.
        if shape.sections && section != Some(live.kind) {
            section = Some(live.kind);
            let count = parts.live.iter().filter(|l| l.kind == live.kind).count();
            // The name in the state's color (bold; idle dim), the count dim, as herdr draws them.
            let title = Style {
                bold: true,
                dim: live.kind == Kind::Idle,
                ..live.kind.style()
            };
            let heading = vec![
                span(live.kind.section(), title),
                span(format!(" {} {count}", parts.dot), DIM),
            ];
            block.push(margin(cut_row(heading, cols)));
        }
        let line_at = block.len();
        block.push(if shape.sections {
            live.compact.clone()
        } else {
            live.line.clone()
        });
        if let Some(doing) = live.doing.as_ref().filter(|_| shape.doing[at]) {
            block.push(doing.clone());
        }
        heads.push((blocks.len(), line_at, &live.target));
        blocks.push(block);
    }
    let hidden = shape.shown.iter().filter(|shown| !**shown).count();
    if hidden > 0 && shape.live_more {
        blocks.push(vec![indented(&format!("+{hidden} more running"), cols)]);
    }
    if shape.nothing {
        blocks.push(vec![margin(vec![span(
            paint::cut("No agents running.", cols),
            DIM,
        )])]);
    }
    let mut ended = Vec::new();
    if shape.sep {
        ended.push(margin(vec![span(
            format!("ended {}", "-".repeat(cols - 7)),
            DIM,
        )]));
    }
    ended.extend(parts.ended[..shape.ended].iter().cloned());
    let more = parts.ended_more + parts.ended.len() - shape.ended;
    if more > 0 && shape.folded {
        // Only promise `list -All` when it shows every ended agent: it shows the newest ENDED_MAX.
        let text = if more + shape.ended <= ENDED_MAX {
            format!("+{more} more  (list -All)")
        } else {
            format!("+{more} more ended")
        };
        ended.push(indented(&text, cols));
    }
    if !ended.is_empty() {
        blocks.push(ended);
    }
    if shape.notes > 0 {
        blocks.push(parts.notes[..shape.notes].to_vec());
    }
    let mut rows = Vec::new();
    let mut agents = Vec::new();
    for (at, block) in blocks.into_iter().enumerate() {
        if at > 0 && at <= shape.gaps {
            rows.push(Vec::new());
        }
        if let Some((_, line_at, target)) = heads.iter().find(|(head, _, _)| *head == at) {
            agents.push(((*target).clone(), rows.len() + line_at));
        }
        rows.extend(block);
    }
    Laid { rows, agents }
}

/// Drops the next part in S2.1's overflow order: blank lines (the lowest first), the section headings, ended rows
/// (folded into `+K more`),
/// the separator, doing lines of the oldest live agents, whole oldest live agents (into `+J more running`), then the
/// summaries, `No agents running.` and the notes. False when only the header is left.
fn drop_one(parts: &Parts, shape: &mut Shape) -> bool {
    if shape.gaps > 0 {
        shape.gaps -= 1;
        return true;
    }
    if shape.sections {
        shape.sections = false;
        return true;
    }
    if shape.ended > 0 {
        shape.ended -= 1;
        return true;
    }
    if shape.sep {
        shape.sep = false;
        return true;
    }
    // The oldest of those still flagged: an unknown age is oldest, a tie goes to the later pane.
    let oldest = |flags: &[bool]| {
        (0..flags.len())
            .filter(|&at| flags[at])
            .max_by_key(|&at| (parts.live[at].age.unwrap_or(i64::MAX), at))
    };
    if let Some(at) = oldest(&shape.doing) {
        shape.doing[at] = false;
        return true;
    }
    if let Some(at) = oldest(&shape.shown) {
        shape.shown[at] = false;
        return true;
    }
    if shape.folded && parts.ended_more + parts.ended.len() > shape.ended {
        shape.folded = false;
        return true;
    }
    if shape.live_more && shape.shown.iter().any(|shown| !shown) {
        shape.live_more = false;
        return true;
    }
    if shape.nothing {
        shape.nothing = false;
        return true;
    }
    if shape.notes > 0 {
        shape.notes -= 1;
        return true;
    }
    false
}

/// The board's rows at the bar's actual size (S2.1): at most `height` rows, each at most `width` display columns
/// (one column is left free at the right, so no row ends in a pending wrap). Under 12 columns, the header only.
/// `None` is a board never read: the header says `? running`.
fn layout(board: Option<&Map<String, Value>>, notes: &[&str], size: Size, look: Look) -> Vec<Row> {
    let notes: Vec<(&str, Style)> = notes.iter().map(|note| (*note, DIM)).collect();
    lay(board, &notes, size, look).rows
}

/// `layout` with each note in its own style, and the drawn running agents with their rows.
fn lay(
    board: Option<&Map<String, Value>>,
    notes: &[(&str, Style)],
    size: Size,
    look: Look,
) -> Laid {
    lay_notes(board, notes, size, look).0
}

/// `lay`, and how many of the notes it kept.
fn lay_notes(
    board: Option<&Map<String, Value>>,
    notes: &[(&str, Style)],
    size: Size,
    look: Look,
) -> (Laid, usize) {
    if size.width < 12 {
        let max = size.width.saturating_sub(1).max(1);
        let laid = Laid {
            rows: vec![cut_row(vec![span("mux-agents", BOLD)], max)],
            agents: Vec::new(),
        };
        return (laid, 0);
    }
    let cols = size.width - 2;
    let parts = parts(board, notes, cols, look);
    let first = parts.live.first().map(|live| live.kind);
    let mut shape = Shape {
        gaps: 0,
        sections: parts.live.iter().any(|live| Some(live.kind) != first),
        doing: parts.live.iter().map(|live| live.doing.is_some()).collect(),
        shown: vec![true; parts.live.len()],
        live_more: true,
        ended: parts.ended.len(),
        folded: true,
        sep: parts.ended_any,
        nothing: parts.nothing,
        notes: parts.notes.len(),
    };
    shape.gaps = usize::MAX;
    shape.gaps = assemble(&parts, &shape, cols)
        .rows
        .iter()
        .filter(|row| row.is_empty())
        .count();
    loop {
        let mut laid = assemble(&parts, &shape, cols);
        if laid.rows.len() <= size.height || !drop_one(&parts, &mut shape) {
            laid.rows.truncate(size.height.max(1));
            let kept = laid.rows.len();
            laid.agents.retain(|(_, row)| *row < kept);
            return (laid, shape.notes);
        }
    }
}

/// `lay` with `HINT` as the last note, only when it costs nothing else: the hinted rows are exactly the bare rows,
/// then the hint's own rows (the hint alone when the bare rows end in notes, else a blank row and the hint).
/// Otherwise the bare layout, so the hint goes before any other row or gap.
fn with_hint(
    board: Option<&Map<String, Value>>,
    notes: &[(&str, Style)],
    size: Size,
    look: Look,
) -> Laid {
    let (bare, kept) = lay_notes(board, notes, size, look);
    let mut hinted_notes = notes.to_vec();
    hinted_notes.push((HINT, DIM));
    let hinted = lay(board, &hinted_notes, size, look);
    let hint = margin(vec![span(
        paint::cut(HINT, size.width.saturating_sub(2)),
        DIM,
    )]);
    let own = if kept > 0 {
        vec![hint]
    } else {
        vec![Row::new(), hint]
    };
    let n = bare.rows.len();
    let fits = hinted.rows.len() == n + own.len()
        && hinted.rows[..n] == bare.rows[..]
        && hinted.rows[n..] == own[..];
    if fits { hinted } else { bare }
}

/// Marks `name`'s drawn row as selected: its margin becomes `▸` (`>` in ASCII) and, with color, the rest of the row
/// is reversed. Nothing when `name` is not drawn.
fn mark(laid: &mut Laid, name: &str, look: Look) {
    let Some(&(_, at)) = laid.agents.iter().find(|(target, _)| target.name == name) else {
        return;
    };
    let row = &mut laid.rows[at];
    row[0] = span(if look.ascii { ">" } else { "\u{25b8}" }, PLAIN);
    if look.color {
        for (_, style) in &mut row[1..] {
            style.reverse = true;
        }
    }
}

/// The board as text rows, SGR only when `look.color`; for tests and screenshots.
pub fn format_board(
    board: Option<&Map<String, Value>>,
    notes: &[&str],
    size: Size,
    look: Look,
) -> Vec<String> {
    layout(board, notes, size, look)
        .iter()
        .map(|row| render(row, look.color))
        .collect()
}

/// One frame (S2.8): synchronized output on, cursor home, each row then erase-to-end-of-line (only a row narrower
/// than the pane has anything to erase), erase-below, synchronized output off; no full clear per tick. The cursor
/// never moves below the last row, so the pane never scrolls. With `TERM=dumb`: the rows and a blank line, with no
/// cursor or mode control (SGR still follows the color switches).
fn frame(rows: &[Row], size: Size, look: Look) -> String {
    let lines: Vec<String> = rows.iter().map(|row| render(row, look.color)).collect();
    if look.dumb {
        return format!("{}\r\n\r\n", lines.join("\r\n"));
    }
    let mut out = String::from("\u{1b}[?2026h\u{1b}[H");
    for (at, (row, line)) in rows.iter().zip(&lines).enumerate() {
        if at > 0 {
            out.push_str("\r\n");
        }
        out.push_str(line);
        if row_width(row) < size.width {
            out.push_str("\u{1b}[K");
        }
    }
    let last_full = rows.last().is_some_and(|row| row_width(row) >= size.width);
    if rows.len() < size.height {
        out.push_str("\r\n\u{1b}[J");
    } else if !last_full {
        out.push_str("\u{1b}[J");
    }
    out.push_str("\u{1b}[?2026l");
    out
}

/// A gone pane answers exit 0 and empty fields; anything else is no proof.
fn gone(probe: &Output) -> bool {
    probe.code == 0 && probe.stdout.trim_end_matches(['\r', '\n']) == "|"
}

/// One pane of the bar's window, as LAYOUT reads it.
#[derive(Debug, PartialEq, Eq)]
struct Pane {
    id: String,
    active: bool,
    at_right: bool,
    at_top: bool,
    at_bottom: bool,
    width: u32,
    left: u32,
    top: u32,
    // The width options as this pane reads them.
    sizing: Sizing,
}

/// The bar's window: its width, whether a pane is zoomed, and its panes.
#[derive(Debug)]
struct Layout {
    width: u32,
    zoomed: bool,
    panes: Vec<Pane>,
}

/// The LAYOUT rows, or none unless every row is exactly twelve fields, the first ten well-formed (a pane id, 0/1
/// flags, decimal numbers) and agreeing on the window's width and zoom, with at least one row. The last two are the
/// width options, each its default when it is no number in range. One final LF ends the answer.
fn parse_layout(text: &str) -> Option<Layout> {
    let flag = |f: &str| match f {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    };
    let number = |f: &str| {
        (!f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
            .then(|| f.parse::<u32>().ok())
            .flatten()
    };
    let body = text.strip_suffix('\n').unwrap_or(text);
    let mut layout: Option<Layout> = None;
    for line in body.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let f: Vec<&str> = line.split('|').collect();
        if f.len() != 12 || !pane_id(f[0]) {
            return None;
        }
        let (width, zoomed) = (number(f[6])?, flag(f[9])?);
        let pane = Pane {
            id: f[0].to_string(),
            active: flag(f[1])?,
            at_right: flag(f[2])?,
            at_top: flag(f[3])?,
            at_bottom: flag(f[4])?,
            width: number(f[5])?,
            left: number(f[7])?,
            top: number(f[8])?,
            sizing: Sizing::read(f[10], f[11]),
        };
        let layout = layout.get_or_insert_with(|| Layout {
            width,
            zoomed,
            panes: Vec::new(),
        });
        if (layout.width, layout.zoomed) != (width, zoomed) {
            return None;
        }
        layout.panes.push(pane);
    }
    layout
}

/// The pane the bar is moved against (S1.4): of the other panes at the right edge, the topmost; else the other pane
/// reaching furthest right (the topmost of those). Never the bar; none when the bar is the window's only pane.
fn move_target<'a>(panes: &'a [Pane], bar: &str) -> Option<&'a str> {
    let others = || panes.iter().filter(|p| p.id != bar);
    others()
        .filter(|p| p.at_right)
        .min_by_key(|p| p.top)
        .or_else(|| others().max_by_key(|p| (p.left + p.width, Reverse(p.top))))
        .map(|p| p.id.as_str())
}

/// The window's layout as the set of `pane,left,top,width`: a paused repair re-arms when it changes.
fn signature(panes: &[Pane]) -> String {
    let mut rows: Vec<String> = panes
        .iter()
        .map(|p| format!("{},{},{},{}", p.id, p.left, p.top, p.width))
        .collect();
    rows.sort();
    rows.join(";")
}

/// The right-edge lock of one `watch` (S1), kept in memory only: the bar stays at the right edge of its window, full
/// height, `bar_width` columns, and the active pane never changes.
#[derive(Default)]
struct Keeper {
    // The window width the bar's width was last set for, and that width; a drag is kept until either changes.
    applied: Option<(u32, u32)>,
    // Repairs that failed in a row: a refused move, or a move the read right after it found did not hold.
    failures: u32,
    // While repair is paused: the layout it paused on and when.
    paused: Option<(String, SystemTime)>,
    // Whether this tick resized or moved the bar: its size is then read again before the frame is laid out.
    acted: bool,
}

impl Keeper {
    fn paused(&self) -> bool {
        self.paused.is_some()
    }

    /// The bar's width as last set, when one was.
    fn width(&self) -> Option<usize> {
        self.applied.map(|(_, width)| width as usize)
    }

    /// One read of the bar's window. A failed or malformed read, or one without the bar, does nothing; so does a
    /// zoomed window, since resizing a pane unzooms it. A pause re-arms when the layout changed or 60 s passed. A bar
    /// in place gets its width when the window's width, or the width its options give, differs from the last one
    /// applied; a bar out of place is repaired unless paused. True when it resized or moved the bar.
    fn tick(&mut self, ctx: &Ctx, bar: &str) -> bool {
        self.acted = false;
        self.check(ctx, bar);
        self.acted
    }

    fn check(&mut self, ctx: &Ctx, bar: &str) {
        let Some(layout) = read_layout(ctx, bar) else {
            return;
        };
        let Some(me) = layout.panes.iter().find(|p| p.id == bar) else {
            return;
        };
        if layout.zoomed {
            return;
        }
        let shape = signature(&layout.panes);
        let now = ctx.clock.now();
        if let Some((paused_on, at)) = &self.paused {
            let waited = now.duration_since(*at).unwrap_or_default();
            if *paused_on != shape || waited >= REARM {
                self.paused = None;
                self.failures = 0;
            }
        }
        if in_place(me) {
            (self.failures, self.paused) = (0, None);
            let want = (layout.width, bar_width(layout.width, me.sizing));
            if self.applied != Some(want) {
                self.resize(ctx, bar, want);
            }
            return;
        }
        if !self.paused() {
            self.repair(ctx, bar, &layout, &shape, now);
        }
    }

    /// Counts one failure; the third in a row pauses repair on the layout `shape`.
    fn fail(&mut self, shape: &str, now: SystemTime) {
        self.failures += 1;
        if self.failures >= REPAIR_TRIES {
            self.paused = Some((shape.to_string(), now));
        }
    }

    /// Sets the bar to `width` for a window `window` wide; remembered only when the multiplexer took it.
    fn resize(&mut self, ctx: &Ctx, bar: &str, (window, width): (u32, u32)) {
        self.acted = true;
        let columns = width.to_string();
        if native(ctx, &["resize-pane", "-t", bar, "-x", &columns], true).is_ok_and(|r| r.code == 0)
        {
            self.applied = Some((window, width));
        }
    }

    /// Moves the bar back to the right edge, full height, against `move_target`, sets its width at once, then gives
    /// the focus back: move-pane moves the active pane even with -d, so the pane active before (the bar included) is
    /// selected again unless it still is. The window is read again at once: a bar still out of place counts a failure
    /// (as a refused move does), one in place ends the run of failures, an unreadable read counts nothing. Checking
    /// now, not on the next tick, keeps a layout the user changes in between from counting against the repair.
    fn repair(&mut self, ctx: &Ctx, bar: &str, layout: &Layout, shape: &str, now: SystemTime) {
        let Some(target) = move_target(&layout.panes, bar) else {
            return;
        };
        let active = layout.panes.iter().find(|p| p.active).map(|p| p.id.clone());
        let moved = native(
            ctx,
            &["move-pane", "-d", "-f", "-h", "-s", bar, "-t", target],
            true,
        )
        .is_ok_and(|moved| moved.code == 0);
        if !moved {
            self.fail(shape, now);
            return;
        }
        self.acted = true;
        self.applied = None;
        let sizing = layout
            .panes
            .iter()
            .find(|p| p.id == bar)
            .map_or_else(Sizing::default, |p| p.sizing);
        self.resize(ctx, bar, (layout.width, bar_width(layout.width, sizing)));
        if let Some(active) = active {
            let still = native(
                ctx,
                &["display-message", "-p", "-t", &active, "#{pane_active}"],
                true,
            )
            .is_ok_and(|r| r.code == 0 && r.stdout.trim_end_matches(['\r', '\n']) == "1");
            if !still {
                let _ = native(ctx, &["select-pane", "-t", &active], true);
            }
        }
        let Some(after) = read_layout(ctx, bar) else {
            return;
        };
        match after.panes.iter().find(|p| p.id == bar).map(in_place) {
            Some(true) => self.failures = 0,
            Some(false) => self.fail(&signature(&after.panes), now),
            None => {}
        }
    }
}

/// The bar's window as LAYOUT reads it; none when the read fails or does not parse.
fn read_layout(ctx: &Ctx, bar: &str) -> Option<Layout> {
    let read = native(ctx, &["list-panes", "-t", bar, "-F", LAYOUT], true)
        .ok()
        .filter(|read| read.code == 0)?;
    parse_layout(&read.stdout)
}

/// The S1.3 invariant: the bar is at the right edge, top and bottom of its window.
fn in_place(bar: &Pane) -> bool {
    bar.at_right && bar.at_top && bar.at_bottom
}

/// Whether the watch probe answered a gone pane: exit 0 and only empty fields.
fn watch_gone(probe: &Output) -> bool {
    let line = probe.stdout.trim_end_matches(['\r', '\n']);
    probe.code == 0 && !line.is_empty() && line.chars().all(|c| c == '|')
}

/// The pane's size from the watch probe's last two fields; none unless both are numbers of at least 1.
fn probe_size(probe: &Output) -> Option<Size> {
    let line = probe.stdout.trim_end_matches(['\r', '\n']);
    let mut fields = line.rsplitn(3, '|');
    let height = fields.next()?.parse::<usize>().ok()?;
    let width = fields.next()?.parse::<usize>().ok()?;
    (width >= 1 && height >= 1).then_some(Size { width, height })
}

/// The bar's own size, read again (read only); none unless the answer names the bar and parses.
fn read_size(ctx: &Ctx, bar: &str) -> Option<Size> {
    let read = native(
        ctx,
        &["display-message", "-p", "-t", bar, SIZE_FORMAT],
        true,
    )
    .ok()
    .filter(|read| read.code == 0 && read.stdout.starts_with(&format!("{bar}|")))?;
    probe_size(&read)
}

/// The side-bar loop for the caller's pane. Each tick asks the pane itself (and its size) and, when it answers,
/// keeps it at the right edge of its window (`Keeper`; by its own pane, not its tag, since rmux 0.10.0's
/// rotate-window leaves pane options behind), reads the board (ended rows with their exit codes, waiting and the
/// parent's unread), lays it out at the pane's size (and `layout: repair paused` while repair is paused, then any
/// `focus failed` note; the selected agent marked, with the hint when it fits), writes the frame, then reads `input`
/// until 2 s after that write, acting on each event as it is read (`act`): every event of one read in order, then a
/// key or left press draws again at once, without counting a tick. A lone ESC is waited on at most 50 ms before it
/// counts as the key. Ends `pane-closed` when the probe answers gone, `output-closed` when
/// a write fails, `probe-failed` after 30 unknown probes in a row and `interrupted` on Ctrl-C. A failed board read
/// still draws the last board marked `refresh failed`, so a dead terminal is found even when nothing else answers.
/// An input that can no longer be read is dropped, which restores its terminal, and the loop goes on without it;
/// `input` is owned here, so it is dropped, and its terminal restored, on every return and every unwind.
pub fn run_watch(
    ctx: &Ctx,
    caller: &Caller,
    mut input: Box<dyn Input>,
    write: &mut dyn FnMut(&str) -> io::Result<()>,
) -> &'static str {
    let own = format!("{}|", caller.pane);
    // watch draws only to a terminal (watch_on refuses otherwise).
    let look = Look::from_env(&ctx.env, true);
    let mut keys = Decoder::default();
    let mut keeper = Keeper::default();
    let mut last: Option<Map<String, Value>> = None;
    let mut size = FALLBACK;
    let mut unknown = 0;
    let mut pick = Pick::default();
    let mut viewing = Viewing::default();
    let mut alerts = Alerts::default();
    loop {
        let probe = native(
            ctx,
            &["display-message", "-p", "-t", &caller.pane, WATCH_PROBE],
            true,
        )
        .ok();
        if probe.as_ref().is_some_and(watch_gone) {
            return "pane-closed";
        }
        let live = probe
            .as_ref()
            .filter(|p| p.code == 0 && p.stdout.starts_with(&own));
        unknown = if live.is_some() { 0 } else { unknown + 1 };
        if unknown >= UNKNOWN_MAX {
            return "probe-failed";
        }
        if let Some(read) = live.and_then(probe_size) {
            size = read;
        }
        if live.is_some() && own_record(ctx, caller) && keeper.tick(ctx, &caller.pane) {
            // The keeper resized or moved the bar: lay out at its size now, else at the width it set.
            match read_size(ctx, &caller.pane) {
                Some(read) => size = read,
                None => {
                    if let Some(width) = keeper.width() {
                        size.width = width;
                    }
                }
            }
        }
        let mut failed = live.is_none();
        match board(ctx, caller, true, true) {
            Ok(board) => last = Some(board),
            Err(_) => failed = true,
        }
        // The pane the person looks at (an unanswered probe changes nothing), then the done spell they leave behind:
        // seen. A spell not marked is kept and marked again on the next tick.
        let looked = shown_pane(ctx, caller);
        if let Some(looked) = looked.clone() {
            viewing.tick(looked, last.as_ref());
        }
        // An agent that starts needing the person, or finishes, while they look elsewhere: one message, as herdr's
        // toast. Only when both the board and the shown pane were read now: a tick that knows neither keeps the news
        // for the next.
        if looked.is_some() && !failed {
            let news = alerts.tick(last.as_ref(), viewing.pane.as_deref());
            if !news.is_empty() {
                alert(ctx, caller, &news);
            }
        }
        viewing.mark(|pane, changed| {
            crate::agents::board::mark_seen(ctx, caller, pane, changed).is_ok()
        });
        let note = pick.note();
        let mut notes = Vec::new();
        if failed {
            notes.push(("refresh failed", DIM));
        }
        if keeper.paused() {
            notes.push((PAUSED, DIM));
        }
        if let Some(note) = note.as_deref() {
            notes.push((note, PLAIN));
        }
        let bare = lay(last.as_ref(), &notes, size, look);
        pick.follow(&targets(&bare));
        let laid = match pick.selected() {
            Some(name) => {
                let mut laid = with_hint(last.as_ref(), &notes, size, look);
                mark(&mut laid, name, look);
                laid
            }
            None => bare,
        };
        if write(&frame(&laid.rows, size, look)).is_err() {
            return "output-closed";
        }
        let mut shown = Shown {
            written: ctx.clock.now(),
            targets: targets(&laid),
            width: size.width,
            looking: viewing.pane.clone(),
        };
        loop {
            let since = ctx
                .clock
                .now()
                .duration_since(shown.written)
                .unwrap_or(TICK);
            let left = TICK.saturating_sub(since);
            if left.is_zero() {
                pick.ticked();
                break;
            }
            let wait = if keys.lone_esc() {
                left.min(ESC_WAIT)
            } else {
                left
            };
            let reads = match input.wait(&*ctx.clock, wait) {
                Waited::Bytes(bytes) => keys.push(&bytes, ctx.clock.now()),
                Waited::Timeout => keys.idle(),
                // The old input is dropped here, which restores its terminal.
                Waited::Closed => {
                    input = Box::new(NoInput);
                    Vec::new()
                }
            };
            let mut redraw = false;
            for read in reads {
                if read.event == Event::Interrupt {
                    return "interrupted";
                }
                redraw |= act(
                    ctx,
                    caller,
                    look,
                    read,
                    &mut pick,
                    &mut shown,
                    last.as_ref(),
                );
            }
            if redraw {
                break;
            }
        }
    }
}

/// The pane the session shows to an attached client, its current window's active pane: `Some(None)` when no client
/// is attached, `None` when the multiplexer did not answer in that shape (nothing is known).
pub(crate) fn shown_pane(ctx: &Ctx, caller: &Caller) -> Option<Option<String>> {
    let read = native(
        ctx,
        &["display-message", "-p", "-t", &caller.session, SHOWN_FORMAT],
        true,
    )
    .ok()?;
    if read.code != 0 {
        return None;
    }
    let line = read.stdout.trim_end_matches(['\r', '\n']);
    let (pane, attached) = line.split_once('|')?;
    let attached = attached.parse::<u32>().ok()?;
    if attached == 0 {
        return Some(None);
    }
    crate::agents::pane_id(pane).then(|| Some(pane.to_string()))
}

const SHOWN_FORMAT: &str = "#{pane_id}|#{session_attached}";

/// Who the person is looking at, as herdr holds a done agent while it is viewed: the shown pane, the newest done
/// spell (a row's `changedAt`) it showed while looked at, and the spells left behind still to be marked seen.
#[derive(Debug, Default, PartialEq, Eq)]
struct Viewing {
    pane: Option<String>,
    done: Option<String>,
    left: Vec<(String, String)>,
}

impl Viewing {
    /// One tick with the pane shown now (`None`: none, no client attached) and the board. Leaving a pane queues the
    /// done spell it showed to be marked seen; while the same pane stays shown, a done row on it is remembered.
    fn tick(&mut self, shown: Option<String>, board: Option<&Map<String, Value>>) {
        if shown != self.pane {
            if let (Some(pane), Some(done)) = (self.pane.take(), self.done.take()) {
                self.left.retain(|(p, _)| *p != pane);
                self.left.push((pane, done));
            }
            self.pane = shown;
        }
        let rows = board
            .and_then(|b| b.get("agents"))
            .and_then(Value::as_array);
        let done = rows
            .into_iter()
            .flatten()
            .filter_map(Value::as_object)
            .find(|row| {
                let field = |name: &str| row.get(name).and_then(Value::as_str);
                field("state") == Some("running")
                    && self.pane.is_some()
                    && field("pane") == self.pane.as_deref()
                    && field("activity") == Some("done")
            });
        if let Some(changed) = done
            .and_then(|row| row.get("changedAt"))
            .and_then(Value::as_str)
        {
            self.done = Some(changed.to_string());
        }
    }

    /// Marks each spell left behind with `seen`; one it could not mark stays for the next tick.
    fn mark(&mut self, mut seen: impl FnMut(&str, &str) -> bool) {
        self.left.retain(|(pane, changed)| !seen(pane, changed));
    }
}

/// Each live agent's last activity, by pane, to tell when one starts needing the person or finishes.
#[derive(Debug, Default)]
struct Alerts {
    last: std::collections::HashMap<String, String>,
}

impl Alerts {
    /// One tick: the agents that turned blocked or done since the last tick, while their pane is not the one shown,
    /// as (name, activity). The first sight of an agent is no news; a board not read changes nothing.
    fn tick(
        &mut self,
        board: Option<&Map<String, Value>>,
        shown: Option<&str>,
    ) -> Vec<(String, String)> {
        let Some(rows) = board
            .and_then(|b| b.get("agents"))
            .and_then(Value::as_array)
        else {
            return Vec::new();
        };
        let mut news = Vec::new();
        let mut now = std::collections::HashMap::new();
        for row in rows.iter().filter_map(Value::as_object) {
            if clean(row.get("state")) != "running" {
                continue;
            }
            let (pane, activity) = (clean(row.get("pane")), clean(row.get("activity")));
            let changed = self.last.get(&pane).is_some_and(|was| *was != activity);
            if changed
                && matches!(activity.as_str(), "blocked" | "done")
                && shown != Some(pane.as_str())
            {
                news.push((clean(row.get("name")), activity.clone()));
            }
            now.insert(pane, activity);
        }
        self.last = now;
        news
    }
}

/// `@mux-agents-alerts`: `off` sends none, `blocked` only who needs the person, anything else (unset) both.
const ALERTS_OPTION: &str = "@mux-agents-alerts";

/// The tmux message for `news`, on the session's clients for 5 s, unless `@mux-agents-alerts` says otherwise. Names
/// are `[a-z0-9-]` and the words fixed, so the text holds no format. Best effort.
fn alert(ctx: &Ctx, caller: &Caller, news: &[(String, String)]) {
    let option = native(
        ctx,
        &["show-options", "-qv", "-t", &caller.session, ALERTS_OPTION],
        true,
    )
    .ok()
    .map(|o| o.stdout.trim().to_string())
    .unwrap_or_default();
    let parts: Vec<String> = news
        .iter()
        .filter(|(_, activity)| match option.as_str() {
            "off" => false,
            "blocked" => activity == "blocked",
            _ => true,
        })
        .filter(|(name, _)| crate::agents::agent_name(name))
        .map(|(name, activity)| {
            let word = if activity == "blocked" {
                "needs you"
            } else {
                "is done"
            };
            format!("{name} {word}")
        })
        .collect();
    if parts.is_empty() {
        return;
    }
    let text = format!("mux-agents: {}", parts.join(", "));
    let _ = native(
        ctx,
        &[
            "display-message",
            "-t",
            &caller.session,
            "-d",
            "5000",
            &text,
        ],
        true,
    );
}

/// A press read less than this long after a frame was written is dropped: the user may have aimed at the frame
/// before it.
const PRESS_GAP: Duration = Duration::from_millis(50);

/// One multiplexer call for the bar's width and its rows, so the rows are read at the width they were drawn at.
fn click_read(bar: &str) -> [&str; 10] {
    [
        "display-message",
        "-p",
        "-t",
        bar,
        "#{pane_width}",
        ";",
        "capture-pane",
        "-p",
        "-t",
        bar,
    ]
}

/// The frame on screen: when it was written, the running agents it drew (top to bottom) and its width.
struct Shown {
    written: SystemTime,
    targets: Vec<Target>,
    width: usize,
    // The pane the session showed at this frame (herdr's next / previous start from it).
    looking: Option<String>,
}

fn focus_target_hc(ctx: &Ctx, caller: &Caller, target: &Target) -> Res<Focused> {
    let core = crate::hc::connect(ctx)?;
    focus_hc(
        ctx,
        &core,
        caller,
        &target.name,
        (!target.pane.is_empty()).then_some(target.pane.as_str()),
    )
}
impl Shown {
    fn focused(&mut self, focused: Focused) {
        for target in &mut self.targets {
            if target.name == focused.name {
                target.pane = focused.pane.clone();
            }
        }
        self.looking = Some(focused.pane);
    }
}

/// Acts on one event read while `shown` is on screen; true when the bar must draw again at once. A left press reads
/// the bar and focuses the running agent on its name line, unless it is ASCII or stale (read less than `PRESS_GAP`
/// after the frame's write, or begun before it); any left press dismisses the note and draws again. Keys go to
/// `pick`. A failed focus shows its note. Other buttons and releases do nothing.
fn act(
    ctx: &Ctx,
    caller: &Caller,
    look: Look,
    read: keys::Read,
    pick: &mut Pick,
    shown: &mut Shown,
    board: Option<&Map<String, Value>>,
) -> bool {
    match read.event {
        Event::Press { button: 0, y, .. } => {
            pick.pressed();
            let stale = read.first < shown.written + PRESS_GAP;
            if look.ascii || stale {
                return true;
            }
            if let Some(target) = click(ctx, caller, y, shown.width, board) {
                match focus_target_hc(ctx, caller, &target) {
                    Ok(focused) => shown.focused(focused),
                    Err(_) => pick.failed(&target.name),
                }
            }
            true
        }
        Event::Press { .. } | Event::Release { .. } | Event::Interrupt => false,
        // herdr's next and previous agent (#90), from the pane the session shows, in the bar's order.
        Event::Next | Event::Prev => {
            let forward = read.event == Event::Next;
            if let Some(target) =
                pick::step(&shown.targets, shown.looking.as_deref(), forward).cloned()
            {
                match focus_target_hc(ctx, caller, &target) {
                    Ok(focused) => shown.focused(focused),
                    Err(_) => pick.failed(&target.name),
                }
            }
            true
        }
        // herdr's peek (#91): the selected agent, else the first, over the window without switching to it.
        Event::Peek => {
            if let Some(target) = pick.chosen(&shown.targets).or(shown.targets.first())
                && peek(ctx, caller, target).is_err()
            {
                pick.failed(&target.name);
            }
            true
        }
        Event::Up | Event::Down | Event::Enter | Event::Esc => {
            match pick.key(read.event, &shown.targets) {
                Act::Nothing => false,
                Act::Redraw => true,
                Act::Focus(target) => {
                    match focus_target_hc(ctx, caller, &target) {
                        Ok(focused) => {
                            pick.focused();
                            shown.focused(focused);
                        }
                        Err(_) => pick.failed(&target.name),
                    }
                    true
                }
            }
        }
    }
}

/// Lines a peek shows: the screen and some history.
const PEEK_LINES: i32 = 200;

/// herdr's peek: the agent's last lines, captured as `read <name> -Lines` captures them (only while its pane is its
/// own live launch), into a private file of the state folder, shown in a popup over the session's client that pages
/// it, deleting it as it starts. The popup is started in the background (`run-shell -b -C`), since display-popup
/// answers only once it closes and the bar must not wait; so a popup that never opens is not seen here, and its
/// file stays in the state folder (no sweep: age cannot tell it from a popup still queued behind another).
fn peek(ctx: &Ctx, caller: &Caller, target: &Target) -> Res<()> {
    let pane = crate::agents::registry::find_reaped(ctx, caller, &target.name)?;
    if pane.pane != target.pane {
        return refuse(
            2,
            "target",
            "the agent is no longer on the pane the bar drew",
        );
    }
    let folder = crate::agents::server_dir(ctx, caller);
    // The path goes through the multiplexer's parser, its formats and a shell: plain characters only.
    if !folder
        .to_str()
        .is_some_and(|f| plain_path(f, crate::state::root::HOST))
    {
        return refuse(
            1,
            "state",
            "peek needs a state folder path of plain characters",
        );
    }
    let text = crate::agents::message::capture(ctx, &pane, PEEK_LINES)?;
    let file = folder.join(format!(
        "peek-{}.txt",
        crate::util::guid()?.replace('-', "")
    ));
    // The name closes the text, where the pager opens: tmux 3.2 popups have no title.
    std::fs::write(
        &file,
        format!("{text}\n-- peek: {} (q closes) --\n", target.name),
    )?;
    let Some(path) = file.to_str() else {
        let _ = std::fs::remove_file(&file);
        return refuse(1, "internal", "the peek file's path is not text");
    };
    let popup = popup_argv(&caller.pane, path, crate::state::root::HOST);
    let popup = popup
        .iter()
        .map(|a| format!("'{a}'"))
        .collect::<Vec<_>>()
        .join(" ");
    let shown = native(ctx, &["run-shell", "-b", "-C", &popup], true)
        .ok()
        .filter(|o| o.code == 0);
    if shown.is_none() {
        let _ = std::fs::remove_file(&file);
        return refuse(1, "target", "the popup did not open");
    }
    Ok(())
}

/// True when `path` holds only characters no layer reads: no format `#`, no `%`, quote, `$`, backquote, `&`, `!` or
/// other shell character, and a backslash only on Windows, where it separates folders.
fn plain_path(path: &str, os: crate::state::root::OsKind) -> bool {
    path.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || " _.-/:+,@=~".contains(c)
            || (c == '\\' && os == crate::state::root::OsKind::Windows)
    })
}

/// The popup that pages `path` (a `plain_path`) over the client showing `bar`'s session, deleting it once read, so a
/// popup closed by force leaves nothing: on Unix the file is opened, deleted, then paged from the end by `less`; on
/// Windows `type` shows it, `del` deletes it, then `pause` waits for a key. No argument holds a `'`, so each goes to
/// `run-shell -C` in single quotes. No -T: tmux 3.2 has none.
fn popup_argv(bar: &str, path: &str, os: crate::state::root::OsKind) -> Vec<String> {
    // On Unix the command is /bin/sh's own argv, never the session's default-shell (which may be tcsh or fish).
    let command = match os {
        crate::state::root::OsKind::Unix => vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("{{ rm -f \"{path}\"; less -R +G; }} < \"{path}\""),
        ],
        crate::state::root::OsKind::Windows => vec![format!(
            "cmd /d /c \"type \"{path}\" & del \"{path}\" & pause\""
        )],
    };
    ["display-popup", "-t", bar, "-w", "80%", "-h", "80%", "-E"]
        .iter()
        .map(|s| s.to_string())
        .chain(command)
        .collect()
}

/// The running agent on row `y` of the bar, read in one call: none unless the call answers, the bar is `width`
/// columns wide (the frame's width), row `y` is a name line and its name is exactly one running agent on `board`.
fn click(
    ctx: &Ctx,
    caller: &Caller,
    y: u16,
    width: usize,
    board: Option<&Map<String, Value>>,
) -> Option<Target> {
    let read = native(ctx, &click_read(&caller.pane), true)
        .ok()
        .filter(|read| read.code == 0)?;
    let name = pick::screen_hit(&read.stdout, width, y)?;
    pick::resolve(board?, name)
}

/// The side-bar pane's process: draws the board to stdout until its pane closes, reading its own terminal when
/// stdin is one too and `TERM` is not `dumb` (`input::open`).
pub fn watch(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    use std::io::{IsTerminal, Write};
    let stdout = io::stdout();
    let terminal = stdout.is_terminal();
    let mut out = stdout.lock();
    watch_on(
        ctx,
        request,
        terminal,
        &|| input::open(&ctx.env),
        &mut |frame| {
            out.write_all(frame.as_bytes())?;
            out.flush()
        },
    )
}

/// Refuses unless stdout is a terminal: a terminal whose pane closed fails the next write, but a pipe whose reader
/// closed may not, and with the pane alive nothing else would end the loop. Opens the input only once it draws, so
/// a refusal leaves the terminal as it was.
pub(crate) fn watch_on(
    ctx: &Ctx,
    _request: &Request,
    terminal: bool,
    open: &dyn Fn() -> Box<dyn Input>,
    write: &mut dyn FnMut(&str) -> io::Result<()>,
) -> Res<Map<String, Value>> {
    if !terminal {
        return refuse(
            2,
            "usage",
            "watch draws only to a terminal: it runs in the side-bar pane",
        );
    }
    let caller = resolve(ctx)?;
    console_utf8();
    let ended = run_watch(ctx, &caller, open(), write);
    let mut reply = Map::new();
    reply.insert("pane".into(), caller.pane.into());
    reply.insert("ended".into(), ended.into());
    Ok(reply)
}

/// The Windows console starts on an OEM code page, which would draw the glyphs as '?'; a refusal keeps it.
#[cfg(windows)]
fn console_utf8() {
    // SAFETY: takes a code page number only.
    unsafe { windows_sys::Win32::System::Console::SetConsoleOutputCP(65001) };
}

#[cfg(not(windows))]
fn console_utf8() {}

/// `sidebar [-Close]`: one bar per session, under the session's sidebar lock. Opens a split at the right edge of the
/// caller's window (`bar_width` columns) running watch (again without -l only when the multiplexer refused the size),
/// labelled @agent-role sidebar and recorded by its process; the proven bar is returned with opened false; -Close
/// kills it only while it is still that process, and reports closed only when the pane is gone. A label is never
/// trusted: an unproven labelled pane refuses both.
/// -Close first sets the session's @mux-agents-sidebar `off` and `sidebar` unsets it, both inside the lock, so a
/// spawn's `ensure` never reopens a bar a returned -Close closed. Refused only from a tagged pane.
pub fn sidebar(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let caller = resolve(ctx)?;
    crate::agents::registry::untagged(ctx, &caller, "an agent pane cannot open the side bar")?;
    let close = request.switch("Close");
    lock(ctx, &lock_key("sidebar", &caller, &caller.session), || {
        let mut reply = Map::new();
        if !close {
            native(
                ctx,
                &["set-option", "-u", "-t", &caller.session, OPTION],
                false,
            )?;
            let (pane, window, opened) = find_or_open(ctx, &caller)?;
            reply.insert("pane".into(), pane.into());
            reply.insert("window".into(), window.into());
            reply.insert("opened".into(), opened.into());
            return Ok(reply);
        }
        native(
            ctx,
            &["set-option", "-t", &caller.session, OPTION, "off"],
            false,
        )?;
        match prove(ctx, &caller)? {
            None => {
                let labelled = labelled(ctx, &caller)?;
                if !labelled.is_empty() {
                    return refuse(1, "sidebar", unproven(&labelled));
                }
                reply.insert("pane".into(), Value::Null);
                reply.insert("closed".into(), false.into());
            }
            Some(bar) => {
                if !kill_bar(ctx, &bar.pane, bar.pid) {
                    return refuse(
                        1,
                        "sidebar",
                        format!(
                            "the side bar {} did not close: the pane no longer runs this session's side bar",
                            bar.pane
                        ),
                    );
                }
                let _ = native(
                    ctx,
                    &["set-option", "-u", "-t", &caller.session, RECORD],
                    true,
                );
                reply.insert("pane".into(), bar.pane.into());
                reply.insert("closed".into(), true.into());
            }
        }
        Ok(reply)
    })
}

/// The bar's width options: at least `min` columns, `percent` of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sizing {
    pub min: u32,
    pub percent: u32,
}

impl Default for Sizing {
    fn default() -> Self {
        Sizing {
            min: MIN_WIDTH,
            percent: PERCENT,
        }
    }
}

impl Sizing {
    /// The options as the multiplexer answers them: `min` 1-9999 and `percent` 1-100, in plain digits; each one
    /// that is not (unset, a sign, a space, too big) is its default alone.
    pub fn read(min: &str, percent: &str) -> Sizing {
        let number = |text: &str, most: u32| {
            (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
                .then(|| text.parse::<u32>().ok())
                .flatten()
                .filter(|n| (1..=most).contains(n))
        };
        let default = Sizing::default();
        Sizing {
            min: number(min, 9999).unwrap_or(default.min),
            percent: number(percent, 100).unwrap_or(default.percent),
        }
    }
}

/// The bar's width for a window `window` columns wide: `max(min, floor(window * percent / 100))`.
pub fn bar_width(window: u32, sizing: Sizing) -> u32 {
    let share = u64::from(window) * u64::from(sizing.percent) / 100;
    u32::try_from(share).unwrap_or(u32::MAX).max(sizing.min)
}

/// WINDOW_PROBE's answer: the window's width (none unless a number) and the width options; nothing in an answer
/// that is not exactly 3 fields on one line is used.
fn window_answer(text: &str) -> (Option<u32>, Sizing) {
    let line = text.strip_suffix('\n').unwrap_or(text);
    let line = line.strip_suffix('\r').unwrap_or(line);
    let f: Vec<&str> = line.split('|').collect();
    if f.len() != 3 || line.contains(['\n', '\r']) {
        return (None, Sizing::default());
    }
    let width = (!f[0].is_empty() && f[0].bytes().all(|b| b.is_ascii_digit()))
        .then(|| f[0].parse().ok())
        .flatten();
    (width, Sizing::read(f[1], f[2]))
}

/// The width of `pane`'s window and its width options; no width when the multiplexer does not answer a number.
fn window_width(ctx: &Ctx, pane: &str) -> (Option<u32>, Sizing) {
    native(
        ctx,
        &["display-message", "-p", "-t", pane, WINDOW_PROBE],
        true,
    )
    .ok()
    .filter(|read| read.code == 0)
    .map_or_else(
        || (None, Sizing::default()),
        |read| window_answer(&read.stdout),
    )
}

fn window_id(text: &str) -> bool {
    text.strip_prefix('@')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The proven bar: its pane, window and process.
struct Proven {
    pane: String,
    window: String,
    pid: u32,
}

/// `<pane> <pid> <start>`, exactly; none for anything else.
fn parse_record(text: &str) -> Option<(String, u32, i64)> {
    let mut fields = text.split(' ');
    let (pane, pid, start) = (fields.next()?, fields.next()?, fields.next()?);
    let digits = |f: &str| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit());
    if fields.next().is_some() || !pane_id(pane) || !digits(pid) || !digits(start) {
        return None;
    }
    Some((pane.to_string(), pid.parse().ok()?, start.parse().ok()?))
}

/// The start of process `pid` when it is this binary run as exactly `<binary> watch`; none when it cannot be read or
/// is anything else. The multiplexer starts the bar's two-item command itself, so its pane pid is the watch.
fn watch_start(ctx: &Ctx, pid: u32) -> Option<i64> {
    let row = ctx.os.row(pid)?;
    let exe = ctx.exe.to_str()?;
    let path = row.path.as_deref()?;
    let same = if cfg!(windows) {
        path.eq_ignore_ascii_case(exe)
    } else {
        path == exe
    };
    let argv = row.argv.as_ref()?;
    (same && argv.len() == 2 && argv[1] == "watch")
        .then_some(row.start)
        .flatten()
}

/// The session's bar, proven now (T9): the record names a pane whose one read shows it alive (`#{pane_dead}` 0) with
/// the recorded pid, and that pid is alive with the recorded start, still this binary `watch`. A failed proof is
/// read once more, then there is no proven bar. An unreadable record refuses: it may name the bar.
fn prove(ctx: &Ctx, caller: &Caller) -> Res<Option<Proven>> {
    let read = native(
        ctx,
        &["show-options", "-qv", "-t", &caller.session, RECORD],
        false,
    )?;
    let Some((pane, pid, start)) = parse_record(read.stdout.trim_end_matches(['\r', '\n'])) else {
        return Ok(None);
    };
    for _ in 0..2 {
        if let Some(window) = proven(ctx, &pane, pid, start) {
            return Ok(Some(Proven { pane, window, pid }));
        }
    }
    Ok(None)
}

/// One proof of `<pane> <pid> <start>`: a read of the pane shows it alive running `pid`, and `pid` is alive with
/// `start`, still this binary `watch`. Answers the pane's window.
fn proven(ctx: &Ctx, pane: &str, pid: u32, start: i64) -> Option<String> {
    let read = native(
        ctx,
        &["display-message", "-p", "-t", pane, PROOF_FORMAT],
        true,
    )
    .ok()
    .filter(|read| read.code == 0)?;
    let answer = read.stdout.trim_end_matches(['\r', '\n']);
    let fields: Vec<&str> = answer.split('|').collect();
    let alive = fields.len() == 4
        && fields[0] == pane
        && window_id(fields[1])
        && fields[2] == "0"
        && fields[3] == pid.to_string();
    (alive && watch_start(ctx, pid).is_some_and(|now| ctx.os.same_start(now, start)))
        .then(|| fields[1].to_string())
}

/// The session's panes, in every window, that carry the side-bar label.
fn labelled(ctx: &Ctx, caller: &Caller) -> Res<Vec<String>> {
    let listed = native(
        ctx,
        &[
            "list-panes",
            "-s",
            "-t",
            &caller.session,
            "-F",
            LABEL_FORMAT,
        ],
        false,
    )?;
    let mut panes: Vec<String> = Vec::new();
    for line in listed.stdout.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(pane) = line.strip_suffix("|1")
            && pane_id(pane)
            && !panes.iter().any(|p| p == pane)
        {
            panes.push(pane.to_string());
        }
    }
    Ok(panes)
}

/// The refusal for labelled panes that are not the proven bar, with the hint for each case.
pub(crate) fn unproven(panes: &[String]) -> String {
    let (noun, verb, target) = match panes {
        [one] => ("pane", "carries", one.as_str()),
        _ => ("panes", "carry", "<pane>"),
    };
    format!(
        "{noun} {} {verb} the side-bar label but no side bar of this session can be proven there: if it is an old \
         side bar (opened by 0.2.1, or restarted), close it with kill-pane -t {target}; if it is your own pane (a \
         label rmux moved there), clear the label with set-option -p -u -t {target} @agent-role",
        panes.join(", ")
    )
}

/// Whether this watch is the session's recorded bar: the record names its own pane and its own pid, and the full
/// proof holds (its pane runs that pid, alive; the pid is this binary `watch` with the recorded start). Only then
/// does it keep the layout; a watch run any other way only draws.
fn own_record(ctx: &Ctx, caller: &Caller) -> bool {
    native(
        ctx,
        &["show-options", "-qv", "-t", &caller.session, RECORD],
        true,
    )
    .ok()
    .filter(|read| read.code == 0)
    .and_then(|read| parse_record(read.stdout.trim_end_matches(['\r', '\n'])))
    .is_some_and(|(pane, pid, start)| {
        pane == caller.pane && pid == ctx.pid && proven(ctx, &pane, pid, start).is_some()
    })
}

/// Whether a failed split refused only its -l size, by the multiplexer's own words: rmux 0.10.0 "invalid split size
/// '<n>': ...", tmux 3.2+ "size invalid" or "size too small", either "command split-window: unknown flag -l".
pub(crate) fn size_refusal(result: &Output) -> bool {
    const SIZE: &str = "invalid split size '";
    result.code != 0
        && result.stderr.split('\n').any(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            line.strip_prefix(SIZE)
                .is_some_and(|rest| rest.contains('\''))
                || line.starts_with("size invalid")
                || line.starts_with("size too small")
                || line.starts_with("command split-window: unknown flag -l")
        })
}

/// Kills `pane` only while it is alive and still runs `pid`, checked by the server in the same command, then reads
/// the pane again: true only when it is gone. `if-shell -F` answers 0 whether or not the kill ran.
fn kill_bar(ctx: &Ctx, pane: &str, pid: u32) -> bool {
    let kill = kill_command(pane, pid);
    let kill: Vec<&str> = kill.iter().map(String::as_str).collect();
    let _ = native(ctx, &kill, true);
    native(
        ctx,
        &["display-message", "-p", "-t", pane, GONE_FORMAT],
        true,
    )
    .is_ok_and(|probe| gone(&probe))
}

/// `command`, run by the server against `pane` only while it is alive and still runs `pid`.
fn bound(pane: &str, pid: u32, command: String) -> Vec<String> {
    vec![
        "if-shell".into(),
        "-F".into(),
        "-t".into(),
        pane.into(),
        format!("#{{&&:#{{==:#{{pane_dead}},0}},#{{==:#{{pane_pid}},{pid}}}}}"),
        command,
    ]
}

/// The conditional kill of `pane`: the server kills it only while it is alive and still runs `pid`.
pub fn kill_command(pane: &str, pid: u32) -> Vec<String> {
    bound(pane, pid, format!("kill-pane -t {pane}"))
}

/// The conditional label of `pane` as the side bar, only while it is alive and still runs `pid`.
pub fn label_command(pane: &str, pid: u32) -> Vec<String> {
    bound(
        pane,
        pid,
        format!("set-option -p -t {pane} @agent-role sidebar"),
    )
}

/// The conditional resize of `pane` to `width` columns, only while it is alive and still runs `pid`.
pub fn resize_command(pane: &str, pid: u32, width: &str) -> Vec<String> {
    bound(pane, pid, format!("resize-pane -t {pane} -x {width}"))
}

/// The split's answer: pane, window and pid.
fn split_answer(stdout: &str) -> Option<(String, String, u32)> {
    let mut fields = stdout.trim_end_matches(['\r', '\n']).split('|');
    let (pane, window, pid) = (fields.next()?, fields.next()?, fields.next()?);
    let pid = (!pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
        .then(|| pid.parse().ok())
        .flatten()?;
    (fields.next().is_none() && pane_id(pane) && window_id(window))
        .then(|| (pane.to_string(), window.to_string(), pid))
}

/// The start of the split's process once it runs this binary `watch`: the split may answer before its child exec'd.
fn new_watch(ctx: &Ctx, pid: u32) -> Option<i64> {
    for n in 0..WATCH_TRIES {
        if let Some(start) = watch_start(ctx, pid) {
            return Some(start);
        }
        if n + 1 < WATCH_TRIES {
            ctx.clock.sleep(Duration::from_millis(50));
        }
    }
    None
}

/// Splits the bar at the right edge of the caller's window, full height (`-f -h`), with the pane environment (Unix:
/// the state root) so watch reads the caller's state. The bar is `bar_width` of the window (its minimum when the width cannot
/// be read), asked for with -l and set again by a pid-bound resize-pane, since rmux ignores -l; a refused resize is left to
/// the next watch tick. `-d` keeps the active pane. The split's process must be this binary `watch` (else nothing is
/// killed and the open refuses); then the pane is labelled (bound to that pid, read back) and recorded as
/// `<pane> <pid> <start>`. A bar that cannot
/// be labelled or recorded is closed by the guarded kill, which spares a pane that runs anything else by then. The
/// bar is proven as the last step; a pane that fails it is not killed, its record is cleared and the open refuses.
fn open(ctx: &Ctx, caller: &Caller) -> Res<(String, String)> {
    let env = pane_env(utf8(&ctx.state_root)?, crate::state::root::HOST);
    let (window, sizing) = window_width(ctx, &caller.pane);
    let width = bar_width(window.unwrap_or(0), sizing).to_string();
    let mut split: Vec<&str> = vec![
        "split-window",
        "-d",
        "-f",
        "-h",
        "-P",
        "-F",
        SPLIT_FORMAT,
        "-t",
        &caller.pane,
    ];
    split.extend(env.iter().map(String::as_str));
    let command = [utf8(&ctx.exe)?, "watch"];
    let mut created = native(ctx, &[&split[..], &["-l", &width], &command].concat(), true)?;
    if size_refusal(&created) {
        created = native(ctx, &[&split[..], &command].concat(), true)?;
    }
    if created.code != 0 {
        return crate::mux::call_failed(ctx);
    }
    let Some((pane, window, pid)) = split_answer(&created.stdout) else {
        return refuse(
            1,
            "rmux",
            format!("{} did not name the side-bar pane", crate::mux::in_use(ctx)),
        );
    };
    let Some(start) = new_watch(ctx, pid) else {
        return refuse(
            1,
            "sidebar",
            format!(
                "the new side-bar pane {pane} did not start this plugin's watch; close it with kill-pane -t {pane}"
            ),
        );
    };
    let record = format!("{pane} {pid} {start}");
    // The label is bound to the checked pid like the kill: a pane replaced since is never labelled. `if-shell -F`
    // answers 0 either way, so the label is read back.
    let label = label_command(&pane, pid);
    let label: Vec<&str> = label.iter().map(String::as_str).collect();
    let labelled = native(ctx, &label, true).is_ok_and(|done| done.code == 0)
        && native(
            ctx,
            &["display-message", "-p", "-t", &pane, "#{@agent-role}"],
            true,
        )
        .is_ok_and(|read| {
            read.code == 0 && read.stdout.trim_end_matches(['\r', '\n']) == "sidebar"
        });
    let recorded = || {
        native(
            ctx,
            &["set-option", "-t", &caller.session, RECORD, &record],
            true,
        )
        .is_ok_and(|done| done.code == 0)
    };
    for (what, done) in [("tagged", labelled), ("recorded", labelled && recorded())] {
        if done {
            continue;
        }
        if kill_bar(ctx, &pane, pid) {
            return refuse(
                1,
                "rmux",
                format!("the side-bar pane {pane} could not be {what}; it was closed"),
            );
        }
        return refuse(
            1,
            "sidebar",
            format!(
                "the side-bar pane {pane} could not be {what} or closed; close it with kill-pane -t {pane}"
            ),
        );
    }
    // Bound to the checked pid like the kill: a pane replaced since is never resized.
    let resize = resize_command(&pane, pid, &width);
    let resize: Vec<&str> = resize.iter().map(String::as_str).collect();
    let _ = native(ctx, &resize, true);
    // Proven last: a pane replaced after the check (a hook on the label, say) is never reported as the bar.
    if proven(ctx, &pane, pid, start).is_none() {
        let _ = native(
            ctx,
            &["set-option", "-u", "-t", &caller.session, RECORD],
            true,
        );
        return refuse(
            1,
            "sidebar",
            format!(
                "the new side-bar pane {pane} no longer runs this plugin's watch; close it with kill-pane -t {pane}, \
                 or, if it is your own pane, clear its label with set-option -p -u -t {pane} @agent-role"
            ),
        );
    }
    Ok((pane, window))
}

#[cfg(test)]
mod tests {
    use super::input::{DISABLE, ENABLE, FakeTerm, Input, NoInput, TermInput, Waited};
    use super::*;
    use crate::agents::fixture::{ID, World, write};
    use crate::error::refuse;
    use crate::mux::Output;
    use crate::testing::Fakes;
    use serde_json::json;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    const READ_RECORD: &str = "-N show-options -qv -t $3 @mux-agents-sidebar-id";
    const CLEAR_RECORD: &str = "-N set-option -u -t $3 @mux-agents-sidebar-id";
    const LABELS: &str = "-N list-panes -s -t $3 -F #{pane_id}|#{==:#{@agent-role},sidebar}";
    const RECORD7: &str = "-N set-option -t $3 @mux-agents-sidebar-id %7 107 1070";
    const PROOF: &str = "#{pane_id}|#{window_id}|#{pane_dead}|#{pane_pid}";
    // The watch's own record: pane %7, the fake process 4242 started at 2000.
    const OWN: &str = "%7 4242 2000\n";
    const READ_OPTION: &str = "-N show-options -qv -t $3 @mux-agents-sidebar";
    const OFF: &str = "-N set-option -t $3 @mux-agents-sidebar off";
    const ON: &str = "-N set-option -u -t $3 @mux-agents-sidebar";

    fn out(code: i32, stdout: &str, stderr: &str) -> Output {
        Output {
            code,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    // ---- the bar's layout (S2.1-S2.9)

    const W40: Size = Size {
        width: 40,
        height: 40,
    };
    const PLAIN_LOOK: Look = Look {
        color: false,
        ascii: false,
        dumb: false,
    };

    // A row of a 40-column bar: `left` after the margin, `right` ending one column short of the edge.
    fn pad(left: &str, right: &str) -> String {
        let used = left.chars().count() + right.chars().count();
        format!(" {left}{}{right}", " ".repeat(38 - used))
    }

    fn plain(board: &Value, size: Size) -> Vec<String> {
        format_board(board.as_object(), &[], size, PLAIN_LOOK)
    }

    // Text without its SGR sequences; any other ESC sequence is left in place.
    fn strip_sgr(text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(at) = rest.find("\u{1b}[") {
            let after = &rest[at + 2..];
            let Some(end) = after.find('m') else { break };
            if !after[..end]
                .bytes()
                .all(|b| b.is_ascii_digit() || b == b';')
            {
                out.push_str(&rest[..at + 2]);
                rest = after;
                continue;
            }
            out.push_str(&rest[..at]);
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        out
    }

    // The mock-up without section headings: the same rows, every live agent working, so row numbers stay put.
    fn mock() -> Value {
        let mut board = sectioned();
        for at in 0..3 {
            board["agents"][at]["activity"] = json!("working");
        }
        board
    }

    // The issue's mock-up: three live agents in the feed's rank (one needs you, one done with an unread message, one
    // working), 7 ended of which the feed has 4.
    fn sectioned() -> Value {
        json!({"running": 3, "endedTotal": 7, "agents": [
            {"name": "add-subtract", "state": "running", "doing": "Adding subtract(a, b) under add in calc.py", "age": 42, "unread": 0, "color": "blue", "activity": "blocked", "since": 12, "waiting": false},
            {"name": "test-calc", "state": "running", "doing": "Done: test_calc.py passed, exit 0", "age": 180, "unread": 1, "color": "magenta", "activity": "done", "since": 30, "waiting": true},
            {"name": "fix-ci-flake", "state": "running", "doing": "Re-running tests/test_watch.rs with --nocapture", "age": 660, "unread": 0, "color": "bright-blue", "activity": "working", "since": null, "waiting": false},
            {"name": "docs-usage", "state": "exited", "exitCode": 0, "age": 1080},
            {"name": "bump-deps", "state": "exited", "exitCode": 101, "age": 1500},
            {"name": "spike-rmux", "state": "stopped", "exitCode": null, "age": 3600},
            {"name": "older", "state": "stopped", "exitCode": null, "age": 7200}
        ]})
    }

    #[test]
    fn layout_draws_the_mock_up() {
        // herdr's sections: each state's heading with its count above its first agent, whose row then shows the
        // compact status, glyph and time, as herdr's priority view does: the heading names the state.
        assert_eq!(
            plain(&sectioned(), W40),
            [
                pad("mux-agents", "1 need you / 1 done"),
                String::new(),
                " Needs you \u{b7} 1".to_string(),
                pad("! add-subtract", "12s"),
                "   Adding subtract(a, b) under add i...".to_string(),
                String::new(),
                " Done \u{b7} 1".to_string(),
                pad("\u{2726} test-calc  \u{2709} 1", "30s"),
                "   Done: test_calc.py passed, exit 0".to_string(),
                String::new(),
                " Working \u{b7} 1".to_string(),
                pad("\u{25c9} fix-ci-flake", "11m"),
                "   Re-running tests/test_watch.rs wi...".to_string(),
                String::new(),
                format!(" ended {}", "-".repeat(31)),
                pad("\u{2713} docs-usage", "done  18m"),
                pad("\u{2717} bump-deps", "exit 101  25m"),
                pad("- spike-rmux", "stopped   1h"),
                "   +4 more  (list -All)".to_string(),
            ]
        );
    }

    #[test]
    fn an_idle_agent_is_muted_whole_and_listening_counts_as_idle() {
        let color = Look {
            color: true,
            ..PLAIN_LOOK
        };
        let sgr = |codes: &str, text: &str| format!("\u{1b}[{codes}m{text}\u{1b}[0m");
        // One idle agent by report, one with no report listening in read -Wait: both idle, no headings.
        let board = json!({"running": 2, "endedTotal": 0, "agents": [
            {"name": "rester", "state": "running", "age": 300, "since": 90, "unread": 0, "color": "magenta", "activity": "idle", "pane": "%1"},
            {"name": "listener", "state": "running", "age": 240, "unread": 0, "color": "blue", "waiting": true, "pane": "%2"}
        ]});
        let rows = format_board(board.as_object(), &[], W40, color);
        assert_eq!(
            rows[2],
            [
                format!(" {}", sgr("2", "\u{25cc}")),
                " ".into(),
                sgr("2;35", "rester"),
                " ".repeat(21),
                sgr("2", "idle"),
                sgr("2", "   1m"),
            ]
            .concat()
        );
        assert!(rows[4].contains(&sgr("2", "idle")), "{:?}", rows[4]);
        assert_eq!(
            rows[0],
            format!(
                " {}{}",
                sgr("1", "mux-agents"),
                " ".repeat(12) + "2 idle / 0 ended"
            )
        );
    }

    #[test]
    fn sections_head_each_state_and_go_first_when_short() {
        // Two agents in one state: no heading, the word on each row says it.
        let mut one = sectioned();
        one["agents"][2]["activity"] = json!("blocked");
        one["agents"][1]["activity"] = json!("blocked");
        let rows = plain(&one, W40);
        assert!(rows.iter().all(|r| !r.contains('\u{b7}')), "{rows:?}");
        // A short bar drops the blank lines, then the headings, before any agent or ended row.
        let all = plain(&sectioned(), W40).len();
        let short = plain(
            &sectioned(),
            Size {
                width: 40,
                height: all - 5,
            },
        );
        assert!(short.iter().all(|r| !r.contains('\u{b7}')), "{short:?}");
        assert!(short.iter().any(|r| r.contains("docs-usage")), "{short:?}");
        // Each drawn agent's row is its name line, under its heading.
        let laid = lay(sectioned().as_object(), &[], W40, PLAIN_LOOK);
        let rows: Vec<usize> = laid.agents.iter().map(|(_, row)| *row).collect();
        assert_eq!(rows, [3, 7, 11]);
        let text = |at: usize| {
            laid.rows[at]
                .iter()
                .map(|(t, _)| t.as_str())
                .collect::<String>()
        };
        assert_eq!(
            [text(2), text(6), text(10)],
            [" Needs you \u{b7} 1", " Done \u{b7} 1", " Working \u{b7} 1"]
        );
        // A heading line is never a click on an agent.
        assert_eq!(pick::row_hit(&text(2)), None);
        assert_eq!(pick::row_hit(&text(3)), Some("add-subtract"));
    }

    #[test]
    fn layout_nothing_running_and_more_ended() {
        // Nothing live: `No agents running.`, even with ended agents. A lost one is counted, never drawn.
        let ended = json!({"running": 0, "endedTotal": 3, "agents": [
            {"name": "gone", "state": "lost", "age": 60},
            {"name": "halted", "state": "stopped", "exitCode": null, "age": 60},
            {"name": "crashed", "state": "exited", "exitCode": null, "age": null}
        ]});
        assert_eq!(
            plain(&ended, W40),
            [
                pad("mux-agents", "0 running / 3 ended"),
                String::new(),
                " No agents running.".to_string(),
                String::new(),
                format!(" ended {}", "-".repeat(31)),
                pad("- halted", "stopped   1m"),
                pad("\u{2717} crashed", "exit ?    ?"),
                "   +1 more  (list -All)".to_string(),
            ]
        );
        let empty = json!({"running": 0, "endedTotal": 0, "agents": []});
        assert_eq!(
            plain(&empty, W40),
            [
                pad("mux-agents", "0 running / 0 ended"),
                String::new(),
                " No agents running.".to_string(),
            ]
        );
        // 12 ended, of which the feed carries 10: `12 ended`, 3 rows, `+9 more ended` (list -All shows only 10).
        let rows: Vec<Value> = (0..10)
            .map(|n| json!({"name": format!("a{n}"), "state": "stopped", "age": 60}))
            .collect();
        let twelve = json!({"running": 0, "endedTotal": 12, "agents": rows});
        let got = plain(&twelve, W40);
        assert_eq!(got[0], pad("mux-agents", "0 running / 12 ended"));
        assert_eq!(
            got[got.len() - 4..],
            [
                pad("- a0", "stopped   1m"),
                pad("- a1", "stopped   1m"),
                pad("- a2", "stopped   1m"),
                "   +9 more ended".to_string()
            ]
        );
        // A board never read.
        assert_eq!(
            format_board(None, &["refresh failed"], W40, PLAIN_LOOK),
            [
                pad("mux-agents", "? running"),
                String::new(),
                " refresh failed".to_string()
            ]
        );
    }

    #[test]
    fn layout_promises_list_all_only_for_rows_it_shows() {
        // `list -All` shows the 10 newest ended agents: `+K more  (list -All)` only when all of them are there.
        let ended = |total: usize| {
            let rows: Vec<Value> = (0..total.min(10))
                .map(|n| json!({"name": format!("a{n}"), "state": "stopped", "age": 60}))
                .collect();
            json!({"running": 0, "endedTotal": total, "agents": rows})
        };
        let last = |board: &Value| plain(board, W40).last().cloned().unwrap();
        assert_eq!(last(&ended(4)), "   +1 more  (list -All)");
        assert_eq!(last(&ended(10)), "   +7 more  (list -All)");
        assert_eq!(last(&ended(11)), "   +8 more ended");
        // Fewer rows fit: the rows the bar drops are still among the 10.
        let short = plain(
            &ended(10),
            Size {
                width: 40,
                height: 3,
            },
        );
        assert_eq!(short.last().unwrap(), "   +10 more  (list -All)");
        let short = plain(
            &ended(11),
            Size {
                width: 40,
                height: 3,
            },
        );
        assert_eq!(short.last().unwrap(), "   +11 more ended");
    }

    #[test]
    fn layout_fits_any_width_in_display_columns() {
        let mut long = sectioned();
        long["agents"][0]["name"] = json!("a-very-long-agent-name-xx");
        long["agents"][0]["model"] = json!("claude-sonnet-4-5[1m]");
        long["agents"][1]["unread"] = json!(12345);
        for width in [40, 55, 20, 13, 12, 11, 10, 4, 2, 1] {
            for board in [mock(), long.clone()] {
                let rows = plain(&board, Size { width, height: 40 });
                for row in &rows {
                    assert!(crate::paint::width(row) <= width, "{width}: {row:?}");
                }
                assert_eq!(rows.len() == 1, width < 12, "{width}: {rows:?}");
            }
        }
        // Below 12 columns, the header only, cut to the width.
        let tiny = |width| plain(&mock(), Size { width, height: 9 });
        assert_eq!(
            [tiny(11), tiny(10), tiny(4), tiny(1)],
            [["mux-agents"], ["mux-ag..."], ["mux"], ["m"]]
        );
        // Too narrow for every count, the header drops the last one, then the next: `N need you` stays longest;
        // narrower still, the title alone. The header ends one column short of the pane (its right margin).
        let header = |width| plain(&sectioned(), Size { width, height: 9 })[0].clone();
        let cut = |width: usize, counts: &str| {
            format!(
                " mux-agents{}{counts}",
                " ".repeat(width - 12 - counts.len())
            )
        };
        // Each count fits from 14 columns beyond its width (margin, title, two spaces, right margin).
        let full = "1 need you / 1 done / 1 working / 7 ended";
        assert_eq!(header(full.len() + 14), cut(full.len() + 14, full));
        let three = "1 need you / 1 done / 1 working";
        assert_eq!(header(full.len() + 13), cut(full.len() + 13, three));
        assert_eq!(header(three.len() + 14), cut(three.len() + 14, three));
        assert_eq!(
            header(three.len() + 13),
            cut(three.len() + 13, "1 need you / 1 done")
        );
        assert_eq!(header(33), cut(33, "1 need you / 1 done"));
        assert_eq!(header(32), cut(32, "1 need you"));
        assert_eq!(header(24), cut(24, "1 need you"));
        assert_eq!(header(24), cut(24, "1 need you"));
        assert_eq!(header(23), " mux-agents");
        // With nobody blocked or done: the working count, then the title alone; nothing live: `0 running`.
        let mut calm = sectioned();
        calm["agents"][0]["activity"] = json!("working");
        calm["agents"][1]["activity"] = json!("working");
        let header = |width| plain(&calm, Size { width, height: 9 })[0].clone();
        assert_eq!(header(33), cut(33, "3 working / 7 ended"));
        assert_eq!(header(32), cut(32, "3 working"));
        assert_eq!(header(23), cut(23, "3 working"));
        assert_eq!(header(22), " mux-agents");
        // Short of room the model goes first, then the name is cut; wider, the model is kept.
        let rows = plain(&long, W40);
        // Under a heading the row has no word, so the whole name fits.
        assert_eq!(rows[3], pad("! a-very-long-agent-name-xx", "12s"));
        let rows = plain(
            &long,
            Size {
                width: 80,
                height: 40,
            },
        );
        assert!(
            rows[3].contains("a-very-long-agent-name-xx [claude-sonnet-4-5[1m]]"),
            "{}",
            rows[3]
        );
        // Doing lines of wide characters or combining marks are cut by display columns.
        let mut wide = mock();
        wide["agents"][0]["doing"] = json!("\u{754c}".repeat(30));
        wide["agents"][1]["doing"] = json!("e\u{301}".repeat(40));
        let rows = plain(&wide, W40);
        assert_eq!(rows[3], format!("   {}...", "\u{754c}".repeat(16)));
        assert_eq!(rows[6], format!("   {}...", "e\u{301}".repeat(33)));
        assert_eq!(
            (crate::paint::width(&rows[3]), crate::paint::width(&rows[6])),
            (38, 39)
        );
    }

    // Columns of the samples below, written out by hand: never the code under test. A ZWJ sequence counts each part,
    // as a multiplexer that does not cluster it draws them.
    fn sample_columns(text: &str) -> usize {
        let chars: Vec<char> = text.chars().collect();
        let mut total = 0;
        for (at, &c) in chars.iter().enumerate() {
            total += match c {
                '\u{301}' | '\u{200b}' | '\u{200d}' | '\u{fe0f}' => 0,
                '\u{2764}' if chars.get(at + 1) == Some(&'\u{fe0f}') => 2,
                '\u{754c}' | '\u{fe10}' | '\u{1f680}' | '\u{1f469}' | '\u{1f4bb}' => 2,
                _ => 1,
            };
        }
        total
    }

    #[test]
    fn layout_never_under_counts_a_zwj_sequence() {
        // 16 of `👩‍💻` (4 columns each, part by part) as a name and a doing line at W = 40: every row fits.
        let coder = "\u{1f469}\u{200d}\u{1f4bb}";
        let board = json!({"running": 1, "endedTotal": 1, "agents": [
            {"name": coder.repeat(16), "state": "running", "doing": coder.repeat(16), "age": 5, "unread": 0},
            {"name": coder.repeat(16), "state": "stopped", "age": 60}
        ]});
        let rows = plain(&board, W40);
        for row in &rows {
            assert!(sample_columns(row) <= 40, "{row:?}");
        }
        assert_eq!(rows[3], format!("   {}...", coder.repeat(8)));
        assert_eq!(sample_columns(&rows[3]), 38);
    }

    #[test]
    fn layout_counts_emoji_and_vertical_forms_by_their_columns() {
        let coder = "\u{1f469}\u{200d}\u{1f4bb}";
        // Each sample's doing line at W = 40: 3 columns of indent, then at most 33 columns of whole clusters and `...`
        // (16 two-column clusters make 32: a 17th would not fit, so the row is 38 columns; 8 four-column `👩‍💻` too).
        let samples: [(&str, usize, usize); 6] = [
            ("\u{fe10}", 16, 38),
            ("\u{1f680}", 16, 38),
            ("\u{2764}\u{fe0f}", 16, 38),
            (coder, 8, 38),
            ("e\u{301}", 33, 39),
            ("a\u{200b}", 33, 39),
        ];
        for (sample, kept, columns) in samples {
            let board = json!({"running": 1, "endedTotal": 0, "agents": [
                {"name": "w", "state": "running", "doing": sample.repeat(40), "age": 5, "unread": 0}
            ]});
            let rows = plain(&board, W40);
            assert_eq!(
                rows[3],
                format!("   {}...", sample.repeat(kept)),
                "{sample:?}"
            );
            assert_eq!(sample_columns(&rows[3]), columns, "{sample:?}");
        }
        // A name of rockets is cut by columns too, and every row of every sample fits each width.
        let rockets = json!({"running": 1, "endedTotal": 0, "agents": [
            {"name": "\u{1f680}".repeat(20), "state": "running", "doing": "\u{fe10}".repeat(40), "age": 5, "unread": 0}
        ]});
        // 38 columns: glyph and space (2), the name cut to 22 (9 rockets and `...`, 21), 3 spaces, `working   5s`.
        assert_eq!(
            plain(&rockets, W40)[2],
            format!(" \u{25c9} {}...   working   5s", "\u{1f680}".repeat(9))
        );
        for width in [55, 40, 20, 13, 12] {
            for (sample, _, _) in samples {
                let board = json!({"running": 1, "endedTotal": 1, "agents": [
                    {"name": sample.repeat(12), "state": "running", "doing": sample.repeat(40), "age": 5, "unread": 0},
                    {"name": sample.repeat(12), "state": "stopped", "age": 60}
                ]});
                for row in plain(&board, Size { width, height: 40 }) {
                    assert!(sample_columns(&row) <= width, "{width} {sample:?}: {row:?}");
                }
            }
        }
    }

    #[test]
    fn layout_keeps_to_the_pane_height_in_overflow_order() {
        // 6 live agents (w6 oldest) and 3 ended at H = 10: blank lines go, the ended rows fold into `+3 more`, the
        // separator goes, then the doing lines of the oldest four.
        let mut agents: Vec<Value> = (1..=6)
            .map(|n| json!({"name": format!("w{n}"), "state": "running", "doing": format!("doing {n}"), "age": n * 10, "unread": 0}))
            .collect();
        for n in 1..=3 {
            agents.push(json!({"name": format!("e{n}"), "state": "stopped", "age": 60}));
        }
        let board = json!({"running": 6, "endedTotal": 3, "agents": agents});
        let live = |n: u32| {
            let age = format_age(Some(&json!(n * 10)));
            pad(&format!("\u{25c9} w{n}"), &format!("working {age:>4}"))
        };
        assert_eq!(
            plain(
                &board,
                Size {
                    width: 40,
                    height: 10
                }
            ),
            [
                pad("mux-agents", "6 working / 3 ended"),
                live(1),
                "   doing 1".to_string(),
                live(2),
                "   doing 2".to_string(),
                live(3),
                live(4),
                live(5),
                live(6),
                "   +3 more  (list -All)".to_string(),
            ]
        );
        // 1 live and 1 ended at H = 3, 2, 1: the live agent folds into `+1 more running`, then the summaries go; the
        // header is the last row kept.
        let board = json!({"running": 1, "endedTotal": 1, "agents": [
            {"name": "w", "state": "running", "doing": "x", "age": 5, "unread": 0},
            {"name": "e", "state": "stopped", "age": 60}
        ]});
        let header = pad("mux-agents", "1 working / 1 ended");
        let at = |height| plain(&board, Size { width: 40, height });
        assert_eq!(at(7).len(), 7);
        assert_eq!(
            at(3),
            [
                header.clone(),
                pad("\u{25c9} w", "working   5s"),
                "   +1 more  (list -All)".to_string()
            ]
        );
        assert_eq!(at(2), [header.clone(), "   +1 more running".to_string()]);
        assert_eq!(at(1), [header]);
        // The mock-up at every height keeps the header first and never more rows than the pane.
        for height in 1..=20 {
            let rows = plain(&sectioned(), Size { width: 40, height });
            assert!(rows.len() <= height, "{height}");
            assert_eq!(rows[0], pad("mux-agents", "1 need you / 1 done"));
        }
    }

    #[test]
    fn layout_colors_by_state_and_identity() {
        let color = Look {
            color: true,
            ..PLAIN_LOOK
        };
        let mut board = sectioned();
        board["agents"][0]["model"] = json!("sonnet");
        board["agents"][1]["unread"] = json!(2);
        let size = Size {
            width: 60,
            height: 40,
        };
        let rows = format_board(board.as_object(), &[], size, color);
        let sgr = |codes: &str, text: &str| format!("\u{1b}[{codes}m{text}\u{1b}[0m");
        let starts = |at: usize, want: String| {
            assert!(rows[at].starts_with(&want), "{at}: {:?}", rows[at]);
        };
        starts(0, format!(" {}", sgr("1", "mux-agents")));
        // The header leads with who needs the person, in yellow.
        assert!(
            rows[0].ends_with(&format!(
                "{} / 1 done / 1 working / 7 ended",
                sgr("33", "1 need you")
            )),
            "{:?}",
            rows[0]
        );
        // Needs you: a bold yellow `!` and the word reversed, herdr's pill.
        starts(
            3,
            format!(
                " {} {}{}",
                sgr("1;33", "!"),
                sgr("34", "add-subtract"),
                sgr("2", " [sonnet]")
            ),
        );
        // Under a heading the row has no word: the heading names the state, in its color.
        assert_eq!(
            rows[2],
            format!(" {}{}", sgr("1;33", "Needs you"), sgr("2", " \u{b7} 1"))
        );
        assert!(rows[3].ends_with("  12s"), "{:?}", rows[3]);
        assert_eq!(
            rows[4],
            format!(
                "   {}",
                sgr("2", "Adding subtract(a, b) under add in calc.py")
            )
        );
        // Done: a bold cyan `✦`.
        starts(
            7,
            format!(
                " {} {}  {}",
                sgr("1;36", "\u{2726}"),
                sgr("35", "test-calc"),
                sgr("1;34", "\u{2709} 2")
            ),
        );
        assert_eq!(
            rows[6],
            format!(" {}{}", sgr("1;36", "Done"), sgr("2", " \u{b7} 1"))
        );
        assert_eq!(
            rows[10],
            format!(" {}{}", sgr("1;32", "Working"), sgr("2", " \u{b7} 1"))
        );
        // Without headings (every live agent in one state) each row carries its word: needs you is the reversed pill.
        let mut flat = sectioned();
        for at in 0..3 {
            flat["agents"][at]["activity"] = json!("blocked");
        }
        let flat_rows = format_board(flat.as_object(), &[], size, color);
        assert!(
            flat_rows[2].ends_with(&format!("{}  12s", sgr("1;7;33", "needs you"))),
            "{:?}",
            flat_rows[2]
        );
        let mut flat = sectioned();
        for at in 0..3 {
            flat["agents"][at]["activity"] = json!("done");
        }
        let flat_rows = format_board(flat.as_object(), &[], size, color);
        assert!(
            flat_rows[2].ends_with(&format!("{}  12s", sgr("1;36", "done"))),
            "{:?}",
            flat_rows[2]
        );
        starts(
            11,
            format!(" {} {}", sgr("32", "\u{25c9}"), sgr("94", "fix-ci-flake")),
        );
        assert_eq!(
            rows[14],
            format!(" {}", sgr("2", &format!("ended {}", "-".repeat(51))))
        );
        starts(
            15,
            format!(" {} {}", sgr("2", "\u{2713}"), sgr("2", "docs-usage")),
        );
        assert!(rows[15].contains(&sgr("2", "done")));
        starts(
            16,
            format!(" {} {}", sgr("31", "\u{2717}"), sgr("2", "bump-deps")),
        );
        assert!(rows[16].contains(&sgr("31", "exit 101")));
        starts(
            17,
            format!(" {} {}", sgr("33", "-"), sgr("2", "spike-rmux")),
        );
        assert!(rows[17].contains(&sgr("33", "stopped")));
        assert_eq!(rows[18], format!("   {}", sgr("2", "+4 more  (list -All)")));
        // Without the SGR they are the plain rows.
        let plain_rows = format_board(board.as_object(), &[], size, PLAIN_LOOK);
        assert_eq!(
            rows.iter().map(|r| strip_sgr(r)).collect::<Vec<_>>(),
            plain_rows
        );
        // A row without a color takes its name's hue, as list -Text does.
        board["agents"][0].as_object_mut().unwrap().remove("color");
        let rows = format_board(board.as_object(), &[], size, color);
        let hue = match crate::paint::identity(None, "add-subtract") {
            "blue" => "34",
            "magenta" => "35",
            "bright-blue" => "94",
            _ => "95",
        };
        assert!(rows[3].contains(&sgr(hue, "add-subtract")), "{:?}", rows[3]);
    }

    #[test]
    fn layout_ascii_glyphs() {
        let ascii = Look {
            ascii: true,
            ..PLAIN_LOOK
        };
        let mut board = sectioned();
        board["agents"][1]["unread"] = json!(2);
        let rows = format_board(board.as_object(), &[], W40, ascii);
        let glyphs: Vec<&str> = [3, 7, 11, 15, 16, 17]
            .iter()
            .map(|&at| &rows[at][1..2])
            .collect();
        assert_eq!(glyphs, ["!", "@", "*", "+", "x", "-"]);
        assert!(rows[7].contains("test-calc  m 2"), "{}", rows[7]);
        assert!(rows[4].ends_with("..."));
        assert_eq!(rows[2], " Needs you - 1");
        assert!(rows.iter().all(|r| r.is_ascii()), "{rows:?}");
    }

    // The mock-up with a pane on each running agent.
    fn mock_panes() -> Value {
        let mut board = mock();
        for (at, pane) in ["%1", "%2", "%3"].into_iter().enumerate() {
            board["agents"][at]["pane"] = json!(pane);
        }
        board
    }

    fn target(name: &str, pane: &str) -> Target {
        Target {
            name: name.into(),
            pane: pane.into(),
        }
    }

    // A line as a pane `w` columns wide shows it: pieces of at most `w` columns, the first its head.
    fn reflow(line: &str, w: usize) -> Vec<String> {
        let mut pieces = Vec::new();
        let mut piece = String::new();
        for c in line.chars() {
            if paint::width(&piece) + paint::width(&c.to_string()) > w {
                pieces.push(std::mem::take(&mut piece));
            }
            piece.push(c);
        }
        pieces.push(piece);
        pieces
    }

    #[test]
    fn own_glyphs_in_agent_text() {
        let board = json!({"running": 2, "endedTotal": 1, "agents": [
            {"name": "worker", "state": "running", "model": "\u{25c9}x", "doing": "\u{25cc} step \u{25c9} two", "age": 5, "unread": 0, "color": "blue", "waiting": false, "pane": "%1"},
            {"name": "b\u{25c9}t", "state": "running", "model": "\u{25cc}m", "age": 7, "unread": 0, "color": "blue", "waiting": false, "pane": "%2"},
            {"name": "a\u{25c9}b", "state": "stopped", "exitCode": null, "age": 60}
        ]});
        let rows = format_board(board.as_object(), &["note \u{25c9}"], W40, PLAIN_LOOK);
        assert_eq!(
            rows,
            [
                pad("mux-agents", "2 working / 1 ended"),
                String::new(),
                pad("\u{25c9} worker [\u{2022}x]", "working   5s"),
                "   \u{2022} step \u{2022} two".to_string(),
                String::new(),
                pad("\u{25c9} b\u{2022}t [\u{2022}m]", "working   7s"),
                String::new(),
                format!(" ended {}", "-".repeat(31)),
                pad("- a\u{2022}b", "stopped   1m"),
                String::new(),
                " note \u{2022}".to_string(),
            ]
        );
        // One column each, as the glyphs they replace: every row keeps its width.
        for glyph in ["\u{25c9}", "\u{25cc}", "\u{2022}"] {
            assert_eq!(paint::width(glyph), 1, "{glyph:?}");
        }
        assert_eq!(own_glyphs("\u{25c9} a \u{25cc}"), "\u{2022} a \u{2022}");
        // Only the bar's own glyph starts a name line.
        let hits: Vec<_> = rows.iter().filter_map(|row| pick::row_hit(row)).collect();
        assert_eq!(hits, ["worker", "b\u{2022}t"]);
        // The target keeps the board's name: only the drawn text changes.
        let notes = [("note \u{25c9}", DIM)];
        let laid = lay(board.as_object(), &notes, W40, PLAIN_LOOK);
        assert_eq!(
            targets(&laid),
            [target("worker", "%1"), target("b\u{25c9}t", "%2")]
        );
    }

    #[test]
    fn lay_lists_drawn_running_agents() {
        let board = mock_panes();
        let laid = lay(board.as_object(), &[], W40, PLAIN_LOOK);
        assert_eq!(
            laid.agents,
            [
                (target("add-subtract", "%1"), 2),
                (target("test-calc", "%2"), 5),
                (target("fix-ci-flake", "%3"), 8),
            ]
        );
        assert_eq!(
            targets(&laid),
            [
                target("add-subtract", "%1"),
                target("test-calc", "%2"),
                target("fix-ci-flake", "%3"),
            ]
        );
        assert_eq!(laid.rows, layout(board.as_object(), &[], W40, PLAIN_LOOK));
        // Four rows: the newest agent, then `+2 more running`, which is no agent.
        let short = Size {
            width: 40,
            height: 4,
        };
        let laid = lay(board.as_object(), &[], short, PLAIN_LOOK);
        assert_eq!(laid.agents, [(target("add-subtract", "%1"), 1)]);
        assert_eq!(render(&laid.rows[2], false), "   +2 more running");
        // The header only: under 12 columns, or one row.
        for size in [
            Size {
                width: 11,
                height: 40,
            },
            Size {
                width: 40,
                height: 1,
            },
            Size {
                width: 40,
                height: 0,
            },
        ] {
            let laid = lay(board.as_object(), &[], size, PLAIN_LOOK);
            assert_eq!(laid.agents, [], "{size:?}");
        }
        // Ended agents are never listed.
        let ended = json!({"running": 0, "endedTotal": 1, "agents": [
            {"name": "halted", "state": "stopped", "exitCode": null, "age": 60, "pane": "%4"}
        ]});
        assert_eq!(lay(ended.as_object(), &[], W40, PLAIN_LOOK).agents, []);
        assert_eq!(lay(None, &[], W40, PLAIN_LOOK).agents, []);
    }

    #[test]
    fn lay_lists_the_agents_overflow_keeps() {
        // Ages out of board order: overflow drops `old` (900s), then `mid` (60s), and keeps `new` and `last`.
        let board = json!({"running": 4, "endedTotal": 0, "agents": [
            {"name": "old", "state": "running", "age": 900, "unread": 0, "color": "blue", "waiting": false, "pane": "%1"},
            {"name": "new", "state": "running", "age": 5, "unread": 0, "color": "blue", "waiting": false, "pane": "%2"},
            {"name": "mid", "state": "running", "age": 60, "unread": 0, "color": "blue", "waiting": false, "pane": "%3"},
            {"name": "last", "state": "running", "age": 30, "unread": 0, "color": "blue", "waiting": false, "pane": "%4"}
        ]});
        let size = Size {
            width: 40,
            height: 4,
        };
        let laid = lay(board.as_object(), &[], size, PLAIN_LOOK);
        let names: Vec<&str> = laid.agents.iter().map(|(t, _)| t.name.as_str()).collect();
        let panes: Vec<&str> = laid.agents.iter().map(|(t, _)| t.pane.as_str()).collect();
        let rows: Vec<usize> = laid.agents.iter().map(|(_, row)| *row).collect();
        assert_eq!(names, ["new", "last"]);
        assert_eq!(panes, ["%2", "%4"]);
        assert_eq!(rows, [1, 2]);
        assert_eq!(
            render(&laid.rows[1], false),
            pad("\u{25c9} new", "working   5s")
        );
        assert_eq!(
            render(&laid.rows[2], false),
            pad("\u{25c9} last", "working  30s")
        );
        assert_eq!(render(&laid.rows[3], false), "   +2 more running");
    }

    #[test]
    fn lay_targets_are_cleaned() {
        // The name and pane are cleaned as every drawn field is: no control reaches the target.
        let board = json!({"running": 1, "endedTotal": 0, "agents": [
            {"name": "wo\u{1b}rk\u{7}er\u{85}", "state": "running", "age": 5, "unread": 0, "color": "blue", "waiting": false, "pane": "\u{9b}%7\n\u{7f}"}
        ]});
        let laid = lay(board.as_object(), &[], W40, PLAIN_LOOK);
        assert_eq!(laid.agents, [(target("worker", "%7"), 2)]);
    }

    #[test]
    fn lay_keeps_each_notes_style() {
        let color = Look {
            color: true,
            ..PLAIN_LOOK
        };
        let notes = [
            ("plain note", PLAIN),
            ("dim note", DIM),
            ("red note", hued(Color::Red)),
        ];
        let rows: Vec<String> = lay(None, &notes, W40, color)
            .rows
            .iter()
            .map(|row| render(row, true))
            .collect();
        assert_eq!(
            rows[2..],
            [
                " plain note".to_string(),
                " \u{1b}[2mdim note\u{1b}[0m".to_string(),
                " \u{1b}[31mred note\u{1b}[0m".to_string(),
            ]
        );
    }

    #[test]
    fn row_hit_on_real_layout_output() {
        // Agent text holding the bar's glyphs and its ASCII ones, at offsets that bring them to a piece's head.
        let text = |lead: &str| format!("{lead}\u{25c9} x  \u{25cc} y  * z  x w  ").repeat(6);
        let board = json!({"running": 3, "endedTotal": 1, "agents": [
            {"name": "add-subtract", "state": "running", "model": "\u{25c9} x", "doing": text(""), "age": 42, "unread": 0, "color": "blue", "waiting": false, "pane": "%1"},
            {"name": "test-calc", "state": "running", "model": "x w", "doing": text("x "), "age": 180, "unread": 1, "color": "magenta", "waiting": true, "pane": "%2"},
            {"name": "fix-ci-flake", "state": "running", "model": "\u{25cc} y", "doing": text("* z  "), "age": 660, "unread": 0, "color": "bright-blue", "waiting": false, "pane": "%3"},
            {"name": "\u{25c9} x", "state": "exited", "exitCode": 0, "age": 1080}
        ]});
        let notes = ["\u{25c9} x", " \u{25cc} y"];
        let ascii = Look {
            ascii: true,
            ..PLAIN_LOOK
        };
        // A hit names a prefix of the agent drawn there (a cut name ends in `...`).
        let names = |hit: &str, name: &str| name.starts_with(hit.trim_end_matches("..."));
        let mut hits = 0;
        for (width, height) in [(40, 40), (40, 8), (30, 24), (18, 24)] {
            let size = Size { width, height };
            let styled: Vec<(&str, Style)> = notes.iter().map(|note| (*note, DIM)).collect();
            let laid = lay(board.as_object(), &styled, size, PLAIN_LOOK);
            let lines = format_board(board.as_object(), &notes, size, PLAIN_LOOK);
            assert!(!laid.agents.is_empty(), "{size:?}");
            // The agent whose name line is row `at`, if any.
            let owner = |at: usize| {
                laid.agents
                    .iter()
                    .find(|(_, row)| *row == at)
                    .map(|(target, _)| target.name.as_str())
            };
            for (at, line) in lines.iter().enumerate() {
                match (pick::row_hit(line), owner(at)) {
                    (Some(hit), Some(name)) => {
                        assert!(names(hit, name), "{size:?} {at}: {line:?}");
                        hits += 1;
                    }
                    (got, _) => assert_eq!(got, None, "{size:?} {at}: {line:?}"),
                }
                for w in [12, 15, 20] {
                    for (part, piece) in reflow(line, w).iter().enumerate() {
                        match (pick::row_hit(piece), owner(at).filter(|_| part == 0)) {
                            (Some(hit), Some(name)) => {
                                assert!(names(hit, name), "{size:?} w={w} {at}: {piece:?}");
                            }
                            (got, _) => assert_eq!(got, None, "{size:?} w={w} {at}: {piece:?}"),
                        }
                    }
                }
            }
            for line in format_board(board.as_object(), &notes, size, ascii) {
                for w in [usize::MAX, 12, 15, 20] {
                    for piece in reflow(&line, w) {
                        assert_eq!(
                            pick::row_hit(&piece),
                            None,
                            "{size:?} ascii w={w}: {piece:?}"
                        );
                    }
                }
            }
        }
        // Every size draws its agents' name lines, and each is hit.
        assert!(hits >= 4, "{hits}");
    }

    #[test]
    fn hint_is_dropped_first() {
        let board = mock_panes();
        let hint = |height: usize| {
            let size = Size { width: 40, height };
            let bare = lay(board.as_object(), &[], size, PLAIN_LOOK);
            (with_hint(board.as_object(), &[], size, PLAIN_LOOK), bare)
        };
        // The mock-up draws 16 rows: with 18, the hint is last, after a blank line.
        let (hinted, bare) = hint(18);
        assert_eq!(bare.rows.len(), 16);
        assert_eq!(hinted.rows[..16], bare.rows[..]);
        assert_eq!(hinted.rows[16], Row::new());
        assert_eq!(render(&hinted.rows[17], false), format!(" {HINT}"));
        assert_eq!(hinted.agents, bare.agents);
        // One row short: the bar as it is without the hint.
        let (hinted, bare) = hint(17);
        assert_eq!(hinted, bare);
        // One agent at 40x4: a hinted layout could keep the hint by dropping the gap; it is the bare one.
        let one = json!({"running": 1, "endedTotal": 0, "agents": [
            {"name": "worker", "state": "running", "doing": "testing", "age": 5, "unread": 0, "color": "blue", "waiting": false, "pane": "%1"}
        ]});
        let size = Size {
            width: 40,
            height: 4,
        };
        let bare = lay(one.as_object(), &[], size, PLAIN_LOOK);
        assert_eq!(with_hint(one.as_object(), &[], size, PLAIN_LOOK), bare);
        // At 40x5 a hinted layout keeps every bare row but drops the hint's own blank line: it is the bare one.
        let size = Size {
            width: 40,
            height: 5,
        };
        let bare = lay(one.as_object(), &[], size, PLAIN_LOOK);
        assert_eq!(bare.rows.len(), 4);
        assert_eq!(with_hint(one.as_object(), &[], size, PLAIN_LOOK), bare);
        let size = Size {
            width: 40,
            height: 6,
        };
        let hinted = with_hint(one.as_object(), &[], size, PLAIN_LOOK);
        assert_eq!(
            render(hinted.rows.last().unwrap(), false),
            format!(" {HINT}")
        );
    }

    #[test]
    fn hint_shows_beside_notes() {
        let one = json!({"running": 1, "endedTotal": 0, "agents": [
            {"name": "worker", "state": "running", "doing": "testing", "age": 5, "unread": 0, "color": "blue", "waiting": false, "pane": "%1"}
        ]});
        let notes = [("refresh failed", DIM)];
        // The notes block takes the hint as its last row: no blank line inside the block.
        let bare = lay(one.as_object(), &notes, W40, PLAIN_LOOK);
        let hinted = with_hint(one.as_object(), &notes, W40, PLAIN_LOOK);
        assert_eq!(bare.rows.len(), 6);
        assert_eq!(hinted.rows.len(), 7);
        assert_eq!(hinted.rows[..6], bare.rows[..]);
        assert_eq!(render(&hinted.rows[6], false), format!(" {HINT}"));
        assert_eq!(hinted.agents, bare.agents);
        // One row short: a hinted layout would drop the gap before the notes; it is the bare one.
        let size = Size {
            width: 40,
            height: 6,
        };
        let bare = lay(one.as_object(), &notes, size, PLAIN_LOOK);
        assert_eq!(with_hint(one.as_object(), &notes, size, PLAIN_LOOK), bare);
    }

    #[test]
    fn hint_is_dim_and_cut_to_the_width() {
        let one = json!({"running": 1, "endedTotal": 0, "agents": [
            {"name": "worker", "state": "running", "doing": "testing", "age": 5, "unread": 0, "color": "blue", "waiting": false, "pane": "%1"}
        ]});
        let color = Look {
            color: true,
            ..PLAIN_LOOK
        };
        let drawn = |width: usize| {
            let size = Size { width, height: 40 };
            let bare = lay(one.as_object(), &[], size, color);
            let hinted = with_hint(one.as_object(), &[], size, color);
            let n = bare.rows.len();
            assert_eq!(hinted.rows.len(), n + 2, "{width}");
            assert_eq!(hinted.rows[..n], bare.rows[..], "{width}");
            assert_eq!(hinted.rows[n], Row::new(), "{width}");
            render(&hinted.rows[n + 1], true)
        };
        assert_eq!(drawn(40), format!(" \u{1b}[2m{HINT}\u{1b}[0m"));
        // 20 columns: 18 for the hint, cut with `...`.
        assert_eq!(drawn(20), " \u{1b}[2mEnter: focus  E...\u{1b}[0m");
    }

    #[test]
    fn selected_row_marks_and_reverses() {
        let board = json!({"running": 2, "endedTotal": 0, "agents": [
            {"name": "worker", "state": "running", "doing": "testing", "age": 240, "unread": 0, "color": "blue", "activity": "working", "pane": "%1"},
            {"name": "writer", "state": "running", "model": "sonnet", "age": 180, "unread": 2, "color": "magenta", "activity": "working", "pane": "%2"}
        ]});
        let color = Look {
            color: true,
            ..PLAIN_LOOK
        };
        let ascii = Look {
            ascii: true,
            ..PLAIN_LOOK
        };
        let drawn = |look: Look, selected: Option<&str>| {
            let mut laid = lay(board.as_object(), &[], W40, look);
            if let Some(name) = selected {
                mark(&mut laid, name, look);
            }
            laid.rows
                .iter()
                .map(|row| render(row, look.color))
                .collect::<Vec<_>>()
        };
        let rows = drawn(PLAIN_LOOK, Some("worker"));
        assert_eq!(
            rows[2],
            pad("\u{25c9} worker", "working   4m").replacen(' ', "\u{25b8}", 1)
        );
        assert!(
            rows[2].starts_with("\u{25b8}\u{25c9} worker"),
            "{:?}",
            rows[2]
        );
        let rows = drawn(ascii, Some("worker"));
        assert!(rows[2].starts_with(">* worker"), "{:?}", rows[2]);
        let rows = drawn(color, Some("worker"));
        let bare = drawn(color, None);
        assert!(
            rows[2].starts_with(
                "\u{25b8}\u{1b}[7;32m\u{25c9}\u{1b}[0m\u{1b}[7m \u{1b}[0m\u{1b}[7;34mworker\u{1b}[0m"
            ),
            "{:?}",
            rows[2]
        );
        assert!(
            rows[2].contains("\u{1b}[7;32mworking\u{1b}[0m"),
            "{:?}",
            rows[2]
        );
        for (at, (got, want)) in rows.iter().zip(&bare).enumerate() {
            if at != 2 {
                assert_eq!(got, want, "{at}");
            }
        }
        // A later agent: every span after the marker is reversed, its model, badge and age included.
        let sgr = |codes: &str, text: &str| format!("\u{1b}[{codes}m{text}\u{1b}[0m");
        let rows = drawn(color, Some("writer"));
        assert_eq!(
            rows[5],
            [
                "\u{25b8}".to_string(),
                sgr("7;32", "\u{25c9}"),
                sgr("7", " "),
                sgr("7;35", "writer"),
                sgr("2;7", " [sonnet]"),
                sgr("7", "  "),
                sgr("1;7;34", "\u{2709} 2"),
                sgr("7", "    "),
                sgr("7;32", "working"),
                sgr("7", "   3m"),
            ]
            .concat()
        );
        for (at, (got, want)) in rows.iter().zip(&bare).enumerate() {
            if at != 5 {
                assert_eq!(got, want, "{at}");
            }
        }
        let rows = drawn(PLAIN_LOOK, Some("writer"));
        assert_eq!(
            rows[5],
            pad("\u{25c9} writer [sonnet]  \u{2709} 2", "working   3m")
                .replacen(' ', "\u{25b8}", 1)
        );
        // An unknown name marks nothing.
        let mut laid = lay(board.as_object(), &[], W40, color);
        let before = laid.rows.clone();
        mark(&mut laid, "nobody", color);
        assert_eq!(laid.rows, before);
    }

    #[test]
    fn agent_text_never_reaches_the_frame_as_controls() {
        let evil = "a\u{1b}[31mred\u{1b}]8;;http://x\u{7}link\u{1b}]8;;\u{7}\u{9b}2J\u{85}\u{7f}z";
        let board = json!({"running": 1, "endedTotal": 1, "agents": [
            {"name": evil, "state": "running", "doing": evil, "model": evil, "age": 5, "unread": 0, "color": evil},
            {"name": evil, "state": "exited", "exitCode": evil, "age": 5}
        ]});
        let size = Size {
            width: 200,
            height: 40,
        };
        for look in [
            PLAIN_LOOK,
            Look {
                color: true,
                ..PLAIN_LOOK
            },
        ] {
            let text = frame(&layout(board.as_object(), &[], size, look), size, look);
            // Only the frame's own controls and SGR are escapes; nothing else is a control.
            let rest = strip_sgr(
                &[
                    "\u{1b}[?2026h",
                    "\u{1b}[?2026l",
                    "\u{1b}[H",
                    "\u{1b}[K",
                    "\u{1b}[J",
                ]
                .iter()
                .fold(text.replace("\r\n", "\n"), |t, c| t.replace(c, "")),
            );
            let bad: Vec<char> = rest
                .chars()
                .filter(|&c| c != '\n' && (c < ' ' || ('\u{7f}'..='\u{9f}').contains(&c)))
                .collect();
            assert_eq!(bad, Vec::<char>::new(), "{rest:?}");
            assert!(rest.contains("a[31mred]8;;http://xlink]8;;2Jz"), "{rest:?}");
        }
        // Every C0, DEL and C1 code point goes, one at a time, before the width is counted.
        for code in (0u32..32).chain([127]).chain(128..160) {
            let name = format!("a{}b", char::from_u32(code).unwrap());
            let board = json!({"running": 0, "endedTotal": 1, "agents": [{"name": name, "state": "stopped", "age": 60}]});
            assert_eq!(
                plain(&board, W40)[5],
                pad("- ab", "stopped   1m"),
                "{code:04X}"
            );
        }
    }

    #[test]
    fn frame_bytes() {
        let size = Size {
            width: 40,
            height: 20,
        };
        let rows = layout(mock().as_object(), &[], size, PLAIN_LOOK);
        let text = frame(&rows, size, PLAIN_LOOK);
        let (head, tail) = ("\u{1b}[?2026h\u{1b}[H", "\r\n\u{1b}[J\u{1b}[?2026l");
        assert!(text.starts_with(head) && text.ends_with(tail), "{text:?}");
        assert!(!text.contains("\u{1b}[2J"));
        let body = &text[head.len()..text.len() - tail.len()];
        let lines: Vec<&str> = body.split("\r\n").collect();
        assert_eq!(lines.len(), 16);
        assert!(lines.iter().all(|l| l.ends_with("\u{1b}[K")), "{lines:?}");
        // At the pane's height the cursor stays on the last row: no line feed after it, so nothing scrolls.
        let short = Size {
            width: 40,
            height: 3,
        };
        let text = frame(
            &layout(mock().as_object(), &[], short, PLAIN_LOOK),
            short,
            PLAIN_LOOK,
        );
        assert_eq!(text.matches("\r\n").count(), 2);
        assert!(text.ends_with("\u{1b}[K\u{1b}[J\u{1b}[?2026l"), "{text:?}");
        // TERM=dumb: the rows and a blank line, no ESC; forced color keeps SGR only.
        let dumb = Look {
            dumb: true,
            ..PLAIN_LOOK
        };
        let text = frame(&rows, size, dumb);
        let plain_rows = format_board(mock().as_object(), &[], size, PLAIN_LOOK);
        assert_eq!(text, format!("{}\r\n\r\n", plain_rows.join("\r\n")));
        assert!(!text.contains('\u{1b}'));
        let forced = Look {
            dumb: true,
            color: true,
            ..PLAIN_LOOK
        };
        let text = frame(&layout(mock().as_object(), &[], size, forced), size, forced);
        assert!(text.contains("\u{1b}[1mmux-agents\u{1b}[0m"));
        assert!(!strip_sgr(&text).contains('\u{1b}'), "{text:?}");
        // The switches from the environment.
        let env = |pairs: &[(&str, &str)]| {
            crate::ctx::Env::from_pairs(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())))
        };
        let look = |color, ascii, dumb| Look { color, ascii, dumb };
        assert_eq!(
            [
                Look::from_env(&env(&[("TERM", "dumb")]), true),
                Look::from_env(&env(&[("TERM", "dumb"), ("FORCE_COLOR", "1")]), true),
                Look::from_env(&env(&[("NO_COLOR", "1"), ("MUX_AGENTS_ASCII", "1")]), true),
                Look::from_env(&env(&[]), true),
                Look::from_env(&env(&[]), false),
            ],
            [
                look(false, false, true),
                look(true, false, true),
                look(false, true, false),
                look(true, false, false),
                look(false, false, false),
            ]
        );
    }

    #[test]
    fn popup_pages_the_file_and_deletes_it_quoted_per_os() {
        use crate::state::root::OsKind;
        let unix = popup_argv("%7", "/s/my state/peek-1.txt", OsKind::Unix);
        assert_eq!(
            unix,
            [
                "display-popup",
                "-t",
                "%7",
                "-w",
                "80%",
                "-h",
                "80%",
                "-E",
                "/bin/sh",
                "-c",
                r#"{ rm -f "/s/my state/peek-1.txt"; less -R +G; } < "/s/my state/peek-1.txt""#
            ]
        );
        // Only plain paths reach it: no format, expansion, quote or separator character, and \ only on Windows.
        assert!(plain_path(
            "/Users/a b/.local/state/mux-agents/s-1",
            OsKind::Unix
        ));
        assert!(plain_path(
            r"C:\Users\a b\AppData\mux-agents",
            OsKind::Windows
        ));
        for bad in [
            "/s/#{@x}", "/s/a'b", "/s/a\"b", "/s/$HOME", "/s/a`b`", "/s/a&b", "/s/a;b", "/s/a|b",
            "/s/%x%", "/s/a!b", "/s/a(b)", "/s/a<b", "/s/a\nb", r"/s/a\b",
        ] {
            assert!(!plain_path(bad, OsKind::Unix), "{bad}");
        }
        assert!(!plain_path(r"C:\qa&echo x&rem", OsKind::Windows));
        assert!(!plain_path(r"C:\a%PATH%", OsKind::Windows));
        let windows = popup_argv("%7", r"C:\s\peek-1.txt", OsKind::Windows);
        assert_eq!(
            windows[8],
            r#"cmd /d /c "type "C:\s\peek-1.txt" & del "C:\s\peek-1.txt" & pause""#
        );
    }

    // The Windows popup command run as the popup runs it, through cmd, on a real Windows host: it shows the text,
    // the file is deleted before its `pause` takes a key (here the end of stdin), from a folder with a space.
    #[cfg(windows)]
    #[test]
    fn windows_popup_command_shows_then_deletes_the_file() {
        use crate::state::root::OsKind;
        let dir = std::env::temp_dir().join(format!("mux peek {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("peek-1.txt");
        std::fs::write(&file, "line one\n-- peek: w1 (q closes) --\n").unwrap();
        let path = file.to_str().unwrap();
        assert!(plain_path(path, OsKind::Windows), "{path}");
        let argv = popup_argv("%7", path, OsKind::Windows);
        let command = argv.last().unwrap();
        // As the popup's shell gets it: one command line, cmd's own quoting, no Rust argument quoting.
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new(std::env::var("ComSpec").unwrap())
            .raw_arg(format!("/d /c {command}"))
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("-- peek: w1 (q closes) --"), "{out:?}");
        assert!(!file.exists(), "{out:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn peek_captures_the_agents_own_pane_into_a_private_file_and_opens_a_popup() {
        let mut w = World::human("");
        // The test root is canonicalized, which on Windows adds `\\?\`; the real root (%LOCALAPPDATA%) has none.
        if let Some(plain) = w
            .ctx
            .state_root
            .to_str()
            .and_then(|r| r.strip_prefix(r"\\?\"))
        {
            w.ctx.state_root = plain.into();
        }
        let caller = w.caller();
        w.agent_record(ID, json!({"name": "worker", "pane": "%9"}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        w.fakes
            .mux_first("show-buffer", 0, "line one\nline two\n", "");
        w.fakes.clear_log();
        let target = Target {
            name: "worker".into(),
            pane: "%9".into(),
        };
        peek(&w.ctx, &caller, &target).unwrap();
        let calls = w.fakes.mux_calls();
        // The capture is the guarded one (only while %9 is its own launch), then the popup over the bar's client.
        assert!(
            calls
                .iter()
                .any(|c| c.iter().any(|a| a.contains("capture-pane -J -t %9"))),
            "{calls:?}"
        );
        // The popup runs in the background (display-popup answers only when it closes), each argument quoted.
        let run = calls.iter().find(|c| c[1] == "run-shell").unwrap();
        assert_eq!(run[2..4], ["-b", "-C"]);
        let popup = &run[4];
        assert!(
            popup.starts_with("'display-popup' '-t' '%1' '-w' '80%' '-h' '80%' '-E' '"),
            "{popup}"
        );
        // The text is in a private peek file of the state folder, which the popup deletes when closed.
        let folder = crate::agents::server_dir(&w.ctx, &caller);
        let files: Vec<_> = std::fs::read_dir(&folder)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("peek-"))
            .collect();
        assert_eq!(files.len(), 1);
        assert_eq!(
            std::fs::read_to_string(files[0].path()).unwrap(),
            "line one\nline two\n-- peek: worker (q closes) --\n"
        );
        assert!(
            popup.contains(files[0].path().to_str().unwrap()),
            "{popup:?}"
        );
        // A second peek never deletes another's file: it may belong to a popup still queued.
        let first = files[0].path();
        peek(&w.ctx, &caller, &target).unwrap();
        assert!(first.exists());
        let peeks = |folder: &std::path::Path| {
            std::fs::read_dir(folder)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().starts_with("peek-"))
                .count()
        };
        assert_eq!(peeks(&folder), 2);
        // No popup: its file goes and peek fails; a moved agent is never read.
        w.fakes.mux_first("run-shell", 1, "", "no current client");
        assert!(peek(&w.ctx, &caller, &target).is_err());
        assert_eq!(peeks(&folder), 2);
        // A state folder whose path the multiplexer or a shell would read: no capture, no popup.
        let mut odd = World::human("");
        odd.ctx.state_root = odd.ctx.state_root.join("#{@x}");
        odd.agent_record(ID, json!({"name": "worker", "pane": "%9"}));
        odd.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        odd.fakes.clear_log();
        assert!(peek(&odd.ctx, &odd.caller(), &target).is_err());
        assert!(!odd.fakes.mux_calls().iter().any(|c| {
            c.iter()
                .any(|a| a.contains("capture-pane") || a == "run-shell")
        }));
        let moved = Target {
            name: "worker".into(),
            pane: "%4".into(),
        };
        w.fakes.clear_log();
        assert!(peek(&w.ctx, &caller, &moved).is_err());
        assert!(
            !w.fakes
                .mux_calls()
                .iter()
                .any(|c| c.iter().any(|a| a.contains("capture-pane")))
        );
    }

    #[test]
    fn viewing_marks_the_done_spell_left_behind() {
        let board = |activity: &str, changed: Value| {
            json!({"running": 1, "agents": [
                {"name": "w", "state": "running", "pane": "%9", "activity": activity, "changedAt": changed},
                {"name": "x", "state": "exited", "pane": "%8", "activity": "done", "changedAt": "old"}
            ]})
        };
        let at = |t: &str| json!(format!("2026-11-08T{t}.0000000Z"));
        let mut v = Viewing::default();
        let shown = |p: &str| Some(p.to_string());
        // Marks what is queued, answering `ok`; returns what was asked.
        let mark = |v: &mut Viewing, ok: bool| {
            let mut asked = Vec::new();
            v.mark(|pane, changed| {
                asked.push(format!("{pane} {changed}"));
                ok
            });
            asked
        };
        // Working while looked at, then done while still looked at: held, nothing marked yet.
        v.tick(shown("%9"), board("working", Value::Null).as_object());
        v.tick(shown("%9"), board("done", at("12:00:00")).as_object());
        v.tick(shown("%9"), board("done", at("12:00:00")).as_object());
        assert!(mark(&mut v, true).is_empty());
        // Leaving marks that spell seen, once; a failed mark is asked again on the next tick.
        v.tick(shown("%1"), board("done", at("12:00:00")).as_object());
        assert_eq!(mark(&mut v, false), ["%9 2026-11-08T12:00:00.0000000Z"]);
        assert_eq!(mark(&mut v, true), ["%9 2026-11-08T12:00:00.0000000Z"]);
        assert!(mark(&mut v, true).is_empty());
        // A spell that ended while the person was elsewhere is not marked by leaving another pane.
        v.tick(None, board("done", at("12:01:00")).as_object());
        assert!(mark(&mut v, true).is_empty());
        // Looked at only while it worked: leaving marks nothing.
        v.tick(shown("%9"), board("working", Value::Null).as_object());
        v.tick(shown("%1"), board("done", at("12:02:00")).as_object());
        assert!(mark(&mut v, true).is_empty());
        // A detached client (no pane shown) is leaving, even with no board; an ended row is never remembered.
        v.tick(shown("%9"), board("done", at("12:02:00")).as_object());
        v.tick(None, None);
        assert_eq!(mark(&mut v, true), ["%9 2026-11-08T12:02:00.0000000Z"]);
        v.tick(shown("%8"), board("done", at("12:03:00")).as_object());
        v.tick(shown("%1"), None);
        assert!(mark(&mut v, true).is_empty());
    }

    #[test]
    fn alerts_tell_of_needs_you_and_done_while_looking_elsewhere() {
        let board = |a: &str, b: &str| {
            json!({"running": 2, "agents": [
                {"name": "a", "state": "running", "pane": "%1", "activity": a},
                {"name": "b", "state": "running", "pane": "%2", "activity": b},
                {"name": "x", "state": "exited", "pane": "%3", "activity": "done"}
            ]})
        };
        let mut al = Alerts::default();
        let news = |al: &mut Alerts, a: &str, b: &str, shown: Option<&str>| {
            al.tick(board(a, b).as_object(), shown)
                .into_iter()
                .map(|(n, w)| format!("{n}={w}"))
                .collect::<Vec<_>>()
        };
        // The first sight is no news, whatever the state.
        assert!(news(&mut al, "blocked", "working", None).is_empty());
        // b finishes, a stays blocked: only b's change.
        assert_eq!(news(&mut al, "blocked", "done", None), ["b=done"]);
        // Nothing changed: nothing.
        assert!(news(&mut al, "blocked", "done", None).is_empty());
        // a works, then needs the person again while they look at a's pane: no message.
        assert!(news(&mut al, "working", "done", None).is_empty());
        assert!(news(&mut al, "blocked", "done", Some("%1")).is_empty());
        // idle and working are never news; an unread board changes nothing.
        assert!(news(&mut al, "idle", "working", None).is_empty());
        assert!(al.tick(None, None).is_empty());
        assert_eq!(
            news(&mut al, "blocked", "working", Some("%2")),
            ["a=blocked"]
        );
    }

    #[test]
    fn alert_sends_one_message_as_the_option_says() {
        let news = vec![
            ("w1".to_string(), "blocked".to_string()),
            ("w2".to_string(), "done".to_string()),
        ];
        let sent = |option: &str| {
            let w = World::human("");
            let caller = w.caller();
            w.fakes
                .mux_rule(ALERTS_OPTION, 0, &format!("{option}\n"), "");
            w.fakes.clear_log();
            alert(&w.ctx, &caller, &news);
            w.fakes
                .mux_calls()
                .into_iter()
                .filter(|c| c.iter().any(|a| a == "display-message"))
                .map(|c| c[c.len() - 1].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(sent(""), ["mux-agents: w1 needs you, w2 is done"]);
        assert_eq!(sent("on"), ["mux-agents: w1 needs you, w2 is done"]);
        assert_eq!(sent("blocked"), ["mux-agents: w1 needs you"]);
        assert!(sent("off").is_empty());
        // On the session's clients, for 5 s.
        let w = World::human("");
        let caller = w.caller();
        w.fakes.clear_log();
        alert(&w.ctx, &caller, &news[..1]);
        let call = w.fakes.mux_calls().pop().unwrap();
        assert_eq!(
            call[call.len() - 5..],
            ["-t", "$3", "-d", "5000", "mux-agents: w1 needs you"]
        );
    }

    #[test]
    fn shown_pane_needs_an_attached_client() {
        let w = World::human("");
        let caller = w.caller();
        for (out, want) in [
            ("%9|1\n", Some(Some("%9"))),
            ("%9|2\n", Some(Some("%9"))),
            ("%9|0\n", Some(None)),
            ("|0\n", Some(None)),
            ("|1\n", None),
            ("%9\n", None),
            ("%9|x\n", None),
            ("", None),
        ] {
            w.fakes.mux_first(SHOWN_FORMAT, 0, out, "");
            let got = shown_pane(&w.ctx, &caller);
            assert_eq!(got.as_ref().map(|p| p.as_deref()), want, "{out:?}");
        }
        // A failed call is not known either: the bar keeps what it had.
        w.fakes.mux_first(SHOWN_FORMAT, 1, "%9|1\n", "");
        assert_eq!(shown_pane(&w.ctx, &caller), None);
        let call = w.fakes.mux_calls().pop().unwrap();
        assert_eq!(call[call.len() - 3..], ["-t", "$3", SHOWN_FORMAT]);
    }

    #[test]
    fn format_age_units() {
        let ages: Vec<String> = [
            json!(0),
            json!(59),
            json!(60),
            json!(3599),
            json!(3600),
            json!(86399),
            json!(86400),
            json!(null),
        ]
        .iter()
        .map(|v| format_age(Some(v)))
        .collect();
        assert_eq!(ages, ["0s", "59s", "1m", "59m", "1h", "23h", "1d", "?"]);
        assert_eq!(format_age(None), "?");
    }

    // ---- watch

    // The bar's own pane %7 (human, untagged); live worker %9 with a status, ended `done` (exited 0).
    fn watch_world(probes: &[&'static str]) -> World {
        let w = World::human("");
        w.fakes
            .mux_first("#{pid}|#{pane_pid}", 0, "900|100|$3|@1|\n", "");
        let mut env: Vec<(String, String)> = w
            .ctx
            .env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        for pair in env.iter_mut().filter(|(k, _)| k == "TMUX_PANE") {
            pair.1 = "%7".into();
        }
        // Plain frames: the watch draws to a terminal, so it would color.
        env.push(("NO_COLOR".into(), "1".into()));
        let mut w = w;
        w.ctx.env = crate::ctx::Env::from_pairs(env);
        w.clock("1970-01-01T00:04:00Z");
        crate::agents::core_fixture::member(
            &w,
            ID,
            json!({"name":"worker","pane":"%9","startCommand":crate::agents::fixture::launch_command("%9")}),
        );
        let worker = w.folder(ID);
        write(
            &worker.join("status.json"),
            json!({"text": "testing the parser", "at": "x"}),
        );
        let done = w.agent_record("bbbbbbbbbbbb", json!({"name": "done", "pane": "%8"}));
        write(
            &done.join("notice.json"),
            json!({"kind": "exited", "exitCode": 0, "at": "1970-01-01T00:03:00Z"}),
        );
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        let queue = Rc::new(RefCell::new(
            probes.iter().copied().collect::<VecDeque<_>>(),
        ));
        w.fakes
            .mux_script("#{pane_id}|#{@agent-role}", move |argv| {
                assert_eq!(argv[4], "%7", "the bar asks its own pane");
                let mut queue = queue.borrow_mut();
                let answer = if queue.len() > 1 {
                    queue.pop_front().unwrap()
                } else {
                    *queue.front().unwrap()
                };
                match answer {
                    "live" => Ok(out(0, "%7|sidebar|40|24\n", "")),
                    // The pane 55 columns wide, as a drag or the split leaves it before the keeper acts.
                    "wide" => Ok(out(0, "%7|sidebar|55|24\n", "")),
                    // The pane 4 rows high.
                    "short" => Ok(out(0, "%7|sidebar|40|4\n", "")),
                    // The pane itself, without the bar's tag: watch run by hand in some pane.
                    "untagged" => Ok(out(0, "%7||40|24\n", "")),
                    "gone" => Ok(out(0, "|||\n", "")),
                    "fail" => Ok(out(1, "", "no server")),
                    _ => refuse(1, "endpoint", "the rmux endpoint did not answer"),
                }
            });
        w
    }

    fn probes(w: &World) -> usize {
        w.fakes
            .mux_calls()
            .iter()
            .filter(|c| c.last().is_some_and(|a| a == WATCH_PROBE))
            .count()
    }

    // The frames of one watch run; a runaway loop fails instead of hanging.
    fn watch_run(w: &World) -> (&'static str, Vec<String>) {
        let caller = w.caller();
        let mut frames = Vec::new();
        let ended = run_watch(&w.ctx, &caller, Box::new(NoInput), &mut |frame: &str| {
            frames.push(frame.to_string());
            assert!(frames.len() <= 100, "runaway");
            Ok(())
        });
        (ended, frames)
    }

    // The watch world's board at 40x24, plain.
    fn board_rows() -> Vec<String> {
        vec![
            pad("mux-agents", "1 working / 1 ended"),
            String::new(),
            pad("\u{25c9} worker", "working   4m"),
            "   testing the parser".into(),
            String::new(),
            format!(" ended {}", "-".repeat(31)),
            pad("\u{2713} done", "done   4m"),
        ]
    }

    // A frame of `rows` in a 40-column pane `height` rows high.
    fn screen(rows: &[String], height: usize) -> String {
        let lines: Vec<String> = rows
            .iter()
            .map(|r| {
                if crate::paint::width(r) < 40 {
                    format!("{r}\u{1b}[K")
                } else {
                    r.clone()
                }
            })
            .collect();
        let end = if rows.len() < height {
            "\r\n\u{1b}[J"
        } else {
            "\u{1b}[J"
        };
        format!(
            "\u{1b}[?2026h\u{1b}[H{}{end}\u{1b}[?2026l",
            lines.join("\r\n")
        )
    }

    fn board_frame() -> String {
        screen(&board_rows(), 24)
    }

    // The board with notes under it, after a blank line.
    fn noted_frame(notes: &[&str]) -> String {
        let mut rows = board_rows();
        rows.push(String::new());
        rows.extend(notes.iter().map(|n| format!(" {n}")));
        screen(&rows, 24)
    }

    #[test]
    fn watch_follows_the_pane_size_each_tick() {
        let w = watch_world(&["live", "short", "live", "gone"]);
        let (ended, frames) = watch_run(&w);
        assert_eq!(ended, "pane-closed");
        let short = [
            pad("mux-agents", "1 working / 1 ended"),
            pad("\u{25c9} worker", "working   4m"),
            "   testing the parser".to_string(),
            "   +1 more  (list -All)".to_string(),
        ];
        assert_eq!(frames, [board_frame(), screen(&short, 4), board_frame()]);
    }

    #[test]
    fn watch_pane_closed() {
        let w = watch_world(&["live", "live", "gone"]);
        let (ended, frames) = watch_run(&w);
        assert_eq!(ended, "pane-closed");
        assert_eq!(frames, [board_frame(), board_frame()]);
        assert_eq!(w.fakes.slept(), [2000, 2000]);
        assert_eq!(probes(&w), 3);
        // A thrown or failed probe is unknown, not gone: its frame says refresh failed, at the default narrowest
        // width (40, the board frames' width) until the pane answers its size.
        let w = watch_world(&["throw", "fail", "live", "gone"]);
        let (ended, frames) = watch_run(&w);
        assert_eq!(FALLBACK.width, 40);
        let marked = noted_frame(&["refresh failed"]);
        assert_eq!(ended, "pane-closed");
        assert_eq!(frames, [marked.clone(), marked, board_frame()]);
        assert_eq!(probes(&w), 4);
    }

    #[test]
    fn watch_probe_failed_after_30_unknown() {
        let w = watch_world(&["fail"]);
        let (ended, frames) = watch_run(&w);
        assert_eq!((ended, frames.len(), probes(&w)), ("probe-failed", 29, 30));
        // A probe that names its pane resets the count.
        let mut answers = vec!["fail"; 29];
        answers.push("live");
        answers.extend(vec!["fail"; 29]);
        answers.push("gone");
        let w = watch_world(&answers);
        let (ended, frames) = watch_run(&w);
        assert_eq!((ended, frames.len(), probes(&w)), ("pane-closed", 59, 60));
    }

    #[test]
    fn watch_output_closed() {
        let w = watch_world(&["live", "live", "gone"]);
        let caller = w.caller();
        let ended = run_watch(&w.ctx, &caller, Box::new(NoInput), &mut |_| {
            Err(io::Error::other("EIO"))
        });
        assert_eq!(ended, "output-closed");
        assert_eq!(probes(&w), 1);
        assert!(w.fakes.slept().is_empty());
        // Probe and board both failing still write a frame, so a dead terminal is found at once.
        let w = watch_world(&["fail"]);
        w.fakes.mux_first("list-panes", 1, "", "lock");
        let caller = w.caller();
        let ended = run_watch(&w.ctx, &caller, Box::new(NoInput), &mut |_| {
            Err(io::Error::other("EIO"))
        });
        assert_eq!((ended, probes(&w)), ("output-closed", 1));
    }

    #[test]
    fn watch_refresh_failed_keeps_last() {
        let w = watch_world(&["live", "live", "live", "gone"]);
        let listed = Rc::new(Cell::new(0));
        let seen = listed.clone();
        w.fakes.mux_script("list-panes", move |argv| {
            // Only the board's session list counts; the bar's own layout read is not answered.
            if !argv.iter().any(|a| a == "-s") {
                return Ok(out(1, "", "lock"));
            }
            seen.set(seen.get() + 1);
            if seen.get() == 2 {
                let rows = ["%9|@5|300|aaaaaaaaaaaa|worker"];
                Ok(out(
                    0,
                    &crate::agents::fixture::listed(argv, &rows, "\n"),
                    "",
                ))
            } else {
                Ok(out(1, "", "lock"))
            }
        });
        let (ended, frames) = watch_run(&w);
        assert_eq!(ended, "pane-closed");
        assert_eq!(
            frames,
            [
                screen(
                    &[
                        pad("mux-agents", "? running"),
                        String::new(),
                        " refresh failed".to_string()
                    ],
                    24
                ),
                board_frame(),
                noted_frame(&["refresh failed"]),
            ]
        );
        // The board is read as the bar's pane, with the ended rows and their exit codes.
        assert_eq!(listed.get(), 3);
    }

    #[test]
    fn watch_alerts_once_only_when_it_knows_who_looks_and_reads_the_option_only_then() {
        // Four ticks. worker (%9) works on the first and needs the person from the second on. The person looks at %9
        // on the first tick and has moved to %1 by the third; the second tick's probe goes unanswered.
        let w = watch_world(&["live", "live", "live", "live", "gone"]);
        let report = w.folder(ID).join("activity.json");
        let tick = Rc::new(Cell::new(0));
        let ticks = tick.clone();
        // Each tick's board read sees that tick's report.
        w.fakes.mux_script("list-panes", move |argv| {
            if !argv.iter().any(|a| a == "-s") {
                return Ok(out(1, "", "lock"));
            }
            ticks.set(ticks.get() + 1);
            let state = if ticks.get() == 1 {
                "working"
            } else {
                "blocked"
            };
            std::fs::write(
                &report,
                format!(
                    r#"{{"state":"{state}","kind":null,"since":"1970-01-01T00:0{}:00.0000000Z"}}"#,
                    ticks.get()
                ),
            )
            .unwrap();
            let rows = ["%9|@5|300|aaaaaaaaaaaa|worker"];
            Ok(out(
                0,
                &crate::agents::fixture::listed(argv, &rows, "\n"),
                "",
            ))
        });
        let probes = Rc::new(Cell::new(0));
        let seen = probes.clone();
        w.fakes
            .mux_script("#{pane_id}|#{session_attached}", move |_| {
                seen.set(seen.get() + 1);
                Ok(match seen.get() {
                    1 => out(0, "%9|1\n", ""),
                    2 => out(1, "", "no server"),
                    _ => out(0, "%1|1\n", ""),
                })
            });
        let (ended, _) = watch_run(&w);
        assert_eq!(ended, "pane-closed");
        let calls = w.fakes.mux_calls();
        let named = |needle: &str| {
            calls
                .iter()
                .filter(|c| c.iter().any(|a| a.contains(needle)))
                .count()
        };
        // worker turned blocked while who looks was unknown; the next tick that knows tells it, once.
        assert_eq!(named("mux-agents: worker needs you"), 1, "{calls:?}");
        // The option is read only to send that message, never on a quiet tick.
        assert_eq!(named(ALERTS_OPTION), 1, "{calls:?}");
        assert_eq!(tick.get(), 4);
    }

    #[test]
    fn watch_requires_terminal_usage_2() {
        let w = watch_world(&["gone"]);
        w.fakes.clear_log();
        let request = w.request(&["watch"]);
        let refused = watch_on(&w.ctx, &request, false, &no_input, &mut |_| {
            panic!("drew to a pipe")
        })
        .unwrap_err();
        assert_eq!(
            (refused.code, refused.check, refused.message.as_str()),
            (
                2,
                "usage",
                "watch draws only to a terminal: it runs in the side-bar pane"
            )
        );
        assert!(w.fakes.mux_calls().is_empty());
        let reply = watch_on(&w.ctx, &request, true, &no_input, &mut |_| Ok(())).unwrap();
        assert_eq!(
            Value::Object(reply),
            json!({"pane": "%7", "ended": "pane-closed"})
        );
    }

    fn no_input() -> Box<dyn Input> {
        Box::new(NoInput)
    }

    // ---- watch's input

    /// An input that answers `steps` in turn, each `(ms, answer)` due that long after the one before, and records
    /// each timeout. A step due within the timeout moves the fake clock to it and answers it; else the whole timeout
    /// passes, without a sleep, and times out.
    struct Script {
        fakes: Fakes,
        steps: VecDeque<(u64, Waited)>,
        waits: Rc<RefCell<Vec<u64>>>,
    }

    impl Script {
        fn new(fakes: &Fakes, steps: Vec<(u64, Waited)>) -> Self {
            Script {
                fakes: fakes.clone(),
                steps: steps.into(),
                waits: Default::default(),
            }
        }
    }

    impl Input for Script {
        fn wait(&mut self, _clock: &dyn crate::ctx::Clock, timeout: Duration) -> Waited {
            let ms = timeout.as_millis() as u64;
            self.waits.borrow_mut().push(ms);
            match self.steps.front_mut() {
                Some((due, _)) if *due <= ms => {
                    self.fakes.advance(Duration::from_millis(*due));
                    self.steps.pop_front().unwrap().1
                }
                Some((due, _)) => {
                    *due -= ms;
                    self.fakes.advance(timeout);
                    Waited::Timeout
                }
                None => {
                    self.fakes.advance(timeout);
                    Waited::Timeout
                }
            }
        }
    }

    // One watch run reading `input`: how it ended, its frames and the fake time (ms) each was written at.
    fn watch_with(w: &World, input: Box<dyn Input>) -> (&'static str, Vec<String>, Vec<u64>) {
        let caller = w.caller();
        let mut frames = Vec::new();
        let mut times = Vec::new();
        let ended = run_watch(&w.ctx, &caller, input, &mut |frame: &str| {
            frames.push(frame.to_string());
            let at = w.ctx.clock.now().duration_since(SystemTime::UNIX_EPOCH);
            times.push(at.unwrap().as_millis() as u64);
            assert!(frames.len() <= 100, "runaway");
            Ok(())
        });
        w.fakes.hc_done().unwrap();
        (ended, frames, times)
    }

    fn diffs(times: &[u64]) -> Vec<u64> {
        times.windows(2).map(|t| t[1] - t[0]).collect()
    }

    #[test]
    fn watch_under_constant_input_still_draws_every_2s() {
        let w = watch_world(&["live", "live", "live", "gone"]);
        let steps = (0..40)
            .map(|_| (300, Waited::Bytes(b"x".to_vec())))
            .collect();
        let script = Script::new(&w.fakes, steps);
        let waits = script.waits.clone();
        let (ended, frames, times) = watch_with(&w, Box::new(script));
        assert_eq!((ended, frames.len()), ("pane-closed", 3));
        assert_eq!(diffs(&times), [2000, 2000]);
        let waits = waits.borrow();
        assert!(waits.iter().all(|&ms| ms > 0 && ms <= 2000), "{waits:?}");
        // Each tick waits for six reads 300 ms apart, then the 200 ms left.
        assert_eq!(waits[..7], [2000, 1700, 1400, 1100, 800, 500, 200]);
        assert!(w.fakes.slept().is_empty());
    }

    #[test]
    fn watch_ctrl_c_ends_interrupted() {
        let w = watch_world(&["live"]);
        let script = Script::new(&w.fakes, vec![(500, Waited::Bytes(vec![3]))]);
        let (ended, frames, _) = watch_with(&w, Box::new(script));
        assert_eq!((ended, frames.len(), probes(&w)), ("interrupted", 1, 1));
    }

    #[test]
    fn watch_lone_escape_waits_50ms() {
        let w = watch_world(&["live", "live", "live", "gone"]);
        let script = Script::new(&w.fakes, vec![(100, Waited::Bytes(b"\x1b".to_vec()))]);
        let waits = script.waits.clone();
        let (ended, frames, times) = watch_with(&w, Box::new(script));
        assert_eq!((ended, frames.len()), ("pane-closed", 3));
        // 50 ms after the ESC it counts as the key, which draws again at once (#61).
        assert_eq!(*waits.borrow(), [2000, 50, 2000, 2000]);
        assert_eq!(diffs(&times), [150, 2000]);
    }

    #[test]
    fn watch_keeps_drawing_after_input_closes() {
        let w = watch_world(&["live", "live", "live", "gone"]);
        let script = Script::new(&w.fakes, vec![(100, Waited::Closed)]);
        let waits = script.waits.clone();
        let (ended, frames, times) = watch_with(&w, Box::new(script));
        assert_eq!((ended, frames.len()), ("pane-closed", 3));
        assert_eq!(*waits.borrow(), [2000]);
        // The rest of the tick, then every tick, is slept as without input.
        assert_eq!(w.fakes.slept()[..2], [1900, 2000]);
        assert_eq!(diffs(&times), [2000, 2000]);
    }

    #[test]
    fn watch_waits_2s_from_the_end_of_a_slow_write() {
        let w = watch_world(&["live", "live", "gone"]);
        let script = Script::new(&w.fakes, vec![]);
        let waits = script.waits.clone();
        let caller = w.caller();
        let mut frames = 0;
        let ended = run_watch(&w.ctx, &caller, Box::new(script), &mut |_| {
            frames += 1;
            assert!(frames <= 100, "runaway");
            // The terminal takes 250 ms to take the frame.
            w.fakes.advance(Duration::from_millis(250));
            Ok(())
        });
        assert_eq!((ended, frames), ("pane-closed", 2));
        assert_eq!(*waits.borrow(), [2000, 2000]);
    }

    #[test]
    fn watch_late_lone_escape_waits_only_the_time_left() {
        let w = watch_world(&["live", "live", "live", "gone"]);
        let script = Script::new(&w.fakes, vec![(1990, Waited::Bytes(b"\x1b".to_vec()))]);
        let waits = script.waits.clone();
        let (ended, frames, times) = watch_with(&w, Box::new(script));
        assert_eq!((ended, frames.len()), ("pane-closed", 3));
        // 10 ms are left of the tick, so the ESC waits 10 ms, not 50, and the next frame is not late.
        assert_eq!(*waits.borrow(), [2000, 10, 2000, 2000]);
        assert_eq!(diffs(&times), [2000, 2000]);
    }

    // A terminal set up on a FakeTerm answering `reads`, and its log.
    fn term_input(reads: Vec<io::Result<Waited>>) -> (Box<dyn Input>, Rc<RefCell<Vec<String>>>) {
        let term = FakeTerm {
            reads: reads.into(),
            ..FakeTerm::new()
        };
        let log = term.log.clone();
        let input = TermInput::setup(term).expect("every step succeeds");
        (Box::new(input), log)
    }

    fn logged(bytes: &[u8]) -> String {
        format!("write {}", bytes.escape_ascii())
    }

    // One way watch ends: how it ends, the probe answers, the terminal's reads and the frame write.
    type End = (
        &'static str,
        &'static [&'static str],
        Vec<io::Result<Waited>>,
        fn(&str) -> io::Result<()>,
    );

    #[test]
    fn watch_restores_after_every_exit() {
        let restored = [logged(DISABLE), "restore".into(), "disarm".into()];
        let ends: [End; 4] = [
            ("pane-closed", &["live", "gone"], vec![], |_| Ok(())),
            ("output-closed", &["live"], vec![], |_| {
                Err(io::Error::other("EIO"))
            }),
            ("probe-failed", &["fail"], vec![], |_| Ok(())),
            (
                "interrupted",
                &["live"],
                vec![Ok(Waited::Bytes(vec![3]))],
                |_| Ok(()),
            ),
        ];
        for (want, answers, reads, write) in ends {
            let w = watch_world(answers);
            let (input, log) = term_input(reads);
            let caller = w.caller();
            let mut frames = 0;
            let ended = run_watch(&w.ctx, &caller, input, &mut |frame| {
                frames += 1;
                assert!(frames <= 100, "runaway");
                write(frame)
            });
            assert_eq!(ended, want);
            let log = log.borrow();
            assert_eq!(log[log.len() - 3..], restored, "{want}");
            let restores = log.iter().filter(|e| *e == "restore").count();
            assert_eq!(restores, 1, "{want}");
        }
        // A panic in the loop unwinds through it, and the input is still dropped.
        let w = watch_world(&["live"]);
        let (input, log) = term_input(vec![]);
        let caller = w.caller();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_watch(&w.ctx, &caller, input, &mut |_| panic!("write"))
        }));
        assert!(panicked.is_err());
        assert_eq!(
            *log.borrow(),
            [
                "save",
                "arm",
                "raw",
                &logged(ENABLE),
                &restored[0],
                "restore",
                "disarm"
            ]
        );
    }

    #[test]
    fn watch_on_keeps_drawing_when_setup_fails() {
        let w = watch_world(&["live", "live", "live", "gone"]);
        let request = w.request(&["watch"]);
        let log: Rc<RefCell<Vec<String>>> = Default::default();
        let opened = Rc::new(Cell::new(0));
        let open = {
            let (log, opened) = (log.clone(), opened.clone());
            move || {
                opened.set(opened.get() + 1);
                let term = FakeTerm {
                    log: log.clone(),
                    fail: Some("raw"),
                    ..FakeTerm::new()
                };
                input::open_with(&Env::default(), true, true, || {
                    TermInput::setup(term).map(|input| Box::new(input) as Box<dyn Input>)
                })
            }
        };
        let mut frames = 0;
        let reply = watch_on(&w.ctx, &request, true, &open, &mut |_| {
            frames += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!((reply["ended"].as_str(), frames), (Some("pane-closed"), 3));
        assert_eq!(opened.get(), 1);
        assert_eq!(*log.borrow(), ["save", "arm", "raw", "restore", "disarm"]);
        assert_eq!(w.fakes.slept(), [2000, 2000, 2000]);
    }

    #[test]
    fn watch_on_reads_the_input_it_opened() {
        let w = watch_world(&["live"]);
        let request = w.request(&["watch"]);
        let opened = Rc::new(Cell::new(0));
        let open = {
            let (fakes, opened) = (w.fakes.clone(), opened.clone());
            move || -> Box<dyn Input> {
                opened.set(opened.get() + 1);
                Box::new(Script::new(&fakes, vec![(500, Waited::Bytes(vec![3]))]))
            }
        };
        let mut frames = 0;
        let reply = watch_on(&w.ctx, &request, true, &open, &mut |_| {
            frames += 1;
            assert!(frames <= 100, "runaway");
            Ok(())
        })
        .unwrap();
        assert_eq!((reply["ended"].as_str(), frames), (Some("interrupted"), 1));
        assert_eq!(opened.get(), 1);
    }

    #[test]
    fn watch_on_opens_the_input_only_once_it_draws() {
        let w = watch_world(&["gone"]);
        let request = w.request(&["watch"]);
        let open = || -> Box<dyn Input> { panic!("opened the terminal") };
        // Refused for a pipe, and refused outside a pane: the terminal is never touched.
        assert!(watch_on(&w.ctx, &request, false, &open, &mut |_| Ok(())).is_err());
        let mut w = w;
        let env: Vec<(String, String)> = w
            .ctx
            .env
            .iter()
            .filter(|(k, _)| *k != "TMUX_PANE")
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        w.ctx.env = Env::from_pairs(env);
        assert!(watch_on(&w.ctx, &request, true, &open, &mut |_| Ok(())).is_err());
    }

    #[test]
    fn watch_poll_failure_restores_then_keeps_drawing() {
        let w = watch_world(&["live", "live", "gone"]);
        let (input, log) = term_input(vec![Err(io::Error::other("EIO"))]);
        let caller = w.caller();
        let ended = run_watch(&w.ctx, &caller, input, &mut |_| {
            log.borrow_mut().push("frame".into());
            Ok(())
        });
        assert_eq!(ended, "pane-closed");
        assert_eq!(
            *log.borrow(),
            [
                "save",
                "arm",
                "raw",
                &logged(ENABLE),
                "frame",
                &logged(DISABLE),
                "restore",
                "disarm",
                "frame"
            ]
        );
        // The failed read took no time: the tick is slept whole, then the next.
        assert_eq!(w.fakes.slept(), [2000, 2000]);
    }

    // ---- clicks and keys (#61)

    // The bar's click read: its width, then its rows.
    const CLICK: &str = "-N display-message -p -t %7 #{pane_width} ; capture-pane -p -t %7";
    const READ_BACK: &str = "#{window_active}";

    // An SGR left press at row `y`.
    fn press(y: u16) -> Waited {
        Waited::Bytes(format!("\x1b[<0;3;{y}M").into_bytes())
    }

    fn key(bytes: &[u8]) -> Waited {
        Waited::Bytes(bytes.to_vec())
    }

    // The watch world whose bar answers the click read with `answer`, and whose worker %9 reads back focused.
    fn click_world(probes: &[&'static str], answer: String) -> World {
        let w = watch_world(probes);
        w.fakes
            .mux_script("capture-pane", move |_| Ok(out(0, &answer, "")));
        w.fakes.mux_first(READ_BACK, 0, "%9|1|1|1\n", "");
        w
    }

    // The click read's answer: `width`, then `rows`.
    fn answer(width: usize, rows: &[String]) -> String {
        format!("{width}\n{}\n", rows.join("\n"))
    }

    fn board_answer() -> String {
        answer(40, &board_rows())
    }

    fn mux_text(w: &World) -> Vec<String> {
        w.fakes.mux_calls().iter().map(|c| c.join(" ")).collect()
    }

    fn click_reads(w: &World) -> usize {
        mux_text(w).iter().filter(|c| *c == CLICK).count()
    }

    // Guarded selects: the if-shell around select-window and select-pane.
    fn selects(w: &World) -> usize {
        w.fakes
            .mux_calls()
            .iter()
            .filter(|c| c[1] == "if-shell")
            .count()
    }

    // The board with worker selected: the marker in its margin and the hint under the board.
    fn marked_frame() -> String {
        let mut rows = board_rows();
        rows[2] = rows[2].replacen(' ', "\u{25b8}", 1);
        rows.push(String::new());
        rows.push(format!(" {HINT}"));
        screen(&rows, 24)
    }

    fn failed_frame() -> String {
        noted_frame(&["focus failed: worker"])
    }

    fn watch_env(w: &mut World, key: &str, value: &str) {
        let mut env: Vec<(String, String)> = w
            .ctx
            .env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        env.push((key.into(), value.into()));
        w.ctx.env = Env::from_pairs(env);
    }

    #[test]
    fn click_focuses_through_one_read_and_guarded_select() {
        let w = click_world(&["live", "live", "live", "gone"], board_answer());
        let script = Script::new(&w.fakes, vec![(300, press(3))]);
        crate::agents::core_fixture::online(&w);
        let (ended, frames, times) = watch_with(&w, Box::new(script));
        assert_eq!((ended, frames.len()), ("pane-closed", 3));
        // The redraw follows the click at once; the next tick is 2 s after it.
        assert_eq!(diffs(&times), [300, 2000]);
        assert_eq!(frames, [board_frame(), board_frame(), board_frame()]);
        let live = crate::agents::registry::Launch {
            command: crate::agents::fixture::launch_command("%9"),
            pid: None,
        }
        .live();
        // The calls in order, the snapshot's pane lists aside.
        let calls: Vec<String> = w
            .fakes
            .mux_calls()
            .iter()
            .filter(|c| c[1] != "list-panes")
            .map(|c| c.join(" "))
            .collect();
        let at = calls.iter().position(|c| c == CLICK).expect("a click read");
        assert_eq!(
            calls[at..at + 3],
            [
                CLICK.to_string(),
                format!("-N if-shell -F -t %9 {live} select-window -t %9 ; select-pane -t %9"),
                format!(
                    "-N display-message -p -t %9 #{{pane_id}}|#{{window_active}}|#{{pane_active}}|{live}"
                ),
            ]
        );
        assert_eq!((click_reads(&w), selects(&w)), (1, 1));
    }

    // The watch world with running agents writer-two and writer as well as worker.
    fn two_writers(w: &World) {
        crate::agents::core_fixture::member(
            w,
            "cccccccccccc",
            json!({"name":"writer-two","pane":"%11","startCommand":crate::agents::fixture::launch_command("%11")}),
        );
        crate::agents::core_fixture::member(
            w,
            "dddddddddddd",
            json!({"name":"writer","pane":"%12","startCommand":crate::agents::fixture::launch_command("%12")}),
        );
        w.panes(&[
            "%9|@5|300|aaaaaaaaaaaa|worker",
            "%11|@6|311|cccccccccccc|writer-two",
            "%12|@7|312|dddddddddddd|writer",
        ]);
    }

    #[test]
    fn next_keys_in_one_read_each_step_on_from_the_last() {
        // `nn` in one read with three running agents: two different agents, in the bar's order, not the same twice.
        let w = click_world(&["live", "live", "live", "gone"], board_answer());
        two_writers(&w);
        let script = Script::new(&w.fakes, vec![(300, key(b"nn"))]);
        crate::agents::core_fixture::online(&w);
        crate::agents::core_fixture::online(&w);
        let (ended, _, _) = watch_with(&w, Box::new(script));
        assert_eq!(ended, "pane-closed");
        let targets: Vec<String> = w
            .fakes
            .mux_calls()
            .iter()
            .filter(|c| c[1] == "if-shell")
            .map(|c| c[4].clone())
            .collect();
        assert_eq!(targets, ["%9", "%11"]);
    }

    #[test]
    fn next_after_enter_in_one_read_steps_on_from_the_agent_entered() {
        // `j`, Enter, `n` in one read: Enter switches to the first agent, then n to the one after it.
        let w = click_world(&["live", "live", "live", "gone"], board_answer());
        two_writers(&w);
        let script = Script::new(&w.fakes, vec![(300, key(b"j\rn"))]);
        crate::agents::core_fixture::online(&w);
        crate::agents::core_fixture::online(&w);
        let (ended, _, _) = watch_with(&w, Box::new(script));
        assert_eq!(ended, "pane-closed");
        let targets: Vec<String> = w
            .fakes
            .mux_calls()
            .iter()
            .filter(|c| c[1] == "if-shell")
            .map(|c| c[4].clone())
            .collect();
        assert_eq!(targets, ["%9", "%11"]);
    }

    #[test]
    fn click_at_another_width_does_nothing_and_redraws() {
        // At 9 columns the head of `writer-two`'s line reflows to ` ◉ writer`, a whole name line of another agent.
        let split: Vec<String> = [
            " mux-agen",
            "ts  3 run",
            " \u{25c9} writer",
            "-two   wa",
            "iting   3m",
        ]
        .iter()
        .map(|r| r.to_string())
        .collect();
        for width in [30, 55, 9] {
            let w = click_world(&["live", "live", "live", "gone"], answer(width, &split));
            two_writers(&w);
            let script = Script::new(&w.fakes, vec![(300, press(3))]);
            let (ended, frames, times) = watch_with(&w, Box::new(script));
            assert_eq!((ended, frames.len()), ("pane-closed", 3), "{width}");
            assert_eq!(diffs(&times), [300, 2000], "{width}");
            assert_eq!((click_reads(&w), selects(&w)), (1, 0), "{width}");
            assert_eq!(frames[0], frames[1], "{width}");
        }
    }

    #[test]
    fn click_elsewhere_does_nothing() {
        // The header, the doing line and an ended row: each read, none focuses, each redraws at once.
        let w = click_world(
            &["live", "live", "live", "live", "live", "gone"],
            board_answer(),
        );
        let steps = vec![(300, press(1)), (300, press(4)), (300, press(7))];
        let (ended, frames, times) = watch_with(&w, Box::new(Script::new(&w.fakes, steps)));
        assert_eq!((ended, frames.len()), ("pane-closed", 5));
        assert_eq!(diffs(&times), [300, 300, 300, 2000]);
        assert_eq!((click_reads(&w), selects(&w)), (3, 0));
        // A release, the wheel and button 2: no read, no redraw.
        let w = click_world(&["live", "live", "gone"], board_answer());
        let steps = vec![
            (300, key(b"\x1b[<0;3;3m")),
            (300, key(b"\x1b[<64;3;3M")),
            (300, key(b"\x1b[<2;3;3M")),
        ];
        let (ended, frames, times) = watch_with(&w, Box::new(Script::new(&w.fakes, steps)));
        assert_eq!((ended, frames.len()), ("pane-closed", 2));
        assert_eq!(diffs(&times), [2000]);
        assert_eq!((click_reads(&w), selects(&w)), (0, 0));
    }

    #[test]
    fn press_near_or_across_a_write_is_dropped() {
        // Read 20 ms after its frame: dropped, but it still redraws at once.
        let w = click_world(&["live", "live", "live", "gone"], board_answer());
        let script = Script::new(&w.fakes, vec![(20, press(3))]);
        let (_, _, times) = watch_with(&w, Box::new(script));
        assert_eq!(diffs(&times), [20, 2000]);
        assert_eq!((click_reads(&w), selects(&w)), (0, 0));
        // Its first byte read before the second write, its last after: dropped.
        let w = click_world(&["live", "live", "live", "live", "gone"], board_answer());
        let steps = vec![(1990, key(b"\x1b[<0;3;")), (110, key(b"3M"))];
        let (_, _, times) = watch_with(&w, Box::new(Script::new(&w.fakes, steps)));
        assert_eq!(diffs(&times), [2000, 100, 2000]);
        assert_eq!((click_reads(&w), selects(&w)), (0, 0));
        // 30 ms after the second write: dropped.
        let w = click_world(&["live", "live", "live", "live", "gone"], board_answer());
        let script = Script::new(&w.fakes, vec![(2030, press(3))]);
        let (_, _, times) = watch_with(&w, Box::new(script));
        assert_eq!(diffs(&times), [2000, 30, 2000]);
        assert_eq!((click_reads(&w), selects(&w)), (0, 0));
        // 100 ms after it: acts.
        let w = click_world(&["live", "live", "live", "live", "gone"], board_answer());
        let script = Script::new(&w.fakes, vec![(2100, press(3))]);
        crate::agents::core_fixture::online(&w);
        let (_, _, times) = watch_with(&w, Box::new(script));
        assert_eq!(diffs(&times), [2000, 100, 2000]);
        assert_eq!((click_reads(&w), selects(&w)), (1, 1));
    }

    #[test]
    fn one_read_handles_every_event_in_order() {
        let w = click_world(&["live", "live", "live", "gone"], board_answer());
        let script = Script::new(&w.fakes, vec![(300, key(b"j\r"))]);
        crate::agents::core_fixture::online(&w);
        let (ended, frames, times) = watch_with(&w, Box::new(script));
        assert_eq!(ended, "pane-closed");
        // j selected worker, then Enter focused it: the next frame is unmarked.
        assert_eq!(frames, [board_frame(), board_frame(), board_frame()]);
        assert_eq!(diffs(&times), [300, 2000]);
        assert_eq!((click_reads(&w), selects(&w)), (0, 1));
        // The focus came before the next frame's probe.
        let calls = mux_text(&w);
        let select = calls.iter().position(|c| c.contains("if-shell")).unwrap();
        let probes_before = calls[..select]
            .iter()
            .filter(|c| c.ends_with(WATCH_PROBE))
            .count();
        assert_eq!(probes_before, 1);
    }

    #[test]
    fn stale_press_dismisses_the_note_and_redraws() {
        let w = click_world(&["live", "live", "live", "live", "gone"], board_answer());
        w.fakes.mux_first(READ_BACK, 0, "%9|0|1|1\n", "");
        let steps = vec![(300, press(3)), (20, press(3))];
        crate::agents::core_fixture::online(&w);
        let (ended, frames, times) = watch_with(&w, Box::new(Script::new(&w.fakes, steps)));
        assert_eq!(ended, "pane-closed");
        assert_eq!(
            frames,
            [board_frame(), failed_frame(), board_frame(), board_frame()]
        );
        assert_eq!(diffs(&times), [300, 20, 2000]);
        assert_eq!((click_reads(&w), selects(&w)), (1, 1));
    }

    #[test]
    fn ascii_turns_clicks_off() {
        let mut w = click_world(
            &["live", "live", "live", "live", "live", "gone"],
            board_answer(),
        );
        watch_env(&mut w, "MUX_AGENTS_ASCII", "1");
        let steps = vec![(300, press(3)), (300, key(b"j")), (300, key(b"\r"))];
        crate::agents::core_fixture::online(&w);
        let (ended, frames, times) = watch_with(&w, Box::new(Script::new(&w.fakes, steps)));
        assert_eq!((ended, frames.len()), ("pane-closed", 5));
        // The press redraws but reads nothing; j selects (ASCII marker) and Enter focuses.
        assert_eq!(diffs(&times), [300, 300, 300, 2000]);
        assert_eq!((click_reads(&w), selects(&w)), (0, 1));
        assert!(frames[2].contains(">* worker"), "{:?}", frames[2]);
        assert!(!frames[3].contains(">* worker"), "{:?}", frames[3]);
    }

    #[test]
    fn drawn_pane_moved_shows_note_never_selects() {
        let w = watch_world(&["live", "live", "live", "gone"]);
        w.fakes.mux_first(READ_BACK, 0, "%10|1|1|1\n", "");
        // The bar drew worker on %9; by the click it is on %10.
        let moved = Rc::new(Cell::new(false));
        let record = w.folder(ID).join("record.json");
        let flag = moved.clone();
        w.fakes.mux_script("capture-pane", move |_| {
            flag.set(true);
            let mut fields = crate::state::fs::read_json(&record).unwrap().unwrap();
            fields["pane"] = json!("%10");
            fields["startCommand"] = json!(crate::agents::fixture::launch_command("%10"));
            write(&record, fields);
            Ok(out(0, &board_answer(), ""))
        });
        let flag = moved.clone();
        w.fakes.mux_script("list-panes", move |argv| {
            let row = if flag.get() {
                "%10|@5|300|aaaaaaaaaaaa|worker"
            } else {
                "%9|@5|300|aaaaaaaaaaaa|worker"
            };
            Ok(out(
                0,
                &crate::agents::fixture::listed(argv, &[row], "\n"),
                "",
            ))
        });
        let script = Script::new(&w.fakes, vec![(300, press(3))]);
        crate::agents::core_fixture::online(&w);
        let (ended, frames, _) = watch_with(&w, Box::new(script));
        assert_eq!((ended, frames.len()), ("pane-closed", 3));
        assert!(moved.get());
        assert_eq!(click_reads(&w), 1);
        assert_eq!(selects(&w), 0, "{:?}", mux_text(&w));
        assert!(
            frames[1].contains("focus failed: worker"),
            "{:?}",
            frames[1]
        );
    }

    #[test]
    fn read_back_failure_shows_note() {
        let w = click_world(&["live", "live", "live", "gone"], board_answer());
        w.fakes.mux_first(READ_BACK, 1, "", "no server");
        let script = Script::new(&w.fakes, vec![(300, press(3))]);
        crate::agents::core_fixture::online(&w);
        let (ended, frames, times) = watch_with(&w, Box::new(script));
        assert_eq!(ended, "pane-closed");
        assert_eq!(frames, [board_frame(), failed_frame(), failed_frame()]);
        assert_eq!(diffs(&times), [300, 2000]);
        assert_eq!(selects(&w), 1);
    }

    #[test]
    fn keys_select_and_focus() {
        let w = click_world(&["live", "live", "live", "live", "gone"], board_answer());
        let steps = vec![(300, key(b"j")), (100, key(b"\r"))];
        crate::agents::core_fixture::online(&w);
        let (ended, frames, times) = watch_with(&w, Box::new(Script::new(&w.fakes, steps)));
        assert_eq!(ended, "pane-closed");
        assert_eq!(
            frames,
            [board_frame(), marked_frame(), board_frame(), board_frame()]
        );
        assert_eq!(diffs(&times), [300, 100, 2000]);
        assert_eq!((click_reads(&w), selects(&w)), (0, 1));
        let select = w
            .fakes
            .mux_calls()
            .into_iter()
            .find(|c| c[1] == "if-shell")
            .unwrap();
        assert_eq!(select[4], "%9");
        // A Down split across two reads.
        let w = click_world(&["live", "live", "live", "gone"], board_answer());
        let steps = vec![(300, key(b"\x1b[")), (100, key(b"B"))];
        let (_, frames, times) = watch_with(&w, Box::new(Script::new(&w.fakes, steps)));
        assert_eq!(frames, [board_frame(), marked_frame(), marked_frame()]);
        assert_eq!(diffs(&times), [400, 2000]);
    }

    // A `Script` that runs `then` as it hands back its last step.
    struct Before {
        script: Script,
        then: Option<Box<dyn FnOnce()>>,
    }

    impl Input for Before {
        fn wait(&mut self, clock: &dyn crate::ctx::Clock, timeout: Duration) -> Waited {
            let last = self.script.steps.len() == 1;
            let waited = self.script.wait(clock, timeout);
            if last
                && self.script.steps.is_empty()
                && let Some(then) = self.then.take()
            {
                then();
            }
            waited
        }
    }

    #[test]
    fn key_focus_of_a_moved_agent_shows_note_never_selects() {
        let w = watch_world(&["live", "live", "live", "live", "gone"]);
        // The bar drew worker on %9 when `j` selected it; by Enter it is on %10.
        let moved = Rc::new(Cell::new(false));
        let flag = moved.clone();
        w.fakes.mux_script("list-panes", move |argv| {
            let row = if flag.get() {
                "%10|@5|300|aaaaaaaaaaaa|worker"
            } else {
                "%9|@5|300|aaaaaaaaaaaa|worker"
            };
            Ok(out(
                0,
                &crate::agents::fixture::listed(argv, &[row], "\n"),
                "",
            ))
        });
        let record = w.folder(ID).join("record.json");
        let flag = moved.clone();
        let input = Before {
            script: Script::new(&w.fakes, vec![(300, key(b"j")), (100, key(b"\r"))]),
            then: Some(Box::new(move || {
                flag.set(true);
                let mut fields = crate::state::fs::read_json(&record).unwrap().unwrap();
                fields["pane"] = json!("%10");
                fields["startCommand"] = json!(crate::agents::fixture::launch_command("%10"));
                write(&record, fields);
            })),
        };
        crate::agents::core_fixture::online(&w);
        let (ended, frames, _) = watch_with(&w, Box::new(input));
        assert_eq!((ended, frames.len()), ("pane-closed", 4));
        assert!(moved.get());
        assert_eq!(selects(&w), 0, "{:?}", mux_text(&w));
        assert!(
            frames[2].contains("focus failed: worker"),
            "{:?}",
            frames[2]
        );
    }

    #[test]
    fn key_focus_unproved_shows_note_and_keeps_the_selection() {
        let w = click_world(&["live", "live", "live", "gone"], board_answer());
        w.fakes.mux_first(READ_BACK, 0, "%9|0|1|1\n", "");
        let steps = vec![(300, key(b"j")), (100, key(b"\r"))];
        crate::agents::core_fixture::online(&w);
        let (ended, frames, _) = watch_with(&w, Box::new(Script::new(&w.fakes, steps)));
        assert_eq!((ended, frames.len()), ("pane-closed", 3));
        assert_eq!(selects(&w), 1);
        // Not focused: the note shows, and worker stays selected.
        assert!(
            frames[2].contains("focus failed: worker"),
            "{:?}",
            frames[2]
        );
        assert!(frames[2].contains("\u{25b8}"), "{:?}", frames[2]);
    }

    #[test]
    fn note_lasts_to_second_tick_or_next_key() {
        let w = click_world(&["live", "live", "live", "live", "gone"], board_answer());
        w.fakes.mux_first(READ_BACK, 0, "%9|0|1|1\n", "");
        let script = Script::new(&w.fakes, vec![(300, press(3))]);
        crate::agents::core_fixture::online(&w);
        let (ended, frames, times) = watch_with(&w, Box::new(script));
        assert_eq!(ended, "pane-closed");
        assert_eq!(
            frames,
            [board_frame(), failed_frame(), failed_frame(), board_frame()]
        );
        assert_eq!(diffs(&times), [300, 2000, 2000]);
        // A release leaves it; j dismisses it.
        let w = click_world(&["live", "live", "live", "gone"], board_answer());
        w.fakes.mux_first(READ_BACK, 0, "%9|0|1|1\n", "");
        let steps = vec![
            (300, press(3)),
            (100, key(b"\x1b[<0;3;3m")),
            (100, key(b"j")),
        ];
        crate::agents::core_fixture::online(&w);
        let (_, frames, times) = watch_with(&w, Box::new(Script::new(&w.fakes, steps)));
        assert_eq!(frames, [board_frame(), failed_frame(), marked_frame()]);
        assert_eq!(diffs(&times), [300, 200]);
    }

    // ---- the right-edge lock (S1)

    #[derive(Debug, Clone)]
    struct Rect {
        id: &'static str,
        left: u32,
        top: u32,
        width: u32,
        height: u32,
    }

    fn r(id: &'static str, left: u32, top: u32, width: u32, height: u32) -> Rect {
        Rect {
            id,
            left,
            top,
            width,
            height,
        }
    }

    /// The bar's window as the multiplexer lays it out: its size, zoom, active pane and panes. move-pane leaves
    /// `moved` (none: the panes as they were) and makes `move_focus` active, or fails when `move_fail`.
    #[derive(Default)]
    struct Win {
        width: u32,
        height: u32,
        zoomed: bool,
        active: &'static str,
        panes: Vec<Rect>,
        moved: Option<Vec<Rect>>,
        move_focus: Option<&'static str>,
        move_fail: bool,
        // The two width options as the format answers them (unset: empty).
        min: &'static str,
        percent: &'static str,
    }

    type Shared = Rc<RefCell<Win>>;

    fn win(active: &'static str, panes: Vec<Rect>) -> Win {
        Win {
            width: 200,
            height: 50,
            active,
            panes,
            min: "40",
            percent: "20",
            ..Win::default()
        }
    }

    fn flag(on: bool) -> &'static str {
        if on { "1" } else { "0" }
    }

    /// What `list-panes -F <LAYOUT>` answers for `w`.
    fn layout_text(w: &Win) -> String {
        w.panes
            .iter()
            .map(|p| {
                format!(
                    "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}\n",
                    p.id,
                    flag(p.id == w.active),
                    flag(p.left + p.width == w.width),
                    flag(p.top == 0),
                    flag(p.top + p.height == w.height),
                    p.width,
                    w.width,
                    p.left,
                    p.top,
                    flag(w.zoomed),
                    if p.id == "%7" { w.min } else { "99" },
                    if p.id == "%7" { w.percent } else { "90" }
                )
            })
            .collect()
    }

    // A pane split right of the bar: %1 | %7 (the bar) | %4.
    fn split_right() -> Vec<Rect> {
        vec![
            r("%1", 0, 0, 100, 50),
            r("%7", 101, 0, 40, 50),
            r("%4", 142, 0, 58, 50),
        ]
    }

    // The same window with the bar back at the right edge.
    fn repaired() -> Vec<Rect> {
        vec![
            r("%1", 0, 0, 100, 50),
            r("%4", 101, 0, 58, 50),
            r("%7", 160, 0, 40, 50),
        ]
    }

    // The bar in place: %1 | %7.
    fn in_place() -> Vec<Rect> {
        vec![r("%1", 0, 0, 159, 50), r("%7", 160, 0, 40, 50)]
    }

    const MOVE: &str = "-N move-pane -d -f -h -s %7 -t %4";

    /// The watch is the recorded bar: its process 4242 is `<exe> watch` started at 2000, the record names %7, 4242
    /// and that start, and the proof read of %7 shows it alive running 4242.
    fn own_watch(w: &World) {
        let exe = w.ctx.exe.display().to_string();
        w.fakes.forget(4242);
        w.fakes.process(crate::proc::Row {
            pid: 4242,
            ppid: crate::agents::fixture::SHELL,
            start: Some(2000),
            path: Some(exe.clone()),
            argv: Some(vec![exe, "watch".into()]),
        });
        w.fakes.mux_rule(RECORD, 0, OWN, "");
        w.fakes.mux_rule(PROOF, 0, "%7|@5|0|4242\n", "");
    }

    /// watch_world with the bar's window answered from `win`: the layout read, move-pane, resize-pane (the pane at
    /// the right edge keeps its right edge), the active check and select-pane.
    fn keep_world(probes: &[&'static str], win: Win) -> (World, Shared) {
        let w = watch_world(probes);
        own_watch(&w);
        let shared = Rc::new(RefCell::new(win));
        let s = shared.clone();
        w.fakes.mux_script("#{pane_active}", move |argv| {
            let win = s.borrow();
            if argv[1] == "list-panes" {
                assert_eq!(argv[2..], ["-t", "%7", "-F", LAYOUT]);
                return Ok(out(0, &layout_text(&win), ""));
            }
            Ok(out(0, &format!("{}\n", flag(argv[4] == win.active)), ""))
        });
        let s = shared.clone();
        w.fakes.mux_script("move-pane", move |_| {
            let mut win = s.borrow_mut();
            if win.move_fail {
                return Ok(out(1, "", "can't move pane\n"));
            }
            if let Some(moved) = win.moved.take() {
                win.panes = moved;
            }
            if let Some(focus) = win.move_focus.take() {
                win.active = focus;
            }
            Ok(Output::default())
        });
        let s = shared.clone();
        w.fakes.mux_script("resize-pane", move |argv| {
            let mut win = s.borrow_mut();
            let width: u32 = argv[5].parse().unwrap();
            let total = win.width;
            for p in win.panes.iter_mut().filter(|p| p.id == argv[3]) {
                if p.left + p.width == total {
                    p.left = total - width;
                }
                p.width = width;
            }
            Ok(Output::default())
        });
        let s = shared.clone();
        w.fakes.mux_script("select-pane", move |argv| {
            let mut win = s.borrow_mut();
            let id = win.panes.iter().find(|p| p.id == argv[3]).unwrap().id;
            win.active = id;
            Ok(Output::default())
        });
        // The bar's size as the window has it now (after any move or resize).
        let s = shared.clone();
        w.fakes.mux_script(SIZE_FORMAT, move |argv| {
            let win = s.borrow();
            Ok(match win.panes.iter().find(|p| p.id == argv[4]) {
                Some(p) => out(0, &format!("{}|{}|{}\n", p.id, p.width, p.height), ""),
                None => out(1, "", "can't find pane\n"),
            })
        });
        (w, shared)
    }

    #[test]
    fn the_frame_is_laid_out_after_the_keeper_resized_the_bar() {
        // The probe reads 55 columns; the window is 136, so the keeper sets the bar to 40 before the frame is drawn.
        // The frame is the 40-column one, never a 54-column header in a 40-column pane.
        let wide = || {
            let mut win = win("%1", vec![r("%1", 0, 0, 80, 50), r("%7", 81, 0, 55, 50)]);
            win.width = 136;
            win
        };
        let (w, state) = keep_world(&["wide", "gone"], wide());
        let (ended, frames) = watch_run(&w);
        assert_eq!(ended, "pane-closed");
        assert_eq!(kept(&w), [RESIZE]);
        assert_eq!(state.borrow().panes[1].width, 40);
        assert_eq!(frames, [board_frame()]);
        // When the size cannot be read again, the width the keeper set is used.
        let (w, _) = keep_world(&["wide", "gone"], wide());
        w.fakes.mux_first(SIZE_FORMAT, 1, "", "no server");
        let (_, frames) = watch_run(&w);
        assert_eq!(frames, [board_frame()]);
    }

    // The lock's own calls: move-pane, resize-pane, the active check and select-pane.
    fn kept(w: &World) -> Vec<String> {
        w.fakes
            .mux_calls()
            .iter()
            .filter(|c| {
                matches!(c[1].as_str(), "move-pane" | "resize-pane" | "select-pane")
                    || c.last().is_some_and(|a| a == "#{pane_active}")
            })
            .map(|c| c.join(" "))
            .collect()
    }

    fn layout_reads(w: &World) -> usize {
        w.fakes
            .mux_calls()
            .iter()
            .filter(|c| c.last().is_some_and(|a| a == LAYOUT))
            .count()
    }

    fn moves(w: &World) -> usize {
        count(w, "move-pane")
    }

    fn paused_frame() -> String {
        noted_frame(&[PAUSED])
    }

    #[test]
    fn layout_read_is_strict() {
        let good = layout_text(&win("%1", in_place()));
        let layout = parse_layout(&good).unwrap();
        assert_eq!(
            (layout.width, layout.zoomed, layout.panes.len()),
            (200, false, 2)
        );
        assert_eq!(
            layout.panes[1],
            Pane {
                id: "%7".into(),
                active: false,
                at_right: true,
                at_top: true,
                at_bottom: true,
                width: 40,
                left: 160,
                top: 0,
                sizing: Sizing {
                    min: 40,
                    percent: 20
                },
            }
        );
        assert_eq!(
            (layout.panes[1].sizing.min, layout.panes[1].sizing.percent),
            (40, 20)
        );
        assert!(parse_layout(&good.replace('\n', "\r\n")).is_some());
        // The two option fields are read per pane and never fail the row: a bad one is its default alone.
        for (tail, want) in [
            ("|", (40, 20)),
            ("44|7", (44, 7)),
            ("40x10|7", (40, 7)),
            ("44|7xx", (44, 20)),
            ("1x2x3|x", (40, 20)),
        ] {
            let layout = parse_layout(&format!("%7|0|1|1|1|40|200|160|0|0|{tail}\n")).unwrap();
            let sizing = layout.panes[0].sizing;
            assert_eq!((sizing.min, sizing.percent), want, "{tail:?}");
        }
        for bad in [
            "",
            "\n",
            "%1|1|0|1|1|159|200|0|0|0",
            "%1|1|0|1|1|159|200|0|0|0|40",
            "%1|1|0|1|1|159|200|0|0|0|40|20|x",
            "%1|2|0|1|1|159|200|0|0|0|40|20",
            "%1|1|0|1|1|+159|200|0|0|0|40|20",
            "%1|1|0|1|1|159|200|0|-1|0|40|20",
            "%1|1|0|1|1|159||0|0|0|40|20",
            "1|1|0|1|1|159|200|0|0|0|40|20",
            "%1|1|0|1|1|159|200|0|0|0|40|20\n%7|0|1|1|1|40|201|160|0|0|40|20",
            "%1|1|0|1|1|159|200|0|0|0|40|20\n%7|0|1|1|1|40|200|160|0|1|40|20",
            "%1|1|0|1|1|159|200|0|0|0|40|20\nnot a row",
        ] {
            assert!(parse_layout(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn move_target_is_never_the_bar() {
        let target = |panes: Vec<Rect>| {
            let layout = parse_layout(&layout_text(&win("%1", panes))).unwrap();
            move_target(&layout.panes, "%7").map(str::to_string)
        };
        // A pane split right of the bar.
        assert_eq!(target(split_right()).as_deref(), Some("%4"));
        // The bar split -v: the other pane at the right edge, below or above it.
        let below = vec![
            r("%1", 0, 0, 159, 50),
            r("%7", 160, 0, 40, 25),
            r("%4", 160, 26, 40, 24),
        ];
        assert_eq!(target(below).as_deref(), Some("%4"));
        let above = vec![
            r("%1", 0, 0, 159, 50),
            r("%4", 160, 0, 40, 25),
            r("%7", 160, 26, 40, 24),
        ];
        assert_eq!(target(above).as_deref(), Some("%4"));
        // even-vertical: every pane is at the right edge; the topmost that is not the bar.
        let even = vec![
            r("%7", 0, 0, 200, 16),
            r("%1", 0, 17, 200, 16),
            r("%4", 0, 34, 200, 16),
        ];
        assert_eq!(target(even).as_deref(), Some("%1"));
        // The bar alone at the right edge and short: the pane reaching furthest right.
        let alone = vec![
            r("%1", 0, 0, 100, 50),
            r("%4", 101, 0, 50, 50),
            r("%7", 160, 0, 40, 25),
        ];
        assert_eq!(target(alone).as_deref(), Some("%4"));
        // The bar is the window's only pane: nothing to move it against.
        assert_eq!(target(vec![r("%7", 0, 0, 200, 50)]), None);
    }

    #[test]
    fn repair_moves_the_bar_back_and_keeps_focus() {
        // (the active pane before, the one move-pane leaves active): the bar or another pane.
        for (active, focus) in [
            ("%1", Some("%7")),
            ("%1", Some("%4")),
            ("%7", Some("%4")),
            ("%7", Some("%1")),
            ("%1", None),
            ("%7", None),
        ] {
            let mut broken = win(active, split_right());
            broken.moved = Some(repaired());
            broken.move_focus = focus;
            let (w, state) = keep_world(&["live", "live", "live", "gone"], broken);
            let (ended, frames) = watch_run(&w);
            assert_eq!((ended, frames.len()), ("pane-closed", 3));
            let mut want = vec![
                MOVE.to_string(),
                RESIZE.into(),
                format!("-N display-message -p -t {active} #{{pane_active}}"),
            ];
            if focus.is_some_and(|f| f != active) {
                want.push(format!("-N select-pane -t {active}"));
            }
            // One repair, checked by a read right after it; the next ticks find the bar in place and do nothing.
            assert_eq!(kept(&w), want, "{active} {focus:?}");
            assert_eq!(state.borrow().active, active, "{focus:?}");
            assert_eq!(layout_reads(&w), 4);
        }
    }

    #[test]
    fn width_follows_the_window_not_a_drag() {
        let (w, state) = keep_world(
            &["live", "live", "live", "live", "live", "gone"],
            win("%1", in_place()),
        );
        let s = state.clone();
        w.fakes.sleep_probe(move |n| {
            let mut win = s.borrow_mut();
            match n {
                // Dragged to 55: still at the right edge, full height.
                1 => win.panes = vec![r("%1", 0, 0, 144, 50), r("%7", 145, 0, 55, 50)],
                3 => {
                    win.width = 300;
                    win.panes = vec![r("%1", 0, 0, 244, 50), r("%7", 245, 0, 55, 50)];
                }
                _ => {}
            }
        });
        watch_run(&w);
        // The first tick sets the width for its window; the drag is kept until the window is 300, then 60.
        assert_eq!(kept(&w), [RESIZE, "-N resize-pane -t %7 -x 60"]);
        assert_eq!(state.borrow().panes[1].width, 60);
        assert_eq!(moves(&w), 0);
    }

    #[test]
    fn a_changed_option_sets_the_width_on_the_next_tick() {
        let (w, state) = keep_world(
            &["live", "live", "live", "live", "live", "gone"],
            win("%1", in_place()),
        );
        let s = state.clone();
        w.fakes.sleep_probe(move |n| {
            let mut win = s.borrow_mut();
            match n {
                // Dragged to 55, then the options change: the new width wins over the drag.
                1 => win.panes = vec![r("%1", 0, 0, 144, 50), r("%7", 145, 0, 55, 50)],
                2 => win.percent = "30",
                // Unset: 20% of 200 is 40, at least 40, so 40.
                3 => (win.min, win.percent) = ("", ""),
                // A value the multiplexer replaced `|` in falls back alone: min 40, percent 25 → 50.
                4 => (win.min, win.percent) = ("40x10", "25"),
                _ => {}
            }
        });
        watch_run(&w);
        assert_eq!(
            kept(&w),
            [
                RESIZE,
                "-N resize-pane -t %7 -x 60",
                "-N resize-pane -t %7 -x 40",
                "-N resize-pane -t %7 -x 50",
            ]
        );
        assert_eq!(state.borrow().panes[1].width, 50);
    }

    #[test]
    fn three_failures_pause_until_the_layout_changes_or_a_minute_passes() {
        let failing = || {
            let mut w = win("%1", split_right());
            w.move_fail = true;
            w
        };
        let six = ["live", "live", "live", "live", "live", "live", "gone"];
        // Three refused moves, then none for three more ticks; the bar says so from the third.
        let (w, _) = keep_world(&six, failing());
        let (_, frames) = watch_run(&w);
        assert_eq!(moves(&w), 3);
        assert_eq!((count(&w, "resize-pane"), count(&w, "select-pane")), (0, 0));
        assert_eq!(frames[..2], [board_frame(), board_frame()]);
        assert_eq!(frames[2..], vec![paused_frame(); 4]);
        // A changed layout re-arms it, and the repair goes through.
        let (w, state) = keep_world(&six, failing());
        let s = state.clone();
        w.fakes.sleep_probe(move |n| {
            if n == 4 {
                let mut win = s.borrow_mut();
                win.panes[0].width = 99;
                win.move_fail = false;
                win.moved = Some(repaired());
            }
        });
        let (_, frames) = watch_run(&w);
        assert_eq!(moves(&w), 4);
        assert_eq!(frames[2..4], [paused_frame(), paused_frame()]);
        assert_eq!(frames[4..], [board_frame(), board_frame()]);
        // 60 s on the clock after the pause re-arm it; 59 s do not.
        for (skip, moved, paused) in [(58, 5, 3), (57, 4, 4)] {
            let (w, _) = keep_world(&six[1..], failing());
            let fakes = w.fakes.clone();
            w.fakes.sleep_probe(move |n| {
                if n == 3 {
                    fakes.advance(Duration::from_secs(skip));
                }
            });
            let (_, frames) = watch_run(&w);
            assert_eq!(moves(&w), moved, "{skip}");
            // The skipped minute ages the agents too, so only each frame's last line is compared.
            let marked: Vec<bool> = frames.iter().map(|f| f.contains(PAUSED)).collect();
            let mut want = vec![false, false];
            want.extend((2..5).map(|i| i < paused));
            assert_eq!(marked, want, "{skip}");
        }
        // A move that answers 0 but leaves the bar out of place counts too, by the read right after it: no 4th.
        let (w, _) = keep_world(&six[1..], win("%1", split_right()));
        let (_, frames) = watch_run(&w);
        assert_eq!((moves(&w), layout_reads(&w)), (3, 8));
        assert_eq!(frames[..2], vec![board_frame(); 2]);
        assert_eq!(frames[2..], vec![paused_frame(); 3]);
        // A layout the user changes between two ticks is not a failure: each repair held when it was made.
        let (w, state) = keep_world(&six, win("%1", split_right()));
        state.borrow_mut().moved = Some(repaired());
        let s = state.clone();
        w.fakes.sleep_probe(move |_| {
            let mut win = s.borrow_mut();
            win.panes = split_right();
            win.moved = Some(repaired());
        });
        let (_, frames) = watch_run(&w);
        assert_eq!(moves(&w), 6);
        assert_eq!(frames, vec![board_frame(); 6]);
    }

    #[test]
    fn a_zoomed_window_is_left_alone() {
        let (w, state) = keep_world(
            &["live", "live", "live", "live", "live", "gone"],
            win("%1", in_place()),
        );
        let s = state.clone();
        w.fakes.sleep_probe(move |n| {
            let mut win = s.borrow_mut();
            match n {
                // %1 zoomed, then the window resized: the bar reads out of place and the width changed.
                1 => {
                    win.zoomed = true;
                    win.width = 300;
                    win.panes = vec![r("%1", 0, 0, 300, 50), r("%7", 160, 0, 40, 50)];
                }
                // Unzoomed: the width that changed meanwhile is applied then.
                3 => {
                    win.zoomed = false;
                    win.panes = vec![r("%1", 0, 0, 259, 50), r("%7", 260, 0, 40, 50)];
                }
                _ => {}
            }
        });
        watch_run(&w);
        assert_eq!(kept(&w), [RESIZE, "-N resize-pane -t %7 -x 60"]);
        assert_eq!(layout_reads(&w), 5);
    }

    #[test]
    fn the_bar_is_kept_by_its_own_pane_with_a_readable_layout() {
        // rmux 0.10.0's rotate-window leaves pane options where they were, so the bar can lose its tag: a probe
        // that names the bar's own pane is enough.
        let mut broken = win("%1", split_right());
        broken.moved = Some(repaired());
        let (w, _) = keep_world(&["untagged", "untagged", "gone"], broken);
        let (ended, frames) = watch_run(&w);
        assert_eq!((ended, frames), ("pane-closed", vec![board_frame(); 2]));
        assert_eq!(moves(&w), 1);
        // A probe that does not answer is no proof the pane is there: nothing is read.
        let (w, _) = keep_world(&["fail", "fail", "gone"], win("%1", split_right()));
        watch_run(&w);
        assert_eq!((layout_reads(&w), kept(&w).len()), (0, 0));
        // A layout that does not list the bar, a failed read, or one that does not parse: that tick does nothing.
        for (code, stdout) in [(0, "%1|1|1|1|1|200|200|0|0|0\n"), (1, ""), (0, "garbage\n")] {
            let w = watch_world(&["live", "live", "gone"]);
            own_watch(&w);
            w.fakes.mux_first("#{pane_at_right}", code, stdout, "");
            let (_, frames) = watch_run(&w);
            assert_eq!(frames, vec![board_frame(); 2], "{stdout:?}");
            assert_eq!((layout_reads(&w), kept(&w).len()), (2, 0), "{stdout:?}");
        }
    }

    // ---- sidebar

    /// A stateful session. `panes` maps pane -> `role|agent-id` (%1 the caller, %2 role `other`), plus `pid <pane>`,
    /// `dead <pane>`, `exe`, `next` (the next split's number) and the session options @mux-agents-sidebar and
    /// @mux-agents-sidebar-id under their own names. A pane's process is in the fake OS table: an open bar %7 runs
    /// `<exe> watch` as pid 107, start 1070; a split opens %7 (then %8, ...) the same way unless `child` says else.
    #[derive(Default)]
    struct Mux {
        extra: Vec<(&'static str, String)>,
        open: bool,
        // A bar opened by 0.2.1: labelled and running watch, never recorded.
        legacy: bool,
        disabled: bool,
        option_fail: Option<&'static str>,
        agent_bar: bool,
        refuse_size: bool,
        split_error: Option<&'static str>,
        // The split's process: `other` (/bin/sh), `args` (`<exe> watch x`) or `unreadable` (no row).
        child: Option<&'static str>,
        tag_fail: Option<&'static str>,
        record_fail: bool,
        kill_fail: Option<&'static str>,
        probe_fail: Option<(&'static str, usize)>,
        // One respawn of the bar's pane: `proof` right after its first proof read, `kill` just before the kill,
        // `pre-label` just before open's label, `label` right after it.
        race: Option<&'static str>,
    }

    type Panes = Rc<RefCell<Vec<(String, String)>>>;
    type Damage = fn(&Panes, &Fakes);

    fn failed(how: &str) -> Res<Output> {
        if how == "throw" {
            refuse(1, "endpoint", "the rmux endpoint did not answer")
        } else {
            Ok(out(1, "", "no server\n"))
        }
    }

    fn set(panes: &Panes, pane: &str, value: String) {
        let mut panes = panes.borrow_mut();
        match panes.iter_mut().find(|(p, _)| p == pane) {
            Some(entry) => entry.1 = value,
            None => panes.push((pane.into(), value)),
        }
    }

    fn get(panes: &Panes, pane: &str) -> Option<String> {
        panes
            .borrow()
            .iter()
            .find(|(p, _)| p == pane)
            .map(|(_, v)| v.clone())
    }

    fn unset(panes: &Panes, key: &str) {
        panes.borrow_mut().retain(|(k, _)| k != key);
    }

    fn roles(panes: &Panes) -> String {
        panes
            .borrow()
            .iter()
            .filter(|(p, _)| p.starts_with('%'))
            .map(|(p, v)| format!("{p}={v}"))
            .collect::<Vec<_>>()
            .join(",")
    }

    fn record(panes: &Panes) -> Option<String> {
        get(panes, RECORD)
    }

    /// `<exe> watch` as process `pid`, started at pid * 10.
    fn watch_row(pid: u32, exe: &str) -> crate::proc::Row {
        crate::proc::Row {
            pid,
            ppid: 900,
            start: Some(i64::from(pid) * 10),
            path: Some(exe.into()),
            argv: Some(vec![exe.into(), "watch".into()]),
        }
    }

    /// `respawn-pane -k -t <pane>`: its process is gone and a new one, pid + 1000, runs `<exe> watch` again.
    fn respawn(panes: &Panes, fakes: &Fakes, pane: &str) {
        let key = format!("pid {pane}");
        let old: u32 = get(panes, &key).unwrap().parse().unwrap();
        fakes.forget(old);
        fakes.process(watch_row(old + 1000, &get(panes, "exe").unwrap()));
        set(panes, &key, (old + 1000).to_string());
    }

    fn sidebar_world(tag: &str, mux: Mux) -> (World, Panes) {
        let w = World::human(tag);
        let exe = w.ctx.exe.display().to_string();
        let panes: Panes = Rc::new(RefCell::new(vec![
            ("%1".to_string(), "|".to_string()),
            ("%2".into(), "other|".into()),
            ("exe".into(), exe.clone()),
            ("next".into(), "7".into()),
        ]));
        if mux.agent_bar {
            set(&panes, "%3", "sidebar|cccccccccccc".into());
        }
        if mux.open || mux.legacy {
            set(&panes, "%7", "sidebar|".into());
            set(&panes, "pid %7", "107".into());
            set(&panes, "next", "8".into());
            w.fakes.process(watch_row(107, &exe));
            if mux.open {
                set(&panes, RECORD, "%7 107 1070".into());
            }
        }
        for (pane, value) in &mux.extra {
            set(&panes, pane, value.clone());
        }
        if mux.disabled {
            set(&panes, OPTION, "off".into());
        }
        // The labels, compared by the server: a value only equal to `sidebar` is the label. A read of one pane's
        // `#{@agent-role}` answers its role.
        let p = panes.clone();
        w.fakes.mux_script("#{@agent-role}", move |argv| {
            if argv[1] == "display-message" {
                let role = get(&p, &argv[4]).unwrap_or_default();
                let role = role.split_once('|').map_or(role.as_str(), |(r, _)| r);
                return Ok(out(0, &format!("{role}\n"), ""));
            }
            assert_eq!(argv[1..6], ["list-panes", "-s", "-t", "$3", "-F"]);
            let text: String = p
                .borrow()
                .iter()
                .filter(|(k, _)| k.starts_with('%'))
                .map(|(pane, v)| {
                    let role = v.split_once('|').map_or(v.as_str(), |(r, _)| r);
                    format!("{pane}|{}\n", flag(role == "sidebar"))
                })
                .collect();
            Ok(out(0, &text, ""))
        });
        let race = Rc::new(Cell::new(mux.race));
        let (p, fakes, r) = (panes.clone(), w.fakes.clone(), race.clone());
        let (left, how) = (
            Rc::new(Cell::new(mux.probe_fail.map_or(0, |(_, n)| n))),
            mux.probe_fail.map(|(how, _)| how),
        );
        w.fakes.mux_script(PROOF, move |argv| {
            if left.get() > 0 {
                left.set(left.get() - 1);
                return failed(how.unwrap());
            }
            let pane = argv[4].clone();
            let text = match (get(&p, &pane), get(&p, &format!("pid {pane}"))) {
                (Some(_), Some(pid)) => {
                    let dead = flag(get(&p, &format!("dead {pane}")).is_some());
                    format!("{pane}|@5|{dead}|{pid}\n")
                }
                _ => "|||\n".into(),
            };
            if r.get() == Some("proof") {
                r.set(None);
                respawn(&p, &fakes, &pane);
            }
            Ok(out(0, &text, ""))
        });
        let p = panes.clone();
        w.fakes.mux_script("#{pane_id}|#{pane_pid}", move |argv| {
            let pane = &argv[4];
            let text = match get(&p, &format!("pid {pane}")) {
                Some(pid) if get(&p, pane).is_some() => format!("{pane}|{pid}\n"),
                _ => "|\n".into(),
            };
            Ok(out(0, &text, ""))
        });
        // The conditional kill: only a live pane that still runs the pid bound in.
        let (p, fakes, r, kill_fail, tag_fail) = (
            panes.clone(),
            w.fakes.clone(),
            race.clone(),
            mux.kill_fail,
            mux.tag_fail,
        );
        w.fakes.mux_script("if-shell", move |argv| {
            let pane = argv[4].clone();
            let want = argv[5].split("#{pane_pid},").nth(1).unwrap();
            let want = want.trim_end_matches('}').to_string();
            if argv[6] == format!("set-option -p -t {pane} @agent-role sidebar") {
                if let Some(how) = tag_fail {
                    return failed(how);
                }
                if r.get() == Some("pre-label") {
                    r.set(None);
                    respawn(&p, &fakes, &pane);
                }
                let alive = get(&p, &format!("dead {pane}")).is_none();
                if alive && get(&p, &format!("pid {pane}")).as_deref() == Some(want.as_str()) {
                    set(&p, &pane, "sidebar|".into());
                }
                if r.get() == Some("label") {
                    r.set(None);
                    respawn(&p, &fakes, &pane);
                }
                return Ok(Output::default());
            }
            if let Some(width) = argv[6].strip_prefix(&format!("resize-pane -t {pane} -x ")) {
                let alive = get(&p, &format!("dead {pane}")).is_none();
                if alive && get(&p, &format!("pid {pane}")).as_deref() == Some(want.as_str()) {
                    set(&p, &format!("width {pane}"), width.to_string());
                }
                return Ok(Output::default());
            }
            assert_eq!(argv[6], format!("kill-pane -t {pane}"));
            if let Some(how) = kill_fail {
                return failed(how);
            }
            if r.get() == Some("kill") {
                r.set(None);
                respawn(&p, &fakes, &pane);
            }
            let alive = get(&p, &format!("dead {pane}")).is_none();
            if alive && get(&p, &format!("pid {pane}")).as_deref() == Some(want.as_str()) {
                unset(&p, &pane);
                unset(&p, &format!("pid {pane}"));
            }
            Ok(Output::default())
        });
        let (p, fakes) = (panes.clone(), w.fakes.clone());
        let (split_error, refuse_size, child) = (mux.split_error, mux.refuse_size, mux.child);
        w.fakes.mux_script("split-window", move |argv| {
            if let Some(stderr) = split_error {
                return Ok(out(1, "", &format!("{stderr}\n")));
            }
            if refuse_size && argv.iter().any(|a| a == "-l") {
                return Ok(out(
                    1,
                    "",
                    "invalid split size '40': invalid digit found in string\n",
                ));
            }
            let n: u32 = get(&p, "next").unwrap().parse().unwrap();
            set(&p, "next", (n + 1).to_string());
            let (pane, pid) = (format!("%{n}"), 100 + n);
            set(&p, &pane, "|".into());
            set(&p, &format!("pid {pane}"), pid.to_string());
            let exe = get(&p, "exe").unwrap();
            match child {
                Some("unreadable") => {}
                Some("other") => fakes.process(crate::proc::Row {
                    path: Some("/bin/sh".into()),
                    argv: Some(vec!["sh".into()]),
                    ..watch_row(pid, &exe)
                }),
                Some("args") => fakes.process(crate::proc::Row {
                    argv: Some(vec![exe.clone(), "watch".into(), "x".into()]),
                    ..watch_row(pid, &exe)
                }),
                _ => fakes.process(watch_row(pid, &exe)),
            }
            Ok(out(0, &format!("{pane}|@5|{pid}\n"), ""))
        });
        // A plain pane write (not inside a pid-bound if-shell) is a bug.
        w.fakes.mux_script("-N set-option -p", |argv| {
            panic!("an unguarded pane write: {argv:?}")
        });
        // Checked before the pane rules: read, set (`-t $3 <option> off`) and unset (`-u -t $3 <option>`).
        let (p, option_fail) = (panes.clone(), mux.option_fail);
        w.fakes.mux_script(OPTION, move |argv| {
            if let Some(how) = option_fail {
                return failed(how);
            }
            if argv[1] == "show-options" {
                let value = get(&p, OPTION).map_or(String::new(), |v| format!("{v}\n"));
                return Ok(out(0, &value, ""));
            }
            if argv[2] == "-u" {
                unset(&p, OPTION);
            } else {
                set(&p, OPTION, argv[5].clone());
            }
            Ok(Output::default())
        });
        // open's window probe, checked before the option (it names the width options): a window too narrow for
        // more than the minimum, with the options unset. A test answers otherwise with `mux_first`.
        w.fakes.mux_first(WINDOW_PROBE, 0, "200||\n", "");
        // The record, checked before the option (its name starts with the option's).
        let (p, record_fail) = (panes.clone(), mux.record_fail);
        w.fakes.mux_script(RECORD, move |argv| {
            if argv[1] == "show-options" {
                let value = get(&p, RECORD).map_or(String::new(), |v| format!("{v}\n"));
                return Ok(out(0, &value, ""));
            }
            if argv[2] == "-u" {
                unset(&p, RECORD);
                return Ok(Output::default());
            }
            if record_fail {
                return failed("exit");
            }
            set(&p, RECORD, argv[5].clone());
            Ok(Output::default())
        });
        (w, panes)
    }

    fn option(panes: &Panes) -> Option<String> {
        get(panes, OPTION)
    }

    // ensure as the caller, after its probe: `opened <pane>`, `existing <pane>`, `disabled` or the refusal.
    fn ensure_run(w: &World) -> String {
        let caller = w.caller();
        match ensure(&w.ctx, &caller) {
            Ok(Bar::Opened(pane)) => format!("opened {pane}"),
            Ok(Bar::Existing(pane)) => format!("existing {pane}"),
            Ok(Bar::Disabled) => "disabled".into(),
            Err(r) => format!("{} {} {}", r.code, r.check, r.message),
        }
    }

    fn run(w: &World, args: &[&str]) -> String {
        let mut full = vec!["sidebar"];
        full.extend(args);
        match sidebar(&w.ctx, &w.request(&full)) {
            Ok(reply) => format!("ok {}", Value::Object(reply)),
            Err(r) => format!("{} {} {}", r.code, r.check, r.message),
        }
    }

    // The agent snapshot's pane list, which proves the caller's pane is no agent's before the bar is touched (#48).
    fn snapshot_list(call: &[String]) -> bool {
        call[1] == "list-panes"
            && call
                .last()
                .is_some_and(|f| f.contains("#{pane_start_command}"))
    }

    // Calls on panes only: the session options' reads and writes are not counted.
    fn count(w: &World, verb: &str) -> usize {
        w.fakes
            .mux_calls()
            .iter()
            .filter(|c| {
                c[1] == verb && !snapshot_list(c) && !c.iter().any(|a| a == OPTION || a == RECORD)
            })
            .count()
    }

    // Kills tried: the conditional kill, or a plain kill-pane (never used).
    fn closes(w: &World) -> usize {
        let conditional = w
            .fakes
            .mux_calls()
            .iter()
            .filter(|c| c[1] == "if-shell" && c.last().is_some_and(|a| a.starts_with("kill-pane")))
            .count();
        count(w, "kill-pane") + conditional
    }

    fn proofs(w: &World) -> usize {
        w.fakes
            .mux_calls()
            .iter()
            .filter(|c| c.last().is_some_and(|a| a == PROOF))
            .count()
    }

    // Every call but the probes, as text.
    fn calls(w: &World) -> Vec<String> {
        w.fakes
            .mux_calls()
            .iter()
            .filter(|c| c[1] != "display-message" && !snapshot_list(c))
            .map(|c| c.join(" "))
            .collect()
    }

    fn env_text(w: &World) -> String {
        let env = crate::agents::spawn::pane_env(
            w.ctx.state_root.to_str().unwrap(),
            crate::state::root::HOST,
        );
        env.iter().map(|e| format!(" {e}")).collect()
    }

    fn split_line(w: &World, sized: bool) -> String {
        format!(
            "-N split-window -d -f -h -P -F #{{pane_id}}|#{{window_id}}|#{{pane_pid}} -t %1{}{} /bin/mux-agent watch",
            env_text(w),
            if sized { " -l 40" } else { "" }
        )
    }

    const RESIZE: &str = "-N resize-pane -t %7 -x 40";

    /// open's label of the new bar, as the multiplexer receives it: only while the pane runs `pid`.
    fn label_line(pane: &str, pid: u32) -> String {
        format!(
            "-N if-shell -F -t {pane} #{{&&:#{{==:#{{pane_dead}},0}},#{{==:#{{pane_pid}},{pid}}}}} set-option -p -t {pane} @agent-role sidebar"
        )
    }

    /// open's resize of the new bar, as the multiplexer receives it: only while the pane runs `pid`.
    fn resize_line(pane: &str, pid: u32, width: u32) -> String {
        format!(
            "-N if-shell -F -t {pane} #{{&&:#{{==:#{{pane_dead}},0}},#{{==:#{{pane_pid}},{pid}}}}} resize-pane -t {pane} -x {width}"
        )
    }

    // The calls of an open that finds no bar: record, labels, split, label, record, resize.
    fn opened(w: &World) -> Vec<String> {
        vec![
            READ_RECORD.into(),
            LABELS.into(),
            split_line(w, true),
            label_line("%7", 107),
            RECORD7.into(),
            resize_line("%7", 107, 40),
        ]
    }

    fn close_line(pane: &str, pid: u32) -> String {
        format!(
            "-N if-shell -F -t {pane} #{{&&:#{{==:#{{pane_dead}},0}},#{{==:#{{pane_pid}},{pid}}}}} kill-pane -t {pane}"
        )
    }

    fn refused(panes: &[&str]) -> String {
        let panes: Vec<String> = panes.iter().map(|p| p.to_string()).collect();
        format!("1 sidebar {}", unproven(&panes))
    }

    const GONE_AFTER: &str = "1 sidebar the new side-bar pane %7 no longer runs this plugin's watch; close it with \
                              kill-pane -t %7, or, if it is your own pane, clear its label with set-option -p -u -t %7 \
                              @agent-role";

    const NOT_CLOSED: &str =
        "1 sidebar the side bar %7 did not close: the pane no longer runs this session's side bar";

    #[test]
    fn bar_width_is_20_percent_of_the_window_at_least_40() {
        let widths: Vec<u32> = [0, 80, 200, 204, 205, 209, 210, 300, 1000, u32::MAX]
            .into_iter()
            .map(|window| bar_width(window, Sizing::default()))
            .collect();
        assert_eq!(widths, [40, 40, 40, 40, 41, 41, 42, 60, 200, 858_993_459]);
        // The options set both numbers; 0.2.4's 5%, at least 30, is 30 and 5.
        let narrow = Sizing::read("30", "5");
        let widths: Vec<u32> = [0, 200, 619, 620, 640, 1000]
            .into_iter()
            .map(|w| bar_width(w, narrow))
            .collect();
        assert_eq!(widths, [30, 30, 30, 31, 32, 50]);
        assert_eq!(bar_width(300, Sizing::read("", "100")), 300);
        assert_eq!(bar_width(10, Sizing::read("1", "1")), 1);
    }

    #[test]
    fn each_option_falls_back_to_its_default_alone() {
        let default = Sizing::default();
        assert_eq!((default.min, default.percent), (40, 20));
        for (min, percent, want) in [
            ("", "", (40, 20)),
            ("44", "7", (44, 7)),
            ("9999", "100", (9999, 100)),
            // Not a whole number in range: that field alone takes its default.
            ("0", "7", (40, 7)),
            ("44", "0", (44, 20)),
            ("44", "101", (44, 20)),
            ("10000", "7", (40, 7)),
            ("+44", "7", (40, 7)),
            ("44", " 7", (44, 20)),
            ("4.5", "7", (40, 7)),
            ("40x10", "7", (40, 7)),
            ("44", "7xx", (44, 20)),
            ("40x10", "7xx", (40, 20)),
            ("99999999999999999999", "7", (40, 7)),
        ] {
            let read = Sizing::read(min, percent);
            assert_eq!((read.min, read.percent), want, "{min:?} {percent:?}");
        }
    }

    #[test]
    fn the_options_are_read_with_the_window_width_and_cannot_break_its_fields() {
        // `|`, LF and CR in a value are replaced by the multiplexer, so the answer is always 3 fields.
        assert!(WINDOW_PROBE.starts_with("#{window_width}|"));
        assert_eq!(WINDOW_PROBE.matches('|').count(), 2 + 2);
        assert_eq!(LAYOUT.matches('|').count(), 11 + 2);
        for option in [MIN_OPTION, PERCENT_OPTION] {
            assert!(WINDOW_PROBE.contains(&format!("#{{s/[|\n\r]/x/:{option}}}")));
            assert!(LAYOUT.ends_with(&format!("#{{s/[|\n\r]/x/:{PERCENT_OPTION}}}")));
            assert!(LAYOUT.contains(&format!("|#{{s/[|\n\r]/x/:{option}}}")));
        }
        let read = |text: &str| {
            let (width, sizing) = window_answer(text);
            (width, sizing.min, sizing.percent)
        };
        assert_eq!(read("250||\n"), (Some(250), 40, 20));
        assert_eq!(read("250|44|7\n"), (Some(250), 44, 7));
        assert_eq!(read("250|40x10|7\r\n"), (Some(250), 40, 7));
        assert_eq!(read("250|44|7xx\n"), (Some(250), 44, 20));
        assert_eq!(read("250|1x2x3|7xx\n"), (Some(250), 40, 20));
        assert_eq!(read("wide|44|7\n"), (None, 44, 7));
        // Not 3 fields: nothing in it is trusted.
        for bad in [
            "",
            "250",
            "250\n",
            "250|44",
            "250|44|7|9",
            "250|44|7\nextra",
        ] {
            assert_eq!(read(bad), (None, 40, 20), "{bad:?}");
        }
    }

    #[test]
    fn open_sizes_the_bar_to_the_window_then_resizes_it() {
        // A 250-column window, options unset: 20% of 250, at least 40, is 50. Split -l 50 at the right edge, full
        // height, then resize-pane -x 50 (rmux ignores -l).
        let (w, _) = sidebar_world("", Mux::default());
        w.fakes.mux_first(WINDOW_PROBE, 0, "250||\n", "");
        assert!(run(&w, &[]).starts_with("ok "));
        let probe: Vec<String> = w
            .fakes
            .mux_calls()
            .iter()
            .filter(|c| c.last().is_some_and(|a| a == WINDOW_PROBE))
            .map(|c| c.join(" "))
            .collect();
        assert_eq!(
            probe,
            [format!("-N display-message -p -t %1 {WINDOW_PROBE}")]
        );
        let mut want = vec![ON.to_string()];
        want.extend(opened(&w));
        want[3] = split_line(&w, true).replace("-l 40", "-l 50");
        want[6] = resize_line("%7", 107, 50);
        assert_eq!(calls(&w), want);
        // Both options replaced (`|`, LF or CR in them): the width still counts, both options are their defaults.
        let (w, _) = sidebar_world("", Mux::default());
        w.fakes.mux_first(WINDOW_PROBE, 0, "1000|1x2|7xx\n", "");
        assert!(run(&w, &[]).starts_with("ok "));
        assert_eq!(calls(&w).last(), Some(&resize_line("%7", 107, 200)));
        // The options size it: 30% of 250, at least 30, is 75.
        let (w, _) = sidebar_world("", Mux::default());
        w.fakes.mux_first(WINDOW_PROBE, 0, "250|30|30\n", "");
        assert!(run(&w, &[]).starts_with("ok "));
        let mut want = vec![ON.to_string()];
        want.extend(opened(&w));
        want[3] = split_line(&w, true).replace("-l 40", "-l 75");
        want[6] = resize_line("%7", 107, 75);
        assert_eq!(calls(&w), want);
        // A minimum the multiplexer replaced `|` in is its default alone: 40% of 250.
        let (w, _) = sidebar_world("", Mux::default());
        w.fakes.mux_first(WINDOW_PROBE, 0, "250|40x10|40\n", "");
        assert!(run(&w, &[]).starts_with("ok "));
        assert_eq!(calls(&w).last(), Some(&resize_line("%7", 107, 100)));
        // An unreadable width (a failed probe, a non-number) opens the narrowest bar: the minimum, 40 or as set.
        for (answer, min) in [
            ((1, ""), 40),
            ((0, "wide||\n"), 40),
            ((0, ""), 40),
            ((0, "wide|44|\n"), 44),
        ] {
            let (w, _) = sidebar_world("", Mux::default());
            w.fakes.mux_first(WINDOW_PROBE, answer.0, answer.1, "");
            assert!(run(&w, &[]).starts_with("ok "), "{answer:?}");
            let resized: Vec<String> = calls(&w)
                .into_iter()
                .filter(|c| c.contains("resize-pane"))
                .collect();
            assert_eq!(resized, [resize_line("%7", 107, min)], "{answer:?}");
        }
        // A refused resize still opens the bar: the next watch tick sizes it.
        let (w, _) = sidebar_world("", Mux::default());
        w.fakes.mux_first("resize-pane", 1, "", "no such pane\n");
        assert_eq!(
            run(&w, &[]),
            r#"ok {"pane":"%7","window":"@5","opened":true}"#
        );
    }

    // T9 test 1: the record is written after the process check, before open returns.
    #[test]
    fn open_records_the_bar_once_its_process_is_this_watch() {
        let (w, panes) = sidebar_world("", Mux::default());
        assert_eq!(
            run(&w, &[]),
            r#"ok {"pane":"%7","window":"@5","opened":true}"#
        );
        let mut want = vec![ON.to_string()];
        want.extend(opened(&w));
        assert_eq!(calls(&w), want);
        assert_eq!(record(&panes).as_deref(), Some("%7 107 1070"));
        assert_eq!(option(&panes), None);
        assert_eq!(get(&panes, "width %7").as_deref(), Some("40"));
        let events = w.fakes.events();
        let read = events.iter().position(|e| e == "row 107").unwrap();
        let written = events
            .iter()
            .position(|e| e.contains("@mux-agents-sidebar-id %7"))
            .unwrap();
        assert!(read < written, "{events:?}");
        // A split whose process is not this binary `watch`: nothing killed, nothing labelled or recorded.
        for child in ["other", "args", "unreadable"] {
            let (w, panes) = sidebar_world(
                "",
                Mux {
                    child: Some(child),
                    ..Mux::default()
                },
            );
            assert_eq!(
                run(&w, &[]),
                "1 sidebar the new side-bar pane %7 did not start this plugin's watch; close it with kill-pane -t %7",
                "{child}"
            );
            assert_eq!((closes(&w), count(&w, "set-option")), (0, 0), "{child}");
            assert_eq!(roles(&panes), "%1=|,%2=other|,%7=|", "{child}");
            assert_eq!(record(&panes), None, "{child}");
            // Read again 50 ms apart while its exec may be pending.
            assert_eq!(w.fakes.slept(), vec![50; 19], "{child}");
        }
    }

    // T9 tests 1-3: a bar is proven only by a live pane, the recorded pid, that pid alive with its start and still
    // this binary `watch`; anything else is not killed, not reused and refused.
    #[test]
    fn a_bar_is_proven_only_by_its_live_recorded_watch() {
        let damage: [(&str, Damage); 10] = [
            ("dead pane", |p, _| set(p, "dead %7", "1".into())),
            ("respawned to the same watch", |p, f| respawn(p, f, "%7")),
            (
                "respawned to a command naming the old pid and watch",
                |p, f| {
                    respawn(p, f, "%7");
                    f.forget(1107);
                    f.process(crate::proc::Row {
                        path: Some("/bin/sh".into()),
                        argv: Some(vec!["sh".into(), "-c".into(), "107 watch".into()]),
                        ..watch_row(1107, "/bin/mux-agent")
                    });
                },
            ),
            ("process gone", |_, f| f.forget(107)),
            ("another start", |_, f| {
                f.forget(107);
                f.process(crate::proc::Row {
                    start: Some(1),
                    ..watch_row(107, "/bin/mux-agent")
                });
            }),
            ("exec'd another program", |_, f| {
                f.forget(107);
                f.process(crate::proc::Row {
                    path: Some("/bin/sh".into()),
                    argv: Some(vec!["sh".into()]),
                    ..watch_row(107, "/bin/mux-agent")
                });
            }),
            ("other arguments", |_, f| {
                f.forget(107);
                f.process(crate::proc::Row {
                    argv: Some(vec!["/bin/mux-agent".into(), "watch".into(), "x".into()]),
                    ..watch_row(107, "/bin/mux-agent")
                });
            }),
            ("record names another pid", |p, _| {
                set(p, RECORD, "%7 108 1070".into())
            }),
            ("record names another pane", |p, _| {
                set(p, RECORD, "%2 107 1070".into())
            }),
            ("record malformed", |p, _| set(p, RECORD, "%7 107".into())),
        ];
        for (name, damage) in damage {
            let (w, panes) = sidebar_world(
                "",
                Mux {
                    open: true,
                    ..Mux::default()
                },
            );
            damage(&panes, &w.fakes);
            assert_eq!(ensure_run(&w), refused(&["%7"]), "{name}");
            assert_eq!(run(&w, &["-Close"]), refused(&["%7"]), "{name}");
            assert_eq!(run(&w, &[]), refused(&["%7"]), "{name}");
            assert_eq!((closes(&w), count(&w, "split-window")), (0, 0), "{name}");
            assert!(roles(&panes).contains("%7=sidebar|"), "{name}");
        }
    }

    // T9 test 1b: -Close closes the proven bar and clears the record; sidebar then opens a new bar with its own.
    #[test]
    fn close_clears_the_record_and_the_next_bar_gets_its_own() {
        let (w, panes) = sidebar_world(
            "",
            Mux {
                open: true,
                ..Mux::default()
            },
        );
        assert_eq!(ensure_run(&w), "existing %7");
        assert_eq!(calls(&w), [READ_OPTION, READ_RECORD]);
        assert_eq!(
            run(&w, &[]),
            r#"ok {"pane":"%7","window":"@5","opened":false}"#
        );
        w.fakes.clear_log();
        assert_eq!(run(&w, &["-Close"]), r#"ok {"pane":"%7","closed":true}"#);
        assert_eq!(
            calls(&w),
            [
                OFF.to_string(),
                READ_RECORD.into(),
                close_line("%7", 107),
                CLEAR_RECORD.into()
            ]
        );
        assert_eq!(
            (option(&panes).as_deref(), record(&panes)),
            (Some("off"), None)
        );
        assert_eq!(roles(&panes), "%1=|,%2=other|");
        w.fakes.clear_log();
        assert_eq!(run(&w, &["-Close"]), r#"ok {"pane":null,"closed":false}"#);
        assert_eq!(calls(&w), [OFF, READ_RECORD, LABELS]);
        assert_eq!(
            run(&w, &[]),
            r#"ok {"pane":"%8","window":"@5","opened":true}"#
        );
        assert_eq!(
            (option(&panes), record(&panes).as_deref()),
            (None, Some("%8 108 1080"))
        );
        assert_eq!(ensure_run(&w), "existing %8");
    }

    // T9 test 5: a bar replaced between its proof and the kill survives, and -Close says so.
    #[test]
    fn a_bar_replaced_before_the_kill_is_never_killed() {
        let (w, panes) = sidebar_world(
            "",
            Mux {
                open: true,
                race: Some("kill"),
                ..Mux::default()
            },
        );
        assert_eq!(run(&w, &["-Close"]), NOT_CLOSED);
        assert_eq!(get(&panes, "pid %7").as_deref(), Some("1107"));
        assert!(roles(&panes).contains("%7=sidebar|"));
        assert_eq!(record(&panes).as_deref(), Some("%7 107 1070"));
        assert_eq!((count(&w, "kill-pane"), count(&w, "if-shell")), (0, 1));
        // A kill the multiplexer refused leaves the pane too.
        for how in ["exit", "throw"] {
            let (w, panes) = sidebar_world(
                "",
                Mux {
                    open: true,
                    kill_fail: Some(how),
                    ..Mux::default()
                },
            );
            assert_eq!(run(&w, &["-Close"]), NOT_CLOSED, "{how}");
            assert!(roles(&panes).contains("%7=sidebar|"), "{how}");
        }
    }

    // T9 test 6: a candidate replaced between its proof read and its return is read again, and is then absent.
    #[test]
    fn a_bar_replaced_after_its_proof_read_is_never_returned() {
        let (w, panes) = sidebar_world(
            "",
            Mux {
                open: true,
                race: Some("proof"),
                ..Mux::default()
            },
        );
        assert_eq!(ensure_run(&w), refused(&["%7"]));
        assert_eq!(proofs(&w), 2);
        assert_eq!((closes(&w), count(&w, "split-window")), (0, 0));
        assert_eq!(get(&panes, "pid %7").as_deref(), Some("1107"));
    }

    // T9-QA 1: open proves the bar last, after labelling, recording and resizing: a bar replaced after its label is
    // written is not opened; the record is cleared and nothing is killed.
    #[test]
    fn open_proves_the_bar_last() {
        let (w, panes) = sidebar_world(
            "",
            Mux {
                race: Some("label"),
                ..Mux::default()
            },
        );
        assert_eq!(run(&w, &[]), GONE_AFTER);
        assert_eq!(record(&panes), None);
        assert_eq!(closes(&w), 0);
        assert_eq!(get(&panes, "pid %7").as_deref(), Some("1107"));
        let all: Vec<String> = w.fakes.mux_calls().iter().map(|c| c.join(" ")).collect();
        let at = |line: &str| all.iter().position(|c| c == line).unwrap();
        let proof = format!("-N display-message -p -t %7 {PROOF}");
        let resize = resize_line("%7", 107, 40);
        assert!(
            at(RECORD7) < at(&resize) && at(&resize) < at(&proof) && at(&proof) < at(CLEAR_RECORD)
        );
        // The replacement keeps its width: the only resize is bound to the pid that was checked.
        assert_eq!(get(&panes, "width %7"), None);
        assert_eq!(count(&w, "resize-pane"), 0);
        // ensure as a spawn runs it opens nothing either: the pane carries the label but is not the bar.
        assert_eq!(ensure_run(&w), refused(&["%7"]));
        assert_eq!(count(&w, "split-window"), 1);
    }

    // T9-QA round 3: the label is bound to the checked pid too. A pane replaced just before it gets no label; open
    // refuses (the guarded cleanup spares the replacement) and nothing is recorded.
    #[test]
    fn a_bar_replaced_before_its_label_is_never_labelled() {
        let (w, panes) = sidebar_world(
            "",
            Mux {
                race: Some("pre-label"),
                ..Mux::default()
            },
        );
        assert_eq!(
            run(&w, &[]),
            "1 sidebar the side-bar pane %7 could not be tagged or closed; close it with kill-pane -t %7"
        );
        assert_eq!(get(&panes, "pid %7").as_deref(), Some("1107"));
        assert_eq!(roles(&panes), "%1=|,%2=other|,%7=|");
        assert_eq!(record(&panes), None);
        let all: Vec<String> = calls(&w);
        assert!(all.contains(&label_line("%7", 107)), "{all:?}");
        assert!(!all.iter().any(|c| c.contains("resize-pane")), "{all:?}");
        // Unlabelled, it blocks nothing: the next open splits a new bar.
        assert_eq!(
            run(&w, &[]),
            r#"ok {"pane":"%8","window":"@5","opened":true}"#
        );
    }

    // T9 test 7: a bar that cannot be labelled or recorded is closed only while it is still the watch it split.
    #[test]
    fn a_failed_label_or_record_closes_only_the_watch_it_split() {
        for how in ["exit", "throw"] {
            let (w, panes) = sidebar_world(
                "",
                Mux {
                    tag_fail: Some(how),
                    ..Mux::default()
                },
            );
            assert_eq!(
                run(&w, &[]),
                "1 rmux the side-bar pane %7 could not be tagged; it was closed",
                "{how}"
            );
            let closed: Vec<String> = calls(&w)
                .into_iter()
                .filter(|c| c.contains("kill-pane"))
                .collect();
            assert_eq!(closed, [close_line("%7", 107)], "{how}");
            assert_eq!(
                (
                    count(&w, "kill-pane"),
                    calls(&w)
                        .iter()
                        .filter(|c| c.contains("resize-pane"))
                        .count()
                ),
                (0, 0),
                "{how}"
            );
            assert_eq!(
                (roles(&panes), record(&panes)),
                ("%1=|,%2=other|".to_string(), None),
                "{how}"
            );
        }
        let (w, panes) = sidebar_world(
            "",
            Mux {
                record_fail: true,
                ..Mux::default()
            },
        );
        assert_eq!(
            run(&w, &[]),
            "1 rmux the side-bar pane %7 could not be recorded; it was closed"
        );
        assert_eq!(closes(&w), 1);
        assert_eq!(
            (roles(&panes), record(&panes)),
            ("%1=|,%2=other|".to_string(), None)
        );
        // Replaced before the cleanup: it stays, and the refusal says so.
        let (w, panes) = sidebar_world(
            "",
            Mux {
                tag_fail: Some("exit"),
                race: Some("kill"),
                ..Mux::default()
            },
        );
        assert_eq!(
            run(&w, &[]),
            "1 sidebar the side-bar pane %7 could not be tagged or closed; close it with kill-pane -t %7"
        );
        assert_eq!(closes(&w), 1);
        assert_eq!(roles(&panes), "%1=|,%2=other|,%7=|");
        for (tag, kill) in [("exit", "exit"), ("throw", "throw"), ("exit", "throw")] {
            let (w, _) = sidebar_world(
                "",
                Mux {
                    tag_fail: Some(tag),
                    kill_fail: Some(kill),
                    ..Mux::default()
                },
            );
            assert_eq!(
                run(&w, &[]),
                "1 sidebar the side-bar pane %7 could not be tagged or closed; close it with kill-pane -t %7",
                "{tag} {kill}"
            );
        }
    }

    // T9 test 8: an unproven label anywhere in the session (a 0.2.1 bar, a label rmux moved, another window, an
    // agent pane) refuses ensure, sidebar and -Close; no second bar, no kill.
    #[test]
    fn an_unproven_label_anywhere_refuses_and_nothing_is_closed() {
        let wording = "1 sidebar pane %7 carries the side-bar label but no side bar of this session can be proven \
                       there: if it is an old side bar (opened by 0.2.1, or restarted), close it with kill-pane -t \
                       %7; if it is your own pane (a label rmux moved there), clear the label with set-option -p -u \
                       -t %7 @agent-role";
        let (w, panes) = sidebar_world(
            "",
            Mux {
                legacy: true,
                ..Mux::default()
            },
        );
        assert_eq!(ensure_run(&w), wording);
        assert_eq!(run(&w, &["-Close"]), wording);
        assert_eq!(run(&w, &[]), wording);
        assert_eq!((closes(&w), count(&w, "split-window")), (0, 0));
        assert_eq!(roles(&panes), "%1=|,%2=other|,%7=sidebar|");
        // Every labelled pane of the session is named, read with one session-wide list.
        let (w, _) = sidebar_world(
            "",
            Mux {
                agent_bar: true,
                extra: vec![("%9", "sidebar|".into())],
                ..Mux::default()
            },
        );
        assert_eq!(ensure_run(&w), refused(&["%3", "%9"]));
        assert!(
            refused(&["%3", "%9"]).starts_with("1 sidebar panes %3, %9 carry the side-bar label")
        );
        assert!(refused(&["%3", "%9"]).contains("kill-pane -t <pane>"));
        assert!(w.fakes.mux_calls().iter().any(|c| c.join(" ") == LABELS));
        // Values that only look like the label in a list are not it: nothing to refuse, nothing to close.
        for (name, value) in [
            ("role newline", "sidebar\nmore|cccccccccccc"),
            ("role escape", "side\u{1b}bar|"),
            ("forged line", "other|x\n%2|@5|sidebar|"),
        ] {
            let (w, _) = sidebar_world(
                "",
                Mux {
                    extra: vec![("%3", value.into())],
                    ..Mux::default()
                },
            );
            assert_eq!(
                run(&w, &["-Close"]),
                r#"ok {"pane":null,"closed":false}"#,
                "{name}"
            );
            assert_eq!(closes(&w), 0, "{name}");
        }
    }

    #[test]
    fn an_unreadable_proof_is_no_proof() {
        for how in ["exit", "throw"] {
            let (w, panes) = sidebar_world(
                "",
                Mux {
                    open: true,
                    probe_fail: Some((how, 4)),
                    ..Mux::default()
                },
            );
            assert_eq!(run(&w, &[]), refused(&["%7"]), "{how}");
            assert_eq!(run(&w, &["-Close"]), refused(&["%7"]), "{how}");
            assert_eq!(
                run(&w, &[]),
                r#"ok {"pane":"%7","window":"@5","opened":false}"#,
                "{how}"
            );
            assert_eq!(
                (
                    count(&w, "split-window"),
                    count(&w, "set-option"),
                    closes(&w)
                ),
                (0, 0, 0),
                "{how}"
            );
            assert_eq!(roles(&panes), "%1=|,%2=other|,%7=sidebar|");
        }
        // An unreadable record may name the bar: refused before anything is listed.
        let (w, _) = sidebar_world(
            "",
            Mux {
                open: true,
                ..Mux::default()
            },
        );
        w.fakes
            .mux_first(READ_RECORD.trim_start_matches("-N "), 1, "", "no server");
        assert_eq!(run(&w, &[]), "1 rmux tmux call failed");
        assert_eq!(count(&w, "list-panes"), 0);
    }

    #[test]
    fn a_split_that_names_no_pane_names_the_mux_in_use() {
        let (w, _) = sidebar_world("", Mux::default());
        w.fakes.mux_first("split-window", 0, "garbage\n", "");
        assert_eq!(run(&w, &[]), "1 rmux tmux did not name the side-bar pane");
    }

    #[test]
    fn sidebar_discover_split_tag_under_one_lock() {
        let (w, _) = sidebar_world("", Mux::default());
        let fakes = w.fakes.clone();
        w.fakes.lock_probe(move |_| {
            fakes
                .mux_calls()
                .iter()
                .map(|c| c[1].clone())
                .collect::<Vec<_>>()
                .join(",")
        });
        assert!(run(&w, &[]).starts_with("ok "));
        let key = w.caller().server_key;
        assert_eq!(
            w.fakes.locks(),
            [
                format!("+sidebar|{key}|$3 display-message,list-panes"),
                format!(
                    "-sidebar|{key}|$3 display-message,list-panes,set-option,show-options,list-panes,display-message,\
                     split-window,if-shell,display-message,set-option,if-shell,display-message"
                ),
            ]
        );
    }

    #[test]
    fn sidebar_tagged_refuses_before_any_mux_call() {
        for tag in ["aaaaaaaaaaaa", "x"] {
            let (w, panes) = sidebar_world(
                tag,
                Mux {
                    open: true,
                    ..Mux::default()
                },
            );
            assert_eq!(
                run(&w, &[]),
                "2 tagged an agent pane cannot open the side bar",
                "{tag}"
            );
            assert_eq!(
                w.fakes
                    .mux_calls()
                    .iter()
                    .map(|c| c[1].as_str())
                    .collect::<Vec<_>>(),
                ["display-message"]
            );
            assert!(w.fakes.locks().is_empty());
            assert_eq!(roles(&panes), "%1=|,%2=other|,%7=sidebar|");
        }
    }

    #[test]
    fn sidebar_close_tagged_refuses() {
        let (w, panes) = sidebar_world(
            "aaaaaaaaaaaa",
            Mux {
                open: true,
                ..Mux::default()
            },
        );
        assert_eq!(
            run(&w, &["-Close"]),
            "2 tagged an agent pane cannot open the side bar"
        );
        assert_eq!(w.fakes.mux_calls().len(), 1);
        assert!(w.fakes.locks().is_empty());
        assert_eq!(roles(&panes), "%1=|,%2=other|,%7=sidebar|");
    }

    #[test]
    fn split_retry_only_on_size_refusal() {
        for stderr in [
            "invalid split size '40': invalid digit found in string\r\n",
            "size invalid\n",
            "size too small\n",
            "command split-window: unknown flag -l\n",
        ] {
            assert!(size_refusal(&out(1, "", stderr)), "{stderr:?}");
        }
        for (code, stderr) in [
            (1, "command split-window: unknown flag -Q\n"),
            (1, "permission denied\n"),
            (1, "no space for new pane\n"),
            (1, "can't find window: x\n"),
            (0, "size invalid\n"),
            (1, ""),
            (1, "error: size invalid\n"),
        ] {
            assert!(!size_refusal(&out(code, "", stderr)), "{stderr:?}");
        }
        // A size refusal retries without -l, then labels and records.
        let (w, panes) = sidebar_world(
            "",
            Mux {
                refuse_size: true,
                ..Mux::default()
            },
        );
        assert_eq!(
            run(&w, &[]),
            r#"ok {"pane":"%7","window":"@5","opened":true}"#
        );
        let splits: Vec<String> = calls(&w)
            .into_iter()
            .filter(|c| c.contains("split-window"))
            .collect();
        assert_eq!(splits, [split_line(&w, true), split_line(&w, false)]);
        assert_eq!(roles(&panes), "%1=|,%2=other|,%7=sidebar|");
        // Without -l the multiplexer picks the width, so the resize sizes it.
        assert_eq!(calls(&w).last(), Some(&resize_line("%7", 107, 40)));
        // Any other failure refuses after exactly one split; a refused retry after exactly two.
        for stderr in [
            "permission denied",
            "can't find window: x",
            "command split-window: unknown flag -Q",
        ] {
            let (w, _) = sidebar_world(
                "",
                Mux {
                    split_error: Some(stderr),
                    ..Mux::default()
                },
            );
            assert_eq!(run(&w, &[]), "1 rmux tmux call failed", "{stderr}");
            assert_eq!((count(&w, "split-window"), count(&w, "set-option")), (1, 0));
        }
        let (w, _) = sidebar_world(
            "",
            Mux {
                split_error: Some("invalid split size '40': invalid digit found in string"),
                ..Mux::default()
            },
        );
        assert_eq!(run(&w, &[]), "1 rmux tmux call failed");
        assert_eq!((count(&w, "split-window"), count(&w, "set-option")), (2, 0));
        // The message names the multiplexer that ran the split; the check stays `rmux`.
        let (w, _) = sidebar_world(
            "",
            Mux {
                split_error: Some("no space for new pane"),
                ..Mux::default()
            },
        );
        w.fakes.mux_kind(crate::mux::Kind::Rmux);
        assert_eq!(run(&w, &[]), "1 rmux rmux call failed");
    }

    #[test]
    fn cmd_is_exe_watch() {
        let (w, panes) = sidebar_world("", Mux::default());
        assert_eq!(
            run(&w, &[]),
            r#"ok {"pane":"%7","window":"@5","opened":true}"#
        );
        let mut want = vec![ON.to_string()];
        want.extend(opened(&w));
        assert_eq!(calls(&w), want);
        assert_eq!(roles(&panes), "%1=|,%2=other|,%7=sidebar|");
        // The pane command is two arguments, the exe (spaces and all) then `watch`, never one joined string.
        let (mut w, panes) = sidebar_world("", Mux::default());
        w.ctx.exe = std::path::PathBuf::from("/opt/mux agents/bin/mux-agent");
        set(&panes, "exe", "/opt/mux agents/bin/mux-agent".into());
        assert!(run(&w, &[]).starts_with("ok "));
        let split: Vec<Vec<String>> = w
            .fakes
            .mux_calls()
            .into_iter()
            .filter(|c| c[1] == "split-window")
            .collect();
        let mut expected: Vec<String> = [
            "-N",
            "split-window",
            "-d",
            "-f",
            "-h",
            "-P",
            "-F",
            "#{pane_id}|#{window_id}|#{pane_pid}",
            "-t",
            "%1",
        ]
        .map(String::from)
        .to_vec();
        expected.extend(crate::agents::spawn::pane_env(
            w.ctx.state_root.to_str().unwrap(),
            crate::state::root::HOST,
        ));
        expected.extend(["-l", "40", "/opt/mux agents/bin/mux-agent", "watch"].map(String::from));
        assert_eq!(split, [expected]);
    }

    // QA on S1: only the watch the record names, by its own pane and its own process, keeps the layout; a watch run
    // by hand, with or without a bar, only draws.
    #[test]
    fn only_the_recorded_watch_keeps_the_layout() {
        // (record, the proof read of the watch's own pane): only its own pane, its own pid in that pane, alive, and
        // its own start keep the layout.
        for (record, proof) in [
            ("", "%7|@5|0|4242\n"),
            ("%8 4242 2000\n", "%7|@5|0|4242\n"),
            ("%7 4243 2000\n", "%7|@5|0|4242\n"),
            ("%7 4242\n", "%7|@5|0|4242\n"),
            // A stale start: the record names another process that once had this pid.
            ("%7 4242 1\n", "%7|@5|0|4242\n"),
            // The pane runs another process (a shell, with this watch run by hand in it), or is dead, or unread.
            ("%7 4242 2000\n", "%7|@5|0|170\n"),
            ("%7 4242 2000\n", "%7|@5|1|4242\n"),
            ("%7 4242 2000\n", ""),
        ] {
            let (w, _) = keep_world(&["live", "live", "live", "gone"], win("%1", split_right()));
            w.fakes.mux_first(RECORD, 0, record, "");
            w.fakes.mux_first(PROOF, 0, proof, "");
            let record = format!("{record:?} {proof:?}");
            let (ended, frames) = watch_run(&w);
            assert_eq!(
                (ended, frames),
                ("pane-closed", vec![board_frame(); 3]),
                "{record:?}"
            );
            assert_eq!((layout_reads(&w), kept(&w).len()), (0, 0), "{record:?}");
        }
    }

    // ---- ensure

    #[test]
    fn ensure_opens_one_bar_then_finds_it() {
        let (w, panes) = sidebar_world("", Mux::default());
        assert_eq!(ensure_run(&w), "opened %7");
        let mut want = vec![READ_OPTION.to_string()];
        want.extend(opened(&w));
        assert_eq!(calls(&w), want);
        w.fakes.clear_log();
        assert_eq!(ensure_run(&w), "existing %7");
        assert_eq!(calls(&w), [READ_OPTION, READ_RECORD]);
        assert_eq!(roles(&panes), "%1=|,%2=other|,%7=sidebar|");
        // ensure only reads the option.
        assert_eq!(option(&panes), None);
    }

    #[test]
    fn ensure_reads_option_under_sidebar_lock() {
        for disabled in [false, true] {
            let (w, _) = sidebar_world(
                "",
                Mux {
                    disabled,
                    ..Mux::default()
                },
            );
            let caller = w.caller();
            w.fakes.clear_log();
            let fakes = w.fakes.clone();
            w.fakes.lock_probe(move |_| {
                fakes
                    .mux_calls()
                    .iter()
                    .map(|c| c[1].clone())
                    .collect::<Vec<_>>()
                    .join(",")
            });
            assert!(ensure(&w.ctx, &caller).is_ok());
            let key = caller.server_key;
            let inside = if disabled {
                "show-options"
            } else {
                "show-options,show-options,list-panes,display-message,split-window,if-shell,display-message,set-option,if-shell,\
                 display-message"
            };
            assert_eq!(
                w.fakes.locks(),
                [
                    format!("+sidebar|{key}|$3"),
                    format!("-sidebar|{key}|$3 {inside}")
                ],
                "{disabled}"
            );
        }
    }

    #[test]
    fn ensure_disabled_opens_nothing() {
        for open in [false, true] {
            let (w, panes) = sidebar_world(
                "",
                Mux {
                    open,
                    disabled: true,
                    ..Mux::default()
                },
            );
            let before = roles(&panes);
            assert_eq!(ensure_run(&w), "disabled", "{open}");
            assert_eq!(calls(&w), [READ_OPTION], "{open}");
            assert_eq!(roles(&panes), before, "{open}");
        }
    }

    #[test]
    fn ensure_failure_refuses_and_opens_no_second_bar() {
        // A bar that cannot be proven is not absent: its label refuses a second one.
        for how in ["exit", "throw"] {
            let (w, panes) = sidebar_world(
                "",
                Mux {
                    open: true,
                    probe_fail: Some((how, 2)),
                    ..Mux::default()
                },
            );
            assert_eq!(ensure_run(&w), refused(&["%7"]), "{how}");
            assert_eq!(count(&w, "split-window"), 0, "{how}");
            assert_eq!(roles(&panes), "%1=|,%2=other|,%7=sidebar|", "{how}");
        }
        // An option that cannot be read may say off: nothing is listed or opened.
        for how in ["exit", "throw"] {
            let (w, _) = sidebar_world(
                "",
                Mux {
                    option_fail: Some(how),
                    ..Mux::default()
                },
            );
            assert!(ensure_run(&w).starts_with("1 "), "{how}");
            assert_eq!(calls(&w), [READ_OPTION], "{how}");
        }
        // A split that fails opens nothing.
        let (w, panes) = sidebar_world(
            "",
            Mux {
                split_error: Some("no space for new pane"),
                ..Mux::default()
            },
        );
        assert_eq!(ensure_run(&w), "1 rmux tmux call failed");
        assert_eq!(roles(&panes), "%1=|,%2=other|");
    }

    #[test]
    fn close_disables_and_sidebar_enables_under_the_lock() {
        let (w, panes) = sidebar_world(
            "",
            Mux {
                open: true,
                ..Mux::default()
            },
        );
        let fakes = w.fakes.clone();
        w.fakes.lock_probe(move |event| {
            if event.starts_with('-') {
                fakes
                    .mux_calls()
                    .iter()
                    .filter(|c| matches!(c[1].as_str(), "set-option" | "kill-pane" | "if-shell"))
                    .map(|c| c.join(" "))
                    .collect::<Vec<_>>()
                    .join(",")
            } else {
                String::new()
            }
        });
        assert_eq!(run(&w, &["-Close"]), r#"ok {"pane":"%7","closed":true}"#);
        // Off is written inside the lock, before the kill; the record is cleared after it.
        let key = w.caller().server_key;
        assert_eq!(
            w.fakes.locks()[1],
            format!(
                "-sidebar|{key}|$3 {OFF},{},{CLEAR_RECORD}",
                close_line("%7", 107)
            )
        );
        assert_eq!(option(&panes), Some("off".into()));
        // Once close has returned, ensure (as a spawn runs it) opens nothing.
        w.fakes.clear_log();
        assert_eq!(ensure_run(&w), "disabled");
        assert_eq!(count(&w, "split-window"), 0);
        assert_eq!(roles(&panes), "%1=|,%2=other|");
        // sidebar turns it back on, inside the lock, and opens a new bar with its own record.
        w.fakes.clear_log();
        assert_eq!(
            run(&w, &[]),
            r#"ok {"pane":"%8","window":"@5","opened":true}"#
        );
        assert_eq!(option(&panes), None);
        assert_eq!(
            w.fakes.locks()[1],
            format!(
                "-sidebar|{key}|$3 {ON},{},-N set-option -t $3 @mux-agents-sidebar-id %8 108 1080,{}",
                label_line("%8", 108),
                resize_line("%8", 108, 40)
            )
        );
        assert_eq!(ensure_run(&w), "existing %8");
    }

    #[test]
    fn option_write_failure_refuses_before_any_pane_call() {
        for args in [&["-Close"][..], &[]] {
            for how in ["exit", "throw"] {
                let (w, panes) = sidebar_world(
                    "",
                    Mux {
                        open: true,
                        option_fail: Some(how),
                        ..Mux::default()
                    },
                );
                assert!(run(&w, args).starts_with("1 "), "{args:?} {how}");
                assert_eq!(
                    (count(&w, "list-panes"), closes(&w)),
                    (0, 0),
                    "{args:?} {how}"
                );
                assert_eq!(
                    roles(&panes),
                    "%1=|,%2=other|,%7=sidebar|",
                    "{args:?} {how}"
                );
            }
        }
    }
}

#[cfg(test)]
#[path = "sidebar_hc_tests.rs"]
mod hc_tests;
