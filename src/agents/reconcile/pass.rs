//! restore-6c Task 7 (#103): reconcile's one pass at the anchor. `restore` keeps restore-6b's meaning; `up` reports
//! every step refusal and takes reentry-6a's validated leader (calls 17, 18, 25).
use super::{Reply, Seen, decide, failed, judged, observe, session_members};
use crate::agents::gens::{self, Gen, Member};
use crate::agents::leader::LeaderStep;
use crate::agents::settle::{Mode, Outcome};
use crate::agents::team_state::Tx;
use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::error::{Refusal, Res};
use crate::hc::{Lazy, Terminal};
use serde_json::{Map, Value, json};

/// Who runs the pass: `restore` (restore-6b), or `up` (6c P4) with 6a's validated leader (call 17).
pub(crate) enum PassFor<'a> {
    Restore,
    Up {
        leader: Option<&'a str>,
        step: Option<LeaderStep>,
    },
}

pub(crate) enum Pass {
    /// P6: the leader is desired `stopped` (read after the cascade finish, before any settle).
    Stopped,
    Done(Map<String, Value>),
}

/// 6b L2 at the anchor in one Tx. For `up` (call 18) a step refusal is reported, never returned: `adoptReplayError`,
/// `cascadeError`, `stoppedError`, `settleError`, `leaderResume.refused`, `membersError`. Only the lock, a crash
/// (`halted`) and P6 (`Stopped`) end it. `restore` returns every refusal, as restore-6b does.
pub(crate) fn pass(ctx: &Ctx, anchor: &Caller, who: PassFor<'_>) -> Res<Pass> {
    let scope = crate::agents::scope::state(anchor); // 6a calls 10, 13
    let (up, leader, step) = match who {
        PassFor::Restore => (false, None, None),
        PassFor::Up { leader, step } => (true, leader, step),
    };
    crate::agents::team_state::open_except(ctx, &scope, up, |tx| {
        let lazy = Lazy::new(tx.ctx);
        let mut reply = Reply::default();
        if up {
            soft(
                crate::agents::adopt::replay(tx),
                "adoptReplayError",
                &mut reply,
            )?;
            let finished = crate::agents::cascade::finish(tx, anchor).map(|_| ());
            soft(finished, "cascadeError", &mut reply)?;
        } else {
            crate::agents::cascade::finish(tx, anchor)?;
        }
        if let Some(id) = leader {
            match stopped(tx, id) {
                Ok(true) => return Ok(Pass::Stopped),
                Ok(false) => {}
                Err(e) if e.check == "halted" => return Err(e),
                Err(e) => reply.error("stoppedError", &e),
            }
        }
        settle_all(tx, &lazy, up, &mut reply)?;
        let carried = match (leader, step) {
            (Some(id), Some(step)) => Some((id, lead(tx, &lazy, id, step, &mut reply)?)),
            _ => None,
        };
        let listed = members(tx, &lazy, anchor, carried, &mut reply);
        if up {
            soft(listed, "membersError", &mut reply)?;
        } else {
            listed?;
        }
        // r6b call 17, 5-9 F7: its own boundary, after the member loop whatever the loop answered.
        match crate::agents::adopt::adopt_locked(tx, anchor) {
            Ok(_) => {}
            Err(e) if e.check == "halted" => return Err(e),
            Err(e) => reply.adopt = Some(e.check),
        }
        Ok(Pass::Done(reply.map()))
    })
}

/// `up`'s step boundary: a refusal other than a crash is reported under `key` and the pass goes on.
fn soft(r: Res<()>, key: &str, reply: &mut Reply) -> Res<()> {
    match r {
        Err(e) if e.check != "halted" => {
            reply.error(key, &e);
            Ok(())
        }
        r => r,
    }
}

/// The leader 6a's `leader_of` chose (call 17) is desired `stopped` (cascade-3d C2).
fn stopped(tx: &Tx<'_>, id: &str) -> Res<bool> {
    use crate::agents::cascade::{Desired, desired_of};
    Ok(desired_of(&tx.folder(id))? == Desired::Stopped)
}

/// Every pending launch settled, every half-done release finished, then the registry sweep (restore-6b's preamble).
/// 5-9 F6: for `up` the first member refusal (a settle that stays Unknown included) is reported as `settleError`.
fn settle_all(tx: &Tx<'_>, lazy: &Lazy<'_>, up: bool, reply: &mut Reply) -> Res<()> {
    let first = match settle_each(tx, lazy) {
        Ok(first) => first,
        Err(e) if up && e.check != "halted" => Some(e),
        Err(e) => return Err(e),
    };
    if let (true, Some(e)) = (up, first) {
        reply.error("settleError", &e);
    }
    if let Err(e) = crate::state::live::sweep(tx.ctx) {
        reply.sweep = Some(e.check);
    }
    Ok(())
}

/// A member that fails here is left to `observe`, which answers it Unknown; its refusal is kept, the first one
/// returned. A `halted` refusal is a crash and passes.
fn settle_each(tx: &Tx<'_>, lazy: &Lazy<'_>) -> Res<Option<Refusal>> {
    #[cfg(test)]
    faults::check("settle")?;
    let mut first = None;
    for (id, g) in session_members(tx)? {
        if g.pending().is_some() {
            let settled = match tx.settle(&id, Mode::Normal) {
                Ok(Outcome::Unknown(e)) => Err(e),
                r => r.map(|_| ()),
            };
            keep(settled, &id, &mut first)?;
        }
        // reread: settle may have promoted (R3), making another generation active.
        if let Member::Life(g) = gens::member_of(g.folder())
            && let Some(n) = g.active()
        {
            let finished = crate::agents::release::finish(tx, lazy, &id, n).map(|_| ());
            keep(finished, &id, &mut first)?;
        }
    }
    Ok(first)
}

fn keep(r: Res<()>, id: &str, first: &mut Option<Refusal>) -> Res<()> {
    match r {
        Err(e) if e.check == "halted" => Err(e),
        Err(e) => {
            if first.is_none() {
                let message = format!("{id}: {}", e.message);
                *first = Some(Refusal { message, ..e });
            }
            Ok(())
        }
        Ok(()) => Ok(()),
    }
}

/// What the leader branch leaves for the member loop (call 25, 5-9 F9, F10): decided in this pass, or its one
/// observation, a refusal included, carried whatever the step answered.
enum Lead {
    Resumed,
    Seen(Box<Res<(Seen, Option<Terminal>)>>),
}

/// Call 18: the leader is observed once and adjudicated (r6b call 29; checklist 33) before its step.
fn lead(tx: &Tx<'_>, lazy: &Lazy<'_>, id: &str, step: LeaderStep, reply: &mut Reply) -> Res<Lead> {
    let seen = life(tx, id).and_then(|g| {
        let o = observe(tx, lazy, &g)?;
        adjudicated(tx, id, &g, o)
    });
    let stepped = match &seen {
        Err(e) if e.check == "halted" => return Err(e.clone()),
        Err(e) => Err(e.clone()),
        Ok(_) => step(tx, lazy, id),
    };
    match stepped {
        Ok(Some(v)) => {
            let resumed = v.get("resumed").is_some();
            reply.insert("leaderResume", v);
            if resumed {
                return Ok(Lead::Resumed); // decided in this pass (6b-R1): the loop does not observe it again
            }
        }
        Ok(None) => {}
        Err(e) if e.check == "halted" => return Err(e),
        Err(e) => reply.insert(
            "leaderResume",
            json!({"refused": e.check, "reason": e.message}),
        ),
    }
    Ok(Lead::Seen(Box::new(seen)))
}

fn life(tx: &Tx<'_>, id: &str) -> Res<Gen> {
    match gens::member_of(&tx.folder(id)) {
        Member::Life(g) => Ok(g),
        Member::Unreadable(e) => Err(e),
        _ => Err(crate::state::evidence::unreadable()),
    }
}

/// Checklist 33 (5-9 F9): an active generation that ended with no recorded outcome is adjudicated now, durably,
/// before the step. A recorded outcome is only read (r6b call 29); a stopped leader is never adjudicated.
fn adjudicated(
    tx: &Tx<'_>,
    id: &str,
    g: &Gen,
    (mut s, t): (Seen, Option<Terminal>),
) -> Res<(Seen, Option<Terminal>)> {
    if let Some(n) = g.active()
        && s.ended
        && !s.stopped
        && s.verdict.is_none()
    {
        s.verdict = judged(tx, id, n, t.as_ref())?;
    }
    Ok((s, t))
}

/// The anchor session's members (r6b call 31), each decided on its own: a refusal makes that member Unknown, only a
/// crash fails the loop (r6b call 16, 5-9 F11). The leader is decided from its carried observation.
fn members(
    tx: &Tx<'_>,
    lazy: &Lazy<'_>,
    anchor: &Caller,
    carried: Option<(&str, Lead)>,
    reply: &mut Reply,
) -> Res<()> {
    #[cfg(test)]
    faults::check("members")?;
    let (leader, mut lead) = match carried {
        Some((id, l)) => (Some(id), Some(l)),
        None => (None, None),
    };
    for (id, _, member) in gens::members(&tx.scope.agents(&tx.ctx.state_root))? {
        let g = match member {
            // 6b-R2, p1c call 6: never read past its record, never written.
            Member::Pre(_) | Member::Absent => continue,
            Member::Unreadable(e) => {
                reply
                    .unknown
                    .push(json!({"id": id, "name": null, "check": e.check}));
                continue;
            }
            Member::Life(g) => g,
        };
        if g.record().get("session").and_then(Value::as_str) != Some(tx.scope.session.as_str()) {
            continue; // r6b call 31
        }
        if g.active().is_none() && g.pending().is_none() {
            continue; // never ran
        }
        let seen = match lead.take_if(|_| leader == Some(id.as_str())) {
            Some(Lead::Resumed) => continue,
            Some(Lead::Seen(seen)) => *seen,
            None => observe(tx, lazy, &g),
        };
        match seen.and_then(|(seen, _)| decide(tx, lazy, anchor, &id, &g, &seen, reply)) {
            Err(e) if e.check == "halted" => return Err(e),
            Err(e) => reply.unknown.push(failed(&id, g.record(), &e)),
            Ok(()) => {}
        }
    }
    Ok(())
}

impl Reply {
    fn insert(&mut self, key: &str, v: Value) {
        self.extra.insert(key.into(), v);
    }
    /// The first refusal of a step, as `{check, message}`.
    fn error(&mut self, key: &str, e: &Refusal) {
        self.extra
            .entry(key)
            .or_insert_with(|| json!({"check": e.check, "message": e.message}));
    }
}

/// Test seam: `settle_each` and `members` refuse at entry when this thread names them (5-9 F5, F6).
#[cfg(test)]
pub(crate) mod faults {
    use std::cell::Cell;
    thread_local! { static AT: Cell<Option<&'static str>> = const { Cell::new(None) }; }
    pub fn at(step: Option<&'static str>) {
        AT.with(|a| a.set(step));
    }
    pub(super) fn check(step: &str) -> crate::error::Res<()> {
        match AT.with(Cell::get) {
            Some(s) if s == step => crate::error::refuse(1, "evidence", "forced"),
            _ => Ok(()),
        }
    }
}
