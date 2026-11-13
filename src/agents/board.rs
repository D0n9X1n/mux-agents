//! The side-bar feed: live agents in pane order and, with -All, the session's ended agents (MuxAgents.ps1:1034-1075,
//! 1216-1269).

use std::collections::HashMap;
use std::time::SystemTime;

use serde_json::{Map, Value};

use super::mail::{doing_line, mailbox_key, unread_counts};
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

/// `{running, agents}` for the caller's session: running agents in pane order and, with `all`, `endedTotal` (every
/// ended agent of the session) and the session's ended agents, newest notice first, at most 10, each with its exitCode
/// from its notice.json. Unread is the caller's own mailbox's: a running agent's pane reads its id's, any other pane
/// `pane-<n>`, as the same snapshot shows it (a tag never names a reader). `watch` (the bar's feed) adds each live
/// row's `waiting` (S2.4), and a live row's unread is instead its own messages still unread
/// in its parent's mailbox (S2.5). Locks one at a time: notice locks, the reader's (or each parent's) mail lock, then
/// each row's agent lock.
pub fn board(ctx: &Ctx, caller: &Caller, all: bool, watch: bool) -> Res<Map<String, Value>> {
    // One list-panes for the notices, the live rows and the ended filter; list runs every 3 s. A reap that killed a
    // pane or posted a notice makes that snapshot stale, so the panes are listed again.
    let snapshot = registry::live_after_reap(ctx, caller)?;
    let panes = snapshot.running();
    let reader = match snapshot.own(caller) {
        registry::Own::Agent(me) => me.id,
        _ => mailbox_key(&caller.pane, "")?,
    };
    // The mailboxes whose unread counts: the reader's, or (watch) each live agent's parent's, in key order.
    let mut keys: Vec<String> = if watch {
        panes
            .iter()
            .filter_map(|pane| parent_key(&pane.record, &panes))
            .collect()
    } else {
        vec![reader.clone()]
    };
    keys.sort();
    keys.dedup();
    let mut unread = HashMap::new();
    for key in keys {
        let counts = unread_counts(ctx, caller, &key)?;
        unread.insert(key, counts);
    }
    let rows = Rows {
        ctx,
        caller,
        unread,
        reader: (!watch).then_some(reader),
        running: &panes,
        now: ctx.clock.now(),
        watch,
    };
    let mut agents = Vec::new();
    for pane in &panes {
        agents.push(rows.row(
            &pane.id,
            &pane.record,
            pane.pane.clone(),
            &pane.window,
            "running",
        )?);
    }
    // Ranked as herdr does, then the most recent change first by its own time (no report last), then pane order
    // (a stable sort).
    agents.sort_by_key(|row| {
        let changed = row
            .get("changedAt")
            .and_then(Value::as_str)
            .and_then(parse_utc);
        (rank(row), changed.is_none(), std::cmp::Reverse(changed))
    });
    let running = agents.len();
    let mut ended_total = None;
    if all {
        let mut ended = ended(ctx, caller, &panes)?;
        ended_total = Some(ended.len());
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
            // Last: its exit code from notice.json, null when it has none.
            row.insert("exitCode".into(), end.code);
            agents.push(row);
        }
    }
    let mut reply = Map::new();
    reply.insert("running".into(), running.into());
    if let Some(total) = ended_total {
        reply.insert("endedTotal".into(), total.into());
    }
    reply.insert(
        "agents".into(),
        Value::Array(agents.into_iter().map(Value::Object).collect()),
    );
    Ok(reply)
}

/// Ended agents a board (`list -All`) shows, newest first.
pub(crate) const ENDED_MAX: usize = 10;
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

/// The mailbox of an agent's parent pane: the running agent's id there, else `pane-<n>` (as `send parent`
/// delivers); none for a malformed pane. Never a tag.
fn parent_key(record: &Value, running: &[Pane]) -> Option<String> {
    let parent = super::text(record.get("parentPane"));
    let id = running
        .iter()
        .find(|p| p.pane == parent)
        .map_or("", |p| p.id.as_str());
    mailbox_key(&parent, id).ok()
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
    // Per mailbox key, its unread counts by `<from>|<pane>`.
    unread: HashMap<String, HashMap<String, usize>>,
    // The one mailbox every row counts in; none in watch mode, where each row counts in its parent's.
    reader: Option<String>,
    running: &'a [Pane],
    now: SystemTime,
    watch: bool,
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
        let live = state == "running";
        let (status, sent, waiting, reported, seen) =
            super::lock(self.ctx, &super::lock_key("agent", self.caller, id), || {
                let read = |name: &str| read_json(&folder.join(name)).ok().flatten();
                let status = read("status.json");
                // Only a live row reads what its client reported and when the person last left its pane.
                let reported = live.then(|| read("activity.json")).flatten();
                let seen = live.then(|| read("seen.json")).flatten();
                // Only a live row reads its waiting files: its activity falls back to them in every feed.
                let waiting = live.then(|| {
                    let at = status
                        .as_ref()
                        .and_then(|s| s.get("at"))
                        .and_then(Value::as_str)
                        .and_then(parse_utc);
                    super::message::waiting_now(self.ctx, &folder, at)
                });
                Ok((status, read("last-sent.json"), waiting, reported, seen))
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
        let mailbox = match &self.reader {
            Some(reader) => Some(reader.clone()),
            None => parent_key(record, self.running),
        };
        let unread = mailbox
            .and_then(|key| self.unread.get(&key))
            .and_then(|counts| counts.get(&format!("{name}|{pane}")))
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
        // The identity hue: the recorded one, else the name's.
        let recorded = record.get("color").and_then(Value::as_str);
        row.insert(
            "color".into(),
            crate::paint::identity(recorded, &name).into(),
        );
        if live {
            let (activity, since) = activity(reported.as_ref(), seen.as_ref(), waiting, self.now);
            row.insert("activity".into(), activity.into());
            row.insert("since".into(), since.map_or(Value::Null, Value::from));
            // The change's own time: names the done episode the bar marks seen.
            let changed = reported
                .as_ref()
                .and_then(|r| r.get("since"))
                .filter(|_| since.is_some())
                .cloned();
            row.insert("changedAt".into(), changed.unwrap_or(Value::Null));
        }
        // Only the bar's feed shows the waiting flag itself.
        if let Some(waiting) = waiting.filter(|_| self.watch) {
            row.insert("waiting".into(), waiting.into());
        }
        Ok(row)
    }
}

/// A live agent's activity and the seconds since it last changed, as herdr names them: what its client reported in
/// activity.json (`blocked`, `working`; `idle` is `done` until the person has looked at this very idle spell, which
/// seen.json names by its `since`); with no readable report, `idle` while it listens in `read -Wait` and `working`
/// otherwise, with no since.
fn activity(
    reported: Option<&Value>,
    seen: Option<&Value>,
    waiting: Option<bool>,
    now: SystemTime,
) -> (&'static str, Option<u64>) {
    let at = |value: Option<&Value>, key: &str| {
        value
            .and_then(|v| v.get(key))
            .and_then(Value::as_str)
            .and_then(parse_utc)
    };
    let state = reported
        .and_then(|r| r.get("state"))
        .and_then(Value::as_str);
    let since = at(reported, "since");
    let word = match (state, since) {
        (Some("blocked"), Some(_)) => "blocked",
        (Some("working"), Some(_)) => "working",
        (Some("idle"), Some(since)) => match at(seen, "since") {
            Some(seen) if seen >= since => "idle",
            _ => "done",
        },
        _ => {
            let word = if waiting == Some(true) {
                "idle"
            } else {
                "working"
            };
            return (word, None);
        }
    };
    let secs = since.map(|since| now.duration_since(since).map_or(0, |d| d.as_secs()));
    (word, secs)
}

/// Records that the person looked at the done spell `changed` (a row's `changedAt`) of the running agent on `pane`:
/// seen.json under its agent lock, never moved back to an older spell. False when no running agent is on the pane.
pub fn mark_seen(ctx: &Ctx, caller: &Caller, pane: &str, changed: &str) -> Res<bool> {
    let Some(at) = parse_utc(changed) else {
        return Ok(false);
    };
    let snapshot = registry::Snapshot::take(ctx, caller)?;
    let Some(agent) = snapshot.running().into_iter().find(|p| p.pane == pane) else {
        return Ok(false);
    };
    let file = super::state_dir(ctx, caller)?
        .join("agents")
        .join(&agent.id)
        .join("seen.json");
    super::lock(ctx, &super::lock_key("agent", caller, &agent.id), || {
        let old = read_json(&file)
            .ok()
            .flatten()
            .and_then(|v| v.get("since").and_then(Value::as_str).and_then(parse_utc));
        if old.is_none_or(|old| old < at) {
            crate::state::fs::write_json(&file, &serde_json::json!({"since": changed}))?;
        }
        Ok(true)
    })
}

/// herdr's rank: who needs the person first.
fn rank(row: &Map<String, Value>) -> u8 {
    match row.get("activity").and_then(Value::as_str) {
        Some("blocked") => 0,
        Some("done") => 1,
        Some("working") => 2,
        _ => 3,
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
            json!({"createdAt": "2026-10-29T11:56:00.0000000Z", "model": "sonnet", "color": "magenta"}),
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
        let reply = board(&w.ctx, &caller, false, false).unwrap();
        let row = |name,
                   client,
                   pane,
                   window,
                   created,
                   doing: Value,
                   from: Value,
                   age,
                   unread,
                   model: Value,
                   color: &str| {
            json!({"name": name, "client": client, "pane": pane, "window": window, "context": "brief",
                "parent": "%1", "sessionId": SESSION_ID, "directory": "/work", "createdAt": created,
                "state": "running", "doing": doing, "doingFrom": from, "age": age, "unread": unread,
                "model": model, "color": color, "activity": "working", "since": null, "changedAt": null})
        };
        // A record without a model (the client's own) lists null; one without a color gets its name's hue.
        let want = json!({"running": 2, "agents": [
            row("worker", "claude", "%9", "@6", "2026-10-29T11:56:00.0000000Z", json!("build it"), json!("task"), 240, 2, json!("sonnet"), "magenta"),
            row("idle", "copilot", "%8", "@7", "2026-10-29T11:59:55.0000000Z", Value::Null, Value::Null, 5, 1, Value::Null, crate::paint::identity(None, "idle"))
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
        let all = board(&w.ctx, &caller, true, false).unwrap();
        assert_eq!(all["running"], 1);
        // Every ended agent of the session, lost ones included; other sessions, odd kinds and foreign records not.
        assert_eq!(all["endedTotal"], 5);
        let keys: Vec<&str> = all.keys().map(String::as_str).collect();
        assert_eq!(keys, ["running", "endedTotal", "agents"]);
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
        let plain = board(&w.ctx, &caller, false, false).unwrap();
        assert_eq!(names(&plain), ["worker=running"]);
        // Without -All the reply is unchanged: no endedTotal.
        assert!(plain.get("endedTotal").is_none());

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
        let all = board(&w.ctx, &caller, true, false).unwrap();
        // Uncapped: 12 ended, 10 rows.
        assert_eq!(all["endedTotal"], 12);
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
        // After the 16 row fields, so they keep their places; exitCode last, after it, in list -All and the bar's feed.
        for watch in [false, true] {
            let all = board(&w.ctx, &caller, true, watch).unwrap();
            let rows = all["agents"].as_array().unwrap();
            let lost = rows[0].as_object().unwrap();
            assert_eq!(lost["name"], "vanished");
            assert_eq!(lost["closedBy"], "pane");
            let keys: Vec<&str> = lost.keys().map(String::as_str).collect();
            assert_eq!(keys[keys.len() - 2..], ["closedBy", "exitCode"]);
            assert!(rows[1].get("closedBy").is_none(), "{}", rows[1]);
        }
    }

    #[test]
    fn ended_rows_carry_exit_code_in_list_and_watch() {
        let (w, caller) = ended_world();
        // list -All and the bar's feed alike: every ended row, never a live one.
        for watch in [false, true] {
            let reply = board(&w.ctx, &caller, true, watch).unwrap();
            let codes: Vec<String> = reply["agents"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| {
                    let row = row.as_object().unwrap();
                    match row.get("exitCode") {
                        // Added last, after the row's fields.
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
                ],
                "watch={watch}"
            );
        }
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
        let reply = board(&w.ctx, &caller, false, false).unwrap();
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
        let reply = board(&w.ctx, &caller, true, false).unwrap();
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
        let reply = board(&w.ctx, &caller, false, false).unwrap();
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
        let reply = board(&w.ctx, &caller, false, false).unwrap();
        let row = &reply["agents"][0];
        assert_eq!(
            (&row["doing"], &row["doingFrom"]),
            (&json!("sent"), &json!("message"))
        );
    }

    #[test]
    fn activity_is_reported_seen_or_listening_and_ranks_the_rows() {
        let w = World::human("");
        w.clock(NOW);
        let caller = w.caller();
        // Five live agents in pane order a..e.
        let ids = [
            "aaaaaaaaaaaa",
            "bbbbbbbbbbbb",
            "cccccccccccc",
            "dddddddddddd",
            "eeeeeeeeeeee",
        ];
        let names = ["a", "b", "c", "d", "e"];
        let mut folders = Vec::new();
        let mut panes = Vec::new();
        for (i, (id, name)) in ids.iter().zip(names).enumerate() {
            folders.push(w.agent_record(id, json!({"name": name, "pane": format!("%{}", 10 + i)})));
            panes.push(format!("%{}|@5|{}|{id}|{name}", 10 + i, 300 + i));
        }
        let panes: Vec<&str> = panes.iter().map(String::as_str).collect();
        w.panes(&panes);
        let report = |i: usize, state: &str, since: &str| {
            write(
                &folders[i].join("activity.json"),
                json!({"state": state, "kind": null, "since": format!("2026-10-29T{since}.0000000Z")}),
            )
        };
        let seen = |i: usize, since: &str| {
            write(
                &folders[i].join("seen.json"),
                json!({"since": format!("2026-10-29T{since}.0000000Z")}),
            )
        };
        // a: idle since 11:50, that spell seen: idle. b: idle since 11:58, only an older spell seen: done.
        // c: working since 11:59. d: no report: working (list) or listening (watch). e: blocked since 11:40.
        report(0, "idle", "11:50:00");
        seen(0, "11:50:00");
        report(1, "idle", "11:58:00");
        seen(1, "11:57:00");
        report(2, "working", "11:59:00");
        report(4, "blocked", "11:40:00");
        w.fakes.process(crate::testing::row(700, 303, 7000));
        write(
            &folders[3].join("waiting-700.json"),
            json!({"pid": 700, "start": 7000, "since": "2026-10-29T11:59:30.0000000Z"}),
        );
        let rows = |watch: bool| -> Vec<String> {
            let reply = board(&w.ctx, &caller, false, watch).unwrap();
            reply["agents"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| {
                    format!(
                        "{}={}/{}",
                        row["name"].as_str().unwrap(),
                        row["activity"],
                        row["since"]
                    )
                })
                .collect()
        };
        // Needs you first, then done, working (newest change first, then pane order), idle.
        // list and the bar's feed agree: d listens in read -Wait, so it is idle in both.
        assert_eq!(rows(false), rows(true));
        assert_eq!(
            rows(true),
            [
                r#"e="blocked"/1200"#,
                r#"b="done"/120"#,
                r#"c="working"/60"#,
                r#"a="idle"/600"#,
                r#"d="idle"/null"#
            ]
        );
        // Its own spell seen: idle; no seen file at all: done; an unreadable report counts as none.
        seen(1, "11:58:00");
        report(0, "idle", "11:50:00");
        std::fs::remove_file(folders[0].join("seen.json")).unwrap();
        std::fs::write(folders[2].join("activity.json"), "{").unwrap();
        assert_eq!(
            rows(false),
            [
                r#"e="blocked"/1200"#,
                r#"a="done"/600"#,
                r#"c="working"/null"#,
                r#"b="idle"/120"#,
                r#"d="idle"/null"#
            ]
        );
        // An unknown word counts as no report.
        report(4, "dreaming", "11:40:00");
        assert_eq!(rows(false)[0], r#"a="done"/600"#);
        // changedAt names the spell; null without a report.
        let reply = board(&w.ctx, &caller, false, false).unwrap();
        let changed: Vec<String> = reply["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| format!("{}={}", row["name"].as_str().unwrap(), row["changedAt"]))
            .collect();
        assert_eq!(
            changed,
            [
                r#"a="2026-10-29T11:50:00.0000000Z""#,
                "c=null",
                "e=null",
                r#"b="2026-10-29T11:58:00.0000000Z""#,
                "d=null"
            ]
        );
        // Two changes within one second rank by their own times, not by whole seconds: the newer first, though
        // the older comes first in pane order.
        for (i, at) in [(2, "11:59:59.8000000Z"), (0, "11:59:59.1000000Z")] {
            write(
                &folders[i].join("activity.json"),
                json!({"state": "blocked", "kind": null, "since": format!("2026-10-29T{at}")}),
            );
        }
        w.clock("2026-10-29T12:00:00.9000000Z");
        assert_eq!(&rows(false)[..2], [r#"c="blocked"/1"#, r#"a="blocked"/1"#]);
    }

    #[test]
    fn mark_seen_names_the_spell_under_the_agent_lock() {
        let w = World::human("");
        w.clock(NOW);
        let caller = w.caller();
        let folder = w.agent_record(ID, json!({}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        let file = folder.join("seen.json");
        let seen = file.clone();
        w.fakes
            .lock_probe(move |_| format!("exists={}", seen.exists()));
        w.fakes.clear_log();
        assert!(mark_seen(&w.ctx, &caller, "%9", "2026-10-29T11:58:00.0000000Z").unwrap());
        let agent = lock_key("agent", &caller, ID);
        assert_eq!(
            w.fakes.locks(),
            [
                format!("+{agent} exists=false"),
                format!("-{agent} exists=true")
            ]
        );
        let body = || std::fs::read_to_string(&file).unwrap();
        assert_eq!(body(), r#"{"since":"2026-10-29T11:58:00.0000000Z"}"#);
        // Never back to an older spell; a newer one replaces it.
        mark_seen(&w.ctx, &caller, "%9", "2026-10-29T11:50:00.0000000Z").unwrap();
        assert_eq!(body(), r#"{"since":"2026-10-29T11:58:00.0000000Z"}"#);
        mark_seen(&w.ctx, &caller, "%9", "2026-10-29T11:59:00.0000000Z").unwrap();
        assert_eq!(body(), r#"{"since":"2026-10-29T11:59:00.0000000Z"}"#);
        // No running agent on the pane, or no time: nothing written.
        assert!(!mark_seen(&w.ctx, &caller, "%4", "2026-10-29T12:00:00.0000000Z").unwrap());
        assert!(!mark_seen(&w.ctx, &caller, "%9", "soon").unwrap());
        assert_eq!(body(), r#"{"since":"2026-10-29T11:59:00.0000000Z"}"#);
    }

    #[test]
    fn watch_shows_waiting_only_for_a_live_newer_read() {
        let w = World::human("");
        w.clock(NOW);
        let caller = w.caller();
        let folder = w.agent_record(ID, json!({}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        // The agent's `read -Wait`: pid 700, started at 7000.
        w.fakes.process(crate::testing::row(700, 300, 7000));
        let wait = |name: u32, pid: u32, start: i64, since: &str| {
            write(
                &folder.join(format!("waiting-{name}.json")),
                json!({"pid": pid, "start": start, "since": format!("2026-10-29T{since}.0000000Z")}),
            )
        };
        let status = |at: &str| {
            write(
                &folder.join("status.json"),
                json!({"text": "x", "at": format!("2026-10-29T{at}.0000000Z")}),
            )
        };
        let waiting = || {
            let reply = board(&w.ctx, &caller, true, true).unwrap();
            reply["agents"][0]["waiting"].clone()
        };
        let files = || {
            let mut names: Vec<String> = std::fs::read_dir(&folder)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("waiting-"))
                .collect();
            names.sort();
            names
        };
        assert_eq!(waiting(), json!(false));
        // A live read, with no status or an older one.
        wait(700, 700, 7000, "11:59:00");
        assert_eq!(waiting(), json!(true));
        status("11:58:00");
        assert_eq!(waiting(), json!(true));
        // A status since the read began: working, and the live read's file stays.
        status("11:59:30");
        assert_eq!(waiting(), json!(false));
        assert_eq!(files(), ["waiting-700.json"]);
        std::fs::remove_file(folder.join("status.json")).unwrap();
        // The pid reused by another process (another start): not waiting, deleted.
        wait(700, 700, 6999, "11:59:00");
        assert_eq!(waiting(), json!(false));
        assert_eq!(files(), Vec::<String>::new());
        // The read was killed: its pid is gone, the file left behind is deleted.
        wait(701, 701, 7010, "11:59:00");
        assert_eq!(waiting(), json!(false));
        assert_eq!(files(), Vec::<String>::new());
        // An unreadable file, or one naming another pid than its own name, is deleted.
        std::fs::write(folder.join("waiting-702.json"), "{bad").unwrap();
        wait(703, 700, 7000, "11:59:00");
        assert_eq!(waiting(), json!(false));
        assert_eq!(files(), Vec::<String>::new());
        // Only the bar's feed says waiting; list's rows are unchanged.
        wait(700, 700, 7000, "11:59:00");
        let list = board(&w.ctx, &caller, true, false).unwrap();
        assert!(list["agents"][0].get("waiting").is_none());
        assert_eq!(files(), ["waiting-700.json"]);
    }

    #[test]
    fn watch_unread_is_the_agents_mail_in_its_parents_mailbox() {
        let w = World::human("");
        w.clock(NOW);
        let caller = w.caller();
        // worker (%9) reports to the human pane %1; child (%8) reports to worker, an agent, whose mailbox is its id.
        w.agent_record(ID, json!({}));
        w.agent_record(
            "bbbbbbbbbbbb",
            json!({"name": "child", "pane": "%8", "parentPane": "%9"}),
        );
        w.panes(&[
            "%9|@5|300|aaaaaaaaaaaa|worker",
            "%8|@5|301|bbbbbbbbbbbb|child",
        ]);
        for _ in 0..2 {
            add_mail(&w.ctx, &caller, "pane-1", "worker", "%9", "x", None).unwrap();
        }
        add_mail(&w.ctx, &caller, ID, "child", "%8", "y", None).unwrap();
        // To a sibling's mailbox: not the parent's, so no badge.
        add_mail(&w.ctx, &caller, "pane-2", "worker", "%9", "z", None).unwrap();
        // The bar's own pane (pane-7) has no mail; its reader is not used.
        let unread = || {
            let reply = board(&w.ctx, &caller, true, true).unwrap();
            reply["agents"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| format!("{}={}", r["name"].as_str().unwrap(), r["unread"]))
                .collect::<Vec<_>>()
        };
        let mail = w.state().join("mail");
        let offsets = || {
            std::fs::read_dir(&mail)
                .unwrap()
                .filter(|e| {
                    let path = e.as_ref().unwrap().path();
                    path.extension().is_some_and(|x| x == "offset")
                })
                .count()
        };
        assert_eq!(unread(), ["worker=2", "child=1"]);
        // Drawing moves no cursor.
        assert_eq!(offsets(), 0);
        assert_eq!(read_mail(&w.ctx, &caller, "pane-1").unwrap().len(), 2);
        assert_eq!(unread(), ["worker=0", "child=1"]);
        assert_eq!(read_mail(&w.ctx, &caller, ID).unwrap().len(), 1);
        assert_eq!(unread(), ["worker=0", "child=0"]);
        // list keeps the calling reader's unread: the caller's own pane-1, never the bar's parents.
        add_mail(&w.ctx, &caller, "pane-1", "worker", "%9", "w", None).unwrap();
        let list = board(&w.ctx, &caller, false, false).unwrap();
        assert_eq!(list["agents"][0]["unread"], 1);
    }

    #[test]
    fn a_dead_launch_a_failed_kill_left_is_not_running() {
        // #48 test 5: reap's conditional kill of the dead launch %9 failed (or the server kept the pane): the pane
        // is still listed dead, so it is no board row and no target.
        let w = World::human("");
        let caller = w.caller();
        w.agent_record(ID, json!({}));
        w.panes_dead(&["%9|@5|300|aaaaaaaaaaaa|worker"], &["%9"]);
        w.fakes.mux_rule("if-shell", 1, "", "no server");
        w.fakes
            .mux_rule("-t %9 #{pane_id}|#{@agent-id}", 0, "%9|aaaaaaaaaaaa\n", "");
        let reply = board(&w.ctx, &caller, true, false).unwrap();
        assert_eq!(reply["running"], 0);
        assert!(names(&reply).is_empty(), "{reply:?}");
        let killed = w
            .fakes
            .mux_calls()
            .iter()
            .filter(|c| c[1] == "if-shell")
            .count();
        assert!(killed >= 1);
        let found = registry::find_pane(&w.ctx, &caller, "worker").unwrap_err();
        assert_eq!((found.code, found.check), (2, "target"));
        assert!(!w.folder(ID).join("notice.json").exists());
    }
}
#[cfg(test)]
#[path = "board_hc_tests.rs"]
mod hc_tests;
/// Finish an ordered legacy-schema row; view is membership's validated optional Pane. No I/O.
pub fn hc_row(
    mut row: Map<String, Value>,
    o: &super::core_facts::Observation,
    view: Option<&Pane>,
) -> Map<String, Value> {
    use super::lifecycle::Life;
    let (life, state) = match o.life {
        Life::Running => ("running", "running".to_string()),
        Life::Lost => ("lost", "lost".to_string()),
        Life::Unknown => ("unknown", "unknown".to_string()),
        Life::Ended => {
            let t = o.terminal.as_ref();
            let killed = t.is_some_and(|t| t.reason == Some(crate::hc::Reason::Killed));
            let code = t.and_then(|t| t.exit_code).filter(|_| !killed);
            row.insert("exitCode".into(), code.map_or(Value::Null, Value::from));
            (
                "ended",
                if killed {
                    "stopped".into()
                } else {
                    code.map_or_else(|| "exited".into(), |n| format!("exited {n}"))
                },
            )
        }
    };
    row.insert("state".into(), state.into());
    row.insert(
        "pane".into(),
        view.map_or(Value::Null, |v| v.pane.clone().into()),
    );
    row.insert(
        "window".into(),
        view.map_or(Value::Null, |v| v.window.clone().into()),
    );
    row.insert("life".into(), life.into());
    row.insert(
        "view".into(),
        if view.is_some() {
            "attached"
        } else {
            "detached"
        }
        .into(),
    );
    row.insert("hcAddress".into(), o.identity.address.clone().into());
    row
}
