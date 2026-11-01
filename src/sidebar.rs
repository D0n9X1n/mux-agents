//! The side bar (MuxAgents.ps1:1423-1591): `sidebar [-Close]` opens or closes one bar per session running
//! `<abs binary> watch`, and `watch` draws the session's board in it until its pane closes.

use std::cmp::Reverse;
use std::io;
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value};

use crate::agents::board::board;
use crate::agents::mail::mailbox_key;
use crate::agents::spawn::{pane_env, utf8};
use crate::agents::{lock, lock_key, pane_id, text};
use crate::caller::{Caller, resolve};
use crate::cli::Request;
use crate::ctx::Ctx;
use crate::error::{Res, refuse};
use crate::mux::{Output, native};

const TICK: Duration = Duration::from_millis(2000);
// A minute of ticks with no answer from the multiplexer: a write to a pipe whose reader closed can still succeed.
const UNKNOWN_MAX: u32 = 30;
const WATCH_PROBE: &str = "#{pane_id}|#{@agent-role}";
// What the bar's split answers: its pane, window and process (#47: the bar is that process, not its label).
const SPLIT_FORMAT: &str = "#{pane_id}|#{window_id}|#{pane_pid}";
// The proof read of the recorded pane: alive, and still the recorded process.
const PROOF_FORMAT: &str = "#{pane_id}|#{window_id}|#{pane_dead}|#{pane_pid}";
// The re-read after a conditional kill: a gone pane answers empty fields.
pub const GONE_FORMAT: &str = "#{pane_id}|#{pane_pid}";
// Each pane of the session and whether it carries the side-bar label, compared by the server: a label value with a
// newline or `|` cannot forge a line.
const LABEL_FORMAT: &str = "#{pane_id}|#{==:#{@agent-role},sidebar}";
const CLEAR: &str = "\u{1b}[H\u{1b}[2J";
// The bar is a fifth of its window, never narrower than this.
const MIN_WIDTH: u32 = 40;
// The bar's window, one row per pane (S1.3), with the zoom flag: a zoomed window is left alone.
const LAYOUT: &str = "#{pane_id}|#{pane_active}|#{pane_at_right}|#{pane_at_top}|#{pane_at_bottom}|#{pane_width}|#{window_width}|#{pane_left}|#{pane_top}|#{window_zoomed_flag}";
// Repairs that may fail in a row before repair pauses, and how long a pause lasts when the layout does not change.
const REPAIR_TRIES: u32 = 3;
const REARM: Duration = Duration::from_secs(60);
// The bar's last line while repair is paused, dim.
const PAUSED: &str = "\u{1b}[2mlayout: repair paused\u{1b}[0m";
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
    text(value)
        .chars()
        .filter(|&c| !(c < ' ' || ('\u{7f}'..='\u{9f}').contains(&c)))
        .collect()
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

/// The count line, then per live agent its line (with ` [<model>]` after the client when one was chosen) and an
/// indented doing line, per ended agent one line, except a lost one (its pane is gone). Every printed field, state
/// included, is cleaned first; lines are joined with LF.
pub fn format_board(board: &Map<String, Value>) -> String {
    let running = board.get("running").and_then(Value::as_i64).unwrap_or(0);
    let mut lines = vec![format!("mux-agents \u{b7} {running} running")];
    let rows = board.get("agents").and_then(Value::as_array);
    for row in rows.map_or(&[][..], Vec::as_slice) {
        let Some(row) = row.as_object() else {
            continue;
        };
        let name = clean(row.get("name"));
        let state = clean(row.get("state"));
        if state == "running" {
            let model = clean(row.get("model"));
            let tag = if model.is_empty() {
                String::new()
            } else {
                format!(" [{model}]")
            };
            lines.push(format!(
                "\u{25cf} {name}  {}{tag}  {}",
                clean(row.get("client")),
                format_age(row.get("age"))
            ));
            let doing = clean(row.get("doing"));
            let doing = doing.trim();
            if !doing.is_empty() {
                lines.push(format!("  {doing}"));
            }
            continue;
        }
        // A lost agent's pane is gone, so there is nothing to act on: list -All keeps it, the bar does not.
        if state == "lost" {
            continue;
        }
        let ended = match row.get("exitCode").filter(|code| !code.is_null()) {
            Some(code) if state == "exited" => format!("exited {}", clean(Some(code))),
            _ => state,
        };
        lines.push(format!("\u{25cb} {name}  {ended}"));
    }
    lines.join("\n")
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
}

/// The bar's window: its width, whether a pane is zoomed, and its panes.
#[derive(Debug)]
struct Layout {
    width: u32,
    zoomed: bool,
    panes: Vec<Pane>,
}

/// The LAYOUT rows, or none unless every row is exactly ten well-formed fields (a pane id, 0/1 flags, decimal
/// numbers) agreeing on the window's width and zoom, with at least one row. One final LF ends the answer.
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
        if f.len() != 10 || !pane_id(f[0]) {
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
    // The window width the bar's width was last set for; a drag is kept until the window's width changes.
    applied: Option<u32>,
    // Repairs that failed in a row: a refused move, or a move the read right after it found did not hold.
    failures: u32,
    // While repair is paused: the layout it paused on and when.
    paused: Option<(String, SystemTime)>,
}

impl Keeper {
    fn paused(&self) -> bool {
        self.paused.is_some()
    }

    /// One read of the bar's window. A failed or malformed read, or one without the bar, does nothing; so does a
    /// zoomed window, since resizing a pane unzooms it. A pause re-arms when the layout changed or 60 s passed. A bar
    /// in place gets its width when the window's width differs from the one last applied for; a bar out of place is
    /// repaired unless paused.
    fn tick(&mut self, ctx: &Ctx, bar: &str) {
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
            if self.applied != Some(layout.width) {
                self.resize(ctx, bar, layout.width);
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

    /// Sets the bar's width for a window `window` wide; remembered only when the multiplexer took it.
    fn resize(&mut self, ctx: &Ctx, bar: &str, window: u32) {
        let width = bar_width(window).to_string();
        if native(ctx, &["resize-pane", "-t", bar, "-x", &width], true).is_ok_and(|r| r.code == 0) {
            self.applied = Some(window);
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
        self.applied = None;
        self.resize(ctx, bar, layout.width);
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

/// The side-bar loop for the caller's pane. Each tick asks the pane itself and, when it answers, keeps it at the right
/// edge of its window (`Keeper`; by its own pane, not its tag, since rmux 0.10.0's rotate-window leaves pane options
/// behind), reads the board (ended rows with their exit codes), writes home, clear and the board (and `layout: repair
/// paused` while repair is paused), then sleeps 2 s. Ends `pane-closed` when the probe answers gone, `output-closed`
/// when a write fails and `probe-failed` after 30 unknown probes in a row. A failed board read still draws the last
/// board marked `refresh failed`, so a dead terminal is found even when nothing else answers.
pub fn run_watch(
    ctx: &Ctx,
    caller: &Caller,
    write: &mut dyn FnMut(&str) -> io::Result<()>,
) -> &'static str {
    // The caller's pane is already a well-formed id, so this never fails.
    let reader = mailbox_key(&caller.pane, &caller.agent_tag).unwrap_or_default();
    let own = format!("{}|", caller.pane);
    let mut keeper = Keeper::default();
    let mut last = "mux-agents \u{b7} ? running".to_string();
    let mut unknown = 0;
    loop {
        let probe = native(
            ctx,
            &["display-message", "-p", "-t", &caller.pane, WATCH_PROBE],
            true,
        )
        .ok();
        if probe.as_ref().is_some_and(gone) {
            return "pane-closed";
        }
        let live = probe
            .as_ref()
            .is_some_and(|p| p.code == 0 && p.stdout.starts_with(&own));
        unknown = if live { 0 } else { unknown + 1 };
        if unknown >= UNKNOWN_MAX {
            return "probe-failed";
        }
        if live && own_record(ctx, caller) {
            keeper.tick(ctx, &caller.pane);
        }
        let mut failed = !live;
        match board(ctx, caller, &reader, true, true) {
            Ok(board) => last = format_board(&board),
            Err(_) => failed = true,
        }
        let mut text = last.clone();
        if failed {
            text.push_str("\nrefresh failed");
        }
        if keeper.paused() {
            text.push('\n');
            text.push_str(PAUSED);
        }
        if write(&format!("{CLEAR}{}\r\n", text.replace('\n', "\r\n"))).is_err() {
            return "output-closed";
        }
        ctx.clock.sleep(TICK);
    }
}

/// The side-bar pane's process: draws the board to stdout until its pane closes.
pub fn watch(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    use std::io::{IsTerminal, Write};
    let stdout = io::stdout();
    let terminal = stdout.is_terminal();
    let mut out = stdout.lock();
    watch_on(ctx, request, terminal, &mut |frame| {
        out.write_all(frame.as_bytes())?;
        out.flush()
    })
}

/// Refuses unless stdout is a terminal: a terminal whose pane closed fails the next write, but a pipe whose reader
/// closed may not, and with the pane alive nothing else would end the loop.
pub(crate) fn watch_on(
    ctx: &Ctx,
    _request: &Request,
    terminal: bool,
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
    let ended = run_watch(ctx, &caller, write);
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
    if !caller.agent_tag.is_empty() {
        return refuse(2, "tagged", "an agent pane cannot open the side bar");
    }
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

/// The bar's width for a window `window` columns wide: `max(40, floor(window / 5))`.
pub fn bar_width(window: u32) -> u32 {
    (window / 5).max(MIN_WIDTH)
}

/// The width of `pane`'s window; none when the multiplexer does not answer a number.
fn window_width(ctx: &Ctx, pane: &str) -> Option<u32> {
    let read = native(
        ctx,
        &["display-message", "-p", "-t", pane, "#{window_width}"],
        true,
    )
    .ok()
    .filter(|read| read.code == 0)?;
    read.stdout.trim_end_matches(['\r', '\n']).parse().ok()
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
/// the state root) so watch reads the caller's state. The bar is `bar_width` of the window (40 when the width cannot
/// be read), asked for with -l and set again by a pid-bound resize-pane, since rmux ignores -l; a refused resize is left to
/// the next watch tick. `-d` keeps the active pane. The split's process must be this binary `watch` (else nothing is
/// killed and the open refuses); then the pane is labelled and recorded as `<pane> <pid> <start>`. A bar that cannot
/// be labelled or recorded is closed by the guarded kill, which spares a pane that runs anything else by then. The
/// bar is proven as the last step; a pane that fails it is not killed, its record is cleared and the open refuses.
fn open(ctx: &Ctx, caller: &Caller) -> Res<(String, String)> {
    let env = pane_env(utf8(&ctx.state_root)?, crate::state::root::HOST);
    let width = bar_width(window_width(ctx, &caller.pane).unwrap_or(0)).to_string();
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
        return refuse(1, "rmux", "rmux call failed");
    }
    let Some((pane, window, pid)) = split_answer(&created.stdout) else {
        return refuse(1, "rmux", "rmux did not name the side-bar pane");
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
    let steps: [(&str, Vec<&str>); 2] = [
        (
            "tagged",
            vec!["set-option", "-p", "-t", &pane, "@agent-role", "sidebar"],
        ),
        (
            "recorded",
            vec!["set-option", "-t", &caller.session, RECORD, &record],
        ),
    ];
    for (what, args) in steps {
        if native(ctx, &args, true).is_ok_and(|done| done.code == 0) {
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
                "the new side-bar pane {pane} no longer runs this plugin's watch; close it with kill-pane -t {pane}"
            ),
        );
    }
    Ok((pane, window))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::fixture::{ID, World, write};
    use crate::error::refuse;
    use crate::mux::Output;
    use crate::testing::Fakes;
    use serde_json::json;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    const DOT: char = '\u{b7}';
    const FULL: char = '\u{25cf}';
    const OPEN: char = '\u{25cb}';
    const READ_RECORD: &str = "-N show-options -qv -t $3 @mux-agents-sidebar-id";
    const CLEAR_RECORD: &str = "-N set-option -u -t $3 @mux-agents-sidebar-id";
    const LABELS: &str = "-N list-panes -s -t $3 -F #{pane_id}|#{==:#{@agent-role},sidebar}";
    const TAG7: &str = "-N set-option -p -t %7 @agent-role sidebar";
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

    fn board(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn format_board_lines_and_cleaning() {
        let text = format_board(&board(json!({"running": 2, "agents": [
            {"name": "claude-1", "client": "claude", "state": "running", "doing": "parse the config", "age": 240},
            {"name": "idle", "client": "copilot", "state": "running", "doing": null, "age": 5},
            {"name": "copilot-1", "client": "copilot", "state": "exited", "exitCode": 0, "age": 900},
            {"name": "crashed", "client": "claude", "state": "exited", "exitCode": null, "age": 60},
            {"name": "halted", "client": "claude", "state": "stopped", "exitCode": 7, "age": 60},
            {"name": "vanished", "client": "claude", "state": "lost", "age": 60},
            null
        ]})));
        // A lost agent's pane is gone: nothing to act on, so the bar draws no row for it.
        assert_eq!(
            text,
            format!(
                "mux-agents {DOT} 2 running\n{FULL} claude-1  claude  4m\n  parse the config\n{FULL} idle  copilot  5s\n\
                 {OPEN} copilot-1  exited 0\n{OPEN} crashed  exited\n{OPEN} halted  stopped"
            )
        );
        assert_eq!(
            format_board(&board(json!({"running": 0, "agents": []}))),
            format!("mux-agents {DOT} 0 running")
        );
        // Every C0, DEL and C1 code point goes, one at a time; its neighbours and U+00A0 stay.
        let mut kept = Vec::new();
        for code in (0u32..32).chain([127]).chain(128..160) {
            let name = format!("a{}b", char::from_u32(code).unwrap());
            let text = format_board(&board(
                json!({"running": 0, "agents": [{"name": name, "state": "stopped"}]}),
            ));
            if text != format!("mux-agents {DOT} 0 running\n{OPEN} ab  stopped") {
                kept.push(format!("{code:04X}"));
            }
        }
        assert_eq!(kept, Vec::<String>::new());
        // Every printed field is cleaned, state included, before it is compared.
        let controls: String = (0u32..32)
            .chain([127])
            .chain(128..160)
            .map(|c| char::from_u32(c).unwrap())
            .collect();
        let text = format_board(&board(json!({"running": 1, "agents": [
            {"name": format!("w{controls}1"), "client": format!("cl{controls}aude"), "state": format!("run{controls}ning"), "doing": format!("do{controls}ing"), "age": 240},
            {"name": format!("x{controls}2"), "state": format!("exi{controls}ted"), "exitCode": format!("3{controls}")},
            {"name": format!("y{controls}3"), "state": format!("sto{controls}pped")},
            {"name": format!("z{controls}4"), "state": format!("lo{controls}st")}
        ]})));
        // A state that reads `lost` once cleaned draws no row either.
        assert_eq!(
            text,
            format!(
                "mux-agents {DOT} 1 running\n{FULL} w1  claude  4m\n  doing\n{OPEN} x2  exited 3\n{OPEN} y3  stopped"
            )
        );
        // An escape sequence keeps only its printable rest.
        let text = format_board(&board(json!({"running": 1, "agents": [
            {"name": "a\u{1b}[2J\u{9b}b\u{7}", "client": "claude", "state": "running", "doing": "do\u{1b}]0;x\u{7}ing", "age": 240}
        ]})));
        assert_eq!(
            text,
            format!("mux-agents {DOT} 1 running\n{FULL} a[2Jb  claude  4m\n  do]0;xing")
        );
    }

    #[test]
    fn format_board_tags_a_chosen_model() {
        // A live agent's chosen model follows its client; inherited (null) or ended shows none. It is cleaned too.
        let text = format_board(&board(json!({"running": 2, "agents": [
            {"name": "w1", "client": "claude", "state": "running", "doing": null, "age": 5, "model": "sonnet"},
            {"name": "w2", "client": "copilot", "state": "running", "doing": null, "age": 5, "model": null},
            {"name": "w3", "client": "claude", "state": "running", "doing": null, "age": 5, "model": "op\u{1b}us"},
            {"name": "w4", "client": "claude", "state": "stopped", "age": 5, "model": "sonnet"}
        ]})));
        assert_eq!(
            text,
            format!(
                "mux-agents {DOT} 2 running\n{FULL} w1  claude [sonnet]  5s\n{FULL} w2  copilot  5s\n\
                 {FULL} w3  claude [opus]  5s\n{OPEN} w4  stopped"
            )
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
        let mut w = w;
        w.ctx.env = crate::ctx::Env::from_pairs(env);
        w.clock("1970-01-01T00:04:00Z");
        let worker = w.agent_record(ID, json!({"name": "worker", "pane": "%9"}));
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
                    "live" => Ok(out(0, "%7|sidebar\n", "")),
                    // The pane itself, without the bar's tag: watch run by hand in some pane.
                    "untagged" => Ok(out(0, "%7|\n", "")),
                    "gone" => Ok(out(0, "|\n", "")),
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
            .filter(|c| c.last().is_some_and(|a| a == "#{pane_id}|#{@agent-role}"))
            .count()
    }

    // The frames of one watch run; a runaway loop fails instead of hanging.
    fn watch_run(w: &World) -> (&'static str, Vec<String>) {
        let caller = w.caller();
        let mut frames = Vec::new();
        let ended = run_watch(&w.ctx, &caller, &mut |frame: &str| {
            frames.push(frame.to_string());
            assert!(frames.len() <= 100, "runaway");
            Ok(())
        });
        (ended, frames)
    }

    fn board_frame() -> String {
        format!(
            "\u{1b}[H\u{1b}[2Jmux-agents {DOT} 1 running\r\n{FULL} worker  claude  4m\r\n  testing the parser\r\n{OPEN} done  exited 0\r\n"
        )
    }

    #[test]
    fn watch_pane_closed() {
        let w = watch_world(&["live", "live", "gone"]);
        let (ended, frames) = watch_run(&w);
        assert_eq!(ended, "pane-closed");
        assert_eq!(frames, [board_frame(), board_frame()]);
        assert_eq!(w.fakes.slept(), [2000, 2000]);
        assert_eq!(probes(&w), 3);
        // A thrown or failed probe is unknown, not gone: its frame says refresh failed.
        let w = watch_world(&["throw", "fail", "live", "gone"]);
        let (ended, frames) = watch_run(&w);
        let marked = format!("{}refresh failed\r\n", board_frame());
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
        let ended = run_watch(&w.ctx, &caller, &mut |_| Err(io::Error::other("EIO")));
        assert_eq!(ended, "output-closed");
        assert_eq!(probes(&w), 1);
        assert!(w.fakes.slept().is_empty());
        // Probe and board both failing still write a frame, so a dead terminal is found at once.
        let w = watch_world(&["fail"]);
        w.fakes.mux_first("list-panes", 1, "", "lock");
        let caller = w.caller();
        let ended = run_watch(&w.ctx, &caller, &mut |_| Err(io::Error::other("EIO")));
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
                format!("\u{1b}[H\u{1b}[2Jmux-agents {DOT} ? running\r\nrefresh failed\r\n"),
                board_frame(),
                format!("{}refresh failed\r\n", board_frame()),
            ]
        );
        // The board is read as the bar's pane, with the ended rows and their exit codes.
        assert_eq!(listed.get(), 3);
    }

    #[test]
    fn watch_requires_terminal_usage_2() {
        let w = watch_world(&["gone"]);
        w.fakes.clear_log();
        let request = w.request(&["watch"]);
        let refused =
            watch_on(&w.ctx, &request, false, &mut |_| panic!("drew to a pipe")).unwrap_err();
        assert_eq!(
            (refused.code, refused.check, refused.message.as_str()),
            (
                2,
                "usage",
                "watch draws only to a terminal: it runs in the side-bar pane"
            )
        );
        assert!(w.fakes.mux_calls().is_empty());
        let reply = watch_on(&w.ctx, &request, true, &mut |_| Ok(())).unwrap();
        assert_eq!(
            Value::Object(reply),
            json!({"pane": "%7", "ended": "pane-closed"})
        );
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
    }

    type Shared = Rc<RefCell<Win>>;

    fn win(active: &'static str, panes: Vec<Rect>) -> Win {
        Win {
            width: 200,
            height: 50,
            active,
            panes,
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
                    "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}\n",
                    p.id,
                    flag(p.id == w.active),
                    flag(p.left + p.width == w.width),
                    flag(p.top == 0),
                    flag(p.top + p.height == w.height),
                    p.width,
                    w.width,
                    p.left,
                    p.top,
                    flag(w.zoomed)
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
        (w, shared)
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
        format!("{}{PAUSED}\r\n", board_frame())
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
            }
        );
        assert!(parse_layout(&good.replace('\n', "\r\n")).is_some());
        for bad in [
            "",
            "\n",
            "%1|1|0|1|1|159|200|0|0",
            "%1|1|0|1|1|159|200|0|0|0|x",
            "%1|2|0|1|1|159|200|0|0|0",
            "%1|1|0|1|1|+159|200|0|0|0",
            "%1|1|0|1|1|159|200|0|-1|0",
            "%1|1|0|1|1|159||0|0|0",
            "1|1|0|1|1|159|200|0|0|0",
            "%1|1|0|1|1|159|200|0|0|0\n%7|0|1|1|1|40|201|160|0|0",
            "%1|1|0|1|1|159|200|0|0|0\n%7|0|1|1|1|40|200|160|0|1",
            "%1|1|0|1|1|159|200|0|0|0\nnot a row",
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
            let marked: Vec<bool> = frames
                .iter()
                .map(|f| f.ends_with(&format!("{PAUSED}\r\n")))
                .collect();
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
        // `label` right after open writes its label.
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
        // The labels, compared by the server: a value only equal to `sidebar` is the label.
        let p = panes.clone();
        w.fakes
            .mux_script("#{==:#{@agent-role},sidebar}", move |argv| {
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
        let (p, fakes, r, kill_fail) =
            (panes.clone(), w.fakes.clone(), race.clone(), mux.kill_fail);
        w.fakes.mux_script("if-shell", move |argv| {
            if let Some(how) = kill_fail {
                return failed(how);
            }
            let pane = argv[4].clone();
            let want = argv[5].split("#{pane_pid},").nth(1).unwrap();
            let want = want.trim_end_matches('}').to_string();
            if let Some(width) = argv[6].strip_prefix(&format!("resize-pane -t {pane} -x ")) {
                let alive = get(&p, &format!("dead {pane}")).is_none();
                if alive && get(&p, &format!("pid {pane}")).as_deref() == Some(want.as_str()) {
                    set(&p, &format!("width {pane}"), width.to_string());
                }
                return Ok(Output::default());
            }
            assert_eq!(argv[6], format!("kill-pane -t {pane}"));
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
        let (p, fakes, tag_fail) = (panes.clone(), w.fakes.clone(), mux.tag_fail);
        w.fakes.mux_script("set-option -p", move |argv| {
            if let Some(how) = tag_fail {
                return failed(how);
            }
            let pane = &argv[4];
            let id = get(&p, pane)
                .unwrap_or_default()
                .split_once('|')
                .map(|(_, id)| id.to_string())
                .unwrap_or_default();
            set(&p, pane, format!("{}|{id}", argv[6]));
            if race.get() == Some("label") {
                race.set(None);
                respawn(&p, &fakes, pane);
            }
            Ok(Output::default())
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

    // Calls on panes only: the session options' reads and writes are not counted.
    fn count(w: &World, verb: &str) -> usize {
        w.fakes
            .mux_calls()
            .iter()
            .filter(|c| c[1] == verb && !c.iter().any(|a| a == OPTION || a == RECORD))
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
            .filter(|c| c[1] != "display-message")
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
            TAG7.into(),
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

    const NOT_CLOSED: &str =
        "1 sidebar the side bar %7 did not close: the pane no longer runs this session's side bar";

    #[test]
    fn bar_width_is_a_fifth_of_the_window_at_least_40() {
        let widths: Vec<u32> = [0, 80, 136, 199, 200, 204, 205, 250, 300]
            .into_iter()
            .map(bar_width)
            .collect();
        assert_eq!(widths, [40, 40, 40, 40, 40, 40, 41, 50, 60]);
    }

    #[test]
    fn open_sizes_the_bar_to_the_window_then_resizes_it() {
        // A 250-column window: split -l 50 at the right edge, full height, then resize-pane -x 50 (rmux ignores -l).
        let (w, _) = sidebar_world("", Mux::default());
        w.fakes.mux_rule("#{window_width}", 0, "250\n", "");
        assert!(run(&w, &[]).starts_with("ok "));
        let probe: Vec<String> = w
            .fakes
            .mux_calls()
            .iter()
            .filter(|c| c.last().is_some_and(|a| a == "#{window_width}"))
            .map(|c| c.join(" "))
            .collect();
        assert_eq!(probe, ["-N display-message -p -t %1 #{window_width}"]);
        let mut want = vec![ON.to_string()];
        want.extend(opened(&w));
        want[3] = split_line(&w, true).replace("-l 40", "-l 50");
        want[6] = resize_line("%7", 107, 50);
        assert_eq!(calls(&w), want);
        // An unreadable width (a failed probe, a non-number) opens the narrowest bar, 40.
        for answer in [(1, ""), (0, "wide\n"), (0, "")] {
            let (w, _) = sidebar_world("", Mux::default());
            w.fakes.mux_rule("#{window_width}", answer.0, answer.1, "");
            assert!(run(&w, &[]).starts_with("ok "), "{answer:?}");
            let resized: Vec<String> = calls(&w)
                .into_iter()
                .filter(|c| c.contains("resize-pane"))
                .collect();
            assert_eq!(resized, [resize_line("%7", 107, 40)], "{answer:?}");
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
        assert_eq!(
            run(&w, &[]),
            "1 sidebar the new side-bar pane %7 no longer runs this plugin's watch; close it with kill-pane -t %7"
        );
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
                .filter(|c| c.contains("if-shell"))
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
        assert_eq!(count(&w, "if-shell"), 1);
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
        assert_eq!(run(&w, &[]), "1 rmux rmux call failed");
        assert_eq!(count(&w, "list-panes"), 0);
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
                format!("+sidebar|{key}|$3 display-message"),
                format!(
                    "-sidebar|{key}|$3 display-message,set-option,show-options,list-panes,display-message,\
                     split-window,set-option,set-option,if-shell,display-message"
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
            assert_eq!(run(&w, &[]), "1 rmux rmux call failed", "{stderr}");
            assert_eq!((count(&w, "split-window"), count(&w, "set-option")), (1, 0));
        }
        let (w, _) = sidebar_world(
            "",
            Mux {
                split_error: Some("invalid split size '40': invalid digit found in string"),
                ..Mux::default()
            },
        );
        assert_eq!(run(&w, &[]), "1 rmux rmux call failed");
        assert_eq!((count(&w, "split-window"), count(&w, "set-option")), (2, 0));
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
                "show-options,show-options,list-panes,display-message,split-window,set-option,set-option,if-shell,\
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
        assert_eq!(ensure_run(&w), "1 rmux rmux call failed");
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
                "-sidebar|{key}|$3 {ON},-N set-option -p -t %8 @agent-role sidebar,\
                 -N set-option -t $3 @mux-agents-sidebar-id %8 108 1080,{}",
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
