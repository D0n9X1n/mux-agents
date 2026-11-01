//! The side-bar feed: live agents in pane order and, with -All, the session's ended agents (MuxAgents.ps1:1034-1075,
//! 1216-1269).

use std::collections::HashMap;
use std::time::SystemTime;

use serde_json::{Map, Value};

use super::mail::{doing_line, unread_counts};
use super::registry::{self, Pane};
use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::error::Res;
use crate::state::fs::read_json;
use crate::util::{age_seconds, parse_utc};

/// What an agent is doing: its last status, else its last sent line, else its task line, each cleaned again; a
/// blank one falls through. `(doing, doingFrom)`, both null when none is left.
pub fn select_doing(
    status: Option<&Value>,
    message: Option<&Value>,
    task: Option<&Value>,
) -> (Value, Value) {
    for (from, source) in [("status", status), ("message", message), ("task", task)] {
        let Some(source) = source.filter(|v| !v.is_null()) else {
            continue;
        };
        let line = doing_line(&super::text(Some(source)));
        if !line.is_empty() {
            return (line.into(), from.into());
        }
    }
    (Value::Null, Value::Null)
}

/// `{running, agents}` for the caller's session: live agents in pane order and, with `all`, the session's ended
/// agents, newest notice first, at most 10. Unread is `reader`'s. `with_exit` (watch only) adds each ended row's
/// exitCode from its notice.json. Locks one at a time: notice locks, the reader's mail lock, then each row's
/// agent lock.
pub fn board(
    ctx: &Ctx,
    caller: &Caller,
    reader: &str,
    all: bool,
    with_exit: bool,
) -> Res<Map<String, Value>> {
    // One list-panes for the notices, the live rows and the ended filter; list runs every 3 s. A reap that killed a
    // pane or posted a notice makes that snapshot stale, so the panes are listed again.
    let mut tagged = registry::tagged(ctx, caller)?;
    if registry::reap(ctx, caller, Some(&tagged), false)? {
        tagged = registry::tagged(ctx, caller)?;
    }
    let rows = Rows {
        ctx,
        caller,
        unread: unread_counts(ctx, caller, reader)?,
        now: ctx.clock.now(),
    };
    let panes = registry::committed(ctx, caller, tagged);
    let mut agents = Vec::new();
    for pane in &panes {
        let Some(record) = registry::record(ctx, caller, &pane.id)? else {
            continue;
        };
        if !same_session(&record, caller) {
            continue;
        }
        agents.push(rows.row(
            &pane.id,
            &record,
            pane.pane.clone(),
            &pane.window,
            "running",
        )?);
    }
    let running = agents.len();
    if all {
        let mut ended = ended(ctx, caller, &panes)?;
        // Newest first (an unreadable time is oldest), then by id.
        ended.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.id.cmp(&b.id)));
        for end in ended.into_iter().take(ENDED_MAX) {
            let pane = super::text(end.record.get("pane"));
            let window = super::text(end.record.get("window"));
            let mut row = rows.row(&end.id, &end.record, pane, &window, &end.kind)?;
            // After the row's fields, so they keep their places: who closed a lost agent.
            if let Some(closed) = end.closed_by {
                row.insert("closedBy".into(), closed.into());
            }
            if with_exit {
                row.insert("exitCode".into(), end.code);
            }
            agents.push(row);
        }
    }
    let mut reply = Map::new();
    reply.insert("running".into(), running.into());
    reply.insert(
        "agents".into(),
        Value::Array(agents.into_iter().map(Value::Object).collect()),
    );
    Ok(reply)
}

const ENDED_MAX: usize = 10;
const ENDED: [&str; 3] = ["exited", "stopped", "lost"];

/// The newest ended agent named `name` of the caller's session that is not live, as (id, record, state): any of
/// them, not only the 10 a board shows. Newest notice first, then by id, as the board orders them.
pub(crate) fn newest_ended(
    ctx: &Ctx,
    caller: &Caller,
    live: &[Pane],
    name: &str,
) -> Res<Option<(String, Value, String)>> {
    let mut ended: Vec<Ended> = ended(ctx, caller, live)?
        .into_iter()
        .filter(|end| end.record.get("name").and_then(Value::as_str) == Some(name))
        .collect();
    ended.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.id.cmp(&b.id)));
    Ok(ended
        .into_iter()
        .next()
        .map(|end| (end.id, end.record, end.kind)))
}

fn same_session(record: &Value, caller: &Caller) -> bool {
    record.get("session").and_then(Value::as_str) == Some(caller.session.as_str())
}

struct Ended {
    at: Option<SystemTime>,
    id: String,
    kind: String,
    record: Value,
    code: Value,
    closed_by: Option<String>,
}

/// The session's agents with a notice that are not live. notice.json is written once, so it is read unlocked.
fn ended(ctx: &Ctx, caller: &Caller, live: &[Pane]) -> Res<Vec<Ended>> {
    let agents = super::server_dir(ctx, caller).join("agents");
    let entries = match std::fs::read_dir(&agents) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !super::agent_id(&id) || !entry.path().is_dir() || live.iter().any(|p| p.id == id) {
            continue;
        }
        let Some(notice) = read_json(&entry.path().join("notice.json"))? else {
            continue;
        };
        let kind = notice
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !ENDED.contains(&kind) {
            continue;
        }
        let Some(record) = registry::record(ctx, caller, &id)? else {
            continue;
        };
        if !same_session(&record, caller) || record.get("id").and_then(Value::as_str) != Some(&id) {
            continue;
        }
        found.push(Ended {
            at: notice.get("at").and_then(Value::as_str).and_then(parse_utc),
            kind: kind.to_string(),
            code: notice.get("exitCode").cloned().unwrap_or(Value::Null),
            closed_by: notice
                .get("closedBy")
                .and_then(Value::as_str)
                .map(str::to_string),
            id,
            record,
        });
    }
    Ok(found)
}

struct Rows<'a> {
    ctx: &'a Ctx,
    caller: &'a Caller,
    unread: HashMap<String, usize>,
    now: SystemTime,
}

impl Rows<'_> {
    /// One row. status.json and last-sent.json are read under the agent lock their writers hold; an unreadable
    /// one is skipped. Doing never comes from the screen.
    fn row(
        &self,
        id: &str,
        record: &Value,
        pane: String,
        window: &str,
        state: &str,
    ) -> Res<Map<String, Value>> {
        let folder = super::server_dir(self.ctx, self.caller)
            .join("agents")
            .join(id);
        let (status, sent) =
            super::lock(self.ctx, &super::lock_key("agent", self.caller, id), || {
                let read = |name: &str| read_json(&folder.join(name)).ok().flatten();
                Ok((read("status.json"), read("last-sent.json")))
            })?;
        let (doing, from) = select_doing(
            status.as_ref().and_then(|s| s.get("text")),
            sent.as_ref().and_then(|s| s.get("line")),
            record.get("taskLine"),
        );
        let field = |name: &str| record.get(name).cloned().unwrap_or(Value::Null);
        let name = super::text(record.get("name"));
        let age = record
            .get("createdAt")
            .and_then(Value::as_str)
            .and_then(|created| age_seconds(created, self.now));
        let unread = self
            .unread
            .get(&format!("{name}|{pane}"))
            .copied()
            .unwrap_or(0);
        let mut row = Map::new();
        row.insert("name".into(), field("name"));
        row.insert("client".into(), field("client"));
        row.insert("pane".into(), pane.into());
        row.insert("window".into(), window.into());
        row.insert("context".into(), field("context"));
        row.insert("parent".into(), field("parentPane"));
        row.insert("sessionId".into(), field("sessionId"));
        row.insert("directory".into(), field("directory"));
        row.insert("createdAt".into(), field("createdAt"));
        row.insert("state".into(), state.into());
        row.insert("doing".into(), doing);
        row.insert("doingFrom".into(), from);
        row.insert("age".into(), age.map_or(Value::Null, Value::from));
        row.insert("unread".into(), unread.into());
        // Last, so the -Text columns before it keep their places; null is the client's own model.
        row.insert("model".into(), field("model"));
        Ok(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::fixture::{ID, SESSION_ID, World, write};
    use crate::agents::lock_key;
    use crate::agents::mail::{add_mail, read_mail};
    use serde_json::json;
    use std::path::{Path, PathBuf};

    const NOW: &str = "2026-10-29T12:00:00.0000000Z";

    // Every file below `dir` with its bytes.
    fn tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut files = Vec::new();
        let mut folders = vec![dir.to_path_buf()];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(folder).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    folders.push(path);
                } else {
                    let bytes = std::fs::read(&path).unwrap();
                    files.push((path, bytes));
                }
            }
        }
        files.sort();
        files
    }

    fn names(reply: &Map<String, Value>) -> Vec<String> {
        reply["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                format!(
                    "{}={}",
                    row["name"].as_str().unwrap(),
                    row["state"].as_str().unwrap()
                )
            })
            .collect()
    }

    #[test]
    fn doing_status_then_message_then_task() {
        let s = |text: &str| json!(text);
        let cases = [
            (Some(s("s")), Some(s("m")), Some(s("t"))),
            (Some(s("")), Some(s("m")), Some(s("t"))),
            (Some(s(" ")), Some(s("")), Some(s("t"))),
            (None, None, None),
            (Some(Value::Null), Some(Value::Null), Some(Value::Null)),
            (Some(s("a\tb\u{9b}\nc")), Some(s("m")), Some(s("t"))),
            (Some(json!(5)), None, None),
        ];
        let got: Vec<String> = cases
            .iter()
            .map(|(a, b, c)| {
                let (doing, from) = select_doing(a.as_ref(), b.as_ref(), c.as_ref());
                format!("{from}={doing}")
            })
            .collect();
        assert_eq!(
            got,
            [
                r#""status"="s""#,
                r#""message"="m""#,
                r#""task"="t""#,
                "null=null",
                "null=null",
                r#""status"="a b""#,
                r#""status"="5""#
            ]
        );
    }

    #[test]
    fn board_row_field_order() {
        let w = World::human("");
        w.clock(NOW);
        let caller = w.caller();
        w.agent_record(
            ID,
            json!({"createdAt": "2026-10-29T11:56:00.0000000Z", "model": "sonnet"}),
        );
        w.agent_record(
            "bbbbbbbbbbbb",
            json!({"name": "idle", "pane": "%8", "client": "copilot", "taskLine": "",
                "createdAt": "2026-10-29T11:59:55.0000000Z"}),
        );
        w.panes(&[
            "%9|@6|300|aaaaaaaaaaaa|worker",
            "%8|@7|301|bbbbbbbbbbbb|idle",
        ]);
        for (from, pane) in [
            ("worker", "%9"),
            ("worker", "%9"),
            ("idle", "%8"),
            ("worker", "%4"),
        ] {
            add_mail(&w.ctx, &caller, "pane-1", from, pane, "x", None).unwrap();
        }
        let before = tree(&w.state());
        w.fakes.clear_log();
        let reply = board(&w.ctx, &caller, "pane-1", false, false).unwrap();
        let row = |name,
                   client,
                   pane,
                   window,
                   created,
                   doing: Value,
                   from: Value,
                   age,
                   unread,
                   model: Value| {
            json!({"name": name, "client": client, "pane": pane, "window": window, "context": "brief",
                "parent": "%1", "sessionId": SESSION_ID, "directory": "/work", "createdAt": created,
                "state": "running", "doing": doing, "doingFrom": from, "age": age, "unread": unread,
                "model": model})
        };
        // A record without a model (the client's own) lists null.
        let want = json!({"running": 2, "agents": [
            row("worker", "claude", "%9", "@6", "2026-10-29T11:56:00.0000000Z", json!("build it"), json!("task"), 240, 2, json!("sonnet")),
            row("idle", "copilot", "%8", "@7", "2026-10-29T11:59:55.0000000Z", Value::Null, Value::Null, 5, 1, Value::Null)
        ]});
        // Compared as text: the order of keys is part of the reply.
        assert_eq!(Value::Object(reply).to_string(), want.to_string());
        // One multiplexer call (no probe, no capture-pane), no state write, the offset unmoved.
        let calls = w.fakes.mux_calls();
        assert_eq!(calls.len(), 1);
        assert!(crate::agents::fixture::list_call(&calls[0]), "{calls:?}");
        assert_eq!(tree(&w.state()), before);
        assert_eq!(read_mail(&w.ctx, &caller, "pane-1").unwrap().len(), 4);
    }

    // A live worker and ended agents of every kind, another session, a foreign record and an unreadable time.
    fn ended_world() -> (World, Caller) {
        let w = World::human("");
        w.clock(NOW);
        let caller = w.caller();
        w.agent_record(ID, json!({}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        for (id, name, kind, code, at, session) in [
            ("bbbbbbbbbbbb", "done", "exited", json!(3), "11:59", "$3"),
            (
                "cccccccccccc",
                "halted",
                "stopped",
                Value::Null,
                "11:50",
                "$3",
            ),
            (
                "dddddddddddd",
                "vanished",
                "lost",
                Value::Null,
                "11:55",
                "$3",
            ),
            ("eeeeeeeeeeee", "other", "exited", json!(0), "11:58", "$4"),
            ("111111111111", "odd", "paused", Value::Null, "11:57", "$3"),
            ("222222222222", "late", "lost", Value::Null, "garbage", "$3"),
            ("333333333333", "tie", "stopped", Value::Null, "11:50", "$3"),
        ] {
            let folder =
                w.agent_record(id, json!({"name": name, "pane": "%7", "session": session}));
            let at = if at == "garbage" {
                at.to_string()
            } else {
                format!("2026-10-29T{at}:00.0000000Z")
            };
            write(
                &folder.join("notice.json"),
                json!({"kind": kind, "exitCode": code, "at": at}),
            );
        }
        let foreign = w.agent_record(
            "444444444444",
            json!({"id": "bbbbbbbbbbbb", "name": "foreign"}),
        );
        write(
            &foreign.join("notice.json"),
            json!({"kind": "lost", "exitCode": null, "at": NOW}),
        );
        (w, caller)
    }

    #[test]
    fn ended_newest_first_max_10() {
        let (w, caller) = ended_world();
        let all = board(&w.ctx, &caller, "pane-1", true, false).unwrap();
        assert_eq!(all["running"], 1);
        // Newest notice first, ties by id, an unreadable time last.
        assert_eq!(
            names(&all),
            [
                "worker=running",
                "done=exited",
                "vanished=lost",
                "tie=stopped",
                "halted=stopped",
                "late=lost"
            ]
        );
        let plain = board(&w.ctx, &caller, "pane-1", false, false).unwrap();
        assert_eq!(names(&plain), ["worker=running"]);

        // The live row and exactly the 10 newest of 12 ended agents.
        let w = World::human("");
        w.clock(NOW);
        let caller = w.caller();
        w.agent_record("ffffffffffff", json!({}));
        w.panes(&["%9|@5|300|ffffffffffff|worker"]);
        for n in 0..12u32 {
            let id = format!("{n:012x}");
            let folder = w.agent_record(
                &id,
                json!({"name": format!("a{n}"), "pane": format!("%{}", 20 + n)}),
            );
            let minutes = (n * 5) % 12 + 1;
            let at = format!("2026-10-29T11:{:02}:00.0000000Z", 60 - minutes);
            write(
                &folder.join("notice.json"),
                json!({"kind": "stopped", "exitCode": null, "at": at}),
            );
        }
        let all = board(&w.ctx, &caller, "pane-1", true, false).unwrap();
        let got: Vec<String> = names(&all)
            .into_iter()
            .map(|n| n.split('=').next().unwrap().to_string())
            .collect();
        assert_eq!(
            (all["running"].clone(), got),
            (
                json!(1),
                [
                    "worker", "a0", "a5", "a10", "a3", "a8", "a1", "a6", "a11", "a4", "a9"
                ]
                .map(String::from)
                .to_vec()
            )
        );
    }

    #[test]
    fn lost_row_says_closed_by_pane() {
        let w = World::human("");
        w.clock(NOW);
        let caller = w.caller();
        for (id, name, notice) in [
            (
                "bbbbbbbbbbbb",
                "vanished",
                json!({"kind": "lost", "exitCode": null, "at": "2026-10-29T11:59:00.0000000Z", "closedBy": "pane"}),
            ),
            (
                "cccccccccccc",
                "halted",
                json!({"kind": "stopped", "exitCode": null, "at": "2026-10-29T11:58:00.0000000Z"}),
            ),
        ] {
            let folder = w.agent_record(id, json!({"name": name, "pane": "%7"}));
            write(&folder.join("notice.json"), notice);
        }
        w.panes(&[]);
        let all = board(&w.ctx, &caller, "pane-1", true, false).unwrap();
        let rows = all["agents"].as_array().unwrap();
        let lost = rows[0].as_object().unwrap();
        assert_eq!(lost["name"], "vanished");
        // After the 15 row fields, so they keep their places.
        assert_eq!(
            lost.iter().map(|(k, v)| (k.as_str(), v)).next_back(),
            Some(("closedBy", &json!("pane")))
        );
        assert!(rows[1].get("closedBy").is_none(), "{}", rows[1]);
        // The watch feed puts exitCode last, after it.
        let watch = board(&w.ctx, &caller, "pane-1", true, true).unwrap();
        let keys: Vec<&str> = watch["agents"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys[keys.len() - 2..], ["closedBy", "exitCode"]);
    }

    #[test]
    fn with_exit_code_only_for_watch() {
        let (w, caller) = ended_world();
        let watch = board(&w.ctx, &caller, "pane-1", true, true).unwrap();
        let codes: Vec<String> = watch["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let row = row.as_object().unwrap();
                match row.get("exitCode") {
                    // Added last, after unread.
                    Some(code) => {
                        assert_eq!(row.keys().next_back().unwrap(), "exitCode");
                        format!("{}={code}", row["name"].as_str().unwrap())
                    }
                    None => format!("{}=none", row["name"].as_str().unwrap()),
                }
            })
            .collect();
        assert_eq!(
            codes,
            [
                "worker=none",
                "done=3",
                "vanished=null",
                "tie=null",
                "halted=null",
                "late=null"
            ]
        );
        let list = board(&w.ctx, &caller, "pane-1", true, false).unwrap();
        assert!(
            !list["agents"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row.get("exitCode").is_some())
        );
    }

    #[test]
    fn board_reads_inside_their_locks() {
        let w = World::human("");
        let caller = w.caller();
        let live = w.agent_record(ID, json!({}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        let mailbox = w.state().join("mail").join("pane-1.jsonl");
        let (agent, mail) = (
            lock_key("agent", &caller, ID),
            lock_key("mail", &caller, "pane-1"),
        );
        let sent = live.join("last-sent.json");
        // last-sent.json exists only while the agent lock is held; a message lands as the mail lock is taken.
        w.fakes.lock_probe(move |event| {
            if event == format!("+{agent}") {
                write(&sent, json!({"line": "fresh", "at": "x"}));
            } else if event == format!("-{agent}") {
                std::fs::remove_file(&sent).unwrap();
            } else if event == format!("+{mail}") {
                let line = r#"{"id":"m1","from":"worker","pane":"%9","at":"x","body":"hi"}"#;
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&mailbox)
                    .unwrap();
                std::io::Write::write_all(&mut file, format!("{line}\n").as_bytes()).unwrap();
            }
            String::new()
        });
        let reply = board(&w.ctx, &caller, "pane-1", false, false).unwrap();
        let row = &reply["agents"][0];
        assert_eq!(
            (&row["doing"], &row["doingFrom"], &row["unread"]),
            (&json!("fresh"), &json!("message"), &json!(1))
        );
    }

    #[test]
    fn board_lists_again_after_reap_acted() {
        use std::cell::Cell;
        use std::rc::Rc;
        let w = World::human("");
        let caller = w.caller();
        let folder = w.agent_record(ID, json!({}));
        write(
            &folder.join("launcher.json"),
            json!({"pid": 300, "exitCode": 0}),
        );
        // %9 is listed dead until the conditional kill ran; then it is gone.
        let killed = Rc::new(Cell::new(false));
        let gone = killed.clone();
        w.fakes.mux_script("list-panes", move |argv| {
            let rows: &[&str] = if gone.get() {
                &[]
            } else {
                &["%9|@5|300|aaaaaaaaaaaa|worker"]
            };
            Ok(crate::mux::Output {
                code: 0,
                stdout: crate::agents::fixture::listed_dead(argv, rows, "\n", &["%9"]),
                stderr: String::new(),
            })
        });
        let kill = killed.clone();
        w.fakes.mux_script("if-shell", move |_| {
            kill.set(true);
            Ok(crate::mux::Output {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });
        w.fakes.mux_rule("#{pane_id}|#{@agent-id}", 0, "|\n", "");
        w.fakes.clear_log();
        let reply = board(&w.ctx, &caller, "pane-1", true, false).unwrap();
        assert!(killed.get());
        assert_eq!(
            (names(&reply), &reply["running"]),
            (vec!["worker=exited".to_string()], &json!(0))
        );
        let lists = w
            .fakes
            .mux_calls()
            .iter()
            .filter(|c| crate::agents::fixture::list_call(c))
            .count();
        assert_eq!(lists, 2);
    }

    #[test]
    fn lock_order_notice_mail_agent() {
        let w = World::human("");
        let caller = w.caller();
        let live = w.agent_record(ID, json!({}));
        w.agent_record("bbbbbbbbbbbb", json!({"name": "gone", "pane": "%8"}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        w.fakes.mux_first("#{pane_id}|#{@agent-id}", 0, "|\n", "");
        // status.json exists only while the row's agent lock is held, so a row that shows it read it there.
        let agent = lock_key("agent", &caller, ID);
        let status = live.join("status.json");
        let (held, file) = (agent.clone(), status.clone());
        w.fakes.lock_probe(move |event| {
            if event == format!("+{held}") {
                write(&file, json!({"text": "held", "at": "x"}));
            } else if event == format!("-{held}") {
                std::fs::remove_file(&file).unwrap();
            }
            String::new()
        });
        w.fakes.clear_log();
        let reply = board(&w.ctx, &caller, "pane-1", false, false).unwrap();
        let notice = lock_key("notice", &caller, "bbbbbbbbbbbb");
        let mail = lock_key("mail", &caller, "pane-1");
        assert_eq!(
            w.fakes.locks(),
            [
                format!("+{notice}"),
                format!("+{mail}"),
                format!("-{mail}"),
                format!("-{notice}"),
                format!("+{mail}"),
                format!("-{mail}"),
                format!("+{agent}"),
                format!("-{agent}")
            ]
        );
        let row = &reply["agents"][0];
        assert_eq!(
            (&row["doing"], &row["doingFrom"]),
            (&json!("held"), &json!("status"))
        );
        assert!(!status.exists());
        // An unreadable status.json is skipped: the row falls to the last sent line.
        std::fs::write(&status, "{bad").unwrap();
        write(
            &live.join("last-sent.json"),
            json!({"line": "sent", "at": "x"}),
        );
        w.fakes.lock_probe(|_| String::new());
        let reply = board(&w.ctx, &caller, "pane-1", false, false).unwrap();
        let row = &reply["agents"][0];
        assert_eq!(
            (&row["doing"], &row["doingFrom"]),
            (&json!("sent"), &json!("message"))
        );
    }
}
