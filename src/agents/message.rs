//! send, read and stop (MuxAgents.ps1:1298-1333, 1354-1421).

use std::collections::{HashSet, VecDeque};
use std::ops::RangeInclusive;
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::mail::{add_mail, doing_line, mailbox_key, message_text, read_mail};
use super::registry::{self, Notice, add_notice, find_pane, self_of, update_notices};
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
    let me = self_of(ctx, &caller)?;
    let target = first(request);
    let pane = if target == "parent" {
        match &me.record {
            Some(record) => text(record.get("parentPane")),
            None => return refuse(2, "parent", "only an agent has a parent"),
        }
    } else if pane_id(target) {
        target.to_string()
    } else {
        find_pane(ctx, &caller, target)?.pane
    };
    let probe = native(
        ctx,
        &["display-message", "-p", "-t", &pane, RECIPIENT],
        true,
    )?;
    // `^(\$\d+)\|(.*)$` after trailing CR and LF are cut: the tag runs to the end and holds no LF.
    let line = probe.stdout.trim_end_matches(['\r', '\n']);
    let recipient = (probe.code == 0 && !line.contains('\n'))
        .then(|| line.split_once('|'))
        .flatten()
        .filter(|(session, _)| *session == caller.session && pane_session(session));
    let Some((_, tag)) = recipient else {
        return refuse(
            2,
            "target",
            "the recipient is not a live pane of this session",
        );
    };
    let key = mailbox_key(&pane, tag)?;
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

const RECIPIENT: &str = "#{session_id}|#{@agent-id}";

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
        let pane = find_pane(ctx, &caller, first(request))?;
        let start = format!("-{lines}");
        let captured = native(
            ctx,
            &["capture-pane", "-p", "-J", "-t", &pane.pane, "-S", &start],
            false,
        )?;
        let all: Vec<&str> = captured
            .stdout
            .trim_end_matches(['\r', '\n'])
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .collect();
        let tail = all[all.len().saturating_sub(lines as usize)..].join("\n");
        reply.insert("name".into(), pane.name.into());
        reply.insert("pane".into(), pane.pane.into());
        reply.insert("text".into(), tail.into());
        return Ok(reply);
    }
    let me = self_of(ctx, &caller)?;
    let deadline = ctx.clock.now() + Duration::from_secs(wait as u64);
    let messages = loop {
        update_notices(ctx, &caller, None)?;
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

const POLL: Duration = Duration::from_millis(500);
const TREE_MAX: usize = 256;

/// Closes a live agent's pane, then stops any survivor of the pane's process tree (same pid and start only). Its
/// parent or a human may stop it. Posts the `stopped` notice and returns the resume command.
pub fn stop(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let caller = resolve(ctx)?;
    let pane = find_pane(ctx, &caller, first(request))?;
    let Some(record) = registry::record(ctx, &caller, &pane.id)? else {
        return refuse(2, "target", "no live agent has that name");
    };
    if caller.kind == Kind::Agent
        && record.get("parentPane").and_then(Value::as_str) != Some(caller.pane.as_str())
    {
        return refuse(
            2,
            "parent",
            "only the parent or a human can stop this agent",
        );
    }
    // A pane pid that is not a number would walk from pid 0, whose "children" are unrelated processes.
    let root: u32 = pane
        .pid
        .parse()
        .map_err(|_| Refusal::internal("the pane pid is not a number"))?;
    let launcher = server_dir(ctx, &caller)
        .join("agents")
        .join(&pane.id)
        .join("launcher.json");
    let left = lock(ctx, &lock_key("notice", &caller, &pane.id), || {
        let table = ctx.os.table()?;
        // The pane's process must be the agent's own launcher: the pid and start it recorded. A pane pid reused since
        // (or a launcher that never recorded itself) is never walked: only the pane is closed.
        let recorded = read_json(&launcher)
            .ok()
            .flatten()
            .and_then(|l| Some((l.get("pid")?.as_u64()?, l.get("start")?.as_i64()?)));
        let listed = table
            .iter()
            .find(|row| row.pid == root)
            .and_then(|row| row.start);
        let unverified = match recorded {
            None => Some(Reason::Open),
            Some((pid, start))
                if pid == u64::from(root)
                    && listed.is_some_and(|listed| ctx.os.same_start(start, listed)) =>
            {
                None
            }
            Some(_) => Some(Reason::Start),
        };
        // Every tree row that is not killed is left with its reason: not opened, another start, or a failed kill.
        let mut left = Vec::new();
        let mut held = Vec::new();
        for row in tree(&table, root) {
            if let Some(reason) = unverified {
                left.push(Left {
                    pid: row.pid,
                    reason,
                });
                continue;
            }
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
        native(ctx, &["kill-pane", "-t", &pane.pane], true)?;
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
        Ok(left)
    })?;
    let client = if text(record.get("client")).eq_ignore_ascii_case("claude") {
        "claude"
    } else {
        "copilot"
    };
    let resume = format!("{client} --resume {}", text(record.get("sessionId")));
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
    reply.insert("resume".into(), resume.into());
    Ok(reply)
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
    use crate::agents::fixture::{CLAUDE, ID, SESSION_ID, World, write};
    use crate::agents::lock_key;
    use crate::agents::view::focus;
    use crate::testing;
    use serde_json::json;

    type Handler = fn(&Ctx, &Request) -> Res<Map<String, Value>>;

    const PROBE: &str = "#{session_id}|#{@agent-id}";
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
        assert!(!mux_verbs(&w).iter().any(|v| v == "kill-pane"));
    }

    #[test]
    fn send_parent_from_human_refuses_parent() {
        let w = World::human("");
        w.fakes.clear_log();
        assert_eq!(
            run(&w, send, &["send", "parent", "-Body", "x"]),
            "2 parent only an agent has a parent"
        );
        // No recipient probe, nothing written.
        assert_eq!(mux_verbs(&w), ["display-message"]);
        assert!(!w.ctx.state_root.exists());
    }

    #[test]
    fn tagged_sender_without_record_refuses_parent() {
        let refused = "2 parent this agent pane has no record";
        for tag in ["bbbbbbbbbbbb", "tag-7"] {
            let w = World::human(tag);
            w.agent_record(ID, json!({}));
            w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
            w.fakes.mux_first(PROBE, 0, "$3|\n", "");
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

    // An agent pane %1 with its record, tagged ID, whose parent is %4; the recipient probe answers session $3.
    fn agent_world() -> World {
        let w = World::agent(ID);
        w.agent_record(ID, json!({"pane": "%1", "parentPane": "%4"}));
        w.fakes.mux_first(PROBE, 0, "$3|\n", "");
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
        w.fakes.mux_first(PROBE, 0, "$3|\n", "");
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
        w.fakes.mux_first(PROBE, 0, "$3|\n", "");
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
            (0, "$4|\n"),
            (1, "$3|\n"),
            (0, ""),
            (0, "|\n"),
            (0, "$3\n"),
            (0, "$3|\nmore\n"),
            (0, "$03|\n"),
            (0, "3|\n"),
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
        // The probe goes to the named pane; an agent pane's mailbox is its id, any other is pane-<n>.
        let w = World::human("");
        w.fakes.mux_first(PROBE, 0, "$3|bbbbbbbbbbbb\r\n", "");
        w.fakes.clear_log();
        assert!(
            run(&w, send, &["send", "%5", "-Body", "x"])
                .starts_with(r#"ok {"to":"%5","pane":"%5""#)
        );
        assert_eq!(
            w.fakes.mux_calls()[1],
            ["-N", "display-message", "-p", "-t", "%5", PROBE]
        );
        assert_eq!(read_mail_of(&w, "bbbbbbbbbbbb"), ["x"]);
        w.fakes.mux_first(PROBE, 0, "$3|\n", "");
        run(&w, send, &["send", "%5", "-Body", "y"]);
        assert_eq!(read_mail_of(&w, "pane-5"), ["y"]);
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
        w.fakes.mux_first(PROBE, 0, "$3|\n", "");
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
        w.fakes.mux_first(PROBE, 0, "$3|\n", "");
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
            w.fakes.mux_first(PROBE, 0, "$3|\n", "");
            run(&w, send, &["send", "%1", "-Body", "m"]);
            let reply = run(&w, read, &["read", "-Wait", wait]);
            assert!(reply.contains("\\nm\""), "{wait:?} {reply}");
        }
        assert!(w.fakes.slept().is_empty());
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
            .mux_first("capture-pane", 0, "one\r\ntwo\n\nthree\r\n\n", "");
        assert_eq!(
            run(&w, read, &["read", "worker", "-Lines", "2"]),
            r#"ok {"name":"worker","pane":"%9","text":"\nthree"}"#
        );
        for (args, s) in [
            (&["read", "worker"][..], "-60"),
            (&["read", "worker", "-Lines", "2000"], "-2000"),
            (&["read", "worker", "-Lines", "1"], "-1"),
        ] {
            w.fakes.clear_log();
            assert!(run(&w, read, args).starts_with("ok "));
            assert_eq!(
                w.fakes.mux_calls().last().unwrap(),
                &["-N", "capture-pane", "-p", "-J", "-t", "%9", "-S", s]
            );
        }
        w.fakes.mux_first("capture-pane", 1, "", "SECRET");
        assert_eq!(
            run(&w, read, &["read", "worker"]),
            "1 rmux rmux call failed"
        );
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
        w
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
        assert!(at("open 321") < at("mux -N kill-pane -t %9"), "{events:?}");
        assert!(at("mux -N kill-pane -t %9") < at("kill 300"), "{events:?}");
        let kill_pane: Vec<_> = w
            .fakes
            .mux_calls()
            .into_iter()
            .filter(|c| c[1] == "kill-pane")
            .collect();
        assert_eq!(kill_pane, [["-N", "kill-pane", "-t", "%9"]]);
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
            assert!(!mux_verbs(&w).iter().any(|v| v == "kill-pane"), "{pid}");
        }
        // The pane pid was reused (another start than launcher.json's), or launcher.json names another pid or
        // none: only the pane is closed, and its tree is left as survivors.
        for (launcher, reason) in [
            (Some(json!({"pid": 300, "start": 2500})), "start"),
            (Some(json!({"pid": 301, "start": 3000})), "start"),
            (Some(json!({"pid": 300})), "open"),
            (None, "open"),
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
            let left = format!(r#""left":["300 {reason}","310 {reason}"]"#);
            assert!(reply.contains(&left), "{launcher:?} {reply}");
            assert_eq!(proc_calls(&w), ["table"], "{launcher:?}");
            let kill_pane: Vec<_> = w
                .fakes
                .mux_calls()
                .into_iter()
                .filter(|c| c[1] == "kill-pane")
                .collect();
            assert_eq!(kill_pane, [["-N", "kill-pane", "-t", "%9"]], "{launcher:?}");
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
        assert!(!mux_verbs(&w).iter().any(|v| v == "kill-pane"));
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
}
