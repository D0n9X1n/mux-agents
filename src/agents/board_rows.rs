//! The side-bar feed's rows: one member's row (`Rows::row`), its activity and the hc fields (`hc_row`).
use std::collections::{BTreeMap, HashMap};
use std::time::SystemTime;

use serde_json::{Map, Value};

use super::select_doing;
use crate::agents::registry::Pane;
use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::error::Res;
use crate::state::fs::read_json;
use crate::util::{age_seconds, parse_utc};

/// Where a row's unread is counted from: the one rule `list`, `list -All`, the bar's ✉ badge and `m` share, for every
/// row (running, ended, lost or unknown; in a team or not). A leader counts its own mailbox whole (`whole`); any other
/// member the mailboxes its mail can land in, each line by `mail::badge_line`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Source {
    pub keys: Vec<String>,
    pub whole: bool,
}

/// What `Mail::source` reads from one snapshot: the running views (the agent on a parent pane), whether the session is
/// a team, each member's logical parents as the bar's view of the tree places them, and each member's own mailbox.
#[derive(Debug, Default)]
pub(crate) struct Mail {
    running: Vec<Pane>,
    team: bool,
    logical: BTreeMap<String, Vec<String>>,
    own: BTreeMap<String, String>,
    away: Option<String>,
}

impl Mail {
    pub(crate) fn of(
        ctx: &Ctx,
        caller: &Caller,
        snapshot: &crate::agents::membership::Snapshot,
        shown: &crate::agents::cascade::Shown,
    ) -> Mail {
        use crate::agents::lifecycle::Life;
        let superseded = snapshot.superseded();
        // Only a live view of a committed member owns its pane's mail (an uncommitted held subject owns none).
        let running = (snapshot.view_order.iter())
            .filter(|id| {
                snapshot.records[*id].committed
                    && !superseded.contains(*id)
                    && (snapshot.facts.local.iter())
                        .any(|o| o.identity.id == **id && o.life == Life::Running)
            })
            .map(|id| snapshot.views[id].clone())
            .collect();
        let team = super::team_leader(ctx, caller, snapshot).is_some();
        let logical = match team {
            true => (snapshot.records.keys())
                .filter_map(|id| Some((id.clone(), shown.placed(id)?)))
                .collect(),
            false => BTreeMap::new(),
        };
        let own = (snapshot.records.iter())
            .map(|(id, s)| {
                (
                    id.clone(),
                    crate::agents::mail::member_mailbox(id, &s.record),
                )
            })
            .collect();
        Mail {
            running,
            team,
            logical,
            own,
            away: crate::agents::mail::elsewhere(caller).map(String::from),
        }
    }

    /// Member `id`'s source. A3 and review: outside a team, the mailbox `send parent` reaches on its parent pane (the
    /// running agent's there, else `pane-<n>`); in a team also its logical parents' own mailboxes (calls 10, 21, 25: as
    /// the tree places it, else its raw `parentAgent.id`) and `pane-<parentPane>`, where exit notices go, so its unread
    /// holds whether the parent is attached, detached, Lost or Ended, and after its own view reopens elsewhere.
    pub(crate) fn source(&self, id: &str, record: &Value) -> Source {
        if crate::agents::tree::is_leader(record) {
            let keys = vec![crate::agents::mail::member_mailbox(id, record)];
            return Source { keys, whole: true };
        }
        let mut keys: Vec<String> = parent_key(record, &self.running, self.away.as_deref())
            .into_iter()
            .collect();
        if self.team {
            keys.extend(parent_mailbox(record));
            let raw = || {
                (record.get("parentAgent"))
                    .and_then(|p| p.get("id"))
                    .and_then(Value::as_str)
                    .map(String::from)
                    .into_iter()
                    .collect()
            };
            let parents: Vec<String> = self.logical.get(id).cloned().unwrap_or_else(raw);
            keys.extend(parents.iter().filter_map(|p| self.own.get(p).cloned()));
        }
        keys.sort();
        keys.dedup();
        Source { keys, whole: false }
    }
}

pub(super) struct Rows<'a> {
    pub(super) ctx: &'a Ctx,
    pub(super) caller: &'a Caller,
    // Per mailbox key, its unread lines (held ones included): every member's read first, in key order; any other row's
    // on demand.
    pub(super) unread: std::cell::RefCell<HashMap<String, Vec<Value>>>,
    // Where each row's unread is counted from.
    pub(super) mail: &'a Mail,
    pub(super) now: SystemTime,
    pub(super) watch: bool,
    // The team's leader on the bar's feed (B1); none outside a team and off the bar.
    pub(super) leader: Option<String>,
    // Each team member's logical parents (call 10, C0), for the ones the tree places; empty outside a team.
    pub(super) logical: &'a BTreeMap<String, Vec<String>>,
    // The members the bar marks unreadable (call 21): unreadable evidence, or on a parent loop.
    pub(super) marked: &'a [String],
}

/// V3: a member's parent pane mailbox, its recorded `parentServerKey` honoured; none for a malformed pane.
pub(crate) fn parent_mailbox(record: &Value) -> Option<String> {
    crate::agents::mail::parent_box(record).ok()
}

/// The mailbox of an agent's parent pane: the running agent's id there when the parent's recorded server is the
/// caller's (`away`, V3), else its pane mailbox (as `send parent` delivers); none for a malformed pane. Never a tag.
pub(super) fn parent_key(record: &Value, running: &[Pane], away: Option<&str>) -> Option<String> {
    let parent = crate::agents::text(record.get("parentPane"));
    let here = record.get("parentServerKey").and_then(Value::as_str) == away;
    match running.iter().find(|p| here && p.pane == parent) {
        Some(p) => Some(crate::agents::mail::member_mailbox(&p.id, &p.record)),
        None => parent_mailbox(record),
    }
}

impl Rows<'_> {
    /// Member `id`'s unread, counted from its `Mail::source`: a leader's own badge, else each line of its mailboxes
    /// `mail::badge_line` takes, the rule `m` shows, so the badge, the popup and `list` never differ.
    pub(super) fn unread_of(&self, id: &str, record: &Value) -> Res<usize> {
        let source = self.mail.source(id, record);
        if source.whole {
            return Ok(leader_badge(self.ctx, self.caller, id, record).unwrap_or(0));
        }
        let legacy = crate::agents::mail::legacy(record);
        let mut n = 0;
        for key in &source.keys {
            if !self.unread.borrow().contains_key(key) {
                let lines = crate::agents::mail::unread_lines(self.ctx, self.caller, key)?;
                self.unread.borrow_mut().insert(key.clone(), lines);
            }
            n += (self.unread.borrow()[key].iter())
                .filter(|m| crate::agents::mail::badge_line(m, id, &legacy))
                .count();
        }
        Ok(n)
    }

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
        let unread = self.unread_of(id, record)?;
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
        // After the model, so the -Text columns before it keep their places; null when spawned with no -Role.
        row.insert("role".into(), field("role"));
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
        // The bar's feed keys rows by member id (B6); in a team it adds the tree (B1). `list` keeps its columns.
        if self.watch {
            row.insert("id".into(), id.into());
            // Call 11: the stored flag counts only for a pre-lifecycle record; `needs_recovery` decides the rest.
            if !crate::agents::gens::lifecycle(record)
                && record.get("needsRecovery") == Some(&Value::Bool(true))
            {
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
/// activity.json (`blocked`, `working`; `idle` and `done` read `done` until the person has seen this very spell, which
/// seen.json names by its `since`, then `idle`); with no readable report, `idle` while it listens in `read -Wait` and
/// `working` otherwise, with no since.
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
        (Some("working"), Some(_)) => "working",
        (Some("idle" | "done"), Some(since)) => match at(seen, "since") {
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
