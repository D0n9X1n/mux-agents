//! The side bar (MuxAgents.ps1:1423-1591): `sidebar [-Close]` opens or closes one bar per session running
//! `<abs binary> watch`, and `watch` draws the session's board in it until its pane closes.

use std::collections::HashSet;
use std::io;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::agents::board::board;
use crate::agents::mail::mailbox_key;
use crate::agents::spawn::{pane_and_window, pane_env, utf8};
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
const LIST_FORMAT: &str = "#{pane_id}|#{window_id}|#{@agent-role}|#{@agent-id}";
const CONFIRM_FORMAT: &str = "#{pane_id}|#{window_id}|#{@agent-role}|#{@agent-id}|end";
const PANE_FORMAT: &str = "#{pane_id}|#{window_id}";
const CLEAR: &str = "\u{1b}[H\u{1b}[2J";
// The bar is a fifth of its window, never narrower than this.
const MIN_WIDTH: u32 = 40;
// `off` once `sidebar -Close` ran in the session; unset again by `sidebar`.
const OPTION: &str = "@mux-agents-sidebar";

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

/// The session's bar as (pane, window, opened): the one found, else a new one beside the caller.
fn find_or_open(ctx: &Ctx, caller: &Caller) -> Res<(String, String, bool)> {
    match find_sidebar(ctx, caller)? {
        Some((pane, window)) => Ok((pane, window, false)),
        None => {
            let (pane, window) = open(ctx, caller)?;
            Ok((pane, window, true))
        }
    }
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

/// The side-bar loop for the caller's pane. Each tick asks the pane itself, reads the board (ended rows with their
/// exit codes), writes home, clear and the board, then sleeps 2 s. Ends `pane-closed` when the probe answers gone,
/// `output-closed` when a write fails and `probe-failed` after 30 unknown probes in a row. A failed board read still
/// draws the last board marked `refresh failed`, so a dead terminal is found even when nothing else answers.
pub fn run_watch(
    ctx: &Ctx,
    caller: &Caller,
    write: &mut dyn FnMut(&str) -> io::Result<()>,
) -> &'static str {
    // The caller's pane is already a well-formed id, so this never fails.
    let reader = mailbox_key(&caller.pane, &caller.agent_tag).unwrap_or_default();
    let own = format!("{}|", caller.pane);
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
        let mut failed = !live;
        match board(ctx, caller, &reader, true, true) {
            Ok(board) => last = format_board(&board),
            Err(_) => failed = true,
        }
        let mut text = last.clone();
        if failed {
            text.push_str("\nrefresh failed");
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
/// tagged @agent-role sidebar; an existing bar is returned with opened false; -Close kills it, and reports closed only
/// when the kill answered 0.
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
        match find_sidebar(ctx, &caller)? {
            None => {
                reply.insert("pane".into(), Value::Null);
                reply.insert("closed".into(), false.into());
            }
            Some((pane, _)) => {
                if !close_pane(ctx, &pane) {
                    return refuse(
                        1,
                        "sidebar",
                        format!(
                            "the side bar {pane} did not close; close it with kill-pane -t {pane}"
                        ),
                    );
                }
                reply.insert("pane".into(), pane.into());
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

/// The session's first pane with @agent-role sidebar and no @agent-id, as (pane, window). list-panes only names
/// candidates (an option value may hold a newline and forge a line); each is asked alone and must answer exactly
/// `<pane>|<window>|sidebar||end`. A candidate whose probe fails is unknown, not absent: it refuses 1 `sidebar`, so a
/// passing failure cannot open a second bar or hide the one to close.
fn find_sidebar(ctx: &Ctx, caller: &Caller) -> Res<Option<(String, String)>> {
    let listed = native(
        ctx,
        &["list-panes", "-s", "-t", &caller.session, "-F", LIST_FORMAT],
        false,
    )?;
    let mut seen = HashSet::new();
    for line in listed.stdout.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        // `^(%\d+)\|@\d+\|sidebar(\||$)`
        let mut parts = line.splitn(3, '|');
        let (Some(pane), Some(window), Some(rest)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let role = rest == "sidebar" || rest.starts_with("sidebar|");
        if !pane_id(pane) || !window_id(window) || !role || !seen.insert(pane.to_string()) {
            continue;
        }
        let probe = native(
            ctx,
            &["display-message", "-p", "-t", pane, CONFIRM_FORMAT],
            true,
        )
        .ok()
        .filter(|p| p.code == 0);
        let Some(probe) = probe else {
            return refuse(
                1,
                "sidebar",
                format!("pane {pane} could not be checked as the side bar; try again"),
            );
        };
        // One final LF (or CR LF) is the line end; anything else is part of the answer.
        let answer = match probe.stdout.strip_suffix('\n') {
            Some(rest) => rest.strip_suffix('\r').unwrap_or(rest),
            None => &probe.stdout,
        };
        let window = answer
            .strip_prefix(&format!("{pane}|"))
            .and_then(|rest| rest.strip_suffix("|sidebar||end"))
            .filter(|window| window_id(window));
        if let Some(window) = window {
            return Ok(Some((pane.to_string(), window.to_string())));
        }
    }
    Ok(None)
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

/// True only when the kill answered 0; a refused or failed kill is false.
fn close_pane(ctx: &Ctx, pane: &str) -> bool {
    native(ctx, &["kill-pane", "-t", pane], true).is_ok_and(|killed| killed.code == 0)
}

/// Splits the bar at the right edge of the caller's window, full height (`-f -h`), with the pane environment (Unix:
/// the state root) so watch reads the caller's state, and tags it; an untagged bar is closed again. The bar is
/// `bar_width` of the window (40 when the width cannot be read), asked for with -l and set again with resize-pane,
/// since rmux ignores -l; a refused resize is left to the next watch tick. `-d` keeps the active pane.
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
        PANE_FORMAT,
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
    let Some((pane, window)) = pane_and_window(&created.stdout) else {
        return refuse(1, "rmux", "rmux did not name the side-bar pane");
    };
    let tagged = native(
        ctx,
        &["set-option", "-p", "-t", &pane, "@agent-role", "sidebar"],
        true,
    )
    .is_ok_and(|tagged| tagged.code == 0);
    if !tagged {
        if close_pane(ctx, &pane) {
            return refuse(
                1,
                "rmux",
                format!("the side-bar pane {pane} could not be tagged; it was closed"),
            );
        }
        return refuse(
            1,
            "sidebar",
            format!(
                "the side-bar pane {pane} could not be tagged or closed; close it with kill-pane -t {pane}"
            ),
        );
    }
    let _ = native(ctx, &["resize-pane", "-t", &pane, "-x", &width], true);
    Ok((pane, window))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::fixture::{ID, World, write};
    use crate::error::refuse;
    use crate::mux::Output;
    use serde_json::json;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    const DOT: char = '\u{b7}';
    const FULL: char = '\u{25cf}';
    const OPEN: char = '\u{25cb}';
    const LIST: &str =
        "-N list-panes -s -t $3 -F #{pane_id}|#{window_id}|#{@agent-role}|#{@agent-id}";
    const CONFIRM: &str = "#{pane_id}|#{window_id}|#{@agent-role}|#{@agent-id}|end";
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

    // ---- sidebar

    /// A stateful pane table: pane -> `role|agent-id`. %1 (the caller) has neither, %2 role `other`. The session
    /// option @mux-agents-sidebar is kept in the same table under its own name.
    #[derive(Default)]
    struct Mux {
        extra: Vec<(&'static str, String)>,
        open: bool,
        disabled: bool,
        option_fail: Option<&'static str>,
        agent_bar: bool,
        refuse_size: bool,
        split_error: Option<&'static str>,
        tag_fail: Option<&'static str>,
        kill_fail: Option<&'static str>,
        probe_fail: Option<(&'static str, usize)>,
    }

    type Panes = Rc<RefCell<Vec<(String, String)>>>;

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

    fn roles(panes: &Panes) -> String {
        panes
            .borrow()
            .iter()
            .filter(|(p, _)| p.starts_with('%'))
            .map(|(p, v)| format!("{p}={v}"))
            .collect::<Vec<_>>()
            .join(",")
    }

    fn sidebar_world(tag: &str, mux: Mux) -> (World, Panes) {
        let w = World::human(tag);
        let mut table = vec![
            ("%1".to_string(), "|".to_string()),
            ("%2".into(), "other|".into()),
        ];
        if mux.agent_bar {
            table.push(("%3".into(), "sidebar|cccccccccccc".into()));
        }
        if mux.open {
            table.push(("%7".into(), "sidebar|".into()));
        }
        table.extend(mux.extra.iter().map(|(p, v)| (p.to_string(), v.clone())));
        if mux.disabled {
            table.push((OPTION.into(), "off".into()));
        }
        let panes: Panes = Rc::new(RefCell::new(table));
        let p = panes.clone();
        w.fakes.mux_script("list-panes", move |_| {
            let text: String = p
                .borrow()
                .iter()
                .filter(|(pane, _)| pane.starts_with('%'))
                .map(|(pane, v)| format!("{pane}|@5|{v}\n"))
                .collect();
            Ok(out(0, &text, ""))
        });
        let (p, left) = (
            panes.clone(),
            Rc::new(Cell::new(mux.probe_fail.map_or(0, |(_, n)| n))),
        );
        let how = mux.probe_fail.map(|(how, _)| how);
        w.fakes.mux_script(CONFIRM, move |argv| {
            if left.get() > 0 {
                left.set(left.get() - 1);
                return failed(how.unwrap());
            }
            let pane = &argv[4];
            Ok(out(
                0,
                &match get(&p, pane) {
                    Some(v) => format!("{pane}|@5|{v}|end\n"),
                    None => "||||end\n".into(),
                },
                "",
            ))
        });
        let p = panes.clone();
        let (split_error, refuse_size) = (mux.split_error, mux.refuse_size);
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
            set(&p, "%7", "|".into());
            Ok(out(0, "%7|@5\n", ""))
        });
        let (p, tag_fail) = (panes.clone(), mux.tag_fail);
        w.fakes.mux_script("set-option", move |argv| {
            if let Some(how) = tag_fail {
                return failed(how);
            }
            let id = get(&p, &argv[4])
                .unwrap_or_default()
                .split_once('|')
                .map(|(_, id)| id.to_string())
                .unwrap_or_default();
            set(&p, &argv[4], format!("{}|{id}", argv[6]));
            Ok(Output::default())
        });
        let (p, kill_fail) = (panes.clone(), mux.kill_fail);
        w.fakes.mux_script("kill-pane", move |argv| {
            if let Some(how) = kill_fail {
                return failed(how);
            }
            p.borrow_mut().retain(|(pane, _)| pane != &argv[3]);
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
                p.borrow_mut().retain(|(key, _)| key != OPTION);
            } else {
                set(&p, OPTION, argv[5].clone());
            }
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

    // Calls on panes only: the session option's reads and writes are not counted.
    fn count(w: &World, verb: &str) -> usize {
        w.fakes
            .mux_calls()
            .iter()
            .filter(|c| c[1] == verb && !c.iter().any(|a| a == OPTION))
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
            "-N split-window -d -f -h -P -F #{{pane_id}}|#{{window_id}} -t %1{}{} /bin/mux-agent watch",
            env_text(w),
            if sized { " -l 40" } else { "" }
        )
    }

    const RESIZE: &str = "-N resize-pane -t %7 -x 40";

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
        assert_eq!(
            calls(&w),
            [
                ON.to_string(),
                LIST.into(),
                split_line(&w, true).replace("-l 40", "-l 50"),
                "-N set-option -p -t %7 @agent-role sidebar".into(),
                "-N resize-pane -t %7 -x 50".into(),
            ]
        );
        // An unreadable width (a failed probe, a non-number) opens the narrowest bar, 40.
        for answer in [(1, ""), (0, "wide\n"), (0, "")] {
            let (w, _) = sidebar_world("", Mux::default());
            w.fakes.mux_rule("#{window_width}", answer.0, answer.1, "");
            assert!(run(&w, &[]).starts_with("ok "), "{answer:?}");
            let resized: Vec<String> = calls(&w)
                .into_iter()
                .filter(|c| c.contains("resize-pane"))
                .collect();
            assert_eq!(resized, [RESIZE], "{answer:?}");
        }
        // A refused resize still opens the bar: the next watch tick sizes it.
        let (w, _) = sidebar_world("", Mux::default());
        w.fakes.mux_rule("resize-pane", 1, "", "no such pane\n");
        assert_eq!(
            run(&w, &[]),
            r#"ok {"pane":"%7","window":"@5","opened":true}"#
        );
    }

    #[test]
    fn find_sidebar_probe_exact_answer() {
        // Reuse, from another pane too: the same bar, opened false, nothing split.
        let (w, panes) = sidebar_world(
            "",
            Mux {
                open: true,
                ..Mux::default()
            },
        );
        assert_eq!(
            run(&w, &[]),
            r#"ok {"pane":"%7","window":"@5","opened":false}"#
        );
        assert_eq!(calls(&w), [ON, LIST]);
        assert_eq!(roles(&panes), "%1=|,%2=other|,%7=sidebar|");
        // An agent pane that also carries the role is never the bar.
        let (w, panes) = sidebar_world(
            "",
            Mux {
                agent_bar: true,
                ..Mux::default()
            },
        );
        let close = run(&w, &["-Close"]);
        assert_eq!(close, r#"ok {"pane":null,"closed":false}"#);
        assert_eq!(count(&w, "kill-pane"), 0);
        assert_eq!(
            run(&w, &[]),
            r#"ok {"pane":"%7","window":"@5","opened":true}"#
        );
        assert_eq!(
            roles(&panes),
            "%1=|,%2=other|,%3=sidebar|cccccccccccc,%7=sidebar|"
        );
        // A pane that only looks like a bar in list-panes: a newline in a role or id, an ESC, a forged line.
        for (name, value) in [
            ("role newline", "sidebar\nmore|cccccccccccc"),
            ("id newline", "sidebar|\ncccccccccccc"),
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
            assert_eq!(count(&w, "kill-pane"), 0, "{name}");
        }
    }

    #[test]
    fn find_sidebar_unknown_probe_refuses_1() {
        for how in ["exit", "throw"] {
            let (w, panes) = sidebar_world(
                "",
                Mux {
                    open: true,
                    probe_fail: Some((how, 2)),
                    ..Mux::default()
                },
            );
            let refused = "1 sidebar pane %7 could not be checked as the side bar; try again";
            assert_eq!(run(&w, &[]), refused, "{how}");
            assert_eq!(run(&w, &["-Close"]), refused, "{how}");
            assert_eq!(
                run(&w, &[]),
                r#"ok {"pane":"%7","window":"@5","opened":false}"#,
                "{how}"
            );
            assert_eq!(
                (
                    count(&w, "split-window"),
                    count(&w, "set-option"),
                    count(&w, "kill-pane")
                ),
                (0, 0, 0),
                "{how}"
            );
            assert_eq!(roles(&panes), "%1=|,%2=other|,%7=sidebar|");
        }
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
                    "-sidebar|{key}|$3 display-message,set-option,list-panes,display-message,split-window,set-option,resize-pane"
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
        // A size refusal retries without -l, then tags.
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
        assert_eq!(calls(&w).last().map(String::as_str), Some(RESIZE));
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
    fn tag_failure_closes_pane() {
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
            let kills: Vec<String> = calls(&w)
                .into_iter()
                .filter(|c| c.contains("kill-pane"))
                .collect();
            assert_eq!(kills, ["-N kill-pane -t %7"]);
            assert_eq!(count(&w, "resize-pane"), 0, "{how}");
            assert_eq!(roles(&panes), "%1=|,%2=other|");
        }
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

    #[test]
    fn close_reports_only_on_kill_0() {
        let (w, panes) = sidebar_world(
            "",
            Mux {
                open: true,
                ..Mux::default()
            },
        );
        assert_eq!(run(&w, &["-Close"]), r#"ok {"pane":"%7","closed":true}"#);
        assert_eq!(calls(&w), [OFF, LIST, "-N kill-pane -t %7"]);
        assert_eq!(roles(&panes), "%1=|,%2=other|");
        w.fakes.clear_log();
        assert_eq!(run(&w, &["-Close"]), r#"ok {"pane":null,"closed":false}"#);
        assert_eq!(calls(&w), [OFF, LIST]);
        for how in ["exit", "throw"] {
            let (w, panes) = sidebar_world(
                "",
                Mux {
                    open: true,
                    kill_fail: Some(how),
                    ..Mux::default()
                },
            );
            assert_eq!(
                run(&w, &["-Close"]),
                "1 sidebar the side bar %7 did not close; close it with kill-pane -t %7",
                "{how}"
            );
            assert_eq!(roles(&panes), "%1=|,%2=other|,%7=sidebar|");
        }
    }

    #[test]
    fn cmd_is_exe_watch() {
        let (w, panes) = sidebar_world("", Mux::default());
        assert_eq!(
            run(&w, &[]),
            r#"ok {"pane":"%7","window":"@5","opened":true}"#
        );
        assert_eq!(
            calls(&w),
            [
                ON.to_string(),
                LIST.to_string(),
                split_line(&w, true),
                "-N set-option -p -t %7 @agent-role sidebar".into(),
                RESIZE.into()
            ]
        );
        assert_eq!(roles(&panes), "%1=|,%2=other|,%7=sidebar|");
        // The pane command is two arguments, the exe (spaces and all) then `watch`, never one joined string.
        let (mut w, _) = sidebar_world("", Mux::default());
        w.ctx.exe = std::path::PathBuf::from("/opt/mux agents/bin/mux-agent");
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
            "#{pane_id}|#{window_id}",
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

    // ---- ensure

    #[test]
    fn ensure_opens_one_bar_then_finds_it() {
        let (w, panes) = sidebar_world("", Mux::default());
        assert_eq!(ensure_run(&w), "opened %7");
        assert_eq!(
            calls(&w),
            [
                READ_OPTION.to_string(),
                LIST.into(),
                split_line(&w, true),
                "-N set-option -p -t %7 @agent-role sidebar".into(),
                RESIZE.into()
            ]
        );
        w.fakes.clear_log();
        assert_eq!(ensure_run(&w), "existing %7");
        assert_eq!(calls(&w), [READ_OPTION, LIST]);
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
                "show-options,list-panes,display-message,split-window,set-option,resize-pane"
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
        // A candidate that cannot be checked is unknown, not absent.
        for how in ["exit", "throw"] {
            let (w, panes) = sidebar_world(
                "",
                Mux {
                    open: true,
                    probe_fail: Some((how, 1)),
                    ..Mux::default()
                },
            );
            assert_eq!(
                ensure_run(&w),
                "1 sidebar pane %7 could not be checked as the side bar; try again",
                "{how}"
            );
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
                    .filter(|c| c[1] == "set-option" || c[1] == "kill-pane")
                    .map(|c| c.join(" "))
                    .collect::<Vec<_>>()
                    .join(",")
            } else {
                String::new()
            }
        });
        assert_eq!(run(&w, &["-Close"]), r#"ok {"pane":"%7","closed":true}"#);
        // Off is written inside the lock, before the kill.
        let key = w.caller().server_key;
        assert_eq!(
            w.fakes.locks()[1],
            format!("-sidebar|{key}|$3 {OFF},-N kill-pane -t %7")
        );
        assert_eq!(option(&panes), Some("off".into()));
        // Once close has returned, ensure (as a spawn runs it) opens nothing.
        w.fakes.clear_log();
        assert_eq!(ensure_run(&w), "disabled");
        assert_eq!(count(&w, "split-window"), 0);
        assert_eq!(roles(&panes), "%1=|,%2=other|");
        // sidebar turns it back on, inside the lock, and opens the bar.
        w.fakes.clear_log();
        assert_eq!(
            run(&w, &[]),
            r#"ok {"pane":"%7","window":"@5","opened":true}"#
        );
        assert_eq!(option(&panes), None);
        assert_eq!(
            w.fakes.locks()[1],
            format!("-sidebar|{key}|$3 {ON},-N set-option -p -t %7 @agent-role sidebar")
        );
        assert_eq!(ensure_run(&w), "existing %7");
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
                    (count(&w, "list-panes"), count(&w, "kill-pane")),
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
