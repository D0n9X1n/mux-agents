//! The side-bar feed's rows: one member's row (`Rows::row`), its activity and the hc fields (`hc_row`).
use std::collections::{BTreeMap, HashMap};
use std::time::SystemTime;

use serde_json::{Map, Value};

use super::{parent_key, select_doing};
use crate::agents::registry::Pane;
use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::error::Res;
use crate::state::fs::read_json;
use crate::util::{age_seconds, parse_utc};

pub(super) struct Rows<'a> {
    pub(super) ctx: &'a Ctx,
    pub(super) caller: &'a Caller,
    // Per mailbox key, its unread counts by `<from>|<pane>`.
    pub(super) unread: HashMap<String, HashMap<String, usize>>,
    // The one mailbox every row counts in; none in watch mode, where each row counts in its parent's.
    pub(super) reader: Option<String>,
    pub(super) running: &'a [Pane],
    pub(super) now: SystemTime,
    pub(super) watch: bool,
    // The team's leader on the bar's feed (#103 B1); none outside a team and off the bar.
    pub(super) leader: Option<String>,
    // Each team member's parent mailboxes (#103 A3 and review); empty outside a team.
    pub(super) parents: &'a HashMap<String, Vec<String>>,
    // Each team member's logical parents (call 10, C0), for the ones the tree places; empty outside a team.
    pub(super) logical: &'a BTreeMap<String, Vec<String>>,
    // The members the bar marks unreadable (call 21): unreadable evidence, or on a parent loop.
    pub(super) marked: &'a [String],
}

impl Rows<'_> {
    /// One row. status.json and last-sent.json are read under the agent lock their writers hold; an unreadable
    /// one is skipped. Doing never comes from the screen.
    pub(super) fn row(
        &self,
        id: &str,
        record: &Value,
        pane: String,
        window: &str,
        state: &str,
    ) -> Res<Map<String, Value>> {
        let folder = crate::agents::server_dir(self.ctx, self.caller)
            .join("agents")
            .join(id);
        let live = state == "running";
        let (status, sent, waiting, reported, seen, on) = crate::agents::lock(
            self.ctx,
            &crate::agents::lock_key("agent", self.caller, id),
            || {
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
                    crate::agents::message::waiting_now(self.ctx, &folder, at)
                });
                // W6: a live lease of the current owner; never the leader, whose unread is its badge.
                let on = live
                    && !crate::agents::tree::is_leader(record)
                    && crate::agents::message::on_call(self.ctx, &folder);
                Ok((status, read("last-sent.json"), waiting, reported, seen, on))
            },
        )?;
        let (doing, from) = select_doing(
            status.as_ref().and_then(|s| s.get("text")),
            sent.as_ref().and_then(|s| s.get("line")),
            record.get("taskLine"),
        );
        let field = |name: &str| record.get(name).cloned().unwrap_or(Value::Null);
        let name = crate::agents::text(record.get("name"));
        let age = record
            .get("createdAt")
            .and_then(Value::as_str)
            .and_then(|created| age_seconds(created, self.now));
        let legacy = format!("{name}|{pane}");
        let unread = match (&self.reader, self.parents.get(id)) {
            (Some(reader), _) => self
                .unread
                .get(reader)
                .and_then(|counts| counts.get(&legacy))
                .copied()
                .unwrap_or(0),
            // A team row (#103 review): every parent mailbox, de-duplicated; the lines naming this member (`#<id>`), and
            // the lines naming no member by name and pane (`<from>|<pane>` less those that name one, `!<from>|<pane>`).
            (None, Some(boxes)) => boxes
                .iter()
                .filter_map(|key| self.unread.get(key))
                .map(|counts| {
                    let got = |k: &str| counts.get(k).copied().unwrap_or(0);
                    got(&format!("#{id}")) + got(&legacy).saturating_sub(got(&format!("!{legacy}")))
                })
                .sum(),
            (None, None) => parent_key(record, self.running)
                .and_then(|key| self.unread.get(&key))
                .and_then(|counts| counts.get(&legacy))
                .copied()
                .unwrap_or(0),
        };
        let unread = leader_badge(self.ctx, self.caller, id, record).unwrap_or(unread);
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
            let activity = listening(activity, on);
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
        // The bar's feed keys rows by member id (#103 B6); in a team it adds the tree (B1). `list` keeps its columns.
        if self.watch {
            row.insert("id".into(), id.into());
            if record.get("needsRecovery") == Some(&Value::Bool(true)) {
                row.insert("needsRecovery".into(), true.into());
            }
            if let Some(leader) = &self.leader {
                let parent = match self.logical.get(id) {
                    Some(ps) => ps.first().cloned().map(Value::from),
                    None => record.get("parentAgent").and_then(|p| p.get("id")).cloned(),
                };
                row.insert("parentId".into(), parent.unwrap_or(Value::Null));
                if self.marked.iter().any(|m| m == id) {
                    row.insert("unreadable".into(), true.into());
                }
                row.insert(
                    "depth".into(),
                    record.get("depth").cloned().unwrap_or(Value::Null),
                );
                row.insert("leader".into(), (id == leader).into());
            }
        }
        Ok(row)
    }
}

/// W6: a live lease of the current owner shows `on call`, below needs-you, above working and done.
pub(super) fn listening(word: &'static str, on_call: bool) -> &'static str {
    if on_call && word != "blocked" {
        "on call"
    } else {
        word
    }
}

/// W6: the leader is never on call; its unread is the badge of its own mailbox (unread plus held).
pub(crate) fn leader_badge(ctx: &Ctx, caller: &Caller, id: &str, record: &Value) -> Option<usize> {
    crate::agents::tree::is_leader(record)
        .then(|| {
            crate::agents::mail::counts(
                ctx,
                caller,
                &crate::agents::mail::member_mailbox(id, record),
            )
            .ok()
        })
        .flatten()
        .map(|c| c.badge)
}

/// A live agent's activity and the seconds since it last changed, as herdr names them: what its client reported in
/// activity.json (`blocked`, `working`; `idle` is `done` until the person has looked at this very idle spell, which
/// seen.json names by its `since`); with no readable report, `idle` while it listens in `read -Wait` and `working`
/// otherwise, with no since.
pub(super) fn activity(
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
        (Some("done"), Some(_)) => "done",
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

/// Finish an ordered legacy-schema row; view is membership's validated optional Pane. No I/O.
pub fn hc_row(
    mut row: Map<String, Value>,
    o: &crate::agents::core_facts::Observation,
    view: Option<&Pane>,
) -> Map<String, Value> {
    use crate::agents::lifecycle::Life;
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
