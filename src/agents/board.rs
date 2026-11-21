//! The side-bar feed: hc membership with optional attached views and terminal history.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Map, Value};

use super::mail::{doing_line, unread_lines};
use super::registry;
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
/// lifecycle and exit codes; notices supply only historical time and closure metadata. Each row's unread
/// is counted from its `Mail::source`, as `m` and `list` count it. Reap's member/notice/mail
/// locks finish before sorted mailbox reads, followed by each row's agent lock. Never refresh or reap views.
pub fn board(ctx: &Ctx, caller: &Caller, all: bool, watch: bool) -> Res<Map<String, Value>> {
    timed(ctx, caller, all, watch).map(|read| read.board)
}

/// The feed's elapsed-seconds origin is captured where rows calculate their ages, not when slow I/O returns.
/// End timestamps are kept outside the public board shape so the bar can preserve subsecond rounding too.
#[derive(Debug)]
pub(crate) struct Timed {
    pub board: Map<String, Value>,
    pub at: std::time::SystemTime,
    pub ended: BTreeMap<String, std::time::SystemTime>,
}

pub(crate) fn timed(ctx: &Ctx, caller: &Caller, all: bool, watch: bool) -> Res<Timed> {
    use super::{lifecycle::Life, membership::Snapshot};
    let scope = super::team_state::Scope::of(caller);
    // cascade-3d C8, adopt-3e call 4: the bar's one try per refresh (cascade journals, then adoption); it only
    // displays, so a busy lock or a refusal changes nothing here. `list`: a sweep under a Tx only when a candidate
    // exists, before this reap, so an end this reap records is adopted on the next refresh (ambiguity 12). An
    // unreadable tree is no candidate here: the board never refuses for it.
    let blocked = if watch {
        (super::adopt::tick(ctx, caller).ok().flatten()).map_or_else(Vec::new, |s| s.blocked)
    } else if !(super::adopt::candidates(ctx, &scope).unwrap_or_default()).is_empty() {
        super::team_state::open(ctx, &scope, super::adopt::sweep)?.blocked
    } else {
        Vec::new()
    };
    let (mut snapshot, mut shown) = steady(ctx, &scope, watch, || Snapshot::take(ctx, caller))?;
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
        (snapshot, shown) = steady(ctx, &scope, watch, || Snapshot::take(ctx, caller))?;
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
                &super::scope::state(caller).key,
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
    let (logical, marked) = match &leader {
        Some(_) => {
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
    // Every reader counts a row's unread from its one source (`Mail::source`): `list`, `list -All`, the bar and `m`.
    let mail = Mail::of(ctx, caller, &snapshot, &shown);
    let mut keys: Vec<String> = (members.iter())
        .map(|o| mail.source(&o.identity.id, &snapshot.records[&o.identity.id].record))
        .filter(|source| !source.whole)
        .flat_map(|source| source.keys)
        .collect();
    keys.sort();
    keys.dedup();
    let mut unread = HashMap::new();
    for key in keys {
        unread.insert(key.clone(), unread_lines(ctx, caller, &key)?);
    }
    let rows = Rows {
        ctx,
        caller,
        unread: std::cell::RefCell::new(unread),
        mail: &mail,
        now: ctx.clock.now(),
        watch,
        leader: leader.clone(),
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
    let now = ctx.clock.now();
    let ended_at = ended
        .iter()
        .filter_map(|(at, o, _)| at.map(|at| (o.identity.id.clone(), at)))
        .collect();
    let ago = |at: std::time::SystemTime| now.duration_since(at).map_or(0, |d| d.as_secs());
    // `ended`: when an Ended member ended (its notice's `at`), for the bar's `endedAgo`.
    let make = |o: &super::core_facts::Observation,
                notice: Option<&Value>,
                ended: Option<std::time::SystemTime>|
     -> Res<Map<String, Value>> {
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
        // The bar's open fold judges an ended agent by when it ended, not when it started.
        if let (true, Some(at)) = (watch, ended) {
            row.insert("endedAgo".into(), ago(at).into());
        }
        Ok(super::ping::last(hc_row(row, o, snapshot.views.get(id))))
    };
    let mut agents = running
        .into_iter()
        .map(|o| make(o, None, None))
        .collect::<Res<Vec<_>>>()?;
    // A 0.2.x pane-based member still running after the upgrade is listed as 0.2.8 listed it, so a person sees it.
    for (p, record, window) in super::prelife::live_panes(ctx, caller) {
        agents.push(super::ping::last(
            rows.row(&p.id, &record, p.pane, &window, "running")?,
        ));
    }
    let count = agents.len();
    agents.sort_by_key(|row| {
        let changed = row
            .get("changedAt")
            .and_then(Value::as_str)
            .and_then(parse_utc);
        (rank(row), changed.is_none(), std::cmp::Reverse(changed))
    });
    for (o, notice) in other {
        agents.push(make(o, notice.as_ref(), None)?);
    }
    // Call 13: a member that needs recovery, or whose recovery could not be decided, is always listed. The bar's feed
    // also keeps every agent that ended within `ENDED_RECENT`, so each one its open fold lists can be selected; only
    // the older ones are capped, and each ended row says how long ago it ended (`endedAgo`, from its notice's `at`).
    let cap = if leader.is_some() {
        usize::MAX
    } else {
        ENDED_MAX
    };
    let mut kept = 0;
    for (at, o, notice) in ended {
        let recent = watch && at.is_some_and(|at| ago(at) < ENDED_RECENT);
        // On the bar, a member that left a report unread stands in Inbox, so the cap never drops it.
        let mailed = || {
            let record = &snapshot.records[&o.identity.id].record;
            watch && matches!(rows.unread_of(&o.identity.id, record), Ok(n) if n > 0)
        };
        if !flagged(&o.identity.id) && !restoring(&o.identity.id) && !recent && !mailed() {
            if kept == cap {
                continue;
            }
            kept += 1;
        }
        agents.push(make(o, notice.as_ref(), at)?);
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
    let listed = lists_kept(watch, snapshot.records.values().map(|s| &s.record))
        .then(|| super::kept::watched(ctx, caller)); // restore-6c Task 11: its predecessors' too
    add_kept(&mut reply, listed);
    // Call 20: `list` rows carry no `id`, so each source is also found by its recorded `hcAddress`.
    let blocked: Vec<Blocked> = (blocked.into_iter())
        .map(|(id, why)| {
            let address = (snapshot.records.get(&id))
                .and_then(|s| s.record.get("hcAddress"))
                .and_then(Value::as_str)
                .map(String::from);
            (id, address, why)
        })
        .collect();
    note_blocked(&mut reply, &blocked);
    Ok(Timed {
        board: reply,
        at: rows.now,
        ended: ended_at,
    })
}

/// views-6f ambiguity 14: a watch of a team that has a lifecycle member (only a lifecycle generation is ever kept).
fn lists_kept<'a>(watch: bool, records: impl IntoIterator<Item = &'a Value>) -> bool {
    watch && records.into_iter().any(super::gens::lifecycle)
}

/// The watch feed's kept shells; a failed listing is shown, never fatal (display only, state-0 S6).
fn add_kept(reply: &mut Map<String, Value>, listed: Option<Res<Vec<super::kept::Shell>>>) {
    match listed {
        None => {}
        Some(Ok(shells)) => {
            reply.insert("kept".into(), super::kept::rows(&shells));
        }
        Some(Err(e)) => {
            reply.insert("kept".into(), Value::Array(Vec::new()));
            reply.insert("keptError".into(), e.message.into());
        }
    }
}

/// Test only: what runs as `steady`'s bracket closes.
#[cfg(test)]
type AfterHook = fn(&Ctx, &super::team_state::Scope);
#[cfg(test)]
thread_local! {
    static AFTER: std::cell::Cell<Option<AfterHook>> = const { std::cell::Cell::new(None) };
}
/// Test only: a hook run as `steady`'s bracket closes (call 13).
#[cfg(test)]
pub(crate) fn after_bracket(h: Option<AfterHook>) {
    AFTER.with(|c| c.set(h));
}
/// A6, call 13: the snapshot and the bar's view of the tree (`Tree::shown`, which never refuses) read inside one
/// `stable` bracket. Still moving: `list` replays under a Tx and reads once more; the bar has had its one try and
/// errs, keeping its frame.
fn steady<T>(
    ctx: &Ctx,
    scope: &super::team_state::Scope,
    watch: bool,
    mut take: impl FnMut() -> Res<T>,
) -> Res<(T, super::cascade::Shown)> {
    let tx = super::team_state::Tx {
        ctx,
        scope: scope.clone(),
    };
    let mut both = || Ok((take()?, super::cascade::Tree::shown(&tx)));
    let got = match super::adopt::stable(ctx, scope, &mut both) {
        Err(e) if !watch && e.check == "topology" && e.message == super::adopt::MOVING => {
            super::team_state::open(ctx, scope, |_| Ok(()))?;
            super::adopt::stable(ctx, scope, &mut both)
        }
        other => other,
    }?;
    #[cfg(test)]
    if let Some(h) = AFTER.with(|c| c.get()) {
        h(ctx, scope);
    }
    Ok(got)
}
/// A blocked source: its id, its recorded `hcAddress`, the reason.
type Blocked = (String, Option<String>, &'static str);
/// Call 9, call 20: each blocked source's row says why (`adoption`): the bar's rows by `id`, `list`'s by `hcAddress`.
fn note_blocked(reply: &mut Map<String, Value>, blocked: &[Blocked]) {
    let Some(rows) = reply.get_mut("agents").and_then(Value::as_array_mut) else {
        return;
    };
    for row in rows {
        let field = |k: &str| row.get(k).and_then(Value::as_str).map(String::from);
        let (id, address) = (field("id"), field("hcAddress"));
        let hit = blocked.iter().find(|(b, at, _)| {
            id.as_deref() == Some(b.as_str())
                || (id.is_none() && address.is_some() && *at == address)
        });
        if let Some((_, _, why)) = hit {
            row["adoption"] = Value::from(*why);
        }
    }
}

/// Ended agents a board (`list -All`) shows, newest first; the bar's feed shows as many older than `ENDED_RECENT`.
pub(crate) const ENDED_MAX: usize = 10;

/// An agent that ended less than this many seconds ago is recent: the bar's feed always carries it, and its open
/// ended fold lists it; the older ones fold into `+N older`.
pub(crate) const ENDED_RECENT: u64 = 3600;

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
    seen_at(ctx, caller, &agent.id, changed, at)
}

/// `mark_seen` of member `id` itself, for a done member with no view (the side bar's Space): no pane to resolve, so
/// only an agent id whose member has a record here is marked. Same lock, same never-backward rule.
pub fn mark_seen_id(ctx: &Ctx, caller: &Caller, id: &str, changed: &str) -> Res<bool> {
    let Some(at) = parse_utc(changed) else {
        return Ok(false);
    };
    if !super::agent_id(id) {
        return Ok(false);
    }
    let member = super::server_dir(ctx, caller).join("agents").join(id);
    if !member.join("record.json").is_file() {
        return Ok(false);
    }
    seen_at(ctx, caller, id, changed, at)
}

/// Writes `seen.json` of member `id` as `changed` (parsed: `at`) under its agent lock, unless it already names a
/// spell as new or newer.
fn seen_at(
    ctx: &Ctx,
    caller: &Caller,
    id: &str,
    changed: &str,
    at: std::time::SystemTime,
) -> Res<bool> {
    let file = super::state_dir(ctx, caller)?
        .join("agents")
        .join(id)
        .join("seen.json");
    super::lock(ctx, &super::lock_key("agent", caller, id), || {
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

/// The leader of the team the bar's session is (B1): its team record's, else a committed depth-0 member with
/// `upTeam` (a team whose record write failed); none outside a team. An unreadable record never fails the bar.
fn team_leader(
    ctx: &Ctx,
    caller: &Caller,
    snapshot: &super::membership::Snapshot,
) -> Option<String> {
    let file = super::team::path(
        &super::server_dir(ctx, caller),
        &super::scope::state(caller).session,
    );
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
pub(crate) use rows::Mail;
pub use rows::hc_row;
#[cfg(test)]
pub(crate) use rows::parent_mailbox;
use rows::*;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod hc_tests;

#[cfg(test)]
mod adopt_tests;
