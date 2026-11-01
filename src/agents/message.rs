//! send, read and stop (MuxAgents.ps1:1298-1333, 1354-1421).

use std::collections::{HashSet, VecDeque};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value, json};

use super::mail::{add_mail, doing_line, message_text, read_mail};
use super::registry::{self, Notice, add_notice, reap, self_of};
use super::view::first;
use super::{FILES_ONLY, lock, lock_key, pane_id, server_dir, text, usage};
use crate::caller::{Kind, resolve};
use crate::cli::Request;
use crate::ctx::Ctx;
use crate::error::{Refusal, Res, refuse};
use crate::mux::native;
use crate::proc::seam::hold;
use crate::proc::{Left, Reason, Row};
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
    let me = self_of(&caller, &snapshot)?;
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
        let pane = registry::find_reaped(ctx, &caller, first(request))?;
        let tail = capture(ctx, &pane, lines)?;
        reply.insert("name".into(), pane.name.into());
        reply.insert("pane".into(), pane.pane.into());
        reply.insert("text".into(), tail.into());
        return Ok(reply);
    }
    // One snapshot names the reader and serves the first reap.
    let snapshot = registry::Snapshot::take(ctx, &caller)?;
    let me = self_of(&caller, &snapshot)?;
    // Removed on every return, mail, timeout or error, when this guard drops.
    let _waiting = match &me.record {
        Some(_) if wait > 0 => Waiting::start(ctx, &caller, &me.key),
        _ => None,
    };
    let deadline = ctx.clock.now() + Duration::from_secs(wait as u64);
    let mut first_pass = Some(snapshot);
    let messages = loop {
        reap(ctx, &caller, first_pass.take().as_ref(), false)?;
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
fn capture(ctx: &Ctx, pane: &registry::Pane, lines: i32) -> Res<String> {
    let buffer = format!("mux-agents-{}", crate::util::guid()?.replace('-', ""));
    let p = pane.pane.as_str();
    let command = format!("capture-pane -J -t {p} -b {buffer} -S -{lines}");
    let argv = registry::bound(p, pane.launch.live(), command);
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let captured =
        native(ctx, &argv, true).and_then(|_| native(ctx, &["show-buffer", "-b", &buffer], true));
    // Always, whatever happened: a buffer left behind would hold the pane's text.
    let _ = native(ctx, &["delete-buffer", "-b", &buffer], true);
    let captured = captured?;
    if captured.code != 0 {
        return refuse(2, "target", "the agent's pane is no longer its launch");
    }
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
const TREE_MAX: usize = 256;
const NOT_ITS_LAUNCH: &str =
    "the agent's pane is no longer its own launch, so nothing was closed or signalled";

/// Closes a live agent's pane, then stops any survivor of the pane's process tree (same pid and start only). Its
/// parent or a human may stop it. Posts the `stopped` notice and returns the resume command. Runs a reap first. A
/// name with no live agent but an ended one (the newest, any of them) answers `already:<state>` and closes nothing;
/// so does a live one found ended under its notice lock, before any kill or signal: a notice already posted, or its
/// pane gone (then noticed here, as a reap would).
pub fn stop(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let caller = resolve(ctx)?;
    let name = first(request);
    let none = || refuse(2, "target", "no live agent has that name");
    if !super::agent_name(name) {
        return none();
    }
    let snapshot = registry::live_after_reap(ctx, &caller)?;
    let live = snapshot.running();
    let Some(pane) = live.iter().find(|p| p.name == name).cloned() else {
        return match super::board::newest_ended(ctx, &caller, &live, name)? {
            Some((_, record, state)) => {
                parent_or_human(&caller, &record)?;
                let pane = text(record.get("pane"));
                Ok(already(&record, pane, &state))
            }
            None => none(),
        };
    };
    let record = pane.record.clone();
    parent_or_human(&caller, &record)?;
    let folder = server_dir(ctx, &caller).join("agents").join(&pane.id);
    let launcher = folder.join("launcher.json");
    let marker = folder.join("notice.json");
    let left = lock(ctx, &lock_key("notice", &caller, &pane.id), || {
        // Ended since it was resolved: a notice is there, or the pane is gone and gets its one notice now. Checked
        // before the pid is read: tmux 3.8 lists a dead pane with none.
        registry::post_if_gone(ctx, &caller, &pane.id, &record)?;
        if let Some(notice) = read_json(&marker)? {
            return Ok(Err(text(notice.get("kind"))));
        }
        // A pane pid that is not a number would walk from pid 0, whose "children" are unrelated processes.
        let root: u32 = pane
            .pid
            .parse()
            .map_err(|_| Refusal::internal("the pane pid is not a number"))?;
        // A 0.2.x record (no startCommand) is its launch only while its launcher is proven: checked again here,
        // before any kill.
        if let Some(pid) = pane.launch.pid
            && (pid != root || !registry::proven_launcher(ctx, &caller, &pane.id, pid))
        {
            return refuse(1, "target", NOT_ITS_LAUNCH);
        }
        let table = ctx.os.table()?;
        // The pane's process must be the agent's own launcher: launcher.json's pid and start, the table's start, and
        // the start of the process the pid opens now, all one. A launcher that never recorded itself, or a pane pid
        // reused since (in the table or at the open), is never walked: only the pane is closed, and every tree row is
        // left unverified.
        let recorded = read_json(&launcher)
            .ok()
            .flatten()
            .and_then(|l| Some((l.get("pid")?.as_u64()?, l.get("start")?.as_i64()?)));
        let listed = table
            .iter()
            .find(|row| row.pid == root)
            .and_then(|row| row.start);
        let launcher_root = match (recorded, listed) {
            (Some((pid, start)), Some(listed))
                if pid == u64::from(root) && ctx.os.same_start(start, listed) =>
            {
                ctx.os.open(root).filter(|opened| {
                    ctx.os.same_start(opened.start, start)
                        && ctx.os.same_start(opened.start, listed)
                })
            }
            _ => None,
        };
        let tree = tree(&table, root);
        // Every tree row that is not killed is left with its reason: unverified, not opened, another start, or a
        // failed kill.
        let mut left = Vec::new();
        let mut held = Vec::new();
        match launcher_root {
            None => left.extend(tree.iter().map(|row| Left {
                pid: row.pid,
                reason: Reason::Unverified,
            })),
            Some(opened) => {
                held.push(opened);
                for row in tree.iter().filter(|row| row.pid != root) {
                    match hold(ctx.os.open(row.pid), row.start, |a, b| {
                        ctx.os.same_start(a, b)
                    }) {
                        Ok(opened) => held.push(opened),
                        Err(reason) => left.push(Left {
                            pid: row.pid,
                            reason,
                        }),
                    }
                }
            }
        }
        // The server kills the pane only while it is still this live launch, in the same command; a fresh probe then
        // decides. Not proven gone (replaced in between, or the probe failed): nothing is signalled, nothing noticed.
        let kill = registry::bound(
            &pane.pane,
            pane.launch.live(),
            format!("kill-pane -t {}", pane.pane),
        );
        let kill: Vec<&str> = kill.iter().map(String::as_str).collect();
        let _ = native(ctx, &kill, true);
        if !registry::pane_gone(ctx, &pane.pane).unwrap_or(false) {
            drop(held);
            return refuse(1, "target", NOT_ITS_LAUNCH);
        }
        for opened in held {
            let pid = opened.pid;
            if !ctx.os.kill(opened) {
                left.push(Left {
                    pid,
                    reason: Reason::Kill,
                });
            }
        }
        add_notice(ctx, &caller, &pane.id, &record, Notice::Stopped)?;
        Ok(Ok(left))
    })?;
    let left = match left {
        Ok(left) => left,
        Err(state) => return Ok(already(&record, pane.pane, &state)),
    };
    let mut reply = Map::new();
    reply.insert(
        "name".into(),
        record.get("name").cloned().unwrap_or(Value::Null),
    );
    reply.insert("pane".into(), pane.pane.into());
    reply.extend(ctx.os.stop_report(&left));
    reply.insert(
        "directory".into(),
        record.get("directory").cloned().unwrap_or(Value::Null),
    );
    reply.insert("resume".into(), resume(&record).into());
    Ok(reply)
}

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

/// `<client> --resume <sessionId>`.
fn resume(record: &Value) -> String {
    let client = if text(record.get("client")).eq_ignore_ascii_case("claude") {
        "claude"
    } else {
        "copilot"
    };
    format!("{client} --resume {}", text(record.get("sessionId")))
}

/// The reply for an agent that had already ended: nothing closed, signalled or noticed.
fn already(record: &Value, pane: String, state: &str) -> Map<String, Value> {
    let mut reply = Map::new();
    reply.insert(
        "name".into(),
        record.get("name").cloned().unwrap_or(Value::Null),
    );
    reply.insert("pane".into(), pane.into());
    reply.insert("already".into(), state.into());
    reply.insert(
        "directory".into(),
        record.get("directory").cloned().unwrap_or(Value::Null),
    );
    reply.insert("resume".into(), resume(record).into());
    reply
}

/// The pane's process and its descendants, breadth first, at most 256. A child that started before its parent is
/// a reused pid, not a child; a row without a start cannot show that, so the walk passes through it. Each pid is
/// walked once, so rows naming each other as parent end.
fn tree(table: &[Row], root: u32) -> Vec<&Row> {
    let mut tree = Vec::new();
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([root]);
    while let Some(current) = queue.pop_front() {
        if tree.len() >= TREE_MAX {
            break;
        }
        if !seen.insert(current) {
            continue;
        }
        let parent = table.iter().find(|row| row.pid == current);
        let parent_start = parent.and_then(|row| row.start);
        tree.extend(parent);
        for row in table {
            let reused =
                matches!((row.start, parent_start), (Some(child), Some(parent)) if child < parent);
            if row.ppid == current && row.pid != current && !reused {
                queue.push_back(row.pid);
            }
        }
    }
    tree
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::fixture::{CLAUDE, ID, SESSION_ID, World, launch_command, write};
    use crate::agents::lock_key;
    use crate::agents::view::focus;
    use crate::mux::Output;
    use crate::proc::Os;
    use crate::testing;
    use serde_json::json;

    type Handler = fn(&Ctx, &Request) -> Res<Map<String, Value>>;

    const PROBE: &str = "#{pane_id}|#{session_id}";
    const NONE: &str = "2 target no live agent has that name";

    fn run(w: &World, handler: Handler, args: &[&str]) -> String {
        match handler(&w.ctx, &w.request(args)) {
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
                assert_eq!(run_owned(&w, handler, &args), NONE, "{args:?}");
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
                assert_eq!(run_owned(&w, handler, &args), NONE, "{args:?}");
                assert_eq!(mux_verbs(&w), ["display-message", "list-panes"], "{args:?}");
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
            assert_eq!(run_owned(&w, handler, &args), NONE, "{args:?}");
        }
        // Committed without a record: stop has nothing to stop or resume, and closes nothing.
        let w = World::human("");
        let folder = w.folder(ID);
        std::fs::create_dir_all(&folder).unwrap();
        write(&folder.join("commit.json"), json!({}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        w.fakes.clear_log();
        assert_eq!(run(&w, stop, &["stop", "worker"]), NONE);
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
    fn named_read_reaps_before_the_name() {
        let w = gone_world();
        w.fakes.clear_log();
        let reply = run(&w, read, &["read", "worker", "-Lines", "5"]);
        assert!(reply.starts_with("ok "), "{reply}");
        // The reap posted a notice, so the panes are listed again before the name is looked up.
        assert_eq!(
            mux_verbs(&w),
            [
                "display-message",
                "list-panes",
                "display-message",
                "list-panes",
                "if-shell",
                "show-buffer",
                "delete-buffer"
            ]
        );
        let notice = w.folder("bbbbbbbbbbbb").join("notice.json");
        assert!(notice.is_file());
        assert_eq!(
            read_mail_of(&w, "pane-1"),
            ["gone is gone: its pane closed without an exit record."]
        );
        // A lost agent's name answers `target`.
        assert_eq!(run(&w, read, &["read", "gone"]), NONE);
    }

    #[test]
    fn read_lines_1_2000() {
        let usage =
            "2 usage usage: mux-agent read [-Wait <seconds>] | read <name> [-Lines <n>] [-Text]";
        let w = World::human("");
        w.agent_record(ID, json!({}));
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
        let reply = run(&w, read, &["read", "-Wait", "600"]);
        assert!(reply.contains(r#"\nlate""#), "{reply}");
        assert_eq!(w.fakes.slept(), [500, 500]);
    }

    // The worker in pane %9 (pid 300) of a caller that is an agent (or a human); its tree is up to each test.
    fn stop_world(agent: bool, fields: Value) -> World {
        let w = if agent {
            World::agent(ID)
        } else {
            World::human("")
        };
        let folder = w.agent_record("bbbbbbbbbbbb", fields);
        // The launcher in pane %9 recorded its own pid and start.
        write(
            &folder.join("launcher.json"),
            json!({"pid": 300, "start": 3000, "startedAt": "x"}),
        );
        w.panes(&["%9|@5|300|bbbbbbbbbbbb|worker"]);
        emulate_pane9(&w, &launch_command("%9"));
        w
    }

    // The multiplexer's pane %9: live, running `command` (shared, so a test can replace it); a conditional kill takes
    // it only when its condition is %9's live launch as it is now, and the gone probe then answers empty fields.
    fn emulate_pane9(w: &World, command: &str) -> std::rc::Rc<std::cell::RefCell<Option<String>>> {
        let pane = std::rc::Rc::new(std::cell::RefCell::new(Some(command.to_string())));
        let state = pane.clone();
        w.fakes.mux_script("if-shell", move |argv| {
            let mut held = state.borrow_mut();
            let holds = held.as_ref().is_some_and(|command| {
                let launch = registry::Launch {
                    command: command.clone(),
                    pid: None,
                };
                argv[5] == launch.live()
            });
            if holds && argv[6] == "kill-pane -t %9" {
                *held = None;
            }
            Ok(crate::mux::Output::default())
        });
        let state = pane.clone();
        w.fakes
            .mux_script("-t %9 #{pane_id}|#{@agent-id}", move |_| {
                let stdout = if state.borrow().is_some() {
                    "%9|bbbbbbbbbbbb\n"
                } else {
                    "|\n"
                };
                Ok(crate::mux::Output {
                    code: 0,
                    stdout: stdout.into(),
                    stderr: String::new(),
                })
            });
        pane
    }

    // The conditional kill of %9 as the multiplexer receives it: only while it is still its live launch.
    fn kill9() -> Vec<String> {
        let launch = registry::Launch {
            command: launch_command("%9"),
            pid: None,
        };
        let mut argv = vec!["-N".to_string()];
        argv.extend(registry::bound(
            "%9",
            launch.live(),
            "kill-pane -t %9".into(),
        ));
        argv
    }

    // Every kill of a pane sent to the multiplexer.
    fn pane_kills(w: &World) -> Vec<Vec<String>> {
        w.fakes
            .mux_calls()
            .into_iter()
            .filter(|c| c[1] == "kill-pane" || c.last().is_some_and(|a| a.starts_with("kill-pane")))
            .collect()
    }

    // The table, open and kill calls of the stop walk; resolve's opens of the server and the caller's agent are
    // left out.
    fn proc_calls(w: &World) -> Vec<String> {
        w.fakes
            .os_calls()
            .into_iter()
            .filter(|c| {
                c == "table"
                    || ((c.starts_with("open ") || c.starts_with("kill "))
                        && c != "open 900"
                        && c != &format!("open {CLAUDE}"))
            })
            .collect()
    }

    fn left_report(left: &[Left]) -> Map<String, Value> {
        let mut report = Map::new();
        let rows: Vec<Value> = left
            .iter()
            .map(|l| format!("{} {:?}", l.pid, l.reason).to_lowercase().into())
            .collect();
        report.insert("left".into(), Value::Array(rows));
        report
    }

    #[test]
    fn stop_tree_bfs_256() {
        let w = stop_world(false, json!({}));
        for (pid, ppid, start) in [
            (300, 900, 3000),
            (310, 300, 3100),
            (320, 300, 3200),
            (311, 310, 3110),
            (321, 320, 3210),
            // Started before its parent: a reused pid, not a child.
            (330, 300, 2999),
            (340, 999, 3400),
        ] {
            w.fakes.process(testing::row(pid, ppid, start));
        }
        for pid in [300, 310, 320, 311, 321] {
            w.fakes.kill_ok(pid);
        }
        w.fakes.stop_report_with(left_report);
        let caller = w.caller();
        w.fakes.clear_log();
        assert_eq!(
            run(&w, stop, &["stop", "worker"]),
            format!(
                r#"ok {{"name":"worker","pane":"%9","left":[],"directory":"/work","resume":"claude --resume {SESSION_ID}"}}"#
            )
        );
        // Breadth first; every open before the pane closes, then the kills.
        assert_eq!(
            proc_calls(&w),
            [
                "table", "open 300", "open 310", "open 320", "open 311", "open 321", "kill 300",
                "kill 310", "kill 320", "kill 311", "kill 321"
            ]
        );
        // OS and multiplexer calls in one order: the last open, then kill-pane, then the first kill.
        let events = w.fakes.events();
        let at = |event: &str| events.iter().position(|e| e == event).expect(event);
        let kill_event = format!("mux {}", kill9().join(" "));
        assert!(at("open 321") < at(&kill_event), "{events:?}");
        assert!(at(&kill_event) < at("kill 300"), "{events:?}");
        let kill_pane: Vec<_> = w
            .fakes
            .mux_calls()
            .into_iter()
            .filter(|c| c.last().is_some_and(|a| a.starts_with("kill-pane")))
            .collect();
        assert_eq!(kill_pane, [kill9()]);
        let notice = lock_key("notice", &caller, "bbbbbbbbbbbb");
        let mail = lock_key("mail", &caller, "pane-1");
        assert_eq!(
            w.fakes.locks(),
            [
                format!("+{notice}"),
                format!("+{mail}"),
                format!("-{mail}"),
                format!("-{notice}")
            ]
        );
        assert_eq!(read_mail_of(&w, "pane-1"), ["worker was stopped."]);

        // At most 256 rows: a chain of 300 below the pane stops after 256 opens.
        let w = stop_world(false, json!({}));
        w.fakes.process(testing::row(300, 900, 3000));
        let mut parent = 300;
        for n in 0..299u32 {
            w.fakes
                .process(testing::row(1000 + n, parent, 3001 + i64::from(n)));
            parent = 1000 + n;
        }
        w.fakes.clear_log();
        run(&w, stop, &["stop", "worker"]);
        assert_eq!(
            proc_calls(&w)
                .iter()
                .filter(|c| c.starts_with("open "))
                .count(),
            256
        );

        // Two rows that name each other as parent: each is walked once.
        let w = stop_world(false, json!({}));
        write(
            &w.folder("bbbbbbbbbbbb").join("launcher.json"),
            json!({"pid": 300, "start": 5}),
        );
        w.fakes.process(testing::row(300, 301, 5));
        w.fakes.process(testing::row(301, 300, 5));
        w.fakes.clear_log();
        run(&w, stop, &["stop", "worker"]);
        assert_eq!(proc_calls(&w)[..3], ["table", "open 300", "open 301"]);
        assert_eq!(
            proc_calls(&w)
                .iter()
                .filter(|c| c.starts_with("open "))
                .count(),
            2
        );
    }

    #[test]
    fn stop_verifies_pane_process_before_walking() {
        // A pane pid of 0 or 1 is no agent: nothing is read, walked or killed.
        for pid in [0, 1] {
            let w = stop_world(false, json!({}));
            w.panes(&[&format!("%9|@5|{pid}|bbbbbbbbbbbb|worker")]);
            w.fakes.process(testing::row(1, 0, 1));
            w.fakes.process(testing::row(300, 1, 3000));
            w.fakes.kill_ok(1);
            w.fakes.clear_log();
            assert_eq!(run(&w, stop, &["stop", "worker"]), NONE, "{pid}");
            assert!(proc_calls(&w).is_empty(), "{pid}");
            assert!(pane_kills(&w).is_empty(), "{pid}");
        }
        // The pane pid was reused (another start than launcher.json's), or launcher.json names another pid or
        // none: only the pane is closed, and its tree is left as survivors.
        for launcher in [
            Some(json!({"pid": 300, "start": 2500})),
            Some(json!({"pid": 301, "start": 3000})),
            Some(json!({"pid": 300})),
            None,
        ] {
            let w = stop_world(false, json!({}));
            let file = w.folder("bbbbbbbbbbbb").join("launcher.json");
            match &launcher {
                Some(record) => write(&file, record.clone()),
                None => std::fs::remove_file(&file).unwrap(),
            }
            w.fakes.process(testing::row(300, 900, 3000));
            w.fakes.process(testing::row(310, 300, 3100));
            w.fakes.kill_ok(300);
            w.fakes.kill_ok(310);
            w.fakes.stop_report_with(left_report);
            w.fakes.clear_log();
            let reply = run(&w, stop, &["stop", "worker"]);
            let left = r#""left":["300 unverified","310 unverified"]"#;
            assert!(reply.contains(left), "{launcher:?} {reply}");
            assert_eq!(proc_calls(&w), ["table"], "{launcher:?}");
            let kill_pane: Vec<_> = w
                .fakes
                .mux_calls()
                .into_iter()
                .filter(|c| c.last().is_some_and(|a| a.starts_with("kill-pane")))
                .collect();
            assert_eq!(kill_pane, [kill9()], "{launcher:?}");
            assert_eq!(read_mail_of(&w, "pane-1"), ["worker was stopped."]);
        }
    }

    #[test]
    fn stop_root_reused_at_open_kills_pane_only() {
        // launcher.json and the table agree on pid 300 at 3000, but the pid opened is another process (4000), or
        // none: the tree is never walked, only the pane closes, and every tree row is left unverified.
        for opened in [Some(4000), None] {
            let w = stop_world(false, json!({}));
            w.fakes.process(testing::row(300, 900, 3000));
            w.fakes.process(testing::row(310, 300, 5000));
            w.fakes.opens(300, &[opened]);
            w.fakes.kill_ok(300);
            w.fakes.kill_ok(310);
            w.fakes.stop_report_with(left_report);
            w.fakes.clear_log();
            let reply = run(&w, stop, &["stop", "worker"]);
            assert!(
                reply.contains(r#""left":["300 unverified","310 unverified"]"#),
                "{opened:?} {reply}"
            );
            assert_eq!(proc_calls(&w), ["table", "open 300"], "{opened:?}");
            let kill_pane: Vec<_> = w
                .fakes
                .mux_calls()
                .into_iter()
                .filter(|c| c.last().is_some_and(|a| a.starts_with("kill-pane")))
                .collect();
            assert_eq!(kill_pane, [kill9()], "{opened:?}");
            assert_eq!(read_mail_of(&w, "pane-1"), ["worker was stopped."]);
        }
    }

    #[test]
    fn stop_skips_reused_start() {
        let w = stop_world(false, json!({}));
        w.fakes.process(testing::row(300, 900, 3000));
        w.fakes.process(testing::row(310, 300, 3100));
        w.fakes.process(testing::row(320, 300, 3200));
        // 310 was reused between the listing and the open; 320 exited.
        w.fakes.opens(310, &[Some(3999)]);
        w.fakes.opens(320, &[None]);
        w.fakes.kill_ok(300);
        w.fakes.stop_report_with(left_report);
        w.fakes.clear_log();
        let reply = run(&w, stop, &["stop", "worker"]);
        assert!(
            reply.contains(r#""left":["310 start","320 open"]"#),
            "{reply}"
        );
        assert_eq!(
            proc_calls(&w),
            ["table", "open 300", "open 310", "open 320", "kill 300"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn stop_report_unix_has_survivor_pids() {
        let w = stop_world(false, json!({}));
        w.fakes.process(testing::row(300, 900, 3000));
        w.fakes.process(testing::row(310, 300, 3100));
        w.fakes.process(testing::row(320, 300, 3200));
        w.fakes.opens(320, &[None]);
        w.fakes.kill_ok(300);
        w.fakes.stop_report_with(crate::proc::unix::stop_report);
        assert_eq!(
            run(&w, stop, &["stop", "worker"]),
            format!(
                r#"ok {{"name":"worker","pane":"%9","survivors":2,"survivorPids":[320,310],"directory":"/work","resume":"claude --resume {SESSION_ID}"}}"#
            )
        );
    }

    #[test]
    fn stop_report_windows_survivors_only() {
        let w = stop_world(false, json!({}));
        w.fakes.process(testing::row(300, 900, 3000));
        w.fakes.process(testing::row(310, 300, 3100));
        w.fakes.process(testing::row(320, 300, 3200));
        w.fakes.opens(320, &[None]);
        w.fakes.kill_ok(300);
        w.fakes.stop_report_with(crate::proc::windows::stop_report);
        // Only the failed kill (310) counts; the row never opened (320) does not.
        assert_eq!(
            run(&w, stop, &["stop", "worker"]),
            format!(
                r#"ok {{"name":"worker","pane":"%9","survivors":1,"directory":"/work","resume":"claude --resume {SESSION_ID}"}}"#
            )
        );
        // An unverified tree (the pane pid's start is not launcher.json's): nothing was killed, so every tree row
        // is a survivor.
        let w = stop_world(false, json!({}));
        write(
            &w.folder("bbbbbbbbbbbb").join("launcher.json"),
            json!({"pid": 300, "start": 2500}),
        );
        w.fakes.process(testing::row(300, 900, 3000));
        w.fakes.process(testing::row(310, 300, 3100));
        w.fakes.kill_ok(300);
        w.fakes.kill_ok(310);
        w.fakes.stop_report_with(crate::proc::windows::stop_report);
        assert_eq!(
            run(&w, stop, &["stop", "worker"]),
            format!(
                r#"ok {{"name":"worker","pane":"%9","survivors":2,"directory":"/work","resume":"claude --resume {SESSION_ID}"}}"#
            )
        );
    }

    #[test]
    fn stop_resume_command() {
        for (client, resume) in [
            (json!("claude"), "claude"),
            (json!("Claude"), "claude"),
            (json!("copilot"), "copilot"),
            (json!("other"), "copilot"),
            (Value::Null, "copilot"),
        ] {
            let w = stop_world(false, json!({"client": client}));
            let reply = run(&w, stop, &["stop", "worker"]);
            assert!(
                reply.ends_with(&format!(r#""resume":"{resume} --resume {SESSION_ID}"}}"#)),
                "{client} {reply}"
            );
        }
    }

    #[test]
    fn stop_agent_not_parent_refuses() {
        let w = stop_world(true, json!({"parentPane": "%7"}));
        w.fakes.clear_log();
        assert_eq!(
            run(&w, stop, &["stop", "worker"]),
            "2 parent only the parent or a human can stop this agent"
        );
        assert!(proc_calls(&w).is_empty());
        assert!(pane_kills(&w).is_empty());
        assert!(!w.folder("bbbbbbbbbbbb").join("notice.json").exists());
        // Its parent (pane %1) may stop it, and so may a human, whoever the parent is.
        let w = stop_world(true, json!({"parentPane": "%1"}));
        assert!(run(&w, stop, &["stop", "worker"]).starts_with("ok "));
        let w = stop_world(false, json!({"parentPane": "%7"}));
        assert!(run(&w, stop, &["stop", "worker"]).starts_with("ok "));
    }

    #[test]
    fn stop_reaches_descendant_behind_unopenable_row() {
        // pane 300 > 310 (listed without a start) > 320.
        let w = stop_world(false, json!({}));
        w.fakes.process(testing::row(300, 900, 3000));
        w.fakes.process(testing::unknown_start(310, 300));
        w.fakes.process(testing::row(320, 310, 3200));
        // Even an open that succeeds does not make a row with no listed start trusted.
        w.fakes.opens(310, &[Some(3100)]);
        for pid in [300, 310, 320] {
            w.fakes.kill_ok(pid);
        }
        w.fakes.stop_report_with(left_report);
        w.fakes.clear_log();
        let reply = run(&w, stop, &["stop", "worker"]);
        assert!(reply.contains(r#""left":["310 open"]"#), "{reply}");
        assert_eq!(
            proc_calls(&w),
            [
                "table", "open 300", "open 310", "open 320", "kill 300", "kill 320"
            ]
        );
    }

    fn kill_panes(w: &World) -> usize {
        w.fakes
            .mux_calls()
            .iter()
            .filter(|c| c[1] == "kill-pane" || c[1] == "if-shell")
            .count()
    }

    // The parent's mailbox lines as stored, without moving the read offset.
    fn stored(w: &World) -> usize {
        let path = w.state().join("mail").join("pane-1.jsonl");
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .count()
    }

    #[test]
    fn stop_an_ended_agent_answers_already() {
        for (kind, code) in [
            ("exited", json!(3)),
            ("stopped", Value::Null),
            ("lost", Value::Null),
        ] {
            let w = World::human("");
            let folder = w.agent_record("bbbbbbbbbbbb", json!({}));
            write(
                &folder.join("notice.json"),
                json!({"kind": kind, "exitCode": code, "at": "2026-10-29T11:00:00.0000000Z"}),
            );
            w.panes(&[]);
            w.fakes.clear_log();
            let before = std::fs::read(folder.join("notice.json")).unwrap();
            assert_eq!(
                run(&w, stop, &["stop", "worker"]),
                format!(
                    r#"ok {{"name":"worker","pane":"%9","already":"{kind}","directory":"/work","resume":"claude --resume {SESSION_ID}"}}"#
                ),
                "{kind}"
            );
            // No kill, no signal, no notice.
            assert_eq!(kill_panes(&w), 0, "{kind}");
            assert!(proc_calls(&w).is_empty(), "{kind}");
            assert_eq!(std::fs::read(folder.join("notice.json")).unwrap(), before);
            assert_eq!(stored(&w), 0, "{kind}");
        }
        // Only its parent or a human, as for a live one.
        let w = World::agent(ID);
        w.uncommitted(ID, json!({"name": "me", "pane": "%1"}));
        let folder = w.agent_record("bbbbbbbbbbbb", json!({"parentPane": "%7"}));
        write(
            &folder.join("notice.json"),
            json!({"kind": "lost", "exitCode": null, "at": "x"}),
        );
        w.panes(&[]);
        assert_eq!(
            run(&w, stop, &["stop", "worker"]),
            "2 parent only the parent or a human can stop this agent"
        );
    }

    #[test]
    fn stop_picks_live_then_newest_ended() {
        // A live agent and an ended one with one name: the live one is stopped.
        let w = stop_world(false, json!({}));
        let ended = w.agent_record("cccccccccccc", json!({"pane": "%4"}));
        write(
            &ended.join("notice.json"),
            json!({"kind": "lost", "exitCode": null, "at": "2026-10-29T11:59:00.0000000Z"}),
        );
        w.fakes.process(testing::row(300, 900, 3000));
        w.fakes.kill_ok(300);
        let reply = run(&w, stop, &["stop", "worker"]);
        assert!(!reply.contains("already"), "{reply}");
        assert_eq!(read_mail_of(&w, "pane-1"), ["worker was stopped."]);

        // Two ended `worker`s beyond the 10 shown, 11 newer agents ahead of them: the newer of the two answers.
        let w = World::human("");
        for n in 0..11u32 {
            let folder = w.agent_record(
                &format!("{n:012x}"),
                json!({"name": format!("a{n}"), "pane": format!("%{}", 20 + n)}),
            );
            write(
                &folder.join("notice.json"),
                json!({"kind": "stopped", "exitCode": null, "at": format!("2026-10-29T11:{:02}:00.0000000Z", 10 + n)}),
            );
        }
        for (id, pane, kind, at) in [
            ("bbbbbbbbbbbb", "%8", "stopped", "10:00"),
            ("cccccccccccc", "%7", "exited", "10:30"),
        ] {
            let folder = w.agent_record(id, json!({"pane": pane}));
            write(
                &folder.join("notice.json"),
                json!({"kind": kind, "exitCode": 0, "at": format!("2026-10-29T{at}:00.0000000Z")}),
            );
        }
        w.panes(&[]);
        let reply = run(&w, stop, &["stop", "worker"]);
        assert!(
            reply.contains(r#""pane":"%7","already":"exited""#),
            "{reply}"
        );
        // No match at all: target, as before.
        assert_eq!(run(&w, stop, &["stop", "nobody"]), NONE);
    }

    #[test]
    fn stop_rechecks_under_the_notice_lock() {
        // A reap posts its notice while stop waits for the lock: already, no kill, no signal, no second notice.
        let w = stop_world(false, json!({}));
        w.fakes.process(testing::row(300, 900, 3000));
        w.fakes.kill_ok(300);
        let caller = w.caller();
        let notice = lock_key("notice", &caller, "bbbbbbbbbbbb");
        let marker = w.folder("bbbbbbbbbbbb").join("notice.json");
        let (raced, ctx_marker) = (std::cell::Cell::new(false), marker.clone());
        w.fakes.lock_probe(move |event| {
            if event == format!("+{notice}") && !raced.get() {
                raced.set(true);
                write(
                    &ctx_marker,
                    json!({"kind": "exited", "exitCode": 0, "at": "x"}),
                );
            }
            String::new()
        });
        w.fakes.clear_log();
        let reply = run(&w, stop, &["stop", "worker"]);
        assert!(reply.contains(r#""already":"exited""#), "{reply}");
        assert_eq!(kill_panes(&w), 0);
        assert!(proc_calls(&w).is_empty());
        assert_eq!(stored(&w), 0);

        // The pane closed (no launcher notice) after stop resolved it: stop posts the one notice itself, lost.
        let w = stop_world(false, json!({}));
        w.fakes.process(testing::row(300, 900, 3000));
        w.fakes.kill_ok(300);
        w.fakes
            .mux_first("-t %9 #{pane_id}|#{@agent-id}", 0, "|\n", "");
        w.fakes.clear_log();
        let reply = run(&w, stop, &["stop", "worker"]);
        assert!(reply.contains(r#""already":"lost""#), "{reply}");
        assert_eq!(kill_panes(&w), 0);
        assert!(proc_calls(&w).is_empty());
        assert_eq!(
            read_mail_of(&w, "pane-1"),
            ["worker is gone: its pane closed without an exit record."]
        );
        let written = crate::state::fs::read_json(&w.folder("bbbbbbbbbbbb").join("notice.json"))
            .unwrap()
            .unwrap();
        assert_eq!(written["closedBy"], "pane");
    }

    #[test]
    fn stop_a_dead_pane_without_pid_answers_already() {
        // tmux 3.8 lists a remain-on-exit dead pane with no pid. The agent ended while reap listed again: its
        // notice is there (or reap's probe posts one), so stop answers already before it ever reads the pid.
        for (noticed, state) in [(true, "exited"), (false, "lost")] {
            let w = stop_world(false, json!({}));
            let folder = w.folder("bbbbbbbbbbbb");
            if noticed {
                write(
                    &folder.join("notice.json"),
                    json!({"kind": "exited", "exitCode": 0, "at": "x"}),
                );
            }
            w.panes_dead(&["%9|@5||bbbbbbbbbbbb|worker"], &["%9"]);
            // The conditional kill ran but the pane is still listed (as a respawn or a slow server would leave it);
            // its probe answers gone.
            w.fakes
                .mux_first("-t %9 #{pane_id}|#{@agent-id}", 0, "|\n", "");
            w.fakes.clear_log();
            let reply = run(&w, stop, &["stop", "worker"]);
            assert!(
                reply.contains(&format!(r#""already":"{state}""#)),
                "{noticed} {reply}"
            );
            assert!(
                !w.fakes.mux_calls().iter().any(|c| c[1] == "kill-pane"),
                "{noticed}"
            );
            assert!(proc_calls(&w).is_empty(), "{noticed}");
            assert_eq!(stored(&w), usize::from(!noticed), "{noticed}");
        }
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
        let reply = run(&w, read, &["read", "-Wait", "600"]);
        assert!(reply.contains(r#"\ngo""#), "{reply}");
        assert_eq!(waiting_names(&w), Vec::<String>::new());

        // A failed mail read or a failed notice pass (the reap's pane list) removes it too.
        let w = waiting_world();
        w.fakes.lock_timeout("mail|");
        assert!(run(&w, read, &["read", "-Wait", "5"]).starts_with("2 lock"));
        assert_eq!(waiting_names(&w), Vec::<String>::new());
        let w = waiting_world();
        w.fakes.mux_first("list-panes", 1, "", "down");
        assert!(run(&w, read, &["read", "-Wait", "5"]).starts_with("1 "));
        assert_eq!(waiting_names(&w), Vec::<String>::new());

        // -Wait 0 never waits, so it marks nothing.
        let w = waiting_world();
        let agent = lock_key("agent", &w.caller(), ID);
        w.fakes.clear_log();
        run(&w, read, &["read"]);
        assert!(!w.fakes.locks().iter().any(|l| l.contains(&agent)));

        // A human caller writes nothing.
        let w = World::human("");
        w.state();
        w.fakes.sleep_probe(|_| {});
        run(&w, read, &["read", "-Wait", "1"]);
        let agents = w.state().join("agents");
        assert_eq!(std::fs::read_dir(agents).unwrap().count(), 0);
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
        emulate_pane9(&w, &launch_command("%9"));
        w.fakes.mux_rule("#{window_active}", 0, "%9|1|1|1\n", "");
        w.fakes.mux_rule("show-buffer", 0, "a's screen\n", "");
        let list = crate::agents::view::list(&w.ctx, &w.request(&["list"])).unwrap();
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
        assert_eq!(
            run(&w, read, &["read", "a"]),
            r#"ok {"name":"a","pane":"%9","text":"a's screen"}"#
        );
        assert_eq!(
            run(&w, focus, &["focus", "a"]),
            r#"ok {"name":"a","pane":"%9","window":"@5"}"#
        );
        let sent = run(&w, send, &["send", "a", "-Body", "hi"]);
        assert!(sent.starts_with(r#"ok {"to":"a","pane":"%9""#), "{sent}");
        assert_eq!(read_mail_of(&w, ID), ["hi"]);
        assert!(read_mail_of(&w, "bbbbbbbbbbbb").is_empty());
        let stopped = run(&w, stop, &["stop", "a"]);
        assert!(
            stopped.starts_with(r#"ok {"name":"a","pane":"%9""#),
            "{stopped}"
        );
        // Only A's own launch at %9 was killed; the root carrying A's tag never was.
        assert_eq!(pane_kills(&w), [kill9()]);

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
        let list = crate::agents::view::list(&w.ctx, &w.request(&["list"])).unwrap();
        assert_eq!(list["agents"][0]["name"], "a");
        assert_eq!(list["agents"][0]["unread"], 1);
        assert!(read_mail_of(&w, ID).len() == 2);
    }

    #[test]
    fn stop_never_kills_a_pane_replaced_after_it_was_resolved() {
        let w = stop_world(false, json!({}));
        w.fakes.process(testing::row(300, 900, 3000));
        w.fakes.kill_ok(300);
        let pane = emulate_pane9(&w, &launch_command("%9"));
        // Respawned (another command) while stop waits for the notice lock.
        let replace = pane.clone();
        let notice = lock_key("notice", &w.caller(), "bbbbbbbbbbbb");
        w.fakes.lock_probe(move |event| {
            if event == format!("+{notice}") {
                *replace.borrow_mut() = Some("sh".into());
            }
            String::new()
        });
        w.fakes.clear_log();
        assert_eq!(
            run(&w, stop, &["stop", "worker"]),
            format!("1 target {NOT_ITS_LAUNCH}")
        );
        // The conditional ran and held nothing: the replacement lives, nothing was signalled or noticed.
        assert_eq!(pane_kills(&w), [kill9()]);
        assert_eq!(pane.borrow().as_deref(), Some("sh"));
        assert!(!proc_calls(&w).iter().any(|c| c.starts_with("kill ")));
        assert!(!w.folder("bbbbbbbbbbbb").join("notice.json").exists());
        assert_eq!(stored(&w), 0);
    }

    #[test]
    fn read_and_focus_never_act_on_a_replaced_pane() {
        let w = World::human("");
        w.agent_record(ID, json!({}));
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
        assert_eq!(
            run(&w, focus, &["focus", "worker"]),
            "2 target the agent's pane is no longer its launch"
        );
        assert!(!acted.get());
    }

    // A 0.2.0-0.2.2 worker (no startCommand) at %9, its launcher pid 300 started 3000, run as `<other exe> launcher
    // <its spec>`; the multiplexer kills %9 only for a conditional naming pid 300.
    fn legacy_stop_world() -> (World, std::rc::Rc<std::cell::Cell<bool>>) {
        let w = stop_world(false, json!({}));
        w.legacy("bbbbbbbbbbbb");
        let spec = w.folder("bbbbbbbbbbbb").join("spec.json");
        w.fakes.process(Row {
            pid: 300,
            ppid: 900,
            start: Some(3000),
            path: Some("/old/mux-agent".into()),
            argv: Some(vec![
                "/old/mux-agent".into(),
                "launcher".into(),
                spec.to_str().unwrap().into(),
            ]),
        });
        w.fakes.kill_ok(300);
        let killed = std::rc::Rc::new(std::cell::Cell::new(false));
        let kill = killed.clone();
        w.fakes.mux_script("if-shell", move |argv| {
            if argv[5].contains("#{==:#{pane_pid},300}") && argv[6] == "kill-pane -t %9" {
                kill.set(true);
            }
            Ok(Output::default())
        });
        let gone = killed.clone();
        w.fakes
            .mux_script("-t %9 #{pane_id}|#{@agent-id}", move |_| {
                Ok(Output {
                    code: 0,
                    stdout: if gone.get() { "|\n" } else { "%9|\n" }.into(),
                    stderr: String::new(),
                })
            });
        (w, killed)
    }

    #[test]
    fn a_legacy_record_is_stopped_only_while_its_launcher_is_proven() {
        // Proven: the kill is bound to its command and its pid, and the launcher is signalled.
        let (w, killed) = legacy_stop_world();
        let reply = run(&w, stop, &["stop", "worker"]);
        assert!(reply.starts_with(r#"ok {"name":"worker""#), "{reply}");
        assert!(killed.get());
        assert!(proc_calls(&w).contains(&"kill 300".to_string()));
        // The pid kept but its start or its command changed between resolve and stop: refused before any kill.
        for change in ["start", "command"] {
            let (w, killed) = legacy_stop_world();
            let fakes = w.fakes.clone();
            let notice = lock_key("notice", &w.caller(), "bbbbbbbbbbbb");
            let change = change.to_string();
            w.fakes.lock_probe(move |event| {
                if event == format!("+{notice}") {
                    let mut row = fakes
                        .table()
                        .unwrap()
                        .into_iter()
                        .find(|r| r.pid == 300)
                        .unwrap();
                    if change == "start" {
                        row.start = Some(3001);
                    } else {
                        row.argv = Some(vec!["/old/mux-agent".into(), "watch".into()]);
                    }
                    fakes.forget(300);
                    fakes.process(row);
                }
                String::new()
            });
            w.fakes.clear_log();
            assert_eq!(
                run(&w, stop, &["stop", "worker"]),
                format!("1 target {NOT_ITS_LAUNCH}")
            );
            assert!(pane_kills(&w).is_empty());
            assert!(!killed.get());
            assert!(!proc_calls(&w).iter().any(|c| c.starts_with("kill ")));
            assert!(!w.folder("bbbbbbbbbbbb").join("notice.json").exists());
        }
        // A dead 0.2.x pane is never killed.
        let (w, killed) = legacy_stop_world();
        w.panes_dead(&["%9|@5||bbbbbbbbbbbb|worker"], &["%9"]);
        w.fakes.clear_log();
        assert!(!reap(&w.ctx, &w.caller(), None, false).unwrap());
        assert!(pane_kills(&w).is_empty() && !killed.get());
    }
}
