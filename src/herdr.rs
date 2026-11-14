//! herdr sees the team (#103). Inside tmux or rmux in a herdr terminal, herdr sees one terminal, the multiplexer,
//! never the agents in its panes: herdr reads an agent's state only from OSC 7501 written by the program in its own
//! terminal, and Claude Code under tmux never writes it. So `spawn` also starts one relay per agent, a hidden hc
//! terminal under the herdr terminal that runs `mux-agent relay <spec>`: it writes the agent's state and step as OSC
//! 7501, which makes herdr list the agent as a sub-agent of that terminal, and turns each line typed into it into a
//! message in the agent's mailbox, from `herdr`. The relay only reads the agent's files and appends mail; it never
//! types into a pane and never stops the agent.

use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use serde_json::Value;

use crate::agents::mail::{add_mail_in, doing_line};
use crate::ctx::Ctx;
use crate::state::fs::read_json;
use crate::state::root::HOST;

/// The `app` herdr shows for a relay.
pub const APP: &str = "mux-agents";
/// `spawn`'s one call to `hc run`.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_secs(1);
/// A typed line, as `send -Body` takes it.
const LINE_MAX: usize = 64 * 1024;

/// What the relay shows herdr: its state word, a blocked agent's kind, the agent's step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub state: &'static str,
    pub kind: Option<&'static str>,
    pub step: String,
}

/// One OSC 7501 report, as Claude Code writes its own: `state` and `app`, a blocked state's `kind`, then `title`
/// and `msg` in base64. Title and step are one clean line each, so no byte ends the sequence early.
pub fn osc(shown: &Shown, title: &str) -> String {
    let mut fields = vec![format!("state={}", shown.state), format!("app={APP}")];
    if let (Some(kind), "blocked") = (shown.kind, shown.state) {
        fields.push(format!("kind={kind}"));
    }
    let title = doing_line(title);
    if !title.is_empty() {
        fields.push(format!("title={}", crate::util::base64(title.as_bytes())));
    }
    let step = doing_line(&shown.step);
    if !step.is_empty() {
        fields.push(format!("msg={}", crate::util::base64(step.as_bytes())));
    }
    format!("\u{1b}]7501;{}\u{1b}\\", fields.join(":"))
}

/// The report that takes the relay's status away.
pub const CLEAR: &str = "\u{1b}]7501;state=clear\u{1b}\\";

/// The agent as herdr names it, from its activity.json, as the side bar names it: `blocked` (with a known kind)
/// and `working` as reported; `idle` is `done` (herdr turns it `idle` once the person looks); with no report yet,
/// `idle` while it listens in `read -Wait`, else `working`. The step is the side bar's: its status, else its last
/// sent line, else its task line. The second value is the idle report's `since`, so one idle spell is `done` once.
pub fn shown(activity: Option<&Value>, step: &str, waiting: bool) -> (Shown, Option<String>) {
    let word = activity
        .and_then(|a| a.get("state"))
        .and_then(Value::as_str);
    let since = activity
        .and_then(|a| a.get("since"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let state = match (word, since) {
        (Some("blocked"), Some(_)) => "blocked",
        (Some("working"), Some(_)) => "working",
        (Some("idle"), Some(_)) => "done",
        _ if waiting => "idle",
        _ => "working",
    };
    let kind = match activity.and_then(|a| a.get("kind")).and_then(Value::as_str) {
        Some("permission") => Some("permission"),
        Some("question") => Some("question"),
        _ => None,
    };
    let spell = since.filter(|_| state == "done").map(String::from);
    let shown = Shown {
        state,
        kind: kind.filter(|_| state == "blocked"),
        step: doing_line(step),
    };
    (shown, spell)
}

/// What to write now, if anything. A new idle spell is `done`, news even when it reads like the last report; the
/// same spell after that writes nothing: herdr holds `done` until the person looks, then turns it `idle` itself,
/// and a second report would take that mark away early. Otherwise only a change is written.
pub fn settle(
    now: Shown,
    spell: Option<String>,
    last: &Option<Shown>,
    done_spell: &mut Option<String>,
) -> Option<Shown> {
    match spell {
        Some(spell) if done_spell.as_deref() == Some(spell.as_str()) => None,
        Some(spell) => {
            *done_spell = Some(spell);
            Some(now)
        }
        None => (last.as_ref() != Some(&now)).then_some(now),
    }
}

/// Whether the agent ended: `Some(code)` once launcher.json holds its exit code; `Some(None)` once a notice of its
/// end exists (a stop, a lost pane), once the launcher launcher.json names no longer runs with its recorded start,
/// or, `waited_out`, when no launcher ever wrote one (the pane never ran). `None` while it runs.
pub fn ended(
    launcher: Option<&Value>,
    noticed: bool,
    waited_out: bool,
    alive: impl Fn(u32, Option<i64>) -> bool,
) -> Option<Option<i64>> {
    if let Some(code) = launcher
        .and_then(|l| l.get("exitCode"))
        .filter(|c| !c.is_null())
    {
        return Some(code.as_i64());
    }
    if noticed {
        return Some(None);
    }
    let pid = launcher
        .and_then(|l| l.get("pid"))
        .and_then(Value::as_u64)
        .and_then(|p| u32::try_from(p).ok());
    let Some(pid) = pid else {
        return waited_out.then_some(None);
    };
    let start = launcher
        .and_then(|l| l.get("start"))
        .and_then(Value::as_i64);
    (!alive(pid, start)).then_some(None)
}

/// Where a relay spec leads: the server folder, its key, and the agent's id and folder. Only an absolute,
/// canonical (no link on the way) `<server folder>/agents/<12-hex id>/spec.json` of a committed agent. The state
/// root comes from the path, not this process's environment: a terminal hc starts has the core's environment, not
/// the caller's, so `MUX_AGENTS_STATE` or `XDG_STATE_HOME` may differ there. Like every state file, the folder
/// holds only what the same user wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub state: PathBuf,
    pub server_key: String,
    pub id: String,
    pub folder: PathBuf,
}

pub fn place(spec: &str) -> Option<Place> {
    let given = Path::new(spec);
    if !given.is_absolute()
        || given.file_name().and_then(|n| n.to_str()) != Some("spec.json")
        || given
            .components()
            .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
    {
        return None;
    }
    let folder = given.parent()?;
    let id = folder.file_name()?.to_str()?;
    let agents = folder.parent()?;
    let state = agents.parent()?;
    let server_key = state.file_name()?.to_str()?;
    if !crate::agents::agent_id(id)
        || agents.file_name().and_then(|n| n.to_str()) != Some("agents")
        || server_key.is_empty()
    {
        return None;
    }
    if !std::fs::canonicalize(given).is_ok_and(|real| crate::launcher::same_path(&real, spec, HOST))
        || !folder.join("commit.json").is_file()
    {
        return None;
    }
    Some(Place {
        state: state.to_path_buf(),
        server_key: server_key.to_string(),
        id: id.to_string(),
        folder: folder.to_path_buf(),
    })
}

/// The `hc run` that starts agent `id`'s relay under herdr terminal `host`, or none when there is no relay to
/// start: no herdr terminal, or `MUX_AGENTS_HERDR=off`.
pub fn relay_args(ctx: &Ctx, folder: &Path, id: &str) -> Option<Vec<String>> {
    let host = ctx.env.get("HC_TERMINAL_ID").filter(|h| !h.is_empty())?;
    // An address names a terminal, never an option hc would read instead.
    if host.starts_with('-') || host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    if ctx.env.get("MUX_AGENTS_HERDR") == Some("off") {
        return None;
    }
    let path = |p: &Path| p.to_string_lossy().into_owned();
    Some(vec![
        "run".into(),
        "--json".into(),
        "--bg".into(),
        "--no-notify".into(),
        "--rm".into(),
        "--parent".into(),
        host.into(),
        "--label".into(),
        format!("mux-agents.agent={id}"),
        "--cwd".into(),
        path(folder),
        "--".into(),
        path(&ctx.exe),
        "relay".into(),
        path(&folder.join("spec.json")),
    ])
}

/// What `spawn` adds to its reply: none when no relay was due, else the relay's address or why it failed.
pub type Started = Option<Result<String, String>>;

/// Starts agent `id`'s relay through `run` (`hc` and its arguments): best effort, never a refusal.
pub fn start_relay(
    ctx: &Ctx,
    folder: &Path,
    id: &str,
    run: &mut dyn FnMut(&Path, &[&str]) -> std::io::Result<crate::spawn::Out>,
) -> Started {
    let args = relay_args(ctx, folder, id)?;
    let image = if cfg!(windows) { "hc.exe" } else { "hc" };
    let Some(hc) = ctx
        .env
        .get("PATH")
        .and_then(|path| crate::mux::find_on_path(image, path))
    else {
        return Some(Err("hc is not on PATH".into()));
    };
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    Some(match run(&hc, &argv) {
        Err(e) => Err(format!("hc run failed: {e}")),
        Ok(out) if out.code != 0 => {
            let said = out.stderr.lines().next().unwrap_or("").trim();
            Err(format!("hc run exited {}: {}", out.code, doing_line(said)))
        }
        Ok(out) => serde_json::from_str::<Value>(out.stdout.trim())
            .ok()
            .and_then(|v| v.get("address").and_then(Value::as_str).map(String::from))
            .ok_or_else(|| "hc run answered no address".to_string()),
    })
}

/// How long a relay waits for the launcher to write launcher.json before it takes the pane as never started.
const LAUNCH_GRACE: Duration = Duration::from_secs(60);

/// `mux-agent relay <spec>`: the relay's process, its exit code. 2 for a spec it will not read.
pub fn relay(ctx: &Ctx, spec: &str) -> i32 {
    let Some(place) = place(spec) else {
        eprintln!("mux-agent: the relay spec is not a committed agent's spec.json");
        return 2;
    };
    let read = |file: &str| read_json(&place.folder.join(file)).ok().flatten();
    let record = read("record.json").unwrap_or(Value::Null);
    let name = doing_line(&crate::agents::text(record.get("name")));
    enable_vt();
    say(&format!(
        "\u{1b}]2;{name}\u{1b}\\mux-agents relay for {name}: its state and step show in herdr.{NL}Type a line and press Enter to send it to {name}'s mailbox as from herdr. Stop the agent from tmux (mux-agent stop {name}).{NL}"
    ));
    let lines = typed_lines();
    let started = std::time::Instant::now();
    let (mut last, mut done_spell): (Option<Shown>, Option<String>) = (None, None);
    loop {
        // What was typed since the last look first, so input never holds the polling up, nor polling the input.
        while let Ok(line) = lines.try_recv() {
            deliver(ctx, &place, &name, &line);
        }
        if !place.folder.join("record.json").is_file() {
            say(&format!("{CLEAR}{name} is gone{NL}"));
            return 0;
        }
        let alive = |pid: u32, start: Option<i64>| {
            ctx.os
                .open(pid)
                .is_some_and(|opened| start.is_none_or(|s| opened.start == s))
        };
        let end = ended(
            read("launcher.json").as_ref(),
            place.folder.join("notice.json").is_file(),
            started.elapsed() >= LAUNCH_GRACE,
            alive,
        );
        if let Some(code) = end {
            let how = code.map_or("ended".to_string(), |c| format!("exit {c}"));
            say(&format!("{CLEAR}{name} ended ({how}){NL}"));
            return 0;
        }
        let status = read("status.json");
        let sent = read("last-sent.json");
        let (doing, _) = crate::agents::board::select_doing(
            status.as_ref().and_then(|s| s.get("text")),
            sent.as_ref().and_then(|s| s.get("line")),
            record.get("taskLine"),
        );
        let status_at = status
            .as_ref()
            .and_then(|s| s.get("at"))
            .and_then(Value::as_str)
            .and_then(crate::util::parse_utc);
        let waiting = crate::agents::message::waiting_now(ctx, &place.folder, status_at);
        let (now, spell) = shown(
            read("activity.json").as_ref(),
            &crate::agents::text(Some(&doing)),
            waiting,
        );
        if let Some(now) = settle(now, spell, &last, &mut done_spell) {
            let step = if now.step.is_empty() {
                String::new()
            } else {
                format!(": {}", now.step)
            };
            say(&format!(
                "{}{name} {}{step}{NL}",
                osc(&now, &name),
                now.state
            ));
            last = Some(now);
        }
        // A typed line wakes the loop at once; otherwise it looks again after POLL.
        match lines.recv_timeout(POLL) {
            Ok(line) => deliver(ctx, &place, &name, &line),
            Err(RecvTimeoutError::Timeout) => {}
            // stdin closed: keep reporting, at the poll's pace.
            Err(RecvTimeoutError::Disconnected) => std::thread::sleep(POLL),
        }
    }
}

/// One typed line to the agent's mailbox, from `herdr`: blank lines are skipped, and the result is said.
fn deliver(ctx: &Ctx, place: &Place, name: &str, line: &str) {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim().is_empty() {
        return;
    }
    if line.len() > LINE_MAX {
        say(&format!("not sent: longer than 64 KB{NL}"));
        return;
    }
    match add_mail_in(
        ctx,
        &place.state,
        &place.server_key,
        &place.id,
        "herdr",
        line,
    ) {
        Ok(_) => say(&format!("sent to {name}{NL}")),
        Err(refusal) => say(&format!("not sent: {}{NL}", refusal.message)),
    }
}

const NL: &str = "\r\n";

fn say(text: &str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

fn typed_lines() -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// Windows: the console passes escape sequences on only with virtual terminal processing.
#[cfg(windows)]
fn enable_vt() {
    use windows_sys::Win32::System::Console::{
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_OUTPUT_HANDLE,
        SetConsoleMode,
    };
    // SAFETY: plain console calls on this process's own output handle.
    unsafe {
        let out = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0;
        if GetConsoleMode(out, &mut mode) != 0 {
            SetConsoleMode(out, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}

#[cfg(not(windows))]
fn enable_vt() {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Fakes, TempDir};
    use serde_json::json;

    fn decode(osc: &str) -> Vec<String> {
        osc.strip_prefix("\u{1b}]7501;")
            .and_then(|s| s.strip_suffix("\u{1b}\\"))
            .unwrap()
            .split(':')
            .map(String::from)
            .collect()
    }

    #[test]
    fn osc_writes_claude_codes_field_order_in_base64() {
        let shown = Shown {
            state: "blocked",
            kind: Some("question"),
            step: "writing parser tests".into(),
        };
        assert_eq!(
            decode(&osc(&shown, "add-tests")),
            [
                "state=blocked",
                "app=mux-agents",
                "kind=question",
                &format!("title={}", crate::util::base64(b"add-tests")),
                &format!("msg={}", crate::util::base64(b"writing parser tests")),
            ]
        );
    }

    #[test]
    fn osc_drops_kind_unless_blocked_and_empty_text() {
        let shown = Shown {
            state: "working",
            kind: Some("question"),
            step: " \n ".into(),
        };
        assert_eq!(
            decode(&osc(&shown, "")),
            ["state=working", "app=mux-agents"]
        );
    }

    #[test]
    fn osc_text_is_one_clean_line_so_nothing_ends_it_early() {
        let shown = Shown {
            state: "working",
            kind: None,
            step: "a\u{1b}\\b\u{7}c\nsecond".into(),
        };
        let text = osc(&shown, "n\u{1b}x");
        assert_eq!(text.matches('\u{1b}').count(), 2, "{text:?}");
        assert!(text.contains(&crate::util::base64(b"a\\bc")));
        assert!(text.contains(&crate::util::base64(b"nx")));
    }

    #[test]
    fn shown_maps_reports_to_herdr_words() {
        let a = |v: Value| shown(Some(&v), "", false).0;
        let at = "2026-11-13T12:00:00.0000000Z";
        assert_eq!(a(json!({"state": "working", "since": at})).state, "working");
        assert_eq!(a(json!({"state": "idle", "since": at})).state, "done");
        let blocked = a(json!({"state": "blocked", "kind": "permission", "since": at}));
        assert_eq!(
            (blocked.state, blocked.kind),
            ("blocked", Some("permission"))
        );
        assert_eq!(
            a(json!({"state": "blocked", "kind": "other", "since": at})).kind,
            None
        );
        assert_eq!(
            a(json!({"state": "working", "kind": "question", "since": at})).kind,
            None
        );
        // No readable report: working, or idle while it listens, as the side bar shows it.
        assert_eq!(shown(None, "", false).0.state, "working");
        assert_eq!(shown(None, "", true).0.state, "idle");
        assert_eq!(
            shown(Some(&json!({"state": "idle"})), "", false).0.state,
            "working"
        );
        assert_eq!(shown(None, "  step one\nmore", false).0.step, "step one");
        // The idle spell is the report's since; only `done` has one.
        assert_eq!(
            shown(Some(&json!({"state": "idle", "since": at})), "", false).1,
            Some(at.into())
        );
        assert_eq!(
            shown(Some(&json!({"state": "working", "since": at})), "", false).1,
            None
        );
    }

    #[test]
    fn settle_writes_changes_and_each_idle_spell_done_once() {
        let s = |state: &'static str, step: &str| Shown {
            state,
            kind: None,
            step: step.into(),
        };
        let (mut last, mut spell) = (None, None);
        let step = |now: Shown,
                    idle: Option<&str>,
                    last: &mut Option<Shown>,
                    spell: &mut Option<String>| {
            let out = settle(now, idle.map(String::from), last, spell);
            if let Some(out) = &out {
                *last = Some(out.clone());
            }
            out.map(|o| o.state)
        };
        assert_eq!(
            step(s("working", "a"), None, &mut last, &mut spell),
            Some("working")
        );
        assert_eq!(step(s("working", "a"), None, &mut last, &mut spell), None);
        assert_eq!(
            step(s("done", "a"), Some("t1"), &mut last, &mut spell),
            Some("done")
        );
        // The same spell writes nothing more: herdr keeps `done` until the person looks (seen live: a second
        // report turned it idle a second later).
        assert_eq!(
            step(s("done", "a"), Some("t1"), &mut last, &mut spell),
            None
        );
        // A step change within the spell is no news either: never a second done, never an early idle.
        assert_eq!(
            step(s("done", "b"), Some("t1"), &mut last, &mut spell),
            None
        );
        // A whole working spell between two looks: a new since is done again.
        assert_eq!(
            step(s("done", "b"), Some("t2"), &mut last, &mut spell),
            Some("done")
        );
        // Back to work after a done: written.
        assert_eq!(
            step(s("working", "c"), None, &mut last, &mut spell),
            Some("working")
        );
    }

    #[test]
    fn ended_reads_the_exit_code_a_notice_then_the_launchers_life() {
        let alive = |_: u32, _: Option<i64>| true;
        let dead = |_: u32, _: Option<i64>| false;
        let running = json!({"pid": 7, "start": 1});
        assert_eq!(ended(None, false, false, dead), None);
        assert_eq!(ended(None, false, true, dead), Some(None));
        assert_eq!(ended(Some(&running), false, true, alive), None);
        assert_eq!(ended(Some(&running), false, false, dead), Some(None));
        assert_eq!(ended(Some(&running), true, false, alive), Some(None));
        assert_eq!(
            ended(Some(&json!({"pid": 7, "exitCode": 3})), false, false, alive),
            Some(Some(3))
        );
        assert_eq!(
            ended(
                Some(&json!({"pid": 7, "exitCode": null})),
                false,
                false,
                alive
            ),
            None
        );
        assert_eq!(
            ended(Some(&json!({"exitCode": null})), false, false, dead),
            None
        );
        assert_eq!(
            ended(Some(&json!({"exitCode": null})), false, true, dead),
            Some(None)
        );
    }

    fn ctx_with(pairs: &[(&str, &str)]) -> Ctx {
        let mut ctx = Fakes::default().ctx();
        ctx.env =
            crate::ctx::Env::from_pairs(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        ctx
    }

    #[test]
    fn relay_args_only_in_a_herdr_terminal_and_not_turned_off() {
        let folder = Path::new("/state/k/agents/aaaaaaaaaaaa");
        assert_eq!(relay_args(&ctx_with(&[]), folder, "aaaaaaaaaaaa"), None);
        assert_eq!(
            relay_args(&ctx_with(&[("HC_TERMINAL_ID", "")]), folder, "a"),
            None
        );
        assert_eq!(
            relay_args(&ctx_with(&[("HC_TERMINAL_ID", "--all")]), folder, "a"),
            None
        );
        let off = [("HC_TERMINAL_ID", "t1h"), ("MUX_AGENTS_HERDR", "off")];
        assert_eq!(relay_args(&ctx_with(&off), folder, "a"), None);
        // A state root of its own still relays: the relay takes it from the spec's path.
        let moved = [("HC_TERMINAL_ID", "t1h"), ("MUX_AGENTS_STATE", "/s")];
        assert!(relay_args(&ctx_with(&moved), folder, "a").is_some());
        let args = relay_args(
            &ctx_with(&[("HC_TERMINAL_ID", "t1h")]),
            folder,
            "aaaaaaaaaaaa",
        )
        .unwrap();
        let spec = folder.join("spec.json").to_string_lossy().into_owned();
        let cwd = folder.to_string_lossy().into_owned();
        assert_eq!(
            args,
            [
                "run",
                "--json",
                "--bg",
                "--no-notify",
                "--rm",
                "--parent",
                "t1h",
                "--label",
                "mux-agents.agent=aaaaaaaaaaaa",
                "--cwd",
                &cwd,
                "--",
                "/bin/mux-agent",
                "relay",
                &spec,
            ]
        );
    }

    fn hc_dir() -> (TempDir, String) {
        let dir = TempDir::new();
        let image = if cfg!(windows) { "hc.exe" } else { "hc" };
        let path = dir.path().join(image);
        std::fs::write(&path, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let var = dir.path().to_string_lossy().into_owned();
        (dir, var)
    }

    #[test]
    fn start_relay_answers_the_address_or_why_not() {
        let folder = Path::new("/state/k/agents/aaaaaaaaaaaa");
        let out = |code: i32, stdout: &str, stderr: &str| crate::spawn::Out {
            code,
            stdout: stdout.into(),
            stderr: stderr.into(),
        };
        // No herdr terminal: nothing is tried, hc never runs.
        let mut never =
            |_: &Path, _: &[&str]| -> std::io::Result<crate::spawn::Out> { panic!("ran") };
        assert_eq!(
            start_relay(&ctx_with(&[]), folder, "aaaaaaaaaaaa", &mut never),
            None
        );
        assert_eq!(
            start_relay(
                &ctx_with(&[("HC_TERMINAL_ID", "t1h"), ("PATH", "")]),
                folder,
                "a",
                &mut never
            ),
            Some(Err("hc is not on PATH".into()))
        );
        let (_dir, path) = hc_dir();
        let ctx = ctx_with(&[("HC_TERMINAL_ID", "t1h"), ("PATH", &path)]);
        let mut seen = Vec::new();
        let mut ok = |hc: &Path, args: &[&str]| {
            seen.push((
                hc.to_path_buf(),
                args.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
            ));
            Ok(out(0, "{\"address\":\"t1k\",\"parent\":\"t1h\"}\n", ""))
        };
        assert_eq!(
            start_relay(&ctx, folder, "aaaaaaaaaaaa", &mut ok),
            Some(Ok("t1k".into()))
        );
        assert_eq!(seen[0].1[..3], ["run", "--json", "--bg"]);
        assert!(seen[0].0.starts_with(&path));
        let mut stale =
            |_: &Path, _: &[&str]| Ok(out(3, "", "{\"error\":{\"kind\":\"not_found\"}}\nmore"));
        assert_eq!(
            start_relay(&ctx, folder, "a", &mut stale),
            Some(Err(
                "hc run exited 3: {\"error\":{\"kind\":\"not_found\"}}".into()
            ))
        );
        let mut garbled = |_: &Path, _: &[&str]| Ok(out(0, "not json", ""));
        assert_eq!(
            start_relay(&ctx, folder, "a", &mut garbled),
            Some(Err("hc run answered no address".into()))
        );
        let mut failed = |_: &Path, _: &[&str]| {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out",
            ))
        };
        assert_eq!(
            start_relay(&ctx, folder, "a", &mut failed),
            Some(Err("hc run failed: timed out".into()))
        );
    }

    #[test]
    fn place_takes_only_a_committed_agents_spec() {
        let dir = TempDir::new();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let root = PathBuf::from(
            root.to_string_lossy()
                .trim_start_matches(r"\\?\")
                .to_string(),
        );
        let folder = root.join("srvkey").join("agents").join("aaaaaaaaaaaa");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("spec.json"), b"{}").unwrap();
        let spec = folder.join("spec.json").to_string_lossy().into_owned();
        // Not committed yet.
        assert_eq!(place(&spec), None);
        std::fs::write(folder.join("commit.json"), b"{}").unwrap();
        assert_eq!(
            place(&spec),
            Some(Place {
                state: root.join("srvkey"),
                server_key: "srvkey".into(),
                id: "aaaaaaaaaaaa".into(),
                folder: folder.clone(),
            })
        );
        // Not an agent id, not under agents, not spec.json, relative, with `..`.
        let bad = root.join("srvkey").join("agents").join("nothexnothex");
        let other = root.join("srvkey").join("mail").join("aaaaaaaaaaaa");
        for dir in [&bad, &other] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("spec.json"), b"{}").unwrap();
            std::fs::write(dir.join("commit.json"), b"{}").unwrap();
        }
        let dotted = root
            .join("srvkey")
            .join("agents")
            .join("..")
            .join("agents")
            .join("aaaaaaaaaaaa");
        for spec in [
            bad.join("spec.json"),
            other.join("spec.json"),
            folder.join("record.json"),
            PathBuf::from("srvkey/agents/aaaaaaaaaaaa/spec.json"),
            dotted.join("spec.json"),
        ] {
            assert_eq!(place(&spec.to_string_lossy()), None, "{spec:?}");
        }
    }

    #[test]
    fn a_herdr_line_lands_in_the_agents_mailbox_under_sends_lock() {
        let dir = TempDir::new();
        let fakes = Fakes::default();
        let ctx = fakes.ctx();
        let state = dir.path().join("srv");
        std::fs::create_dir_all(state.join("mail")).unwrap();
        add_mail_in(
            &ctx,
            &state,
            "srv",
            "aaaaaaaaaaaa",
            "herdr",
            "check the parser",
        )
        .unwrap();
        let text = std::fs::read_to_string(state.join("mail").join("aaaaaaaaaaaa.jsonl")).unwrap();
        let message: Value = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(
            (&message["from"], &message["pane"], &message["body"]),
            (&json!("herdr"), &json!("herdr"), &json!("check the parser"))
        );
        assert!(
            fakes.locks().iter().any(|k| k == "+mail|srv|aaaaaaaaaaaa"),
            "{:?}",
            fakes.locks()
        );
    }
}
