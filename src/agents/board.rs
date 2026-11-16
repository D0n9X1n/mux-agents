//! The side-bar feed: hc membership with optional attached views and terminal history.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Map, Value};

use super::mail::{doing_line, mailbox_key, unread_counts};
use super::registry::{self, Pane};
use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::error::Res;
use crate::state::fs::read_json;
use crate::util::parse_utc;

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

/// One reconciled hc snapshot: committed Running members ranked by activity, then Lost/Unknown by id.
/// With `all`, append up to ten terminal-Ended rows and the uncapped `endedTotal`. Terminal facts own
/// lifecycle and exit codes; notices supply only historical time and closure metadata. Unread belongs
/// to the caller's mailbox, or each member's parent mailbox in watch mode. Reap's member/notice/mail
/// locks finish before sorted mailbox reads, followed by each row's agent lock. Never refresh or reap views.
pub fn board(ctx: &Ctx, caller: &Caller, all: bool, watch: bool) -> Res<Map<String, Value>> {
    use super::{lifecycle::Life, membership::Snapshot};
    // cascade-3d C8: the bar's refresh finishes a cut-short cascade with one try of the team lock. It only displays,
    // so a busy lock or a refusal changes nothing here; `stop` and `up` refuse on the same journal.
    if watch {
        let _ = super::cascade::tick(ctx, caller);
    }
    let mut snapshot = Snapshot::take(ctx, caller)?;
    // a3b call 42 (as spawn, calls 45, 51): a lifecycle member this snapshot read as not Ended whose generation is done
    // after the reap (this reap recorded its end, or another did) is read again, so the first list after a natural
    // exit renders the recorded end. Recorded completion stays the only way out; a pre-lifecycle notice changes nothing.
    let not_done: Vec<(String, u64)> = snapshot
        .facts
        .local
        .iter()
        .filter(|o| o.life != Life::Ended)
        .filter(|o| {
            snapshot
                .records
                .get(&o.identity.id)
                .is_some_and(|s| super::gens::lifecycle(&s.record))
        })
        .filter_map(|o| Some((o.identity.id.clone(), o.identity.generation.parse().ok()?)))
        .collect();
    registry::reap_hc(ctx, caller, &snapshot)?;
    let agents = super::server_dir(ctx, caller).join("agents");
    let mut now_done = false;
    for (id, n) in &not_done {
        now_done |= super::gens::done(&agents.join(id), *n)?;
    }
    if now_done {
        snapshot = Snapshot::take(ctx, caller)?;
    }
    // A member a committed resume superseded is its successor now: it has no row of its own.
    let superseded = snapshot.superseded();
    // outcome-2 O6 (calls 11-13): each lifecycle member's recovery, once, from this snapshot (R1): its record, its active
    // generation's `active_done` and the terminal observed for it, over every local before the committed-subject filter
    // (call 12). A refusal stays on that member's row.
    let recovery: HashMap<String, Res<Option<super::gens::Recovery>>> = snapshot
        .facts
        .local
        .iter()
        .filter(|o| !superseded.contains(&o.identity.id))
        .filter(|o| super::gens::lifecycle(&snapshot.records[&o.identity.id].record))
        .map(|o| {
            let stored = &snapshot.records[&o.identity.id];
            let got = super::gens::needs_recovery(
                ctx,
                &caller.server_key,
                &o.identity.id,
                &stored.record,
                stored.active_done,
                o.terminal.as_ref(),
            );
            (o.identity.id.clone(), got)
        })
        .collect();
    let flagged = |id: &str| matches!(recovery.get(id), Some(Ok(Some(_)) | Err(_)));
    // restore-6b: each lifecycle member released for restore and waiting (display only, state-0 S6), once, from the
    // records this snapshot holds (r6b call 26). A refusal stays on that member's row.
    let waiting: HashMap<String, Res<bool>> = snapshot
        .records
        .iter()
        .filter(|(_, stored)| super::gens::lifecycle(&stored.record))
        .map(|(id, stored)| {
            let got = super::reconcile::waiting(&agents.join(id), &stored.record);
            (id.clone(), got)
        })
        .collect();
    let restoring = |id: &str| matches!(waiting.get(id), Some(Ok(true) | Err(_)));
    let leader = if watch {
        team_leader(ctx, caller, &snapshot)
    } else {
        None
    };
    let members: Vec<_> = snapshot
        .facts
        .local
        .iter()
        .filter(|o| snapshot.records[&o.identity.id].committed || flagged(&o.identity.id))
        .filter(|o| !superseded.contains(&o.identity.id))
        .collect();
    let panes: Vec<_> = snapshot
        .view_order
        .iter()
        .filter(|id| {
            members
                .iter()
                .any(|o| &o.identity.id == *id && o.life == Life::Running)
                // An uncommitted held subject listed for its recovery owns no pane.
                && snapshot.records[*id].committed
        })
        .map(|id| snapshot.views[id].clone())
        .collect();
    let mine: Vec<String> = panes
        .iter()
        .filter(|p| p.pane == caller.pane)
        .map(|p| p.id.clone())
        .collect();
    let reader = reader_key(caller.member.as_deref(), &mine, &caller.pane)?;
    // A member reads the mailbox its record names (inherited by a resume), else its id.
    let reader = match snapshot.records.get(&reader) {
        Some(stored) => super::mail::member_mailbox(&reader, &stored.record),
        None => reader,
    };
    // #103 A3 and review: in a team, a member's mail reaches its parent in every mailbox it can land in: the parent's own
    // (from `parentAgent.id` and its record, an inherited `mailbox` honoured), the one `send parent` reaches on the parent
    // pane (the running agent's there, else `pane-<n>`), and `pane-<parentPane>`, where exit notices go. A team row sums
    // them, de-duplicated, by member identity, so its unread holds whether the parent is attached, detached, Lost or
    // Ended, and after the member's own view reopens on another pane. Outside a team: the parent pane, as before.
    // Calls 10, 21, 25: in a team each member's logical parents (C0) from the bar's view of the tree, which never
    // refuses; a member it does not place (unreadable, on a loop, or any member unreadable) keeps its raw parent id.
    let (logical, marked) = match &leader {
        Some(_) => {
            let shown = super::cascade::Tree::shown(&super::team_state::Tx {
                ctx,
                scope: super::team_state::Scope::of(caller),
            });
            let logical: BTreeMap<String, Vec<String>> = (members.iter())
                .filter_map(|o| Some((o.identity.id.clone(), shown.placed(&o.identity.id)?)))
                .collect();
            let marked: Vec<String> = (members.iter())
                .filter(|o| shown.bad(&o.identity.id))
                .map(|o| o.identity.id.clone())
                .collect();
            (logical, marked)
        }
        None => (BTreeMap::new(), Vec::new()),
    };
    let parents: HashMap<String, Vec<String>> = if leader.is_some() {
        members
            .iter()
            .map(|o| {
                let record = &snapshot.records[&o.identity.id].record;
                let raw = record
                    .get("parentAgent")
                    .and_then(|p| p.get("id"))
                    .and_then(Value::as_str)
                    .map(String::from);
                let ids = match logical.get(&o.identity.id) {
                    Some(ps) => ps.clone(),
                    None => raw.into_iter().collect(),
                };
                let own = ids.into_iter().filter_map(|parent| {
                    let stored = snapshot.records.get(&parent)?;
                    Some(super::mail::member_mailbox(&parent, &stored.record))
                });
                let notices = mailbox_key(&super::text(record.get("parentPane")), "").ok();
                let mut boxes: Vec<String> = own
                    .chain([parent_key(record, &panes), notices].into_iter().flatten())
                    .collect();
                boxes.sort();
                boxes.dedup();
                (o.identity.id.clone(), boxes)
            })
            .collect()
    } else {
        HashMap::new()
    };
    let mut keys: Vec<_> = if watch {
        members
            .iter()
            .filter(|o| o.life == Life::Running || leader.is_some())
            .flat_map(|o| match parents.get(&o.identity.id) {
                Some(boxes) => boxes.clone(),
                None => parent_key(&snapshot.records[&o.identity.id].record, &panes)
                    .into_iter()
                    .collect(),
            })
            .collect()
    } else {
        vec![reader.clone()]
    };
    keys.sort();
    keys.dedup();
    let mut unread = HashMap::new();
    for key in keys {
        unread.insert(key.clone(), unread_counts(ctx, caller, &key)?);
    }
    let rows = Rows {
        ctx,
        caller,
        unread,
        reader: (!watch).then_some(reader),
        running: &panes,
        now: ctx.clock.now(),
        watch,
        leader: leader.clone(),
        parents: &parents,
        logical: &logical,
        marked: &marked,
    };
    let mut running = Vec::new();
    let mut other = Vec::new();
    let mut ended = Vec::new();
    for o in members {
        if o.life == Life::Ended && !all && !flagged(&o.identity.id) && !restoring(&o.identity.id) {
            continue;
        }
        let id = &o.identity.id;
        let notice = if o.life == Life::Running {
            None
        } else if super::gens::lifecycle(&snapshot.records[id].record) {
            snapshot.records[id].notice.clone() // R1: read once in membership (Task 2)
        } else {
            read_json(
                &super::server_dir(ctx, caller)
                    .join("agents")
                    .join(id)
                    .join("notice.json"),
            )?
        };
        if o.life == Life::Ended {
            let at = notice
                .as_ref()
                .and_then(|n| n.get("at"))
                .and_then(Value::as_str)
                .and_then(parse_utc);
            ended.push((at, o, notice));
        } else if o.life == Life::Running {
            running.push(o);
        } else {
            other.push((o, notice));
        }
    }
    let ended_total = ended.len();
    ended.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.identity.id.cmp(&b.1.identity.id))
    });
    running.sort_by_key(|o| {
        (
            snapshot
                .view_order
                .iter()
                .position(|id| id == &o.identity.id)
                .unwrap_or(usize::MAX),
            o.identity.id.clone(),
        )
    });
    other.sort_by(|a, b| a.0.identity.id.cmp(&b.0.identity.id));
    let make =
        |o: &super::core_facts::Observation, notice: Option<&Value>| -> Res<Map<String, Value>> {
            let id = &o.identity.id;
            let record = &snapshot.records[id].record;
            let live = o.life == Life::Running;
            let mut row = rows.row(
                id,
                record,
                super::text(record.get("pane")),
                &super::text(record.get("window")),
                if live { "running" } else { "unknown" },
            )?;
            // Call 18a: the selected identity's session (a held one's is its pending generation's), in place.
            row.insert("sessionId".into(), o.identity.session_id.clone().into());
            // outcome-2 O6: the one predicate's answer, on this row only.
            match recovery.get(id) {
                Some(Ok(Some(r))) => {
                    row.insert("needsRecovery".into(), true.into());
                    row.insert("recoveryReason".into(), r.reason.clone().into());
                    row.insert("recoveryRunning".into(), r.running.into());
                }
                Some(Err(e)) => {
                    row.insert("recoveryError".into(), e.check.into());
                }
                _ => {}
            }
            match waiting.get(id) {
                Some(Ok(true)) => {
                    row.insert("restore".into(), "waiting".into());
                }
                Some(Err(e)) => {
                    row.insert("restoreError".into(), e.check.into());
                }
                _ => {}
            }
            if !live {
                if let Some(closed) = notice
                    .and_then(|n| n.get("closedBy"))
                    .and_then(Value::as_str)
                {
                    row.insert("closedBy".into(), closed.into());
                }
                row.insert("exitCode".into(), Value::Null);
            }
            // `list -All` offers the command that starts an ended member again; the bar's feed does not.
            if all && !watch && o.life == Life::Ended {
                row.insert("resume".into(), format!("mux-agent resume {id}").into());
            }
            Ok(hc_row(row, o, snapshot.views.get(id)))
        };
    let count = running.len();
    let mut agents = running
        .into_iter()
        .map(|o| make(o, None))
        .collect::<Res<Vec<_>>>()?;
    agents.sort_by_key(|row| {
        let changed = row
            .get("changedAt")
            .and_then(Value::as_str)
            .and_then(parse_utc);
        (rank(row), changed.is_none(), std::cmp::Reverse(changed))
    });
    for (o, notice) in other {
        agents.push(make(o, notice.as_ref())?);
    }
    // Call 13: a member that needs recovery, or whose recovery could not be decided, is always listed.
    let cap = if leader.is_some() {
        usize::MAX
    } else {
        ENDED_MAX
    };
    let mut kept = 0;
    for (_, o, notice) in ended {
        if !flagged(&o.identity.id) && !restoring(&o.identity.id) {
            if kept == cap {
                continue;
            }
            kept += 1;
        }
        agents.push(make(o, notice.as_ref())?);
    }
    let mut reply = Map::new();
    reply.insert("running".into(), count.into());
    if all {
        reply.insert("endedTotal".into(), ended_total.into());
    }
    reply.insert(
        "agents".into(),
        Value::Array(agents.into_iter().map(Value::Object).collect()),
    );
    if leader.is_some() {
        reply.insert("team".into(), true.into());
    }
    Ok(reply)
}

/// Ended agents a board (`list -All`) shows, newest first.
pub(crate) const ENDED_MAX: usize = 10;

/// The mailbox of an agent's parent pane: the running agent's id there, else `pane-<n>` (as `send parent`
/// delivers); none for a malformed pane. Never a tag.
fn parent_key(record: &Value, running: &[Pane]) -> Option<String> {
    let parent = super::text(record.get("parentPane"));
    match running.iter().find(|p| p.pane == parent) {
        Some(p) => Some(super::mail::member_mailbox(&p.id, &p.record)),
        None => mailbox_key(&parent, "").ok(),
    }
}

/// Records the viewed done spell `changed` under the agent lock, never moving seen.json backward.
/// Requires a unique committed attachment on `pane`; historical notices do not invalidate view proof.
pub fn mark_seen(ctx: &Ctx, caller: &Caller, pane: &str, changed: &str) -> Res<bool> {
    let Some(at) = parse_utc(changed) else {
        return Ok(false);
    };
    let snapshot = registry::Snapshot::take(ctx, caller)?;
    let Some(agent) = snapshot.view_at(pane) else {
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

/// The rank of herdr: who needs the person first; on call below needs-you, above done and working (W6).
fn rank(row: &Map<String, Value>) -> u8 {
    match row.get("activity").and_then(Value::as_str) {
        Some("blocked") => 0,
        Some("on call") => 1,
        Some("done") => 2,
        Some("working") => 3,
        _ => 4,
    }
}

/// The mailbox whose unread `list` counts for the caller. A member proven through its own hc terminal counts its own
/// id, always: whichever pane shows it, and even when a later hc look cannot show it running, it is never matched by
/// pane, so it can never count another member's mail (#104 phase 1c). Any other caller: the one running agent on its
/// pane, else the pane's own mailbox.
pub(crate) fn reader_key(member: Option<&str>, on_pane: &[String], pane: &str) -> Res<String> {
    match (member, on_pane) {
        (Some(id), _) => Ok(id.to_string()),
        (None, [id]) => Ok(id.clone()),
        _ => mailbox_key(pane, ""),
    }
}

/// The leader of the team the bar's session is (#103 B1): its team record's, else a committed depth-0 member with
/// `upTeam` (a team whose record write failed); none outside a team. An unreadable record never fails the bar.
fn team_leader(
    ctx: &Ctx,
    caller: &Caller,
    snapshot: &super::membership::Snapshot,
) -> Option<String> {
    let file = super::team::path(&super::server_dir(ctx, caller), &caller.session);
    if let Ok(Some(team)) = super::team::read(&file) {
        return Some(team.leader);
    }
    snapshot
        .records
        .iter()
        .find(|(_, s)| {
            s.committed
                && s.record.get("depth").and_then(Value::as_u64) == Some(0)
                && s.record.get("upTeam").and_then(Value::as_str).is_some()
        })
        .map(|(id, _)| id.clone())
}

#[path = "board_rows.rs"]
mod rows;
pub use rows::hc_row;
use rows::*;

#[cfg(test)]
#[path = "board_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "board_hc_tests.rs"]
mod hc_tests;
