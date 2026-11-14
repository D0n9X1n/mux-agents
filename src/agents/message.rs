//! send, read and stop (MuxAgents.ps1:1298-1333, 1354-1421).

use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value, json};

use super::mail::{add_mail, doing_line, message_text, read_mail};
use super::registry::{self, Notice, add_notice, self_of};
use super::view::first;
use super::{FILES_ONLY, lock, lock_key, pane_id, server_dir, text, usage};
use crate::caller::{Kind, resolve};
use crate::cli::Request;
use crate::ctx::Ctx;
use crate::error::{Res, refuse};
use crate::mux::native;
use crate::state::fs::{BODY_MAX, read_json, read_text, write_json};

/// Appends a message to a live agent's, the parent's or a session pane's mailbox. Never types into a pane.
pub fn send(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let (body, body_file) = (request.value("Body"), request.value("BodyFile"));
    if body.is_some() == body_file.is_some() {
        return Err(usage("send"));
    }
    let caller = resolve(ctx)?;
    let body = match (body, body_file) {
        (Some(_), _) if caller.kind == Kind::Agent => return refuse(2, "usage", FILES_ONLY),
        (Some(body), _) => body.to_string(),
        (None, file) => read_text(&ctx.cwd.join(file.unwrap_or_default()), BODY_MAX)?,
    };
    let body = message_text(&body);
    if body.len() as u64 > BODY_MAX {
        return refuse(2, "usage", "the message is larger than 64 KB");
    }
    let target = first(request);
    // A malformed name refuses before any pane is listed.
    if target != "parent" && !pane_id(target) && !super::agent_name(target) {
        return refuse(2, "target", "no live agent has that name");
    }
    // One snapshot names the sender and the recipient; a tag never names either.
    let snapshot = registry::Snapshot::take(ctx, &caller)?;
    let me = match super::membership::proven_self(ctx, &caller) {
        Some(me) => me?,
        None => self_of(&caller, &snapshot)?,
    };
    let pane = if target == "parent" {
        match &me.record {
            Some(record) => text(record.get("parentPane")),
            None => return refuse(2, "parent", "only an agent has a parent"),
        }
    } else if pane_id(target) {
        target.to_string()
    } else {
        if !super::agent_name(target) {
            return refuse(2, "target", "no live agent has that name");
        }
        match snapshot.find(target) {
            Some(agent) => agent.pane,
            None => return refuse(2, "target", "no live agent has that name"),
        }
    };
    // The pane must be a live pane of this session; its mailbox is the running agent's there, else `pane-<n>`.
    let probe = native(
        ctx,
        &["display-message", "-p", "-t", &pane, RECIPIENT],
        true,
    )?;
    let line = probe.stdout.trim_end_matches(['\r', '\n']);
    let live = probe.code == 0
        && line.split_once('|').is_some_and(|(at, session)| {
            at == pane && session == caller.session && pane_session(session)
        });
    if !live {
        return refuse(
            2,
            "target",
            "the recipient is not a live pane of this session",
        );
    }
    let key = registry::mailbox_of(&snapshot, &pane)?;
    // The recipient's Claude Code session, when it is one: a running Claude agent's own, or (to `parent`) the session
    // its spawner recorded. The sender's hooks wake it with it (#92).
    let claude = |record: &Value, field: &str| {
        let id = record.get(field).and_then(Value::as_str)?;
        crate::checks::session_id(id).then(|| id.to_string())
    };
    let to_session = match snapshot.at(&pane) {
        Some(agent) if agent.record.get("client").and_then(Value::as_str) == Some("claude") => {
            claude(&agent.record, "sessionId")
        }
        Some(_) => None,
        None if target == "parent" => me
            .record
            .as_ref()
            .and_then(|r| r.get("parentAgent"))
            .filter(|p| p.get("client").and_then(Value::as_str) == Some("claude"))
            .and_then(|p| claude(p, "sessionId")),
        None => None,
    };
    let deliver = || {
        let id = add_mail(ctx, &caller, &key, &me.name, &caller.pane, &body, None)?;
        if me.record.is_some() {
            // Best effort, inside the sender lock, so last-sent.json names the last appended message.
            let file = server_dir(ctx, &caller)
                .join("agents")
                .join(&me.key)
                .join("last-sent.json");
            let at = crate::util::utc_o(ctx.clock.now());
            let _ = write_json(&file, &json!({"line": doing_line(&body), "at": at}));
            // A message sent is the agent's step done: it no longer waits.
            if let Some(folder) = file.parent() {
                clear_waiting(folder);
            }
        }
        Ok(id)
    };
    // Lock order: the sender's agent lock, then (inside add_mail) the recipient's mail lock.
    let id = match me.record {
        Some(_) => lock(ctx, &lock_key("agent", &caller, &me.key), deliver)?,
        None => deliver()?,
    };
    let mut reply = Map::new();
    reply.insert("to".into(), target.into());
    reply.insert("pane".into(), pane.into());
    reply.insert("id".into(), id.into());
    reply.insert("from".into(), me.name.clone().into());
    reply.insert(
        "toSessionId".into(),
        to_session.map_or(Value::Null, Value::from),
    );
    Ok(reply)
}

const RECIPIENT: &str = "#{pane_id}|#{session_id}";

fn pane_session(text: &str) -> bool {
    text.strip_prefix('$')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// .NET's int.TryParse: white space around, one sign, ASCII digits, in range.
fn int(text: &str) -> Option<i32> {
    let text = text.trim_matches(|c| matches!(c, '\t'..='\r' | ' '));
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

// A flag's value within bounds, its default when absent, `None` when malformed.
fn bounded(request: &Request, name: &str, default: i32, range: RangeInclusive<i32>) -> Option<i32> {
    match request.value(name) {
        None => Some(default),
        Some(value) => int(value).filter(|n| range.contains(n)),
    }
}

/// `read [-Wait <s>]`: the caller's unread messages, waiting up to 600 s for one. `read <name> [-Lines n]`: the
/// pane's tail.
pub fn read(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let (Some(wait), Some(lines)) = (
        bounded(request, "Wait", 0, 0..=600),
        bounded(request, "Lines", 60, 1..=2000),
    ) else {
        return Err(usage("read"));
    };
    let named = !request.positional.is_empty();
    if (named && request.value("Wait").is_some()) || (!named && request.value("Lines").is_some()) {
        return Err(usage("read"));
    }
    let caller = resolve(ctx)?;
    let mut reply = Map::new();
    if named {
        let (pane, tail) = capture_hc(ctx, &caller, first(request), None, lines)?;
        reply.insert("name".into(), pane.name.into());
        reply.insert("pane".into(), pane.pane.into());
        reply.insert("text".into(), tail.into());
        return Ok(reply);
    }
    // One membership snapshot names the reader and serves the first reconciliation.
    let snapshot = super::membership::Snapshot::take(ctx, &caller)?;
    let me = snapshot.reader(&caller)?;
    // Removed on every return, mail, timeout or error, when this guard drops.
    let _waiting = match &me.record {
        Some(_) if wait > 0 => Waiting::start(ctx, &caller, &me.key),
        _ => None,
    };
    let deadline = ctx.clock.now() + Duration::from_secs(wait as u64);
    let mut first_pass = Some(snapshot);
    let messages = loop {
        let snapshot = match first_pass.take() {
            Some(snapshot) => snapshot,
            None => super::membership::Snapshot::take(ctx, &caller)?,
        };
        registry::reap_hc(ctx, &caller, &snapshot)?;
        let messages = read_mail(ctx, &caller, &me.key)?;
        if !messages.is_empty() || ctx.clock.now() >= deadline {
            break messages;
        }
        // File-watch waits are a non-goal; this polls as the reference did.
        ctx.clock.sleep(POLL);
    };
    reply.insert(
        "messages".into(),
        Value::Array(messages.into_iter().map(Value::Object).collect()),
    );
    Ok(reply)
}

/// The last `lines` lines of a running agent's pane, captured by the server into a private buffer only while the
/// pane is still its live launch (one `if-shell`), then read and deleted. No buffer (the check failed) refuses 2
/// `target`: a pane replaced in between is never read.
pub(crate) fn capture(ctx: &Ctx, pane: &registry::Pane, lines: i32) -> Res<String> {
    let buffer = format!("mux-agents-{}", crate::util::guid()?.replace('-', ""));
    let p = pane.pane.as_str();
    let command = format!("capture-pane -J -t {p} -b {buffer} -S -{lines}");
    let argv = registry::bound(p, pane.launch.live(), command);
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let captured =
        native(ctx, &argv, true).and_then(|_| native(ctx, &["show-buffer", "-b", &buffer], true));
    // Always, whatever happened: a buffer left behind would hold the pane's text.
    let _ = native(ctx, &["delete-buffer", "-b", &buffer], true);
    // No buffer, or a call that failed as a call: nothing proves the text is the agent's.
    let Some(captured) = captured.ok().filter(|c| c.code == 0) else {
        return refuse(2, "target", "the agent's pane is no longer its launch");
    };
    let all: Vec<&str> = captured
        .stdout
        .trim_end_matches(['\r', '\n'])
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    Ok(all[all.len().saturating_sub(lines as usize)..].join("\n"))
}

/// `agents/<id>/waiting-<pid>.json`: the agent's `read -Wait` run by process `pid` (S2.4).
fn waiting_file(folder: &Path, pid: u32) -> PathBuf {
    folder.join(format!("waiting-{pid}.json"))
}

/// The pid a waiting file's name holds; none for any other file.
fn waiting_pid(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("waiting-")?.strip_suffix(".json")?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

/// Every waiting file in an agent's folder, with the pid its name holds.
fn waiting_files(folder: &Path) -> Vec<(u32, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let pid = waiting_pid(e.file_name().to_str()?)?;
            Some((pid, e.path()))
        })
        .collect()
}

/// Removes all of an agent's waiting files; its caller holds the agent lock. Best effort.
pub(crate) fn clear_waiting(folder: &Path) {
    for (_, file) in waiting_files(folder) {
        let _ = std::fs::remove_file(file);
    }
}

/// Whether the agent waits: some waiting file's pid is alive with its recorded start, and the file is newer than
/// `status_at` (none: no status). A file whose process is gone or reused, or that cannot be read, is deleted. The
/// caller holds the agent lock.
pub(crate) fn waiting_now(ctx: &Ctx, folder: &Path, status_at: Option<SystemTime>) -> bool {
    let mut waiting = false;
    for (pid, file) in waiting_files(folder) {
        let marker = read_json(&file).ok().flatten();
        let field = |name: &str| marker.as_ref().and_then(|m| m.get(name));
        let since = field("since")
            .and_then(Value::as_str)
            .and_then(crate::util::parse_utc);
        let start = field("start").and_then(Value::as_i64);
        let named = field("pid").and_then(Value::as_u64) == Some(u64::from(pid));
        let alive =
            named && start.is_some() && ctx.os.row(pid).is_some_and(|row| row.start == start);
        let Some(since) = since.filter(|_| alive) else {
            let _ = std::fs::remove_file(&file);
            continue;
        };
        if status_at.is_none_or(|at| since > at) {
            waiting = true;
        }
    }
    waiting
}

/// The caller's own waiting file while its `read -Wait` runs; removed under the agent lock when dropped.
struct Waiting<'a> {
    ctx: &'a Ctx,
    key: String,
    file: PathBuf,
}

impl<'a> Waiting<'a> {
    /// Writes `{pid, start, since}` under the agent lock before the first poll. Best effort: no file without a
    /// known start time (it could not be told from a reused pid) or when the write fails.
    fn start(ctx: &'a Ctx, caller: &crate::caller::Caller, id: &str) -> Option<Waiting<'a>> {
        let start = ctx.os.row(ctx.pid)?.start?;
        let file = waiting_file(&server_dir(ctx, caller).join("agents").join(id), ctx.pid);
        let since = crate::util::utc_o(ctx.clock.now());
        let key = lock_key("agent", caller, id);
        lock(ctx, &key, || {
            write_json(
                &file,
                &json!({"pid": ctx.pid, "start": start, "since": since}),
            )
        })
        .ok()?;
        Some(Waiting { ctx, key, file })
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        // While unwinding, the file goes without the lock, so a second panic cannot abort.
        if std::thread::panicking() {
            let _ = std::fs::remove_file(&self.file);
            return;
        }
        let removed = lock(self.ctx, &self.key, || {
            let _ = std::fs::remove_file(&self.file);
            Ok(())
        });
        if removed.is_err() {
            let _ = std::fs::remove_file(&self.file);
        }
    }
}

const POLL: Duration = Duration::from_millis(500);

/// Refuses an agent caller that is not the record's parent.
fn parent_or_human(caller: &crate::caller::Caller, record: &Value) -> Res<()> {
    if caller.kind == Kind::Agent
        && record.get("parentPane").and_then(Value::as_str) != Some(caller.pane.as_str())
    {
        return refuse(
            2,
            "parent",
            "only the parent or a human can stop this agent",
        );
    }
    Ok(())
}

pub fn capture_hc(
    ctx: &Ctx,
    caller: &crate::caller::Caller,
    name: &str,
    expected: Option<&str>,
    lines: i32,
) -> Res<(registry::Pane, String)> {
    if !(1..=2000).contains(&lines) {
        return Err(usage("read"));
    }
    let id = super::member_target::named(ctx, caller, name)?;
    lock(ctx, &lock_key("member", caller, &id.id), || {
        let record = super::member_target::current(ctx, caller, &id)?;
        let Some(pane) = id.view(&record, &registry::rows(ctx, caller)?) else {
            return refuse(
                2,
                "target",
                format!("the agent has no live view; run focus {name}"),
            );
        };
        if expected.is_some_and(|e| e != pane.pane) {
            return refuse(
                2,
                "target",
                "the agent is no longer on the pane the bar drew",
            );
        }
        let tail = capture(ctx, &pane, lines)?;
        Ok((pane, tail))
    })
}

pub fn stop_hc(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let caller = resolve(ctx)?;
    let id = super::member_target::named(ctx, &caller, first(request))?;
    registry::with_member_notice(ctx, &caller, &id.id, || {
        let record = super::member_target::current(ctx, &caller, &id)?;
        parent_or_human(&caller, &record)?;
        let core = crate::hc::connect(ctx)?;
        core.kill(&id.address)?;
        let fact = core.get(&id.address).ok().filter(|t| id.matches(t));
        use super::lifecycle::{Life, classify};
        let life = match classify(fact.as_ref(), &id.launcher) {
            Life::Running => "running",
            Life::Lost => "lost",
            Life::Ended => "ended",
            Life::Unknown => "unknown",
        };
        add_notice(ctx, &caller, &id.id, &record, Notice::Stopped)?;
        let mut reply = Map::new();
        reply.insert("name".into(), id.name.clone().into());
        reply.insert("hcAddress".into(), id.address.clone().into());
        reply.insert("life".into(), life.into());
        Ok(reply)
    })
}

#[cfg(test)]
mod tests {
    use super::stop_hc as stop;
    use super::*;
    use crate::agents::core_fixture as cf;
    use crate::agents::fixture::{ID, World, launch_command, write};
    use crate::agents::lock_key;
    use crate::agents::view::focus;
    use crate::error::Refusal;
    use crate::mux::Output;
    use serde_json::json;

    type Handler = fn(&Ctx, &Request) -> Res<Map<String, Value>>;

    const PROBE: &str = "#{pane_id}|#{session_id}";
    const NONE: &str = "2 target no live agent has that name";

    fn target_refusal(verb: &str) -> &'static str {
        if verb == "focus" || verb == "stop" || verb == "read" {
            "2 target no unique committed member has that identity"
        } else {
            NONE
        }
    }
    fn run(w: &World, handler: Handler, args: &[&str]) -> String {
        let result = handler(&w.ctx, &w.request(args));
        w.fakes.hc_done().unwrap();
        match result {
            Ok(reply) => format!("ok {}", Value::Object(reply)),
            Err(r) => format!("{} {} {}", r.code, r.check, r.message),
        }
    }

    // Each verb that names a target, with the arguments around the name.
    fn verbs(name: &str) -> Vec<(Handler, Vec<String>)> {
        let args = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        vec![
            (focus as Handler, args(&["focus", name])),
            (read, args(&["read", name])),
            (send, args(&["send", name, "-Body", "x"])),
            (stop, args(&["stop", name])),
        ]
    }

    fn run_owned(w: &World, handler: Handler, args: &[String]) -> String {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        run(w, handler, &args)
    }

    fn mux_verbs(w: &World) -> Vec<String> {
        w.fakes.mux_calls().iter().map(|c| c[1].clone()).collect()
    }

    #[test]
    fn target_malformed_name() {
        for name in ["Worker", "1x", "worker!", "a_b", &"a".repeat(25)] {
            let w = World::human("");
            w.agent_record(ID, json!({"name": "worker"}));
            w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
            for (handler, args) in verbs(name) {
                w.fakes.clear_log();
                if args[0] == "focus" {
                    cf::online(&w);
                }
                assert_eq!(
                    run_owned(&w, handler, &args),
                    target_refusal(&args[0]),
                    "{args:?}"
                );
                // Refused before any pane is listed.
                assert!(!mux_verbs(&w).iter().any(|v| v == "list-panes"), "{args:?}");
            }
        }
    }

    #[test]
    fn target_unknown_name() {
        let w = World::human("");
        w.agent_record(ID, json!({}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        for name in ["nobody", "worke", "workers"] {
            for (handler, args) in verbs(name) {
                w.fakes.clear_log();
                if args[0] == "focus" {
                    cf::online(&w);
                }
                assert_eq!(
                    run_owned(&w, handler, &args),
                    target_refusal(&args[0]),
                    "{args:?}"
                );
                assert_eq!(
                    mux_verbs(&w),
                    if args[0] == "focus" || args[0] == "stop" || args[0] == "read" {
                        vec!["display-message"]
                    } else {
                        vec!["display-message", "list-panes"]
                    },
                    "{args:?}"
                );
            }
        }
    }

    #[test]
    fn target_uncommitted() {
        // Tagged and live but without commit.json: not an agent yet.
        let w = World::human("");
        w.uncommitted(ID, json!({}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        for (handler, args) in verbs("worker") {
            w.fakes.clear_log();
            if args[0] == "focus" {
                cf::online(&w);
            }
            assert_eq!(
                run_owned(&w, handler, &args),
                target_refusal(&args[0]),
                "{args:?}"
            );
            if args[0] != "send" {
                assert_eq!(mux_verbs(&w), ["display-message"], "{args:?}");
            }
        }
        // Committed without a record: stop has no hc identity and closes nothing.
        let w = World::human("");
        let folder = w.folder(ID);
        std::fs::create_dir_all(&folder).unwrap();
        write(&folder.join("commit.json"), json!({}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        w.fakes.clear_log();
        assert_eq!(run(&w, stop, &["stop", "worker"]), target_refusal("stop"));
        assert!(pane_kills(&w).is_empty());
    }

    #[test]
    fn send_parent_from_human_refuses_parent() {
        let w = World::human("");
        w.fakes.clear_log();
        assert_eq!(
            run(&w, send, &["send", "parent", "-Body", "x"]),
            "2 parent only an agent has a parent"
        );
        // One pane list names the sender; no recipient probe, nothing written.
        assert_eq!(mux_verbs(&w), ["display-message", "list-panes"]);
        assert!(!w.ctx.state_root.exists());
    }

    #[test]
    fn tagged_sender_without_record_refuses_parent() {
        let refused = "2 parent this agent pane has no record";
        for tag in ["bbbbbbbbbbbb", "tag-7"] {
            let w = World::human(tag);
            w.agent_record(ID, json!({}));
            w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
            w.recipients_live();
            for args in [
                &["send", "parent", "-Body", "x"][..],
                &["send", "%1", "-Body", "x"],
                &["send", "worker", "-Body", "x"],
                &["read"],
            ] {
                if args[0] == "read" {
                    poll(&w, &[]);
                }
                assert_eq!(
                    run(&w, args_handler(args[0]), args),
                    refused,
                    "{tag} {args:?}"
                );
            }
            assert!(!w.state().join("mail").join("pane-1.jsonl").exists());
        }
    }

    fn args_handler(verb: &str) -> Handler {
        match verb {
            "send" => send,
            "read" => read,
            _ => stop,
        }
    }

    // An agent pane %1, running agent ID by its record and launch, whose parent is %4; the recipient probe answers
    // every pane live in session $3.
    fn agent_world() -> World {
        let w = World::agent(ID);
        w.agent_record(ID, json!({"pane": "%1", "parentPane": "%4"}));
        w.panes(&["%1|@1|100|aaaaaaaaaaaa|worker"]);
        w.recipients_live();
        w
    }

    #[test]
    fn send_agent_body_refused_use_bodyfile() {
        let w = agent_world();
        assert_eq!(
            run(&w, send, &["send", "parent", "-Body", "x"]),
            "2 usage agents pass free text only in files: use -BodyFile"
        );
        let usage = "2 usage usage: mux-agent send <name|parent|%pane> (-Body <text> | -BodyFile <path>) [-Text]";
        w.file("b.md", b"ok");
        w.fakes.clear_log();
        assert_eq!(run(&w, send, &["send", "parent"]), usage);
        assert_eq!(
            run(
                &w,
                send,
                &["send", "parent", "-Body", "x", "-BodyFile", "b.md"]
            ),
            usage
        );
        assert!(w.fakes.mux_calls().is_empty());
        let reply = run(&w, send, &["send", "parent", "-BodyFile", "b.md"]);
        assert!(
            reply.starts_with(r#"ok {"to":"parent","pane":"%4","id":""#),
            "{reply}"
        );
        let messages = read_mail_of(&w, "pane-4");
        assert_eq!(messages, ["ok"]);
        // A human may pass -Body.
        let w = World::human("");
        w.recipients_live();
        assert!(run(&w, send, &["send", "%1", "-Body", "hi"]).starts_with("ok "));
    }

    // The bodies of a mailbox's unread messages.
    fn read_mail_of(w: &World, key: &str) -> Vec<String> {
        read_mail(&w.ctx, &w.caller(), key)
            .unwrap()
            .iter()
            .map(|m| {
                crate::agents::text(m.get("text"))
                    .split_once('\n')
                    .unwrap()
                    .1
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn send_64kb_limit() {
        let w = World::human("");
        w.recipients_live();
        let big = "the message is larger than 64 KB";
        let send_body = |body: &str| {
            let args = ["send", "%1", "-Body", body];
            run(&w, send, &args)
        };
        // Counted in UTF-8 bytes after cleaning: CR LF becomes LF and controls go.
        assert!(send_body(&"a".repeat(65536)).starts_with("ok "));
        assert_eq!(send_body(&"a".repeat(65537)), format!("2 usage {big}"));
        assert!(send_body(&format!("{}\r\n\u{7}", "a".repeat(65535))).starts_with("ok "));
        assert!(send_body(&"\u{e9}".repeat(32768)).starts_with("ok "));
        assert_eq!(
            send_body(&format!("{}a", "\u{e9}".repeat(32768))),
            format!("2 usage {big}")
        );
        // A file over 64 KB is refused by the reader, before cleaning.
        let w = agent_world();
        w.file("ok.md", "a".repeat(65536).as_bytes());
        w.file("big.md", format!("{}\r\n", "a".repeat(65535)).as_bytes());
        assert!(run(&w, send, &["send", "parent", "-BodyFile", "ok.md"]).starts_with("ok "));
        assert_eq!(
            run(&w, send, &["send", "parent", "-BodyFile", "big.md"]),
            "2 usage a task or brief file is larger than 65536 bytes"
        );
    }

    #[test]
    fn send_target_other_session_refuses() {
        let refused = "2 target the recipient is not a live pane of this session";
        for (code, out) in [
            (0, "%5|$4\n"),
            (1, "%5|$3\n"),
            (0, ""),
            (0, "|\n"),
            (0, "%5\n"),
            (0, "%6|$3\n"),
            (0, "%5|$3\nmore\n"),
            (0, "%5|$03|\n"),
            (0, "%5|3\n"),
        ] {
            let w = World::human("");
            w.fakes.mux_first(PROBE, code, out, "");
            assert_eq!(
                run(&w, send, &["send", "%5", "-Body", "x"]),
                refused,
                "{code} {out:?}"
            );
            assert!(!w.ctx.state_root.exists(), "{out:?}");
        }
        // The probe goes to the named pane and never reads a tag; a running agent's pane is its id's mailbox, any
        // other pane (whatever tag it carries) is pane-<n>.
        let w = World::human("");
        w.agent_record("bbbbbbbbbbbb", json!({"pane": "%5"}));
        w.panes(&["%5|@5|300|cccccccccccc|other", "%6|@5|301|bbbbbbbbbbbb|w"]);
        w.recipients_live();
        w.fakes.clear_log();
        assert!(
            run(&w, send, &["send", "%5", "-Body", "x"])
                .starts_with(r#"ok {"to":"%5","pane":"%5""#)
        );
        assert_eq!(
            w.fakes.mux_calls()[2],
            ["-N", "display-message", "-p", "-t", "%5", PROBE]
        );
        assert_eq!(read_mail_of(&w, "bbbbbbbbbbbb"), ["x"]);
        run(&w, send, &["send", "%6", "-Body", "y"]);
        assert_eq!(read_mail_of(&w, "pane-6"), ["y"]);
        assert!(read_mail_of(&w, "cccccccccccc").is_empty());
    }

    #[test]
    fn send_sender_lock_then_mail_lock() {
        let w = agent_world();
        w.file("b.md", b"ok");
        let caller = w.caller();
        let sent = w.folder(ID).join("last-sent.json");
        let mailbox = w.state().join("mail").join("pane-4.jsonl");
        let seen = sent.clone();
        w.fakes.lock_probe(move |_| {
            let mailed = std::fs::metadata(&mailbox).is_ok_and(|m| m.len() > 0);
            format!("sent={} mailed={mailed}", seen.exists())
        });
        w.fakes.clear_log();
        assert!(run(&w, send, &["send", "parent", "-BodyFile", "b.md"]).starts_with("ok "));
        let agent = lock_key("agent", &caller, ID);
        let mail = lock_key("mail", &caller, "pane-4");
        // One hold of the sender's lock around the append and the last-sent write; the append inside the
        // recipient's mail lock.
        assert_eq!(
            w.fakes.locks(),
            [
                format!("+{agent} sent=false mailed=false"),
                format!("+{mail} sent=false mailed=false"),
                format!("-{mail} sent=false mailed=true"),
                format!("-{agent} sent=true mailed=true")
            ]
        );
        // A human sender takes only the recipient's mail lock.
        let w = World::human("");
        w.recipients_live();
        let mail = lock_key("mail", &w.caller(), "pane-1");
        w.fakes.clear_log();
        run(&w, send, &["send", "%1", "-Body", "x"]);
        assert_eq!(w.fakes.locks(), [format!("+{mail}"), format!("-{mail}")]);
    }

    #[test]
    fn last_sent_written_best_effort() {
        let w = agent_world();
        w.clock("2026-10-29T12:00:00.0000000Z");
        let sent = w.folder(ID).join("last-sent.json");
        w.file("b.md", "\r\ndone:\u{9b}\tall green\u{7}\nmore".as_bytes());
        w.file("blank.md", b" \n");
        run(&w, send, &["send", "parent", "-BodyFile", "b.md"]);
        assert_eq!(
            std::fs::read_to_string(&sent).unwrap(),
            r#"{"line":"done: all green","at":"2026-10-29T12:00:00.0000000Z"}"#
        );
        w.clock("2026-10-29T12:00:05.0000000Z");
        run(&w, send, &["send", "parent", "-BodyFile", "blank.md"]);
        assert_eq!(
            std::fs::read_to_string(&sent).unwrap(),
            r#"{"line":"","at":"2026-10-29T12:00:05.0000000Z"}"#
        );
        // A failed write still delivers the message.
        std::fs::remove_file(&sent).unwrap();
        std::fs::create_dir(&sent).unwrap();
        w.file("c.md", b"still");
        assert!(run(&w, send, &["send", "parent", "-BodyFile", "c.md"]).starts_with("ok "));
        assert_eq!(read_mail_of(&w, "pane-4").last().unwrap(), "still");
        // A human writes none.
        let w = World::human("");
        w.recipients_live();
        run(&w, send, &["send", "%1", "-Body", "x"]);
        let agents = w.state().join("agents");
        assert_eq!(std::fs::read_dir(agents).unwrap().count(), 0);
    }

    #[test]
    fn read_wait_bounds_0_600() {
        let usage =
            "2 usage usage: mux-agent read [-Wait <seconds>] | read <name> [-Lines <n>] [-Text]";
        let w = World::human("");
        w.fakes.clear_log();
        for wait in ["601", "-1", "x", "", "1.5", "99999999999", "0x10"] {
            assert_eq!(run(&w, read, &["read", "-Wait", wait]), usage, "{wait:?}");
        }
        assert_eq!(run(&w, read, &["read", "worker", "-Wait", "1"]), usage);
        assert!(w.fakes.mux_calls().is_empty());
        // .NET's int.TryParse: a sign and white space around are fine.
        for wait in ["0", "600", "+5", " 7 ", "-0"] {
            w.recipients_live();
            run(&w, send, &["send", "%1", "-Body", "m"]);
            poll(&w, &[]);
            let reply = run(&w, read, &["read", "-Wait", wait]);
            assert!(reply.contains("\\nm\""), "{wait:?} {reply}");
        }
        assert!(w.fakes.slept().is_empty());
    }

    // A live worker (%9) and a committed agent `gone` (%8) whose pane is gone, its notice not yet posted.
    fn gone_world() -> World {
        let w = World::human("");
        w.agent_record(ID, json!({}));
        w.agent_record("bbbbbbbbbbbb", json!({"name": "gone", "pane": "%8"}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        w.fakes
            .mux_rule("-t %8 #{pane_id}|#{@agent-id}", 0, "|\n", "");
        w
    }

    #[test]
    fn named_read_does_not_reap_other_members() {
        let w = gone_world();
        cf::member(&w, ID, json!({"startCommand":launch_command("%9")}));
        cf::member(&w, "bbbbbbbbbbbb", json!({"name":"gone","pane":"%8"}));
        w.fakes.mux_rule("show-buffer", 0, "one\ntwo\n", "");
        w.fakes.clear_log();
        assert_eq!(
            run(&w, read, &["read", "worker", "-Lines", "1"]),
            r#"ok {"name":"worker","pane":"%9","text":"two"}"#
        );
        assert_eq!(
            mux_verbs(&w),
            [
                "display-message",
                "list-panes",
                "if-shell",
                "show-buffer",
                "delete-buffer"
            ]
        );
        assert!(!w.folder("bbbbbbbbbbbb").join("notice.json").exists());
        assert!(read_mail_of(&w, "pane-1").is_empty());
        assert_eq!(
            run(&w, read, &["read", "gone"]),
            "2 target the agent has no live view; run focus gone"
        );
        w.fakes.hc_done().unwrap();
    }

    #[test]
    fn read_lines_1_2000() {
        let usage =
            "2 usage usage: mux-agent read [-Wait <seconds>] | read <name> [-Lines <n>] [-Text]";
        let w = World::human("");
        cf::member(&w, ID, json!({"startCommand":launch_command("%9")}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        w.fakes.clear_log();
        for lines in ["0", "2001", "x", "-1"] {
            assert_eq!(
                run(&w, read, &["read", "worker", "-Lines", lines]),
                usage,
                "{lines}"
            );
        }
        assert_eq!(run(&w, read, &["read", "-Lines", "5"]), usage);
        assert!(w.fakes.mux_calls().is_empty());
        w.fakes
            .mux_first("show-buffer", 0, "one\r\ntwo\n\nthree\r\n\n", "");
        assert_eq!(
            run(&w, read, &["read", "worker", "-Lines", "2"]),
            r#"ok {"name":"worker","pane":"%9","text":"\nthree"}"#
        );
        let live = registry::Launch {
            command: launch_command("%9"),
            pid: None,
        }
        .live();
        for (args, s) in [
            (&["read", "worker"][..], "-60"),
            (&["read", "worker", "-Lines", "2000"], "-2000"),
            (&["read", "worker", "-Lines", "1"], "-1"),
        ] {
            w.fakes.clear_log();
            assert!(run(&w, read, args).starts_with("ok "));
            // Captured by the server only while %9 is still its live launch, into a private buffer that is read and
            // then deleted.
            let calls = w.fakes.mux_calls();
            let n = calls.len();
            let capture = &calls[n - 3];
            assert_eq!(
                capture[..6],
                ["-N", "if-shell", "-F", "-t", "%9", live.as_str()]
            );
            let command: Vec<&str> = capture[6].split(' ').collect();
            assert_eq!(command[..4], ["capture-pane", "-J", "-t", "%9"]);
            assert_eq!((command[4], command[6], command[7]), ("-b", "-S", s));
            let buffer = command[5];
            assert!(
                buffer
                    .strip_prefix("mux-agents-")
                    .is_some_and(|h| h.len() == 32),
                "{buffer}"
            );
            assert_eq!(calls[n - 2], ["-N", "show-buffer", "-b", buffer]);
            assert_eq!(calls[n - 1], ["-N", "delete-buffer", "-b", buffer]);
        }
        // No buffer (the pane was not its launch when the server checked): nothing read, the buffer still deleted.
        w.fakes.mux_first("show-buffer", 1, "", "no buffer");
        w.fakes.clear_log();
        assert_eq!(
            run(&w, read, &["read", "worker"]),
            "2 target the agent's pane is no longer its launch"
        );
        assert_eq!(mux_verbs(&w).last().unwrap(), "delete-buffer");
    }

    #[test]
    fn read_wait_polls_500ms_until_deadline() {
        let w = World::human("");
        w.state();
        for _ in 0..7 {
            poll(&w, &[]);
        }
        assert_eq!(
            run(&w, read, &["read", "-Wait", "3"]),
            r#"ok {"messages":[]}"#
        );
        assert_eq!(w.fakes.slept(), [500; 6]);
        // Mail that lands while waiting ends the wait at the next poll.
        let w = World::human("");
        let mailbox = w.state().join("mail").join("pane-1.jsonl");
        let polls = std::cell::Cell::new(0);
        w.fakes.lock_probe(move |event| {
            if event.starts_with('-') {
                polls.set(polls.get() + 1);
                if polls.get() == 2 {
                    std::fs::write(&mailbox, "{\"id\":\"1\",\"from\":\"w\",\"pane\":\"%9\",\"at\":\"x\",\"body\":\"late\"}\n").unwrap();
                }
            }
            String::new()
        });
        for _ in 0..3 {
            poll(&w, &[]);
        }
        let reply = run(&w, read, &["read", "-Wait", "600"]);
        assert!(reply.contains(r#"\nlate""#), "{reply}");
        assert_eq!(w.fakes.slept(), [500, 500]);
    }

    // Every kill of a pane sent to the multiplexer.
    fn pane_kills(w: &World) -> Vec<Vec<String>> {
        w.fakes
            .mux_calls()
            .into_iter()
            .filter(|c| c[1] == "kill-pane" || c.last().is_some_and(|a| a.starts_with("kill-pane")))
            .collect()
    }

    // ---- waiting (S2.4)

    const SINCE: &str = "2026-10-29T12:00:00.0000000Z";

    // An agent caller (pid 4242, started 2000) whose own pane %1 is listed live, at SINCE.
    fn waiting_world() -> World {
        let w = agent_world();
        w.panes(&["%1|@1|100|aaaaaaaaaaaa|worker"]);
        w.clock(SINCE);
        w
    }

    // The waiting files in the caller's folder, by name.
    fn waiting_names(w: &World) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(w.folder(ID))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("waiting-"))
            .collect();
        names.sort();
        names
    }

    fn other_wait(w: &World) {
        write(
            &w.folder(ID).join("waiting-77.json"),
            json!({"pid": 77, "start": 770, "since": SINCE}),
        );
    }

    #[test]
    fn read_wait_marks_the_agent_waiting_until_it_returns() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let mine = format!(r#"{{"pid":4242,"start":2000,"since":"{SINCE}"}}"#);
        // A timeout: written under the agent lock before the first poll, there at every poll, gone after.
        let w = waiting_world();
        let r = cf::member(
            &w,
            ID,
            json!({"pane":"%1","window":"@1","parentPane":"%4","startCommand":launch_command("%1")}),
        );
        other_wait(&w);
        let file = w.folder(ID).join("waiting-4242.json");
        let seen = Rc::new(RefCell::new(Vec::new()));
        let (probe, during) = (file.clone(), seen.clone());
        w.fakes.sleep_probe(move |_| {
            during
                .borrow_mut()
                .push(std::fs::read_to_string(&probe).ok())
        });
        let agent = lock_key("agent", &w.caller(), ID);
        for _ in 0..3 {
            poll(&w, &[cf::wire(&r)]);
        }
        w.fakes.clear_log();
        assert_eq!(
            run(&w, read, &["read", "-Wait", "1"]),
            r#"ok {"messages":[]}"#
        );
        assert_eq!(*seen.borrow(), vec![Some(mine.clone()); 2]);
        let locks = w.fakes.locks();
        assert_eq!(locks[..2], [format!("+{agent}"), format!("-{agent}")]);
        assert_eq!(
            locks[locks.len() - 2..],
            [format!("+{agent}"), format!("-{agent}")]
        );
        // An overlapping wait keeps its own file.
        assert_eq!(waiting_names(&w), ["waiting-77.json"]);

        // Mail ends the wait and the file goes.
        let w = waiting_world();
        let r = cf::member(
            &w,
            ID,
            json!({"pane":"%1","window":"@1","parentPane":"%4","startCommand":launch_command("%1")}),
        );
        let mailbox = w.state().join("mail").join(format!("{ID}.jsonl"));
        let file = w.folder(ID).join("waiting-4242.json");
        let probe = file.clone();
        w.fakes.sleep_probe(move |_| {
            assert!(probe.is_file());
            std::fs::write(
                &mailbox,
                "{\"id\":\"1\",\"from\":\"p\",\"pane\":\"%4\",\"at\":\"x\",\"body\":\"go\"}\n",
            )
            .unwrap();
        });
        for _ in 0..2 {
            poll(&w, &[cf::wire(&r)]);
        }
        let reply = run(&w, read, &["read", "-Wait", "600"]);
        assert!(reply.contains(r#"\ngo""#), "{reply}");
        assert_eq!(waiting_names(&w), Vec::<String>::new());

        // A failed mail read or a failed notice pass (the reap's pane list) removes it too.
        let w = waiting_world();
        let r = cf::member(
            &w,
            ID,
            json!({"pane":"%1","window":"@1","parentPane":"%4","startCommand":launch_command("%1")}),
        );
        poll(&w, &[cf::wire(&r)]);
        w.fakes.lock_timeout("mail|");
        assert!(run(&w, read, &["read", "-Wait", "5"]).starts_with("2 lock"));
        assert_eq!(waiting_names(&w), Vec::<String>::new());
        let w = waiting_world();
        let r = cf::member(
            &w,
            ID,
            json!({"pane":"%1","window":"@1","parentPane":"%4","startCommand":launch_command("%1")}),
        );
        poll(&w, &[cf::wire(&r)]);
        w.fakes.mux_first("list-panes", 1, "", "down");
        assert!(run(&w, read, &["read", "-Wait", "5"]).starts_with("1 "));
        assert_eq!(waiting_names(&w), Vec::<String>::new());

        // -Wait 0 never waits, so it marks nothing.
        let w = waiting_world();
        let r = cf::member(
            &w,
            ID,
            json!({"pane":"%1","window":"@1","parentPane":"%4","startCommand":launch_command("%1")}),
        );
        let agent = lock_key("agent", &w.caller(), ID);
        w.fakes.clear_log();
        poll(&w, &[cf::wire(&r)]);
        run(&w, read, &["read"]);
        assert!(!w.fakes.locks().iter().any(|l| l.contains(&agent)));

        // A human caller writes nothing.
        let w = World::human("");
        w.state();
        w.fakes.sleep_probe(|_| {});
        for _ in 0..3 {
            poll(&w, &[]);
        }
        run(&w, read, &["read", "-Wait", "1"]);
        let agents = w.state().join("agents");
        assert_eq!(std::fs::read_dir(agents).unwrap().count(), 0);
    }

    #[test]
    fn send_names_the_sender_and_a_claude_recipients_session() {
        // #92: the reply names who sent it and, for a Claude Code recipient, its session, so the sender's hooks can
        // wake it (herdr delivers a message as the recipient's next input); the mailbox stays the message itself.
        const CHILD: &str = "11111111-2222-4333-8444-555555555555";
        const ROOT: &str = "66666666-7777-4888-8999-aaaaaaaaaaaa";
        let mut w = rotated("", "%1");
        let tmux = w.ctx.env.get("TMUX").unwrap().to_string();
        w.ctx.env =
            crate::ctx::Env::from_pairs([("TMUX", tmux.clone()), ("TMUX_PANE", "%1".into())]);
        w.panes(&[
            "%1|@1|100||",
            "%9|@5|300|aaaaaaaaaaaa|a",
            "%8|@5|301|bbbbbbbbbbbb|b",
        ]);
        w.agent_record(ID, json!({"name": "a", "pane": "%9", "parentPane": "%1", "client": "claude",
            "sessionId": CHILD, "parentAgent": {"client": "claude", "pid": 1, "start": 1, "sessionId": ROOT}}));
        w.agent_record(
            "bbbbbbbbbbbb",
            json!({"name": "b", "pane": "%8", "parentPane": "%1", "client": "copilot",
            "sessionId": CHILD}),
        );
        let field = |reply: &str, key: &str| -> Value {
            let json: Value = serde_json::from_str(reply.strip_prefix("ok ").unwrap()).unwrap();
            json[key].clone()
        };
        // The root to a Claude agent: its session; the sender is the root, as its mail names it.
        let sent = run(&w, send, &["send", "a", "-Body", "hi"]);
        assert_eq!(
            (field(&sent, "from"), field(&sent, "toSessionId")),
            (json!("root"), json!(CHILD)),
            "{sent}"
        );
        // To a Copilot agent: no Claude session to wake.
        let sent = run(&w, send, &["send", "b", "-Body", "hi"]);
        assert_eq!(field(&sent, "toSessionId"), Value::Null, "{sent}");
        // From agent a to its parent, the root Claude session it was spawned from.
        w.ctx.env =
            crate::ctx::Env::from_pairs([("TMUX", tmux.clone()), ("TMUX_PANE", "%9".into())]);
        let sent = run(&w, send, &["send", "parent", "-Body", "done"]);
        assert_eq!(
            (field(&sent, "from"), field(&sent, "toSessionId")),
            (json!("a"), json!(ROOT)),
            "{sent}"
        );
        // From agent c, spawned by agent a: its parent is a, so a's own session, not the root's a recorded.
        w.panes(&[
            "%1|@1|100||",
            "%9|@5|300|aaaaaaaaaaaa|a",
            "%8|@5|301|bbbbbbbbbbbb|b",
            "%7|@5|302|cccccccccccc|c",
        ]);
        w.agent_record(
            "cccccccccccc",
            json!({"name": "c", "pane": "%7", "parentPane": "%9", "client": "copilot",
            "parentAgent": {"client": "claude", "pid": 2, "start": 2, "sessionId": ROOT}}),
        );
        w.ctx.env =
            crate::ctx::Env::from_pairs([("TMUX", tmux.clone()), ("TMUX_PANE", "%7".into())]);
        let sent = run(&w, send, &["send", "parent", "-Body", "done"]);
        assert_eq!(
            (field(&sent, "from"), field(&sent, "toSessionId")),
            (json!("c"), json!(CHILD)),
            "{sent}"
        );
        // A root that is a Copilot session: nothing to wake, whatever id its record holds.
        w.agent_record(
            ID,
            json!({"name": "a", "pane": "%9", "parentPane": "%1", "client": "claude",
            "sessionId": CHILD, "parentAgent": {"client": "copilot", "pid": 1, "start": 1, "sessionId": ROOT}}),
        );
        w.ctx.env =
            crate::ctx::Env::from_pairs([("TMUX", tmux.clone()), ("TMUX_PANE", "%9".into())]);
        let sent = run(&w, send, &["send", "parent", "-Body", "done"]);
        assert_eq!(field(&sent, "toSessionId"), Value::Null, "{sent}");
        // A session id that is not one is never answered.
        w.agent_record(
            ID,
            json!({"name": "a", "pane": "%9", "parentPane": "%1", "client": "claude",
            "sessionId": "x; rm -rf /", "parentAgent": {"client": "claude", "sessionId": "nope"}}),
        );
        let sent = run(&w, send, &["send", "parent", "-Body", "done"]);
        assert_eq!(field(&sent, "toSessionId"), Value::Null, "{sent}");
        w.ctx.env = crate::ctx::Env::from_pairs([("TMUX", tmux), ("TMUX_PANE", "%1".into())]);
        let sent = run(&w, send, &["send", "a", "-Body", "hi"]);
        assert_eq!(field(&sent, "toSessionId"), Value::Null, "{sent}");
    }

    #[test]
    fn send_and_status_clear_every_waiting_file() {
        let w = waiting_world();
        for pid in [4242, 77] {
            write(
                &w.folder(ID).join(format!("waiting-{pid}.json")),
                json!({"pid": pid, "start": 1, "since": SINCE}),
            );
        }
        w.file("b.md", b"done");
        assert!(run(&w, send, &["send", "parent", "-BodyFile", "b.md"]).starts_with("ok "));
        assert_eq!(waiting_names(&w), Vec::<String>::new());
        other_wait(&w);
        let status = crate::agents::view::status;
        assert!(run(&w, status, &["status", "-BodyFile", "b.md"]).starts_with("ok "));
        assert_eq!(waiting_names(&w), Vec::<String>::new());
    }

    // ---- #48: agents found by their pane and exact launch, never by their tags

    // rmux rotated the window: the root %1 carries A's tag, A's pane %9 carries B's, B's pane %8 none. The caller is
    // `pane` (tagged `tag`); every pane answers live in session $3.
    fn rotated(tag: &str, pane: &str) -> World {
        let mut w = World::human(tag);
        let tmux = w.ctx.env.get("TMUX").unwrap().to_string();
        w.ctx.env = crate::ctx::Env::from_pairs([("TMUX", tmux), ("TMUX_PANE", pane.to_string())]);
        w.agent_record(ID, json!({"name": "a", "pane": "%9", "parentPane": "%1"}));
        w.agent_record(
            "bbbbbbbbbbbb",
            json!({"name": "b", "pane": "%8", "parentPane": "%1"}),
        );
        w.panes(&[
            "%1|@1|100|aaaaaaaaaaaa|a",
            "%9|@5|300|bbbbbbbbbbbb|b",
            "%8|@5|301||",
            "%2|@1|102||",
        ]);
        w.recipients_live();
        w
    }

    #[test]
    fn rotated_tags_never_redirect_a_verb() {
        // From a clean pane, every verb reaches A at %9 and A's mailbox.
        let mut w = rotated("", "%2");
        w.fakes.mux_rule("#{window_active}", 0, "%9|1|1|1\n", "");
        w.fakes.mux_rule("show-buffer", 0, "a's screen\n", "");
        let a = cf::member(
            &w,
            ID,
            json!({"name":"a","startCommand":launch_command("%9")}),
        );
        let b = cf::member(
            &w,
            "bbbbbbbbbbbb",
            json!({"name":"b","pane":"%8","startCommand":launch_command("%8")}),
        );
        cf::online(&w);
        cf::listing(&w, a["team"].as_str().unwrap(), &[]);
        cf::get(&w, "taaaaaaaaaaaa", cf::wire(&a));
        cf::get(&w, "tbbbbbbbbbbbb", cf::wire(&b));
        let list = crate::agents::view::list(&w.ctx, &w.request(&["list"])).unwrap();
        w.fakes.hc_done().unwrap();
        let rows: Vec<String> = list["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                format!(
                    "{}@{}",
                    r["name"].as_str().unwrap(),
                    r["pane"].as_str().unwrap()
                )
            })
            .collect();
        assert_eq!(rows, ["a@%9", "b@%8"]);
        cf::member(
            &w,
            ID,
            json!({"name":"a","startCommand":launch_command("%9")}),
        );
        assert_eq!(
            run(&w, read, &["read", "a"]),
            r#"ok {"name":"a","pane":"%9","text":"a's screen"}"#
        );
        cf::online(&w);
        assert_eq!(
            run(&w, focus, &["focus", "a"]),
            r#"ok {"name":"a","pane":"%9","window":"@5"}"#
        );
        let sent = run(&w, send, &["send", "a", "-Body", "hi"]);
        assert!(sent.starts_with(r#"ok {"to":"a","pane":"%9""#), "{sent}");
        assert_eq!(read_mail_of(&w, ID), ["hi"]);
        assert!(read_mail_of(&w, "bbbbbbbbbbbb").is_empty());
        let r = cf::member(
            &w,
            ID,
            json!({"name":"a","startCommand":launch_command("%9")}),
        );
        cf::online(&w);
        cf::answer(&w, &["kill", "taaaaaaaaaaaa", "--json"], "{}");
        let mut t = cf::wire(&r);
        t["state"] = json!("finished");
        t["reason"] = json!("killed");
        cf::get(&w, "taaaaaaaaaaaa", t);
        assert_eq!(
            run(&w, stop, &["stop", "a"]),
            r#"ok {"name":"a","hcAddress":"taaaaaaaaaaaa","life":"ended"}"#
        );
        assert!(pane_kills(&w).is_empty());
        w.fakes.hc_done().unwrap();

        // #48 test 2: then B, tagless, is still addressable, and its own pane is B.
        w.panes(&["%1|@1|100|aaaaaaaaaaaa|a", "%8|@5|301||", "%2|@1|102||"]);
        let sent = run(&w, send, &["send", "b", "-Body", "to b"]);
        assert!(sent.starts_with(r#"ok {"to":"b","pane":"%8""#), "{sent}");
        assert_eq!(read_mail_of(&w, "bbbbbbbbbbbb"), ["to b"]);
        let tmux = w.ctx.env.get("TMUX").unwrap().to_string();
        w.ctx.env = crate::ctx::Env::from_pairs([("TMUX", tmux), ("TMUX_PANE", "%8".into())]);
        let caller = w.caller();
        let s = registry::Snapshot::take(&w.ctx, &caller).unwrap();
        assert!(matches!(s.own(&caller), registry::Own::Agent(ref p) if p.id == "bbbbbbbbbbbb"));
        // B's parent %1 carries A's tag: its mailbox is still pane-1.
        let sent = run(&w, send, &["send", "parent", "-Body", "done"]);
        assert!(
            sent.starts_with(r#"ok {"to":"parent","pane":"%1""#),
            "{sent}"
        );
        assert_eq!(read_mail_of(&w, "pane-1").last().unwrap(), "done");
    }

    #[test]
    fn a_rotated_tag_on_the_root_is_no_identity() {
        // The root %1 carries A's tag: it is no agent, so it neither sends as A nor spawns, and list reads pane-1.
        let w = rotated(ID, "%1");
        assert_eq!(
            run(&w, send, &["send", "a", "-Body", "x"]),
            "2 parent this agent pane has no record"
        );
        assert_eq!(
            run(
                &w,
                crate::agents::spawn::spawn,
                &["spawn", "-Client", "claude", "-Task", "hi"]
            ),
            "2 tagged an agent pane cannot spawn agents"
        );
        let caller = w.caller();
        add_mail(&w.ctx, &caller, "pane-1", "a", "%9", "for root", None).unwrap();
        for _ in 0..2 {
            add_mail(&w.ctx, &caller, ID, "a", "%9", "for a", None).unwrap();
        }
        let a = cf::member(
            &w,
            ID,
            json!({"name":"a","startCommand":launch_command("%9")}),
        );
        let b = cf::member(
            &w,
            "bbbbbbbbbbbb",
            json!({"name":"b","pane":"%8","startCommand":launch_command("%8")}),
        );
        cf::online(&w);
        cf::listing(&w, a["team"].as_str().unwrap(), &[]);
        cf::get(&w, "taaaaaaaaaaaa", cf::wire(&a));
        cf::get(&w, "tbbbbbbbbbbbb", cf::wire(&b));
        let list = crate::agents::view::list(&w.ctx, &w.request(&["list"])).unwrap();
        w.fakes.hc_done().unwrap();
        assert_eq!(list["agents"][0]["name"], "a");
        assert_eq!(list["agents"][0]["unread"], 1);
        assert!(read_mail_of(&w, ID).len() == 2);
    }

    #[test]
    fn read_and_focus_never_act_on_a_replaced_pane() {
        let w = World::human("");
        cf::member(&w, ID, json!({"startCommand":launch_command("%9")}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        let live = registry::Launch {
            command: launch_command("%9"),
            pid: None,
        }
        .live();
        // When the server checks, %9 runs `sh` (replaced), and it is restored right after: the condition never holds.
        let acted = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = acted.clone();
        let sh = registry::Launch {
            command: "sh".into(),
            pid: None,
        }
        .live();
        w.fakes.mux_script("if-shell", move |argv| {
            assert_eq!(argv[5], live);
            if argv[5] == sh {
                seen.set(true);
            }
            Ok(Output::default())
        });
        let captured = acted.clone();
        w.fakes.mux_script("show-buffer", move |_| {
            Ok(Output {
                code: if captured.get() { 0 } else { 1 },
                stdout: if captured.get() {
                    "text\n".into()
                } else {
                    String::new()
                },
                stderr: "no buffer".into(),
            })
        });
        // Read back after the select: restored, so live and its launch again, but never selected.
        w.fakes.mux_rule("#{window_active}", 0, "%9|0|0|1\n", "");
        w.fakes.clear_log();
        assert_eq!(
            run(&w, read, &["read", "worker"]),
            "2 target the agent's pane is no longer its launch"
        );
        assert_eq!(mux_verbs(&w).last().unwrap(), "delete-buffer");
        cf::online(&w);
        assert_eq!(
            run(&w, focus, &["focus", "worker"]),
            "2 target the agent's pane is no longer its launch"
        );
        assert!(!acted.get());
    }

    // A call that fails as a call: no server, a timeout.
    fn lost_server() -> Refusal {
        Refusal {
            code: 1,
            check: "endpoint",
            message: "rmux endpoint did not answer".into(),
            reason: None,
        }
    }

    #[test]
    fn read_deletes_its_buffer_whatever_call_fails() {
        // D9: the conditional capture or show-buffer fails as a call: `2 target`, and delete-buffer still runs.
        for failing in ["if-shell", "show-buffer"] {
            let w = World::human("");
            cf::member(&w, ID, json!({"startCommand":launch_command("%9")}));
            w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
            w.fakes.mux_rule("show-buffer", 0, "text\n", "");
            w.fakes.mux_script(failing, |_| Err(lost_server()));
            w.fakes.clear_log();
            assert_eq!(
                run(&w, read, &["read", "worker"]),
                "2 target the agent's pane is no longer its launch",
                "{failing}"
            );
            let calls = w.fakes.mux_calls();
            let last = calls.last().unwrap();
            assert_eq!(last[1], "delete-buffer", "{failing} {calls:?}");
            assert!(last[3].starts_with("mux-agents-"), "{failing}");
        }
    }

    fn poll(w: &World, terminals: &[Value]) {
        cf::online(w);
        cf::listing(w, &super::super::membership::team(&w.caller()), &[]);
        for t in terminals {
            cf::get(w, t["address"].as_str().unwrap(), t.clone());
        }
    }
    #[test]
    fn mailbox_read_reconciles_one_hc_snapshot_without_pane_authority() {
        for mode in ["running", "exited", "killed", "lost", "unknown"] {
            let w = World::human("");
            let r = cf::member(&w, ID, json!({}));
            write(&w.folder(ID).join("launcher.json"), json!({"exitCode":99}));
            let mut t = cf::wire(&r);
            match mode {
                "exited" | "killed" => {
                    t["state"] = json!("finished");
                    t["reason"] = json!(mode);
                    t["exit_code"] = json!(7);
                }
                "lost" => t["argv"] = json!(["/bin/sh"]),
                "unknown" => t["labels"]["mux.gen"] = json!("1"),
                _ => {}
            }
            w.panes(&[]);
            let notice = w.folder(ID).join("notice.json");
            for round in 0..2 {
                poll(&w, std::slice::from_ref(&t));
                w.fakes.clear_log();
                let result = read(&w.ctx, &w.request(&["read"])).unwrap();
                let messages = result["messages"].as_array().unwrap();
                let ended = ["exited", "killed"].contains(&mode);
                assert_eq!(
                    messages.len(),
                    usize::from(ended && round == 0),
                    "{mode}/{round}"
                );
                if let Some(message) = messages.first() {
                    assert_eq!(
                        message["text"],
                        if mode == "exited" {
                            "[mux-agent from worker (%9) #exit-aaaaaaaaaaaa]\nworker exited with code 7."
                        } else {
                            "[mux-agent from worker (%9) #exit-aaaaaaaaaaaa]\nworker was stopped."
                        }
                    );
                    let n = read_json(&notice).unwrap().unwrap();
                    assert_eq!(
                        n["kind"],
                        if mode == "exited" {
                            "exited"
                        } else {
                            "stopped"
                        }
                    );
                    assert_eq!(
                        n["exitCode"],
                        if mode == "exited" {
                            json!(7)
                        } else {
                            Value::Null
                        }
                    );
                }
                assert_eq!(notice.exists(), ended, "{mode}");
                assert_eq!(mux_verbs(&w), ["display-message", "list-panes"], "{mode}");
                assert_eq!(
                    w.fakes
                        .hc_calls()
                        .iter()
                        .map(|(_, a, _)| a[0].clone())
                        .collect::<Vec<_>>(),
                    ["--version", "ls", "get"]
                );
                w.fakes.hc_done().unwrap();
            }
        }
    }
    #[test]
    fn mailbox_wait_refreshes_once_per_poll_and_reads_terminal_notice() {
        let w = World::human("");
        let r = cf::member(&w, ID, json!({}));
        w.panes(&[]);
        let live = cf::wire(&r);
        let mut ended = live.clone();
        ended["state"] = json!("finished");
        ended["reason"] = json!("exited");
        ended["exit_code"] = json!(7);
        poll(&w, &[live]);
        poll(&w, &[ended]);
        w.fakes.clear_log();
        let result = read(&w.ctx, &w.request(&["read", "-Wait", "3"])).unwrap();
        assert_eq!(result["messages"].as_array().unwrap().len(), 1);
        assert_eq!(
            result["messages"][0]["text"],
            "[mux-agent from worker (%9) #exit-aaaaaaaaaaaa]\nworker exited with code 7."
        );
        assert_eq!(w.fakes.slept(), [500]);
        assert_eq!(
            w.fakes
                .mux_calls()
                .iter()
                .filter(|a| a[1] == "list-panes")
                .count(),
            2
        );
        assert_eq!(w.fakes.hc_calls().len(), 6);
        w.fakes.hc_done().unwrap();
    }
    #[test]
    fn mailbox_wait_drops_marker_on_reconciliation_errors() {
        for mode in ["corrupt-record", "notice-lock"] {
            let w = waiting_world();
            let r = cf::member(
                &w,
                ID,
                json!({"pane":"%1","window":"@1","parentPane":"%4","startCommand":launch_command("%1")}),
            );
            let folder = w.folder(ID);
            let waiting = folder.join("waiting-4242.json");
            if mode == "corrupt-record" {
                poll(&w, &[cf::wire(&r)]);
                w.fakes.sleep_probe(move |_| {
                    assert!(waiting.is_file());
                    std::fs::write(folder.join("record.json"), "{").unwrap();
                });
            } else {
                let child = cf::member(&w, "bbbbbbbbbbbb", json!({"name":"child","pane":"%9"}));
                let mut ended = cf::wire(&child);
                ended["state"] = json!("finished");
                ended["reason"] = json!("exited");
                poll(&w, &[cf::wire(&r), ended]);
                w.fakes.lock_timeout("notice|");
            }
            let e = read(&w.ctx, &w.request(&["read", "-Wait", "1"])).unwrap_err();
            assert_eq!(
                (e.code, e.check),
                if mode == "corrupt-record" {
                    (1, "internal")
                } else {
                    (2, "lock")
                }
            );
            assert!(waiting_names(&w).is_empty());
            w.fakes.hc_done().unwrap();
        }
    }
    #[test]
    fn mailbox_root_reads_existing_mail_during_core_failure() {
        for mode in ["missing", "ls-error", "get-error"] {
            let w = World::human("");
            let r = cf::member(&w, ID, json!({}));
            w.panes(&[]);
            let c = w.caller();
            add_mail(&w.ctx, &c, "pane-1", "parent", "%2", "keep", None).unwrap();
            if mode != "missing" {
                cf::online(&w);
                if mode == "ls-error" {
                    w.fakes.hc_expect(
                        &[
                            "ls",
                            "--all",
                            "--label",
                            "mux.managed=1",
                            "--label",
                            &format!("mux.team={}", super::super::membership::team(&c)),
                            "--json",
                        ],
                        Err(Refusal::internal("ls failed")),
                    );
                } else {
                    cf::listing(&w, &super::super::membership::team(&c), &[]);
                    w.fakes.hc_expect(
                        &["get", r["hcAddress"].as_str().unwrap(), "--json"],
                        Err(Refusal::internal("get failed")),
                    );
                }
            }
            let reply = read(&w.ctx, &w.request(&["read"])).unwrap();
            assert_eq!(reply["messages"].as_array().unwrap().len(), 1);
            assert!(
                reply["messages"][0]["text"]
                    .as_str()
                    .unwrap()
                    .ends_with("\nkeep")
            );
            assert!(!w.folder(ID).join("notice.json").exists());
            w.fakes.hc_done().unwrap();
        }
    }
}
