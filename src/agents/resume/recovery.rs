//! restore-6c P2: a person's recovery of a member that needs it, as the next generation of the same id (#103).
use serde_json::{Map, Value, json};

use super::super::gens::{Gen, Member, Verdict};
use super::super::reconcile::{Next, launch_next, observe_recovery};
use super::super::team_state::Tx;
use super::super::text;
use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::error::{Res, refuse};
use crate::state::evidence::unreadable;

/// P2: the person actions on a member that needs recovery.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Retry,
}

/// What `target` names among the anchor session's members (call 15).
enum Subject {
    Life(String),
    /// A pre-lifecycle record: today's resume (prelife-1c G10 refuses it there).
    Pre,
    None,
}

/// P2 (ambiguities 1-4; calls 15, 25): X of the caller's anchor session, under `up|T`, then one team Tx: settle,
/// one observation, the decision, the person check, then n+1 of the same id. `None`: not a recovery (no lifecycle
/// team in the session, a pre-lifecycle subject, or X Ended without a failure); `resume` keeps today's meaning.
pub(crate) fn recover(
    ctx: &Ctx,
    caller: &Caller,
    action: Action,
    target: &str,
) -> Res<Option<Map<String, Value>>> {
    let here = super::super::scope::state(caller); // 6a call 10
    let Some(team) = super::super::reconcile::session_team(ctx, &here)? else {
        return Ok(None);
    };
    super::super::admit(ctx, &format!("up|{team}"), || {
        super::super::team_state::open(ctx, &here, |tx| {
            let id = match subject(tx, target)? {
                Subject::Life(id) => id,
                Subject::Pre => return Ok(None),
                Subject::None => {
                    return refuse(2, "target", "no member of this team has that name or id");
                }
            };
            tx.settle(&id, super::super::settle::Mode::Normal)?;
            let g = life_of(tx, &id)?; // reread: settle may have promoted
            let lazy = crate::hc::Lazy::new(tx.ctx);
            let seen = observe_recovery(tx, &lazy, &g)?; // 6c-R1: the one observation decides
            let name = text(g.record().get("name"));
            if !matches!(seen.verdict, Some(Verdict::Failed(_))) {
                return match action {
                    Action::Retry if seen.ended || seen.released => Ok(None),
                    _ => refuse(2, "state", format!("{name} does not need recovery")),
                };
            }
            super::super::up::person_as(ctx, "recover a member")?;
            let session = retry_session(&g)?;
            let n = super::super::cascade::reopen_same(tx, &id, || {
                let next = Next {
                    kind: "retry",
                    session: &session,
                };
                launch_next(tx, lazy.get()?, &id, &g, next)
            })?;
            Ok(Some(reply(&g, n, "retry", &session)))
        })
    })
}

/// Call 15: X among the anchor session's lifecycle members only (`session` is the Tx's, `id` names its folder), by
/// exact id, else the newest (`createdAt`) of that name. Any unreadable member may be X: `evidence` (call 25).
fn subject(tx: &Tx<'_>, target: &str) -> Res<Subject> {
    let (mut named, mut pre) = (Vec::new(), false);
    for (id, _, m) in super::super::gens::members(&tx.scope.agents(&tx.ctx.state_root))? {
        let g = match m {
            Member::Life(g) => g,
            Member::Unreadable(e) => return Err(e),
            Member::Pre(r) => {
                pre |= id == target || text(r.get("name")) == target;
                continue;
            }
            Member::Absent => continue,
        };
        let r = g.record();
        if text(r.get("session")) != tx.scope.session || text(r.get("id")) != id {
            continue;
        }
        if id == target {
            return Ok(Subject::Life(id));
        }
        if text(r.get("name")) == target {
            named.push((text(r.get("createdAt")), id));
        }
    }
    Ok(match named.into_iter().max() {
        Some((_, id)) => Subject::Life(id),
        None if pre => Subject::Pre,
        None => Subject::None,
    })
}

fn life_of(tx: &Tx<'_>, id: &str) -> Res<Gen> {
    match super::super::gens::member_of(&tx.folder(id)) {
        Member::Life(g) => Ok(g),
        Member::Unreadable(e) => Err(e),
        _ => Err(unreadable()),
    }
}

/// Checklist 8: the conversation a retry resumes is the failed generation's flushed `launch.json` `sessionId`, a
/// valid conversation id that the record names too. Anything else is `evidence`, before anything is launched.
fn retry_session(g: &Gen) -> Res<String> {
    let n = g.active().ok_or_else(unreadable)?;
    let launch = super::super::gens::read_flushed(g.folder(), n, "launch")?;
    let sid = launch
        .as_ref()
        .and_then(|l| l.get("sessionId"))
        .and_then(Value::as_str)
        .filter(|s| crate::checks::session_id(s));
    match sid {
        Some(s) if g.record().get("sessionId").and_then(Value::as_str) == Some(s) => Ok(s.into()),
        _ => Err(unreadable()),
    }
}

fn reply(g: &Gen, n: u64, kind: &str, session: &str) -> Map<String, Value> {
    let r = g.record();
    let mut m = Map::new();
    m.insert(
        "agent".into(),
        json!({"id": r["id"], "name": r["name"], "generation": n, "kind": kind, "sessionId": session}),
    );
    m
}
