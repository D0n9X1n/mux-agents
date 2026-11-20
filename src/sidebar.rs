//! The side bar (MuxAgents.ps1:1423-1591): `sidebar [-Close]` opens or closes one bar per session running
//! `<abs binary> watch`, and `watch` draws the session's board in it until its pane closes.

pub(crate) mod input;
mod jobs;
mod keys;
mod pick;
mod tree;

use std::cmp::Reverse;
use std::io;
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value};

use crate::agents::board::board;
use crate::agents::spawn::{pane_env, utf8};
use crate::agents::view::Focused;
use crate::agents::{lock, pane_id, scope, text};
use crate::caller::{Caller, resolve};
use crate::cli::Request;
use crate::ctx::{Ctx, Env};
use crate::error::{Res, refuse};
use crate::mux::{Output, native};
use crate::paint::{self, Color, Style};
use input::{Input, NoInput, Waited};
use keys::{Decoder, Event, Keymap};
use pick::{Act, Answer, Pick, Target};

/// How often the bar reads the board (`tick`), and how often between ticks it looks, cheaply, for a change it should
/// show at once (`poll`): another pane on screen (a tab switch) or an agent's folder written (a report, a status, a
/// message). A full read costs about 100 ms of calls with six agents, so it runs once a second; a poll is one
/// multiplexer read and a few file stats, so it runs every 200 ms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pace {
    pub tick: Duration,
    pub poll: Option<Duration>,
}

impl Default for Pace {
    #[cfg(not(test))]
    fn default() -> Self {
        Pace {
            tick: Duration::from_millis(1000),
            poll: Some(Duration::from_millis(200)),
        }
    }
    // The unit tests pin the frames of a 2 s tick with no poll; `pace` sets another for one test.
    #[cfg(test)]
    fn default() -> Self {
        Pace {
            tick: Duration::from_millis(2000),
            poll: None,
        }
    }
}

std::thread_local! {
    static PACE: std::cell::Cell<Option<Pace>> = const { std::cell::Cell::new(None) };
}

/// The pace this thread's watch runs at.
fn current_pace() -> Pace {
    PACE.with(|p| p.get()).unwrap_or_default()
}

/// Tests: the pace for this thread's next watch runs.
#[cfg(test)]
fn pace(p: Pace) {
    PACE.with(|c| c.set(Some(p)));
}

/// What a poll compares: the newest change time among the agents' folders, their generations' folders and the
/// mailboxes. Each write is a rename into its folder (`state::fs::write_json`: an agent's own files, a generation's
/// exit evidence under `gens/<n>/`) or an append to a mailbox file, so one of these times moves with it; no process is
/// started. Unreadable entries are skipped.
fn stamp(root: &std::path::Path) -> Option<SystemTime> {
    let time = |p: &std::path::Path| std::fs::metadata(p).ok().and_then(|m| m.modified().ok());
    let entries = |dir: &std::path::Path| -> Vec<std::fs::DirEntry> {
        std::fs::read_dir(dir)
            .map(|d| d.filter_map(|e| e.ok()).collect())
            .unwrap_or_default()
    };
    let mut newest = time(&root.join("agents"));
    let mut see = |t: Option<SystemTime>| newest = newest.max(t);
    for agent in entries(&root.join("agents")) {
        see(time(&agent.path()));
        let gens = agent.path().join("gens");
        see(time(&gens));
        for generation in entries(&gens) {
            see(time(&generation.path()));
        }
    }
    for mailbox in entries(&root.join("mail")) {
        see(time(&mailbox.path()));
    }
    newest
}
// An idle bar under a poll writes its unchanged frame again this often: a terminal that dropped it (a clear, a
// popup over it) gets it back, and what reads the bar's output sees it is alive.
const REWRITE: Duration = Duration::from_secs(2);
// The walk over the other view windows (two reads each) runs on the first tick and every this many after.
const WALK_EVERY: u32 = 5;
// How long a lone ESC may wait for the rest of a sequence before it counts as the Esc key.
const ESC_WAIT: Duration = Duration::from_millis(50);
// A minute of ticks with no answer from the multiplexer: a write to a pipe whose reader closed can still succeed.
const UNKNOWN_MAX: u32 = 30;
// The bar's own pane, its label and its size, read each tick (S2.1: the layout follows the actual pane).
const WATCH_PROBE: &str = "#{pane_id}|#{@agent-role}|#{pane_width}|#{pane_height}";
// The bar's own size, read again after the keeper moved or resized it, so the frame fits the pane as it is now.
const SIZE_FORMAT: &str = "#{pane_id}|#{pane_width}|#{pane_height}";
// What the bar's split answers: its pane, window and process (the bar is that process, not its label).
const SPLIT_FORMAT: &str = "#{pane_id}|#{window_id}|#{pane_pid}";
// The proof read of the recorded pane: alive, and still the recorded process.
const PROOF_FORMAT: &str = "#{pane_id}|#{window_id}|#{pane_dead}|#{pane_pid}";
// The re-read after a conditional kill: a gone pane answers empty fields.
pub const GONE_FORMAT: &str = "#{pane_id}|#{pane_pid}";
// Each pane of the session and whether it carries the side-bar label and is alive, compared by the server: a label
// value with a newline or `|` cannot forge a line, and a dead pane (remain-on-exit) never counts as labelled.
const LABEL_FORMAT: &str = "#{pane_id}|#{&&:#{==:#{@agent-role},sidebar},#{==:#{pane_dead},0}}";
// The label as LABEL_FORMAT reads it, then the pane's pid and the bar's mark: what tells a bar still running apart.
const LEFT_FORMAT: &str = "#{pane_id}|#{&&:#{==:#{@agent-role},sidebar},#{==:#{pane_dead},0}}|#{pane_pid}|#{@mux-agents-bar}";
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
// `1` on a window once a verb placed or focused a member view in it (`ensure`, under the session lock), so the
// window's bar knows it held a view even when the view is gone before the bar's first look. Unset when the
// bar leaves. shortcut: a window viewed while the bar was on, whose view then closed with no bar left to unset it
// (the bar closed by `sidebar -Close` first), keeps it; a bar opened there later by `sidebar` leaves at once.
const VIEWED: &str = "@mux-agents-viewed";
/// reentry-6a E4.5: a window `up` opened because no member runs; it never holds a view, so its bar stays.
pub const STATUS: &str = "@mux-agents-status";
// The session's bar as `<pane> <pid> <start>`, written only by `open` once the split's process is this binary
// `watch`, cleared by `sidebar -Close` once the bar is gone.
const RECORD: &str = "@mux-agents-sidebar-id";
// The bar's mark: a pane option set to the pid of the split that made the bar, written right after the split
// and never cleared, so a pane an open left behind (its watch not started in time, or not labelled) is still known
// as ours. Only a pane whose mark equals its own pid is ours: a mark rotate-window moved to another pane is not.
const MARK: &str = "@mux-agents-bar";
// A window's panes with their pid, whether they are dead and the bar's mark: `<pid>|<dead>|<pane>|<mark>`.
const MARK_FORMAT: &str = "#{pane_pid}|#{pane_dead}|#{pane_id}|#{@mux-agents-bar}";
// A watch not proven (its window's record not naming its pane) for this long, since it started or since it was last
// proven, whose pane carries its own mark, removes its label and mark and leaves, so its pane closes with it.
const STARTUP: Duration = Duration::from_secs(10);
// The flag the bar's split passes its watch, and only it (`Origin::Split`), followed by its state root's key.
const MANAGED: &str = "-Managed";
// The watch's own pane's pid and the bar's mark.
const OWN_MARK: &str = "#{pane_pid}|#{@mux-agents-bar}";
// A new split's process may not have exec'd yet: it is read up to this many times, 50 ms apart.
const WATCH_TRIES: u32 = 20;

/// Who started a watch: a person (`watch`), or the split of the bar's open (`watch -Managed <key>`, `open`), which no
/// person passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Hand,
    Split,
}

/// What `ensure` found: a bar it opened, the session's bar, or none because the session turned it off.
#[derive(Debug, PartialEq, Eq)]
pub enum Bar {
    Opened(String),
    Existing(String),
    Disabled,
}

/// The bar of `window` a verb keeps open: under the session's sidebar lock, nothing when the session's option says
/// `off`, else that window's bar, found or opened in it. A failed read or probe refuses and opens nothing.
pub fn ensure(ctx: &Ctx, caller: &Caller, window: &str) -> Res<Bar> {
    lock(ctx, &scope::shown_lock("sidebar", caller), || {
        ensure_locked(ctx, caller, window)
    })
}

/// `ensure`'s work, under the session's sidebar lock its caller holds.
fn ensure_locked(ctx: &Ctx, caller: &Caller, window: &str) -> Res<Bar> {
    {
        let read = native(
            ctx,
            &["show-options", "-qv", "-t", &caller.session, OPTION],
            false,
        )?;
        if read.stdout.trim_end_matches(['\r', '\n']) == "off" {
            return Ok(Bar::Disabled);
        }
        // The window held a member view: a verb placed or focused one there. Best effort: the bar also sees
        // the view itself.
        let _ = native(ctx, &["set-option", "-w", "-t", window, VIEWED, "1"], true);
        migrate(ctx, &caller.session)?;
        let (pane, _, opened) = find_or_open(ctx, window)?;
        Ok(if opened {
            Bar::Opened(pane)
        } else {
            Bar::Existing(pane)
        })
    }
}

/// The best-effort bar of `window` for a verb whose own work is done (spawn, focus): `ensure` under catch_unwind, so
/// neither a refusal nor a panic (which unwinding has already freed the sidebar lock from) fails the verb. The reply
/// gets `sidebar` (the bar's pane, or null when off or failed) and, on failure, `sidebarError`.
pub fn keep(ctx: &Ctx, caller: &Caller, window: &str, reply: &mut Map<String, Value>) {
    let bar =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ensure(ctx, caller, window)))
            .unwrap_or_else(|_| refuse(1, "internal", "unexpected error (panic)"));
    match bar {
        Ok(Bar::Opened(pane) | Bar::Existing(pane)) => {
            reply.insert("sidebar".into(), pane.into());
        }
        Ok(Bar::Disabled) => {
            reply.insert("sidebar".into(), Value::Null);
        }
        Err(refusal) => {
            reply.insert("sidebar".into(), Value::Null);
            reply.insert("sidebarError".into(), refusal.message.into());
        }
    }
}

/// `window`'s bar as (pane, window, opened): the proven one, else a new one; refused while a pane of that window
/// carries the side-bar label without being the proven bar (a 0.2.1 bar, or a label rmux moved), or the bar's mark
/// for its own pid without being proven (`stuck`), so no second bar opens beside one that cannot be proven.
fn find_or_open(ctx: &Ctx, window: &str) -> Res<(String, String, bool)> {
    if let Some(bar) = prove(ctx, window)? {
        return Ok((bar.pane, bar.window, false));
    }
    let labelled = labelled(ctx, &["-t", window])?;
    if !labelled.is_empty() {
        return refuse(1, "sidebar", unproven(&labelled));
    }
    stuck(ctx, window)?;
    let (pane, window) = open(ctx, window)?;
    Ok((pane, window, true))
}

/// Refuses while a pane of `window` carries the bar's mark for its own pid but is not the proven bar: an open
/// that failed after its split left it, and its watch proves itself (then `prove` adopts it) or leaves within
/// STARTUP. Nothing is killed here: the refusal names the pane. A mark for another pid (one rotate-window moved) is
/// no bar's, and a dead pane blocks nothing. A list that cannot be read refuses: it may hold such a pane.
/// shortcut: a watch the bar's split started whose opener died before the mark carries none, so it blocks nothing
/// here; an ensure within its STARTUP opens a second bar beside it, and the stray closes itself at STARTUP
/// (`self_exit`, `Origin::Split`). Reading each pane's process here would cost every ensure a process read per pane.
fn stuck(ctx: &Ctx, window: &str) -> Res<()> {
    let listed = native(ctx, &["list-panes", "-t", window, "-F", MARK_FORMAT], false)?;
    for line in listed.stdout.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let mut fields = line.splitn(4, '|');
        let (Some(pid), Some(dead), Some(pane), Some(mark)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if dead == "0" && pane_id(pane) && pid.parse::<u32>().is_ok() && mark == pid {
            return refuse(
                1,
                "sidebar",
                format!(
                    "pane {pane} carries the side-bar mark but is not a proven side bar: a side bar that is starting \
                     proves itself or closes within {} s; if it stays, close it with kill-pane -t {pane}",
                    STARTUP.as_secs()
                ),
            );
        }
    }
    Ok(())
}

/// 0.2.x kept one bar per session, recorded as a session option. Under the session's sidebar lock, before any
/// window's bar is looked at: a proven session record moves to its bar's window (where `prove` finds it) and is
/// unset, so it is moved once; an unproven one refuses while any pane of the session carries the side-bar label (it
/// may be that bar), else it names nothing and is unset. An unreadable record refuses: it may name the bar.
fn migrate(ctx: &Ctx, session: &str) -> Res<()> {
    let read = native(ctx, &["show-options", "-qv", "-t", session, RECORD], false)?;
    let text = read.stdout.trim_end_matches(['\r', '\n']);
    if text.is_empty() {
        return Ok(());
    }
    let window = parse_record(text)
        .and_then(|(pane, pid, start)| (0..2).find_map(|_| proven(ctx, &pane, pid, start)));
    // A window destroyed between the proof and the move took the bar with it, unless its pane moved on: as
    // unproven as a record proven nowhere.
    let moved = match window {
        Some(window) => on_window(
            ctx,
            session,
            &window,
            &["set-option", "-w", "-t", &window, RECORD, text],
        )?
        .is_some(),
        None => false,
    };
    if !moved {
        let labelled = labelled(ctx, &["-s", "-t", session])?;
        if !labelled.is_empty() {
            return refuse(1, "sidebar", unproven(&labelled));
        }
    }
    native(ctx, &["set-option", "-u", "-t", session, RECORD], false)?;
    Ok(())
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
const PLAIN: Style = Style {
    bold: false,
    dim: false,
    reverse: false,
    color: None,
    bg: None,
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
// The soft background of the row on screen: a 256-color dark grey, a faint band rather than a highlight.
const BAND: u8 = 236;
// The main chat's glyph: Nerd Font `nf-fa-terminal` (`$` in ASCII).
const MAIN: &str = "\u{f120}";

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Blocked,
    OnCall,
    Done,
    Working,
    Idle,
    Finished,
    Exit,
    Stopped,
    Other,
}

/// The flat bar's live sections, by what an agent asks of the person: Inbox (needs them, done and not yet seen,
/// recovery, or unread mail), Working, then Free (idle or on call).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Inbox,
    Working,
    Free,
}

impl Section {
    const ALL: [Section; 3] = [Section::Inbox, Section::Working, Section::Free];

    /// A row's section: the board reports `done` only until the person has seen that done spell.
    fn of(row: &Map<String, Value>, kind: Kind) -> Section {
        match kind {
            _ if has_mail(row) || recovery_word(row).is_some() => Section::Inbox,
            Kind::Blocked | Kind::Done => Section::Inbox,
            Kind::Working => Section::Working,
            _ => Section::Free,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Section::Inbox => "Inbox",
            Section::Working => "Working",
            Section::Free => "Free",
        }
    }

    /// The heading's word: Inbox bold yellow, Working bold green, Free bold dim; only Inbox stands out.
    fn style(self) -> Style {
        match self {
            Section::Inbox => Style {
                bold: true,
                ..hued(Color::Yellow)
            },
            Section::Working => Style {
                bold: true,
                ..hued(Color::Green)
            },
            Section::Free => Style { bold: true, ..DIM },
        }
    }
}

/// Whether `row` has unread mail: it stands in Inbox, an ended agent's row too, until the mail is read.
fn has_mail(row: &Map<String, Value>) -> bool {
    row.get("unread").and_then(Value::as_u64).unwrap_or(0) > 0
}

impl Kind {
    /// A live row's kind: its `activity`, else (a feed without it) `waiting`.
    fn live(row: &Map<String, Value>) -> Kind {
        match row.get("activity").and_then(Value::as_str) {
            Some("blocked") => Kind::Blocked,
            Some("on call") => Kind::OnCall,
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
            (Kind::OnCall, false) => "\u{25cc}",
            (Kind::OnCall, true) => "o",
            (Kind::Stopped | Kind::Other, _) => "-",
        }
    }

    /// A live kind's word.
    fn word(self) -> &'static str {
        match self {
            Kind::Blocked => "needs you",
            Kind::OnCall => "on call",
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
            Kind::Idle | Kind::Finished | Kind::Other | Kind::OnCall => DIM,
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

/// o2 call 34: `agent_line` for a recovery row, whose right side (`recover`) is mandatory. When it cannot keep the right
/// side as `agent_line` does, the model goes, the name is cut, the unread badge yields, then the name goes; only a
/// right side wider than the whole row is cut.
fn kept_line(glyph: Span, name: Span, model: Row, badge: Row, right: Row, cols: usize) -> Row {
    let (glyph_w, right_w) = (paint::width(&glyph.0), row_width(&right));
    if glyph_w + 1 + row_width(&badge) + 2 + right_w + 4 <= cols {
        return agent_line(glyph, name, model, badge, right, cols);
    }
    let line = |left: Row, right: Row| {
        let pad = cols.saturating_sub(row_width(&left) + row_width(&right));
        let mut row = left;
        row.push(span(" ".repeat(pad), PLAIN));
        row.extend(right);
        row
    };
    for badge in [badge.clone(), Vec::new()] {
        let used = glyph_w + 1 + row_width(&badge) + 2 + right_w;
        if used < cols {
            let mut left = vec![
                glyph.clone(),
                span(" ", PLAIN),
                (paint::cut(&name.0, cols - used), name.1),
            ];
            left.extend(badge);
            return line(left, right);
        }
    }
    if glyph_w + 1 + right_w <= cols {
        return line(vec![glyph], right);
    }
    let mut row = vec![glyph, span(" ", PLAIN)];
    row.extend(right);
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

/// One agent of a live section: its line, its doing line, its age (the oldest drop first) and what focusing it needs.
struct Live {
    section: Section,
    line: Row,
    // The line under a section heading, as herdr's compact status: the glyph and the time, no word (the heading
    // names the state), so the name keeps the room.
    compact: Row,
    doing: Option<Row>,
    age: Option<i64>,
    target: Target,
}

/// The rows as drawn, and each drawn running agent with its name line's row, top to bottom.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Laid {
    pub rows: Vec<Row>,
    pub agents: Vec<(Target, usize)>,
    /// The other rows a drawn agent's entry takes (its doing line), by member id: a click there opens it too.
    pub more: Vec<(String, usize)>,
}

/// The drawn running agents, top to bottom.
#[cfg(test)]
fn targets(laid: &Laid) -> Vec<Target> {
    laid.agents
        .iter()
        .map(|(target, _)| target.clone())
        .collect()
}

/// Each drawn agent row: its member id, its row and its text after column 0, as `pick::frame_hit` compares them
/// (a doing line's margin is part of its indent span, so the text is cut after its first character, not its first span).
fn drawn(laid: &Laid) -> Vec<(String, usize, String)> {
    let text = |at: usize| {
        let line: String = laid.rows[at].iter().map(|(t, _)| t.as_str()).collect();
        line.chars().skip(1).collect::<String>()
    };
    let more = laid.more.iter().map(|(id, at)| (id, at));
    laid.agents
        .iter()
        .map(|(t, at)| (&t.id, at))
        .chain(more)
        .map(|(id, at)| (id.clone(), *at, text(*at)))
        .collect()
}

/// The rows a board can draw, before the height decides which are kept.
struct Parts {
    // Between a section's name and its count: `·`, or `-` in ASCII.
    dot: &'static str,
    header: Row,
    // The live sections' agents, in the board's order (`assemble` groups them).
    live: Vec<Live>,
    // The ended fold's line and target (none without ended agents), and with the fold open its rows, newest first.
    fold: Option<(Row, Target)>,
    open: Vec<(Row, Target)>,
    // Every ended agent the fold counts.
    ended_total: usize,
    nothing: bool,
    notes: Vec<Row>,
}

/// Which parts are kept (S2.1's overflow order, in reverse).
struct Shape {
    gaps: usize,
    // The section headings, above each section's agents.
    sections: bool,
    doing: Vec<bool>,
    shown: Vec<bool>,
    live_more: bool,
    // The open fold's rows kept (the newest), and its `+N older` line.
    open: usize,
    older: bool,
    fold: bool,
    nothing: bool,
    notes: usize,
}

/// An ended, lost or unknown row's kind and word (S2.3), as `parts` and the tree draw it.
fn ended_kind(row: &Map<String, Value>) -> (Kind, String) {
    let state = clean(row.get("state"));
    match state.as_str() {
        "lost" | "unknown" => (Kind::Other, state.clone()),
        value if value.starts_with("exited ") => (Kind::Exit, state.clone()),
        "exited" => match row.get("exitCode").filter(|code| !code.is_null()) {
            Some(code) if clean(Some(code)) == "0" => (Kind::Finished, "done".to_string()),
            Some(code) => (Kind::Exit, format!("exit {}", clean(Some(code)))),
            None => (Kind::Exit, "exit ?".to_string()),
        },
        "stopped" => (Kind::Stopped, state.clone()),
        _ => (Kind::Other, state.clone()),
    }
}

/// The header's counts (B7: a team board holds only the team): `N inbox` (yellow), then `N working`, each only when
/// not 0; `0 running` when no agent lives. Free and ended agents are not counted here: the Free heading and the ended
/// fold line count them; on the flat bar an ended agent with unread mail stands in Inbox and is counted there.
fn counts(board: &Map<String, Value>) -> Vec<(String, Style)> {
    let flat = !tree::is_team(Some(board));
    let (mut inbox, mut working, mut live) = (0, 0, 0);
    for row in board
        .get("agents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
    {
        let running = clean(row.get("state")) == "running";
        live += usize::from(running);
        if running || recovery_word(row).is_some() || (flat && has_mail(row)) {
            match Section::of(row, Kind::live(row)) {
                Section::Inbox => inbox += 1,
                Section::Working if running => working += 1,
                _ => {}
            }
        }
    }
    let mut counts = Vec::new();
    if inbox > 0 {
        counts.push((format!("{inbox} inbox"), hued(Color::Yellow)));
    }
    if working > 0 {
        counts.push((format!("{working} working"), PLAIN));
    }
    if live == 0 {
        counts.push(("0 running".into(), PLAIN));
    }
    counts
}

/// The unread badge after a name: `✉ N` (ASCII `m N`), none at 0. Live and recovery rows share it (B8).
fn unread_badge(row: &Map<String, Value>, look: Look) -> Row {
    let unread = row.get("unread").and_then(Value::as_u64).unwrap_or(0);
    if unread == 0 {
        return Vec::new();
    }
    vec![
        span("  ", PLAIN),
        span(
            format!("{} {unread}", if look.ascii { "m" } else { MAIL }),
            BADGE,
        ),
    ]
}

/// A live row's pieces before layout: `live_row` and the team tree lay them out each its own way.
struct LiveParts {
    kind: Kind,
    muted: bool,
    glyph: Span,
    name: Span,
    model: Row,
    badge: Row,
    age: String,
    doing: String,
}

fn live_parts(row: &Map<String, Value>, look: Look) -> LiveParts {
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
    // The time since the activity last changed, else since the agent started.
    let since = row
        .get("since")
        .filter(|since| !since.is_null())
        .or_else(|| row.get("age"));
    LiveParts {
        kind,
        muted,
        glyph: span(kind.glyph(look.ascii), kind.style()),
        name: span(
            own_glyphs(&name),
            Style {
                color: hue,
                dim: muted,
                ..PLAIN
            },
        ),
        model,
        badge: unread_badge(row, look),
        age: format_age(since),
        doing: own_glyphs(&clean(row.get("doing"))).trim().to_string(),
    }
}

/// outcome-2 O6 (calls 9, 20): the flat bar's word for a member that needs recovery, running or not.
fn recovery_word(row: &Map<String, Value>) -> Option<&'static str> {
    (row.get("needsRecovery") == Some(&Value::Bool(true))).then(|| {
        if row.get("recoveryRunning") == Some(&Value::Bool(true)) {
            "recover (still running)"
        } else {
            "recover"
        }
    })
}

/// o2 call 28: a recovery row's right side in every flat layout: its words and age when they fit beside the glyph, the
/// badge and a cut name (`agent_line`'s own test for keeping a right side), else the bare `recover` pill, which
/// `agent_line` keeps where it would drop the longer side (as the wide bar's `bare` floor).
fn recovery_side(
    row: &Map<String, Value>,
    word: &str,
    age: &str,
    glyph: &Span,
    badge: &Row,
    cols: usize,
) -> Row {
    let fixed = paint::width(&glyph.0) + 1 + row_width(badge);
    let fits = |side: &Row| fixed + 2 + row_width(side) + 4 <= cols;
    // restore-6c checklist 18 (P1): a stopped recovery row shows its reason beside `recover` while it fits.
    let reason = clean(row.get("recoveryReason"));
    if word == "recover" && !reason.is_empty() {
        let with = right(Kind::Blocked, &format!("recover: {reason}"), age, false);
        if fits(&with) {
            return with;
        }
    }
    let full = right(Kind::Blocked, word, age, false);
    if fits(&full) {
        full
    } else {
        vec![span("recover", Kind::Blocked.word_style())]
    }
}

/// A live row: its line with its state word, and the compact line a section heading stands over. The model is drawn
/// only when `models` (the live agents' models differ).
fn live_row(row: &Map<String, Value>, cols: usize, look: Look, models: bool) -> Live {
    let mut p = live_parts(row, look);
    if !models {
        p.model = Vec::new();
    }
    let age_style = if p.muted { DIM } else { PLAIN };
    // Call 20: a Running member that needs recovery keeps its live row (still clickable) with the recovery word.
    let recovery =
        recovery_word(row).map(|word| recovery_side(row, word, &p.age, &p.glyph, &p.badge, cols));
    let side = recovery
        .clone()
        .unwrap_or_else(|| right(p.kind, p.kind.word(), &p.age, p.muted));
    // o2 call 34: a recovery row never drops its pill.
    let fit: fn(Span, Span, Row, Row, Row, usize) -> Row = if recovery.is_some() {
        kept_line
    } else {
        agent_line
    };
    let line = fit(
        p.glyph.clone(),
        p.name.clone(),
        p.model.clone(),
        p.badge.clone(),
        side,
        cols,
    );
    let compact = fit(
        p.glyph,
        p.name,
        p.model,
        p.badge,
        // o2 call 28: under a section heading a recovery row keeps `recover` in place of its bare age.
        recovery.unwrap_or_else(|| vec![span(format!("{:>4}", p.age), age_style)]),
        cols,
    );
    Live {
        section: Section::of(row, p.kind),
        line: margin(line),
        compact: margin(compact),
        doing: (!p.doing.is_empty()).then(|| indented(&p.doing, cols)),
        age: row.get("age").and_then(Value::as_i64),
        target: Target::of(row),
    }
}

/// A member that needs recovery and no longer runs: in Inbox with its `recover` pill, never folded away with the ended
/// agents, since the person has to act on it.
fn recovery_row(row: &Map<String, Value>, word: &str, cols: usize, look: Look) -> Live {
    let glyph = span(Kind::Blocked.glyph(look.ascii), Kind::Blocked.style());
    let age = format_age(row.get("age"));
    let side = resuming(row, look)
        .unwrap_or_else(|| recovery_side(row, word, &age, &glyph, &Vec::new(), cols));
    let name = span(own_glyphs(&clean(row.get("name"))), DIM);
    let line = margin(kept_line(glyph, name, Vec::new(), Vec::new(), side, cols));
    Live {
        section: Section::Inbox,
        compact: line.clone(),
        line,
        doing: None,
        age: row.get("age").and_then(Value::as_i64),
        target: Target::of(row),
    }
}

/// An agent that no longer runs but left unread mail: in Inbox, never folded away, with its ended glyph and word (only
/// `exit N` in red, as the fold draws it), its dim name and its ✉ badge. Once its mail is read it goes back to the fold.
fn mail_row(row: &Map<String, Value>, cols: usize, look: Look) -> Live {
    let (kind, word) = ended_kind(row);
    let quiet = |style: Style| if kind == Kind::Exit { style } else { DIM };
    let glyph = span(kind.glyph(look.ascii), quiet(kind.style()));
    let age = ended_age(row);
    let side = resuming(row, look).unwrap_or_else(|| {
        vec![
            span(word, quiet(kind.word_style())),
            span(format!(" {age:>4}"), DIM),
        ]
    });
    let name = span(own_glyphs(&clean(row.get("name"))), DIM);
    let badge = unread_badge(row, look);
    let line = margin(agent_line(glyph, name, Vec::new(), badge, side, cols));
    Live {
        section: Section::Inbox,
        compact: line.clone(),
        line,
        doing: None,
        age: row.get("age").and_then(Value::as_i64),
        target: Target::of(row),
    }
}

/// The right side of a row whose resume runs (`Jobs::mark`): `resuming…` (`resuming...` in ASCII), plain, until the
/// board catches up.
fn resuming(row: &Map<String, Value>, look: Look) -> Option<Row> {
    (row.get("resuming") == Some(&Value::Bool(true))).then(|| {
        let word = if look.ascii {
            "resuming..."
        } else {
            "resuming\u{2026}"
        };
        vec![span(word, PLAIN)]
    })
}

/// An ended row's time as the bar shows it: since it ended (`endedAgo`), else (a lost or unknown row, which has not
/// ended) its age.
fn ended_age(row: &Map<String, Value>) -> String {
    format_age((row.get("endedAgo").filter(|v| !v.is_null())).or_else(|| row.get("age")))
}

/// An open fold's row: its glyph, dim name and dim time since it ended; only `exit N` keeps its word, in red, so the
/// fold stays quiet.
fn ended_row(row: &Map<String, Value>, cols: usize, look: Look) -> (Row, Target) {
    let (kind, word) = ended_kind(row);
    let age = ended_age(row);
    let exit = kind == Kind::Exit;
    let side = if let Some(side) = resuming(row, look) {
        side
    } else if exit {
        vec![
            span(word, kind.word_style()),
            span(format!(" {age:>4}"), DIM),
        ]
    } else {
        vec![span(format!("{age:>4}"), DIM)]
    };
    let glyph = span(
        kind.glyph(look.ascii),
        if exit { kind.style() } else { DIM },
    );
    let name = span(own_glyphs(&clean(row.get("name"))), DIM);
    let line = agent_line(glyph, name, Vec::new(), Vec::new(), side, cols);
    (margin(line), Target::of(row))
}

/// The ended fold's line, dim: `▸ Ended · N` closed, `▾ Ended · N` open (`>` and `v` in ASCII).
fn fold_line(total: usize, open: bool, dot: &str, cols: usize, look: Look) -> Row {
    let mark = match (open, look.ascii) {
        (false, false) => "\u{25b8}",
        (true, false) => "\u{25be}",
        (false, true) => ">",
        (true, true) => "v",
    };
    let text = format!("{mark} Ended {dot} {total}");
    margin(vec![span(paint::cut(&text, cols), DIM)])
}

/// Between a heading's name and its count, and between the header's counts: `·`, or `-` in ASCII.
fn dot(look: Look) -> &'static str {
    if look.ascii { "-" } else { "\u{b7}" }
}

/// `mux-agents` bold, then the counts right-aligned when they fit, joined by a dim ` · `. Short of room the counts lose
/// their last part, then the next, then the title stands alone.
fn header(parts: &[(String, Style)], dot: &str, cols: usize) -> Row {
    let title = span("mux-agents", BOLD);
    let shown = |n: usize| -> Row {
        let mut row = Vec::new();
        for (at, (text, style)) in parts[..n].iter().enumerate() {
            if at > 0 {
                row.push(span(format!(" {dot} "), DIM));
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
    ended_open: bool,
) -> Parts {
    let dot = dot(look);
    let notes = notes
        .iter()
        .map(|(note, style)| margin(vec![span(paint::cut(&own_glyphs(note), cols), *style)]))
        .collect();
    let Some(board) = board else {
        return Parts {
            dot,
            header: header(&[("? running".into(), PLAIN)], dot, cols),
            live: Vec::new(),
            fold: None,
            open: Vec::new(),
            ended_total: 0,
            nothing: false,
            notes,
        };
    };
    let rows: Vec<&Map<String, Value>> = board
        .get("agents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .collect();
    // Every printed field, state included, is cleaned first.
    let running = |r: &Map<String, Value>| clean(r.get("state")) == "running";
    let mut models = rows
        .iter()
        .filter(|r| running(r))
        .map(|r| clean(r.get("model")));
    let first = models.next();
    let models = models.any(|m| Some(&m) != first.as_ref());
    let (mut live, mut ended) = (Vec::new(), Vec::new());
    for row in &rows {
        if running(row) {
            live.push(live_row(row, cols, look, models));
        } else if let Some(word) = recovery_word(row) {
            live.push(recovery_row(row, word, cols, look));
        } else if has_mail(row) {
            live.push(mail_row(row, cols, look));
        } else {
            ended.push(*row);
        }
    }
    // `endedTotal` counts the terminal-Ended agents: one in Inbox (recovery or unread mail) leaves the fold's count, a
    // lost or unknown row the fold lists joins it.
    let ended_life = |r: &Map<String, Value>| matches!(clean(r.get("life")).as_str(), "" | "ended");
    let total = match board.get("endedTotal").and_then(Value::as_u64) {
        Some(total) => {
            let inbox = (rows.iter())
                .filter(|r| {
                    !running(r) && (recovery_word(r).is_some() || has_mail(r)) && ended_life(r)
                })
                .count();
            let other = ended.iter().filter(|r| !ended_life(r)).count();
            (total as usize).saturating_sub(inbox) + other
        }
        None => ended.len(),
    }
    .max(ended.len());
    // Open, the fold lists the agents that ended within `ENDED_RECENT`, in the board's order (newest end first): an
    // ended agent by when it ended (`endedAgo`; without it, older), a lost or unknown one, which has not ended, by its
    // age.
    let open = if ended_open {
        let recent = |r: &&Map<String, Value>| {
            let seconds = match ended_life(r) {
                true => r.get("endedAgo").and_then(Value::as_u64),
                false => r.get("age").and_then(Value::as_u64),
            };
            seconds.is_some_and(|s| s < crate::agents::board::ENDED_RECENT)
        };
        ended.retain(recent);
        ended.iter().map(|r| ended_row(r, cols, look)).collect()
    } else {
        Vec::new()
    };
    Parts {
        dot,
        header: header(&counts(board), dot, cols),
        nothing: live.is_empty(),
        live,
        fold: (total > 0).then(|| {
            (
                fold_line(total, ended_open, dot, cols, look),
                Target::fold(),
            )
        }),
        open,
        ended_total: total,
        notes,
    }
}

/// One block of the frame: its rows, and each target on them (its name line's row, the target, its doing line's row).
type Block = (Vec<Row>, Vec<(usize, Target, Option<usize>)>);

/// The kept parts as rows: blocks (the header, `No agents running.`, each section under its heading, `+J more
/// running`, the ended fold, the notes) with a blank row above the first `gaps` blocks after the header; each drawn
/// target with its row.
fn assemble(parts: &Parts, shape: &Shape, cols: usize) -> Laid {
    let mut blocks: Vec<Block> = vec![(vec![parts.header.clone()], Vec::new())];
    if shape.nothing {
        let line = margin(vec![span(paint::cut("No agents running.", cols), DIM)]);
        blocks.push((vec![line], Vec::new()));
    }
    for section in Section::ALL {
        let mut block: Block = (Vec::new(), Vec::new());
        if shape.sections {
            let count = parts.live.iter().filter(|l| l.section == section).count();
            let heading = vec![
                span(section.title(), section.style()),
                span(format!(" {} {count}", parts.dot), DIM),
            ];
            block.0.push(margin(cut_row(heading, cols)));
        }
        let mut any = false;
        for (at, live) in parts.live.iter().enumerate() {
            if live.section != section || !shape.shown[at] {
                continue;
            }
            any = true;
            let line_at = block.0.len();
            // Under a heading, Working and Free rows show glyph and age only (the heading names their state); Inbox
            // rows keep their word, since their reasons differ.
            block
                .0
                .push(if shape.sections && section != Section::Inbox {
                    live.compact.clone()
                } else {
                    live.line.clone()
                });
            let doing_at = live.doing.as_ref().filter(|_| shape.doing[at]).map(|d| {
                block.0.push(d.clone());
                block.0.len() - 1
            });
            block.1.push((line_at, live.target.clone(), doing_at));
        }
        if any {
            blocks.push(block);
        }
    }
    let hidden = shape.shown.iter().filter(|shown| !**shown).count();
    if hidden > 0 && shape.live_more {
        let line = indented(&format!("+{hidden} more running"), cols);
        blocks.push((vec![line], Vec::new()));
    }
    if let Some((line, target)) = parts.fold.as_ref().filter(|_| shape.fold) {
        let mut block: Block = (vec![line.clone()], vec![(0, target.clone(), None)]);
        for (row, target) in &parts.open[..shape.open] {
            block.1.push((block.0.len(), target.clone(), None));
            block.0.push(row.clone());
        }
        let older = parts.ended_total.saturating_sub(shape.open);
        if shape.older && older > 0 {
            block.0.push(indented(&format!("+{older} older"), cols));
        }
        blocks.push(block);
    }
    if shape.notes > 0 {
        blocks.push((parts.notes[..shape.notes].to_vec(), Vec::new()));
    }
    let mut laid = Laid::default();
    for (at, (rows, targets)) in blocks.into_iter().enumerate() {
        if at > 0 && at <= shape.gaps {
            laid.rows.push(Row::new());
        }
        let base = laid.rows.len();
        for (line_at, target, doing_at) in targets {
            laid.more
                .extend(doing_at.map(|d| (target.id.clone(), base + d)));
            laid.agents.push((target, base + line_at));
        }
        laid.rows.extend(rows);
    }
    laid
}

/// Drops the next part in S2.1's overflow order: blank rows (the lowest first), the section headings, the open fold's
/// rows (the oldest first, into `+N older`) and then that line, doing lines of the oldest live agents, whole oldest
/// live agents (into `+J more running`), then the fold line, `+J more running`, `No agents running.` and the notes.
/// False when only the header is left.
fn drop_one(parts: &Parts, shape: &mut Shape) -> bool {
    if shape.gaps > 0 {
        shape.gaps -= 1;
        return true;
    }
    if shape.sections {
        shape.sections = false;
        return true;
    }
    if shape.open > 0 {
        shape.open -= 1;
        return true;
    }
    if shape.older {
        shape.older = false;
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
    if shape.fold && parts.fold.is_some() {
        shape.fold = false;
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
    let mut laid = lay(board, &notes, size, look);
    dress(
        &mut laid,
        &notes,
        size,
        look,
        Dress::default(),
        &Keymap::default(),
    );
    laid.rows
}

/// Tests (o2 call 30): a board's flat bar as plain text, `width` by `height`, so a board test renders what it built.
#[cfg(test)]
pub(crate) fn flat_text(board: &Map<String, Value>, width: usize, height: usize) -> Vec<String> {
    let look = Look {
        color: false,
        ascii: false,
        dumb: false,
    };
    lay(Some(board), &[], Size { width, height }, look)
        .rows
        .iter()
        .map(|r| render(r, false))
        .collect()
}

/// `layout` with each note in its own style, and the drawn running agents with their rows.
fn lay(
    board: Option<&Map<String, Value>>,
    notes: &[(&str, Style)],
    size: Size,
    look: Look,
) -> Laid {
    lay_focus(board, notes, size, look, tree::Focus::default()).0
}

/// The fewest rows a pane needs to keep the hint when the frame does not fit: a blank row and the hint below at least
/// 6 rows of frame.
const HINT_MIN_HEIGHT: usize = 8;

/// `lay_focus` for the bar's frame: a frame too tall to leave the hint its 2 rows is laid again 2 rows shorter, so
/// the hint stays pinned on a pane of `HINT_MIN_HEIGHT` rows or more and the frame gives way from its bottom. Pure:
/// the second lay costs no call.
fn lay_hinted(
    board: Option<&Map<String, Value>>,
    notes: &[(&str, Style)],
    size: Size,
    look: Look,
    focus: tree::Focus,
) -> Laid {
    let laid = lay_focus(board, notes, size, look, focus).0;
    let asked = notes.iter().any(|(note, _)| pick::asking(note));
    if laid.rows.len() + 2 <= size.height
        || size.height < HINT_MIN_HEIGHT
        || size.width < 12
        || look.dumb
        || asked
    {
        return laid;
    }
    let shorter = Size {
        height: size.height - 2,
        ..size
    };
    lay_focus(board, notes, shorter, look, focus).0
}

/// `lay` with what the bar marks, over the board's kept shells (views-6f ambiguity 15, v6f call 11): the
/// question `close? y/n` takes the last line first, out of the notes `lay_members` may drop; the kept block gets at
/// most half of the rest, never leaving the members fewer than 3 rows, but always a row for a selected kept shell, and
/// none with fewer than 2 rows of room otherwise; the members get the rest. Also how many of the notes it kept, the question included. Under 12 columns, as before.
fn lay_focus(
    board: Option<&Map<String, Value>>,
    notes: &[(&str, Style)],
    size: Size,
    look: Look,
    focus: tree::Focus,
) -> (Laid, usize) {
    if size.width < 12 {
        let (mut laid, kept) = lay_members(board, notes, size, look, focus);
        shade(&mut laid, focus.shown, size.width, look);
        return (laid, kept);
    }
    let (asked, notes): (Vec<_>, Vec<_>) =
        notes.iter().copied().partition(|(n, _)| pick::asking(n));
    let asked_n = asked.len().min(size.height);
    let rest = size.height - asked_n;
    let cols = size.width - 2;
    let shells = kept_targets(board);
    let chosen = focus
        .selected
        .is_some_and(|s| shells.iter().any(|t| t.id == s));
    let room = (rest / 2).min(rest.saturating_sub(3));
    let room = if chosen {
        room.max(rest.min(1))
    } else if room < 2 {
        0
    } else {
        room
    };
    let kept = lay_kept(&shells, cols, look, room, focus.selected);
    // The main rows come right under the header. The first is pinned like the header: it is drawn while the pane has a
    // row for it beside the header, and the agents give way first. Each further one is taken only while the members,
    // laid out in what is left (from the running rows alone when the ended rows no longer fit, the header still
    // counting them), draw every agent they drew without it. Checked on the frame itself, notes and folds included, not
    // predicted.
    let targets = main_targets(board);
    let lay_in = |board: Option<&Map<String, Value>>, height: usize| {
        if height == 0 {
            (Laid::default(), 0)
        } else {
            let size = Size {
                width: size.width,
                height,
            };
            lay_members(board, &notes, size, look, focus)
        }
    };
    // The members a main row must not cost: the live ones, above the ended fold (its line and rows are the ended
    // agents, which the trimmed board leaves out).
    let ids = |laid: &Laid| -> Vec<String> {
        (laid.agents.iter())
            .take_while(|(t, _)| t.life != pick::FOLD)
            .map(|(t, _)| t.id.clone())
            .collect()
    };
    let room = rest - kept.len();
    let bare = lay_in(board, room);
    let want = ids(&bare.0);
    let trimmed = live_only(board);
    let with = |n: usize| {
        let full = lay_in(board, room - n);
        if ids(&full.0) == want {
            return Some(full);
        }
        let (mut laid, shown) = lay_in(trimmed.as_ref(), room - n);
        (ids(&laid) == want).then(|| {
            // The header counts the whole board, ended agents included.
            if let (Some(row), Some(b)) = (laid.rows.first_mut(), board) {
                *row = header(&counts(b), dot(look), cols);
            }
            (laid, shown)
        })
    };
    // The members keep their header (the counts) whatever the main rows take.
    let floor = usize::from(!bare.0.rows.is_empty());
    let pinned = !targets.is_empty() && room > floor;
    let fits = (1..=targets.len().min(room.saturating_sub(floor)))
        .rev()
        .find_map(|n| with(n).map(|l| (n, l)));
    let (n, (mut laid, shown)) = match fits {
        Some(found) => found,
        // No main row fits beside every agent: `main` stays, and the members fold their agents into the rest.
        None if pinned => (1, lay_in(board, room - 1)),
        None => (0, bare),
    };
    let mains: Vec<(Row, Option<Target>)> = (targets.into_iter().take(n))
        .map(|t| (main_row(&t, cols, look), Some(t)))
        .collect();
    fold_kept(&mut laid, kept);
    fold_kept(&mut laid, mains);
    // More space for the main chat at the top: a blank row between the header and the main rows, only in a row the
    // bar has to spare, so it never costs an agent, main or ended row.
    let headed = !laid.rows.is_empty() && !laid.agents.iter().any(|(_, at)| *at == 0);
    if n > 0 && headed && laid.rows.len() + asked_n < size.height {
        laid.rows.insert(1, Row::new());
        for at in
            (laid.agents.iter_mut().map(|(_, at)| at)).chain(laid.more.iter_mut().map(|(_, at)| at))
        {
            *at += 1;
        }
    }
    shade(&mut laid, focus.shown, size.width, look);
    for (n, style) in asked.into_iter().take(asked_n) {
        laid.rows
            .push(margin(vec![span(paint::cut(n, cols), style)]));
    }
    (laid, shown + asked_n)
}

/// Outside a team, the panes the board's agents were spawned from, as main rows: each distinct `parent` that is a pane
/// id and no agent's own pane (a child's parent is an agent), so the person can go back to the chat that started them,
/// also after every agent ended. Numbered by the oldest agent each started (ties: the board's order), so `main` keeps
/// its name while the agents' activity reorders the board. A gone pane is refused at focus.
pub(crate) fn main_targets(board: Option<&Map<String, Value>>) -> Vec<Target> {
    let Some(rows) = board
        .filter(|b| !tree::is_team(Some(b)))
        .and_then(|b| b.get("agents"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let rows: Vec<&Map<String, Value>> = rows.iter().filter_map(Value::as_object).collect();
    let own: Vec<String> = rows.iter().map(|r| clean(r.get("pane"))).collect();
    // Each main pane with the age of its oldest agent.
    let mut panes: Vec<(String, i64)> = Vec::new();
    for row in &rows {
        let parent = clean(row.get("parent"));
        if !crate::agents::pane_id(&parent) || own.contains(&parent) {
            continue;
        }
        let age = row.get("age").and_then(Value::as_i64).unwrap_or(0);
        match panes.iter_mut().find(|(p, _)| *p == parent) {
            Some((_, oldest)) => *oldest = (*oldest).max(age),
            None => panes.push((parent, age)),
        }
    }
    panes.sort_by_key(|(_, age)| std::cmp::Reverse(*age));
    panes
        .iter()
        .enumerate()
        .map(|(n, (p, _))| Target::main(p, n))
        .collect()
}

/// The board with its live sections only (running agents, those that need recovery, and ended ones with unread mail)
/// and no ended count: what the members draw when main rows leave no room for the ended fold; the caller puts back the
/// header with the whole board's counts.
fn live_only(board: Option<&Map<String, Value>>) -> Option<Map<String, Value>> {
    let board = board?;
    let rows = board.get("agents").and_then(Value::as_array)?;
    let mut trimmed = board.clone();
    trimmed.insert("endedTotal".into(), 0.into());
    trimmed.insert(
        "agents".into(),
        Value::Array(
            rows.iter()
                .filter(|r| {
                    let row = r.as_object();
                    clean(r.get("state")) == "running"
                        || row.is_some_and(|r| recovery_word(r).is_some() || has_mail(r))
                })
                .cloned()
                .collect(),
        ),
    );
    Some(trimmed)
}

/// A main row: the glyph in cyan, then its name in bold.
fn main_row(target: &Target, cols: usize, look: Look) -> Row {
    let glyph = if look.ascii { "$" } else { MAIN };
    margin(cut_row(
        vec![
            span(glyph, hued(Color::Cyan)),
            span(" ", PLAIN),
            span(target.name.clone(), BOLD),
        ],
        cols,
    ))
}

/// The board's kept shells as targets, in its order.
pub(crate) fn kept_targets(board: Option<&Map<String, Value>>) -> Vec<Target> {
    board
        .and_then(|b| b.get("kept"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_object)
                .map(Target::kept)
                .collect()
        })
        .unwrap_or_default()
}

/// Ambiguity 15, v6f call 11: `shells` in `room` rows. All fit: a dim heading `kept shells · N`, then one row each.
/// Too many: a window of `room - 1` shells holding `selected` (else the first), then `+M more kept`; one row: the
/// window alone. The shells left out stay in the keys' walk (`walk_of`), so each is reachable.
fn lay_kept(
    shells: &[Target],
    cols: usize,
    look: Look,
    room: usize,
    selected: Option<&str>,
) -> Vec<(Row, Option<Target>)> {
    if shells.is_empty() || room == 0 {
        return Vec::new();
    }
    let line = |text: String| margin(vec![span(paint::cut(&text, cols), DIM)]);
    let shell = |t: &Target| {
        let row = line(format!("kept shell {}", own_glyphs(&t.name)));
        (row, Some(t.clone()))
    };
    if shells.len() < room {
        let dot = if look.ascii { "-" } else { "\u{b7}" };
        let mut rows = vec![(line(format!("kept shells {dot} {}", shells.len())), None)];
        rows.extend(shells.iter().map(shell));
        return rows;
    }
    let shown = if room == 1 { 1 } else { room - 1 };
    let at = selected
        .and_then(|s| shells.iter().position(|t| t.id == s))
        .unwrap_or(0);
    let start = at.saturating_sub(shown - 1).min(shells.len() - shown);
    let mut rows: Vec<(Row, Option<Target>)> =
        shells[start..start + shown].iter().map(shell).collect();
    if shown < shells.len() && room > 1 {
        rows.push((line(format!("+{} more kept", shells.len() - shown)), None));
    }
    rows
}

/// The kept rows after the header, or first when the frame has none (a team's `last_resort` starts with an agent row,
/// checklist 13); member rows shift by as many, so no two targets share a row.
fn fold_kept(laid: &mut Laid, kept: Vec<(Row, Option<Target>)>) {
    if kept.is_empty() {
        return;
    }
    let headed = !laid.rows.is_empty() && !laid.agents.iter().any(|(_, at)| *at == 0);
    let first = usize::from(headed);
    let n = kept.len();
    for at in
        (laid.agents.iter_mut().map(|(_, at)| at)).chain(laid.more.iter_mut().map(|(_, at)| at))
    {
        *at += n;
    }
    let mut targets = Vec::new();
    for (i, (row, target)) in kept.into_iter().enumerate() {
        laid.rows.insert(first + i, row);
        if let Some(t) = target {
            targets.push((t, first + i));
        }
    }
    targets.append(&mut laid.agents);
    laid.agents = targets;
}

/// Whether `target` is a member's row: not a kept shell, nor the ended fold line. Peek and `n`/`N` act on these only.
pub(crate) fn member_row(target: &Target) -> bool {
    target.life != pick::KEPT && target.life != pick::FOLD
}

/// Whether Enter on `selected` (an id of the keys' walk) resumes it: an ended agent, on every bar, a team's folded rows
/// included (`enter_action`). The hint row says `⏎ resume` then.
fn enter_resumes(walk: &[Target], selected: Option<&str>) -> bool {
    selected
        .and_then(|id| walk.iter().find(|t| t.id == id))
        .is_some_and(|t| enter_action(t) == EnterAction::Resume)
}

/// The walk without kept and main rows: peek and herdr's next and previous act on members only.
fn members(targets: &[Target]) -> Vec<Target> {
    (targets.iter())
        .filter(|t| member_row(t) && t.life != pick::MAIN)
        .cloned()
        .collect()
}

/// Audit 6-10 F1, v6f call 11: the one walk of the keys, for `pick.follow` and `Shown.targets` alike: every kept shell
/// of the board (drawn or behind `+N more kept`), then the team's whole tree (team) or the drawn members (any other bar).
pub(crate) fn walk_of(board: Option<&Map<String, Value>>, laid: &Laid, team: bool) -> Vec<Target> {
    let kept = kept_targets(board).into_iter();
    match board.filter(|_| team) {
        Some(b) => kept.chain(tree::walk(b)).collect(),
        // The main rows are drawn first but walked last, so `j` from nothing still starts at the first agent.
        None => {
            let drawn = laid.agents.iter().map(|(t, _)| t.clone());
            // The fold line is walked (keys open it), never clicked.
            let (mains, members): (Vec<_>, Vec<_>) = drawn
                .filter(|t| t.life != pick::KEPT)
                .partition(|t| t.life == pick::MAIN);
            kept.chain(members).chain(mains).collect()
        }
    }
}

/// `y`: `kept::close` decides (the re-read, the team, the kill); a refusal is shown.
/// restore-6c Task 11 (10-13 F9): with the row's own team, so a predecessor's shell closes; a row without one
/// refuses at `close`'s label check (fail-closed).
fn close_kept(ctx: &Ctx, target: &Target, pick: &mut Pick) {
    let closed = crate::hc::connect(ctx)
        .and_then(|core| crate::agents::kept::close(&core, &target.team, &target.id));
    if closed.is_err() {
        pick.close_failed(&target.name);
    }
}

/// The members' frame: a team board draws as a tree; any other board in sections, its ended fold open with
/// `focus.ended_open`. Also how many of the notes it kept.
fn lay_members(
    board: Option<&Map<String, Value>>,
    notes: &[(&str, Style)],
    size: Size,
    look: Look,
    focus: tree::Focus,
) -> (Laid, usize) {
    // A team bar at any width, under 12 columns included, so it can still ask to be widened (A2).
    if let Some(team) = board.filter(|b| tree::is_team(Some(b))) {
        return tree::lay_team(team, notes, size, look, focus);
    }
    if size.width < 12 {
        let max = size.width.saturating_sub(1).max(1);
        let laid = Laid {
            rows: vec![cut_row(vec![span("mux-agents", BOLD)], max)],
            ..Laid::default()
        };
        return (laid, 0);
    }
    let cols = size.width - 2;
    let parts = parts(board, notes, cols, look, focus.ended_open);
    let mut shape = Shape {
        gaps: 0,
        sections: !parts.live.is_empty(),
        doing: parts.live.iter().map(|live| live.doing.is_some()).collect(),
        shown: vec![true; parts.live.len()],
        live_more: true,
        open: parts.open.len(),
        // `+N older` is the open fold's last line; a closed fold is its line alone.
        older: focus.ended_open,
        fold: true,
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
            laid.more.retain(|(_, row)| *row < kept);
            return (laid, shape.notes);
        }
    }
}

/// What the bar draws in its last rows: the key help (`?`), else the hint row, whose Enter item says what Enter on the
/// selection does.
#[derive(Debug, Default, Clone, Copy)]
struct Dress {
    help: bool,
    enter: EnterHint,
}

/// The hint row's Enter item: `⏎ open`; `⏎ resume` on an ended row; `resuming…` alone while that row's resume runs,
/// when Enter starts nothing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum EnterHint {
    #[default]
    Open,
    Resume,
    Resuming,
}

/// The bar's last rows (after `lay_focus`, `mark` and `shade`): with `dress.help` the key help; otherwise the hint
/// row, pinned to the pane's last row with a blank row above it, only when the frame leaves it room, so it costs no
/// other row. Neither while a question holds the last line, nor under 12 columns. Both name `keys`' keys.
fn dress(
    laid: &mut Laid,
    notes: &[(&str, Style)],
    size: Size,
    look: Look,
    dress: Dress,
    keys: &Keymap,
) {
    if size.width < 12 || notes.iter().any(|(note, _)| pick::asking(note)) {
        return;
    }
    let cols = size.width - 2;
    // A dumb terminal scrolls each frame, so it gets no padding to the pane's last row.
    let pin = |laid: &mut Laid, below: usize| {
        let to = if look.dumb {
            laid.rows.len() + 1
        } else {
            size.height.saturating_sub(below)
        };
        laid.rows.resize(to.max(laid.rows.len()), Row::new());
    };
    if dress.help {
        let help = help_rows(cols, size.width >= MIN_WIDTH as usize, keys);
        // As many of the frame's rows as leave a blank row and the help; on a short pane the help alone, cut.
        let keep = size
            .height
            .saturating_sub(help.len() + 1)
            .min(laid.rows.len());
        laid.rows.truncate(keep);
        laid.agents.retain(|(_, at)| *at < keep);
        laid.more.retain(|(_, at)| *at < keep);
        if keep > 0 {
            pin(laid, help.len());
        }
        laid.rows.extend(help);
        laid.rows.truncate(size.height);
        return;
    }
    if laid.rows.len() + 2 > size.height {
        return;
    }
    pin(laid, 1);
    laid.rows.push(hint_row(cols, look, dress.enter, keys));
}

/// The hint row: `⏎ open  p peek  m mail  ? keys` (`enter` in ASCII; `⏎ resume` on a selected ended row, `resuming…`
/// while its resume runs), each key as `keys` has it, plain, and its word dim. Narrow, `m mail` goes first, then
/// `p peek`; `? keys` always stays. An action without a key (another took it) is left out.
fn hint_row(cols: usize, look: Look, enter: EnterHint, keys: &Keymap) -> Row {
    let key = Some(if look.ascii { "enter" } else { "\u{23ce}" }.to_string());
    let (key, word) = match enter {
        EnterHint::Open => (key, "open"),
        EnterHint::Resume => (key, "resume"),
        EnterHint::Resuming if look.ascii => (None, "resuming..."),
        EnterHint::Resuming => (None, "resuming\u{2026}"),
    };
    let items = [
        (key, word),
        (keys.label(Event::Peek), "peek"),
        (keys.label(Event::Mail), "mail"),
        (keys.label(Event::Help), "keys"),
    ];
    let build = |skip: &[usize]| -> Row {
        let mut row = Vec::new();
        let kept = (items.iter().enumerate()).filter(|(at, _)| !skip.contains(at));
        for (at, (key, word)) in kept {
            let parts = match key {
                Some(key) => vec![span(key.clone(), PLAIN), span(format!(" {word}"), DIM)],
                // Enter's item while the selection's resume runs: its word alone.
                None if at == 0 => vec![span(*word, DIM)],
                None => continue,
            };
            if !row.is_empty() {
                row.push(span("  ", PLAIN));
            }
            row.extend(parts);
        }
        row
    };
    let row = [&[][..], &[2], &[1, 2]]
        .into_iter()
        .map(build)
        .find(|row| row_width(row) <= cols)
        .unwrap_or_else(|| build(&[1, 2]));
    margin(cut_row(row, cols))
}

/// The keys `?` lists, in order: each row's keys (Enter, Tab and Esc fixed, the rest as `@mux-agents-keys` left them)
/// and its action.
const KEYS: [(&[Event], &str); 14] = [
    (&[Event::Enter], "open"),
    (&[Event::Down, Event::Up], "move"),
    (&[Event::First, Event::Last], "first/last"),
    (&[Event::Next, Event::Prev], "next/prev"),
    (&[Event::Peek], "peek"),
    (&[Event::Output], "output"),
    (&[Event::Mail], "mail"),
    (&[Event::Notes], "notes"),
    (&[Event::Placement], "placement"),
    (&[Event::Ack], "mark seen"),
    (&[Event::Tab], "fold ended"),
    (&[Event::Close], "close bar"),
    (&[Event::Esc], "clear"),
    (&[Event::Help], "keys"),
];

/// `events`' keys as `?` writes them, space-separated: the fixed ones by name, the others as `keys` has them; empty
/// when none has a key.
fn key_names(events: &[Event], keys: &Keymap) -> String {
    (events.iter())
        .filter_map(|event| match event {
            Event::Enter => Some("Enter".to_string()),
            Event::Tab => Some("Tab".to_string()),
            Event::Esc => Some("Esc".to_string()),
            event => keys.label(*event),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The key help: `Keys` bold and `? or Esc closes` dim, then each key bold and its action dim, two to a row when
/// `wide` (40 columns and more), else one; then each `@mux-agents-keys` entry ignored, dim. Keys as `keys` has them;
/// a row whose keys all went to other actions is left out.
fn help_rows(cols: usize, wide: bool, keys: &Keymap) -> Vec<Row> {
    let close = match keys.label(Event::Help) {
        Some(key) => format!("{key} or Esc closes"),
        None => "Esc closes".to_string(),
    };
    let mut title = vec![span("Keys", BOLD)];
    if 4 + 2 + close.len() <= cols {
        title.push(span(" ".repeat(cols - 4 - close.len()), PLAIN));
        title.push(span(close, DIM));
    }
    let listed: Vec<(String, &str)> = (KEYS.iter())
        .map(|(events, action)| (key_names(events, keys), *action))
        .filter(|(key, _)| !key.is_empty())
        .collect();
    let key_w = listed.iter().map(|(key, _)| key.len()).max().unwrap_or(0);
    let action_w = listed.iter().map(|(_, act)| act.len()).max().unwrap_or(0);
    let per = if wide && 2 * (key_w + 1 + action_w) + 2 <= cols {
        2
    } else {
        1
    };
    let mut rows = vec![margin(cut_row(title, cols))];
    for pair in listed.chunks(per) {
        let mut row = Vec::new();
        for (at, (key, action)) in pair.iter().enumerate() {
            row.push(span(format!("{key:<key_w$}"), BOLD));
            let action = if at + 1 < pair.len() {
                format!(" {action:<action_w$}  ")
            } else {
                format!(" {action}")
            };
            row.push(span(action, DIM));
        }
        rows.push(margin(cut_row(row, cols)));
    }
    for name in keys.ignored() {
        let text = format!("ignored: {}", clean(Some(&Value::from(name.as_str()))));
        rows.push(margin(cut_row(vec![span(text, DIM)], cols)));
    }
    rows
}

/// The band on the agent (or main row) whose pane the session shows: its whole block (name line and doing line) on a
/// soft background, `BAND`, across the bar's `width`, so the agent reads as the one on screen without a glyph. Only with color; nothing when
/// the shown pane is no drawn row's. The text is unchanged, so clicks hit it as before.
pub(crate) fn shade(laid: &mut Laid, shown: Option<&str>, width: usize, look: Look) {
    let Some(pane) = shown.filter(|p| !p.is_empty() && look.color && !look.ascii) else {
        return;
    };
    let Some(target) = laid
        .agents
        .iter()
        .find(|(t, _)| t.pane == pane)
        .map(|(t, _)| t.clone())
    else {
        return;
    };
    band(laid, &target, width);
}

/// The band on `target`'s whole block: its name line and the rows it takes under it (its doing line), each across
/// `width`.
fn band(laid: &mut Laid, target: &Target, width: usize) {
    let rows: Vec<usize> = (laid.agents.iter())
        .filter(|(t, _)| t.id == target.id && t.pane == target.pane)
        .map(|(_, at)| *at)
        .chain(
            laid.more
                .iter()
                .filter(|(id, _)| *id == target.id && !id.is_empty())
                .map(|(_, at)| *at),
        )
        .collect();
    for at in rows {
        let row = &mut laid.rows[at];
        let pad = width.saturating_sub(row_width(row));
        if pad > 0 {
            row.push(span(" ".repeat(pad), PLAIN));
        }
        for (_, style) in row.iter_mut() {
            style.bg = Some(BAND);
        }
    }
}

/// Marks member `id`'s drawn row as selected. With color the band is the one mark: it leaves the shown row for this
/// one, across `width`, so the bar never shows two marks. Without a band to draw (no color, ASCII) column 0 becomes
/// `>`, so the keys still show what Enter opens. Nothing when `id` is not drawn.
fn mark(laid: &mut Laid, id: &str, width: usize, look: Look) {
    let Some(&(_, at)) = laid.agents.iter().find(|(target, _)| target.id == id) else {
        return;
    };
    if look.color && !look.ascii {
        for (_, style) in laid.rows.iter_mut().flatten() {
            style.bg = None;
        }
        let target = laid
            .agents
            .iter()
            .find(|(t, _)| t.id == id)
            .map(|(t, _)| t.clone());
        if let Some(target) = target {
            band(laid, &target, width);
        }
        return;
    }
    laid.rows[at][0] = span(">", PLAIN);
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

/// The board as the bar draws it with `shown` on screen and `selected` picked (the band on the selection when there is
/// one, else on the shown row), as
/// text rows; for screenshots of the real frame.
pub fn format_focus(
    board: Option<&Map<String, Value>>,
    size: Size,
    look: Look,
    shown: Option<&str>,
    selected: Option<&str>,
) -> Vec<String> {
    let focus = tree::Focus {
        shown,
        selected,
        ended_open: false,
    };
    let mut laid = lay_hinted(board, &[], size, look, focus);
    if let Some(id) = selected {
        mark(&mut laid, id, size.width, look);
    }
    let walk = walk_of(board, &laid, tree::is_team(board));
    let resume = enter_resumes(&walk, selected);
    dress(
        &mut laid,
        &[],
        size,
        look,
        Dress {
            help: false,
            enter: if resume {
                EnterHint::Resume
            } else {
                EnterHint::Open
            },
        },
        &Keymap::default(),
    );
    laid.rows
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
/// `input` is owned here, so it is dropped, and its terminal restored, on every return (once the jobs finished) and
/// every unwind.
pub fn run_watch(
    ctx: &Ctx,
    caller: &Caller,
    origin: Origin,
    mut input: Box<dyn Input>,
    write: &mut dyn FnMut(&str) -> io::Result<()>,
) -> &'static str {
    let mut panel = Panel::default();
    let ended = watch_loop(ctx, caller, origin, &mut input, write, &mut panel);
    // Every way out waits for the work running off the loop first (`Jobs::finish`): another window's bar between its
    // split and its mark, or a resume between its launch and its record, is never cut when this process exits. The
    // input is kept until then, so the terminal stays as the loop had it: a second Ctrl-C while this waits is a byte
    // no one reads, not a signal that ends the process mid-open.
    let finished = panel.jobs.finish(ctx, caller, JOBS_WAIT);
    drop(input);
    if finished {
        return ended;
    }
    // Reported in the end reason, never waited for for ever. Leaked once: the watch's process ends right after.
    Box::leak(format!("{ended} (a worker did not finish)").into_boxed_str())
}

/// How long a watch that ends waits for the work running off its loop: the sidebar lock's own wait, which an open of
/// another window's bar may spend, and as long again for the work it then does under the lock.
const JOBS_WAIT: Duration = Duration::from_secs(2 * crate::state::lock::LOCK_WAIT.as_secs());

/// `run_watch`'s loop, its actions running off the loop in `panel`'s jobs.
fn watch_loop(
    ctx: &Ctx,
    caller: &Caller,
    origin: Origin,
    input: &mut Box<dyn Input>,
    write: &mut dyn FnMut(&str) -> io::Result<()>,
    panel: &mut Panel,
) -> &'static str {
    let own = format!("{}|", caller.pane);
    // watch draws only to a terminal (watch_on refuses otherwise).
    let look = Look::from_env(&ctx.env, true);
    // `@mux-agents-keys`, read once: a tick never reads it (a changed value applies to the next bar).
    let keymap = read_keys(ctx, caller);
    let mut keys = Decoder::new(keymap.clone());
    let mut keeper = Keeper::default();
    let mut last: Option<Map<String, Value>> = None;
    let mut size = FALLBACK;
    let mut unknown = 0;
    let mut pick = Pick::default();
    let mut viewing = Viewing::default();
    // The pane whose row is banded: the last pane on screen that is not the bar; and whether the bar was on screen.
    let mut banded: Option<String> = None;
    let mut in_bar = false;
    let mut alerts = Alerts::default();
    // Whether this bar's window has held a member view (kept in memory only): a bar leaves only once it held one.
    // shortcut: the window is the one resolved when the watch started; a bar moved to another window by hand stops
    // proving itself there (own_record) and only draws.
    let mut held = false;
    // reentry-6a E4.5: a status window never held a view, so its bar never leaves for want of one (read once).
    let mut status: Option<bool> = None;
    // When the watch started or was last proven, and whether its pane was read as not marked for it (run by hand: a
    // mark is only ever set by the open that splits the pane, and a watch that split started is never run by hand),
    // so the startup limit is checked no more.
    let mut proven_at = ctx.clock.now();
    let mut by_hand = false;
    // The other view windows whose ensure failed, and when: each is tried again once REARM has passed.
    let mut failed_windows: Vec<(String, SystemTime)> = Vec::new();
    // Ticks since the last walk over the other view windows: the first tick walks.
    let mut since_walk = WALK_EVERY;
    // News found while the sidebar lock was held elsewhere, told on a later tick.
    let mut pending_news: Vec<News> = Vec::new();
    let pace = current_pace();
    // The last frame written and when (`Shown::written`): an unchanged frame is not written again.
    let mut last_frame: Option<(String, Size)> = None;
    let mut last_written = SystemTime::UNIX_EPOCH;
    let mut last_changed = SystemTime::UNIX_EPOCH;
    let root = crate::agents::server_dir(ctx, caller);
    // Peek files of popups that never opened, from an earlier bar (review).
    sweep_peeks(ctx, &crate::agents::server_dir(ctx, caller));
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
        let recorded = live.is_some() && own_record(ctx, caller);
        if recorded {
            proven_at = ctx.clock.now();
        }
        // a bar cleans up after itself, decided each tick from what it reads now: not proven for STARTUP, it
        // decides under the session lock (`self_exit`).
        if live.is_some()
            && !recorded
            && !by_hand
            && ctx
                .clock
                .now()
                .duration_since(proven_at)
                .unwrap_or_default()
                >= STARTUP
        {
            match self_exit(ctx, caller, origin) {
                SelfExit::Left => return "unproven",
                SelfExit::ByHand => by_hand = true,
                SelfExit::Kept => proven_at = ctx.clock.now(),
                SelfExit::Unread => {}
            }
        }
        // the window held a view once a verb marked it viewed (`ensure`), or once this bar sees a live member
        // view in it, recorded or not (the first tick may run before open records the bar), whichever comes first.
        if live.is_some() && !held {
            held = native(
                ctx,
                &["show-options", "-wqv", "-t", &caller.window, VIEWED],
                true,
            )
            .is_ok_and(|read| read.code == 0 && read.stdout.trim_end_matches(['\r', '\n']) == "1");
        }
        // Only the recorded bar acts: once its window held a view and has none now, it first gives the other view
        // windows their bar (no tick of it follows), then leaves. An unread view list changes nothing.
        let read = if recorded || (live.is_some() && !held) {
            view_windows(ctx, caller)
        } else {
            None
        };
        if let Some(windows) = read {
            let here = windows.contains(&caller.window);
            held |= here;
            let lost = recorded
                && held
                && !here
                && !*status.get_or_insert_with(|| is_status(ctx, &caller.window));
            since_walk = since_walk.saturating_add(1);
            if recorded && (lost || since_walk >= WALK_EVERY) {
                since_walk = 0;
                // Every other view window without a proven bar gets its own (a view moved by break-pane), checked
                // every WALK_EVERY ticks so a tick stays cheap. One whose ensure failed waits a minute before it is
                // tried again, so a failing open never splits every tick; a leaving bar tries each one now, since it
                // is the last chance this watch has.
                let now = ctx.clock.now();
                failed_windows
                    .retain(|(_, at)| now.duration_since(*at).unwrap_or_default() < REARM);
                for window in windows.iter().filter(|w| **w != caller.window) {
                    if (!lost && failed_windows.iter().any(|(w, _)| w == window))
                        || matches!(prove(ctx, window), Ok(Some(_)))
                    {
                        continue;
                    }
                    // The tick never opens a bar itself: it only decides which window needs one, and a worker opens it
                    // (`Jobs::ensure`, waiting on the sidebar lock as long as another holds it), its answer drained on
                    // a later pass. A leaving bar has no later pass, so it opens them itself.
                    if !lost {
                        panel.jobs.ensure(ctx, caller, window);
                    } else if ensure(ctx, caller, window).is_err() {
                        failed_windows.push((window.clone(), now));
                    }
                }
            }
            if lost && leave(ctx, caller) {
                return "view-closed";
            }
        }
        if recorded && keeper.tick(ctx, &caller.pane) {
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
        // The poll's baseline, taken before the board is read: a write that lands during the read is newer, so the
        // next poll draws it instead of taking it as drawn.
        let polled_stamp = pace.poll.and_then(|_| stamp(&root));
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
        // The band follows the pane on screen. With the bar itself in front (the person switched to this tab, or works
        // in the bar) it is this window's other pane, so a tab switch moves the band to that tab's agent; a window
        // read that names no other pane keeps the band where it was (detached: none).
        match looked.as_ref() {
            Some(Some(pane)) if *pane == caller.pane => {
                if let Some(other) = window_other(ctx, caller) {
                    banded = Some(other);
                }
            }
            Some(seen) => banded.clone_from(seen),
            None => {}
        }
        // The person left the bar (it was the pane on screen, now another is): a selection made in it no longer
        // points anywhere they chose, so it clears; a pending question stays. Keys sent to a bar that was never on
        // screen keep their selection; an unread probe or a detached session changes nothing.
        if let Some(Some(pane)) = looked.as_ref() {
            let here = *pane == caller.pane;
            if in_bar && !here {
                pick.left();
            }
            in_bar = here;
        }
        // An agent that starts needing the person, or finishes, while they look elsewhere: one message, as herdr's
        // toast. Only when both the board and the shown pane were read now: a tick that knows neither keeps the news
        // for the next.
        if looked.is_some() && !failed {
            pending_news.extend(alerts.tick(last.as_ref(), viewing.pane.as_deref()));
            // News that met the sidebar lock held elsewhere waits for a later tick, never this one.
            if !pending_news.is_empty() && alert(ctx, caller, &pending_news) {
                pending_news.clear();
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
        let team = tree::is_team(last.as_ref());
        pick.set_wrap(team);
        let focus = tree::Focus {
            shown: banded.as_deref(),
            selected: None,
            ended_open: panel.ended_open,
        };
        // A row whose resume runs is drawn `resuming…` from memory until the board catches up.
        let marked = panel.jobs.mark(last.as_ref());
        let drawn_board = marked.as_ref().or(last.as_ref());
        let bare = lay_hinted(drawn_board, &notes, size, look, focus);
        // The board's kept shells first, then a team's whole tree, folded agents included (B10), or the rows any other
        // bar drew: one walk followed here and acted on below (audit 6-10 F1).
        let walk = walk_of(drawn_board, &bare, team);
        pick.follow(&walk);
        let selected = pick.selected().map(str::to_string);
        let mut laid = match selected.as_deref() {
            Some(id) => {
                let focus = tree::Focus {
                    selected: Some(id),
                    ..focus
                };
                let mut laid = lay_hinted(drawn_board, &notes, size, look, focus);
                mark(&mut laid, id, size.width, look);
                laid
            }
            None => bare,
        };
        let dressed = Dress {
            help: panel.help,
            enter: match selected.as_deref() {
                Some(id) if panel.jobs.pending(id) => EnterHint::Resuming,
                id if enter_resumes(&walk, id) => EnterHint::Resume,
                _ => EnterHint::Open,
            },
        };
        dress(&mut laid, &notes, size, look, dressed, &keymap);
        // With a poll the bar ticks often: a frame already on screen at this size is written again only every
        // REWRITE, so the screen does not churn, and a rewrite of the same frame is no change: PRESS_GAP drops a
        // click only after a frame that changed (`last_changed`), never after one that moved nothing under it.
        let text = frame(&laid.rows, size, look);
        let now = ctx.clock.now();
        let same = last_frame
            .as_ref()
            .is_some_and(|(rows, at)| *at == size && *rows == text);
        let due = now.duration_since(last_written).unwrap_or_default() >= REWRITE;
        if pace.poll.is_none() || !same || due {
            if write(&text).is_err() {
                return "output-closed";
            }
            last_written = ctx.clock.now();
            if pace.poll.is_none() || !same {
                last_changed = last_written;
            }
            last_frame = Some((text, size));
        }
        // `close bar? y/n` is on screen from now (it is drawn from 12 columns): `BAR_GAP` runs from here.
        if size.width >= 12 {
            pick.question_shown(ctx.clock.now());
        }
        // The next tick is a tick after this one ends (after a slow write, from its end).
        let ticked = ctx.clock.now();
        let mut shown = Shown {
            written: last_changed,
            targets: walk,
            drawn: drawn(&laid),
            width: size.width,
            // The agent on screen as the band shows it: with the bar itself in front, its window's other pane.
            looking: banded.clone(),
            team,
            done: done_spells(last.as_ref()),
        };
        // What the next polls compare against: the pane on screen as this tick drew it (the stamp is from before the
        // read).
        let polled_pane = looked.clone().flatten();
        let mut polled_at = ctx.clock.now();
        loop {
            // An action that answered off the loop (a resume) is applied, and the bar draws again at once.
            if settle_jobs(ctx, caller, &mut pick, &mut shown, panel) {
                break;
            }
            // Another window's bar the worker could not open waits `REARM` before it is tried again.
            for (window, answer) in panel.jobs.drain_windows(ctx, caller) {
                if answer.is_err() {
                    failed_windows.push((window, ctx.clock.now()));
                }
            }
            let since = ctx.clock.now().duration_since(ticked).unwrap_or(pace.tick);
            let left = pace.tick.saturating_sub(since);
            if left.is_zero() {
                pick.ticked();
                break;
            }
            // A poll is due: another pane on screen or a state change ends the wait, and the next tick draws it now.
            let mut changed = false;
            if let Some(poll) = pace.poll
                && ctx.clock.now().duration_since(polled_at).unwrap_or(poll) >= poll
            {
                polled_at = ctx.clock.now();
                let pane = shown_pane(ctx, caller);
                let moved = pane.is_some_and(|p| p != polled_pane);
                changed = moved || stamp(&root) != polled_stamp;
            }
            let mut wait = if keys.lone_esc() {
                left.min(ESC_WAIT)
            } else {
                left
            };
            if let Some(poll) = pace.poll {
                let next =
                    poll.saturating_sub(ctx.clock.now().duration_since(polled_at).unwrap_or(poll));
                wait = wait.min(next.max(Duration::from_millis(1)));
            }
            // With a change found, only input already waiting is read, against the frame it was aimed at: read after
            // the redraw, a press would be dropped as aimed at the frame before it. A click from another pane is such
            // a change, since the multiplexer's mouse binding selects the bar's pane before it passes the press on.
            if changed {
                wait = Duration::ZERO;
            }
            let reads = match input.wait(&*ctx.clock, wait) {
                Waited::Bytes(bytes) => keys.push(&bytes, ctx.clock.now()),
                // A lone ESC keeps its wait for the rest of its sequence: the redraw's wait gives it.
                Waited::Timeout if changed => Vec::new(),
                Waited::Timeout => keys.idle(),
                // The old input is dropped here, which restores its terminal.
                Waited::Closed => {
                    *input = Box::new(NoInput);
                    Vec::new()
                }
            };
            let mut redraw = changed;
            for read in reads {
                if read.event == Event::Interrupt {
                    return "interrupted";
                }
                redraw |= act(ctx, caller, look, read, &mut pick, &mut shown, panel);
                if panel.closed {
                    return "closed";
                }
            }
            if redraw {
                break;
            }
        }
    }
}

/// `@mux-agents-keys` as the bar's pane reads it (pane, window, session, then `set -g`), parsed; the keys as shipped
/// when it is unset or the read fails.
fn read_keys(ctx: &Ctx, caller: &Caller) -> Keymap {
    native(
        ctx,
        &["display-message", "-p", "-t", &caller.pane, KEYS_FORMAT],
        true,
    )
    .ok()
    .filter(|read| read.code == 0)
    .map_or_else(Keymap::default, |read| {
        Keymap::parse(read.stdout.trim_end_matches(['\r', '\n']))
    })
}

const KEYS_FORMAT: &str = "#{@mux-agents-keys}";

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

/// The bar's window's other pane, the one active last (else the first): what a tab shows beside its bar. None when
/// the read fails or the bar is alone.
fn window_other(ctx: &Ctx, caller: &Caller) -> Option<String> {
    let read = native(
        ctx,
        &["list-panes", "-t", &caller.window, "-F", WINDOW_PANES],
        true,
    )
    .ok()?;
    if read.code != 0 {
        return None;
    }
    let rows: Vec<(&str, bool)> = (read.stdout.lines())
        .filter_map(|line| {
            let mut f = line.trim_end_matches('\r').split('|');
            let (pane, _active, last) = (f.next()?, f.next()?, f.next()?);
            (crate::agents::pane_id(pane) && pane != caller.pane).then_some((pane, last == "1"))
        })
        .collect();
    let (pane, _) = rows.iter().find(|(_, last)| *last).or(rows.first())?;
    Some(pane.to_string())
}

const WINDOW_PANES: &str = "#{pane_id}|#{pane_active}|#{pane_last}";

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

/// The live agents `board` shows done and unseen (the board says `done` only until the person has seen that spell),
/// each as its target id (`Target::of`: the id, else the name) and the spell's `changedAt`. From the board the tick
/// already read: no call.
fn done_spells(board: Option<&Map<String, Value>>) -> Vec<(String, String)> {
    let rows = board
        .and_then(|b| b.get("agents"))
        .and_then(Value::as_array);
    (rows.into_iter().flatten().filter_map(Value::as_object))
        .filter(|row| clean(row.get("state")) == "running" && clean(row.get("activity")) == "done")
        .filter_map(|row| {
            let changed = clean(row.get("changedAt"));
            (!changed.is_empty()).then(|| (Target::of(row).id, changed))
        })
        .collect()
}

/// Each live agent's last activity, by pane, to tell when one starts needing the person or finishes.
/// One agent's change worth a message: its name, its activity (`blocked` or `done`) and the change's `changedAt`.
type News = (String, String, String);

#[derive(Debug, Default)]
struct Alerts {
    last: std::collections::HashMap<String, String>,
}

impl Alerts {
    /// One tick: the agents that turned blocked or done since the last tick, while their pane is not the one shown,
    /// as (name, activity, the change's `changedAt`). The first sight of an agent is no news; a board not read
    /// changes nothing.
    fn tick(&mut self, board: Option<&Map<String, Value>>, shown: Option<&str>) -> Vec<News> {
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
                news.push((
                    clean(row.get("name")),
                    activity.clone(),
                    clean(row.get("changedAt")),
                ));
            }
            now.insert(pane, activity);
        }
        self.last = now;
        news
    }
}

/// `@mux-agents-alerts`: `off` sends none, `blocked` only who needs the person, anything else (unset) both.
const ALERTS_OPTION: &str = "@mux-agents-alerts";
/// The session's changes already told (with a bar per window): `<name>=<changedAt>`, per agent the newest change
/// told, space-separated, so a bar that sees that change, or an older one, sends nothing.
const ALERTED: &str = "@mux-agents-alerted";

/// The tmux message for `news`, on the session's clients for 5 s, unless `@mux-agents-alerts` says otherwise. Names
/// are `[a-z0-9-]` and the words fixed, so the text holds no format. Best effort.
///
/// Once per change, monotonic (with a bar per window): under the session's sidebar lock, an agent's news is
/// sent only when its `changedAt` is strictly newer than the newest one told for it (`@mux-agents-alerted`, by any
/// bar of the session); an older time is never written back. The newer times are recorded as told (whatever
/// `@mux-agents-alerts` says, so turning it on later replays nothing), and only then sent. News whose time does not
/// parse, or a record that cannot be read or written, sends nothing: a change is told at most once.
///
/// The tick never waits on that lock (a resume's post-launch bar step holds it as long as its split takes): false when
/// another holds it, nothing sent, and the caller keeps `news` for a later tick; the record above keeps a retried
/// change from being told twice.
fn alert(ctx: &Ctx, caller: &Caller, news: &[News]) -> bool {
    let taken = crate::agents::try_lock(ctx, &scope::shown_lock("sidebar", caller), || {
        let read = native(
            ctx,
            &["show-options", "-qv", "-t", &caller.session, ALERTED],
            true,
        )?;
        if read.code != 0 {
            return Ok(());
        }
        let mut told: Vec<(String, String)> = read
            .stdout
            .split_whitespace()
            .filter_map(|entry| entry.split_once('='))
            .map(|(name, at)| (name.to_string(), at.to_string()))
            .collect();
        let mut fresh: Vec<&News> = Vec::new();
        for item in news {
            let (name, _, changed) = item;
            if !crate::agents::agent_name(name)
                || changed.contains(|c: char| c.is_whitespace() || c == '=')
            {
                continue;
            }
            let Some(at) = crate::util::parse_utc(changed) else {
                continue;
            };
            let newer = told
                .iter()
                .find(|(told_name, _)| told_name == name)
                .and_then(|(_, was)| crate::util::parse_utc(was))
                .is_none_or(|was| at > was);
            if !newer {
                continue;
            }
            told.retain(|(told_name, _)| told_name != name);
            told.push((name.clone(), changed.clone()));
            fresh.push(item);
        }
        if fresh.is_empty() {
            return Ok(());
        }
        let record: Vec<String> = told
            .iter()
            .map(|(name, at)| format!("{name}={at}"))
            .collect();
        let written = native(
            ctx,
            &[
                "set-option",
                "-t",
                &caller.session,
                ALERTED,
                &record.join(" "),
            ],
            true,
        )?;
        if written.code != 0 {
            return Ok(());
        }
        send_alert(ctx, caller, &fresh);
        Ok(())
    });
    !matches!(taken, Ok(None))
}

/// The message itself, as `@mux-agents-alerts` says: `off` none, `blocked` only who needs the person, else both.
fn send_alert(ctx: &Ctx, caller: &Caller, news: &[&News]) {
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
        .filter(|(_, activity, _)| match option.as_str() {
            "off" => false,
            "blocked" => activity == "blocked",
            _ => true,
        })
        .map(|(name, activity, _)| {
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

/// The bar's own state beside the selection, kept in memory only: the ended fold open (Tab), the key help shown (`?`),
/// the bar closed by `q` (the watch ends), and the actions running off the loop.
#[derive(Debug, Default)]
pub(crate) struct Panel {
    pub ended_open: bool,
    pub help: bool,
    pub closed: bool,
    pub jobs: jobs::Jobs,
}

/// The frame on screen: when it was written, the running agents it drew (top to bottom) and its width.
struct Shown {
    written: SystemTime,
    targets: Vec<Target>,
    // Each agent row's id, row and text, for hits.
    drawn: Vec<(String, usize, String)>,
    width: usize,
    // The pane the session showed at this frame (herdr's next / previous start from it).
    looking: Option<String>,
    // A team's bar: keys wrap, `n`/`N` select, ASCII takes clicks.
    team: bool,
    // The live agents done and unseen on this frame's board, as (target id, the done spell's `changedAt`): Space
    // marks the selected one seen.
    done: Vec<(String, String)>,
}

fn focus_target_hc(ctx: &Ctx, caller: &Caller, target: &Target) -> Res<Focused> {
    if target.life == pick::MAIN {
        return focus_main(ctx, target);
    }
    let core = crate::hc::connect(ctx)?;
    let drawn = (!target.pane.is_empty()).then_some(target.pane.as_str());
    match crate::agents::view::focus_member(ctx, Some(&core), caller, &target.id, drawn)? {
        crate::agents::view::Member::Focused(focused) => Ok(focused),
        _ => refuse(2, "target", "no unique committed member has that identity"),
    }
}
/// The main chat's pane, selected only while it is alive (the multiplexer never gives a gone pane's id to another
/// pane), then read back: that pane, active in the active window, not dead. Anything else refuses `target`.
fn focus_main(ctx: &Ctx, target: &Target) -> Res<Focused> {
    let pane = target.pane.as_str();
    if !crate::agents::pane_id(pane) {
        return refuse(2, "target", "the main pane is gone");
    }
    let select = crate::agents::registry::bound(
        pane,
        "#{==:#{pane_dead},0}".into(),
        format!("select-window -t {pane} ; select-pane -t {pane}"),
    );
    let select: Vec<&str> = select.iter().map(String::as_str).collect();
    let _ = native(ctx, &select, true);
    let probe = "#{pane_id}|#{window_active}|#{pane_active}|#{pane_dead}";
    let back = native(ctx, &["display-message", "-p", "-t", pane, probe], true);
    if !back.is_ok_and(|r| {
        r.code == 0 && r.stdout.trim_end_matches(['\r', '\n']) == format!("{pane}|1|1|0")
    }) {
        return refuse(2, "target", "the main pane is gone");
    }
    Ok(Focused {
        id: target.id.clone(),
        name: target.name.clone(),
        pane: pane.into(),
        window: String::new(),
        duplicates: 0,
    })
}

impl Shown {
    fn focused(&mut self, focused: Focused) {
        for target in &mut self.targets {
            if target.id == focused.id {
                target.pane = focused.pane.clone();
            }
        }
        self.looking = Some(focused.pane);
    }
}

/// Acts on one event read while `shown` is on screen; true when the bar must draw again at once. A left press reads
/// the bar, selects the row it hit and does what Enter on it does (`enter`; ASCII or stale presses do nothing outside a
/// team; a team bar takes ASCII clicks, B6); stale is read less than `PRESS_GAP` after the frame's write, or begun
/// before it; any left press dismisses the note and draws again. Keys go to `pick`. A failed focus shows its note.
/// Other buttons and releases do nothing.
fn act(
    ctx: &Ctx,
    caller: &Caller,
    look: Look,
    read: keys::Read,
    pick: &mut Pick,
    shown: &mut Shown,
    panel: &mut Panel,
) -> bool {
    // A pending `close? y/n` (views-6f ambiguity 15) or `close bar? y/n`: an event answers it, and only `y` closes;
    // nothing is focused, selected or clicked. A `y` too soon after `close bar?` showed leaves it asked.
    match pick.answer(&read) {
        Answer::None => {}
        Answer::Wait => return false,
        Answer::Cancel => return true,
        Answer::Kept(target) => {
            close_kept(ctx, &target, pick);
            return true;
        }
        Answer::Bar => {
            match close_bar(ctx, caller) {
                Ok(()) => panel.closed = true,
                Err(_) => pick.close_failed("side bar"),
            }
            return true;
        }
    }
    // The key help closes on `?` or Esc alone; any other key or press closes it and also does its own action.
    if panel.help && !matches!(read.event, Event::Release { .. }) {
        panel.help = false;
        if matches!(read.event, Event::Help | Event::Esc) {
            return true;
        }
        act(ctx, caller, look, read, pick, shown, panel);
        return true;
    }
    match read.event {
        Event::Help => {
            panel.help = true;
            true
        }
        Event::Close => {
            pick.ask_bar();
            true
        }
        Event::Notes => {
            show_notes(ctx, caller, pick);
            true
        }
        // Where `spawn` opens an agent that names no place: the next of tab, split h, split v, as `placement` sets it.
        Event::Placement => {
            match crate::agents::placement::cycle(ctx, caller) {
                Ok(next) => {
                    let place = match next {
                        "h" => "split h",
                        "v" => "split v",
                        _ => "tab",
                    };
                    pick.say(format!("opens in: {place}"));
                }
                Err(e) => pick.say(format!("placement refused: {}", e.message)),
            }
            true
        }
        Event::Tab => {
            toggle_fold(pick, shown, panel);
            true
        }
        Event::First | Event::Last => {
            let walk = &shown.targets;
            let rows: Vec<(Target, usize)> = shown
                .drawn
                .iter()
                .filter_map(|(id, at, _)| {
                    walk.iter().find(|t| t.id == *id).map(|t| (t.clone(), *at))
                })
                .collect();
            if let Some(at) = pick::edge(walk, &rows, read.event == Event::First) {
                pick.select(walk, at);
            }
            true
        }
        Event::Press { button: 0, y, .. } => {
            pick.pressed();
            let stale = read.first < shown.written + PRESS_GAP;
            if (look.ascii && !shown.team) || stale {
                return true;
            }
            // A click is Enter on the row it hit, selected first: after nothing to open or a failure, `p` and Enter act
            // on it, and a resume keeps it selected while it runs.
            if let Some(target) = click(ctx, caller, y, shown) {
                if let Some(at) = shown.targets.iter().position(|t| t.id == target.id) {
                    pick.select(&shown.targets, at);
                }
                enter(ctx, caller, target, pick, shown, panel);
            }
            true
        }
        Event::Press { .. }
        | Event::Release { .. }
        | Event::Interrupt
        | Event::Yes
        | Event::Other => false,
        // The selected row only, and only one the person chose (never a fallback, nor a row the selection fell to when
        // theirs left): done and unseen, its spell is marked seen as leaving its pane marks it (`Viewing::mark`), and
        // the board read at once moves it out of Inbox. Any other row: nothing.
        Event::Ack => {
            let Some(target) = pick.own(&shown.targets).cloned() else {
                return false;
            };
            // By the member's id, not its pane: a done member with no view has none to resolve.
            let spell = (shown.done.iter()).find(|(id, _)| *id == target.id);
            spell.is_some_and(|(id, changed)| {
                crate::agents::board::mark_seen_id(ctx, caller, id, changed).is_ok_and(|m| m)
            })
        }
        // A team's n/N select the next/previous agent with a live view, from the selection, else the shown agent (B10).
        // Kept rows are no agents (F9).
        Event::Next | Event::Prev if shown.team => {
            let walk = members(&shown.targets);
            let from = pick
                .chosen(&walk)
                .and_then(|t| walk.iter().position(|w| w.id == t.id))
                .or_else(|| {
                    shown
                        .looking
                        .as_deref()
                        .and_then(|p| walk.iter().position(|t| t.pane == p))
                });
            if let Some(at) = pick::step_live(&walk, from, read.event == Event::Next) {
                pick.select(&walk, at);
            }
            true
        }
        // herdr's next and previous agent, from the pane the session shows, in the bar's order: the running ones
        // (an ended row has no view to focus).
        Event::Next | Event::Prev => {
            let forward = read.event == Event::Next;
            let mut walk = members(&shown.targets);
            walk.retain(|t| t.life != "ended");
            if let Some(target) = pick::step(&walk, shown.looking.as_deref(), forward).cloned() {
                match focus_target_hc(ctx, caller, &target) {
                    Ok(focused) => shown.focused(focused),
                    Err(_) => pick.failed(&target.name),
                }
            }
            true
        }
        // A team's `p`: the selection, else the shown agent (B10); never a kept row.
        Event::Peek if shown.team => {
            let walk = members(&shown.targets);
            let target = pick.chosen(&walk).cloned().or_else(|| {
                shown
                    .looking
                    .as_deref()
                    .and_then(|p| walk.iter().find(|t| t.pane == p).cloned())
            });
            if let Some(target) = target {
                preview(ctx, caller, &target, pick);
            }
            true
        }
        // `subject`'s unread mail, read-only.
        Event::Mail => {
            if let Some(target) = subject(pick, shown) {
                show_mail(ctx, caller, &target, pick);
            }
            true
        }
        // `subject`'s retained output, live or ended.
        Event::Output => {
            if let Some(target) = subject(pick, shown) {
                match show_output(ctx, caller, &target) {
                    Opened::Shown => {}
                    Opened::Nothing => pick.nothing(&target.name),
                    Opened::Failed => pick.failed(&target.name),
                }
            }
            true
        }
        // herdr's peek of `subject`, over the window without switching to it; an ended agent shows its retained
        // output.
        Event::Peek => {
            match subject(pick, shown) {
                Some(target) if target.life == "ended" => preview(ctx, caller, &target, pick),
                Some(target) if peek(ctx, caller, &target).is_err() => pick.failed(&target.name),
                _ => {}
            }
            true
        }
        Event::Up | Event::Down | Event::Enter | Event::Esc => {
            // A kept shell the selection fell to (the person's row left): Enter takes it as theirs and asks nothing
            // yet, so `close? y/n` only ever names a shell they picked.
            if read.event == Event::Enter
                && pick.own(&shown.targets).is_none()
                && let Some(at) = (pick.chosen(&shown.targets))
                    .filter(|t| t.life == pick::KEPT)
                    .and_then(|t| shown.targets.iter().position(|w| w.id == t.id))
            {
                pick.select(&shown.targets, at);
                return true;
            }
            match pick.key(read.event, &shown.targets) {
                Act::Nothing => false,
                Act::Redraw => true,
                Act::Focus(target) => {
                    enter(ctx, caller, target, pick, shown, panel);
                    true
                }
            }
        }
    }
}

/// The agent `m`, `o` and the flat bar's `p` act on: the selected one, else the agent on screen (the pane the session
/// shows), else the first running one. Never a kept, main or fold row.
fn subject(pick: &Pick, shown: &Shown) -> Option<Target> {
    let walk = members(&shown.targets);
    let looked = shown.looking.as_deref().filter(|pane| !pane.is_empty());
    (pick.chosen(&walk))
        .or_else(|| looked.and_then(|pane| walk.iter().find(|t| t.pane == pane)))
        .or_else(|| walk.iter().find(|t| t.life == "running"))
        .cloned()
}

/// Tab, or Enter on the fold line: the ended fold opens or closes. Closing it with one of its rows selected moves the
/// selection to the fold line, so it never falls to a row the person did not choose.
fn toggle_fold(pick: &mut Pick, shown: &Shown, panel: &mut Panel) {
    panel.ended_open = !panel.ended_open;
    if panel.ended_open {
        return;
    }
    let row = |id: &str| {
        shown
            .drawn
            .iter()
            .find(|(d, ..)| d == id)
            .map(|(_, at, _)| *at)
    };
    let fold = shown.targets.iter().position(|t| t.life == pick::FOLD);
    // Only the fold's own rows are drawn below its line.
    if let (Some(fold), Some(chosen)) = (fold, pick.selected().and_then(row))
        && row(&shown.targets[fold].id).is_some_and(|at| at < chosen)
    {
        pick.select(&shown.targets, fold);
    }
}

/// Lines a peek shows: the screen and some history.
const PEEK_LINES: i32 = 200;

/// The state folder a peek file goes to; its path goes through the multiplexer's parser, its formats and a shell, so
/// plain characters only.
fn peek_folder(ctx: &Ctx, caller: &Caller) -> Res<std::path::PathBuf> {
    let folder = crate::agents::server_dir(ctx, caller);
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
    Ok(folder)
}

/// `text` for agent `name` in a private file of `folder`, paged in a popup over the session's client (see `peek`).
/// How old the bar's own peek file must be before it is swept (review, PM decision): a popup still open keeps a
/// younger one.
const PEEK_STALE: Duration = Duration::from_secs(600);

/// Deletes this bar's own peek files (`peek-*.txt` in `folder`) last written more than `PEEK_STALE` ago: a popup queued
/// with `run-shell -b -C` whose client went away never deletes its file. A younger file, one with a future time or one
/// whose time cannot be read stays. Best effort: never fails the bar.
fn sweep_peeks(ctx: &Ctx, folder: &std::path::Path) {
    let now = ctx.clock.now();
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("peek-") && name.ends_with(".txt")) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| now.duration_since(at).ok())
            .is_some_and(|age| age > PEEK_STALE);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn popup(ctx: &Ctx, caller: &Caller, folder: &std::path::Path, name: &str, text: &str) -> Res<()> {
    sweep_peeks(ctx, folder);
    let file = folder.join(format!(
        "peek-{}.txt",
        crate::util::guid()?.replace('-', "")
    ));
    // The name closes the text, where the pager opens: tmux 3.2 popups have no title.
    std::fs::write(&file, format!("{text}\n-- peek: {name} (q closes) --\n"))?;
    let Some(path) = file.to_str() else {
        let _ = std::fs::remove_file(&file);
        return refuse(1, "internal", "the peek file's path is not text");
    };
    let popup = popup_command(&caller.pane, path);
    if native(ctx, &["run-shell", "-b", "-C", &popup], true)
        .ok()
        .filter(|o| o.code == 0)
        .is_none()
    {
        let _ = std::fs::remove_file(&file);
        return refuse(1, "target", "the popup did not open");
    }
    Ok(())
}

/// herdr's peek: the agent's last lines, captured as `read <name> -Lines` captures them (only while its pane is its
/// own live launch), into a private file of the state folder, shown in a popup over the session's client that pages
/// it, deleting it as it starts. The popup is started in the background (`run-shell -b -C`), since display-popup
/// answers only once it closes and the bar must not wait; so a popup that never opens is not seen here, and its
/// file stays in the state folder until the bar sweeps its peek files older than `PEEK_STALE` (`sweep_peeks`).
fn peek(ctx: &Ctx, caller: &Caller, target: &Target) -> Res<()> {
    let folder = peek_folder(ctx, caller)?;
    let drawn = (!target.pane.is_empty()).then_some(target.pane.as_str());
    let (_, text) =
        crate::agents::message::capture_member(ctx, caller, &target.id, drawn, PEEK_LINES)?;
    popup(ctx, caller, &folder, &target.name, &text)
}

/// Lines of retained output an agent's output popup shows.
const OUTPUT_LINES: u32 = 200;

/// What opening a row came to (B6): shown (a view or the output), nothing to show, or a real failure.
enum Opened {
    Shown,
    Nothing,
    Failed,
}

/// Member `target`'s retained output (`hc read`) in the peek popup. Blank output, a member gone or a terminal hc
/// dropped is nothing to show; hc unreachable or a popup that did not open is a failure.
fn show_output(ctx: &Ctx, caller: &Caller, target: &Target) -> Opened {
    let Ok(folder) = peek_folder(ctx, caller) else {
        return Opened::Failed;
    };
    match crate::agents::message::retained(ctx, caller, &target.id, OUTPUT_LINES) {
        Ok(Some(text)) if popup(ctx, caller, &folder, &target.name, &text).is_ok() => Opened::Shown,
        Ok(None) => Opened::Nothing,
        Err(e) if e.check == "target" => Opened::Nothing,
        _ => Opened::Failed,
    }
}

/// `m`: what `target`'s ✉ badge counts, read-only, in the popup (`crate::agents::mail::badge_mail` acks nothing, so the
/// chat or leader still gets it).
fn show_mail(ctx: &Ctx, caller: &Caller, target: &Target, pick: &mut Pick) {
    let shown = peek_folder(ctx, caller).and_then(|folder| {
        let reports = crate::agents::mail::badge_mail(ctx, caller, &target.id)?;
        popup(
            ctx,
            caller,
            &folder,
            &format!("mail \u{b7} {}", target.name),
            &mail_text(&reports),
        )
    });
    if shown.is_err() {
        pick.failed(&target.name);
    }
}

/// The popup's text for `m`: each report as `from · at`, then its body, a blank line between; controls stripped.
fn mail_text(reports: &[crate::agents::mail::Report]) -> String {
    if reports.is_empty() {
        return "no unread mail\n".into();
    }
    let clean = |t: &str| -> String {
        t.chars()
            .filter(|c| *c == '\n' || !c.is_control())
            .collect()
    };
    reports
        .iter()
        .map(|r| {
            format!(
                "{} \u{b7} {}\n{}\n",
                clean(&r.from),
                clean(&r.at),
                clean(&r.body)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `i`: the team's notebook (the `notes` verb, in this process as the bar) in the popup, newest first. Outside a team,
/// or with nothing noted yet, the note line says `no notes`; any other refusal, or a popup that did not open, says
/// `notes failed`.
fn show_notes(ctx: &Ctx, caller: &Caller, pick: &mut Pick) {
    let request = Request {
        verb: "notes",
        values: Vec::new(),
        switches: Vec::new(),
        positional: Vec::new(),
        text: false,
        rest: Vec::new(),
    };
    let reply = match crate::agents::notebook::notes(ctx, &request) {
        Ok(reply) => reply,
        Err(e) if e.check == "team" => return pick.say("no notes".into()),
        Err(_) => return pick.say("notes failed".into()),
    };
    let Some(text) = notes_text(&reply) else {
        return pick.say("no notes".into());
    };
    if peek_folder(ctx, caller)
        .and_then(|folder| popup(ctx, caller, &folder, "notes", &text))
        .is_err()
    {
        pick.say("notes failed".into());
    }
}

/// The popup's text for `i`: each note's title, then `author · at · id`, a blank line between; then `+N older`
/// when the verb left some out. None when there is no note. Controls stripped.
fn notes_text(reply: &Map<String, Value>) -> Option<String> {
    let notes = reply.get("notes").and_then(Value::as_array)?;
    if notes.is_empty() {
        return None;
    }
    let mut blocks: Vec<String> = notes
        .iter()
        .map(|n| {
            let field = |name: &str| clean(n.get(name));
            format!(
                "{}\n{} \u{b7} {} \u{b7} {}\n",
                field("title"),
                field("author"),
                field("at"),
                field("id")
            )
        })
        .collect();
    let older = reply.get("older").and_then(Value::as_u64).unwrap_or(0);
    if older > 0 {
        blocks.push(format!("+{older} older\n"));
    }
    Some(blocks.join("\n"))
}

/// `p` in a team (B10): member `target` by id in the popup: its live view captured, else its retained output, else
/// the note. Nothing switches, restarts or types.
fn preview(ctx: &Ctx, caller: &Caller, target: &Target, pick: &mut Pick) {
    if !target.pane.is_empty() && peek(ctx, caller, target).is_ok() {
        return;
    }
    match show_output(ctx, caller, target) {
        Opened::Shown => {}
        Opened::Nothing => pick.nothing(&target.name),
        Opened::Failed => pick.failed(&target.name),
    }
}

/// What Enter does with a row (the hub, slice 1): fold or unfold the ended section, ask before closing a kept shell,
/// resume an ended agent as a tab (off the loop), else open (focus) it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnterAction {
    Fold,
    Ask,
    Resume,
    Open,
}

fn enter_action(target: &Target) -> EnterAction {
    match target.life.as_str() {
        pick::FOLD => EnterAction::Fold,
        pick::KEPT => EnterAction::Ask,
        "ended" => EnterAction::Resume,
        _ => EnterAction::Open,
    }
}

/// Enter on a row: the fold line opens or closes, a kept shell asks `close? y/n`, an ended agent resumes off the loop
/// (`Jobs`; while it runs a second Enter starts nothing), anything else opens: a team's row by its life (`open_target`),
/// the flat bar's by focusing it.
fn enter(
    ctx: &Ctx,
    caller: &Caller,
    target: Target,
    pick: &mut Pick,
    shown: &mut Shown,
    panel: &mut Panel,
) {
    match enter_action(&target) {
        EnterAction::Fold => toggle_fold(pick, shown, panel),
        EnterAction::Ask => pick.ask(target),
        EnterAction::Resume => {
            panel.jobs.resume(ctx, caller, &target);
        }
        EnterAction::Open if shown.team => open_target(ctx, caller, &target, pick, shown),
        EnterAction::Open => match focus_target_hc(ctx, caller, &target) {
            Ok(focused) => {
                pick.focused();
                shown.focused(focused);
            }
            Err(_) => pick.failed(&target.name),
        },
    }
}

/// A resume's answer, on the pass it came: the successor's view focused by the id the resume answered (the ended id is
/// superseded, so it has none), else the note `resumed <name>`; a refusal says why on the note line,
/// `resume refused: <reason>`.
fn resumed(ctx: &Ctx, caller: &Caller, done: jobs::Done, pick: &mut Pick, shown: &mut Shown) {
    let (target, answer) = done;
    let reply = match answer {
        Ok(reply) => reply,
        Err(e) => return pick.say(format!("resume refused: {}", e.message)),
    };
    let successor = (reply.get("agent"))
        .and_then(|agent| agent.get("id"))
        .and_then(Value::as_str);
    let focused =
        successor.map(|id| crate::agents::view::focus_member(ctx, None, caller, id, None));
    match focused {
        Some(Ok(crate::agents::view::Member::Focused(f))) => {
            pick.focused();
            shown.focused(f);
        }
        _ => pick.say(format!("resumed {}", target.name)),
    }
}

/// The actions that answered since the last pass (`Jobs::drain`, which never waits), applied; true when any did, so
/// the bar draws again at once.
fn settle_jobs(
    ctx: &Ctx,
    caller: &Caller,
    pick: &mut Pick,
    shown: &mut Shown,
    panel: &mut Panel,
) -> bool {
    let done = panel.jobs.drain(ctx, caller);
    let any = !done.is_empty();
    for d in done {
        resumed(ctx, caller, d, pick, shown);
    }
    any
}

/// Opens a drawn agent row by member id and life (B6); nothing restarts or types. A validated view is focused
/// through the multiplexer alone, so an Unknown row's view opens without hc. Without one: Running reopens its view and
/// Lost reattaches its recovery terminal, each only when hc keeps the member's own terminal (review: a Lost row
/// whose reattach does not take shows its retained output); Unknown shows its output. An ended row never comes here:
/// Enter and a click resume it (`enter`). A member gone, a
/// terminal hc dropped or another identity's, or blank output, is `nothing to open for <name>`; a focus that did not
/// take, or hc unreachable, is `focus failed: <name>`. A successful open clears the selection; otherwise the row stays
/// selected, so `p` or Enter act on it next.
fn open_target(ctx: &Ctx, caller: &Caller, target: &Target, pick: &mut Pick, shown: &mut Shown) {
    use crate::agents::view::{Member, focus_member};
    let focused = |member: Res<Member>, shown: &mut Shown| match member {
        Ok(Member::Focused(f)) => {
            shown.focused(f);
            Some(Opened::Shown)
        }
        Ok(Member::Gone) => Some(Opened::Nothing),
        Ok(Member::NoView) => None,
        Err(_) => Some(Opened::Failed),
    };
    let opened = match target.life.as_str() {
        "running" | "lost" | "unknown" => {
            match focused(focus_member(ctx, None, caller, &target.id, None), shown) {
                Some(opened) => opened,
                None if target.life == "unknown" => show_output(ctx, caller, target),
                None => {
                    // Reattach only a terminal hc still keeps as the member's own: one it dropped is nothing to open; hc
                    // unreachable fails.
                    let Ok(core) = crate::hc::connect(ctx) else {
                        pick.failed(&target.name);
                        return;
                    };
                    match crate::agents::message::kept_member(ctx, caller, &core, &target.id) {
                        Ok(Some(true)) => {
                            match focused(
                                focus_member(ctx, Some(&core), caller, &target.id, None),
                                shown,
                            ) {
                                Some(Opened::Shown) => Opened::Shown,
                                // A Lost row whose recovery terminal did not reattach: its retained output instead.
                                _ if target.life == "lost" => show_output(ctx, caller, target),
                                other => other.unwrap_or(Opened::Failed),
                            }
                        }
                        Ok(_) => Opened::Nothing,
                        Err(_) => Opened::Failed,
                    }
                }
            }
        }
        _ => show_output(ctx, caller, target),
    };
    match opened {
        Opened::Shown => pick.focused(),
        Opened::Nothing => pick.nothing(&target.name),
        Opened::Failed => pick.failed(&target.name),
    }
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

/// The `run-shell -C` argument that opens the popup paging `path` over `bar`'s client: `popup_argv`, each word single-quoted.
pub fn popup_command(bar: &str, path: &str) -> String {
    popup_argv(bar, path, crate::state::root::HOST)
        .iter()
        .map(|a| format!("'{a}'"))
        .collect::<Vec<_>>()
        .join(" ")
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

/// The agent row a press on row `y` hit, read in one call and confirmed against the frame on screen.
fn click(ctx: &Ctx, caller: &Caller, y: u16, shown: &Shown) -> Option<Target> {
    let read = native(ctx, &click_read(&caller.pane), true)
        .ok()
        .filter(|read| read.code == 0)?;
    let id = pick::frame_hit(&read.stdout, shown.width, y, &shown.drawn)?;
    shown.targets.iter().find(|t| t.id == id).cloned()
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
    request: &Request,
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
    crate::cli::member_views(ctx, &caller)?;
    console_utf8();
    // A bar ticks for as long as it lives: hc's version is checked once, not on each tick's read. The unit tests
    // script hc call by call, so they keep the check per connect (`hc::tests` covers the cache).
    #[cfg(not(test))]
    crate::hc::remember_versions();
    let origin = if request.value("Managed").is_some() {
        Origin::Split
    } else {
        Origin::Hand
    };
    let ended = run_watch(ctx, &caller, origin, open(), write);
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
/// kills it only while it is still that process, and reports closed only when the pane is gone and no bar it could not
/// prove still runs. A label is never trusted: an unproven labelled pane refuses both.
/// -Close first sets the session's @mux-agents-sidebar `off` and `sidebar` unsets it, both inside the lock, so a
/// spawn's `ensure` never reopens a bar a returned -Close closed. Refused only from a tagged pane.
pub fn sidebar(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let close = request.switch("Close");
    let auto = request.switch("Auto");
    let stdin = request.switch("Stdin");
    let given = request.value("Source").is_some();
    // `-Source` and `-Stdin` belong to `-Auto` only, exactly one of them; `-Close` takes neither.
    let sources = usize::from(given) + usize::from(stdin);
    if close && auto || if auto { sources != 1 } else { sources != 0 } {
        return Err(crate::agents::usage("sidebar"));
    }
    // A chat's `clear` or `compact` is the same session going on: the person already had the bar or closed it.
    if auto && matches!(auto_source(request).as_deref(), Some("clear" | "compact")) {
        return Ok(skipped("source"));
    }
    let caller = resolve(ctx)?;
    crate::cli::member_views(ctx, &caller)?;
    crate::agents::registry::untagged(ctx, &caller, "an agent pane cannot open the side bar")?;
    lock(ctx, &scope::shown_lock("sidebar", &caller), || {
        let mut reply = Map::new();
        if auto {
            // The bar by default (a person's chat started or resumed): as `sidebar`, but a session whose bar the
            // person closed (`off`) keeps it closed, and the window is not marked viewed, since a chat window holds
            // no member view and its bar would leave at once.
            let read = native(
                ctx,
                &["show-options", "-qv", "-t", &caller.session, OPTION],
                false,
            )?;
            if read.stdout.trim_end_matches(['\r', '\n']) == "off" {
                return Ok(skipped("off"));
            }
        } else if !close {
            native(
                ctx,
                &["set-option", "-u", "-t", &caller.session, OPTION],
                false,
            )?;
        }
        if !close {
            migrate(ctx, &caller.session)?;
            let (pane, window, opened) = find_or_open(ctx, &caller.window)?;
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
        migrate(ctx, &caller.session)?;
        let mut closed: Vec<String> = Vec::new();
        for window in windows(ctx, &caller.session)? {
            closed.extend(close_window(ctx, &caller.session, &window)?);
        }
        // A watch this root's split started that no record proves (its opener died before it marked it) is killed
        // by its process (`close_strays`).
        closed.extend(close_strays(ctx, &caller.session)?);
        // Closed answers only with no bar left running. Nothing closed: any label refuses, as it may be a bar no
        // record proves (a 0.2.1 bar). A bar closed: a label may be one its rotate-window moved onto another pane, so
        // only a bar still running refuses (`running_bars`), such as one whose pane left a window that went.
        let left = if closed.is_empty() {
            labelled(ctx, &["-s", "-t", &caller.session])?
        } else {
            running_bars(ctx, &caller.session)?
        };
        if !left.is_empty() {
            return refuse(1, "sidebar", unproven(&left));
        }
        reply.insert(
            "pane".into(),
            closed.first().map_or(Value::Null, |p| p.clone().into()),
        );
        reply.insert("closed".into(), (!closed.is_empty()).into());
        reply.insert("panes".into(), closed.into());
        Ok(reply)
    })
}

/// `window`'s proven bar, killed only while it is still that process, then its record cleared: the pane closed,
/// none when the window has no proven bar or is gone from `session` (it took its bar with it). A kill that did not
/// take refuses.
fn close_window(ctx: &Ctx, session: &str, window: &str) -> Res<Option<String>> {
    let read = &["show-options", "-wqv", "-t", window, RECORD];
    let Some(bar) =
        on_window(ctx, session, window, read)?.and_then(|read| prove_read(ctx, window, &read))
    else {
        return Ok(None);
    };
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
    let _ = native(ctx, &["set-option", "-wu", "-t", &bar.window, RECORD], true);
    Ok(Some(bar.pane))
}

/// The session's watches this state root's bar splits started (`watch -Managed <key>` with this root's key, read from
/// each live pane's process) that are left once every proven bar closed, each killed only while it is alive and still
/// runs that process: an open whose opener died between its split and its record left one that no window's record
/// proves (it would close itself within STARTUP). A watch a person ran is never one, and another root's managed watch
/// (MUX_AGENTS_STATE=B in the same session) is left to its own STARTUP limit. The proven bars before it are closed
/// session-wide whatever root opened them, by design: a window has one bar. Answers the panes closed; a kill that did
/// not take is left to the watch's own STARTUP limit. An unreadable list refuses: it may hold such a watch.
fn close_strays(ctx: &Ctx, session: &str) -> Res<Vec<String>> {
    let key = root_key(ctx);
    let listed = native(
        ctx,
        &["list-panes", "-s", "-t", session, "-F", MARK_FORMAT],
        false,
    )?;
    let mut closed: Vec<String> = Vec::new();
    for line in listed.stdout.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let mut fields = line.splitn(4, '|');
        let (Some(pid), Some("0"), Some(pane)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Ok(pid) = pid.parse::<u32>() else {
            continue;
        };
        if pane_id(pane) && split_watch(ctx, pid, &key) && kill_bar(ctx, pane, pid) {
            closed.push(pane.to_string());
        }
    }
    Ok(closed)
}

/// Whether `pid` is this binary run as `<binary> watch -Managed <key>`: a watch a bar's split under the state root of
/// `key` started.
fn split_watch(ctx: &Ctx, pid: u32, key: &str) -> bool {
    ctx.os.row(pid).is_some_and(|row| {
        is_watch(ctx, &row)
            && row.argv.as_ref().is_some_and(|a| {
                a.get(2).is_some_and(|f| f == MANAGED) && a.get(3).is_some_and(|k| k == key)
            })
    })
}

/// The file in the state root that holds the root's key.
const ROOT_KEY: &str = "bar-key";

/// The state root's key a bar's split passes its watch, so `-Close` sweeps only its own root's strays: 32 random hex
/// digits stored once in the root (`ROOT_KEY`), the first writer's kept, and read back by every process, so the key
/// never depends on how the root's path is spelled or encoded. A root that cannot hold one gives this call a key of its
/// own: no other process's `-Close` matches its stray, which cleans itself up after STARTUP.
pub(crate) fn root_key(ctx: &Ctx) -> String {
    root_key_with(ctx, &|buf| crate::util::random_bytes(buf))
}

/// `root_key` with its random source, so a test can make it fail. The root is made private as every state folder is
/// (`agents::server_dir`), never by a plain create. Only a regular file is read, at most 64 bytes, so a FIFO or a large
/// file named `bar-key` blocks nothing. The key is published whole: written to a file of its own, then hard-linked to
/// `ROOT_KEY`, which fails if another process published first, so a reader never sees a part of a key. With no random
/// source, the key is this process's id (no other live process has it).
fn root_key_with(ctx: &Ctx, random: &dyn Fn(&mut [u8]) -> std::io::Result<()>) -> String {
    let file = ctx.state_root.join(ROOT_KEY);
    let read = || -> Option<String> {
        use std::io::Read;
        let meta = std::fs::symlink_metadata(&file).ok()?;
        if !meta.file_type().is_file() {
            return None;
        }
        let mut text = String::new();
        std::fs::File::open(&file)
            .ok()?
            .take(64)
            .read_to_string(&mut text)
            .ok()?;
        let key = text.trim();
        (key.len() == 32 && key.bytes().all(|c| c.is_ascii_hexdigit())).then(|| key.to_string())
    };
    if let Some(key) = read() {
        return key;
    }
    let mut bytes = [0u8; 16];
    if random(&mut bytes).is_err() {
        return format!("{:032x}", ctx.pid);
    }
    let mine: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let private = {
        #[cfg(unix)]
        let made = crate::state::private_unix::private_dirs(&ctx.state_root, &[&ctx.state_root]);
        #[cfg(windows)]
        let made = crate::state::private_windows::private_dirs(&[&ctx.state_root]);
        made
    };
    if private.is_err() {
        return mine;
    }
    let staged = ctx.state_root.join(format!("{ROOT_KEY}.{mine}.tmp"));
    let published =
        std::fs::write(&staged, &mine).and_then(|()| std::fs::hard_link(&staged, &file));
    let _ = std::fs::remove_file(&staged);
    match published {
        Ok(()) => mine,
        // Another process published first: its key is the root's.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => read().unwrap_or(mine),
        Err(_) => mine,
    }
}

/// `q` then `y`: the bar closes as `sidebar -Close` closes it, for its own window only, in this process. Under the
/// session's sidebar lock the session turns `off` (else another window's bar would open this one again on its next
/// walk; `sidebar` turns it back on), 0.2.x's record migrates, then `close_window`. A bar this process kills ends with
/// its pane; a bar not proven (run by hand, or its pane moved out of a window that went) has nothing to kill, and the
/// watch ends all the same, so `q` never leaves its own bar running.
fn close_bar(ctx: &Ctx, caller: &Caller) -> Res<()> {
    lock(ctx, &scope::shown_lock("sidebar", caller), || {
        native(
            ctx,
            &["set-option", "-t", &caller.session, OPTION, "off"],
            false,
        )?;
        migrate(ctx, &caller.session)?;
        close_window(ctx, &caller.session, &caller.window).map(|_| ())
    })
}

/// What started the chat, for `sidebar -Auto`: `-Source`, or `source` in the client's JSON on stdin (Copilot's
/// `sessionStart`). None when stdin is a terminal, too long, not JSON or has no `source`: that still opens.
fn auto_source(request: &Request) -> Option<String> {
    if let Some(source) = request.value("Source") {
        return Some(source.to_string());
    }
    let body: Value = serde_json::from_slice(&crate::agents::session_report::read_stdin()?).ok()?;
    body.get("source")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// `sidebar -Auto` that opened nothing, and why: `source` (`clear` or `compact`) or `off` (the person closed it).
fn skipped(why: &str) -> Map<String, Value> {
    let mut reply = Map::new();
    reply.insert("pane".into(), Value::Null);
    reply.insert("opened".into(), false.into());
    reply.insert("skipped".into(), why.into());
    reply
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
    is_watch(ctx, &row).then_some(row.start).flatten()
}

/// Whether `row` is this binary run as exactly `<binary> watch`, or `<binary> watch -Managed <key>` as the bar's split
/// starts it: a side bar, known by its process because a leaving bar removes its own label and mark before it exits.
pub(crate) fn is_watch(ctx: &Ctx, row: &crate::proc::Row) -> bool {
    let (Some(exe), Some(path), Some(argv)) = (ctx.exe.to_str(), &row.path, &row.argv) else {
        return false;
    };
    let same = if cfg!(windows) {
        path.eq_ignore_ascii_case(exe)
    } else {
        path == exe
    };
    same && argv.get(1).is_some_and(|a| a == "watch")
        && match argv.len() {
            2 => true,
            4 => argv[2] == MANAGED,
            _ => false,
        }
}

/// `window`'s bar, proven now (T9): its window record names a pane whose one read shows it alive (`#{pane_dead}` 0)
/// in `window` with the recorded pid, and that pid is alive with the recorded start, still this binary `watch`. A
/// failed proof is read once more, then there is no proven bar. An unreadable record refuses: it may name the bar.
fn prove(ctx: &Ctx, window: &str) -> Res<Option<Proven>> {
    let read = native(ctx, &["show-options", "-wqv", "-t", window, RECORD], false)?;
    Ok(prove_read(ctx, window, &read))
}

/// `prove` of `window`'s record as `read` answered it.
fn prove_read(ctx: &Ctx, window: &str, read: &Output) -> Option<Proven> {
    let (pane, pid, start) = parse_record(read.stdout.trim_end_matches(['\r', '\n']))?;
    for _ in 0..2 {
        if let Some(at) = proven(ctx, &pane, pid, start) {
            return (at == window).then_some(Proven {
                pane,
                window: at,
                pid,
            });
        }
    }
    None
}

/// The window ids of `session`. Each window's record is read as that window's own: a window list's option fields
/// fall back to the session's value, so only the ids are listed. An unreadable list refuses.
fn windows(ctx: &Ctx, session: &str) -> Res<Vec<String>> {
    let listed = native(
        ctx,
        &["list-windows", "-t", session, "-F", "#{window_id}"],
        false,
    )?;
    Ok(listed
        .stdout
        .split('\n')
        .map(|w| w.strip_suffix('\r').unwrap_or(w))
        .filter(|w| window_id(w))
        .map(str::to_string)
        .collect())
}

/// `args`, a call on `window` of `session`. A call on a window destroyed since it was listed or proven (its last pane
/// went) fails: rmux fails even a read, which tmux answers empty. A failed call answers none once `session` no longer
/// lists the window, and refuses while it still does or when the list cannot be read.
fn on_window(ctx: &Ctx, session: &str, window: &str, args: &[&str]) -> Res<Option<Output>> {
    let out = native(ctx, args, true)?;
    if out.code == 0 {
        return Ok(Some(out));
    }
    if windows(ctx, session)?.iter().any(|w| w == window) {
        return crate::mux::call_failed(ctx);
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

/// The panes that carry the side-bar label, of `scope`: `-t <window>` one window, `-s -t <session>` every window.
fn labelled(ctx: &Ctx, scope: &[&str]) -> Res<Vec<String>> {
    let listed = native(
        ctx,
        &[&["list-panes"], scope, &["-F", LABEL_FORMAT]].concat(),
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

/// The panes of `session` still running a side bar: labelled, alive, and marked for the pid they run (`open` marks a
/// bar's pane only while it runs that pid). A bar whose pane left a window that went (`join-pane` of its last pane into
/// another window) is one: its label and mark moved with its pane, its record went with the window. A label alone is
/// not: rmux's rotate-window moves a bar's label and mark onto the pane it swaps with, which runs another pid, and a
/// 0.2.1 bar carries no mark. An unreadable list refuses: it may hold such a pane.
fn running_bars(ctx: &Ctx, session: &str) -> Res<Vec<String>> {
    let listed = native(
        ctx,
        &["list-panes", "-s", "-t", session, "-F", LEFT_FORMAT],
        false,
    )?;
    let mut panes: Vec<String> = Vec::new();
    for line in listed.stdout.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let mut fields = line.splitn(4, '|');
        let (Some(pane), Some("1"), Some(pid), Some(mark)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if pane_id(pane)
            && pid.parse::<u32>().is_ok()
            && mark == pid
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

/// Whether this watch is its window's recorded bar: the record of its window names its own pane and its own pid,
/// and the full proof holds (its pane runs that pid, alive, in that window; the pid is this binary `watch` with the
/// recorded start). Only then does it keep the layout; a watch run any other way only draws.
fn own_record(ctx: &Ctx, caller: &Caller) -> bool {
    native(
        ctx,
        &["show-options", "-wqv", "-t", &caller.window, RECORD],
        true,
    )
    .ok()
    .filter(|read| read.code == 0)
    .and_then(|read| parse_record(read.stdout.trim_end_matches(['\r', '\n'])))
    .is_some_and(|(pane, pid, start)| {
        pane == caller.pane
            && pid == ctx.pid
            && proven(ctx, &pane, pid, start).is_some_and(|at| at == caller.window)
    })
}

/// The windows of the session that show a member's live view: each local member's record through `Identity::view`
/// on the session's panes (a dead pane is no view). None when either read fails; no pane read without members.
pub(crate) fn view_windows(ctx: &Ctx, caller: &Caller) -> Option<Vec<String>> {
    let locals = crate::agents::membership::locals(ctx, caller).ok()?;
    if locals.is_empty() {
        return Some(Vec::new());
    }
    let rows = crate::agents::registry::rows(ctx, caller).ok()?;
    let mut windows: Vec<String> = Vec::new();
    for local in &locals {
        if let Some(view) = local.identity.view(
            &local.stored.record,
            &rows,
            &crate::agents::core_identity::Key::of(caller),
        ) && !windows.contains(&view.window)
        {
            windows.push(view.window);
        }
    }
    Some(windows)
}

/// The bar leaves its window: under the session's sidebar lock, the window's views are read again, and a
/// member view there again (a spawn or focus that reused the bar while this waited on the lock), an unread list or a
/// lock not taken keeps the bar: false. Else its pane's label is removed only while the pane still runs this
/// process, its window's record is cleared only while it still names this bar, and the bar leaves: true.
/// reentry-6a E4.5: `window` carries `STATUS` (`up` opened it because no member runs).
fn is_status(ctx: &Ctx, window: &str) -> bool {
    native(ctx, &["show-options", "-wqv", "-t", window, STATUS], true)
        .is_ok_and(|read| read.code == 0 && read.stdout.trim_end_matches(['\r', '\n']) == "1")
}

fn leave(ctx: &Ctx, caller: &Caller) -> bool {
    lock(ctx, &scope::shown_lock("sidebar", caller), || {
        if view_windows(ctx, caller).is_none_or(|windows| windows.contains(&caller.window)) {
            return Ok(false);
        }
        let unlabel = unlabel_command(&caller.pane, ctx.pid);
        let unlabel: Vec<&str> = unlabel.iter().map(String::as_str).collect();
        let _ = native(ctx, &unlabel, true);
        let unmark = unmark_command(&caller.pane, ctx.pid);
        let unmark: Vec<&str> = unmark.iter().map(String::as_str).collect();
        let _ = native(ctx, &unmark, true);
        if own_record(ctx, caller) {
            let _ = native(
                ctx,
                &["set-option", "-wu", "-t", &caller.window, RECORD],
                true,
            );
        }
        // The window holds no view now: a bar opened here later starts as one that never held a view.
        let _ = native(
            ctx,
            &["set-option", "-wu", "-t", &caller.window, VIEWED],
            true,
        );
        Ok(true)
    })
    .unwrap_or(false)
}

/// The conditional removal of `pane`'s side-bar label, only while it is alive and still runs `pid`.
pub fn unlabel_command(pane: &str, pid: u32) -> Vec<String> {
    bound(pane, pid, format!("set-option -p -u -t {pane} @agent-role"))
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

/// The bar's mark on `pane`, set to `pid` only while the pane runs `pid` (alive or dead, so a split whose watch
/// ended at once is still marked).
pub fn mark_command(pane: &str, pid: u32) -> Vec<String> {
    vec![
        "if-shell".into(),
        "-F".into(),
        "-t".into(),
        pane.into(),
        format!("#{{==:#{{pane_pid}},{pid}}}"),
        format!("set-option -p -t {pane} {MARK} {pid}"),
    ]
}

/// What a watch's self-exit check found under the session's sidebar lock.
enum SelfExit {
    Left,
    Kept,
    ByHand,
    Unread,
}

/// a watch not proven for STARTUP decides under the session's sidebar lock, from what it reads there. A watch a
/// person ran (`Origin::Hand`) whose pane is not marked for this process is no bar's to clean up; one the bar's split
/// started (`Origin::Split`) is the bar's marked or not, since its opener may have died before it marked the pane.
/// Its window's record proving it in full (an ensure recorded or adopted it while it waited on the lock) keeps it;
/// else it removes its pane's label and mark (each bound to its pid, like `leave`; a no-op when neither is set) and
/// leaves, so its pane closes with it and, dead, blocks no later bar. An unread mark or record, or a lock not taken,
/// decides nothing.
fn self_exit(ctx: &Ctx, caller: &Caller, origin: Origin) -> SelfExit {
    lock(ctx, &scope::shown_lock("sidebar", caller), || {
        let read = |args: &[&str]| native(ctx, args, true).ok().filter(|r| r.code == 0);
        if origin == Origin::Hand {
            let Some(mark) = read(&["display-message", "-p", "-t", &caller.pane, OWN_MARK]) else {
                return Ok(SelfExit::Unread);
            };
            let own = ctx.pid.to_string();
            if mark.stdout.trim_end_matches(['\r', '\n']) != format!("{own}|{own}") {
                return Ok(SelfExit::ByHand);
            }
        }
        let Some(record) = read(&["show-options", "-wqv", "-t", &caller.window, RECORD]) else {
            return Ok(SelfExit::Unread);
        };
        // Kept only by the full proof the tick uses (`own_record`): this pane, this pid, alive in this window, with the
        // recorded start. A record naming the pane and pid with a stale start keeps nothing.
        if parse_record(record.stdout.trim_end_matches(['\r', '\n'])).is_some_and(
            |(pane, pid, start)| {
                pane == caller.pane
                    && pid == ctx.pid
                    && proven(ctx, &pane, pid, start).is_some_and(|at| at == caller.window)
            },
        ) {
            return Ok(SelfExit::Kept);
        }
        for command in [
            unlabel_command(&caller.pane, ctx.pid),
            unmark_command(&caller.pane, ctx.pid),
        ] {
            let command: Vec<&str> = command.iter().map(String::as_str).collect();
            let _ = native(ctx, &command, true);
        }
        Ok(SelfExit::Left)
    })
    .unwrap_or(SelfExit::Unread)
}

/// The conditional removal of `pane`'s mark, only while it is alive and still runs `pid`.
pub fn unmark_command(pane: &str, pid: u32) -> Vec<String> {
    bound(pane, pid, format!("set-option -p -u -t {pane} {MARK}"))
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

/// Splits the bar at the right edge of `window`, full height (`-f -h`), with the pane environment (Unix:
/// the state root) so watch reads the caller's state. The bar is `bar_width` of the window (its minimum when the width cannot
/// be read), asked for with -l and set again by a pid-bound resize-pane, since rmux ignores -l; a refused resize is left to
/// the next watch tick. `-d` keeps the active pane. The split's process must be this binary `watch` (else nothing is
/// killed and the open refuses); then the pane is labelled (bound to that pid, read back) and recorded as
/// `<pane> <pid> <start>` as `window`'s record. A bar that cannot
/// be labelled or recorded is closed by the guarded kill, which spares a pane that runs anything else by then. The
/// bar is proven, in `window`, as the last step; a pane that fails it is not killed, its record is cleared and the
/// open refuses.
fn open(ctx: &Ctx, target: &str) -> Res<(String, String)> {
    let env = pane_env(utf8(&ctx.state_root)?, crate::state::root::HOST);
    let (columns, sizing) = window_width(ctx, target);
    let width = bar_width(columns.unwrap_or(0), sizing).to_string();
    let mut split: Vec<&str> = vec![
        "split-window",
        "-d",
        "-f",
        "-h",
        "-P",
        "-F",
        SPLIT_FORMAT,
        "-t",
        target,
    ];
    split.extend(env.iter().map(String::as_str));
    // `-Managed <key>`: the watch knows this split started it, so it cleans itself up even when this open dies before
    // the mark below (`self_exit`), and `sidebar -Close` under this state root knows it for the bar's.
    let key = root_key(ctx);
    let command = [utf8(&ctx.exe)?, "watch", MANAGED, &key];
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
    // Marked at once, before anything can fail: a pane this open leaves behind is found and closed by the next one
    // (`sweep`). shortcut: the mark is the call right after the split, not part of it (split-window cannot set a
    // pane option on tmux 3.2 or rmux 0.10); a mark that fails closes the pane by its pid, and only a pane whose mark
    // and close both fail is left unmarked, named in the refusal.
    let mark = mark_command(&pane, pid);
    let mark: Vec<&str> = mark.iter().map(String::as_str).collect();
    if !native(ctx, &mark, true).is_ok_and(|done| done.code == 0) {
        if kill_bar(ctx, &pane, pid) {
            return refuse(
                1,
                "rmux",
                format!("the side-bar pane {pane} could not be marked; it was closed"),
            );
        }
        return refuse(
            1,
            "sidebar",
            format!(
                "the side-bar pane {pane} could not be marked or closed; close it with kill-pane -t {pane}"
            ),
        );
    }
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
            &["set-option", "-w", "-t", target, RECORD, &record],
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
    if proven(ctx, &pane, pid, start).is_none_or(|at| at != target) {
        let _ = native(ctx, &["set-option", "-wu", "-t", target, RECORD], true);
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

    // o2 call 28 (QA F2): every flat layout keeps `recover` for a member that needs recovery: the compact row under
    // a section heading (width 60, an idle sibling), and the narrow row (width 30), Running or Lost, running or not (the
    // bare `recover` pill when its words and age do not fit).
    #[test]
    fn every_flat_layout_keeps_recover() {
        let look = Look {
            color: false,
            ascii: false,
            dumb: false,
        };
        let fixer = |state: &str, running: bool, unread: u64| {
            json!({"name": "fixer", "state": state, "life": state, "age": 60, "activity": "working",
                "needsRecovery": true, "recoveryRunning": running, "unread": unread})
        };
        let rest = json!({"name": "rest", "state": "running", "life": "running", "age": 60, "activity": "idle"});
        let badge = "\u{2709} 12";
        // (case, width, rows, the name or its cut shown, the badge shown)
        for (case, width, agents, name, mail) in [
            (
                "compact",
                60,
                vec![fixer("running", true, 0), rest.clone()],
                "fixer",
                false,
            ),
            (
                "narrow running",
                30,
                vec![fixer("running", true, 0)],
                "fixer",
                false,
            ),
            (
                "narrow lost",
                30,
                vec![fixer("lost", true, 0)],
                "fixer",
                false,
            ),
            (
                "narrow lost, not running",
                30,
                vec![fixer("lost", false, 0)],
                "fixer",
                false,
            ),
            // o2 call 34: the name is cut before the pill goes.
            (
                "narrowest running",
                16,
                vec![fixer("running", true, 0)],
                "fix",
                false,
            ),
            (
                "narrowest lost",
                16,
                vec![fixer("lost", true, 0)],
                "fix",
                false,
            ),
            ("unread 12", 20, vec![fixer("running", true, 12)], "f", true),
            // The badge yields too: at 14 the pill and a cut name fit only without it.
            (
                "unread 12, badge yields",
                14,
                vec![fixer("running", true, 12)],
                "f",
                false,
            ),
        ] {
            let board = json!({"running": 1, "agents": agents});
            let laid = lay(board.as_object(), &[], Size { width, height: 40 }, look);
            let text: Vec<String> = laid.rows.iter().map(|r| render(r, false)).collect();
            let line = text
                .iter()
                .skip(1)
                .find(|l| l.contains("recover"))
                .cloned()
                .unwrap_or_default();
            assert!(line.contains("recover"), "{case}: {text:#?}");
            assert!(
                line.contains(name),
                "{case}: the name, or its cut: {line:?}"
            );
            assert_eq!(line.contains(badge), mail, "{case}: the badge: {line:?}");
            // A member that needs recovery is in Inbox, under its heading.
            if case == "compact" {
                assert!(
                    text.iter().any(|l| l.contains("Inbox")),
                    "{case}: no section heading: {text:#?}"
                );
            }
        }
    }

    // Call 9 and build checklist 1 (call 20): a member that needs recovery reads `recover` on the flat bar too
    // (`recover (still running)` while its client runs), never its hc state word or its live activity word; a Running
    // member stays in the live section (still clickable) with the recovery word.
    #[test]
    fn the_flat_bar_shows_recover_for_a_member_that_needs_recovery() {
        for (state, running) in [("lost", false), ("lost", true), ("running", true)] {
            let board = json!({"running": 0, "agents": [{"name": "fixer", "state": state, "life": state, "age": 60,
                "activity": "working", "needsRecovery": true, "recoveryRunning": running}]});
            let look = Look {
                color: false,
                ascii: false,
                dumb: false,
            };
            let laid = lay(
                board.as_object(),
                &[],
                Size {
                    width: 60,
                    height: 20,
                },
                look,
            );
            let text: Vec<String> = laid.rows.iter().map(|r| render(r, false)).collect();
            let fixer = text
                .iter()
                .find(|l| l.contains("fixer"))
                .cloned()
                .unwrap_or_default();
            let word = if running {
                "recover (still running)"
            } else {
                "recover"
            };
            let case = format!("{state}, running {running}");
            assert!(
                fixer.trim_end().ends_with(&format!("{word}   1m")),
                "{case}: {fixer:?}"
            );
            assert!(
                !fixer.contains("lost") && !fixer.contains("working"),
                "{case}: {fixer:?}"
            );
        }
    }

    // restore-6c checklist 18 (P1): the flat bar shows the recovery reason beside `recover` where it fits (60),
    // `recover` and the age where only that fits (24), and the bare `recover` pill below that (16).
    #[test]
    fn the_flat_bar_shows_the_recovery_reason_where_it_fits() {
        let board = json!({"running": 0, "agents": [{"name": "fixer", "state": "lost", "life": "lost", "age": 60,
            "needsRecovery": true, "recoveryReason": "exited 1"}]});
        let look = Look {
            color: false,
            ascii: false,
            dumb: false,
        };
        for (width, tail) in [
            (60, "recover: exited 1   1m"),
            (24, "recover   1m"),
            (16, "recover"),
        ] {
            let laid = lay(board.as_object(), &[], Size { width, height: 20 }, look);
            let text: Vec<String> = laid.rows.iter().map(|r| render(r, false)).collect();
            let fixer = text
                .iter()
                .find(|l| l.contains("recover"))
                .cloned()
                .unwrap_or_default();
            assert!(fixer.trim_end().ends_with(tail), "{width}: {fixer:?}");
            if width < 60 {
                assert!(!fixer.contains("exited"), "{width}: {fixer:?}");
            }
        }
    }
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    #[test]
    fn on_call_is_its_own_kind() {
        let row = serde_json::json!({"activity": "on call"});
        let k = Kind::live(row.as_object().unwrap());
        assert_eq!(
            (k, k.word(), k.glyph(false), k.glyph(true)),
            (Kind::OnCall, "on call", "\u{25cc}", "o")
        );
        assert!(
            Kind::Blocked < Kind::OnCall
                && Kind::OnCall < Kind::Done
                && Kind::OnCall < Kind::Working
        );
    }

    // The caller's window @1 record: read, cleared and written as a window option.
    const READ_RECORD: &str = "-N show-options -wqv -t @1 @mux-agents-sidebar-id";
    const CLEAR_RECORD: &str = "-N set-option -wu -t @1 @mux-agents-sidebar-id";
    const LABELS: &str =
        "-N list-panes -t @1 -F #{pane_id}|#{&&:#{==:#{@agent-role},sidebar},#{==:#{pane_dead},0}}";
    const RECORD7: &str = "-N set-option -w -t @1 @mux-agents-sidebar-id %7 107 1070";
    // 0.2.x's session record, read and unset by the one migration.
    const SESSION_RECORD: &str = "-N show-options -qv -t $3 @mux-agents-sidebar-id";
    const CLEAR_SESSION: &str = "-N set-option -u -t $3 @mux-agents-sidebar-id";
    const SESSION_LABELS: &str = "-N list-panes -s -t $3 -F #{pane_id}|#{&&:#{==:#{@agent-role},sidebar},#{==:#{pane_dead},0}}";
    // -Close's last read once it closed a bar: the session's panes with their label, pid and the bar's mark.
    const SESSION_LEFT: &str = "-N list-panes -s -t $3 -F #{pane_id}|#{&&:#{==:#{@agent-role},sidebar},#{==:#{pane_dead},0}}|#{pane_pid}|#{@mux-agents-bar}";
    // -Close's list of the session's windows.
    const WINDOWS: &str = "-N list-windows -t $3 -F #{window_id}";
    const PROOF: &str = "#{pane_id}|#{window_id}|#{pane_dead}|#{pane_pid}";
    // The watch's own record: pane %7, the fake process 4242 started at 2000.
    const OWN: &str = "%7 4242 2000\n";
    const READ_OPTION: &str = "-N show-options -qv -t $3 @mux-agents-sidebar";
    // QA: ensure marks its window @1 viewed, right after it reads the session's option.
    const VIEWED_ON: &str = "-N set-option -w -t @1 @mux-agents-viewed 1";
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

    // The hint row as the bar draws it at 40 columns, nothing selected.
    const HINT: &str = "\u{23ce} open  p peek  m mail  ? keys";

    // `lay` with the bar's last rows as `run_watch` adds them: the hint row, pinned to the last row.
    fn with_hint(
        board: Option<&Map<String, Value>>,
        notes: &[(&str, Style)],
        size: Size,
        look: Look,
    ) -> Laid {
        let mut laid = lay(board, notes, size, look);
        dress(
            &mut laid,
            notes,
            size,
            look,
            Dress::default(),
            &Keymap::default(),
        );
        laid
    }

    // The flat bar's rows without its last rows (the hint pinned to the pane's last row): what the layout goldens
    // compare.
    fn body(board: &Value, size: Size) -> Vec<String> {
        let rows = lay(board.as_object(), &[], size, PLAIN_LOOK).rows;
        rows.iter().map(|r| render(r, false)).collect()
    }

    // The flat bar as `lay` draws it, with the ended fold open (Tab).
    fn lay_open(board: Option<&Map<String, Value>>, size: Size, look: Look) -> Laid {
        let focus = tree::Focus {
            ended_open: true,
            ..tree::Focus::default()
        };
        lay_focus(board, &[], size, look, focus).0
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
            {"name": "add-subtract", "state": "running", "doing": "Adding subtract(a, b) under add in calc.py", "age": 42, "unread": 0, "color": "blue", "activity": "blocked", "since": 12, "waiting": false, "life": "running", "view": "attached", "hcAddress": "t-add"},
            {"name": "test-calc", "state": "running", "doing": "Done: test_calc.py passed, exit 0", "age": 180, "unread": 1, "color": "magenta", "activity": "done", "since": 30, "waiting": true, "life": "running", "view": "attached", "hcAddress": "t-test"},
            {"name": "fix-ci-flake", "state": "running", "doing": "Re-running tests/test_watch.rs with --nocapture", "age": 660, "unread": 0, "color": "bright-blue", "activity": "working", "since": null, "waiting": false, "life": "running", "view": "attached", "hcAddress": "t-fix"},
            {"name": "docs-usage", "state": "exited 0", "age": 1080, "endedAgo": 1080, "exitCode": 0, "life": "ended", "view": "detached", "hcAddress": "t-docs"},
            {"name": "bump-deps", "state": "exited 101", "age": 1500, "endedAgo": 1500, "exitCode": 101, "life": "ended", "view": "detached", "hcAddress": "t-bump"},
            {"name": "spike-rmux", "state": "stopped", "age": 3600, "endedAgo": 3600, "exitCode": null, "life": "ended", "view": "detached", "hcAddress": "t-spike"},
            {"name": "older", "state": "stopped", "age": 7200, "endedAgo": 7200, "exitCode": null, "life": "ended", "view": "detached", "hcAddress": "t-older"}
        ]})
    }

    #[test]
    fn layout_draws_the_mock_up() {
        // The hub's sections: Inbox (needs you, done and unseen with mail), Working, each heading with its count; an
        // Inbox row keeps its word (its reasons differ), a Working row shows glyph and time (the heading names it); the
        // ended agents are one folded line.
        assert_eq!(
            body(&sectioned(), W40),
            [
                pad("mux-agents", "2 inbox \u{b7} 1 working"),
                String::new(),
                " Inbox \u{b7} 2".to_string(),
                pad("! add-subtract", "needs you  12s"),
                "   Adding subtract(a, b) under add i...".to_string(),
                pad("\u{2726} test-calc  \u{2709} 1", "done  30s"),
                "   Done: test_calc.py passed, exit 0".to_string(),
                String::new(),
                " Working \u{b7} 1".to_string(),
                pad("\u{25c9} fix-ci-flake", "11m"),
                "   Re-running tests/test_watch.rs wi...".to_string(),
                String::new(),
                " \u{25b8} Ended \u{b7} 7".to_string(),
            ]
        );
    }

    #[test]
    fn the_header_counts_the_inbox_and_working_only() {
        // On call and idle agents are Free: the header counts neither, nor the ended agents (the Free heading and the
        // fold line count them).
        let one = json!({"running": 1, "endedTotal": 0, "agents": [
            {"name": "b", "state": "running", "age": 5, "unread": 0, "color": "blue", "activity": "on call", "pane": "%1"}
        ]});
        let rows = body(&one, W40);
        assert_eq!(rows[0], " mux-agents");
        assert_eq!(rows[2], " Free \u{b7} 1");
        let words = |board: &Value| -> Vec<String> {
            counts(board.as_object().unwrap())
                .into_iter()
                .map(|(text, _)| text)
                .collect()
        };
        let mut all = sectioned();
        all["agents"][2]["activity"] = json!("on call");
        assert_eq!(words(&all), ["2 inbox"]);
        // Nothing alive: `0 running`. A member that needs recovery and no longer runs is in the inbox, not the fold.
        let fixer = json!({"running": 0, "endedTotal": 1, "agents": [
            {"name": "fixer", "state": "stopped", "life": "ended", "age": 60, "needsRecovery": true}
        ]});
        assert_eq!(words(&fixer), ["1 inbox", "0 running"]);
        let rows = body(&fixer, W40);
        assert!(
            rows.iter()
                .any(|r| r.contains("fixer") && r.contains("recover")),
            "{rows:?}"
        );
        assert!(!rows.iter().any(|r| r.contains("Ended")), "{rows:?}");
    }

    #[test]
    fn an_idle_agent_is_muted_whole_and_listening_counts_as_idle() {
        let color = Look {
            color: true,
            ..PLAIN_LOOK
        };
        let sgr = |codes: &str, text: &str| format!("\u{1b}[{codes}m{text}\u{1b}[0m");
        // One idle agent by report, one with no report listening in read -Wait: both idle, under the Free heading.
        let board = json!({"running": 2, "endedTotal": 0, "agents": [
            {"name": "rester", "state": "running", "age": 300, "since": 90, "unread": 0, "color": "magenta", "activity": "idle", "pane": "%1"},
            {"name": "listener", "state": "running", "age": 240, "unread": 0, "color": "blue", "waiting": true, "pane": "%2"}
        ]});
        let rows = format_board(board.as_object(), &[], W40, color);
        // The heading bold and dim, its count dim.
        assert_eq!(
            rows[2],
            format!(" {}{}", sgr("1;2", "Free"), sgr("2", " \u{b7} 2"))
        );
        // Under it the row is glyph, name and age, each muted (the name keeps its hue).
        assert_eq!(
            rows[3],
            [
                format!(" {}", sgr("2", "\u{25cc}")),
                " ".into(),
                sgr("2;35", "rester"),
                " ".repeat(26),
                sgr("2", "  1m"),
            ]
            .concat()
        );
        assert!(
            rows[4].starts_with(&format!(" {}", sgr("2", "\u{25cc}"))),
            "{:?}",
            rows[4]
        );
        // Free agents are not counted in the header.
        assert_eq!(rows[0], format!(" {}", sgr("1", "mux-agents")));
    }

    #[test]
    fn sections_head_each_state_and_go_first_when_short() {
        // A heading shows whenever an agent lives, even with every agent in one section.
        let mut one = sectioned();
        one["agents"][2]["activity"] = json!("blocked");
        let rows = body(&one, W40);
        assert!(rows.iter().any(|r| r == " Inbox \u{b7} 3"), "{rows:?}");
        assert!(!rows.iter().any(|r| r.contains("Working")), "{rows:?}");
        // A short bar drops the blank rows, then the headings, before any agent or the fold line; without its heading
        // a row keeps its word.
        let all = body(&sectioned(), W40).len();
        let short = body(
            &sectioned(),
            Size {
                width: 40,
                height: all - 5,
            },
        );
        assert!(
            short
                .iter()
                .all(|r| !r.is_empty() && !r.contains("Inbox") && !r.contains("Working ")),
            "{short:?}"
        );
        for name in ["add-subtract", "test-calc", "fix-ci-flake", "Ended"] {
            assert!(short.iter().any(|r| r.contains(name)), "{name}: {short:?}");
        }
        assert!(
            short
                .iter()
                .any(|r| r.contains("fix-ci-flake") && r.contains("working")),
            "{short:?}"
        );
        // Each drawn target's row is its name line, under its heading; the fold line is a target too.
        let laid = lay(sectioned().as_object(), &[], W40, PLAIN_LOOK);
        let rows: Vec<usize> = laid.agents.iter().map(|(_, row)| *row).collect();
        assert_eq!(rows, [3, 5, 9, 12]);
        let text = |at: usize| {
            laid.rows[at]
                .iter()
                .map(|(t, _)| t.as_str())
                .collect::<String>()
        };
        assert_eq!([text(2), text(8)], [" Inbox \u{b7} 2", " Working \u{b7} 1"]);
        // A heading line is never a click on an agent.
        assert_eq!(pick::row_hit(&text(2)), None);
        assert_eq!(pick::row_hit(&text(3)), Some("add-subtract"));
    }

    #[test]
    fn layout_nothing_running_and_more_ended() {
        // Nothing live: the ended agents, lost ones included, are one folded line.
        let ended = json!({"running": 0, "endedTotal": 2, "agents": [
            {"name": "gone", "state": "lost", "age": 60, "life":"lost", "view":"detached", "hcAddress":"t-gone"},
            {"name": "halted", "state": "stopped", "age": 60, "endedAgo": 60, "exitCode": null, "life":"ended", "view":"detached", "hcAddress":"t-halted"},
            {"name": "crashed", "state": "exited", "age": null, "exitCode": null, "life":"ended", "view":"detached", "hcAddress":"t-crashed"}
        ]});
        assert_eq!(
            body(&ended, W40),
            [
                pad("mux-agents", "0 running"),
                String::new(),
                " No agents running.".to_string(),
                String::new(),
                " \u{25b8} Ended \u{b7} 3".to_string(),
            ]
        );
        // Open: those that ended within the hour, newest first (a tie keeps the board's order), glyph and age only;
        // one of unknown age is among the older.
        let open: Vec<String> = (lay_open(ended.as_object(), W40, PLAIN_LOOK).rows.iter())
            .map(|r| render(r, false))
            .collect();
        assert_eq!(
            open[4..],
            [
                " \u{25be} Ended \u{b7} 3".to_string(),
                pad("- gone", "1m"),
                pad("- halted", "1m"),
                "   +1 older".to_string(),
            ]
        );
        let mut unknown = ended.clone();
        unknown["agents"][0]["state"] = json!("unknown");
        unknown["agents"][0]["life"] = json!("unknown");
        assert_eq!(body(&unknown, W40)[0], pad("mux-agents", "0 running"));
        let empty = json!({"running": 0, "endedTotal": 0, "agents": []});
        assert_eq!(
            body(&empty, W40),
            [
                pad("mux-agents", "0 running"),
                String::new(),
                " No agents running.".to_string(),
            ]
        );
        // 12 ended, of which the feed carries 10: the fold counts 12.
        let rows: Vec<Value> = (0..10)
            .map(|n| json!({"name": format!("a{n}"), "state": "stopped", "age": 60, "endedAgo": 60, "life":"ended", "view":"detached", "hcAddress":format!("t{n}")}))
            .collect();
        let twelve = json!({"running": 0, "endedTotal": 12, "agents": rows});
        assert_eq!(
            body(&twelve, W40).last().unwrap(),
            " \u{25b8} Ended \u{b7} 12"
        );
        // A board never read: `? running`, the note, and the hint on the last row.
        let never = format_board(None, &["refresh failed"], W40, PLAIN_LOOK);
        assert_eq!(
            never[..3],
            [
                pad("mux-agents", "? running"),
                String::new(),
                " refresh failed".to_string()
            ]
        );
        assert_eq!(never.len(), 40);
        assert_eq!(never[39], format!(" {HINT}"));
    }

    // The open fold judges an ended agent by when it ended (`endedAgo`), never by when it started: one created 2 h ago
    // that ended 10 s ago is listed; one that ended over an hour ago, or whose end time is unknown, folds into
    // `+N older`. The board's order (newest end first) is kept.
    #[test]
    fn the_open_fold_lists_an_agent_by_when_it_ended() {
        let board = json!({"running": 0, "endedTotal": 3, "agents": [
            {"id": "aaaaaaaaaaaa", "name": "late", "state": "stopped", "life": "ended", "age": 7200, "endedAgo": 10},
            {"id": "bbbbbbbbbbbb", "name": "young", "state": "stopped", "life": "ended", "age": 60, "endedAgo": 30},
            {"id": "cccccccccccc", "name": "long", "state": "stopped", "life": "ended", "age": 60, "endedAgo": 4000},
            {"id": "dddddddddddd", "name": "untimed", "state": "stopped", "life": "ended", "age": 60}
        ]});
        let laid = lay_open(
            board.as_object(),
            Size {
                width: 40,
                height: 24,
            },
            PLAIN_LOOK,
        );
        let rows: Vec<String> = laid.rows.iter().map(|r| render(r, false)).collect();
        let at = |name: &str| rows.iter().position(|r| r.contains(name));
        assert!(
            at("late").is_some_and(|late| at("young").is_some_and(|young| late < young)),
            "{rows:?}"
        );
        assert!(at("long").is_none() && at("untimed").is_none(), "{rows:?}");
        assert!(
            rows.iter().any(|r| r.trim_start() == "+2 older"),
            "{rows:?}"
        );
    }

    // An ended agent with unread mail is never folded away: it stands in Inbox with its ended glyph and word and its ✉
    // badge, and the header counts it; once its mail is read it goes back to the Ended fold.
    #[test]
    fn an_ended_agent_with_unread_mail_stands_in_inbox() {
        let board = |unread: u64| {
            json!({"running": 1, "endedTotal": 2, "agents": [
                {"id": "aaaaaaaaaaaa", "name": "worker", "state": "running", "activity": "working", "age": 60,
                 "pane": "%2", "life": "running"},
                {"id": "bbbbbbbbbbbb", "name": "reporter", "state": "exited", "exitCode": 0, "life": "ended",
                 "age": 120, "endedAgo": 60, "unread": unread},
                {"id": "cccccccccccc", "name": "quiet", "state": "stopped", "life": "ended", "age": 300,
                 "endedAgo": 200}
            ]})
        };
        let rows = plain(
            &board(2),
            Size {
                width: 40,
                height: 24,
            },
        );
        assert_eq!(
            rows[0],
            pad("mux-agents", "1 inbox \u{b7} 1 working"),
            "{rows:?}"
        );
        let inbox = rows
            .iter()
            .position(|r| r.starts_with(" Inbox"))
            .expect("an Inbox");
        let line = &rows[inbox + 1];
        assert!(
            line.starts_with(" \u{2713} reporter  \u{2709} 2") && line.contains("done"),
            "{rows:?}"
        );
        assert!(
            rows.iter().any(|r| r == " \u{25b8} Ended \u{b7} 1"),
            "{rows:?}"
        );
        let rows = plain(
            &board(0),
            Size {
                width: 40,
                height: 24,
            },
        );
        assert_eq!(rows[0], pad("mux-agents", "1 working"), "{rows:?}");
        assert!(!rows.iter().any(|r| r.starts_with(" Inbox")), "{rows:?}");
        assert!(
            rows.iter().any(|r| r == " \u{25b8} Ended \u{b7} 2"),
            "{rows:?}"
        );
    }

    // An ended row shows the time since it ended (`endedAgo`), in the open fold and in Inbox alike; a row without it
    // keeps its age.
    #[test]
    fn an_ended_row_shows_the_time_since_it_ended() {
        let board = json!({"running": 0, "endedTotal": 3, "agents": [
            {"id": "aaaaaaaaaaaa", "name": "fresh", "state": "stopped", "life": "ended", "age": 7200, "endedAgo": 30},
            {"id": "bbbbbbbbbbbb", "name": "mailer", "state": "exited", "exitCode": 0, "life": "ended", "age": 7200,
             "endedAgo": 600, "unread": 1},
            {"id": "cccccccccccc", "name": "gone", "state": "lost", "life": "lost", "age": 120}
        ]});
        let laid = lay_open(
            board.as_object(),
            Size {
                width: 40,
                height: 24,
            },
            PLAIN_LOOK,
        );
        let rows: Vec<String> = laid.rows.iter().map(|r| render(r, false)).collect();
        let row = |name: &str| {
            rows.iter()
                .find(|r| r.contains(name))
                .cloned()
                .unwrap_or_default()
        };
        assert!(row("fresh").ends_with(" 30s"), "{rows:?}");
        assert!(row("mailer").ends_with(" 10m"), "{rows:?}");
        assert!(row("gone").ends_with(" 2m"), "{rows:?}");
    }

    #[test]
    fn the_open_fold_counts_every_older_ended_agent() {
        // The feed carries the 10 newest ended agents; `+N older` counts the rest of `endedTotal` too.
        let ended = |total: usize| {
            let rows: Vec<Value> = (0..total.min(10))
                .map(|n| json!({"name": format!("a{n}"), "state": "stopped", "age": 60, "endedAgo": 60}))
                .collect();
            json!({"running": 0, "endedTotal": total, "agents": rows})
        };
        let open = |board: &Value, height: usize| -> Vec<String> {
            let laid = lay_open(board.as_object(), Size { width: 40, height }, PLAIN_LOOK);
            laid.rows.iter().map(|r| render(r, false)).collect()
        };
        assert_eq!(open(&ended(4), 40).last().unwrap(), &pad("- a3", "1m"));
        assert_eq!(open(&ended(11), 40).last().unwrap(), "   +1 older");
        // Fewer rows fit: the open rows go, the oldest first, into `+N older`, then that line; the fold line stays.
        let fold = " \u{25be} Ended \u{b7} 11".to_string();
        let (header, nothing) = (
            pad("mux-agents", "0 running"),
            " No agents running.".to_string(),
        );
        assert_eq!(
            open(&ended(11), 5),
            [
                header.clone(),
                nothing.clone(),
                fold.clone(),
                pad("- a0", "1m"),
                "   +10 older".into()
            ]
        );
        assert_eq!(
            open(&ended(11), 4),
            [
                header.clone(),
                nothing.clone(),
                fold.clone(),
                "   +11 older".into()
            ]
        );
        assert_eq!(open(&ended(11), 3), [header, nothing, fold]);
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
        // Too narrow for both counts, the header drops `N working`, then the title stands alone. The header ends one
        // column short of the pane (its right margin).
        let header = |width| plain(&sectioned(), Size { width, height: 9 })[0].clone();
        let cut = |width: usize, counts: &str| {
            format!(
                " mux-agents{}{counts}",
                " ".repeat(width - 12 - counts.chars().count())
            )
        };
        // Each count fits from 14 columns beyond its width (margin, title, two spaces, right margin).
        let full = "2 inbox \u{b7} 1 working";
        assert_eq!(header(33), cut(33, full));
        assert_eq!(header(32), cut(32, "2 inbox"));
        assert_eq!(header(21), cut(21, "2 inbox"));
        assert_eq!(header(20), " mux-agents");
        // With nobody in the inbox: the working count, then the title alone.
        let mut calm = sectioned();
        calm["agents"][0]["activity"] = json!("working");
        calm["agents"][1]["activity"] = json!("working");
        calm["agents"][1]["unread"] = json!(0);
        let header = |width| plain(&calm, Size { width, height: 9 })[0].clone();
        assert_eq!(header(23), cut(23, "3 working"));
        assert_eq!(header(22), " mux-agents");
        // Short of room the model goes first, then the name is cut; an Inbox row keeps its word. Wider, the model is
        // kept (the live agents' models differ).
        let rows = body(&long, W40);
        assert!(rows[3].starts_with(" ! a-very-long-agent"), "{rows:?}");
        assert!(
            rows[3].ends_with("needs you  12s") && !rows[3].contains("[claude"),
            "{}",
            rows[3]
        );
        let rows = body(
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
        // Doing lines of wide characters or combining marks are cut by display columns: agents[1] (with mail) is in
        // Inbox, its doing line row 4; agents[0] is in Working, its doing line row 8.
        let mut wide = mock();
        wide["agents"][0]["doing"] = json!("\u{754c}".repeat(30));
        wide["agents"][1]["doing"] = json!("e\u{301}".repeat(40));
        let rows = body(&wide, W40);
        assert_eq!(rows[8], format!("   {}...", "\u{754c}".repeat(16)));
        assert_eq!(rows[4], format!("   {}...", "e\u{301}".repeat(33)));
        assert_eq!(
            (crate::paint::width(&rows[8]), crate::paint::width(&rows[4])),
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
        assert_eq!(rows[4], format!("   {}...", coder.repeat(8)));
        assert_eq!(sample_columns(&rows[4]), 38);
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
                rows[4],
                format!("   {}...", sample.repeat(kept)),
                "{sample:?}"
            );
            assert_eq!(sample_columns(&rows[4]), columns, "{sample:?}");
        }
        // A name of rockets is cut by columns too, and every row of every sample fits each width.
        let rockets = json!({"running": 1, "endedTotal": 0, "agents": [
            {"name": "\u{1f680}".repeat(20), "state": "running", "doing": "\u{fe10}".repeat(40), "age": 5, "unread": 0}
        ]});
        // 38 columns under the Working heading: glyph and space (2), the name cut to 30 (13 rockets and `...`, 29),
        // 3 spaces, the age in 4.
        assert_eq!(
            plain(&rockets, W40)[3],
            format!(" \u{25c9} {}...     5s", "\u{1f680}".repeat(13))
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
        // 6 live agents (w6 oldest) and 3 ended at H = 10: blank rows go, the heading goes (each row keeps its word),
        // then the doing lines of the oldest four; the ended fold line stays.
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
                pad("mux-agents", "6 working"),
                live(1),
                "   doing 1".to_string(),
                live(2),
                "   doing 2".to_string(),
                live(3),
                live(4),
                live(5),
                live(6),
                " \u{25b8} Ended \u{b7} 3".to_string(),
            ]
        );
        // 1 live and 1 ended at H = 3, 2, 1: the live agent folds into `+1 more running`, then the fold line goes; the
        // header is the last row kept.
        let board = json!({"running": 1, "endedTotal": 1, "agents": [
            {"name": "w", "state": "running", "doing": "x", "age": 5, "unread": 0},
            {"name": "e", "state": "stopped", "age": 60}
        ]});
        let header = pad("mux-agents", "1 working");
        let at = |height| plain(&board, Size { width: 40, height });
        assert_eq!(at(7).len(), 7);
        assert_eq!(
            at(3),
            [
                header.clone(),
                pad("\u{25c9} w", "working   5s"),
                " \u{25b8} Ended \u{b7} 1".to_string()
            ]
        );
        assert_eq!(at(2), [header.clone(), "   +1 more running".to_string()]);
        assert_eq!(at(1), [header]);
        // The mock-up at every height keeps the header first and never more rows than the pane.
        for height in 1..=20 {
            let rows = plain(&sectioned(), Size { width: 40, height });
            assert!(rows.len() <= height, "{height}");
            assert_eq!(rows[0], pad("mux-agents", "2 inbox \u{b7} 1 working"));
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
        // The header: the inbox count in yellow, then the working count, joined by a dim dot.
        assert!(
            rows[0].ends_with(&format!(
                "{}{}1 working",
                sgr("33", "2 inbox"),
                sgr("2", " \u{b7} ")
            )),
            "{:?}",
            rows[0]
        );
        // Only Inbox stands out: its heading bold yellow; Working bold green; each count dim.
        assert_eq!(
            rows[2],
            format!(" {}{}", sgr("1;33", "Inbox"), sgr("2", " \u{b7} 2"))
        );
        assert_eq!(
            rows[8],
            format!(" {}{}", sgr("1;32", "Working"), sgr("2", " \u{b7} 1"))
        );
        // Needs you: a bold yellow `!`, the model dim, and the word reversed, herdr's pill: an Inbox row keeps it.
        starts(
            3,
            format!(
                " {} {}{}",
                sgr("1;33", "!"),
                sgr("34", "add-subtract"),
                sgr("2", " [sonnet]")
            ),
        );
        assert!(
            rows[3].ends_with(&format!("{}  12s", sgr("1;7;33", "needs you"))),
            "{:?}",
            rows[3]
        );
        assert_eq!(
            rows[4],
            format!(
                "   {}",
                sgr("2", "Adding subtract(a, b) under add in calc.py")
            )
        );
        // Done: a bold cyan `✦` and its word.
        starts(
            5,
            format!(
                " {} {}  {}",
                sgr("1;36", "\u{2726}"),
                sgr("35", "test-calc"),
                sgr("1;34", "\u{2709} 2")
            ),
        );
        assert!(
            rows[5].ends_with(&format!("{}  30s", sgr("1;36", "done"))),
            "{:?}",
            rows[5]
        );
        // Under Working the row has no word: the heading names the state.
        starts(
            9,
            format!(" {} {}", sgr("32", "\u{25c9}"), sgr("94", "fix-ci-flake")),
        );
        assert!(
            rows[9].ends_with(" 11m") && !rows[9].contains("working"),
            "{:?}",
            rows[9]
        );
        // The ended fold: one dim line.
        assert_eq!(
            rows[12],
            format!(" {}", sgr("2", "\u{25b8} Ended \u{b7} 7"))
        );
        // Open, its rows are quiet: glyph, name and age dim; only `exit N` keeps its word, in red. spike-rmux ended
        // 10 m ago, so the board feeds it first of the ended rows (newest end first).
        let mut opened = board.clone();
        let agents = opened["agents"].as_array_mut().unwrap();
        let mut spike = agents.remove(5);
        spike["age"] = json!(600);
        spike["endedAgo"] = json!(600);
        agents.insert(3, spike);
        let open: Vec<String> = (lay_open(opened.as_object(), size, color).rows.iter())
            .map(|r| render(r, true))
            .collect();
        assert_eq!(
            open[12],
            format!(" {}", sgr("2", "\u{25be} Ended \u{b7} 7"))
        );
        assert!(
            open[13].starts_with(&format!(" {} {}", sgr("2", "-"), sgr("2", "spike-rmux"))),
            "{:?}",
            open[13]
        );
        assert!(
            open[13].ends_with(&sgr("2", " 10m")) && !open[13].contains("stopped"),
            "{:?}",
            open[13]
        );
        assert!(open[14].starts_with(&format!(
            " {} {}",
            sgr("31", "\u{2717}"),
            sgr("2", "docs-usage")
        )));
        assert!(open[14].contains(&sgr("31", "exited 0")), "{:?}", open[14]);
        assert!(
            open[15].contains(&sgr("31", "exited 101")) && open[15].ends_with(&sgr("2", "  25m"))
        );
        assert_eq!(open[16], format!("   {}", sgr("2", "+4 older")));
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
        let glyphs: Vec<&str> = [3, 5, 9].iter().map(|&at| &rows[at][1..2]).collect();
        assert_eq!(glyphs, ["!", "@", "*"]);
        assert!(rows[5].contains("test-calc  m 2"), "{}", rows[5]);
        assert!(rows[4].ends_with("..."));
        assert_eq!(rows[0], pad("mux-agents", "2 inbox - 1 working"));
        assert_eq!(rows[2], " Inbox - 2");
        assert_eq!(rows[12], " > Ended - 7");
        assert_eq!(rows.last().unwrap(), " enter open  p peek  m mail  ? keys");
        assert!(rows.iter().all(|r| r.is_ascii()), "{rows:?}");
        // Open: `v`, and the ended glyphs.
        let open: Vec<String> = (lay_open(board.as_object(), W40, ascii).rows.iter())
            .map(|r| render(r, false))
            .collect();
        assert_eq!(open[12], " v Ended - 7");
        assert_eq!([&open[13][1..2], &open[14][1..2]], ["x", "x"]);
        assert!(open.iter().all(|r| r.is_ascii()), "{open:?}");
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
        Target::new(name, pane)
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
            rows[..10],
            [
                pad("mux-agents", "2 working"),
                String::new(),
                " Working \u{b7} 2".to_string(),
                pad("\u{25c9} worker [\u{2022}x]", "5s"),
                "   \u{2022} step \u{2022} two".to_string(),
                pad("\u{25c9} b\u{2022}t [\u{2022}m]", "7s"),
                String::new(),
                " \u{25b8} Ended \u{b7} 1".to_string(),
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
            [
                target("worker", "%1"),
                target("b\u{25c9}t", "%2"),
                Target::fold()
            ]
        );
    }

    #[test]
    fn lay_lists_drawn_running_agents() {
        let board = mock_panes();
        let laid = lay(board.as_object(), &[], W40, PLAIN_LOOK);
        // test-calc has mail: Inbox first; then Working; then the ended fold line, a target of keys.
        assert_eq!(
            laid.agents,
            [
                (target("test-calc", "%2"), 3),
                (target("add-subtract", "%1"), 7),
                (target("fix-ci-flake", "%3"), 9),
                (Target::fold(), 12),
            ]
        );
        let n = laid.rows.len();
        assert_eq!(
            layout(board.as_object(), &[], W40, PLAIN_LOOK)[..n],
            laid.rows[..]
        );
        // Four rows: the newest agent, then `+2 more running`, which is no agent, then the fold line.
        let short = Size {
            width: 40,
            height: 4,
        };
        let laid = lay(board.as_object(), &[], short, PLAIN_LOOK);
        assert_eq!(
            laid.agents,
            [(target("add-subtract", "%1"), 1), (Target::fold(), 3)]
        );
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
        // A closed fold lists no ended agent: its line is the one target.
        let ended = json!({"running": 0, "endedTotal": 1, "agents": [
            {"name": "halted", "state": "stopped", "exitCode": null, "age": 60, "pane": "%4"}
        ]});
        assert_eq!(
            lay(ended.as_object(), &[], W40, PLAIN_LOOK).agents,
            [(Target::fold(), 4)]
        );
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
        assert_eq!(laid.agents, [(target("worker", "%7"), 3)]);
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
    fn drawn_rows_hit_their_own_agent_and_nothing_else() {
        let text = |lead: &str| format!("{lead}\u{25c9} x  \u{25cc} y  * z  x w  ").repeat(6);
        let board = json!({"running": 2, "endedTotal": 1, "agents": [
            {"name": "add-subtract", "state": "running", "model": "\u{25c9} x", "doing": text(""), "age": 42, "pane": "%1"},
            {"name": "test-calc", "state": "running", "doing": text("x "), "age": 180, "unread": 1, "pane": "%2"},
            {"name": "\u{25c9} x", "state": "exited", "exitCode": 0, "age": 1080}]});
        for (width, height) in [(40, 40), (40, 8), (30, 24), (18, 24)] {
            let laid = lay(board.as_object(), &[], Size { width, height }, PLAIN_LOOK);
            let lines: Vec<String> = laid.rows.iter().map(|r| render(r, false)).collect();
            let answer = format!("{width}\n{}\n", lines.join("\n"));
            let hits = drawn(&laid);
            for at in 0..lines.len() {
                let want = (laid.agents.iter().map(|(t, row)| (t.id.as_str(), *row)))
                    .chain(laid.more.iter().map(|(id, row)| (id.as_str(), *row)))
                    .find(|(_, row)| *row == at)
                    .map(|(id, _)| id);
                let y = u16::try_from(at + 1).unwrap();
                assert_eq!(
                    pick::frame_hit(&answer, width, y, &hits),
                    want,
                    "{width}x{height} {at}"
                );
            }
        }
    }

    #[test]
    fn a_click_on_an_agents_doing_line_hits_that_agent() {
        // Two agents doing the same thing: each doing line still hits its own agent, by row.
        let board = json!({"running": 2, "endedTotal": 1, "agents": [
            {"name": "first", "state": "running", "doing": "running the tests", "age": 42, "pane": "%1"},
            {"name": "second", "state": "running", "doing": "running the tests", "age": 50, "pane": "%2"},
            {"name": "gone", "state": "exited", "exitCode": 0, "age": 1080}]});
        let laid = lay(board.as_object(), &[], W40, PLAIN_LOOK);
        let lines: Vec<String> = laid.rows.iter().map(|r| render(r, false)).collect();
        let answer = format!("40\n{}\n", lines.join("\n"));
        let hits = drawn(&laid);
        let hit = |text: &str, nth: usize| {
            let at = (lines.iter().enumerate())
                .filter(|(_, l)| l.contains(text))
                .nth(nth)
                .map(|(at, _)| at)
                .unwrap_or_else(|| panic!("{text} in {lines:?}"));
            pick::frame_hit(&answer, 40, u16::try_from(at + 1).unwrap(), &hits)
        };
        assert_eq!(hit("first", 0), Some("first"));
        assert_eq!(hit("running the tests", 0), Some("first"));
        assert_eq!(hit("second", 0), Some("second"));
        assert_eq!(hit("running the tests", 1), Some("second"));
        // The header and a heading hit nothing; the fold line is a target of keys, never of clicks.
        assert_eq!(hit("mux-agents", 0), None);
        assert_eq!(hit("Working", 0), None);
        assert_eq!(hit("Ended", 0), Some("fold:ended"));
        assert!(!member_row(&Target::fold()));
    }

    #[test]
    fn hint_is_dropped_first() {
        let board = mock_panes();
        let hint = |height: usize| {
            let size = Size { width: 40, height };
            let bare = lay(board.as_object(), &[], size, PLAIN_LOOK);
            (with_hint(board.as_object(), &[], size, PLAIN_LOOK), bare)
        };
        // The mock-up draws 13 rows: with 15, the hint is the last row, after a blank row.
        let (hinted, bare) = hint(15);
        assert_eq!(bare.rows.len(), 13);
        assert_eq!(hinted.rows[..13], bare.rows[..]);
        assert_eq!(hinted.rows[13], Row::new());
        assert_eq!(render(&hinted.rows[14], false), format!(" {HINT}"));
        assert_eq!(hinted.agents, bare.agents);
        // Taller, it stays on the pane's last row.
        let (hinted, _) = hint(20);
        assert_eq!(hinted.rows.len(), 20);
        assert_eq!(render(&hinted.rows[19], false), format!(" {HINT}"));
        // One row short: the bar as it is without the hint.
        let (hinted, bare) = hint(14);
        assert_eq!(hinted, bare);
        // One agent: at 40x4 and 40x6 the bare rows leave no two rows for the hint and its blank row: no hint, and
        // no other row gives way for it.
        let one = json!({"running": 1, "endedTotal": 0, "agents": [
            {"name": "worker", "state": "running", "doing": "testing", "age": 5, "unread": 0, "color": "blue", "waiting": false, "pane": "%1"}
        ]});
        for height in [4, 6] {
            let size = Size { width: 40, height };
            let bare = lay(one.as_object(), &[], size, PLAIN_LOOK);
            assert_eq!(
                with_hint(one.as_object(), &[], size, PLAIN_LOOK),
                bare,
                "{height}"
            );
        }
        let size = Size {
            width: 40,
            height: 7,
        };
        let hinted = with_hint(one.as_object(), &[], size, PLAIN_LOOK);
        assert_eq!(hinted.rows.len(), 7);
        assert_eq!(
            render(hinted.rows.last().unwrap(), false),
            format!(" {HINT}")
        );
    }

    #[test]
    fn a_short_pane_of_8_rows_or_more_keeps_the_hint() {
        let board = hub_board();
        let focus = tree::Focus::default();
        for height in [8, 10, 12] {
            let size = Size { width: 40, height };
            let mut dressed = lay_hinted(board.as_object(), &[], size, PLAIN_LOOK, focus);
            dress(
                &mut dressed,
                &[],
                size,
                PLAIN_LOOK,
                Dress::default(),
                &Keymap::default(),
            );
            let rows: Vec<String> = dressed.rows.iter().map(|r| render(r, false)).collect();
            assert_eq!(rows.len(), height, "{height}: {rows:#?}");
            assert!(rows[height - 1].contains("? keys"), "{height}: {rows:#?}");
            // The frame gave way, dropping headings and doing lines first: the header and the agent who needs the
            // person stay.
            assert!(rows[0].contains("mux-agents"), "{height}: {rows:#?}");
            assert!(
                rows.iter().any(|r| r.contains("qa-spec-audit")),
                "{height}: {rows:#?}"
            );
        }
        // Below 8 rows the frame keeps its rows and goes without the hint.
        let size = Size {
            width: 40,
            height: 7,
        };
        assert_eq!(
            lay_hinted(board.as_object(), &[], size, PLAIN_LOOK, focus),
            lay_focus(board.as_object(), &[], size, PLAIN_LOOK, focus).0
        );
        // A tall pane lays the frame once, unchanged.
        let size = Size {
            width: 40,
            height: 40,
        };
        assert_eq!(
            lay_hinted(board.as_object(), &[], size, PLAIN_LOOK, focus),
            lay_focus(board.as_object(), &[], size, PLAIN_LOOK, focus).0
        );
    }

    #[test]
    fn hint_shows_beside_notes() {
        let one = json!({"running": 1, "endedTotal": 0, "agents": [
            {"name": "worker", "state": "running", "doing": "testing", "age": 5, "unread": 0, "color": "blue", "waiting": false, "pane": "%1"}
        ]});
        let notes = [("refresh failed", DIM)];
        let size = |height| Size { width: 40, height };
        // The notes stay as they are; a blank row, then the hint.
        let bare = lay(one.as_object(), &notes, size(9), PLAIN_LOOK);
        let hinted = with_hint(one.as_object(), &notes, size(9), PLAIN_LOOK);
        assert_eq!(bare.rows.len(), 7);
        assert_eq!(hinted.rows.len(), 9);
        assert_eq!(hinted.rows[..7], bare.rows[..]);
        assert_eq!(hinted.rows[7], Row::new());
        assert_eq!(render(&hinted.rows[8], false), format!(" {HINT}"));
        assert_eq!(hinted.agents, bare.agents);
        // One row short: the bare one.
        let bare = lay(one.as_object(), &notes, size(8), PLAIN_LOOK);
        assert_eq!(
            with_hint(one.as_object(), &notes, size(8), PLAIN_LOOK),
            bare
        );
        // A question holds the last line: no hint.
        let asked = [(pick::ASK, PLAIN)];
        let bare = lay(one.as_object(), &asked, size(40), PLAIN_LOOK);
        assert_eq!(
            with_hint(one.as_object(), &asked, size(40), PLAIN_LOOK),
            bare
        );
    }

    // The hint names what Enter does with the selection: `⏎ resume` on an ended agent on every bar, a team's included
    // (Enter resumes it there as on the flat bar), its folded rows too; `open` on anything else.
    #[test]
    fn the_hint_says_resume_wherever_enter_resumes() {
        let ended = json!({"state": "stopped", "life": "ended", "pane": null});
        let team = tree::tests::team(vec![
            tree::tests::member("L", None, 0, json!({})),
            tree::tests::member("w", Some("L"), 1, ended.clone()),
        ]);
        let walk = tree::walk(&team);
        assert!(enter_resumes(&walk, Some("w")), "a team's ended row");
        assert!(!enter_resumes(&walk, Some("L")), "a running row opens");
        assert!(!enter_resumes(&walk, None));
        assert!(!enter_resumes(&walk, Some("gone")));
        let walk = [Target::fold(), Target::new("x", "%9")];
        assert!(!enter_resumes(&walk, Some(Target::fold().id.as_str())));
        assert!(!enter_resumes(&walk, Some("x")));
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
        let last = |width: usize, look: Look| {
            let size = Size { width, height: 40 };
            let hinted = with_hint(one.as_object(), &[], size, look);
            render(hinted.rows.last().unwrap(), look.color)
        };
        // Keys plain, words dim.
        let dim = |text: &str| format!("\u{1b}[2m{text}\u{1b}[0m");
        assert_eq!(
            last(40, color),
            format!(
                " \u{23ce}{}  p{}  m{}  ?{}",
                dim(" open"),
                dim(" peek"),
                dim(" mail"),
                dim(" keys")
            )
        );
        // Narrow, `m mail` goes first, then `p peek`; `? keys` stays.
        assert_eq!(last(30, PLAIN_LOOK), " \u{23ce} open  p peek  ? keys");
        assert_eq!(last(20, PLAIN_LOOK), " \u{23ce} open  ? keys");
        // On a selected ended row Enter resumes it; ASCII writes `enter`.
        let row = |enter, look| render(&hint_row(38, look, enter, &Keymap::default()), false);
        assert_eq!(
            row(EnterHint::Resume, PLAIN_LOOK),
            " \u{23ce} resume  p peek  m mail  ? keys"
        );
        let ascii = Look {
            ascii: true,
            ..PLAIN_LOOK
        };
        assert_eq!(
            row(EnterHint::Open, ascii),
            " enter open  p peek  m mail  ? keys"
        );
        // While that row's resume runs, Enter starts nothing: its item is `resuming…` alone.
        assert_eq!(
            row(EnterHint::Resuming, PLAIN_LOOK),
            " resuming\u{2026}  p peek  m mail  ? keys"
        );
        assert_eq!(
            row(EnterHint::Resuming, ascii),
            " resuming...  p peek  m mail  ? keys"
        );
        // `@mux-agents-keys`: the hint shows each action's own key; an action left without one is left out.
        let moved = |value: &str| {
            render(
                &hint_row(38, PLAIN_LOOK, EnterHint::Open, &Keymap::parse(value)),
                false,
            )
        };
        assert_eq!(
            moved("peek=v,help=h"),
            " \u{23ce} open  v peek  m mail  h keys"
        );
        assert_eq!(moved("notes=p"), " \u{23ce} open  m mail  ? keys");
    }

    // The hub (slice 1): agents grouped by what they ask of the person, as the design brief draws them at 40 columns.
    fn hub_board() -> Value {
        json!({"running": 4, "endedTotal": 48, "agents": [
            {"id": "aaaaaaaaaaaa", "name": "qa-spec-audit", "state": "running", "activity": "blocked", "since": 12,
             "age": 60, "pane": "%2", "parent": "%1", "doing": "Allow edit to src/lexer.rs?"},
            {"id": "bbbbbbbbbbbb", "name": "fc-research", "state": "running", "activity": "done", "since": 180,
             "age": 300, "pane": "%3", "parent": "%1", "unread": 1},
            {"id": "cccccccccccc", "name": "dev-fix-l2", "state": "running", "activity": "working", "since": 240,
             "age": 400, "pane": "%4", "parent": "%1", "doing": "Splitting parse.rs into modules"},
            {"id": "dddddddddddd", "name": "docs-usage", "state": "running", "activity": "idle", "since": 1200,
             "age": 1300, "pane": "%5", "parent": "%1"},
            {"id": "eeeeeeeeeeee", "name": "qa-fix-l1-2", "state": "stopped", "age": 540, "endedAgo": 540, "parent": "%1"},
            {"id": "ffffffffffff", "name": "bump-deps", "state": "exited", "exitCode": 101, "age": 560, "endedAgo": 560, "parent": "%1"}
        ]})
    }

    #[test]
    fn the_hub_groups_inbox_working_free_and_folds_ended() {
        let rows = plain(
            &hub_board(),
            Size {
                width: 40,
                height: 30,
            },
        );
        let at = |needle: &str| {
            rows.iter()
                .position(|r| r.contains(needle))
                .unwrap_or_else(|| panic!("{needle}: {rows:?}"))
        };
        // Header: inbox and working counts, no `free`, no `ended` (the fold line counts those).
        assert!(
            rows[0].starts_with(" mux-agents")
                && rows[0].contains("2 inbox")
                && rows[0].contains("1 working"),
            "{rows:?}"
        );
        assert!(
            !rows[0].contains("free") && !rows[0].contains("ended"),
            "{rows:?}"
        );
        // main, then Inbox (needs you, then done with mail), Working, Free; each heading with its count.
        let (main, inbox, working, free, ended) = (
            at(" main"),
            at("Inbox \u{b7} 2"),
            at("Working \u{b7} 1"),
            at("Free \u{b7} 1"),
            at("Ended \u{b7} 48"),
        );
        assert!(
            main < inbox && inbox < working && working < free && free < ended,
            "{rows:?}"
        );
        assert!(
            at("qa-spec-audit") > inbox && at("fc-research") > inbox && at("fc-research") < working
        );
        assert!(at("dev-fix-l2") > working && at("dev-fix-l2") < free && at("docs-usage") > free);
        // Inbox rows keep their words (their reasons differ); Working and Free rows show glyph and age only.
        assert!(rows[at("qa-spec-audit")].contains("needs you"), "{rows:?}");
        assert!(
            !rows[at("dev-fix-l2")].contains("working") && !rows[at("docs-usage")].contains("idle"),
            "{rows:?}"
        );
        // Ended is one folded, selectable line, a blank row above the key hints, which are the last row.
        assert_eq!(
            rows[ended].trim_start(),
            "\u{25b8} Ended \u{b7} 48",
            "{rows:?}"
        );
        assert!(
            !rows.iter().any(|r| r.contains("qa-fix-l1-2")),
            "folded: {rows:?}"
        );
        let last = rows.last().unwrap();
        assert!(last.contains("open") && last.contains("? keys"), "{rows:?}");
        assert!(rows[rows.len() - 2].is_empty(), "{rows:?}");
    }

    // The hub keys on the flat bar: `g` and `G` select the top and bottom rows as drawn (not the walk's ends: the main
    // row is drawn first, walked last); Enter on the fold line and Tab open and close the ended fold; `?` shows the keys
    // and `?` or Esc closes them; any other key closes them and does its own action.
    #[test]
    fn hub_keys_select_the_edges_fold_ended_and_show_the_keys() {
        let w = World::human("");
        let c = w.caller();
        let board = hub_board();
        let shown_at = |open: bool| {
            let focus = tree::Focus {
                ended_open: open,
                ..tree::Focus::default()
            };
            let size = Size {
                width: 40,
                height: 30,
            };
            let laid = lay_focus(board.as_object(), &[], size, PLAIN_LOOK, focus).0;
            Shown {
                written: SystemTime::UNIX_EPOCH,
                targets: walk_of(board.as_object(), &laid, false),
                drawn: drawn(&laid),
                width: 40,
                looking: None,
                team: false,
                done: Vec::new(),
            }
        };
        let press = |event: Event, pick: &mut Pick, shown: &mut Shown, panel: &mut Panel| {
            let read = keys::Read {
                event,
                first: SystemTime::UNIX_EPOCH,
                alone: true,
            };
            assert!(act(&w.ctx, &c, PLAIN_LOOK, read, pick, shown, panel));
        };
        let (mut pick, mut panel, mut shown) = (Pick::default(), Panel::default(), shown_at(false));
        press(Event::First, &mut pick, &mut shown, &mut panel);
        assert_eq!(pick.selected(), Some("main:%1"));
        press(Event::Last, &mut pick, &mut shown, &mut panel);
        assert_eq!(pick.selected(), Some("fold:ended"));
        // Enter on the fold line opens it; the selection stays there.
        press(Event::Enter, &mut pick, &mut shown, &mut panel);
        assert!(panel.ended_open);
        assert_eq!(pick.selected(), Some("fold:ended"));
        // Open, its rows are walked: Down selects the newest ended agent.
        let mut shown = shown_at(true);
        press(Event::Down, &mut pick, &mut shown, &mut panel);
        assert_eq!(pick.selected(), Some("eeeeeeeeeeee"));
        // Tab closes it from one of its rows: the selection goes to the fold line, never to a row not chosen.
        press(Event::Tab, &mut pick, &mut shown, &mut panel);
        assert!(!panel.ended_open);
        assert_eq!(pick.selected(), Some("fold:ended"));
        let mut shown = shown_at(false);
        // `?` shows the keys, `?` closes them; Esc closes them and keeps the selection.
        press(Event::Help, &mut pick, &mut shown, &mut panel);
        assert!(panel.help);
        press(Event::Help, &mut pick, &mut shown, &mut panel);
        assert!(!panel.help);
        press(Event::Help, &mut pick, &mut shown, &mut panel);
        press(Event::Esc, &mut pick, &mut shown, &mut panel);
        assert!(!panel.help);
        assert_eq!(pick.selected(), Some("fold:ended"));
        // Any other key closes them and does its action: Up moves to the row above the fold line.
        press(Event::Help, &mut pick, &mut shown, &mut panel);
        press(Event::Up, &mut pick, &mut shown, &mut panel);
        assert!(!panel.help);
        assert_eq!(pick.selected(), Some("dddddddddddd"));
        // Without the keys shown, Esc clears the selection.
        press(Event::Esc, &mut pick, &mut shown, &mut panel);
        assert_eq!(pick.selected(), None);
    }

    #[test]
    fn the_key_help_takes_the_last_rows() {
        let board = hub_board();
        let with = |width: usize, height: usize, keys: &Keymap| -> Vec<String> {
            let size = Size { width, height };
            let mut laid = lay(board.as_object(), &[], size, PLAIN_LOOK);
            let help = Dress {
                help: true,
                enter: EnterHint::Open,
            };
            dress(&mut laid, &[], size, PLAIN_LOOK, help, keys);
            laid.rows.iter().map(|r| render(r, false)).collect()
        };
        let at = |width: usize, height: usize| with(width, height, &Keymap::default());
        // 40 columns: the title, then two keys to a row, in the pane's last rows; the frame above keeps its rows.
        let rows = at(40, 30);
        assert_eq!(rows.len(), 30);
        assert_eq!(
            rows[..17],
            body(
                &board,
                Size {
                    width: 40,
                    height: 30
                }
            )[..]
        );
        assert_eq!(
            rows[22..],
            [
                pad("Keys", "? or Esc closes"),
                " Enter open        j k   move".to_string(),
                " g G   first/last  n N   next/prev".to_string(),
                " p     peek        o     output".to_string(),
                " m     mail        i     notes".to_string(),
                " P     placement   Space mark seen".to_string(),
                " Tab   fold ended  q     close bar".to_string(),
                " Esc   clear       ?     keys".to_string(),
            ]
        );
        // Below 40 columns, one key to a row.
        let rows = at(30, 30);
        assert_eq!(
            rows[15..],
            [
                format!(" Keys{}? or Esc closes", " ".repeat(9)),
                " Enter open".to_string(),
                " j k   move".to_string(),
                " g G   first/last".to_string(),
                " n N   next/prev".to_string(),
                " p     peek".to_string(),
                " o     output".to_string(),
                " m     mail".to_string(),
                " i     notes".to_string(),
                " P     placement".to_string(),
                " Space mark seen".to_string(),
                " Tab   fold ended".to_string(),
                " q     close bar".to_string(),
                " Esc   clear".to_string(),
                " ?     keys".to_string(),
            ]
        );
        // `@mux-agents-keys`: each action shows its own key, one that lost its key to another shows the one it keeps,
        // and each entry ignored is listed last, dim.
        let rows = with(30, 30, &Keymap::parse("help=h,notes=j,stop=x"));
        assert_eq!(rows[14], format!(" Keys{}h or Esc closes", " ".repeat(9)));
        assert!(rows.contains(&" k     move".to_string()), "{rows:?}");
        assert!(rows.contains(&" j     notes".to_string()), "{rows:?}");
        assert!(rows.contains(&" h     keys".to_string()), "{rows:?}");
        assert_eq!(rows.last().map(String::as_str), Some(" ignored: stop"));
        // A short pane: the keys alone, cut to its height.
        let rows = at(40, 4);
        assert_eq!(rows.len(), 4);
        assert!(rows[0].starts_with(" Keys"), "{rows:?}");
    }

    #[test]
    fn the_model_shows_only_when_the_live_agents_models_differ() {
        let board = |second: &str| {
            json!({"running": 2, "endedTotal": 0, "agents": [
                {"name": "a", "state": "running", "model": "opus", "activity": "working", "age": 5},
                {"name": "b", "state": "running", "model": second, "activity": "working", "age": 6}]})
        };
        let same = body(&board("opus"), W40);
        assert!(!same.iter().any(|r| r.contains("[opus]")), "{same:?}");
        let mixed = body(&board("sonnet"), W40);
        assert!(
            mixed[3].contains("a [opus]") && mixed[4].contains("b [sonnet]"),
            "{mixed:?}"
        );
    }

    // Enter on an ended row resumes it by id as a tab (the hub, slice 1); a live row is focused as before; a stopped
    // row whose resume is refused falls back to its retained output.
    #[test]
    fn enter_resumes_an_ended_row_and_focuses_a_live_one() {
        let row = |life: &str| Target {
            life: life.into(),
            ..Target::new("aaaaaaaaaaaa", "%9")
        };
        assert_eq!(enter_action(&row("ended")), EnterAction::Resume);
        for life in ["running", "lost", "unknown"] {
            assert_eq!(enter_action(&row(life)), EnterAction::Open, "{life}");
        }
        assert_eq!(enter_action(&row(pick::FOLD)), EnterAction::Fold);
        assert_eq!(enter_action(&row(pick::KEPT)), EnterAction::Ask);
        assert_eq!(enter_action(&row(pick::MAIN)), EnterAction::Open);
    }

    // `m`: the selected row's unread mail as the popup's text, oldest first, each as `from · at` then its body; none
    // is `no unread mail`. Read-only: nothing is acked.
    #[test]
    fn mail_text_lists_each_report_under_its_sender() {
        use crate::agents::mail::Report;
        let r = |from: &str, at: &str, body: &str| Report {
            from: from.into(),
            at: at.into(),
            body: body.into(),
        };
        assert_eq!(mail_text(&[]), "no unread mail\n");
        let text = mail_text(&[
            r("qa-1", "2026-11-20T05:00:00Z", "two bugs\nsee findings.md"),
            r("qa-1", "2026-11-20T05:01:00Z", "done"),
        ]);
        assert_eq!(
            text,
            "qa-1 \u{b7} 2026-11-20T05:00:00Z\ntwo bugs\nsee findings.md\n\nqa-1 \u{b7} 2026-11-20T05:01:00Z\ndone\n"
        );
        // Controls in a body never reach the terminal.
        assert!(!mail_text(&[r("x", "t", "a\u{1b}[31mred")]).contains('\u{1b}'));
    }

    #[test]
    fn a_closed_ended_fold_is_one_line() {
        let rows = plain(
            &hub_board(),
            Size {
                width: 40,
                height: 30,
            },
        );
        let fold = rows
            .iter()
            .position(|r| r.contains("Ended \u{b7} 48"))
            .unwrap();
        // Closed, the fold names no ended agent and no `+N older`: the blank row and the hint row follow it.
        assert!(!rows.iter().any(|r| r.contains("older")), "{rows:?}");
        assert!(rows[fold + 1].is_empty(), "{rows:?}");
    }

    #[test]
    fn an_open_ended_fold_lists_the_newest_and_folds_the_older() {
        let laid = lay_open(
            hub_board().as_object(),
            Size {
                width: 40,
                height: 30,
            },
            PLAIN_LOOK,
        );
        let rows: Vec<String> = laid.rows.iter().map(|r| render(r, false)).collect();
        let ended = rows
            .iter()
            .position(|r| r.contains("Ended \u{b7} 48"))
            .unwrap();
        assert_eq!(
            rows[ended].trim_start(),
            "\u{25be} Ended \u{b7} 48",
            "{rows:?}"
        );
        assert!(
            rows[ended + 1].contains("qa-fix-l1-2") && rows[ended + 2].contains("bump-deps"),
            "{rows:?}"
        );
        // Only `exit N` keeps a word; the rest of the 48 fold into `+N older`.
        assert!(
            rows[ended + 2].contains("exit 101") && !rows[ended + 1].contains("stopped"),
            "{rows:?}"
        );
        assert!(
            rows[ended + 3].trim_start().starts_with("+46 older"),
            "{rows:?}"
        );
        // Each ended row is selectable: a target of its own id.
        assert!(
            laid.agents
                .iter()
                .any(|(t, _)| t.id == "eeeeeeeeeeee" && t.life != "running")
        );
    }

    #[test]
    fn the_flat_bar_shows_the_main_chat_first_and_walks_to_it_last() {
        // worker and writer were spawned from %3; an ended agent from %3 and one from %4.
        let board = json!({"running": 2, "endedTotal": 2, "agents": [
            {"id": "aaaaaaaaaaaa", "name": "worker", "state": "running", "doing": "testing", "age": 240, "pane": "%1", "parent": "%3"},
            {"id": "bbbbbbbbbbbb", "name": "writer", "state": "running", "age": 180, "pane": "%2", "parent": "%3"},
            {"id": "cccccccccccc", "name": "done", "state": "exited", "exitCode": 0, "age": 300, "parent": "%4"},
            {"id": "dddddddddddd", "name": "nested", "state": "exited", "exitCode": 0, "age": 400, "parent": "%1"}
        ]});
        let laid = lay(board.as_object(), &[], W40, PLAIN_LOOK);
        let text: Vec<String> = laid.rows.iter().map(|r| render(r, false)).collect();
        // Header, a blank row, then one row per main pane, the one whose oldest agent is oldest first (%4's `done`,
        // 300 s, before %3's `worker`, 240 s; %1 is an agent's own pane, so no main), then the agents.
        assert_eq!(text[1], "", "{text:?}");
        assert_eq!(text[2], format!(" {MAIN} main"), "{text:?}");
        assert_eq!(text[3], format!(" {MAIN} main 2"), "{text:?}");
        assert_eq!(text[5], " Working \u{b7} 2", "{text:?}");
        assert!(text[6].starts_with(" \u{25c9} worker"), "{text:?}");
        // The header counts agents only.
        assert_eq!(text[0], pad("mux-agents", "2 working"));
        let mains: Vec<(&str, usize)> = (laid.agents.iter())
            .filter(|(t, _)| t.life == pick::MAIN)
            .map(|(t, at)| (t.pane.as_str(), *at))
            .collect();
        assert_eq!(mains, [("%4", 2), ("%3", 3)]);
        // A click on a main row hits it; the walk ends with the main rows, so `j` still starts at the first agent. The
        // fold line is walked in its place (keys open it).
        let walk = walk_of(board.as_object(), &laid, false);
        let names: Vec<&str> = walk.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["worker", "writer", "Ended", "main", "main 2"]);
        // ASCII draws `$`; the shown mark and the chevron land on a main row as on any other.
        let ascii = Look {
            ascii: true,
            ..PLAIN_LOOK
        };
        let rows = lay(board.as_object(), &[], W40, ascii);
        assert_eq!(render(&rows.rows[2], false), " $ main");
        let focus = tree::Focus {
            shown: Some("%3"),
            selected: None,
            ended_open: false,
        };
        let shown = lay_focus(board.as_object(), &[], W40, PLAIN_LOOK, focus).0;
        // %3 is `main 2`, row 3: banded in color, its text unchanged.
        let color = Look {
            color: true,
            ..PLAIN_LOOK
        };
        let lit = lay_focus(board.as_object(), &[], W40, color, focus).0;
        assert!(lit.rows[3].iter().all(|(_, s)| s.bg.is_some()));
        assert!(!lit.rows[2].iter().any(|(_, s)| s.bg.is_some()));
        assert!(render(&shown.rows[3], false).starts_with(' '));
        // No parent anywhere, or a team: no main row.
        let plain = json!({"running": 1, "agents": [{"name": "w", "state": "running", "age": 1, "pane": "%1"}]});
        assert!(
            lay(plain.as_object(), &[], W40, PLAIN_LOOK)
                .agents
                .iter()
                .all(|(t, _)| t.life != pick::MAIN)
        );
    }

    // More space for the main chat at the top: a blank row between the header and `main` when the bar has a row to
    // spare. A short bar drops it before any agent, main or ended row.
    #[test]
    fn a_blank_row_sets_main_apart_from_the_header() {
        let board = json!({"running": 1, "endedTotal": 0, "agents": [{"id": "aaaaaaaaaaaa", "name": "worker",
            "state": "running", "age": 1, "pane": "%2", "parent": "%1"}]});
        let rows = |height: usize| -> Vec<String> {
            lay(
                board.as_object(),
                &[],
                Size { width: 40, height },
                PLAIN_LOOK,
            )
            .rows
            .iter()
            .map(|r| render(r, false))
            .collect()
        };
        let tall = rows(24);
        assert!(
            tall[0].contains("mux-agents") && tall[1].is_empty(),
            "{tall:?}"
        );
        assert_eq!(tall[2], format!(" {MAIN} main"), "{tall:?}");
        let laid = lay(board.as_object(), &[], W40, PLAIN_LOOK);
        let main_at = laid
            .agents
            .iter()
            .find(|(t, _)| t.life == pick::MAIN)
            .unwrap()
            .1;
        assert_eq!(main_at, 2, "a click on row 2 hits main");
        // Exactly as tall as the rows without it: no blank row, every row kept.
        let tight = rows(tall.len() - 1);
        assert_eq!(tight[1], format!(" {MAIN} main"), "{tight:?}");
        assert!(tight.iter().any(|r| r.contains("worker")), "{tight:?}");
    }

    // The blank row above `main` takes only a spare row: with kept shells and a pending `close? y/n`, a frame exactly
    // as tall as its rows keeps the question on its last line and no blank row; one row more brings the blank row.
    #[test]
    fn the_blank_row_never_pushes_out_a_pending_question() {
        let board = json!({"running": 1, "endedTotal": 0, "agents": [{"id": "aaaaaaaaaaaa", "name": "worker",
            "state": "running", "age": 1, "pane": "%2", "parent": "%1"}],
            "kept": [{"address": "t7", "name": "old", "member": "aaaaaaaaaaaa"}]});
        let asked: &[(&str, Style)] = &[(pick::ASK, PLAIN)];
        let rows = |height: usize| -> Vec<String> {
            lay_focus(
                board.as_object(),
                asked,
                Size { width: 40, height },
                PLAIN_LOOK,
                tree::Focus::default(),
            )
            .0
            .rows
            .iter()
            .map(|r| render(r, false))
            .collect()
        };
        let roomy = rows(30);
        let at = roomy.iter().position(|r| r.contains("main")).unwrap();
        assert!(
            roomy[at - 1].is_empty() && roomy[0].contains("mux-agents"),
            "{roomy:?}"
        );
        assert!(roomy.last().unwrap().contains(pick::ASK), "{roomy:?}");
        let tight = rows(roomy.len() - 1);
        assert_eq!(tight.len(), roomy.len() - 1, "{tight:?}");
        assert!(tight.last().unwrap().contains(pick::ASK), "{tight:?}");
        assert!(
            !tight[1].is_empty(),
            "no blank row in a tight frame: {tight:?}"
        );
        assert!(tight.iter().any(|r| r.contains("worker")), "{tight:?}");
    }

    // `main` is pinned under the header like the header itself: while the pane has a row for it beside the header, it
    // is drawn there, a note or not, and the agents give way first (into `+J more running`).
    #[test]
    fn main_is_pinned_and_the_agents_give_way_first() {
        let board = json!({"running": 4, "agents": (1..=4).map(|n| json!({"id": format!("{n:0>12}"),
            "name": format!("w{n}"), "state": "running", "activity": "working", "age": n,
            "pane": format!("%{}", n + 10), "parent": "%1"})).collect::<Vec<_>>()});
        let note: &[(&str, Style)] = &[("focus failed: w1", PLAIN)];
        for notes in [&[][..], note] {
            for height in 2..=9 {
                let size = Size { width: 40, height };
                let laid = lay_focus(
                    board.as_object(),
                    notes,
                    size,
                    PLAIN_LOOK,
                    tree::Focus::default(),
                )
                .0;
                let text: Vec<String> = laid.rows.iter().map(|r| render(r, false)).collect();
                // Right under the header, or under the blank row the bar spares for it.
                let main = laid
                    .agents
                    .iter()
                    .find(|(t, _)| t.life == pick::MAIN)
                    .map(|(_, at)| *at);
                assert!(matches!(main, Some(1 | 2)), "{height} {notes:?}: {text:?}");
                let first = laid.agents.iter().map(|(_, at)| *at).min();
                assert_eq!(
                    first, main,
                    "{height} {notes:?}: no agent above main: {text:?}"
                );
            }
        }
        assert_eq!(
            lay_focus(
                board.as_object(),
                &[],
                Size {
                    width: 40,
                    height: 1
                },
                PLAIN_LOOK,
                tree::Focus::default()
            )
            .0
            .rows
            .len(),
            1,
            "the header alone"
        );
    }

    // Under a note `main` still stays: on a 3-row bar the header, `main` and the note, the agent given way (the keys
    // then walk to `main`); with room the agent comes back, and the ten-row bar with eight chats keeps its running
    // agent under a note.
    #[test]
    fn a_note_keeps_main_and_the_agent_comes_back_with_room() {
        let note: &[(&str, Style)] = &[("focus failed: worker", PLAIN)];
        let one = json!({"running": 1, "agents": [{"id": "aaaaaaaaaaaa", "name": "worker", "state": "running",
            "age": 1, "pane": "%2", "parent": "%1"}]});
        let at = |height: usize| {
            let size = Size { width: 40, height };
            lay_focus(
                one.as_object(),
                note,
                size,
                PLAIN_LOOK,
                tree::Focus::default(),
            )
            .0
        };
        let three = at(3);
        let text: Vec<String> = three.rows.iter().map(|r| render(r, false)).collect();
        assert_eq!(
            text,
            [
                pad("mux-agents", "1 working"),
                " \u{f120} main".to_string(),
                " focus failed: worker".to_string()
            ]
        );
        let walk = walk_of(one.as_object(), &three, false);
        assert_eq!(walk.first().map(|t| t.life.as_str()), Some(pick::MAIN));
        for height in 4..=6 {
            let laid = at(height);
            assert!(
                laid.agents.iter().any(|(t, _)| t.name == "worker")
                    && laid.agents.iter().any(|(t, _)| t.life == pick::MAIN),
                "{height}"
            );
        }
        // The ten-row bar with eight chats keeps its running agent under a note too.
        let eight = json!({"running": 1, "endedTotal": 7, "agents": (1..=8).map(|n| json!({"id": format!("{n:0>12}"),
            "name": format!("w{n}"), "state": if n == 1 { "running" } else { "exited" }, "exitCode": 0, "age": n,
            "pane": format!("%{}", n + 10), "parent": format!("%{n}")})).collect::<Vec<_>>()});
        let ten = lay_focus(
            eight.as_object(),
            note,
            Size {
                width: 40,
                height: 10,
            },
            PLAIN_LOOK,
            tree::Focus::default(),
        )
        .0;
        assert!(
            ten.agents.iter().any(|(t, _)| t.name == "w1"),
            "{:?}",
            ten.rows
                .iter()
                .map(|r| render(r, false))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn next_and_peek_act_on_agents_only_never_main() {
        let board = json!({"running": 2, "agents": [
            {"id": "aaaaaaaaaaaa", "name": "worker", "state": "running", "age": 240, "pane": "%1", "parent": "%3"},
            {"id": "bbbbbbbbbbbb", "name": "writer", "state": "running", "age": 180, "pane": "%2", "parent": "%3"}
        ]});
        let laid = lay(board.as_object(), &[], W40, PLAIN_LOOK);
        let walk = walk_of(board.as_object(), &laid, false);
        assert_eq!(walk.last().map(|t| t.life.as_str()), Some(pick::MAIN));
        let agents = members(&walk);
        // From the last agent `n` wraps to the first agent; from the main pane it starts at the first agent.
        let next = |looking: &str| pick::step(&agents, Some(looking), true).map(|t| t.name.clone());
        assert_eq!(next("%2").as_deref(), Some("worker"));
        assert_eq!(next("%3").as_deref(), Some("worker"));
        assert!(agents.iter().all(|t| t.life != pick::MAIN));
        // The main row selected: peek's walk has no main, so the selection is no agent to peek.
        let mut pick = Pick::default();
        pick.select(&walk, walk.len() - 1);
        assert_eq!(pick.chosen(&agents), None);
    }

    #[test]
    fn main_rows_outlive_their_agents_and_never_name_an_agents_pane() {
        // Every agent ended: their chat is still the way back. `nested` was spawned from `done`'s pane %5 (an agent
        // pane, even though it ended), so %5 is no main.
        let ended = json!({"running": 0, "endedTotal": 2, "agents": [
            {"id": "aaaaaaaaaaaa", "name": "done", "state": "exited", "exitCode": 0, "age": 300, "pane": "%5", "parent": "%3"},
            {"id": "bbbbbbbbbbbb", "name": "nested", "state": "exited", "exitCode": 0, "age": 200, "pane": "%6", "parent": "%5"}
        ]});
        let mains: Vec<String> = main_targets(ended.as_object())
            .into_iter()
            .map(|t| t.pane)
            .collect();
        assert_eq!(mains, ["%3"]);
        let laid = lay(ended.as_object(), &[], W40, PLAIN_LOOK);
        assert_eq!(render(&laid.rows[2], false), format!(" {MAIN} main"));
        // A team's bar has its leader instead.
        let team = json!({"team": true, "agents": [{"id": "L", "name": "lead", "state": "running", "leader": true, "parent": "%3"}]});
        assert!(main_targets(team.as_object()).is_empty());
        // Eight chats, one running agent and seven ended ones in a 10-row bar: the header, every main row and the
        // running agent fit (the ended rows and gaps give way); a 3-row bar keeps its one main row.
        let eight = json!({"running": 1, "endedTotal": 7, "agents": (1..=8).map(|n| json!({"id": format!("{n:0>12}"),
            "name": format!("w{n}"), "state": if n == 1 { "running" } else { "exited" }, "exitCode": 0, "age": n,
            "pane": format!("%{}", n + 10), "parent": format!("%{n}")})).collect::<Vec<_>>()});
        let ten = lay(
            eight.as_object(),
            &[],
            Size {
                width: 40,
                height: 10,
            },
            PLAIN_LOOK,
        );
        assert_eq!(
            ten.agents
                .iter()
                .filter(|(t, _)| t.life == pick::MAIN)
                .count(),
            8
        );
        assert!(
            ten.agents.iter().any(|(t, _)| t.name == "w1"),
            "the running agent stays: {:?}",
            ten.rows
                .iter()
                .map(|r| render(r, false))
                .collect::<Vec<_>>()
        );
        let one = json!({"running": 1, "agents": [{"id": "aaaaaaaaaaaa", "name": "w", "state": "running", "age": 1,
            "pane": "%2", "parent": "%1"}]});
        let three = lay(
            one.as_object(),
            &[],
            Size {
                width: 40,
                height: 3,
            },
            PLAIN_LOOK,
        );
        assert!(
            three.agents.iter().any(|(t, _)| t.life == pick::MAIN),
            "{:?}",
            three
                .rows
                .iter()
                .map(|r| render(r, false))
                .collect::<Vec<_>>()
        );
        // Every agent ended, four chats, a 4-row bar: the header stays (its counts), three main rows take the rest.
        let four = json!({"running": 0, "endedTotal": 4, "agents": (1..=4).map(|n| json!({"id": format!("{n:0>12}"),
            "name": format!("e{n}"), "state": "exited", "exitCode": 0, "age": n, "pane": format!("%{}", n + 10),
            "parent": format!("%{n}")})).collect::<Vec<_>>()});
        let tight = lay(
            four.as_object(),
            &[],
            Size {
                width: 40,
                height: 4,
            },
            PLAIN_LOOK,
        );
        let text: Vec<String> = tight.rows.iter().map(|r| render(r, false)).collect();
        assert!(text[0].starts_with(" mux-agents"), "{text:?}");
        assert!(text[0].ends_with("0 running"), "{text:?}");
        assert_eq!(
            tight
                .agents
                .iter()
                .filter(|(t, _)| t.life == pick::MAIN)
                .count(),
            3
        );
        // Many chats in a short bar: the agents keep their rows, the main rows take the rest.
        let many = json!({"running": 4, "agents": (1..=4).map(|n| json!({"id": format!("{n:0>12}"), "name": format!("w{n}"),
            "state": "running", "age": n, "pane": format!("%{}", n + 10), "parent": format!("%{n}")})).collect::<Vec<_>>()});
        let short = lay(
            many.as_object(),
            &[],
            Size {
                width: 40,
                height: 10,
            },
            PLAIN_LOOK,
        );
        let shown = short
            .agents
            .iter()
            .filter(|(t, _)| t.life == pick::MAIN)
            .count();
        assert_eq!(
            shown,
            4,
            "{:?}",
            short
                .rows
                .iter()
                .map(|r| render(r, false))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_agent_on_screen_gets_a_soft_band_not_a_glyph() {
        let board = json!({"running": 2, "endedTotal": 0, "agents": [
            {"name": "worker", "state": "running", "doing": "testing", "age": 240, "pane": "%1", "parent": "%3"},
            {"name": "writer", "state": "running", "age": 180, "pane": "%2", "parent": "%3"}
        ]});
        let color = Look {
            color: true,
            ..PLAIN_LOOK
        };
        let laid = |look: Look, shown: Option<&str>, selected: Option<&str>| {
            let focus = tree::Focus {
                shown,
                selected,
                ended_open: false,
            };
            let mut laid = lay_focus(board.as_object(), &[], W40, look, focus).0;
            if let Some(id) = selected {
                mark(&mut laid, id, 40, look);
            }
            laid
        };
        let text =
            |laid: &Laid| -> Vec<String> { laid.rows.iter().map(|r| render(r, false)).collect() };
        // Rows: header, gap, main, gap, the Working heading, worker, its doing line, writer.
        let (main, worker, doing, writer) = (2, 5, 6, 7);
        let banded = |laid: &Laid, at: usize| laid.rows[at].iter().all(|(_, s)| s.bg == Some(BAND));
        let any_band = |laid: &Laid| laid.rows.iter().flatten().any(|(_, s)| s.bg.is_some());
        let shown = laid(color, Some("%2"), None);
        assert!(banded(&shown, writer), "{:?}", shown.rows[writer]);
        assert_eq!(row_width(&shown.rows[writer]), 40, "the band spans the bar");
        for at in (0..shown.rows.len()).filter(|&at| at != writer) {
            assert!(!shown.rows[at].iter().any(|(_, s)| s.bg.is_some()), "{at}");
        }
        // No glyph: the text is the bare frame's, so clicks hit as before.
        assert!(
            text(&shown)
                .iter()
                .all(|t| !t.contains('\u{25cf}') && !t.starts_with('#'))
        );
        assert_eq!(
            text(&shown)[writer].trim_end(),
            text(&laid(color, None, None))[writer].trim_end()
        );
        // The band covers the agent's whole block, its doing line too, each row across the bar; the main row is
        // banded when its pane is on screen.
        let block = laid(color, Some("%1"), None);
        assert!(
            banded(&block, worker) && banded(&block, doing),
            "{:?}",
            text(&block)
        );
        assert_eq!(
            row_width(&block.rows[doing]),
            40,
            "the block's band spans the bar"
        );
        let lit: Vec<usize> = (0..block.rows.len())
            .filter(|&at| block.rows[at].iter().any(|(_, s)| s.bg.is_some()))
            .collect();
        assert_eq!(lit, [worker, doing], "only the block");
        // A selection takes the whole block's band too.
        let picked = laid(color, Some("%2"), Some("worker"));
        assert!(banded(&picked, worker) && banded(&picked, doing) && !banded(&picked, writer));
        assert!(banded(&laid(color, Some("%3"), None), main));
        // Without color, in ASCII, or for a pane no row draws: no band.
        assert!(!any_band(&laid(PLAIN_LOOK, Some("%2"), None)));
        assert!(!any_band(&laid(
            Look {
                ascii: true,
                ..PLAIN_LOOK
            },
            Some("%2"),
            None
        )));
        // ASCII with color (a terminal without the symbols): no band either.
        assert!(!any_band(&laid(
            Look {
                ascii: true,
                ..color
            },
            Some("%2"),
            None
        )));
        // A team bar narrower than 12 columns still bands its member on screen.
        let team = json!({"team": true, "running": 1, "agents": [{"id": "L", "name": "worker", "state": "running",
            "life": "running", "leader": true, "parentId": null, "pane": "%1", "age": 60,
            "createdAt": "2026-11-18T10:00:00Z"}]});
        let narrow = lay_focus(
            team.as_object(),
            &[],
            Size {
                width: 11,
                height: 6,
            },
            color,
            tree::Focus {
                shown: Some("%1"),
                selected: None,
                ended_open: false,
            },
        )
        .0;
        assert!(any_band(&narrow), "{:?}", narrow.rows);
        assert!(!any_band(&laid(color, Some("%9"), None)));
        // One mark: a selection takes the band from the shown row; no chevron anywhere.
        let both = laid(color, Some("%2"), Some("writer"));
        assert!(banded(&both, writer) && !banded(&both, worker));
        let apart = laid(color, Some("%2"), Some("worker"));
        assert!(
            banded(&apart, worker)
                && banded(&apart, doing)
                && !(0..apart.rows.len())
                    .filter(|&at| at != worker && at != doing)
                    .any(|at| apart.rows[at].iter().any(|(_, s)| s.bg.is_some())),
            "{:?}",
            text(&apart)
        );
        for l in [&both, &apart] {
            assert!(
                text(l)
                    .iter()
                    .all(|r| !r.contains('\u{f054}') && !r.starts_with('>')),
                "{:?}",
                text(l)
            );
        }
    }

    #[test]
    fn the_selection_takes_the_band_and_shows_no_chevron() {
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
                mark(&mut laid, name, 40, look);
            }
            laid
        };
        // With color the band is the selection's only mark, over its whole block (worker's name and doing lines, under
        // Working; writer has mail, so it is in Inbox): the text is the bare frame's, nothing reversed.
        for (selected, block) in [("worker", &[6, 7][..]), ("writer", &[3][..])] {
            let laid = drawn(color, Some(selected));
            let bare = drawn(color, None);
            for (n, (got, want)) in laid.rows.iter().zip(&bare.rows).enumerate() {
                let banded = got.iter().any(|(_, s)| s.bg == Some(BAND));
                assert_eq!(banded, block.contains(&n), "{selected} {n}");
                assert_eq!(
                    render(got, false).trim_end(),
                    render(want, false).trim_end(),
                    "{n}"
                );
                assert!(!render(got, true).contains("\u{1b}[7"), "{n}");
            }
        }
        // Without a band to draw (no color, ASCII) the keys still show their row: `>` in column 0.
        for look in [PLAIN_LOOK, ascii] {
            let rows: Vec<String> = drawn(look, Some("worker"))
                .rows
                .iter()
                .map(|r| render(r, false))
                .collect();
            assert!(
                rows[6].starts_with('>') && !rows[6].contains('\u{f054}'),
                "{rows:?}"
            );
            assert!(
                rows.iter()
                    .enumerate()
                    .all(|(n, r)| n == 6 || !r.starts_with('>')),
                "{rows:?}"
            );
        }
        // An unknown name marks nothing.
        let mut laid = lay(board.as_object(), &[], W40, color);
        let before = laid.rows.clone();
        mark(&mut laid, "nobody", 40, color);
        assert_eq!(laid.rows, before);
    }

    #[test]
    fn agent_text_never_reaches_the_frame_as_controls() {
        let evil = "a\u{1b}[31mred\u{1b}]8;;http://x\u{7}link\u{1b}]8;;\u{7}\u{9b}2J\u{85}\u{7f}z";
        let board = json!({"running": 1, "endedTotal": 1, "agents": [
            {"name": evil, "state": "running", "doing": evil, "model": evil, "age": 5, "unread": 0, "color": evil},
            {"name": evil, "state": "exited", "exitCode": evil, "age": 5, "endedAgo": 5}
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
            let board = json!({"running": 0, "endedTotal": 1, "agents": [{"name": name, "state": "stopped", "age": 60, "endedAgo": 60}]});
            let open = lay_open(board.as_object(), W40, PLAIN_LOOK);
            assert_eq!(
                render(&open.rows[5], false),
                pad("- ab", "1m"),
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
        // Fewer rows than the pane: a line feed, then erase-below.
        let rows = lay(mock().as_object(), &[], size, PLAIN_LOOK).rows;
        let text = frame(&rows, size, PLAIN_LOOK);
        let (head, tail) = ("\u{1b}[?2026h\u{1b}[H", "\r\n\u{1b}[J\u{1b}[?2026l");
        assert!(text.starts_with(head) && text.ends_with(tail), "{text:?}");
        assert!(!text.contains("\u{1b}[2J"));
        let body = &text[head.len()..text.len() - tail.len()];
        let lines: Vec<&str> = body.split("\r\n").collect();
        assert_eq!(lines.len(), 13);
        assert!(lines.iter().all(|l| l.ends_with("\u{1b}[K")), "{lines:?}");
        // The hint row pinned to the pane's last row: the frame fills the pane and the cursor stays on that row.
        let pinned = frame(
            &layout(mock().as_object(), &[], size, PLAIN_LOOK),
            size,
            PLAIN_LOOK,
        );
        assert_eq!(pinned.matches("\r\n").count(), 19);
        assert!(
            pinned.ends_with("keys\u{1b}[K\u{1b}[J\u{1b}[?2026l"),
            "{pinned:?}"
        );
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
        // TERM=dumb: the rows and a blank line, no ESC (the hint after one blank row, never padded to the pane's
        // height, since each frame scrolls); forced color keeps SGR only.
        let dumb = Look {
            dumb: true,
            ..PLAIN_LOOK
        };
        let rows = layout(mock().as_object(), &[], size, dumb);
        assert_eq!(rows.len(), 15);
        let text = frame(&rows, size, dumb);
        let plain_rows = format_board(mock().as_object(), &[], size, dumb);
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
        crate::agents::core_fixture::member(
            &w,
            ID,
            json!({"name":"worker","pane":"%9","startCommand":crate::agents::fixture::launch_command("%9")}),
        );
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        w.fakes
            .mux_first("show-buffer", 0, "line one\nline two\n", "");
        w.fakes.clear_log();
        let target = Target {
            id: ID.into(),
            ..Target::new("worker", "%9")
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
        crate::agents::core_fixture::member(
            &odd,
            ID,
            json!({"name":"worker","pane":"%9","startCommand":crate::agents::fixture::launch_command("%9")}),
        );
        odd.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        odd.fakes.clear_log();
        assert!(peek(&odd.ctx, &odd.caller(), &target).is_err());
        assert!(!odd.fakes.mux_calls().iter().any(|c| {
            c.iter()
                .any(|a| a.contains("capture-pane") || a == "run-shell")
        }));
        let moved = Target {
            id: ID.into(),
            ..Target::new("worker", "%4")
        };
        w.fakes.clear_log();
        assert!(peek(&w.ctx, &caller, &moved).is_err());
        assert!(
            !w.fakes
                .mux_calls()
                .iter()
                .any(|c| c.iter().any(|a| a.contains("capture-pane")))
        );
        w.fakes.hc_done().unwrap();
        odd.fakes.hc_done().unwrap();
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
                .map(|(n, w, _)| format!("{n}={w}"))
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
            ("w1".to_string(), "blocked".to_string(), T1.to_string()),
            ("w2".to_string(), "done".to_string(), T2.to_string()),
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

    // Four times, a minute apart, as a board's `changedAt` reads them.
    const T0: &str = "1970-01-01T00:01:00Z";
    const T1: &str = "1970-01-01T00:02:00Z";
    const T2: &str = "1970-01-01T00:03:00Z";
    const T3: &str = "1970-01-01T00:04:00Z";

    // A session with a stateful `@mux-agents-alerted` (read, written) and two bars' callers: %7 in @1 and %8 in @2.
    fn two_bars() -> (World, Caller, Caller) {
        let w = World::human("");
        let alerted = Rc::new(RefCell::new(String::new()));
        let a = alerted.clone();
        w.fakes.mux_script(ALERTED, move |argv| {
            if argv[1] == "show-options" {
                return Ok(out(0, &format!("{}\n", a.borrow()), ""));
            }
            *a.borrow_mut() = argv.last().unwrap().clone();
            Ok(Output::default())
        });
        let one = w.caller();
        let mut two = one.clone();
        two.window = "@2".into();
        two.pane = "%8".into();
        (w, one, two)
    }

    fn told(w: &World) -> Vec<String> {
        sent(&w.fakes)
    }

    fn sent(fakes: &Fakes) -> Vec<String> {
        fakes
            .mux_calls()
            .into_iter()
            .filter(|c| {
                c[1] == "display-message" && c.last().is_some_and(|t| t.starts_with("mux-agents:"))
            })
            .map(|c| c.last().unwrap().clone())
            .collect()
    }

    fn agent_board(activity: &str, changed: &str) -> Value {
        json!({"running": 1, "agents": [
            {"name": "w2", "state": "running", "pane": "%9", "activity": activity, "changedAt": changed}
        ]})
    }

    // with a bar per window: two bars of one session see w2 turn blocked; the session is told once. The
    // decision and the message are made under the session's sidebar lock.
    #[test]
    fn two_bars_of_one_session_tell_one_change_once() {
        let (w, one, two) = two_bars();
        let fakes = w.fakes.clone();
        w.fakes.lock_probe(move |_| sent(&fakes).len().to_string());
        let (mut a, mut b) = (Alerts::default(), Alerts::default());
        for al in [&mut a, &mut b] {
            assert!(
                al.tick(agent_board("working", T0).as_object(), None)
                    .is_empty()
            );
        }
        for (al, caller) in [(&mut a, &one), (&mut b, &two)] {
            let news = al.tick(agent_board("blocked", T1).as_object(), None);
            assert_eq!(news.len(), 1);
            alert(&w.ctx, caller, &news);
        }
        assert_eq!(told(&w), ["mux-agents: w2 needs you"]);
        // The message went out while the first alert held the session lock.
        let key = one.server_key.clone();
        let locks = w.fakes.locks();
        assert_eq!(
            locks[..2],
            [
                format!("+sidebar|{key}|$3 0"),
                format!("-sidebar|{key}|$3 1")
            ]
        );
        // A later change (blocked again, a new changedAt) is told once more, by whichever bar sees it first.
        for al in [&mut a, &mut b] {
            al.tick(agent_board("working", T2).as_object(), None);
        }
        for (al, caller) in [(&mut a, &one), (&mut b, &two)] {
            let news = al.tick(agent_board("blocked", T3).as_object(), None);
            alert(&w.ctx, caller, &news);
        }
        assert_eq!(told(&w).len(), 2);
    }

    // with a bar per window: a bar opened after the alert, whose first read still saw w2 working (a read
    // racing the change), sees the same blocked change next: it was told, so nothing is sent.
    #[test]
    fn a_bar_opened_after_the_alert_tells_none_of_it() {
        let (w, one, two) = two_bars();
        let mut a = Alerts::default();
        a.tick(agent_board("working", T0).as_object(), None);
        let news = a.tick(agent_board("blocked", T1).as_object(), None);
        alert(&w.ctx, &one, &news);
        assert_eq!(told(&w).len(), 1);
        let mut late = Alerts::default();
        late.tick(agent_board("working", T0).as_object(), None);
        let news = late.tick(agent_board("blocked", T1).as_object(), None);
        assert_eq!(news.len(), 1);
        alert(&w.ctx, &two, &news);
        assert_eq!(told(&w).len(), 1);
    }

    // The writes of `@mux-agents-alerted`, as their value.
    fn alerted_writes(w: &World) -> Vec<String> {
        w.fakes
            .mux_calls()
            .into_iter()
            .filter(|c| c[1] == "set-option" && c.iter().any(|a| a == ALERTED))
            .map(|c| c.last().unwrap().clone())
            .collect()
    }

    //, monotonic (redesign): bar B told w2's newer change; bar A, behind, then reports w2's older one.
    // Nothing is sent and the record keeps the newer time: an older time is never written back.
    #[test]
    fn a_delayed_bar_replays_no_older_news() {
        let (w, one, two) = two_bars();
        let newer = ("w2".to_string(), "blocked".to_string(), T2.to_string());
        alert(&w.ctx, &two, &[newer]);
        assert_eq!(
            (told(&w).len(), alerted_writes(&w)),
            (1, vec![format!("w2={T2}")])
        );
        w.fakes.clear_log();
        let older = ("w2".to_string(), "done".to_string(), T1.to_string());
        alert(&w.ctx, &one, &[older]);
        assert!(told(&w).is_empty());
        assert!(alerted_writes(&w).is_empty());
    }

    // (redesign): the session lock not taken, the record unread or not written: nothing is sent, so a change
    // is told at most once.
    #[test]
    fn an_alert_whose_lock_or_record_fails_sends_nothing() {
        let news = vec![("w2".to_string(), "blocked".to_string(), T1.to_string())];
        for case in ["lock", "read", "write"] {
            let w = World::human("");
            let caller = w.caller();
            match case {
                "lock" => w.fakes.lock_timeout("sidebar|"),
                "read" => w.fakes.mux_rule(
                    "show-options -qv -t $3 @mux-agents-alerted",
                    1,
                    "",
                    "no server\n",
                ),
                _ => w
                    .fakes
                    .mux_rule("set-option -t $3 @mux-agents-alerted", 1, "", "no server\n"),
            }
            alert(&w.ctx, &caller, &news);
            assert!(told(&w).is_empty(), "{case}");
        }
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

    // A tab switch to a window whose active pane is its bar: the row on screen is the window's other pane (the one
    // active last), so the band moves to that tab's agent and never stays on the last pane of another tab.
    #[test]
    fn the_bar_in_front_shows_its_windows_other_pane() {
        let w = World::human("");
        let caller = w.caller();
        let window = caller.window.clone();
        for (listed, want) in [
            (format!("{}|1|0\n%53|0|1\n", caller.pane), Some("%53")),
            (
                format!("%60|0|0\n{}|1|0\n%61|0|1\n", caller.pane),
                Some("%61"),
            ),
            // No last pane: the first other pane.
            (format!("%60|0|0\n{}|1|0\n", caller.pane), Some("%60")),
            // The bar alone, or an unreadable list: none.
            (format!("{}|1|0\n", caller.pane), None),
            ("garbage".into(), None),
        ] {
            w.fakes.mux_first(WINDOW_PANES, 0, &listed, "");
            assert_eq!(window_other(&w.ctx, &caller).as_deref(), want, "{listed:?}");
            let call = w.fakes.mux_calls().pop().unwrap();
            assert_eq!(
                call[call.len() - 4..],
                ["-t", window.as_str(), "-F", WINDOW_PANES],
                "{listed:?}"
            );
        }
        w.fakes.mux_first(WINDOW_PANES, 1, "%53|0|1\n", "");
        assert_eq!(window_other(&w.ctx, &caller), None);
    }

    // Enter on an ended row resumes it off the loop: the bar draws the row as `resuming…` and keeps ticking, and the
    // verb's answer is applied on a later pass (here a refusal, `done` being a pre-lifecycle member: the note line says
    // why). Tab opens the fold, `G` selects its last row.
    #[test]
    fn enter_on_an_ended_row_draws_resuming_before_the_resume_answers() {
        let w = watch_world(&["live", "live", "live", "live", "live", "live", "gone"]);
        member_frames(&w, 7, &[ID, "bbbbbbbbbbbb"]);
        let caller = w.caller();
        let steps = vec![(300, key(b"\t")), (300, key(b"G")), (300, key(b"\r"))];
        let mut frames: Vec<String> = Vec::new();
        run_watch(
            &w.ctx,
            &caller,
            Origin::Hand,
            Box::new(Script::new(&w.fakes, steps)),
            &mut |frame: &str| {
                frames.push(frame.to_string());
                assert!(frames.len() <= 100, "runaway");
                Ok(())
            },
        );
        let at = |text: &str| frames.iter().position(|f| f.contains(text));
        let pending = at("resuming\u{2026}").expect("a frame shows the row resuming");
        let answered = at("resume refused").expect("a later frame shows the refusal");
        assert!(pending < answered, "{frames:?}");
        assert!(
            !frames[pending].contains("resume refused"),
            "{}",
            frames[pending]
        );
        assert!(
            frames[pending]
                .lines()
                .any(|l| l.contains("done") && l.contains("resuming\u{2026}")),
            "{}",
            frames[pending]
        );
        // The hint names what Enter does now: nothing new while the resume runs, so it says `resuming…`.
        assert!(
            frames[pending].contains(" resuming\u{2026}  p peek")
                && !frames[pending].contains("\u{23ce} resume"),
            "{}",
            frames[pending]
        );
    }

    // `p`, `m` and `o` with nothing selected act on the agent on screen as the band shows it: with the bar itself in
    // front (%7), the window's other pane, writer's view %10, never the first row (worker, %9). Through `run_watch`, so
    // the frame's own idea of the agent on screen is what the key gets.
    #[test]
    fn unselected_keys_act_on_the_agent_beside_the_focused_bar() {
        for key_byte in [b"p", b"m", b"o"] {
            let w = watch_world(&["live", "live", "live", "gone"]);
            crate::agents::core_fixture::member(
                &w,
                "cccccccccccc",
                json!({"name":"writer","pane":"%10","startCommand":crate::agents::fixture::launch_command("%10")}),
            );
            w.panes(&[
                "%9|@5|300|aaaaaaaaaaaa|worker",
                "%10|@5|310|cccccccccccc|writer",
            ]);
            w.fakes
                .mux_first("#{pane_id}|#{session_attached}", 0, "%7|1\n", "");
            w.fakes.mux_first(WINDOW_PANES, 0, "%10|0|1\n%7|1|0\n", "");
            member_frames(&w, 6, &[ID, "bbbbbbbbbbbb", "cccccccccccc"]);
            let caller = w.caller();
            let mut frames = Vec::new();
            let steps = vec![(300, key(key_byte))];
            run_watch(
                &w.ctx,
                &caller,
                Origin::Hand,
                Box::new(Script::new(&w.fakes, steps)),
                &mut |frame: &str| {
                    frames.push(frame.to_string());
                    assert!(frames.len() <= 100, "runaway");
                    Ok(())
                },
            );
            // Who the key acted on: a capture of its pane, a popup file naming it, or a note naming it.
            let calls: Vec<String> = w.fakes.mux_calls().iter().map(|c| c.join(" ")).collect();
            let captured = |pane: &str| {
                (calls.iter())
                    .any(|c| c.contains("capture-pane") && c.split(' ').any(|t| t == pane))
            };
            let files: Vec<String> = std::fs::read_dir(crate::agents::server_dir(&w.ctx, &caller))
                .map(|d| {
                    d.flatten()
                        .filter(|e| e.file_name().to_string_lossy().starts_with("peek-"))
                        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
                        .collect()
                })
                .unwrap_or_default();
            let named = |name: &str| {
                files
                    .iter()
                    .any(|f| f.contains(&format!("{name} (q closes)")))
                    || frames.iter().any(|f| {
                        f.contains(&format!("failed: {name}"))
                            || f.contains(&format!("open for {name}"))
                    })
            };
            let key = String::from_utf8_lossy(key_byte).to_string();
            assert!(
                captured("%10") || named("writer"),
                "{key}: {calls:?} {files:?}"
            );
            assert!(
                !captured("%9") && !named("worker"),
                "{key}: {calls:?} {files:?}"
            );
        }
    }

    #[test]
    fn a_tab_switch_moves_the_band_to_that_tabs_agent() {
        // A pane with no row (%2, a plain shell in another tab) on screen, then the person switches to the bar's tab
        // (%7 in front) whose other pane is worker's view (%9): worker's row takes the band.
        let mut w = click_world(&["live", "live", "live", "gone"], board_answer());
        let env: Vec<(String, String)> = (w.ctx.env.iter())
            .filter(|(k, _)| *k != "NO_COLOR")
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        w.ctx.env = Env::from_pairs(env);
        watch_env(&mut w, "FORCE_COLOR", "1");
        let probes = Rc::new(Cell::new(0));
        let seen = probes.clone();
        w.fakes
            .mux_script("#{pane_id}|#{session_attached}", move |_| {
                seen.set(seen.get() + 1);
                Ok(out(
                    0,
                    if seen.get() == 1 { "%2|1\n" } else { "%7|1\n" },
                    "",
                ))
            });
        w.fakes.mux_first(WINDOW_PANES, 0, "%9|0|1\n%7|1|0\n", "");
        watch_frames(&w, 3);
        let (ended, frames) = watch_run(&w);
        assert_eq!(ended, "pane-closed");
        let band = "\u{1b}[48;5;236m";
        assert!(!frames[0].contains(band), "{frames:?}");
        let last: Vec<String> = (frames.last().unwrap().split("\r\n"))
            .filter(|l| l.contains(band))
            .map(|l| {
                // The text without its SGR: each band span is its own `ESC[..m`.
                let mut out = String::new();
                let mut esc = false;
                for c in l.chars() {
                    match (esc, c) {
                        (false, '\u{1b}') => esc = true,
                        (true, 'm') => esc = false,
                        (false, c) => out.push(c),
                        _ => {}
                    }
                }
                out
            })
            .collect();
        // worker's block: its name line and its doing line.
        assert!(
            last.len() == 2 && last[0].contains("worker") && last[1].contains("testing the parser"),
            "{last:?} {:?}",
            frames.last()
        );
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
        crate::agents::core_fixture::member(
            &w,
            "bbbbbbbbbbbb",
            json!({"name":"done", "pane":"%8"}),
        );
        let done = w.folder("bbbbbbbbbbbb");
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

    // Queue exactly the caller-declared frame count before execution, never respond to observed calls.
    // Focus version calls are queued explicitly between these frame batches by each test.
    fn watch_frames(w: &World, count: usize) {
        member_frames(w, count, &[ID, "bbbbbbbbbbbb"]);
    }

    fn writer_frames(w: &World, count: usize) {
        member_frames(
            w,
            count,
            &[ID, "bbbbbbbbbbbb", "cccccccccccc", "dddddddddddd"],
        );
    }

    fn member_frames(w: &World, count: usize, ids: &[&str]) {
        use crate::agents::core_fixture as cf;
        let caller = w.caller();
        let folder = crate::agents::server_dir(&w.ctx, &caller).join("agents");
        let records: Vec<_> = ids
            .iter()
            .map(|id| {
                crate::state::fs::read_json(&folder.join(id).join("record.json"))
                    .unwrap()
                    .unwrap()
            })
            .collect();
        for _ in 0..count {
            cf::online(w);
            cf::listing(w, &crate::agents::membership::team(&caller), &[]);
            for record in &records {
                let mut terminal = cf::wire(record);
                if record["id"] == "bbbbbbbbbbbb" {
                    terminal["state"] = json!("finished");
                    terminal["reason"] = json!("exited");
                    terminal["exit_code"] = json!(0);
                }
                cf::get(w, record["hcAddress"].as_str().unwrap(), terminal);
            }
        }
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
        let ended = run_watch(
            &w.ctx,
            &caller,
            Origin::Hand,
            Box::new(NoInput),
            &mut |frame: &str| {
                frames.push(frame.to_string());
                assert!(frames.len() <= 100, "runaway");
                Ok(())
            },
        );
        w.fakes.hc_done().unwrap();
        (ended, frames)
    }

    // The watch world's board at 40x24, plain.
    // The watch world's agents were spawned from %1, so its main row heads the board.
    fn board_rows() -> Vec<String> {
        vec![
            pad("mux-agents", "1 working"),
            String::new(),
            format!(" {MAIN} main"),
            String::new(),
            " Working \u{b7} 1".into(),
            pad("\u{25c9} worker", "4m"),
            "   testing the parser".into(),
            String::new(),
            " \u{25b8} Ended \u{b7} 1".into(),
        ]
    }

    // `rows` with the hint row pinned to the last of `height` rows (a blank row at least above it), when it fits.
    fn pinned(mut rows: Vec<String>, height: usize) -> Vec<String> {
        if rows.len() + 2 <= height {
            rows.resize(height - 1, String::new());
            rows.push(format!(" {HINT}"));
        }
        rows
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
        board_frame_in(24)
    }

    // The board in a pane `height` rows high: the hint row is pinned to its last row.
    fn board_frame_in(height: usize) -> String {
        screen(&pinned(board_rows(), height), height)
    }

    // The board with notes under it, after a blank line.
    fn noted_frame(notes: &[&str]) -> String {
        noted_frame_in(notes, 24)
    }

    // `noted_frame` in a pane `height` rows high.
    fn noted_frame_in(notes: &[&str], height: usize) -> String {
        let mut rows = board_rows();
        rows.push(String::new());
        rows.extend(notes.iter().map(|n| format!(" {n}")));
        screen(&pinned(rows, height), height)
    }

    #[test]
    fn watch_follows_the_pane_size_each_tick() {
        let w = watch_world(&["live", "short", "live", "gone"]);
        watch_frames(&w, 3);
        let (ended, frames) = watch_run(&w);
        assert_eq!(ended, "pane-closed");
        // Four rows: the main row stays (the way back to the chat), the doing line and the heading go, so the row
        // keeps its word; the ended fold stays, the one way to its agents.
        let short = [
            pad("mux-agents", "1 working"),
            format!(" {MAIN} main"),
            pad("\u{25c9} worker", "working   4m"),
            " \u{25b8} Ended \u{b7} 1".to_string(),
        ];
        assert_eq!(frames, [board_frame(), screen(&short, 4), board_frame()]);
    }

    #[test]
    fn watch_pane_closed() {
        let w = watch_world(&["live", "live", "gone"]);
        watch_frames(&w, 2);
        let (ended, frames) = watch_run(&w);
        assert_eq!(ended, "pane-closed");
        assert_eq!(frames, [board_frame(), board_frame()]);
        assert_eq!(w.fakes.slept(), [2000, 2000]);
        assert_eq!(probes(&w), 3);
        // A thrown or failed probe is unknown, not gone: its frame says refresh failed, at the default narrowest
        // width (40, the board frames' width) until the pane answers its size.
        let w = watch_world(&["throw", "fail", "live", "gone"]);
        watch_frames(&w, 3);
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
        watch_frames(&w, 29);
        let (ended, frames) = watch_run(&w);
        assert_eq!((ended, frames.len(), probes(&w)), ("probe-failed", 29, 30));
        // A probe that names its pane resets the count.
        let mut answers = vec!["fail"; 29];
        answers.push("live");
        answers.extend(vec!["fail"; 29]);
        answers.push("gone");
        let w = watch_world(&answers);
        watch_frames(&w, 59);
        let (ended, frames) = watch_run(&w);
        assert_eq!((ended, frames.len(), probes(&w)), ("pane-closed", 59, 60));
    }

    #[test]
    fn watch_output_closed() {
        let w = watch_world(&["live", "live", "gone"]);
        let caller = w.caller();
        watch_frames(&w, 1);
        let ended = run_watch(
            &w.ctx,
            &caller,
            Origin::Hand,
            Box::new(NoInput),
            &mut |_| Err(io::Error::other("EIO")),
        );
        w.fakes.hc_done().unwrap();
        assert_eq!(ended, "output-closed");
        assert_eq!(probes(&w), 1);
        assert!(w.fakes.slept().is_empty());
        // An unknown probe and failed view read still write a frame, detecting a closed output at once.
        let w = watch_world(&["fail"]);
        w.fakes.mux_first("list-panes", 1, "", "lock");
        let caller = w.caller();
        watch_frames(&w, 1);
        let ended = run_watch(
            &w.ctx,
            &caller,
            Origin::Hand,
            Box::new(NoInput),
            &mut |_| Err(io::Error::other("EIO")),
        );
        w.fakes.hc_done().unwrap();
        assert_eq!((ended, probes(&w)), ("output-closed", 1));
    }

    #[test]
    fn watch_refresh_failed_keeps_last() {
        let w = watch_world(&["live", "live", "live", "gone"]);
        let record = w.folder(ID).join("record.json");
        let good = std::fs::read(&record).unwrap();
        watch_frames(&w, 1);
        std::fs::write(&record, "{").unwrap();
        w.fakes.sleep_probe(move |tick| {
            if tick == 1 {
                std::fs::write(&record, &good).unwrap();
            } else {
                std::fs::write(&record, "{").unwrap();
            }
        });
        let (ended, frames) = watch_run(&w);
        assert_eq!(ended, "pane-closed");
        assert_eq!(
            frames,
            [
                screen(
                    &pinned(
                        vec![
                            pad("mux-agents", "? running"),
                            String::new(),
                            " refresh failed".to_string()
                        ],
                        24
                    ),
                    24
                ),
                board_frame(),
                noted_frame(&["refresh failed"]),
            ]
        );
        // The board is read as the bar's pane, with the ended rows and their exit codes.
        assert_eq!(w.fakes.hc_calls().len(), 4);
        // Corrupt local records fail before pane reads; only the middle tick lists panes: its view read (a
        // watch whose window never held a view looks for one each tick) and its snapshot.
        assert_eq!(
            w.fakes
                .mux_calls()
                .iter()
                .filter(|c| snapshot_list(c))
                .count(),
            2
        );
        assert_eq!(count(&w, "list-panes"), 0); // This helper excludes membership snapshot lists.
    }

    #[test]
    fn watch_view_error_keeps_running_member_detached() {
        let w = watch_world(&["live", "gone"]);
        let caller = w.caller();
        w.fakes.mux_script("list-panes", |_| Ok(out(1, "", "down")));
        watch_frames(&w, 1);
        let board = crate::agents::board::board(&w.ctx, &caller, true, true).unwrap();
        w.fakes.hc_done().unwrap();
        assert_eq!(board["running"], 1);
        assert_eq!(board["agents"][0]["life"], "running");
        assert_eq!(board["agents"][0]["view"], "detached");
        assert_eq!(board["agents"][0]["pane"], Value::Null);
        assert_eq!(board["agents"][0]["window"], Value::Null);
        watch_frames(&w, 1);
        let (ended, frames) = watch_run(&w);
        assert_eq!(ended, "pane-closed");
        assert_eq!(frames, [board_frame()]);
        assert!(!frames[0].contains("refresh failed"));
        w.fakes.hc_done().unwrap();
    }

    #[test]
    fn watch_alerts_once_only_when_it_knows_who_looks_and_reads_the_option_only_then() {
        // Four ticks. worker (%9) works on the first and needs the person from the second on. The person looks at %9
        // on the first tick and has moved to %1 by the third; the second tick's probe goes unanswered.
        let w = watch_world(&["live", "live", "live", "live", "gone"]);
        let report = w.folder(ID).join("activity.json");
        let tick = Rc::new(Cell::new(0));
        let (ticks, fakes) = (tick.clone(), w.fakes.clone());
        // Each tick's board read sees that tick's report (the tick's view read sees the same).
        w.fakes.mux_script("list-panes", move |argv| {
            if !argv.iter().any(|a| a == "-s") {
                return Ok(out(1, "", "lock"));
            }
            ticks.set(probed(&fakes));
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
        watch_frames(&w, 4);
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

    // An alert never makes the tick wait on the session's sidebar lock: held on another thread when worker turns
    // blocked (the second tick), the alert stays pending and that frame is drawn at once; the first tick after the
    // lock is released sends it, exactly once.
    #[test]
    fn an_alert_waits_for_the_bar_lock_on_later_ticks_never_in_one() {
        use std::sync::mpsc;
        let w = watch_world(&["live", "live", "live", "live", "gone"]);
        let report = w.folder(ID).join("activity.json");
        let fakes = w.fakes.clone();
        w.fakes.mux_script("list-panes", move |argv| {
            if !argv.iter().any(|a| a == "-s") {
                return Ok(out(1, "", "lock"));
            }
            let tick = probed(&fakes);
            let state = if tick == 1 { "working" } else { "blocked" };
            std::fs::write(
                &report,
                format!(
                    r#"{{"state":"{state}","kind":null,"since":"1970-01-01T00:01:00.0000000Z"}}"#
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
        // The person looks at another pane all along.
        w.fakes.mux_script("#{pane_id}|#{session_attached}", |_| {
            Ok(out(0, "%1|1\n", ""))
        });
        watch_frames(&w, 4);
        let root = w.ctx.state_root.parent().unwrap().join("locks");
        w.fakes.real_locks(&root);
        let caller = w.caller();
        let key = scope::shown_lock("sidebar", &caller);
        let (held_tx, held_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let other = World::human("");
            let mut ctx = other.fakes.ctx();
            ctx.clock = Box::new(crate::ctx::SystemClock);
            crate::state::lock::with_lock_at(&ctx, &root, &key, || {
                held_tx.send(()).unwrap();
                let _ = go_rx.recv();
                Ok(())
            })
            .unwrap();
        });
        held_rx.recv().unwrap();
        let (mut go, mut holder) = (Some(go_tx), Some(holder));
        let (start, mut frames, mut sent_by_frame) = (std::time::Instant::now(), 0, Vec::new());
        let fakes = w.fakes.clone();
        run_watch(
            &w.ctx,
            &caller,
            Origin::Hand,
            Box::new(NoInput),
            &mut |_: &str| {
                frames += 1;
                sent_by_frame.push(sent(&fakes).len());
                // The tick that found the news drew its frame: the lock is released only now.
                if frames == 2 {
                    go.take().unwrap().send(()).unwrap();
                    holder.take().unwrap().join().unwrap();
                }
                Ok(())
            },
        );
        w.fakes.hc_done().unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "a tick waited: {:?}",
            start.elapsed()
        );
        // Nothing while the lock was held; sent by the third tick (the first after the release), once.
        assert_eq!(sent_by_frame, [0, 0, 1, 1], "sends seen at each frame");
        assert_eq!(sent(&w.fakes), ["mux-agents: worker needs you"]);
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
        w.fakes.hc_done().unwrap();
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
        // Runs before each wait with its number (from 1): a test changes the world between polls.
        on_wait: Option<Box<dyn FnMut(usize)>>,
    }

    impl Script {
        fn new(fakes: &Fakes, steps: Vec<(u64, Waited)>) -> Self {
            Script {
                fakes: fakes.clone(),
                steps: steps.into(),
                waits: Default::default(),
                on_wait: None,
            }
        }
    }

    impl Input for Script {
        fn wait(&mut self, _clock: &dyn crate::ctx::Clock, timeout: Duration) -> Waited {
            let ms = timeout.as_millis() as u64;
            self.waits.borrow_mut().push(ms);
            let n = self.waits.borrow().len();
            if let Some(hook) = self.on_wait.as_mut() {
                hook(n);
            }
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
        let ended = run_watch(&w.ctx, &caller, Origin::Hand, input, &mut |frame: &str| {
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

    /// The fake time (ms) of each tick: each starts with the bar's probe of its own pane. Wraps the probe answers of
    /// `watch_world`, answering each in turn from `probes`.
    fn tick_times(w: &World, probes: &'static [&'static str]) -> Rc<RefCell<Vec<u64>>> {
        let times: Rc<RefCell<Vec<u64>>> = Default::default();
        let (seen, fakes) = (times.clone(), w.fakes.clone());
        let queue = Rc::new(RefCell::new(
            probes.iter().copied().collect::<VecDeque<_>>(),
        ));
        w.fakes.mux_script(WATCH_PROBE, move |_| {
            use crate::ctx::Clock;
            let at = fakes.now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
            seen.borrow_mut().push(at.as_millis() as u64);
            let mut queue = queue.borrow_mut();
            let answer = if queue.len() > 1 {
                queue.pop_front().unwrap()
            } else {
                *queue.front().unwrap()
            };
            Ok(match answer {
                "live" => out(0, "%7|sidebar|40|24\n", ""),
                _ => out(0, "|||\n", ""),
            })
        });
        times
    }

    #[test]
    fn watch_under_constant_input_still_draws_every_2s() {
        let w = watch_world(&["live", "live", "live", "gone"]);
        let steps = (0..40)
            .map(|_| (300, Waited::Bytes(b"x".to_vec())))
            .collect();
        let script = Script::new(&w.fakes, steps);
        let waits = script.waits.clone();
        watch_frames(&w, 3);
        let (ended, frames, times) = watch_with(&w, Box::new(script));
        assert_eq!((ended, frames.len()), ("pane-closed", 3));
        assert_eq!(diffs(&times), [2000, 2000]);
        let waits = waits.borrow();
        assert!(waits.iter().all(|&ms| ms > 0 && ms <= 2000), "{waits:?}");
        // Each tick waits for six reads 300 ms apart, then the 200 ms left.
        assert_eq!(waits[..7], [2000, 1700, 1400, 1100, 800, 500, 200]);
        assert!(w.fakes.slept().is_empty());
    }

    // The refresh between ticks: with a 200 ms poll, a tab switch (another pane on screen) or an agent's report (its
    // folder written) redraws within one poll, not at the next tick; a poll that sees no change redraws nothing.
    #[test]
    fn a_poll_redraws_a_change_before_the_tick() {
        for change in ["tab", "report", "none"] {
            let w = watch_world(&["live", "live", "live", "gone"]);
            pace(Pace {
                tick: Duration::from_millis(2000),
                poll: Some(Duration::from_millis(200)),
            });
            let shown = Rc::new(Cell::new(0));
            let seen = shown.clone();
            let tab = change == "tab";
            w.fakes
                .mux_script("#{pane_id}|#{session_attached}", move |_| {
                    seen.set(seen.get() + 1);
                    // The tick's own read, then polls: the pane changes from the third poll on.
                    Ok(out(
                        0,
                        if tab && seen.get() > 3 {
                            "%2|1\n"
                        } else {
                            "%9|1\n"
                        },
                        "",
                    ))
                });
            let folder = crate::agents::server_dir(&w.ctx, &w.caller())
                .join("agents")
                .join(ID);
            let fakes = w.fakes.clone();
            let report = change == "report";
            let steps: Vec<(u64, Waited)> = Vec::new();
            let mut script = Script::new(&w.fakes, steps);
            script.on_wait = Some(Box::new(move |n| {
                if report && n == 3 {
                    std::thread::sleep(Duration::from_millis(5));
                    write(
                        &folder.join("activity.json"),
                        json!({"state": "done", "kind": null, "since": "x"}),
                    );
                }
                let _ = &fakes;
            }));
            let ticks = tick_times(&w, &["live", "live", "live", "gone"]);
            watch_frames(&w, 3);
            let (ended, _, _) = watch_with(&w, Box::new(script));
            pace(Pace::default());
            assert_eq!(ended, "pane-closed", "{change}");
            let ticks = diffs(&ticks.borrow());
            match change {
                "none" => assert_eq!(ticks[..2], [2000, 2000], "{change}"),
                _ => assert_eq!(
                    ticks[0], 600,
                    "{change}: the next tick at the poll after the change, {ticks:?}"
                ),
            }
        }
    }

    // The stamp sees every write the board reads: an agent's own files, and a generation's under `gens/<n>/` (its
    // exit evidence), each a rename into its folder.
    #[test]
    fn the_stamp_moves_with_a_generations_write() {
        let dir = std::env::temp_dir().join(format!("ma-stamp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let generation = dir.join("agents").join(ID).join("gens").join("0");
        std::fs::create_dir_all(&generation).unwrap();
        std::fs::create_dir_all(dir.join("mail")).unwrap();
        let before = stamp(&dir);
        std::thread::sleep(Duration::from_millis(20));
        write(&generation.join("exit.json"), json!({"code": 0}));
        let after = stamp(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(after > before, "{before:?} {after:?}");
    }

    // A write that lands while a tick reads the board is not taken as drawn: the poll's stamp is the one from before
    // the read, so the next poll sees it and draws it.
    #[test]
    fn a_write_during_the_read_is_drawn_at_the_next_poll() {
        let w = watch_world(&["live", "live", "live", "gone"]);
        pace(Pace {
            tick: Duration::from_millis(2000),
            poll: Some(Duration::from_millis(200)),
        });
        let folder = crate::agents::server_dir(&w.ctx, &w.caller())
            .join("agents")
            .join(ID);
        let caller = w.caller();
        let mut frames = 0;
        let ticks = tick_times(&w, &["live", "live", "live", "gone"]);
        watch_frames(&w, 3);
        let ended = run_watch(
            &w.ctx,
            &caller,
            Origin::Hand,
            Box::new(Script::new(&w.fakes, vec![])),
            &mut |_| {
                frames += 1;
                // The first frame's write: an agent reports after the board was read.
                if frames == 1 {
                    std::thread::sleep(Duration::from_millis(5));
                    write(
                        &folder.join("activity.json"),
                        json!({"state": "done", "kind": null, "since": "x"}),
                    );
                }
                Ok(())
            },
        );
        pace(Pace::default());
        w.fakes.hc_done().unwrap();
        assert_eq!(ended, "pane-closed");
        let ticks = diffs(&ticks.borrow());
        assert_eq!(ticks[0], 200, "the next tick at the first poll, {ticks:?}");
    }

    // With the 200 ms poll a bar may tick often, so a tick whose frame is the one already on screen writes nothing:
    // the screen does not change, and a click is never dropped for a frame that changed nothing (PRESS_GAP measures
    // from the last write). A changed frame is written at once; a size change or a minute without a write writes anyway.
    #[test]
    fn an_unchanged_frame_is_written_again_only_every_two_seconds() {
        let w = watch_world(&["live", "live", "live", "live", "live", "gone"]);
        pace(Pace {
            tick: Duration::from_millis(1000),
            poll: Some(Duration::from_millis(200)),
        });
        watch_frames(&w, 5);
        let (ended, frames, times) = watch_with(&w, Box::new(Script::new(&w.fakes, vec![])));
        pace(Pace::default());
        assert_eq!(ended, "pane-closed");
        // Ticks at 0, 1, 2, 3, 4 s: written at 0, then again at 2 and 4.
        assert_eq!(diffs(&times), [2000, 2000], "{} frames", frames.len());
    }

    // A rewrite of the same frame moves nothing under the pointer: a click right after it is taken, not dropped.
    #[test]
    fn a_click_right_after_a_rewrite_of_the_same_frame_is_taken() {
        let w = click_world(&["live", "live", "live", "live", "gone"], board_answer());
        pace(Pace {
            tick: Duration::from_millis(1000),
            poll: Some(Duration::from_millis(200)),
        });
        // Ticks at 0, 1, 2 s (the rewrite at 2 s); the click 10 ms after it, then its redraw and the last tick.
        let script = Script::new(&w.fakes, vec![(2010, press(6))]);
        watch_frames(&w, 3);
        crate::agents::core_fixture::online(&w);
        watch_frames(&w, 1);
        let (ended, _, _) = watch_with(&w, Box::new(script));
        pace(Pace::default());
        assert_eq!(ended, "pane-closed");
        assert_eq!(
            (click_reads(&w), selects(&w)),
            (1, 1),
            "the click focused worker"
        );
    }

    // A resize is always written, even when the rows come out the same (a taller pane whose rows all fit): the
    // terminal may have cleared the pane, and a click aims at the new geometry.
    #[test]
    fn a_resize_is_written_even_with_the_same_rows() {
        let w = watch_world(&["live", "tall", "tall", "gone"]);
        w.fakes.mux_script(WATCH_PROBE, {
            let queue = Rc::new(RefCell::new(VecDeque::from([
                "live", "tall", "tall", "gone",
            ])));
            move |_| {
                let mut queue = queue.borrow_mut();
                let answer = if queue.len() > 1 {
                    queue.pop_front().unwrap()
                } else {
                    *queue.front().unwrap()
                };
                Ok(match answer {
                    "live" => out(0, "%7|sidebar|40|24\n", ""),
                    "tall" => out(0, "%7|sidebar|40|30\n", ""),
                    _ => out(0, "|||\n", ""),
                })
            }
        });
        pace(Pace {
            tick: Duration::from_millis(1000),
            poll: Some(Duration::from_millis(200)),
        });
        watch_frames(&w, 3);
        let (ended, frames, _) = watch_with(&w, Box::new(Script::new(&w.fakes, vec![])));
        pace(Pace::default());
        assert_eq!(ended, "pane-closed");
        assert_eq!(frames.len(), 2, "24 rows, then 30: written; 30 again: not");
    }

    #[test]
    fn watch_ctrl_c_ends_interrupted() {
        let w = watch_world(&["live"]);
        let script = Script::new(&w.fakes, vec![(500, Waited::Bytes(vec![3]))]);
        watch_frames(&w, 1);
        let (ended, frames, _) = watch_with(&w, Box::new(script));
        assert_eq!((ended, frames.len(), probes(&w)), ("interrupted", 1, 1));
    }

    // `q` draws `close bar? y/n` at once; a `y` read alone `BAR_GAP` after that frame was written closes the bar and
    // ends the watch `closed` (no bar is proven in this world, so nothing is killed). A `y` sooner leaves it asked.
    #[test]
    fn watch_q_then_y_ends_closed_only_after_the_question_showed() {
        let w = watch_world(&["live"]);
        let script = Script::new(
            &w.fakes,
            vec![
                (100, Waited::Bytes(b"q".to_vec())),
                (300, Waited::Bytes(b"y".to_vec())),
            ],
        );
        watch_frames(&w, 2);
        let (ended, frames, times) = watch_with(&w, Box::new(script));
        assert_eq!((ended, frames.len()), ("closed", 2));
        assert!(frames[1].contains("close bar? y/n"), "{frames:?}");
        assert_eq!(diffs(&times), [100], "the question is drawn at once");
        let w = watch_world(&["live", "live", "gone"]);
        let script = Script::new(
            &w.fakes,
            vec![
                (100, Waited::Bytes(b"q".to_vec())),
                (200, Waited::Bytes(b"y".to_vec())),
            ],
        );
        watch_frames(&w, 2);
        let (ended, frames, _) = watch_with(&w, Box::new(script));
        assert_eq!(
            (ended, frames.len()),
            ("pane-closed", 2),
            "too soon: still asked"
        );
    }

    // `@mux-agents-keys` is read once, when the bar starts, as the bar's pane reads it; its keys are decoded from then
    // on and the key help shows them. No tick reads it again.
    #[test]
    fn watch_reads_the_keys_once_and_decodes_by_them() {
        let w = watch_world(&["live", "live", "live", "gone"]);
        w.fakes.mux_rule("#{@mux-agents-keys}", 0, "help=h\n", "");
        let script = Script::new(&w.fakes, vec![(100, Waited::Bytes(b"h".to_vec()))]);
        watch_frames(&w, 3);
        let (ended, frames, _) = watch_with(&w, Box::new(script));
        assert_eq!((ended, frames.len()), ("pane-closed", 3));
        assert!(frames[1].contains("h or Esc closes"), "{frames:?}");
        let reads: Vec<Vec<String>> = (w.fakes.mux_calls().into_iter())
            .filter(|c| c.iter().any(|a| a.contains("@mux-agents-keys")))
            .collect();
        assert_eq!(reads.len(), 1, "{reads:?}");
        assert_eq!(reads[0][1..5], ["display-message", "-p", "-t", "%7"]);
    }

    #[test]
    fn watch_lone_escape_waits_50ms() {
        let w = watch_world(&["live", "live", "live", "gone"]);
        let script = Script::new(&w.fakes, vec![(100, Waited::Bytes(b"\x1b".to_vec()))]);
        let waits = script.waits.clone();
        watch_frames(&w, 3);
        let (ended, frames, times) = watch_with(&w, Box::new(script));
        assert_eq!((ended, frames.len()), ("pane-closed", 3));
        // 50 ms after the ESC it counts as the key, which draws again at once.
        assert_eq!(*waits.borrow(), [2000, 50, 2000, 2000]);
        assert_eq!(diffs(&times), [150, 2000]);
    }

    #[test]
    fn watch_keeps_drawing_after_input_closes() {
        let w = watch_world(&["live", "live", "live", "gone"]);
        let script = Script::new(&w.fakes, vec![(100, Waited::Closed)]);
        let waits = script.waits.clone();
        watch_frames(&w, 3);
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
        watch_frames(&w, 2);
        let ended = run_watch(&w.ctx, &caller, Origin::Hand, Box::new(script), &mut |_| {
            frames += 1;
            assert!(frames <= 100, "runaway");
            // The terminal takes 250 ms to take the frame.
            w.fakes.advance(Duration::from_millis(250));
            Ok(())
        });
        w.fakes.hc_done().unwrap();
        assert_eq!((ended, frames), ("pane-closed", 2));
        assert_eq!(*waits.borrow(), [2000, 2000]);
    }

    #[test]
    fn watch_late_lone_escape_waits_only_the_time_left() {
        let w = watch_world(&["live", "live", "live", "gone"]);
        let script = Script::new(&w.fakes, vec![(1990, Waited::Bytes(b"\x1b".to_vec()))]);
        let waits = script.waits.clone();
        watch_frames(&w, 3);
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
            watch_frames(&w, if want == "probe-failed" { 29 } else { 1 });
            let ended = run_watch(&w.ctx, &caller, Origin::Hand, input, &mut |frame| {
                frames += 1;
                assert!(frames <= 100, "runaway");
                write(frame)
            });
            w.fakes.hc_done().unwrap();
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
        watch_frames(&w, 1);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_watch(&w.ctx, &caller, Origin::Hand, input, &mut |_| {
                panic!("write")
            })
        }));
        assert!(panicked.is_err());
        w.fakes.hc_done().unwrap();
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
        watch_frames(&w, 3);
        let reply = watch_on(&w.ctx, &request, true, &open, &mut |_| {
            frames += 1;
            Ok(())
        })
        .unwrap();
        w.fakes.hc_done().unwrap();
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
        watch_frames(&w, 1);
        let reply = watch_on(&w.ctx, &request, true, &open, &mut |_| {
            frames += 1;
            assert!(frames <= 100, "runaway");
            Ok(())
        })
        .unwrap();
        w.fakes.hc_done().unwrap();
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
        w.fakes.hc_done().unwrap();
    }

    #[test]
    fn watch_poll_failure_restores_then_keeps_drawing() {
        let w = watch_world(&["live", "live", "gone"]);
        let (input, log) = term_input(vec![Err(io::Error::other("EIO"))]);
        let caller = w.caller();
        watch_frames(&w, 2);
        let ended = run_watch(&w.ctx, &caller, Origin::Hand, input, &mut |_| {
            log.borrow_mut().push("frame".into());
            Ok(())
        });
        w.fakes.hc_done().unwrap();
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

    // ---- clicks and keys

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

    // The board with worker selected: the marker in its margin, the hint on the last row.
    fn marked_frame() -> String {
        let mut rows = board_rows();
        rows[5] = rows[5].replacen(' ', ">", 1);
        screen(&pinned(rows, 24), 24)
    }

    // A click whose focus failed: the clicked row stays selected, as after Enter on it, and the note says why.
    fn marked_failed_frame() -> String {
        let mut rows = board_rows();
        rows[5] = rows[5].replacen(' ', ">", 1);
        rows.push(String::new());
        rows.push(" focus failed: worker".into());
        screen(&pinned(rows, 24), 24)
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
        let script = Script::new(&w.fakes, vec![(300, press(6))]);
        watch_frames(&w, 1);
        crate::agents::core_fixture::online(&w);
        watch_frames(&w, 2);
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

    #[test]
    fn click_on_the_doing_line_focuses_its_agent() {
        // Row 7 is worker's doing line: the same read and guarded select as its name line.
        let w = click_world(&["live", "live", "live", "gone"], board_answer());
        let script = Script::new(&w.fakes, vec![(300, press(7))]);
        watch_frames(&w, 1);
        crate::agents::core_fixture::online(&w);
        watch_frames(&w, 2);
        let (ended, _, _) = watch_with(&w, Box::new(script));
        assert_eq!(ended, "pane-closed");
        assert_eq!((click_reads(&w), selects(&w)), (1, 1));
    }

    #[test]
    fn main_row_click_and_enter_focus_the_main_pane() {
        // Row 2 is the main row (the agents' parent %1): one read, a guarded select of %1, its read-back.
        let main_select =
            "-N if-shell -F -t %1 #{==:#{pane_dead},0} select-window -t %1 ; select-pane -t %1";
        let main_back =
            "-N display-message -p -t %1 #{pane_id}|#{window_active}|#{pane_active}|#{pane_dead}";
        let w = click_world(&["live", "live", "live", "gone"], board_answer());
        w.fakes.mux_first(main_back, 0, "%1|1|1|0\n", "");
        let script = Script::new(&w.fakes, vec![(300, press(3))]);
        // The main pane is no member: no hc connection.
        watch_frames(&w, 3);
        let (ended, frames, _) = watch_with(&w, Box::new(script));
        assert_eq!(ended, "pane-closed");
        let calls = mux_text(&w);
        let at = calls.iter().position(|c| c == CLICK).expect("a click read");
        assert_eq!(
            calls[at + 1..at + 3],
            [main_select.to_string(), main_back.to_string()]
        );
        assert!(frames.iter().all(|f| !f.contains("failed")), "{frames:?}");
        // `k` from nothing selects the last of the walk, the main row; Enter focuses it.
        let w = click_world(&["live", "live", "live", "live", "gone"], board_answer());
        w.fakes.mux_first(main_back, 0, "%1|1|1|0\n", "");
        let steps = vec![(300, key(b"k")), (100, key(b"\r"))];
        watch_frames(&w, 4);
        let (ended, _, _) = watch_with(&w, Box::new(Script::new(&w.fakes, steps)));
        assert_eq!(ended, "pane-closed");
        assert!(
            mux_text(&w).iter().any(|c| c == main_select),
            "{:?}",
            mux_text(&w)
        );
        // A main pane that is gone: the read-back fails, the bar says so and selects nothing else.
        let w = click_world(&["live", "live", "live", "gone"], board_answer());
        w.fakes.mux_first(main_back, 0, "\n", "");
        let script = Script::new(&w.fakes, vec![(300, press(3))]);
        // The main pane is no member: no hc connection.
        watch_frames(&w, 3);
        let (_, frames, _) = watch_with(&w, Box::new(script));
        assert!(
            frames.iter().any(|f| f.contains("focus failed: main")),
            "{frames:?}"
        );
        assert_eq!(selects(&w), 1, "only the guarded select of %1");
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
        writer_frames(&w, 1);
        crate::agents::core_fixture::online(&w);
        crate::agents::core_fixture::online(&w);
        writer_frames(&w, 2);
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
        writer_frames(&w, 1);
        crate::agents::core_fixture::online(&w);
        crate::agents::core_fixture::online(&w);
        writer_frames(&w, 2);
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
            writer_frames(&w, 3);
            let (ended, frames, times) = watch_with(&w, Box::new(script));
            assert_eq!((ended, frames.len()), ("pane-closed", 3), "{width}");
            assert_eq!(diffs(&times), [300, 2000], "{width}");
            assert_eq!((click_reads(&w), selects(&w)), (1, 0), "{width}");
            assert_eq!(frames[0], frames[1], "{width}");
        }
    }

    // Audit 6-10 F8: the real `click` on a rendered frame: a kept row takes no click; the member below it, shifted by
    // the fold, still does. `frame_hit` counts rows from 1, so the press is the drawn row + 1.
    #[test]
    fn a_click_never_lands_on_a_kept_row_and_members_keep_their_rows() {
        let board = json!({"agents": [{"name": "worker", "id": ID, "state": "running", "pane": "%9"}],
            "kept": [{"address": "k1", "name": "a"}]});
        let laid = lay_focus(
            board.as_object(),
            &[],
            Size {
                width: 40,
                height: 24,
            },
            PLAIN_LOOK,
            tree::Focus::default(),
        )
        .0;
        let lines: Vec<String> = laid
            .rows
            .iter()
            .map(|r| r.iter().map(|(t, _)| t.as_str()).collect())
            .collect();
        let shown = Shown {
            written: SystemTime::UNIX_EPOCH,
            targets: walk_of(board.as_object(), &laid, false),
            drawn: drawn(&laid),
            width: 40,
            looking: None,
            team: false,
            done: Vec::new(),
        };
        let w = World::human("");
        let screen = answer(40, &lines);
        w.fakes
            .mux_script("capture-pane", move |_| Ok(out(0, &screen, "")));
        let row_of = |id: &str| {
            let at = laid.agents.iter().find(|(t, _)| t.id == id).unwrap().1;
            (at + 1) as u16
        };
        // A click hits a kept row too: it does what Enter on it does, asking `close? y/n`.
        assert_eq!(
            click(&w.ctx, &w.caller(), row_of("k1"), &shown).map(|t| t.id),
            Some("k1".to_string()),
            "a click hits a kept row"
        );
        assert_eq!(
            click(&w.ctx, &w.caller(), row_of(ID), &shown).map(|t| t.id),
            Some(ID.to_string()),
            "the member's shifted row"
        );
    }

    #[test]
    fn click_elsewhere_does_nothing() {
        // The header, the blank line and an ended row: each read, none focuses, each redraws at once.
        let w = click_world(
            &["live", "live", "live", "live", "live", "gone"],
            board_answer(),
        );
        let steps = vec![(300, press(1)), (300, press(2)), (300, press(9))];
        watch_frames(&w, 5);
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
        watch_frames(&w, 2);
        let (ended, frames, times) = watch_with(&w, Box::new(Script::new(&w.fakes, steps)));
        assert_eq!((ended, frames.len()), ("pane-closed", 2));
        assert_eq!(diffs(&times), [2000]);
        assert_eq!((click_reads(&w), selects(&w)), (0, 0));
    }

    #[test]
    fn press_near_or_across_a_write_is_dropped() {
        // Read 20 ms after its frame: dropped, but it still redraws at once.
        let w = click_world(&["live", "live", "live", "gone"], board_answer());
        let script = Script::new(&w.fakes, vec![(20, press(6))]);
        watch_frames(&w, 3);
        let (_, _, times) = watch_with(&w, Box::new(script));
        assert_eq!(diffs(&times), [20, 2000]);
        assert_eq!((click_reads(&w), selects(&w)), (0, 0));
        // Its first byte read before the second write, its last after: dropped.
        let w = click_world(&["live", "live", "live", "live", "gone"], board_answer());
        let steps = vec![(1990, key(b"\x1b[<0;3;")), (110, key(b"3M"))];
        watch_frames(&w, 4);
        let (_, _, times) = watch_with(&w, Box::new(Script::new(&w.fakes, steps)));
        assert_eq!(diffs(&times), [2000, 100, 2000]);
        assert_eq!((click_reads(&w), selects(&w)), (0, 0));
        // 30 ms after the second write: dropped.
        let w = click_world(&["live", "live", "live", "live", "gone"], board_answer());
        let script = Script::new(&w.fakes, vec![(2030, press(6))]);
        watch_frames(&w, 4);
        let (_, _, times) = watch_with(&w, Box::new(script));
        assert_eq!(diffs(&times), [2000, 30, 2000]);
        assert_eq!((click_reads(&w), selects(&w)), (0, 0));
        // 100 ms after it: acts.
        let w = click_world(&["live", "live", "live", "live", "gone"], board_answer());
        let script = Script::new(&w.fakes, vec![(2100, press(6))]);
        watch_frames(&w, 2);
        crate::agents::core_fixture::online(&w);
        watch_frames(&w, 2);
        let (_, _, times) = watch_with(&w, Box::new(script));
        assert_eq!(diffs(&times), [2000, 100, 2000]);
        assert_eq!((click_reads(&w), selects(&w)), (1, 1));
    }

    // A guard for the rule the live click tests wait on: a press read with the clock not moved since its frame is
    // dropped and only redraws; the same press read on a later frame, more than PRESS_GAP after it, focuses.
    #[test]
    fn a_press_read_at_its_frame_is_dropped_and_one_read_after_the_gap_focuses() {
        let w = click_world(&["live", "live", "live", "gone"], board_answer());
        let script = Script::new(&w.fakes, vec![(0, press(6))]);
        watch_frames(&w, 3);
        let (_, frames, times) = watch_with(&w, Box::new(script));
        assert_eq!(diffs(&times), [0, 2000], "redrawn at once, no tick counted");
        assert_eq!(frames.len(), 3);
        assert_eq!((click_reads(&w), selects(&w)), (0, 0), "dropped: no focus");
        let w = click_world(&["live", "live", "live", "live", "gone"], board_answer());
        let script = Script::new(&w.fakes, vec![(2060, press(6))]);
        watch_frames(&w, 2);
        crate::agents::core_fixture::online(&w);
        watch_frames(&w, 2);
        let (_, _, times) = watch_with(&w, Box::new(script));
        assert_eq!(diffs(&times), [2000, 60, 2000]);
        assert_eq!(
            (click_reads(&w), selects(&w)),
            (1, 1),
            "60 ms after the second frame: select-pane"
        );
    }

    // A press already waiting when a poll finds a change is taken against the frame still on screen, before the redraw:
    // read after the new frame, it would be dropped as aimed at the frame before it. A click from another pane makes
    // exactly that change, since the multiplexer's mouse binding selects the bar's pane before it passes the press on.
    #[test]
    fn a_press_waiting_when_a_poll_redraws_is_taken_first() {
        let w = click_world(&["live", "live", "live", "gone"], board_answer());
        pace(Pace {
            tick: Duration::from_millis(2000),
            poll: Some(Duration::from_millis(200)),
        });
        let folder = crate::agents::server_dir(&w.ctx, &w.caller())
            .join("agents")
            .join(ID);
        // The first wait times out at the first poll, which finds the status written meanwhile (the redraw changes
        // the frame), and the press is waiting by then.
        let mut script = Script::new(&w.fakes, vec![(200, Waited::Timeout), (0, press(6))]);
        let waits = script.waits.clone();
        script.on_wait = Some(Box::new(move |n| {
            if n == 1 {
                std::thread::sleep(Duration::from_millis(5));
                write(
                    &folder.join("status.json"),
                    json!({"text": "reading the tests", "at": "x"}),
                );
            }
        }));
        watch_frames(&w, 1);
        crate::agents::core_fixture::online(&w);
        watch_frames(&w, 2);
        let caller = w.caller();
        let (mut frames, mut times) = (Vec::new(), Vec::new());
        let ended = run_watch(
            &w.ctx,
            &caller,
            Origin::Hand,
            Box::new(script),
            &mut |frame: &str| {
                frames.push(frame.to_string());
                let at = w.ctx.clock.now().duration_since(SystemTime::UNIX_EPOCH);
                times.push(at.unwrap().as_millis() as u64);
                assert!(frames.len() <= 100, "runaway");
                Ok(())
            },
        );
        pace(Pace::default());
        assert_eq!(
            (click_reads(&w), selects(&w)),
            (1, 1),
            "the waiting press focused worker"
        );
        w.fakes.hc_done().unwrap();
        assert_eq!(ended, "pane-closed");
        assert_eq!(
            waits.borrow()[..2],
            [200, 0],
            "the input read without a wait"
        );
        assert_eq!(diffs(&times), [200, 2000]);
        assert!(
            !frames[0].contains("reading the tests") && frames[1].contains("reading the tests"),
            "the redraw changed the frame: {frames:?}"
        );
    }

    // A lone ESC already read when a poll finds a change keeps waiting for the rest of its sequence through the
    // redraw: the read of waiting input before the redraw does not make it the Esc key.
    #[test]
    fn a_lone_escape_waiting_when_a_poll_redraws_keeps_its_sequence() {
        let w = click_world(&["live", "live", "live", "gone"], board_answer());
        pace(Pace {
            tick: Duration::from_millis(2000),
            poll: Some(Duration::from_millis(200)),
        });
        let folder = crate::agents::server_dir(&w.ctx, &w.caller())
            .join("agents")
            .join(ID);
        // The ESC at the first poll, which finds the status written meanwhile; the rest of a Down 30 ms later.
        let steps = vec![(200, key(b"\x1b")), (30, key(b"[B"))];
        let mut script = Script::new(&w.fakes, steps);
        script.on_wait = Some(Box::new(move |n| {
            if n == 1 {
                std::thread::sleep(Duration::from_millis(5));
                write(
                    &folder.join("status.json"),
                    json!({"text": "reading the tests", "at": "x"}),
                );
            }
        }));
        watch_frames(&w, 3);
        let (ended, frames, _) = watch_with(&w, Box::new(script));
        pace(Pace::default());
        assert_eq!(ended, "pane-closed");
        assert!(
            frames.iter().any(|f| f.contains(">\u{25c9} worker")),
            "the Down selected worker: {frames:?}"
        );
    }

    #[test]
    fn one_read_handles_every_event_in_order() {
        let w = click_world(&["live", "live", "live", "gone"], board_answer());
        let script = Script::new(&w.fakes, vec![(300, key(b"j\r"))]);
        watch_frames(&w, 1);
        crate::agents::core_fixture::online(&w);
        watch_frames(&w, 2);
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
        let steps = vec![(300, press(6)), (20, press(6))];
        watch_frames(&w, 1);
        crate::agents::core_fixture::online(&w);
        watch_frames(&w, 3);
        let (ended, frames, times) = watch_with(&w, Box::new(Script::new(&w.fakes, steps)));
        assert_eq!(ended, "pane-closed");
        assert_eq!(
            frames,
            [
                board_frame(),
                marked_failed_frame(),
                marked_frame(),
                marked_frame()
            ]
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
        watch_frames(&w, 3);
        crate::agents::core_fixture::online(&w);
        watch_frames(&w, 2);
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
        let script = Script::new(&w.fakes, vec![(300, press(6))]);
        watch_frames(&w, 1);
        crate::agents::core_fixture::online(&w);
        watch_frames(&w, 2);
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
        let script = Script::new(&w.fakes, vec![(300, press(6))]);
        watch_frames(&w, 1);
        crate::agents::core_fixture::online(&w);
        watch_frames(&w, 2);
        let (ended, frames, times) = watch_with(&w, Box::new(script));
        assert_eq!(ended, "pane-closed");
        assert_eq!(
            frames,
            [board_frame(), marked_failed_frame(), marked_failed_frame()]
        );
        assert_eq!(diffs(&times), [300, 2000]);
        assert_eq!(selects(&w), 1);
    }

    #[test]
    fn leaving_the_bar_clears_the_selection() {
        // `j` selects worker while the bar (%7) is the pane on screen; on a tick that shows another pane (%1, the
        // person went back to their chat) the selection and its hint are gone, the frame bare again.
        // (the bar was on screen and the person left it, the selection cleared): keys sent to a bar that was never on
        // screen (the session shows %1 throughout) keep theirs; so does a tick whose probe is unanswered or whose
        // session has no client attached (`unknown`).
        for (left, never) in [(false, false), (true, false), (false, true)] {
            let w = click_world(&["live", "live", "live", "gone"], board_answer());
            let probes = Rc::new(Cell::new(0));
            let seen = probes.clone();
            w.fakes
                .mux_script("#{pane_id}|#{session_attached}", move |_| {
                    seen.set(seen.get() + 1);
                    Ok(if never || (left && seen.get() > 2) {
                        out(0, "%1|1\n", "")
                    } else {
                        out(0, "%7|1\n", "")
                    })
                });
            let steps = vec![(300, key(b"j"))];
            watch_frames(&w, 3);
            let (ended, frames, _) = watch_with(&w, Box::new(Script::new(&w.fakes, steps)));
            assert_eq!(ended, "pane-closed");
            let last = frames.last().unwrap();
            if left {
                assert_eq!(last, &board_frame(), "cleared after leaving: {frames:?}");
            } else {
                assert_eq!(last, &marked_frame(), "kept while in the bar: {frames:?}");
            }
        }
    }

    #[test]
    fn the_band_stays_on_the_last_agent_while_the_bar_has_focus() {
        // worker (%9) on screen, then the person clicks into the bar (%7): worker's row keeps its band.
        let mut w = click_world(&["live", "live", "live", "gone"], board_answer());
        // Colored frames: the watch world draws plain (NO_COLOR), so that is taken out.
        let env: Vec<(String, String)> = (w.ctx.env.iter())
            .filter(|(k, _)| *k != "NO_COLOR")
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        w.ctx.env = Env::from_pairs(env);
        watch_env(&mut w, "FORCE_COLOR", "1");
        let probes = Rc::new(Cell::new(0));
        let seen = probes.clone();
        w.fakes
            .mux_script("#{pane_id}|#{session_attached}", move |_| {
                seen.set(seen.get() + 1);
                Ok(out(
                    0,
                    if seen.get() == 1 { "%9|1\n" } else { "%7|1\n" },
                    "",
                ))
            });
        watch_frames(&w, 3);
        let (ended, frames) = watch_run(&w);
        assert_eq!(ended, "pane-closed");
        let band = "\u{1b}[48;5;236m";
        assert!(frames.iter().all(|f| f.contains(band)), "{frames:?}");
    }

    #[test]
    fn a_pending_close_question_keeps_its_selection_after_leaving() {
        let mut p = Pick::default();
        let kept = Target {
            life: pick::KEPT.into(),
            ..Target::new("k1", "")
        };
        p.select(std::slice::from_ref(&kept), 0);
        p.ask(kept);
        p.left();
        assert_eq!(
            p.selected(),
            Some("k1"),
            "the question's target stays selected"
        );
        let mut q = Pick::default();
        q.select(&[Target::new("w", "%9")], 0);
        q.left();
        assert_eq!(q.selected(), None);
    }

    #[test]
    fn an_unanswered_or_detached_tick_keeps_the_selection() {
        for after in ["", "%1|0\n"] {
            let w = click_world(&["live", "live", "live", "gone"], board_answer());
            let probes = Rc::new(Cell::new(0));
            let seen = probes.clone();
            let later = after.to_string();
            w.fakes
                .mux_script("#{pane_id}|#{session_attached}", move |_| {
                    seen.set(seen.get() + 1);
                    Ok(match seen.get() {
                        1 | 2 => out(0, "%7|1\n", ""),
                        _ if later.is_empty() => out(1, "", "no server"),
                        _ => out(0, &later, ""),
                    })
                });
            let steps = vec![(300, key(b"j"))];
            watch_frames(&w, 3);
            let (ended, frames, _) = watch_with(&w, Box::new(Script::new(&w.fakes, steps)));
            assert_eq!(ended, "pane-closed");
            assert_eq!(
                frames.last().unwrap(),
                &marked_frame(),
                "{after:?}: {frames:?}"
            );
        }
    }

    #[test]
    fn keys_select_and_focus() {
        let w = click_world(&["live", "live", "live", "live", "gone"], board_answer());
        let steps = vec![(300, key(b"j")), (100, key(b"\r"))];
        watch_frames(&w, 2);
        crate::agents::core_fixture::online(&w);
        watch_frames(&w, 2);
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
        watch_frames(&w, 3);
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
        watch_frames(&w, 2);
        crate::agents::core_fixture::online(&w);
        watch_frames(&w, 2);
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
        watch_frames(&w, 2);
        crate::agents::core_fixture::online(&w);
        watch_frames(&w, 1);
        let (ended, frames, _) = watch_with(&w, Box::new(Script::new(&w.fakes, steps)));
        assert_eq!((ended, frames.len()), ("pane-closed", 3));
        assert_eq!(selects(&w), 1);
        // Not focused: the note shows, and worker stays selected.
        assert!(
            frames[2].contains("focus failed: worker"),
            "{:?}",
            frames[2]
        );
        assert!(
            frames[2].lines().any(|l| l.starts_with('>')),
            "{:?}",
            frames[2]
        );
    }

    #[test]
    fn note_lasts_to_second_tick_or_next_key() {
        let w = click_world(&["live", "live", "live", "live", "gone"], board_answer());
        w.fakes.mux_first(READ_BACK, 0, "%9|0|1|1\n", "");
        let script = Script::new(&w.fakes, vec![(300, press(6))]);
        watch_frames(&w, 1);
        crate::agents::core_fixture::online(&w);
        watch_frames(&w, 3);
        let (ended, frames, times) = watch_with(&w, Box::new(script));
        assert_eq!(ended, "pane-closed");
        assert_eq!(
            frames,
            [
                board_frame(),
                marked_failed_frame(),
                marked_failed_frame(),
                marked_frame()
            ]
        );
        assert_eq!(diffs(&times), [300, 2000, 2000]);
        // A release leaves it; j dismisses it.
        let w = click_world(&["live", "live", "live", "gone"], board_answer());
        w.fakes.mux_first(READ_BACK, 0, "%9|0|1|1\n", "");
        let steps = vec![
            (300, press(6)),
            (100, key(b"\x1b[<0;3;3m")),
            (100, key(b"j")),
        ];
        watch_frames(&w, 1);
        crate::agents::core_fixture::online(&w);
        watch_frames(&w, 2);
        let (_, frames, times) = watch_with(&w, Box::new(Script::new(&w.fakes, steps)));
        // The click left worker selected, as Enter on it would: `j` moves on to the next row, the fold line.
        let mut fold = board_rows();
        let at = fold.iter().position(|r| r.contains("Ended")).unwrap();
        fold[at] = fold[at].replacen(' ', ">", 1);
        let fold = screen(&pinned(fold, 24), 24);
        assert_eq!(frames, [board_frame(), marked_failed_frame(), fold]);
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
            image: None,
        });
        w.fakes.mux_rule(RECORD, 0, OWN, "");
        w.fakes.mux_rule(PROOF, 0, "%7|@1|0|4242\n", "");
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
        watch_frames(&w, 1);
        let (ended, frames) = watch_run(&w);
        assert_eq!(ended, "pane-closed");
        assert_eq!(kept(&w), [RESIZE]);
        assert_eq!(state.borrow().panes[1].width, 40);
        // Laid out at the size read back after the resize: the fake window's 50 rows.
        assert_eq!(frames, [board_frame_in(50)]);
        // When the size cannot be read again, the width the keeper set is used.
        let (w, _) = keep_world(&["wide", "gone"], wide());
        w.fakes.mux_first(SIZE_FORMAT, 1, "", "no server");
        watch_frames(&w, 1);
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
            watch_frames(&w, 3);
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
        watch_frames(&w, 5);
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
        watch_frames(&w, 5);
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
        watch_frames(&w, 6);
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
        watch_frames(&w, 6);
        let (_, frames) = watch_run(&w);
        assert_eq!(moves(&w), 4);
        assert_eq!(frames[2..4], [paused_frame(), paused_frame()]);
        assert_eq!(frames[4..], [board_frame_in(50), board_frame()]);
        // 60 s on the clock after the pause re-arm it; 59 s do not.
        for (skip, moved, paused) in [(58, 5, 3), (57, 4, 4)] {
            let (w, _) = keep_world(&six[1..], failing());
            let fakes = w.fakes.clone();
            w.fakes.sleep_probe(move |n| {
                if n == 3 {
                    fakes.advance(Duration::from_secs(skip));
                }
            });
            watch_frames(&w, 5);
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
        watch_frames(&w, 5);
        let (_, frames) = watch_run(&w);
        assert_eq!((moves(&w), layout_reads(&w)), (3, 8));
        // Each tick that moved the bar lays out at the size it reads back (the fake window's 50 rows).
        assert_eq!(frames[..2], vec![board_frame_in(50); 2]);
        assert_eq!(
            frames[2..],
            [
                noted_frame_in(&[PAUSED], 50),
                paused_frame(),
                paused_frame()
            ]
        );
        // A layout the user changes between two ticks is not a failure: each repair held when it was made.
        let (w, state) = keep_world(&six, win("%1", split_right()));
        state.borrow_mut().moved = Some(repaired());
        let s = state.clone();
        w.fakes.sleep_probe(move |_| {
            let mut win = s.borrow_mut();
            win.panes = split_right();
            win.moved = Some(repaired());
        });
        watch_frames(&w, 6);
        let (_, frames) = watch_run(&w);
        assert_eq!(moves(&w), 6);
        // Each tick moved the bar, so each frame is laid out at the size read back (the fake window's 50 rows).
        assert_eq!(frames, vec![board_frame_in(50); 6]);
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
        watch_frames(&w, 5);
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
        watch_frames(&w, 2);
        let (ended, frames) = watch_run(&w);
        // The tick that moved the bar lays out at the size it reads back (the fake window's 50 rows).
        assert_eq!(
            (ended, frames),
            ("pane-closed", vec![board_frame_in(50), board_frame()])
        );
        assert_eq!(moves(&w), 1);
        // A probe that does not answer is no proof the pane is there: nothing is read.
        let (w, _) = keep_world(&["fail", "fail", "gone"], win("%1", split_right()));
        watch_frames(&w, 2);
        watch_run(&w);
        assert_eq!((layout_reads(&w), kept(&w).len()), (0, 0));
        // A layout that does not list the bar, a failed read, or one that does not parse: that tick does nothing.
        for (code, stdout) in [(0, "%1|1|1|1|1|200|200|0|0|0\n"), (1, ""), (0, "garbage\n")] {
            let w = watch_world(&["live", "live", "gone"]);
            own_watch(&w);
            w.fakes.mux_first("#{pane_at_right}", code, stdout, "");
            watch_frames(&w, 2);
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

    /// The key of `window`'s record in `Panes`; the session's record stays under RECORD.
    fn wrec(window: &str) -> String {
        format!("{RECORD} {window}")
    }

    /// The caller's window @1 record.
    fn record(panes: &Panes) -> Option<String> {
        get(panes, &wrec("@1"))
    }

    /// The window `pane` stands in: `win <pane>`, else the caller's @1.
    fn window_of(panes: &Panes, pane: &str) -> String {
        get(panes, &format!("win {pane}")).unwrap_or_else(|| "@1".into())
    }

    /// `<exe> watch` as process `pid`, started at pid * 10.
    fn watch_row(pid: u32, exe: &str) -> crate::proc::Row {
        crate::proc::Row {
            pid,
            ppid: 900,
            start: Some(i64::from(pid) * 10),
            path: Some(exe.into()),
            argv: Some(vec![exe.into(), "watch".into()]),
            image: None,
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
                set(&panes, &wrec("@1"), "%7 107 1070".into());
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
            let dead_asked = argv.last().is_some_and(|f| f.contains("pane_dead"));
            let marks_asked = argv
                .last()
                .is_some_and(|f| f.ends_with("|#{pane_pid}|#{@mux-agents-bar}"));
            // `-s -t $3` lists the session; `-t @<n>` one window.
            let window = if argv[2] == "-s" {
                assert_eq!(argv[2..6], ["-s", "-t", "$3", "-F"]);
                None
            } else {
                assert_eq!((argv[2].as_str(), argv[4].as_str()), ("-t", "-F"));
                Some(argv[3].clone())
            };
            let text: String = p
                .borrow()
                .iter()
                .filter(|(k, _)| k.starts_with('%'))
                .filter(|(k, _)| window.as_ref().is_none_or(|w| *w == window_of(&p, k)))
                .map(|(pane, v)| {
                    let role = v.split_once('|').map_or(v.as_str(), |(r, _)| r);
                    let dead = dead_asked && get(&p, &format!("dead {pane}")).is_some();
                    // With the pid and the mark asked: `<pid>|<mark>` after the label, each empty when unset.
                    let tail = if marks_asked {
                        let pid = get(&p, &format!("pid {pane}")).unwrap_or_default();
                        let mark = get(&p, &format!("mark {pane}")).unwrap_or_default();
                        format!("|{pid}|{mark}")
                    } else {
                        String::new()
                    };
                    format!("{pane}|{}{tail}\n", flag(role == "sidebar" && !dead))
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
                    format!("{pane}|{}|{dead}|{pid}\n", window_of(&p, &pane))
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
            // The bar's mark, written only while the pane runs the pid bound in, alive or dead.
            if let Some(mark) =
                argv[6].strip_prefix(&format!("set-option -p -t {pane} @mux-agents-bar "))
            {
                if get(&p, &format!("pid {pane}")).as_deref() == Some(want.as_str()) {
                    set(&p, &format!("mark {pane}"), mark.to_string());
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
            // A kill bound to the bar's mark needs the mark and the pid, alive or dead; any other, a live pane.
            let held = if argv[5].contains("@mux-agents-bar") {
                get(&p, &format!("mark {pane}")).as_deref() == Some(want.as_str())
            } else {
                get(&p, &format!("dead {pane}")).is_none()
            };
            if held && get(&p, &format!("pid {pane}")).as_deref() == Some(want.as_str()) {
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
            let at = argv.iter().position(|a| a == "-t").unwrap();
            let window = argv[at + 1].clone();
            assert!(window.starts_with('@'), "a split beside a pane: {argv:?}");
            set(&p, &format!("win {pane}"), window.clone());
            set(&p, &pane, "|".into());
            set(&p, &format!("pid {pane}"), pid.to_string());
            let exe = get(&p, "exe").unwrap();
            match child {
                Some("unreadable") => {}
                // The first split's process is not read as the watch in time (it starts later); later splits are.
                Some("late") if n == 7 => {}
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
            Ok(out(0, &format!("{pane}|{window}|{pid}\n"), ""))
        });
        // The window's panes with their pid and the bar's mark: `<pid>|<pane>|<mark>`, with `<dead>|` after the pid
        // when the format asks `#{pane_dead}`.
        let p = panes.clone();
        w.fakes
            .mux_script("#{pane_id}|#{@mux-agents-bar}", move |argv| {
                // `-s -t $3` lists the session (-Close's strays); `-t @<n>` one window.
                let window = if argv[2] == "-s" {
                    assert_eq!(argv[2..6], ["-s", "-t", "$3", "-F"]);
                    None
                } else {
                    assert_eq!((argv[2].as_str(), argv[4].as_str()), ("-t", "-F"));
                    Some(argv[3].clone())
                };
                let dead_asked = argv.last().unwrap().contains("pane_dead");
                let text: String = p
                    .borrow()
                    .iter()
                    .filter(|(k, _)| {
                        k.starts_with('%') && window.as_ref().is_none_or(|w| *w == window_of(&p, k))
                    })
                    .map(|(pane, _)| {
                        let pid = get(&p, &format!("pid {pane}")).unwrap_or_default();
                        let mark = get(&p, &format!("mark {pane}")).unwrap_or_default();
                        let dead = if dead_asked {
                            format!("{}|", flag(get(&p, &format!("dead {pane}")).is_some()))
                        } else {
                            String::new()
                        };
                        format!("{pid}|{dead}{pane}|{mark}\n")
                    })
                    .collect();
                Ok(out(0, &text, ""))
            });
        // A plain pane write (not inside a pid-bound if-shell) is a bug.
        w.fakes.mux_script("-N set-option -p", |argv| {
            panic!("an unguarded pane write: {argv:?}")
        });
        // The session's windows: the caller's @1, then each other window a pane stands in, in order.
        let p = panes.clone();
        w.fakes.mux_script("list-windows", move |argv| {
            assert_eq!(argv[2..], ["-t", "$3", "-F", "#{window_id}"]);
            let mut windows = vec!["@1".to_string()];
            for (k, _) in p.borrow().iter().filter(|(k, _)| k.starts_with('%')) {
                let window = window_of(&p, k);
                if !windows.contains(&window) {
                    windows.push(window);
                }
            }
            let text: String = windows.iter().map(|w| format!("{w}\n")).collect();
            Ok(out(0, &text, ""))
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
        // A window target (`-w`) keeps that window's record, `-t $3` the session's.
        w.fakes.mux_script(RECORD, move |argv| {
            let at = argv.iter().position(|a| a == "-t").unwrap();
            let target = &argv[at + 1];
            assert_eq!(target.starts_with('@'), argv[2].contains('w'), "{argv:?}");
            let key = if target.starts_with('@') {
                wrec(target)
            } else {
                RECORD.to_string()
            };
            if argv[1] == "show-options" {
                let value = get(&p, &key).map_or(String::new(), |v| format!("{v}\n"));
                return Ok(out(0, &value, ""));
            }
            if argv[2].contains('u') {
                unset(&p, &key);
                return Ok(Output::default());
            }
            if record_fail {
                return failed("exit");
            }
            set(&p, &key, argv[at + 3].clone());
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
        match ensure(&w.ctx, &caller, &caller.window) {
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

    // The agent snapshot's pane list, which proves the caller's pane is no agent's before the bar is touched.
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
            "-N split-window -d -f -h -P -F #{{pane_id}}|#{{window_id}}|#{{pane_pid}} -t @1{}{} /bin/mux-agent watch -Managed {}",
            env_text(w),
            if sized { " -l 40" } else { "" },
            root_key(&w.ctx)
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

    // The calls of an open that finds no bar: the session record (migration), the window record, labels, split,
    // label, record, resize.
    fn opened(w: &World) -> Vec<String> {
        vec![
            SESSION_RECORD.into(),
            READ_RECORD.into(),
            LABELS.into(),
            MARKS.into(),
            split_line(w, true),
            mark_line("%7", 107),
            label_line("%7", 107),
            RECORD7.into(),
            resize_line("%7", 107, 40),
        ]
    }

    // the window's panes with their pid and the bar's mark, read before a split.
    const MARKS: &str =
        "-N list-panes -t @1 -F #{pane_pid}|#{pane_dead}|#{pane_id}|#{@mux-agents-bar}";
    // -Close's last read: the session's panes with their pid, for a watch the bar's split started that no record proves.
    const STRAYS: &str =
        "-N list-panes -s -t $3 -F #{pane_pid}|#{pane_dead}|#{pane_id}|#{@mux-agents-bar}";

    /// open's mark of the new bar, as the multiplexer receives it: only while the pane runs `pid`.
    fn mark_line(pane: &str, pid: u32) -> String {
        format!(
            "-N if-shell -F -t {pane} #{{==:#{{pane_pid}},{pid}}} set-option -p -t {pane} @mux-agents-bar {pid}"
        )
    }

    fn close_line(pane: &str, pid: u32) -> String {
        format!(
            "-N if-shell -F -t {pane} #{{&&:#{{==:#{{pane_dead}},0}},#{{==:#{{pane_pid}},{pid}}}}} kill-pane -t {pane}"
        )
    }

    /// The refusal while `pane` carries the bar's mark but is not proven.
    fn stuck_message(pane: &str) -> String {
        format!(
            "pane {pane} carries the side-bar mark but is not a proven side bar: a side bar that is starting proves \
             itself or closes within 10 s; if it stays, close it with kill-pane -t {pane}"
        )
    }

    // redesign: nothing outside a bar kills its pane. A failed open leaves its marked pane (%7, its watch not read
    // in time); while it is unproven, ensure (so spawn and focus, through keep) and the sidebar verb open no second bar
    // beside it, kill nothing and name it. Once it proves itself (its window's record names it), it is adopted as any
    // bar is. A pane marked for another pid (%5, a mark rotate-window moved) blocks nothing.
    #[test]
    fn a_marked_unproven_pane_blocks_a_split_until_it_is_adopted() {
        let (w, panes) = sidebar_world(
            "",
            Mux {
                child: Some("late"),
                extra: vec![
                    ("%5", "|".into()),
                    ("pid %5", "105".into()),
                    ("mark %5", "999".into()),
                ],
                ..Mux::default()
            },
        );
        let first = ensure_run(&w);
        assert!(
            first.starts_with("1 sidebar the new side-bar pane %7 did not start"),
            "{first}"
        );
        assert_eq!(get(&panes, "mark %7").as_deref(), Some("107"));
        w.fakes.clear_log();
        let stuck = format!("1 sidebar {}", stuck_message("%7"));
        assert_eq!(ensure_run(&w), stuck);
        assert_eq!(run(&w, &[]), stuck);
        let mut reply = Map::new();
        keep(&w.ctx, &w.caller(), "@1", &mut reply);
        assert_eq!(
            (&reply["sidebar"], &reply["sidebarError"]),
            (&Value::Null, &json!(stuck_message("%7")))
        );
        assert_eq!((count(&w, "split-window"), closes(&w)), (0, 0));
        assert!(get(&panes, "%7").is_some() && get(&panes, "%5").is_some());
        // Its watch starts after all and proves itself: the window's record names it.
        w.fakes
            .process(watch_row(107, &get(&panes, "exe").unwrap()));
        set(&panes, &wrec("@1"), "%7 107 1070".into());
        assert_eq!(ensure_run(&w), "existing %7");
        assert_eq!((count(&w, "split-window"), closes(&w)), (0, 0));
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
            [format!("-N display-message -p -t @1 {WINDOW_PROBE}")]
        );
        let mut want = vec![ON.to_string()];
        want.extend(opened(&w));
        want[5] = split_line(&w, true).replace("-l 40", "-l 50");
        want[9] = resize_line("%7", 107, 50);
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
        want[5] = split_line(&w, true).replace("-l 40", "-l 75");
        want[9] = resize_line("%7", 107, 75);
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
            r#"ok {"pane":"%7","window":"@1","opened":true}"#
        );
    }

    // A root's key is stored in the root: whoever reads it, from any working directory and however the root is
    // spelled, gets the same key; another root has its own. The first use makes the root private, as every state
    // folder is, so a later `up` accepts it.
    #[test]
    fn a_roots_key_is_stored_in_it_and_shared_by_every_reader() {
        let tmp = crate::testing::TempDir::new();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        let mut w = World::human("");
        let mut key = |root: &std::path::Path, cwd: &str| {
            w.ctx.state_root = root.to_path_buf();
            w.ctx.cwd = cwd.into();
            root_key(&w.ctx)
        };
        let first = key(&a, "/work/a");
        assert_eq!(first.len(), 32, "{first}");
        assert!(first.bytes().all(|c| c.is_ascii_hexdigit()), "{first}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&a).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "the root is made private");
        }
        // The opener and the closer each read it: the same key, whatever their working directory.
        assert_eq!(key(&a, "/elsewhere"), first);
        assert_eq!(std::fs::read_to_string(a.join(ROOT_KEY)).unwrap(), first);
        // The same root through another spelling of its path; no staging file is left.
        assert_eq!(key(&a.join("."), "/work/b"), first);
        let names: Vec<String> = std::fs::read_dir(&a)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [ROOT_KEY], "{names:?}");
        // Another root, its own key.
        assert_ne!(key(&b, "/work/a"), first);
    }

    // A root that cannot hold a key gives each call one of its own, so no -Close matches its stray: a root that cannot
    // be made (a file stands where its folder would be), a `bar-key` that is no key (left untouched), one that is not a
    // regular file (a FIFO: never opened, so nothing blocks), and no random source at all (the process id).
    #[test]
    fn a_root_that_cannot_hold_a_key_gives_each_call_its_own() {
        let tmp = crate::testing::TempDir::new();
        let mut w = World::human("");
        let blocked = tmp.path().join("blocked");
        std::fs::write(&blocked, "x").unwrap();
        w.ctx.state_root = blocked.join("state");
        assert_ne!(root_key(&w.ctx), root_key(&w.ctx));
        let bad = root_holding(&tmp, "bad", "not hex");
        w.ctx.state_root = bad.clone();
        assert_ne!(root_key(&w.ctx), root_key(&w.ctx));
        assert_eq!(
            std::fs::read_to_string(bad.join(ROOT_KEY)).unwrap(),
            "not hex"
        );
        #[cfg(unix)]
        {
            let fifo = root_holding(&tmp, "fifo", "");
            std::fs::remove_file(fifo.join(ROOT_KEY)).unwrap();
            let made = std::process::Command::new("mkfifo")
                .arg(fifo.join(ROOT_KEY))
                .status()
                .unwrap();
            assert!(made.success());
            w.ctx.state_root = fifo;
            let at = std::time::Instant::now();
            assert_ne!(root_key(&w.ctx), root_key(&w.ctx));
            assert!(
                at.elapsed() < Duration::from_secs(2),
                "a FIFO blocked the read"
            );
        }
        w.ctx.state_root = tmp.path().join("norandom");
        let failing = |_: &mut [u8]| Err(std::io::Error::other("no random source"));
        assert_eq!(
            root_key_with(&w.ctx, &failing),
            format!("{:032x}", w.ctx.pid)
        );
        assert!(!tmp.path().join("norandom").join(ROOT_KEY).exists());
    }

    /// A state root `root_key` made (private as it makes it, on every OS), its `bar-key` then holding `text`.
    fn root_holding(tmp: &crate::testing::TempDir, name: &str, text: &str) -> std::path::PathBuf {
        let root = tmp.path().join(name);
        let mut w = World::human("");
        w.ctx.state_root = root.clone();
        root_key(&w.ctx);
        std::fs::write(root.join(ROOT_KEY), text).unwrap();
        root
    }

    // Two processes publishing at once: each stages its own key and hard-links it to `bar-key`; the link fails for the
    // second, which reads the first's back, so both end with the same key and no reader ever sees part of one.
    #[test]
    fn a_racing_second_writer_takes_the_first_writers_key() {
        let tmp = crate::testing::TempDir::new();
        // A root that already holds a key: it is taken as is.
        let first = "0123456789abcdef0123456789abcdef";
        let root = root_holding(&tmp, "root", first);
        let mut w = World::human("");
        w.ctx.state_root = root.clone();
        assert_eq!(root_key(&w.ctx), first);
        // A root with none: this call's key is published and read back.
        let empty = tmp.path().join("empty");
        w.ctx.state_root = empty.clone();
        let mine = root_key(&w.ctx);
        assert_eq!(std::fs::read_to_string(empty.join(ROOT_KEY)).unwrap(), mine);
        let again = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let root = tmp.path().join("race");
                    scope.spawn(move || {
                        let mut w = World::human("");
                        w.ctx.state_root = root;
                        root_key(&w.ctx)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(again.windows(2).all(|p| p[0] == p[1]), "{again:?}");
    }

    // The bar's split starts its watch as `watch -Managed <key>`, the key its state root's, so the watch knows the split
    // started it, whether or not its opener lives to mark it, and -Close knows which root's it is; that process is
    // still proven this binary's watch, recorded and labelled.
    #[test]
    fn the_bars_split_starts_its_watch_managed() {
        let (mut w, panes) = sidebar_world("", Mux::default());
        let tmp = crate::testing::TempDir::new();
        let root = tmp.path().join("state");
        w.ctx.state_root = root.clone();
        let exe = w.ctx.exe.display().to_string();
        let key = key_of(&mut w, &root);
        assert!(key.len() == 32 && key.bytes().all(|b| b.is_ascii_hexdigit()));
        let want = key.clone();
        w.fakes.mux_script("-l 40", move |argv| {
            assert_eq!(
                argv[argv.len() - 4..],
                [exe.as_str(), "watch", "-Managed", want.as_str()]
            );
            Ok(out(0, "%7|@1|107\n", ""))
        });
        w.fakes.forget(107);
        w.fakes.process(crate::proc::Row {
            argv: Some(vec![
                w.ctx.exe.display().to_string(),
                "watch".into(),
                "-Managed".into(),
                key,
            ]),
            ..watch_row(107, &w.ctx.exe.display().to_string())
        });
        set(&panes, "%7", "|".into());
        set(&panes, "pid %7", "107".into());
        assert_eq!(
            run(&w, &[]),
            r#"ok {"pane":"%7","window":"@1","opened":true}"#
        );
        assert_eq!(record(&panes).as_deref(), Some("%7 107 1070"));
        assert_eq!(roles(&panes), "%1=|,%2=other|,%7=sidebar|");
    }

    // T9 test 1: the record is written after the process check, before open returns.
    #[test]
    fn open_records_the_bar_once_its_process_is_this_watch() {
        let (w, panes) = sidebar_world("", Mux::default());
        assert_eq!(
            run(&w, &[]),
            r#"ok {"pane":"%7","window":"@1","opened":true}"#
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
        // A dead pane (once in this list) blocks nothing since the review: a_dead_bar_pane_blocks_no_later_bar.
        let damage: [(&str, Damage); 9] = [
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
                set(p, &wrec("@1"), "%7 108 1070".into())
            }),
            ("record names another pane", |p, _| {
                set(p, &wrec("@1"), "%2 107 1070".into())
            }),
            ("record malformed", |p, _| {
                set(p, &wrec("@1"), "%7 107".into())
            }),
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
        assert_eq!(
            calls(&w),
            [READ_OPTION, VIEWED_ON, SESSION_RECORD, READ_RECORD]
        );
        assert_eq!(
            run(&w, &[]),
            r#"ok {"pane":"%7","window":"@1","opened":false}"#
        );
        w.fakes.clear_log();
        assert_eq!(
            run(&w, &["-Close"]),
            r#"ok {"pane":"%7","closed":true,"panes":["%7"]}"#
        );
        assert_eq!(
            calls(&w),
            [
                OFF.to_string(),
                SESSION_RECORD.into(),
                WINDOWS.into(),
                READ_RECORD.into(),
                close_line("%7", 107),
                CLEAR_RECORD.into(),
                STRAYS.into(),
                SESSION_LEFT.into(),
            ]
        );
        assert_eq!(
            (option(&panes).as_deref(), record(&panes)),
            (Some("off"), None)
        );
        assert_eq!(roles(&panes), "%1=|,%2=other|");
        w.fakes.clear_log();
        assert_eq!(
            run(&w, &["-Close"]),
            r#"ok {"pane":null,"closed":false,"panes":[]}"#
        );
        assert_eq!(
            calls(&w),
            [
                OFF,
                SESSION_RECORD,
                WINDOWS,
                READ_RECORD,
                STRAYS,
                SESSION_LABELS
            ]
        );
        assert_eq!(
            run(&w, &[]),
            r#"ok {"pane":"%8","window":"@1","opened":true}"#
        );
        assert_eq!(
            (option(&panes), record(&panes).as_deref()),
            (None, Some("%8 108 1080"))
        );
        assert_eq!(ensure_run(&w), "existing %8");
    }

    // `q` asks `close bar? y/n`; a `y` read alone `BAR_GAP` after the question showed closes this window's bar as
    // `sidebar -Close` does (the session turned off, so no other bar reopens it; the proven bar killed, its record
    // cleared) and ends the watch. A `y` sooner leaves it asked; a refused kill is shown and the bar stays.
    #[test]
    fn q_then_a_late_lone_y_closes_the_bar_as_close_does() {
        let at = |ms: u64| SystemTime::UNIX_EPOCH + Duration::from_millis(ms);
        let key = |event: Event, first: SystemTime| keys::Read {
            event,
            first,
            alone: true,
        };
        for (case, mux) in [
            (
                "closes",
                Mux {
                    open: true,
                    ..Mux::default()
                },
            ),
            (
                "refused",
                Mux {
                    open: true,
                    kill_fail: Some("exit"),
                    ..Mux::default()
                },
            ),
            ("no bar", Mux::default()),
            (
                "window gone",
                Mux {
                    open: true,
                    ..Mux::default()
                },
            ),
        ] {
            let (w, panes) = sidebar_world("", mux);
            // The bar's pane was joined into another window, and its own window @1 went with its record.
            if case == "window gone" {
                w.fakes
                    .mux_first(READ_RECORD, 1, "", "can't find window: @1\n");
                w.fakes.mux_first("list-windows", 0, "@2\n", "");
            }
            let c = w.caller();
            let mut shown = Shown {
                written: SystemTime::UNIX_EPOCH,
                targets: Vec::new(),
                drawn: Vec::new(),
                width: 40,
                looking: None,
                team: false,
                done: Vec::new(),
            };
            let (mut pick, mut panel) = (Pick::default(), Panel::default());
            w.fakes.clear_log();
            assert!(act(
                &w.ctx,
                &c,
                PLAIN_LOOK,
                key(Event::Close, at(0)),
                &mut pick,
                &mut shown,
                &mut panel
            ));
            assert_eq!(pick.note().as_deref(), Some(pick::ASK_BAR), "{case}");
            pick.question_shown(at(1000));
            assert!(!act(
                &w.ctx,
                &c,
                PLAIN_LOOK,
                key(Event::Yes, at(1100)),
                &mut pick,
                &mut shown,
                &mut panel
            ));
            assert!(
                w.fakes.mux_calls().is_empty(),
                "{case}: asking reads nothing"
            );
            assert_eq!(
                pick.note().as_deref(),
                Some(pick::ASK_BAR),
                "{case}: too soon"
            );
            assert!(act(
                &w.ctx,
                &c,
                PLAIN_LOOK,
                key(Event::Yes, at(1250)),
                &mut pick,
                &mut shown,
                &mut panel
            ));
            match case {
                "closes" => {
                    assert_eq!(
                        calls(&w),
                        [
                            OFF.to_string(),
                            SESSION_RECORD.into(),
                            READ_RECORD.into(),
                            close_line("%7", 107),
                            CLEAR_RECORD.into()
                        ]
                    );
                    assert_eq!(
                        (option(&panes).as_deref(), record(&panes)),
                        (Some("off"), None)
                    );
                    assert!(panel.closed && pick.note().is_none());
                }
                "refused" => {
                    assert!(!panel.closed);
                    assert_eq!(pick.note().as_deref(), Some("close failed: side bar"));
                }
                "window gone" => {
                    assert_eq!(calls(&w), [OFF, SESSION_RECORD, READ_RECORD, WINDOWS]);
                    assert_eq!(closes(&w), 0);
                    assert!(
                        panel.closed,
                        "{case}: no record proves it, yet the watch ends, and its pane with it"
                    );
                }
                _ => {
                    assert_eq!(calls(&w), [OFF, SESSION_RECORD, READ_RECORD]);
                    assert!(
                        panel.closed,
                        "{case}: nothing to kill, the watch still ends"
                    );
                }
            }
        }
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
            r#"ok {"pane":"%8","window":"@1","opened":true}"#
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
                r#"ok {"pane":null,"closed":false,"panes":[]}"#,
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
                r#"ok {"pane":"%7","window":"@1","opened":false}"#,
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
                    "-sidebar|{key}|$3 display-message,list-panes,set-option,show-options,show-options,list-panes,list-panes,\
                     display-message,split-window,if-shell,if-shell,display-message,set-option,if-shell,display-message"
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

    // The bar by default: a person's chat that starts or resumes runs `sidebar -Auto -Source <word>`. It opens the
    // bar as `sidebar` does, but never undoes `sidebar -Close` and never reopens after `clear` or `compact`.
    #[test]
    fn auto_opens_the_bar_then_finds_it() {
        let (w, panes) = sidebar_world("", Mux::default());
        assert_eq!(
            run(&w, &["-Auto", "-Source", "startup"]),
            r#"ok {"pane":"%7","window":"@1","opened":true}"#
        );
        assert_eq!(roles(&panes), "%1=|,%2=other|,%7=sidebar|");
        // Never unsets the option, never marks the window viewed (a chat window holds no member view).
        let lines: Vec<String> = w.fakes.mux_calls().iter().map(|c| c.join(" ")).collect();
        assert!(!lines.iter().any(|l| l.ends_with(ON)), "{lines:?}");
        assert!(!lines.iter().any(|l| l.contains(VIEWED)), "{lines:?}");
        for source in ["resume", "new", "unknown"] {
            assert_eq!(
                run(&w, &["-Auto", "-Source", source]),
                r#"ok {"pane":"%7","window":"@1","opened":false}"#,
                "{source}"
            );
        }
    }

    #[test]
    fn auto_keeps_a_closed_bar_closed_and_skips_clear_and_compact() {
        let (w, panes) = sidebar_world(
            "",
            Mux {
                disabled: true,
                ..Mux::default()
            },
        );
        assert_eq!(
            run(&w, &["-Auto", "-Source", "startup"]),
            r#"ok {"pane":null,"opened":false,"skipped":"off"}"#
        );
        assert_eq!(option(&panes).as_deref(), Some("off"));
        assert_eq!(count(&w, "split-window"), 0);
        for source in ["clear", "compact"] {
            let (w, _) = sidebar_world("", Mux::default());
            assert_eq!(
                run(&w, &["-Auto", "-Source", source]),
                r#"ok {"pane":null,"opened":false,"skipped":"source"}"#,
                "{source}"
            );
            assert!(w.fakes.mux_calls().is_empty(), "{source}");
        }
    }

    // Copilot's `sessionStart` passes its JSON on stdin, `source` among its fields.
    #[test]
    fn auto_reads_the_source_from_stdin() {
        use crate::agents::session_report::hooks;
        for (body, want) in [
            (
                Some(r#"{"sessionId":"x","source":"compact"}"#),
                r#"ok {"pane":null,"opened":false,"skipped":"source"}"#,
            ),
            (
                Some(r#"{"sessionId":"x","source":"new"}"#),
                r#"ok {"pane":"%7","window":"@1","opened":true}"#,
            ),
            (
                Some("not json"),
                r#"ok {"pane":"%7","window":"@1","opened":true}"#,
            ),
            (None, r#"ok {"pane":"%7","window":"@1","opened":true}"#),
        ] {
            let (w, _) = sidebar_world("", Mux::default());
            hooks::stdin(body.map(str::as_bytes));
            assert_eq!(run(&w, &["-Auto", "-Stdin"]), want, "{body:?}");
        }
        hooks::stdin(None);
        let (w, _) = sidebar_world("", Mux::default());
        for args in [
            &["-Auto"][..],
            &["-Auto", "-Stdin", "-Source", "x"],
            &["-Close", "-Auto", "-Source", "x"],
            &["-Source", "x"],
            &["-Stdin"],
            &["-Source", "clear", "-Stdin"],
            &["-Close", "-Stdin"],
        ] {
            assert!(run(&w, args).starts_with("2 usage "), "{args:?}");
        }
    }

    #[test]
    fn auto_from_an_agent_pane_refuses_and_opens_nothing() {
        let (w, panes) = sidebar_world("aaaaaaaaaaaa", Mux::default());
        assert_eq!(
            run(&w, &["-Auto", "-Source", "startup"]),
            "2 tagged an agent pane cannot open the side bar"
        );
        assert_eq!(roles(&panes), "%1=|,%2=other|");
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
            r#"ok {"pane":"%7","window":"@1","opened":true}"#
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
            r#"ok {"pane":"%7","window":"@1","opened":true}"#
        );
        let mut want = vec![ON.to_string()];
        want.extend(opened(&w));
        assert_eq!(calls(&w), want);
        assert_eq!(roles(&panes), "%1=|,%2=other|,%7=sidebar|");
        // The pane command is four arguments, the exe (spaces and all), `watch`, `-Managed` and the root's key, never
        // one joined string.
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
            "@1",
        ]
        .map(String::from)
        .to_vec();
        expected.extend(crate::agents::spawn::pane_env(
            w.ctx.state_root.to_str().unwrap(),
            crate::state::root::HOST,
        ));
        expected.extend(
            [
                "-l",
                "40",
                "/opt/mux agents/bin/mux-agent",
                "watch",
                "-Managed",
            ]
            .map(String::from),
        );
        expected.push(root_key(&w.ctx));
        assert_eq!(split, [expected]);
    }

    // The session's panes for the watch's view read and board: worker %9 in `window` until the `after`-th probe,
    // then `then` (`closed`: no pane, `dead`: a dead pane, `fail`: an unread list).
    fn views_by_tick(w: &World, window: &'static str, after: usize, then: &'static str) {
        let fakes = w.fakes.clone();
        w.fakes.mux_script("#{pane_start_command}", move |argv| {
            let probed = fakes
                .mux_calls()
                .iter()
                .filter(|c| c.last().is_some_and(|a| a == WATCH_PROBE))
                .count();
            let row = format!("%9|{window}|300|aaaaaaaaaaaa|worker");
            let rows = [row.as_str()];
            let text = match (probed < after, then) {
                (true, _) => crate::agents::fixture::listed_dead(argv, &rows, "\n", &[]),
                (false, "closed") => crate::agents::fixture::listed_dead(argv, &[], "\n", &[]),
                (false, "dead") => crate::agents::fixture::listed_dead(argv, &rows, "\n", &["%9"]),
                _ => return Ok(out(1, "", "no server\n")),
            };
            Ok(out(0, &text, ""))
        });
    }

    const UNLABEL: &str = "-N if-shell -F -t %7 #{&&:#{==:#{pane_dead},0},#{==:#{pane_pid},4242}} set-option -p -u -t %7 \
                           @agent-role";
    const CLEAR_OWN: &str = "-N set-option -wu -t @1 @mux-agents-sidebar-id";

    // the recorded bar leaves once its window, having held a member view, lost its last one (closed, or dead):
    // under the session lock it unlabels its own pane and clears its window's record, then ends `view-closed`.
    #[test]
    fn a_bar_leaves_once_its_window_lost_its_last_view() {
        for gone in ["closed", "dead"] {
            let (w, _) = keep_world(&["live", "live", "live", "gone"], win("%1", in_place()));
            views_by_tick(&w, "@1", 2, gone);
            let fakes = w.fakes.clone();
            w.fakes
                .lock_probe(move |_| fakes.mux_calls().len().to_string());
            let (ended, frames) = watch_run(&w);
            assert_eq!((ended, frames.len()), ("view-closed", 1), "{gone}");
            let all: Vec<String> = w.fakes.mux_calls().iter().map(|c| c.join(" ")).collect();
            let at = |line: &str| all.iter().position(|c| c == line).unwrap();
            let key = w.caller().server_key;
            // The board takes each member's lock on its own; the bar's leave holds the session's sidebar lock.
            let locks: Vec<String> = w
                .fakes
                .locks()
                .into_iter()
                .filter(|l| l[1..].starts_with("sidebar|"))
                .collect();
            assert_eq!(locks.len(), 2, "{gone}");
            let held =
                |event: &str| -> usize { event.rsplit(' ').next().unwrap().parse().unwrap() };
            assert!(
                locks[0].starts_with(&format!("+sidebar|{key}|$3 ")),
                "{gone}"
            );
            assert!(held(&locks[0]) <= at(UNLABEL), "{gone}");
            assert!(
                at(UNLABEL) < at(CLEAR_OWN) && at(CLEAR_OWN) < held(&locks[1]),
                "{gone}"
            );
        }
    }

    // a bar stays while its window never held a view, while a view read fails, and when it is not the
    // recorded bar (a watch run by hand).
    #[test]
    fn a_bar_stays_unless_its_window_held_a_view_and_lost_it() {
        for (case, window, then, own) in [
            ("never held", "@5", "closed", true),
            ("unread list", "@1", "fail", true),
            ("hand-run", "@1", "closed", false),
        ] {
            let w = if own {
                keep_world(&["live", "live", "live", "gone"], win("%1", in_place())).0
            } else {
                watch_world(&["live", "live", "live", "gone"])
            };
            views_by_tick(&w, window, 2, then);
            let (ended, _) = watch_run(&w);
            assert_eq!(ended, "pane-closed", "{case}");
            let all: Vec<String> = w.fakes.mux_calls().iter().map(|c| c.join(" ")).collect();
            assert!(
                !all.iter().any(|c| c == UNLABEL || c == CLEAR_OWN),
                "{case}"
            );
        }
    }

    // step 6: each tick, the recorded bar gives every other view window of the session without a proven bar its
    // own (`ensure`, a view moved by break-pane); a window whose bar is proven is left alone. A window whose ensure
    // failed is not tried again by this watch, so a failing open never splits every tick.
    #[test]
    fn each_tick_another_view_window_without_a_bar_gets_one() {
        for proven_there in [false, true] {
            let (w, _) = keep_world(&["live", "live", "live", "gone"], win("%1", in_place()));
            crate::agents::core_fixture::member(
                &w,
                "cccccccccccc",
                json!({"name":"third","pane":"%10","startCommand":crate::agents::fixture::launch_command("%10")}),
            );
            w.fakes.mux_script("#{pane_start_command}", |argv| {
                let rows = [
                    "%9|@1|300|aaaaaaaaaaaa|worker",
                    "%10|@6|301|cccccccccccc|third",
                ];
                Ok(out(
                    0,
                    &crate::agents::fixture::listed_dead(argv, &rows, "\n", &[]),
                    "",
                ))
            });
            // @6's record names %11, this same watch process: proven there only when %11 stands in @6.
            w.fakes.mux_first(
                "show-options -wqv -t @6 @mux-agents-sidebar-id",
                0,
                "%11 4242 2000\n",
                "",
            );
            let at = if proven_there { "@6" } else { "@9" };
            w.fakes.mux_first(
                &format!("-t %11 {PROOF}"),
                0,
                &format!("%11|{at}|0|4242\n"),
                "",
            );
            // No pane of @6 carries the label; the split names no pane, so the open refuses.
            w.fakes.mux_first("#{==:#{@agent-role},sidebar}", 0, "", "");
            let (ended, frames) = watch_run(&w);
            assert_eq!((ended, frames.len()), ("pane-closed", 3), "{proven_there}");
            let splits: Vec<String> = w
                .fakes
                .mux_calls()
                .iter()
                .filter(|c| c[1] == "split-window")
                .map(|c| c.join(" "))
                .collect();
            if proven_there {
                assert!(splits.is_empty(), "{splits:?}");
            } else {
                assert_eq!(splits.len(), 1, "{splits:?}");
                assert!(splits[0].contains(" -t @6 "), "{splits:?}");
            }
            // The watch's own window @1 is never ensured by its tick.
            assert!(
                !w.fakes
                    .mux_calls()
                    .iter()
                    .any(|c| c[1] == "split-window" && c.iter().any(|a| a == "@1"))
            );
        }
    }

    // A faster refresh: the walk over the other view windows (two reads per window) runs on the first tick and every
    // WALK_EVERY ticks after, not on each one, so a bar's tick stays cheap.
    #[test]
    fn the_other_view_windows_are_walked_every_few_ticks() {
        let mut probes = vec!["live"; 11];
        probes.push("gone");
        let probes: &'static [&'static str] = Box::leak(probes.into_boxed_slice());
        let (w, _) = keep_world(probes, win("%1", in_place()));
        crate::agents::core_fixture::member(
            &w,
            "cccccccccccc",
            json!({"name":"third","pane":"%10","startCommand":crate::agents::fixture::launch_command("%10")}),
        );
        w.fakes.mux_script("#{pane_start_command}", |argv| {
            let rows = [
                "%9|@1|300|aaaaaaaaaaaa|worker",
                "%10|@6|301|cccccccccccc|third",
            ];
            Ok(out(
                0,
                &crate::agents::fixture::listed_dead(argv, &rows, "\n", &[]),
                "",
            ))
        });
        // @6's bar is proven: the walk only reads it.
        w.fakes.mux_first(
            "show-options -wqv -t @6 @mux-agents-sidebar-id",
            0,
            "%11 4242 2000\n",
            "",
        );
        w.fakes
            .mux_first(&format!("-t %11 {PROOF}"), 0, "%11|@6|0|4242\n", "");
        let (ended, frames) = watch_run(&w);
        assert_eq!((ended, frames.len()), ("pane-closed", 11));
        let walks = w
            .fakes
            .mux_calls()
            .iter()
            .filter(|c| {
                c.join(" ")
                    .ends_with("show-options -wqv -t @6 @mux-agents-sidebar-id")
            })
            .count();
        assert_eq!(
            walks,
            11usize.div_ceil(WALK_EVERY as usize),
            "ticks 1, 6, 11"
        );
    }

    // step 6: a window whose ensure failed (a lock held elsewhere, say) is tried again once a minute has passed.
    #[test]
    fn a_failed_window_is_tried_again_after_a_minute() {
        let mut probes = vec!["live"; 33];
        probes.push("gone");
        let probes: &'static [&'static str] = Box::leak(probes.into_boxed_slice());
        let (w, _) = keep_world(probes, win("%1", in_place()));
        crate::agents::core_fixture::member(
            &w,
            "cccccccccccc",
            json!({"name":"third","pane":"%10","startCommand":crate::agents::fixture::launch_command("%10")}),
        );
        w.fakes.mux_script("#{pane_start_command}", |argv| {
            let rows = [
                "%9|@1|300|aaaaaaaaaaaa|worker",
                "%10|@6|301|cccccccccccc|third",
            ];
            Ok(out(
                0,
                &crate::agents::fixture::listed_dead(argv, &rows, "\n", &[]),
                "",
            ))
        });
        w.fakes.mux_first("#{==:#{@agent-role},sidebar}", 0, "", "");
        let (ended, frames) = watch_run(&w);
        assert_eq!((ended, frames.len()), ("pane-closed", 33));
        // Ticks are 2 s apart: the first at once, the next 60 s later, no third within the run.
        let splits = w
            .fakes
            .mux_calls()
            .iter()
            .filter(|c| c[1] == "split-window" && c.iter().any(|a| a == "@6"))
            .count();
        assert_eq!(splits, 2);
    }

    // The tick never waits on the session's sidebar lock: a resume's post-launch bar step (`keep_bar`) holds it for
    // as long as it takes. With it held on another thread the bar draws its first frame at once, never after the
    // lock's 15 s wait: the walk only hands the window without a bar to the worker, which opens it on a later pass,
    // here once the lock is released after that frame.
    #[test]
    fn the_tick_draws_at_once_while_another_holds_the_sidebar_lock() {
        use std::sync::mpsc;
        let mut probes = vec!["live"; 2];
        probes.push("gone");
        let probes: &'static [&'static str] = Box::leak(probes.into_boxed_slice());
        let (w, _) = keep_world(probes, win("%1", in_place()));
        crate::agents::core_fixture::member(
            &w,
            "cccccccccccc",
            json!({"name":"third","pane":"%10","startCommand":crate::agents::fixture::launch_command("%10")}),
        );
        w.fakes.mux_script("#{pane_start_command}", |argv| {
            let rows = [
                "%9|@1|300|aaaaaaaaaaaa|worker",
                "%10|@6|301|cccccccccccc|third",
            ];
            Ok(out(
                0,
                &crate::agents::fixture::listed_dead(argv, &rows, "\n", &[]),
                "",
            ))
        });
        w.fakes.mux_first("#{==:#{@agent-role},sidebar}", 0, "", "");
        let root = w.ctx.state_root.parent().unwrap().join("locks");
        w.fakes.real_locks(&root);
        let caller = w.caller();
        let key = scope::shown_lock("sidebar", &caller);
        let (held_tx, held_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let other = World::human("");
            let mut ctx = other.fakes.ctx();
            ctx.clock = Box::new(crate::ctx::SystemClock);
            crate::state::lock::with_lock_at(&ctx, &root, &key, || {
                held_tx.send(()).unwrap();
                let _ = go_rx.recv();
                Ok(())
            })
            .unwrap();
        });
        held_rx.recv().unwrap();
        let split = |fakes: &Fakes| {
            (fakes.mux_calls().iter())
                .filter(|c| c[1] == "split-window" && c.iter().any(|a| a == "@6"))
                .count()
        };
        let start = std::time::Instant::now();
        let (mut first, mut split_at_first) = (None, None);
        let (mut go, mut holder, fakes) = (Some(go_tx), Some(holder), w.fakes.clone());
        run_watch(
            &w.ctx,
            &caller,
            Origin::Hand,
            Box::new(NoInput),
            &mut |_: &str| {
                if first.is_none() {
                    first = Some(start.elapsed());
                    split_at_first = Some(split(&fakes));
                    // The lock is released only once that frame is drawn.
                    go.take().unwrap().send(()).unwrap();
                    holder.take().unwrap().join().unwrap();
                }
                Ok(())
            },
        );
        let first = first.expect("a frame");
        assert!(
            first < Duration::from_millis(200),
            "the first frame came after {first:?}"
        );
        assert_eq!(split_at_first, Some(0), "no bar opened by the first frame");
        assert!(split(&w.fakes) > 0, "the worker opened it on a later pass");
    }

    // The tick never opens a bar itself (the reviewer's repro): another view window without one, the sidebar lock
    // free, and a `split-window` that takes 350 ms. The walk hands that window to the worker, so the first frame is
    // drawn within one poll interval, and the bar's split runs on a later pass.
    #[test]
    fn a_slow_split_for_another_window_never_delays_a_frame() {
        use std::time::Instant;
        let (w, _) = keep_world(&["live", "live", "gone"], win("%1", in_place()));
        crate::agents::core_fixture::member(
            &w,
            "cccccccccccc",
            json!({"name":"third","pane":"%10","startCommand":crate::agents::fixture::launch_command("%10")}),
        );
        w.fakes.mux_script("#{pane_start_command}", |argv| {
            let rows = [
                "%9|@1|300|aaaaaaaaaaaa|worker",
                "%10|@6|301|cccccccccccc|third",
            ];
            Ok(out(
                0,
                &crate::agents::fixture::listed_dead(argv, &rows, "\n", &[]),
                "",
            ))
        });
        w.fakes.mux_first("#{==:#{@agent-role},sidebar}", 0, "", "");
        let split_at = Rc::new(Cell::new(None::<Instant>));
        let at = split_at.clone();
        w.fakes.mux_script("split-window", move |_| {
            std::thread::sleep(Duration::from_millis(350));
            at.set(Some(Instant::now()));
            Ok(out(0, "", ""))
        });
        let caller = w.caller();
        let start = Instant::now();
        let mut first = None;
        run_watch(
            &w.ctx,
            &caller,
            Origin::Hand,
            Box::new(NoInput),
            &mut |_: &str| {
                first.get_or_insert_with(Instant::now);
                Ok(())
            },
        );
        let first = first.expect("a frame");
        let late = first - start;
        assert!(
            late < Duration::from_millis(200),
            "the first frame came after {late:?}"
        );
        let split = split_at.get().expect("the bar's split ran");
        assert!(split > first, "the split ran on a later pass");
    }

    // A watch never exits while its worker opens another window's bar: an open cut between its split and its mark
    // would leave an unmarked watch there, which `prove` takes for one started by hand. The walk hands @6 to the
    // worker, then the frame's write fails (`output-closed`, a way out before the loop reads the worker's answers),
    // and the watch returns only after that open finished: split, then marked.
    #[test]
    fn a_watch_returns_only_after_its_workers_open() {
        let (w, _) = keep_world(&["live", "gone"], win("%1", in_place()));
        crate::agents::core_fixture::member(
            &w,
            "cccccccccccc",
            json!({"name":"third","pane":"%10","startCommand":crate::agents::fixture::launch_command("%10")}),
        );
        w.fakes.mux_script("#{pane_start_command}", |argv| {
            let rows = [
                "%9|@1|300|aaaaaaaaaaaa|worker",
                "%10|@6|301|cccccccccccc|third",
            ];
            Ok(out(
                0,
                &crate::agents::fixture::listed_dead(argv, &rows, "\n", &[]),
                "",
            ))
        });
        w.fakes.mux_first("#{==:#{@agent-role},sidebar}", 0, "", "");
        w.fakes.mux_rule("split-window", 0, "%20|@6|4242\n", "");
        let caller = w.caller();
        let ended = run_watch(
            &w.ctx,
            &caller,
            Origin::Hand,
            Box::new(NoInput),
            &mut |_: &str| Err(io::Error::other("closed")),
        );
        assert_eq!(ended, "output-closed");
        let calls: Vec<String> = w.fakes.mux_calls().iter().map(|c| c.join(" ")).collect();
        let split = calls
            .iter()
            .position(|c| c.contains("split-window") && c.contains("@6"));
        let mark = calls
            .iter()
            .position(|c| c.contains(&format!("set-option -p -t %20 {MARK}")));
        assert!(
            split.is_some() && mark.is_some_and(|m| Some(m) > split),
            "split, then marked, before the watch returned: {calls:?}"
        );
    }

    // A watch that ends while its worker opens another window's bar keeps its terminal as the loop had it (raw, its
    // signal handlers armed) until that open is done: a second Ctrl-C meanwhile is a byte no one reads, never a
    // signal that kills the waiting watch before its worker marks the new bar.
    #[test]
    fn a_watch_keeps_its_terminal_until_its_workers_finish() {
        let (w, _) = keep_world(&["live", "gone"], win("%1", in_place()));
        crate::agents::core_fixture::member(
            &w,
            "cccccccccccc",
            json!({"name":"third","pane":"%10","startCommand":crate::agents::fixture::launch_command("%10")}),
        );
        w.fakes.mux_script("#{pane_start_command}", |argv| {
            let rows = [
                "%9|@1|300|aaaaaaaaaaaa|worker",
                "%10|@6|301|cccccccccccc|third",
            ];
            Ok(out(
                0,
                &crate::agents::fixture::listed_dead(argv, &rows, "\n", &[]),
                "",
            ))
        });
        w.fakes.mux_first("#{==:#{@agent-role},sidebar}", 0, "", "");
        let (input, log) = term_input(vec![]);
        let opened = log.clone();
        w.fakes.mux_script("split-window", move |_| {
            opened.borrow_mut().push("split".into());
            Ok(out(0, "%20|@6|4242\n", ""))
        });
        let caller = w.caller();
        let ended = run_watch(&w.ctx, &caller, Origin::Hand, input, &mut |_: &str| {
            Err(io::Error::other("closed"))
        });
        assert_eq!(ended, "output-closed");
        let log = log.borrow();
        let split = log.iter().position(|e| e == "split");
        let restore = log.iter().position(|e| e == "restore");
        assert!(
            split.is_some() && restore.is_some_and(|r| Some(r) > split),
            "the worker's split, then the terminal restored: {log:?}"
        );
    }

    // The watch probes made so far.
    fn probed(fakes: &Fakes) -> usize {
        fakes
            .mux_calls()
            .iter()
            .filter(|c| c.last().is_some_and(|a| a == WATCH_PROBE))
            .count()
    }

    // QA: break-pane moves the window's last view to @6. The bar, before it leaves, gives every other view window
    // without a proven bar its own (under the session lock, as a tick does), since no tick of it follows: one whose
    // ensure failed within the minute (`retried`: @6 held a third view from the first tick) is tried again then.
    #[test]
    fn a_leaving_bar_first_gives_the_other_view_windows_a_bar() {
        for retried in [false, true] {
            let (w, _) = keep_world(&["live", "live", "live", "gone"], win("%1", in_place()));
            if retried {
                crate::agents::core_fixture::member(
                    &w,
                    "cccccccccccc",
                    json!({"name":"third","pane":"%10","startCommand":crate::agents::fixture::launch_command("%10")}),
                );
            }
            let fakes = w.fakes.clone();
            w.fakes.mux_script("#{pane_start_command}", move |argv| {
                let at = if probed(&fakes) < 2 { "@1" } else { "@6" };
                let worker = format!("%9|{at}|300|aaaaaaaaaaaa|worker");
                let mut rows = vec![worker.as_str()];
                if retried {
                    rows.push("%10|@6|301|cccccccccccc|third");
                }
                Ok(out(
                    0,
                    &crate::agents::fixture::listed_dead(argv, &rows, "\n", &[]),
                    "",
                ))
            });
            // No pane of @6 carries the label; the split names no pane, so each open refuses.
            w.fakes.mux_first("#{==:#{@agent-role},sidebar}", 0, "", "");
            let (ended, frames) = watch_run(&w);
            assert_eq!((ended, frames.len()), ("view-closed", 1), "{retried}");
            let all: Vec<String> = w.fakes.mux_calls().iter().map(|c| c.join(" ")).collect();
            let splits: Vec<usize> = all
                .iter()
                .enumerate()
                .filter(|(_, c)| c.contains(" split-window ") && c.contains(" -t @6 "))
                .map(|(i, _)| i)
                .collect();
            assert_eq!(splits.len(), if retried { 2 } else { 1 }, "{retried}");
            let unlabel = all.iter().position(|c| c == UNLABEL).unwrap();
            assert!(splits.iter().all(|s| *s < unlabel), "{retried}");
        }
    }

    // QA: a view seen in the bar's own window counts from the first tick that sees it, recorded or not: a view
    // closed before the record proves the bar (the first tick ran before open recorded it) still lets it leave.
    const OWN_MARK_READ: &str = "-N display-message -p -t %7 #{pane_pid}|#{@mux-agents-bar}";
    const UNMARK: &str = "-N if-shell -F -t %7 #{&&:#{==:#{pane_dead},0},#{==:#{pane_pid},4242}} set-option -p -u -t %7 \
                          @mux-agents-bar";

    // redesign: a bar cleans up after itself. A watch never proven by the startup limit (10 s on the watch's own
    // clock: its window's record never named its pane) whose pane carries its own mark (an open that failed after the
    // split) removes the mark and ends `unproven`, so its pane closes with it. A watch run by hand (its pane not
    // marked for it) reads that once and only draws; an unread mark is read again on the next tick.
    #[test]
    fn a_watch_that_cannot_prove_itself_leaves_within_the_limit() {
        let mut probes = vec!["live"; 12];
        probes.push("gone");
        let probes: &'static [&'static str] = Box::leak(probes.into_boxed_slice());
        for (case, answers) in [
            ("ours", vec![(0, "4242|4242\n")]),
            ("hand-run", vec![(0, "4242|\n")]),
            ("unread first", vec![(1, ""), (0, "4242|4242\n")]),
        ] {
            let w = watch_world(probes);
            let queue = Rc::new(RefCell::new(VecDeque::from(answers)));
            w.fakes
                .mux_script("#{pane_pid}|#{@mux-agents-bar}", move |argv| {
                    assert_eq!(argv[4], "%7");
                    let mut queue = queue.borrow_mut();
                    let (code, text) = if queue.len() > 1 {
                        queue.pop_front().unwrap()
                    } else {
                        *queue.front().unwrap()
                    };
                    Ok(out(code, text, ""))
                });
            let (ended, frames) = watch_run(&w);
            let all: Vec<String> = w.fakes.mux_calls().iter().map(|c| c.join(" ")).collect();
            let reads = all.iter().filter(|c| *c == OWN_MARK_READ).count();
            let unmarked = all.iter().any(|c| c == UNMARK);
            let want = match case {
                "ours" => ("unproven", 5, 1, true),
                "hand-run" => ("pane-closed", 12, 1, false),
                _ => ("unproven", 6, 2, true),
            };
            assert_eq!((ended, frames.len(), reads, unmarked), want, "{case}");
            assert_eq!(all.iter().any(|c| c == UNLABEL), want.3, "{case}");
        }
    }

    // A watch the bar's split started (`watch -Managed`) is the bar's whether or not its pane was marked: its opener
    // may have died between the split and the mark (killed, or ended before its worker's open). Never proven by the
    // startup limit, it unlabels and unmarks its pane (a no-op when neither is set) and ends `unproven`, so its pane
    // closes with it. The same watch run by a person (`watch`) reads the missing mark as run by hand and only draws.
    #[test]
    fn a_watch_the_split_started_leaves_unproven_though_never_marked() {
        let mut probes = vec!["live"; 12];
        probes.push("gone");
        let probes: &'static [&'static str] = Box::leak(probes.into_boxed_slice());
        for (args, want) in [
            (
                &["watch", "-Managed", "0123456789abcdef"][..],
                ("unproven", 5, true),
            ),
            (&["watch"][..], ("pane-closed", 12, false)),
        ] {
            let w = watch_world(probes);
            w.fakes.mux_rule(OWN_MARK, 0, "4242|\n", "");
            let request = w.request(args);
            let mut frames = 0;
            let reply = watch_on(
                &w.ctx,
                &request,
                true,
                &|| Box::new(NoInput) as Box<dyn Input>,
                &mut |_| {
                    frames += 1;
                    assert!(frames <= 100, "runaway");
                    Ok(())
                },
            )
            .unwrap();
            w.fakes.hc_done().unwrap();
            let all: Vec<String> = w.fakes.mux_calls().iter().map(|c| c.join(" ")).collect();
            let cleaned = all.iter().any(|c| c == UNLABEL) && all.iter().any(|c| c == UNMARK);
            assert_eq!(
                (reply["ended"].as_str().unwrap(), frames, cleaned),
                want,
                "{args:?}"
            );
        }
    }

    // review: the startup limit runs from the last tick the bar was proven, not from a flag set once: a bar whose
    // window record is cleared after it was proven (open's last proof failed and cleared it) leaves within the limit,
    // unlabelled and unmarked, so no labelled pane is stranded beside a refusing retry.
    #[test]
    fn a_bar_whose_record_is_cleared_leaves_within_the_limit() {
        let mut probes = vec!["live"; 20];
        probes.push("gone");
        let probes: &'static [&'static str] = Box::leak(probes.into_boxed_slice());
        let (w, _) = keep_world(probes, win("%1", in_place()));
        views_by_tick(&w, "@1", 100, "closed");
        let fakes = w.fakes.clone();
        w.fakes.mux_script(RECORD, move |_| {
            Ok(out(0, if probed(&fakes) < 3 { OWN } else { "" }, ""))
        });
        w.fakes
            .mux_script(OWN_MARK, |_| Ok(out(0, "4242|4242\n", "")));
        let (ended, frames) = watch_run(&w);
        assert_eq!((ended, frames.len()), ("unproven", 6));
        let all: Vec<String> = w.fakes.mux_calls().iter().map(|c| c.join(" ")).collect();
        assert!(all.iter().any(|c| c == UNLABEL) && all.iter().any(|c| c == UNMARK));
    }

    // review: `Kept` needs the full proof the tick uses. A record naming this pane and pid with a stale start fails
    // every proof; it keeps nothing, and the bar leaves within the limit, unlabelled and unmarked.
    #[test]
    fn a_record_with_a_stale_start_keeps_no_bar() {
        let mut probes = vec!["live"; 12];
        probes.push("gone");
        let probes: &'static [&'static str] = Box::leak(probes.into_boxed_slice());
        let (w, _) = keep_world(probes, win("%1", in_place()));
        views_by_tick(&w, "@1", 100, "closed");
        w.fakes.mux_first(RECORD, 0, "%7 4242 1\n", "");
        w.fakes
            .mux_script(OWN_MARK, |_| Ok(out(0, "4242|4242\n", "")));
        let (ended, frames) = watch_run(&w);
        assert_eq!((ended, frames.len()), ("unproven", 5));
        let all: Vec<String> = w.fakes.mux_calls().iter().map(|c| c.join(" ")).collect();
        assert!(all.iter().any(|c| c == UNLABEL) && all.iter().any(|c| c == UNMARK));
    }

    // review, fail closed: a self-exit whose session lock is not taken, or whose window record cannot be read under
    // it, decides nothing: the bar keeps running, neither unlabelled nor unmarked, and tries again on the next tick.
    #[test]
    fn a_self_exit_without_its_lock_or_record_decides_nothing() {
        let mut probes = vec!["live"; 12];
        probes.push("gone");
        let probes: &'static [&'static str] = Box::leak(probes.into_boxed_slice());
        for case in ["lock", "record"] {
            let w = watch_world(probes);
            w.fakes
                .mux_script(OWN_MARK, |_| Ok(out(0, "4242|4242\n", "")));
            if case == "lock" {
                w.fakes.lock_timeout("sidebar|");
            } else {
                w.fakes.mux_first(RECORD, 1, "", "no server\n");
            }
            let (ended, frames) = watch_run(&w);
            assert_eq!((ended, frames.len()), ("pane-closed", 12), "{case}");
            let all: Vec<String> = w.fakes.mux_calls().iter().map(|c| c.join(" ")).collect();
            assert!(!all.iter().any(|c| c == UNLABEL || c == UNMARK), "{case}");
            // Tried again on each later tick.
            let tries = if case == "lock" {
                w.fakes
                    .locks()
                    .iter()
                    .filter(|l| l.starts_with("!sidebar|"))
                    .count()
            } else {
                all.iter().filter(|c| *c == OWN_MARK_READ).count()
            };
            assert!(tries >= 2, "{case}: {tries}");
        }
    }

    // review: the self-exit is decided under the session's sidebar lock, the record read again there: an ensure
    // that recorded (adopted) the bar while the bar waited on the lock keeps it running, neither unlabelled nor
    // unmarked.
    #[test]
    fn a_record_written_while_the_lock_is_held_keeps_the_bar() {
        let mut probes = vec!["live"; 12];
        probes.push("gone");
        let probes: &'static [&'static str] = Box::leak(probes.into_boxed_slice());
        let (w, _) = keep_world(probes, win("%1", in_place()));
        views_by_tick(&w, "@1", 100, "closed");
        let adopted = Rc::new(Cell::new(false));
        let a = adopted.clone();
        w.fakes.mux_script(RECORD, move |_| {
            Ok(out(0, if a.get() { OWN } else { "" }, ""))
        });
        let a = adopted.clone();
        w.fakes.lock_probe(move |event| {
            if event.starts_with("+sidebar|") {
                a.set(true);
            }
            String::new()
        });
        w.fakes
            .mux_script(OWN_MARK, |_| Ok(out(0, "4242|4242\n", "")));
        let (ended, _) = watch_run(&w);
        assert_eq!(ended, "pane-closed");
        assert!(adopted.get(), "the self-exit took the session lock");
        let all: Vec<String> = w.fakes.mux_calls().iter().map(|c| c.join(" ")).collect();
        assert!(!all.iter().any(|c| c == UNLABEL || c == UNMARK));
    }

    // QA: the window's `@mux-agents-viewed` (set by the verb that placed the view) makes it held, even when the
    // view is gone before the bar's first tick: the bar leaves at once, and unsets it.
    #[test]
    fn a_view_closed_before_the_first_tick_still_lets_the_bar_leave() {
        let (w, _) = keep_world(&["live", "live", "live", "gone"], win("%1", in_place()));
        views_by_tick(&w, "@1", 0, "closed");
        w.fakes.mux_first(VIEWED, 0, "1\n", "");
        let (ended, frames) = watch_run(&w);
        assert_eq!((ended, frames.len()), ("view-closed", 0));
        let all: Vec<String> = w.fakes.mux_calls().iter().map(|c| c.join(" ")).collect();
        assert!(
            all.iter()
                .any(|c| c == "-N set-option -wu -t @1 @mux-agents-viewed")
        );
        // It removes its pane's mark as it leaves, so a spawn racing it is not refused.
        assert!(all.iter().any(|c| c == UNMARK));
    }

    // reentry-6a E4.5, finding 11: a status window has no member view; its bar does not leave for want of views (it
    // runs until its own pane goes), unlike a viewed window (`a_view_closed_before_the_first_tick_still_lets_the_bar_leave`).
    #[test]
    fn a_status_windows_bar_stays_without_views() {
        let (w, _) = keep_world(&["live", "live", "live", "gone"], win("%1", in_place()));
        views_by_tick(&w, "@1", 0, "closed");
        w.fakes.mux_first(VIEWED, 0, "1\n", "");
        w.fakes.mux_rule(STATUS, 0, "1\n", "");
        let (ended, _) = watch_run(&w);
        assert_eq!(
            ended, "pane-closed",
            "it stayed until its own pane closed, never view-closed"
        );
    }

    #[test]
    fn a_view_seen_before_the_record_proves_the_bar_counts() {
        let (w, _) = keep_world(&["live", "live", "live", "gone"], win("%1", in_place()));
        views_by_tick(&w, "@1", 2, "closed");
        let fakes = w.fakes.clone();
        w.fakes.mux_script(RECORD, move |_| {
            Ok(out(0, if probed(&fakes) < 2 { "" } else { OWN }, ""))
        });
        let (ended, frames) = watch_run(&w);
        assert_eq!((ended, frames.len()), ("view-closed", 1));
    }

    // QA: a leave waits on the session lock, and a spawn or focus may reuse the bar meanwhile. Under the lock the
    // window is read again: a view there again keeps the bar, which neither unlabels nor clears its record.
    #[test]
    fn a_view_back_under_the_lock_keeps_the_bar() {
        let (w, _) = keep_world(&["live", "live", "live", "gone"], win("%1", in_place()));
        let back = Rc::new(Cell::new(false));
        let (fakes, b) = (w.fakes.clone(), back.clone());
        w.fakes.mux_script("#{pane_start_command}", move |argv| {
            let rows: &[&str] = if probed(&fakes) < 2 || b.get() {
                &["%9|@1|300|aaaaaaaaaaaa|worker"]
            } else {
                &[]
            };
            Ok(out(
                0,
                &crate::agents::fixture::listed_dead(argv, rows, "\n", &[]),
                "",
            ))
        });
        let b = back.clone();
        w.fakes.lock_probe(move |event| {
            if event.starts_with("+sidebar|") {
                b.set(true);
            }
            String::new()
        });
        let (ended, _) = watch_run(&w);
        assert_eq!(ended, "pane-closed");
        assert!(back.get());
        let all: Vec<String> = w.fakes.mux_calls().iter().map(|c| c.join(" ")).collect();
        assert!(!all.iter().any(|c| c == UNLABEL || c == CLEAR_OWN));
    }

    // QA on S1: only the watch the record names, by its own pane and its own process, keeps the layout; a watch run
    // by hand, with or without a bar, only draws.
    #[test]
    fn only_the_recorded_watch_keeps_the_layout() {
        // (record, the proof read of the watch's own pane): only its own pane, its own pid in that pane, alive, and
        // its own start keep the layout.
        for (record, proof) in [
            ("", "%7|@1|0|4242\n"),
            ("%8 4242 2000\n", "%7|@1|0|4242\n"),
            ("%7 4243 2000\n", "%7|@1|0|4242\n"),
            ("%7 4242\n", "%7|@1|0|4242\n"),
            // A stale start: the record names another process that once had this pid.
            ("%7 4242 1\n", "%7|@1|0|4242\n"),
            // The pane runs another process (a shell, with this watch run by hand in it), or is dead, or unread.
            ("%7 4242 2000\n", "%7|@1|0|170\n"),
            ("%7 4242 2000\n", "%7|@1|1|4242\n"),
            // Proven, but in another window than the watch's own: that window's record is not this one.
            ("%7 4242 2000\n", "%7|@5|0|4242\n"),
            ("%7 4242 2000\n", ""),
        ] {
            let (w, _) = keep_world(&["live", "live", "live", "gone"], win("%1", split_right()));
            w.fakes.mux_first(RECORD, 0, record, "");
            w.fakes.mux_first(PROOF, 0, proof, "");
            let record = format!("{record:?} {proof:?}");
            watch_frames(&w, 3);
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

    // ensure as the caller for `window`: as ensure_run.
    fn ensure_in(w: &World, window: &str) -> String {
        let caller = w.caller();
        match ensure(&w.ctx, &caller, window) {
            Ok(Bar::Opened(pane)) => format!("opened {pane}"),
            Ok(Bar::Existing(pane)) => format!("existing {pane}"),
            Ok(Bar::Disabled) => "disabled".into(),
            Err(r) => format!("{} {} {}", r.code, r.check, r.message),
        }
    }

    // each window keeps its own bar, found by its own window record, split in that window, under the one
    // session lock.
    #[test]
    fn each_window_keeps_its_own_bar() {
        let (w, panes) = sidebar_world("", Mux::default());
        assert_eq!(ensure_in(&w, "@1"), "opened %7");
        w.fakes.clear_log();
        let key = w.caller().server_key;
        assert_eq!(ensure_in(&w, "@2"), "opened %8");
        let split = split_line(&w, true).replace("-t @1", "-t @2");
        assert_eq!(
            calls(&w),
            [
                READ_OPTION.to_string(),
                VIEWED_ON.replace("@1", "@2"),
                SESSION_RECORD.into(),
                READ_RECORD.replace("@1", "@2"),
                LABELS.replace("@1", "@2"),
                MARKS.replace("@1", "@2"),
                split,
                mark_line("%8", 108),
                label_line("%8", 108),
                "-N set-option -w -t @2 @mux-agents-sidebar-id %8 108 1080".into(),
                resize_line("%8", 108, 40),
            ]
        );
        assert!(
            w.fakes
                .mux_calls()
                .iter()
                .any(|c| c.join(" ") == format!("-N display-message -p -t @2 {WINDOW_PROBE}"))
        );
        assert_eq!(
            (
                record(&panes).as_deref(),
                get(&panes, &wrec("@2")).as_deref()
            ),
            (Some("%7 107 1070"), Some("%8 108 1080"))
        );
        assert_eq!(get(&panes, RECORD), None);
        assert!(
            w.fakes
                .locks()
                .iter()
                .all(|l| l.starts_with(&format!("+sidebar|{key}|$3"))
                    || l.starts_with(&format!("-sidebar|{key}|$3")))
        );
        assert_eq!(ensure_in(&w, "@1"), "existing %7");
        assert_eq!(ensure_in(&w, "@2"), "existing %8");
    }

    // a window's record is that window's bar only while the proven bar stands in that window.
    #[test]
    fn a_record_naming_a_bar_in_another_window_is_not_that_windows_bar() {
        let (w, panes) = sidebar_world(
            "",
            Mux {
                open: true,
                ..Mux::default()
            },
        );
        // %7 is the proven bar of @1; @2's record names it too.
        set(&panes, &wrec("@2"), "%7 107 1070".into());
        assert_eq!(ensure_in(&w, "@2"), "opened %8");
        assert_eq!(get(&panes, &wrec("@2")).as_deref(), Some("%8 108 1080"));
        assert_eq!(closes(&w), 0);
        // %7 was moved to @5: @1's record no longer proves a bar in @1, and @1 holds no label, so @1 gets its own.
        set(&panes, "win %7", "@5".into());
        assert_eq!(ensure_in(&w, "@1"), "opened %9");
        assert_eq!(record(&panes).as_deref(), Some("%9 109 1090"));
        assert_eq!(closes(&w), 0);
    }

    // an unproven label refuses a bar in its own window only.
    #[test]
    fn an_unproven_label_refuses_only_its_own_window() {
        let (w, _) = sidebar_world(
            "",
            Mux {
                legacy: true,
                ..Mux::default()
            },
        );
        assert_eq!(ensure_in(&w, "@1"), refused(&["%7"]));
        assert_eq!(ensure_in(&w, "@2"), "opened %8");
    }

    // a 0.2.x session record moves once, under the session lock, to its proven bar's window; the session's is
    // unset, so the next look reads it no more.
    #[test]
    fn a_proven_session_record_moves_to_its_bars_window() {
        for window in ["@1", "@5"] {
            let (w, panes) = sidebar_world(
                "",
                Mux {
                    legacy: true,
                    extra: vec![(RECORD, "%7 107 1070".into()), ("win %7", window.into())],
                    ..Mux::default()
                },
            );
            let moved = format!("-N set-option -w -t {window} @mux-agents-sidebar-id %7 107 1070");
            let got = ensure_in(&w, "@1");
            assert_eq!(
                calls(&w)[..6],
                [
                    READ_OPTION.to_string(),
                    VIEWED_ON.into(),
                    SESSION_RECORD.into(),
                    moved,
                    CLEAR_SESSION.into(),
                    READ_RECORD.into()
                ],
                "{window}"
            );
            assert_eq!(get(&panes, RECORD), None, "{window}");
            assert_eq!(
                get(&panes, &wrec(window)).as_deref(),
                Some("%7 107 1070"),
                "{window}"
            );
            // The bar stays in its window: @1 keeps it, or gets its own beside a bar moved to @5.
            let want = if window == "@1" {
                "existing %7"
            } else {
                "opened %8"
            };
            assert_eq!(got, want, "{window}");
            assert_eq!(closes(&w), 0, "{window}");
            w.fakes.clear_log();
            assert_eq!(ensure_in(&w, "@1"), want.replace("opened", "existing"));
            assert_eq!(
                calls(&w),
                [READ_OPTION, VIEWED_ON, SESSION_RECORD, READ_RECORD],
                "{window}"
            );
        }
        // -Close migrates too, then closes the bar by its window record.
        let (w, panes) = sidebar_world(
            "",
            Mux {
                legacy: true,
                extra: vec![(RECORD, "%7 107 1070".into())],
                ..Mux::default()
            },
        );
        assert_eq!(
            run(&w, &["-Close"]),
            r#"ok {"pane":"%7","closed":true,"panes":["%7"]}"#
        );
        assert_eq!((get(&panes, RECORD), record(&panes)), (None, None));
    }

    // an unproven session record refuses while any pane of the session carries the label (it may be that
    // bar), in every window; with no label anywhere it names nothing and is cleared. An unreadable one refuses.
    #[test]
    fn an_unproven_session_record_refuses_while_the_session_holds_a_label_else_is_cleared() {
        let (w, panes) = sidebar_world(
            "",
            Mux {
                legacy: true,
                extra: vec![(RECORD, "%7 108 1070".into()), ("win %7", "@5".into())],
                ..Mux::default()
            },
        );
        for window in ["@1", "@2"] {
            assert_eq!(ensure_in(&w, window), refused(&["%7"]), "{window}");
        }
        assert_eq!(run(&w, &[]), refused(&["%7"]));
        assert_eq!(run(&w, &["-Close"]), refused(&["%7"]));
        assert_eq!(get(&panes, RECORD).as_deref(), Some("%7 108 1070"));
        assert_eq!((closes(&w), count(&w, "split-window")), (0, 0));
        assert!(
            w.fakes
                .mux_calls()
                .iter()
                .any(|c| c.join(" ") == SESSION_LABELS)
        );
        let (w, panes) = sidebar_world(
            "",
            Mux {
                extra: vec![(RECORD, "%9 109 1090".into())],
                ..Mux::default()
            },
        );
        assert_eq!(ensure_in(&w, "@1"), "opened %7");
        assert_eq!(get(&panes, RECORD), None);
        assert_eq!(record(&panes).as_deref(), Some("%7 107 1070"));
        let (w, _) = sidebar_world("", Mux::default());
        w.fakes
            .mux_first(SESSION_RECORD.trim_start_matches("-N "), 1, "", "no server");
        assert!(ensure_in(&w, "@1").starts_with("1 rmux "));
        assert_eq!(count(&w, "split-window"), 0);
    }

    // -Close lists the session's windows and closes the proven bar of each, clearing each window's record.
    #[test]
    fn close_closes_the_bar_of_every_window() {
        let (w, panes) = sidebar_world("", Mux::default());
        assert_eq!(ensure_in(&w, "@1"), "opened %7");
        assert_eq!(ensure_in(&w, "@2"), "opened %8");
        w.fakes.clear_log();
        assert_eq!(
            run(&w, &["-Close"]),
            r#"ok {"pane":"%7","closed":true,"panes":["%7","%8"]}"#
        );
        assert_eq!(
            calls(&w),
            [
                OFF.to_string(),
                SESSION_RECORD.into(),
                WINDOWS.into(),
                READ_RECORD.into(),
                close_line("%7", 107),
                CLEAR_RECORD.into(),
                READ_RECORD.replace("@1", "@2"),
                close_line("%8", 108),
                CLEAR_RECORD.replace("@1", "@2"),
                STRAYS.into(),
                SESSION_LEFT.into(),
            ]
        );
        assert_eq!((record(&panes), get(&panes, &wrec("@2"))), (None, None));
        assert_eq!(roles(&panes), "%1=|,%2=other|");
        assert_eq!(option(&panes).as_deref(), Some("off"));
        // Only @2 holds a bar: it is the one closed.
        let (w, _) = sidebar_world("", Mux::default());
        assert_eq!(ensure_in(&w, "@2"), "opened %7");
        assert_eq!(
            run(&w, &["-Close"]),
            r#"ok {"pane":"%7","closed":true,"panes":["%7"]}"#
        );
    }

    /// The key `root` holds (`root_key` on a world rooted there): the same for every reader of that root.
    fn key_of(w: &mut World, root: &std::path::Path) -> String {
        let saved = std::mem::replace(&mut w.ctx.state_root, root.to_path_buf());
        let key = root_key(&w.ctx);
        w.ctx.state_root = saved;
        key
    }

    // A watch the bar's split started (`watch -Managed <key>`) whose opener died before it marked, labelled or recorded
    // it is the session's bar all the same: -Close kills it, proven by its process, so no stray outlives the close even
    // within the startup limit. Only its own state root's: a managed watch with another root's key (a bar of
    // MUX_AGENTS_STATE=B) is spared, and closes itself at its own limit. A watch a person ran (`watch`) is never one.
    #[test]
    fn close_kills_a_watch_the_split_started_but_never_marked() {
        let (mut w, panes) = sidebar_world("", Mux::default());
        let tmp = crate::testing::TempDir::new();
        let (root, another) = (tmp.path().join("state"), tmp.path().join("another"));
        w.ctx.state_root = root.clone();
        let exe = w.ctx.exe.display().to_string();
        let managed = |key: &str| vec![exe.clone(), "watch".into(), "-Managed".into(), key.into()];
        let (mine, other) = (key_of(&mut w, &root), key_of(&mut w, &another));
        assert_ne!(other, mine);
        for (pane, pid, argv) in [
            ("%9", 109, managed(&mine)),
            ("%10", 110, vec![exe.clone(), "watch".into()]),
            ("%11", 111, managed(&other)),
        ] {
            set(&panes, pane, "|".into());
            set(&panes, &format!("pid {pane}"), pid.to_string());
            set(&panes, &format!("win {pane}"), "@2".into());
            w.fakes.process(crate::proc::Row {
                argv: Some(argv),
                ..watch_row(pid, &exe)
            });
        }
        assert_eq!(
            run(&w, &["-Close"]),
            r#"ok {"pane":"%9","closed":true,"panes":["%9"]}"#
        );
        assert_eq!(closes(&w), 1);
        assert_eq!(roles(&panes), "%1=|,%2=other|,%10=|,%11=|");
        assert_eq!(option(&panes).as_deref(), Some("off"));
    }

    // A window destroyed between -Close's list of the session's windows and the read of its record (its last pane
    // went) took its bar with it: the read fails, a new list no longer names it, and the close goes on to the next
    // window. A failed read of a window still listed refuses, as does a list that cannot be read again.
    #[test]
    fn close_skips_a_window_gone_before_its_record_is_read() {
        for case in ["gone", "listed", "unlisted"] {
            let (w, panes) = sidebar_world("", Mux::default());
            assert_eq!(ensure_in(&w, "@2"), "opened %7", "{case}");
            assert_eq!(ensure_in(&w, "@1"), "opened %8", "{case}");
            w.fakes.clear_log();
            let (p, fakes) = (panes.clone(), w.fakes.clone());
            w.fakes.mux_script(
                "show-options -wqv -t @2 @mux-agents-sidebar-id",
                move |_| {
                    if case != "listed" {
                        for key in ["%7", "pid %7", "win %7"] {
                            unset(&p, key);
                        }
                    }
                    if case == "unlisted" {
                        fakes.mux_first("list-windows", 1, "", "no server\n");
                    }
                    Ok(out(1, "", "can't find window: @2\n"))
                },
            );
            let got = run(&w, &["-Close"]);
            if case == "gone" {
                assert_eq!(
                    got, r#"ok {"pane":"%8","closed":true,"panes":["%8"]}"#,
                    "{case}"
                );
                assert_eq!(
                    calls(&w),
                    [
                        OFF.to_string(),
                        SESSION_RECORD.into(),
                        WINDOWS.into(),
                        READ_RECORD.into(),
                        close_line("%8", 108),
                        CLEAR_RECORD.into(),
                        READ_RECORD.replace("@1", "@2"),
                        WINDOWS.into(),
                        STRAYS.into(),
                        SESSION_LEFT.into(),
                    ],
                    "{case}"
                );
            } else {
                assert_eq!(got, "1 rmux tmux call failed", "{case}");
            }
            assert_eq!(closes(&w), 1, "{case}");
        }
    }

    // A window can go because its last pane moved, not died: `join-pane` of the bar into another window takes the
    // bar's label and mark with it but not its record (a window option), so no record proves it any more. -Close
    // closes the other bars, then finds the moved one still labelled and marked for its own pid: refused, naming it,
    // never reported closed. The same holds for a join made before -Close ran (that window is not even listed).
    #[test]
    fn close_refuses_a_bar_whose_pane_left_a_window_gone_mid_close() {
        let (w, panes) = sidebar_world("", Mux::default());
        assert_eq!(ensure_in(&w, "@2"), "opened %7");
        assert_eq!(ensure_in(&w, "@1"), "opened %8");
        w.fakes.clear_log();
        let p = panes.clone();
        // Just before @2's record is read, its bar %7 is joined into @1: @2 is destroyed with its record.
        w.fakes.mux_script(
            "show-options -wqv -t @2 @mux-agents-sidebar-id",
            move |_| {
                set(&p, "win %7", "@1".into());
                unset(&p, &wrec("@2"));
                Ok(out(1, "", "can't find window: @2\n"))
            },
        );
        assert_eq!(run(&w, &["-Close"]), refused(&["%7"]));
        assert_eq!(closes(&w), 1, "only %8 was killed");
        assert!(roles(&panes).contains("%7=sidebar|"), "{}", roles(&panes));
        assert_eq!(calls(&w).last().map(String::as_str), Some(SESSION_LEFT));
        // Joined before -Close ran: @2 is not listed, @1's bar %8 is closed, and %7 is refused all the same.
        let (w, panes) = sidebar_world("", Mux::default());
        assert_eq!(ensure_in(&w, "@2"), "opened %7");
        assert_eq!(ensure_in(&w, "@1"), "opened %8");
        set(&panes, "win %7", "@1".into());
        unset(&panes, &wrec("@2"));
        assert_eq!(run(&w, &["-Close"]), refused(&["%7"]));
        assert_eq!(closes(&w), 1, "only %8 was killed");
    }

    // A label left by a closed bar is no bar: rmux's rotate-window moves the bar's label and its mark (the bar's pid)
    // onto the pane it swaps with, and a 0.2.1 bar carries no mark. -Close that closed a bar refuses neither; only a
    // pane labelled and marked for its own pid is a bar still running.
    #[test]
    fn close_that_closed_a_bar_is_not_refused_by_a_label_left_behind() {
        for (case, extra) in [
            (
                "moved by rotate",
                vec![("mark %2", "107".into()), ("pid %2", "102".into())],
            ),
            (
                "0.2.1 bar",
                vec![
                    ("%9", "sidebar|".into()),
                    ("pid %9", "109".into()),
                    ("win %9", "@5".into()),
                ],
            ),
        ] {
            let (w, panes) = sidebar_world(
                "",
                Mux {
                    open: true,
                    extra,
                    ..Mux::default()
                },
            );
            if case == "moved by rotate" {
                set(&panes, "%2", "sidebar|".into());
            }
            assert_eq!(
                run(&w, &["-Close"]),
                r#"ok {"pane":"%7","closed":true,"panes":["%7"]}"#,
                "{case}"
            );
            assert_eq!(
                calls(&w).last().map(String::as_str),
                Some(SESSION_LEFT),
                "{case}"
            );
        }
    }

    // 0.2.x's session record proven in a window destroyed before the record moves there (its last pane went) is as
    // unproven as one proven nowhere: unset while no pane of the session carries the label, refused while one does
    // (the bar's pane moved to another window). A failed move to a window still listed refuses; each refusal keeps
    // the record.
    #[test]
    fn a_session_record_whose_window_went_before_the_move_is_unproven() {
        for case in ["gone", "moved", "listed"] {
            let (w, panes) = sidebar_world(
                "",
                Mux {
                    legacy: true,
                    extra: vec![(RECORD, "%7 107 1070".into()), ("win %7", "@5".into())],
                    ..Mux::default()
                },
            );
            let p = panes.clone();
            w.fakes
                .mux_script("set-option -w -t @5 @mux-agents-sidebar-id", move |_| {
                    match case {
                        "gone" => {
                            for key in ["%7", "pid %7", "win %7"] {
                                unset(&p, key);
                            }
                        }
                        "moved" => set(&p, "win %7", "@1".into()),
                        _ => {}
                    }
                    Ok(out(1, "", "can't find window: @5\n"))
                });
            let got = run(&w, &["-Close"]);
            let (want, record) = match case {
                "gone" => (
                    r#"ok {"pane":null,"closed":false,"panes":[]}"#.to_string(),
                    None,
                ),
                "moved" => (refused(&["%7"]), Some("%7 107 1070")),
                _ => ("1 rmux tmux call failed".to_string(), Some("%7 107 1070")),
            };
            assert_eq!(got, want, "{case}");
            assert_eq!(get(&panes, RECORD).as_deref(), record, "{case}");
            assert_eq!(closes(&w), 0, "{case}");
        }
    }

    // redesign: no open or ensure kills a pane it did not just split. In the recorded argv a kill appears only in
    // `sidebar -Close` (the proven bar) and in a spawn whose tagging failed (its own new view).
    #[test]
    fn the_recorded_argv_kills_only_in_close_and_a_failed_spawn() {
        let fixture = include_str!("../tests/fixtures/mux-argv.txt");
        let mut scenario = "";
        for line in fixture.lines() {
            if line.starts_with('[') {
                scenario = line;
            } else if !line.starts_with('#') && line.contains("kill-pane") {
                assert!(
                    matches!(
                        scenario,
                        "[sidebar -Close]" | "[spawn -Client claude -Task hi, tagging fails]"
                    ),
                    "{scenario}: {line}"
                );
            }
        }
    }

    // QA: `ensure` (spawn, focus and resume keep their view's bar through it) marks the window viewed, under the
    // session's sidebar lock, before the bar is found or opened.
    #[test]
    fn ensure_marks_its_window_viewed_under_the_lock() {
        let (w, _) = sidebar_world("", Mux::default());
        let fakes = w.fakes.clone();
        w.fakes
            .lock_probe(move |_| fakes.mux_calls().len().to_string());
        assert_eq!(ensure_run(&w), "opened %7");
        let all: Vec<String> = w.fakes.mux_calls().iter().map(|c| c.join(" ")).collect();
        let at = all
            .iter()
            .position(|c| c == "-N set-option -w -t @1 @mux-agents-viewed 1")
            .expect("the window is marked viewed");
        let locks = w.fakes.locks();
        let held = |event: &str| -> usize { event.rsplit(' ').next().unwrap().parse().unwrap() };
        assert!(held(&locks[0]) <= at && at < held(&locks[1]), "{locks:?}");
        assert!(
            at < all
                .iter()
                .position(|c| c.contains(" split-window "))
                .unwrap()
        );
    }

    // review: a dead pane (remain-on-exit on, a bar that left) blocks no later bar, whatever its options: with
    // the side-bar label and its mark, or the mark alone, the next ensure opens a bar.
    #[test]
    fn a_dead_bar_pane_blocks_no_later_bar() {
        for role in ["sidebar|", "|"] {
            let (w, panes) = sidebar_world(
                "",
                Mux {
                    extra: vec![
                        ("%7", role.into()),
                        ("pid %7", "107".into()),
                        ("mark %7", "107".into()),
                        ("dead %7", "1".into()),
                        ("next", "8".into()),
                    ],
                    ..Mux::default()
                },
            );
            assert_eq!(ensure_run(&w), "opened %8", "{role}");
            assert!(
                get(&panes, "%7").is_some(),
                "{role}: the dead pane is left alone"
            );
            assert_eq!(closes(&w), 0, "{role}");
        }
        // Recorded as the window's bar too: a dead pane is never proven, so it is not the bar and blocks nothing.
        let (w, panes) = sidebar_world(
            "",
            Mux {
                open: true,
                extra: vec![("dead %7", "1".into())],
                ..Mux::default()
            },
        );
        assert_eq!(ensure_run(&w), "opened %8");
        assert!(get(&panes, "%7").is_some());
        assert_eq!(closes(&w), 0);
    }

    #[test]
    fn ensure_opens_one_bar_then_finds_it() {
        let (w, panes) = sidebar_world("", Mux::default());
        assert_eq!(ensure_run(&w), "opened %7");
        let mut want = vec![READ_OPTION.to_string(), VIEWED_ON.to_string()];
        want.extend(opened(&w));
        assert_eq!(calls(&w), want);
        w.fakes.clear_log();
        assert_eq!(ensure_run(&w), "existing %7");
        assert_eq!(
            calls(&w),
            [READ_OPTION, VIEWED_ON, SESSION_RECORD, READ_RECORD]
        );
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
            assert!(ensure(&w.ctx, &caller, &caller.window).is_ok());
            let key = caller.server_key;
            let inside = if disabled {
                "show-options"
            } else {
                "show-options,set-option,show-options,show-options,list-panes,list-panes,display-message,split-window,if-shell,\
                 if-shell,display-message,set-option,if-shell,display-message"
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
        assert_eq!(
            run(&w, &["-Close"]),
            r#"ok {"pane":"%7","closed":true,"panes":["%7"]}"#
        );
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
            r#"ok {"pane":"%8","window":"@1","opened":true}"#
        );
        assert_eq!(option(&panes), None);
        assert_eq!(
            w.fakes.locks()[1],
            format!(
                "-sidebar|{key}|$3 {ON},{},{},-N set-option -w -t @1 @mux-agents-sidebar-id %8 108 1080,{}",
                mark_line("%8", 108),
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
    #[test]
    fn watch_hc_route_consumes_one_snapshot_per_frame() {
        use crate::agents::core_fixture as cf;
        let w = watch_world(&["live", "live", "gone"]);
        let c = w.caller();
        let a = cf::member(
            &w,
            ID,
            json!({"name":"worker","pane":"%9","startCommand":crate::agents::fixture::launch_command("%9")}),
        );
        let b = cf::member(&w, "bbbbbbbbbbbb", json!({"name":"done","pane":"%8"}));
        for _ in 0..2 {
            cf::online(&w);
            cf::listing(&w, &crate::agents::membership::team(&c), &[]);
            cf::get(&w, "taaaaaaaaaaaa", cf::wire(&a));
            let mut t = cf::wire(&b);
            t["state"] = json!("finished");
            t["reason"] = json!("exited");
            t["exit_code"] = json!(0);
            cf::get(&w, "tbbbbbbbbbbbb", t);
        }
        let (end, frames) = watch_run(&w);
        assert_eq!(end, "pane-closed");
        assert_eq!(frames.len(), 2);
        for frame in frames {
            // The ended member read through hc is counted on the folded line.
            assert!(frame.contains("worker"));
            assert!(frame.contains("1 working"));
            assert!(frame.contains("Ended \u{b7} 1"));
            assert_eq!(frame, board_frame());
        }
        assert_eq!(w.fakes.hc_calls().len(), 8);
        w.fakes.hc_done().unwrap();
    }

    #[test]
    fn keys_walk_the_whole_tree_and_the_layout_keeps_the_selection() {
        let board = tree::tests::room();
        let walk = tree::walk(&board);
        let (mut pick, mut keys) = (Pick::default(), Decoder::default());
        pick.set_wrap(true);
        let mut seen = Vec::new();
        let downs: [&[u8]; 9] = [
            b"j", b"\x1b[B", b"j", b"\x1bOB", b"j", b"j", b"j", b"j", b"j",
        ];
        for bytes in downs {
            for read in keys.push(bytes, SystemTime::UNIX_EPOCH) {
                pick.key(read.event, &walk);
            }
            seen.push(pick.selected().unwrap().to_string());
        }
        assert_eq!(
            seen,
            ["L", "A", "B", "C", "C1", "C2", "D", "D1", "L"],
            "leader, child, grandchild, next child; wraps; ended and viewless agents too"
        );
        let ups: [&[u8]; 2] = [b"k", b"\x1b[A"];
        for bytes in ups {
            for read in keys.push(bytes, SystemTime::UNIX_EPOCH) {
                pick.key(read.event, &walk);
            }
        }
        assert_eq!(pick.selected(), Some("D"), "k wraps to D1, Up goes to D");
        let ids = |h: usize, shown: Option<&str>, selected: Option<&str>| {
            lay_focus(
                Some(&board),
                &[],
                Size {
                    width: 40,
                    height: h,
                },
                PLAIN_LOOK,
                tree::Focus {
                    shown,
                    selected,
                    ended_open: false,
                },
            )
            .0
            .agents
            .iter()
            .map(|(t, _)| t.id.clone())
            .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(3, Some("%15"), Some("D1")),
            ["L", "D1"],
            "fallback: the selection on another branch than the shown agent"
        );
        assert_eq!(
            ids(3, None, Some("D1")),
            ["L", "D1"],
            "fallback: no agent shown"
        );
        // The same with the bar's last rows, as `run_watch` draws a selection: the row selected on another branch stays
        // drawn.
        let size = Size {
            width: 40,
            height: 3,
        };
        let focus = tree::Focus {
            shown: Some("%15"),
            selected: Some("D1"),
            ended_open: false,
        };
        let mut seen = lay_focus(Some(&board), &[], size, PLAIN_LOOK, focus).0;
        mark(&mut seen, "D1", 40, PLAIN_LOOK);
        dress(
            &mut seen,
            &[],
            size,
            PLAIN_LOOK,
            Dress::default(),
            &Keymap::default(),
        );
        assert!(
            seen.rows.iter().any(|r| render(r, false).starts_with('>')),
            "through hinted: {:?}",
            seen.rows.len()
        );
        assert!(
            !ids(7, Some("%15"), None).contains(&"C2".to_string()),
            "C2 is folded at height 7"
        );
        assert!(
            ids(7, Some("%15"), Some("C2")).contains(&"C2".to_string()),
            "a folded agent reached by the keys is shown"
        );
        // n/N select the next/previous agent with a live view (C2 has none), never focusing; Esc clears.
        let w = World::human("");
        let c = w.caller();
        w.fakes.clear_log();
        let mut shown = Shown {
            written: SystemTime::UNIX_EPOCH,
            targets: walk.clone(),
            drawn: Vec::new(),
            width: 40,
            looking: Some("%15".into()),
            team: true,
            done: Vec::new(),
        };
        let mut pick = Pick::default();
        let mut step = |event: Event, pick: &mut Pick| {
            act(
                &w.ctx,
                &c,
                PLAIN_LOOK,
                keys::Read {
                    event,
                    first: SystemTime::UNIX_EPOCH,
                    alone: true,
                },
                pick,
                &mut shown,
                &mut Panel::default(),
            );
            pick.selected().map(str::to_string)
        };
        let got: Vec<_> = [
            Event::Next,
            Event::Prev,
            Event::Next,
            Event::Next,
            Event::Next,
            Event::Prev,
            Event::Esc,
        ]
        .into_iter()
        .map(|e| step(e, &mut pick))
        .collect();
        let want = [
            Some("D"),
            Some("C1"),
            Some("D"),
            Some("D1"),
            Some("L"),
            Some("D1"),
            None,
        ]
        .map(|s| s.map(str::to_string));
        assert_eq!(got, want);
        assert!(
            w.fakes.mux_calls().is_empty() && w.fakes.hc_calls().is_empty(),
            "n/N and Esc focus nothing"
        );
        // ASCII alike: `#` marks the shown agent, `>` the selection.
        let ascii = Look {
            ascii: true,
            ..PLAIN_LOOK
        };
        let mut laid = lay_focus(
            Some(&board),
            &[],
            Size {
                width: 40,
                height: 24,
            },
            ascii,
            tree::Focus {
                shown: Some("%15"),
                selected: Some("D"),
                ended_open: false,
            },
        )
        .0;
        mark(&mut laid, "D", 40, ascii);
        let text: Vec<String> = laid.rows.iter().map(|r| render(r, false)).collect();
        assert!(
            text.iter().any(|l| l.starts_with("     * C1"))
                && text.iter().any(|l| l.starts_with(">  * D")),
            "{text:?}"
        );
    }

    #[test]
    fn a_team_bar_narrower_than_12_still_asks_to_be_widened() {
        use tree::tests::{member, team};
        let board = team(vec![
            member("L", None, 1, json!({})),
            member("A", Some("L"), 2, json!({})),
            member("G", Some("A"), 3, json!({})),
            member(
                "Z",
                Some("G"),
                4,
                json!({"name": "zed", "state": "exited 2147483647", "life": "ended", "exitCode": 2147483647,
                "pane": null, "unread": 9}),
            ),
        ]);
        // Z needs 34. The decorated forms keep their margin; at 3 `34` fits unindented with the last column free; at 1 the title alone.
        for (width, note) in [
            (33, Some(" widen the bar: needs 34 columns")),
            (20, Some(" widen: 34 columns")),
            (11, Some(" 34")),
            (4, Some(" 34")),
            (3, Some("34")),
            (1, None),
        ] {
            for look in [
                PLAIN_LOOK,
                Look {
                    ascii: true,
                    ..PLAIN_LOOK
                },
            ] {
                let laid = lay_focus(
                    Some(&board),
                    &[],
                    Size { width, height: 24 },
                    look,
                    tree::Focus::default(),
                )
                .0;
                let text: Vec<String> = laid.rows.iter().map(|r| render(r, false)).collect();
                assert!(laid.agents.is_empty(), "{width}: {text:?}");
                assert!(
                    laid.rows.iter().all(|r| row_width(r) < width.max(2)),
                    "{width}: the last column stays free: {text:?}"
                );
                assert_eq!(text.get(1).map(String::as_str), note, "{width}: {text:?}");
                if width < 12 {
                    let title = text[0].trim().trim_end_matches("...");
                    assert!(
                        "mux-agents".starts_with(title),
                        "{width}: the cut title: {text:?}"
                    );
                }
            }
        }
    }

    // review (PM): a queued popup that never opened leaves its peek file. The bar sweeps its own peek files older
    // than 10 minutes when it starts and when it opens a popup; a younger one may be a popup still open, so it stays.
    #[test]
    fn the_bar_sweeps_only_its_own_peek_files_older_than_10_minutes() {
        let w = watch_world(&["gone"]);
        w.fakes.advance(Duration::from_secs(3600));
        let caller = w.caller();
        let folder = crate::agents::server_dir(&w.ctx, &caller);
        std::fs::create_dir_all(&folder).unwrap();
        let now = w.ctx.clock.now();
        let file = |name: &str, age: u64| {
            let path = folder.join(name);
            std::fs::write(&path, "x").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(now - Duration::from_secs(age))
                .unwrap();
            path
        };
        let kept =
            |paths: &[&std::path::PathBuf]| paths.iter().map(|p| p.exists()).collect::<Vec<_>>();
        // Directly: only a peek file older than PEEK_STALE goes.
        let (stale, fresh, edge, other) = (
            file("peek-a.txt", 601),
            file("peek-b.txt", 30),
            file("peek-c.txt", 599),
            file("notes-d.txt", 3000),
        );
        sweep_peeks(&w.ctx, &folder);
        assert_eq!(
            kept(&[&stale, &fresh, &edge, &other]),
            [false, true, true, true]
        );
        // When the bar opens a popup: the stale file goes, the young one and the popup's own new file stay.
        let stale = file("peek-e.txt", 900);
        popup(&w.ctx, &caller, &folder, "worker", "text").unwrap();
        let peeks = || {
            std::fs::read_dir(&folder)
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("peek-"))
                .count()
        };
        assert_eq!((stale.exists(), fresh.exists(), peeks()), (false, true, 3));
        // When the bar starts.
        let stale = file("peek-f.txt", 3000);
        let ended = run_watch(
            &w.ctx,
            &caller,
            Origin::Hand,
            Box::new(NoInput),
            &mut |_| Ok(()),
        );
        assert_eq!(
            (ended, stale.exists(), fresh.exists()),
            ("pane-closed", false, true)
        );
    }
}

#[cfg(test)]
mod hc_tests;
#[cfg(test)]
mod kept_tests;
