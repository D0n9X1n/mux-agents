//! reentry-6a E6 replay: the team's leader at the anchor (ambiguity 9, calls 7, 36; F13).
use serde_json::Value;

use super::{index, replay};
use crate::agents::reconcile::{Pass, PassFor, anchor_caller, pass};
use crate::{
    agents::gens::{self, Member},
    caller::Caller,
    ctx::Ctx,
    error::{Refusal, Res, refuse},
};

const UNREADABLE: &str = "the leader's record cannot be read; the team record cannot be repaired";

/// Whether `record` is this team's leader record for folder `id` in the anchor's session (calls 7, 36).
fn of_team(record: &Value, ix: &index::Index, session: &str, id: &str) -> bool {
    record.get("upTeam").and_then(Value::as_str) == Some(ix.team.as_str())
        && record.get("session").and_then(Value::as_str) == Some(session)
        && record.get("id").and_then(Value::as_str) == Some(id)
}

/// Ambiguity 9, call 7: the non-void leader of T at the anchor, in the anchor's session, by group B's full check;
/// a pre-lifecycle one keeps today's refusal; an unreadable record there: 1 `team` (E4.2); two: 1 `index`.
pub(crate) fn leader_of(ctx: &Ctx, ix: &index::Index) -> Res<(replay::Leader, Option<String>)> {
    let Some(a) = &ix.anchor else {
        return Ok((replay::Leader::Absent, None));
    };
    let mut found = Vec::new();
    for (id, _, member) in gens::members(&ctx.state_root.join(&a.key).join("agents"))? {
        let record = match &member {
            Member::Absent => continue,
            Member::Unreadable(_) => return refuse(1, "team", UNREADABLE),
            Member::Pre(r) => r,
            Member::Life(g) => g.record(),
        };
        if !of_team(record, ix, &a.session, &id) {
            continue;
        }
        let Member::Life(g) = &member else {
            return refuse(2, "leader", "the team has a leader from an earlier version");
        };
        let s = g.status()?;
        if g.never_ran_recorded(&s)? {
            continue; // void: its folder stays as evidence, counted by no reader
        }
        let held = if g.pending().is_some() {
            replay::Leader::Pending
        } else {
            replay::Leader::Active
        };
        found.push((held, id));
    }
    match found.len() {
        0 => Ok((replay::Leader::Absent, None)),
        1 => Ok(found.pop().map(|(l, id)| (l, Some(id))).expect("one")),
        _ => Err(index::bad("two leaders claim this team")),
    }
}

/// Ambiguity 11: settle-1b G3 `normal` on the leader; a `pending` that stays is 1 `team`.
pub(crate) fn settle_leader(ctx: &Ctx, ix: &index::Index, id: &str) -> Res<()> {
    let a = ix
        .anchor
        .as_ref()
        .ok_or_else(|| Refusal::internal("no anchor"))?;
    let scope = crate::agents::team_state::Scope {
        key: a.key.clone(),
        session: a.session.clone(),
    };
    crate::agents::team_state::open(ctx, &scope, |tx| {
        tx.settle(id, crate::agents::settle::Mode::Normal)
            .map(|_| ())
    })?;
    if leader_of(ctx, ix)?.0 == replay::Leader::Pending {
        return refuse(1, "team", "the leader's launch is unresolved; retry");
    }
    Ok(())
}

/// The leader's current record at the anchor, reread after reconcile so a restored leader's address is current;
/// F13: its id, `upTeam` and the anchor's session are checked again at this read.
pub(crate) fn leader_record(ctx: &Ctx, ix: &index::Index, id: &str) -> Res<Value> {
    let a = ix
        .anchor
        .as_ref()
        .ok_or_else(|| Refusal::internal("no anchor"))?;
    match gens::member_of(&ctx.state_root.join(&a.key).join("agents").join(id)) {
        Member::Life(g) if of_team(g.record(), ix, &a.session, id) => Ok(g.record().clone()),
        _ => refuse(1, "team", UNREADABLE),
    }
}

/// restore-6c Task 7 (call 18, ambiguity 10): reconcile's pass at the anchor for `up`, with 6a's validated leader
/// (call 17), before `reenter_arm` (6a call 8's order). Task 8 gives it the leader step; Task 10 replaces the refusal.
pub(crate) fn reconciled(
    ctx: &Ctx,
    caller: &Caller,
    ix: &index::Index,
    leader: Option<&str>,
) -> Res<serde_json::Map<String, Value>> {
    let a = ix
        .anchor
        .clone()
        .ok_or_else(|| Refusal::internal("no anchor"))?;
    let scope = crate::agents::team_state::Scope {
        key: a.key,
        session: a.session,
    };
    let who = PassFor::Up { leader, step: None };
    match pass(ctx, &anchor_caller(caller, &scope), who)? {
        Pass::Done(m) => Ok(m),
        Pass::Stopped => refuse(1, "stopping", "the team's leader is stopped"),
    }
}
