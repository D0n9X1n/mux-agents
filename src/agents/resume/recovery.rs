//! restore-6c P2: a person's recovery of a member that needs it, as the next generation of the same id (#103).
use serde_json::{Map, Value, json};

use super::super::gens::{Gen, Member, Verdict};
use super::super::lifecycle::Life;
use super::super::reconcile::{Next, Seen, launch_next, observe_recovery};
use super::super::settle::{Mode, Outcome};
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
    Fresh,
}

/// What `target` names among the anchor session's members (call 15).
pub(crate) enum Subject {
    Life(String),
    /// A pre-lifecycle record: today's resume (prelife-1c G10 refuses it there, retry or fresh).
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
        return match action {
            Action::Retry => Ok(None),
            Action::Fresh => refuse(
                1,
                "team",
                "no team in this session; run up from the team folder",
            ),
        };
    };
    super::super::admit(ctx, &format!("up|{team}"), || {
        super::super::team_state::open(ctx, &here, |tx| {
            let id = match subject(tx, target)? {
                Subject::Life(id) => id,
                Subject::Pre => return Ok(None), // prelife-1c G10d refuses it in today's resume
                Subject::None => {
                    return refuse(2, "target", "no member of this team has that name or id");
                }
            };
            // checklist 5: settle's unresolved answer is kept; torn evidence is `evidence`, never "still pending"
            if let Outcome::Unknown(e) = tx.settle(&id, Mode::Normal)?
                && e.check == "evidence"
            {
                return Err(e);
            }
            let lazy = crate::hc::Lazy::new(tx.ctx);
            // checklist 2: a release this member's last recovery left half done is finished first (R1 replay)
            if let Some(n) = life_of(tx, &id)?.active() {
                super::super::release::finish(tx, &lazy, &id, n)?;
            }
            let g = life_of(tx, &id)?; // reread: settle may have promoted, the replay released
            let seen = observe_recovery(tx, &lazy, &g)?; // 6c-R1: the one observation decides
            let name = text(g.record().get("name"));
            if !matches!(seen.verdict, Some(Verdict::Failed(_))) {
                return match action {
                    Action::Retry if seen.ended || seen.released => Ok(None),
                    _ => refuse(2, "state", format!("{name} does not need recovery")),
                };
            }
            super::super::up::person_as(ctx, "recover a member")?;
            // ambiguity 5: retry resumes the failed generation's conversation; fresh takes a new one (no occupancy)
            let (kind, session) = match action {
                Action::Retry => ("retry", retry_session(&g)?),
                Action::Fresh => ("fresh", fresh_session()?),
            };
            let (g, released) = gates(
                tx,
                &lazy,
                caller,
                &id,
                g,
                &seen,
                (action == Action::Retry).then_some(session.as_str()),
            )?;
            let n = super::super::cascade::reopen_same(tx, &id, || {
                let next = Next {
                    kind,
                    session: &session,
                    fresh: action == Action::Fresh,
                };
                launch_next(tx, lazy.get()?, &id, &g, next)
            })?;
            let mut out = reply(&g, n, kind, &session);
            if let Some(r) = released {
                out.insert("released".into(), json!(r));
            }
            Ok(Some(out))
        })
    })
}

/// P2 gates (ambiguity 3), after the person check: a Lost active generation is released `cause: recovery`
/// (release-1d R2), then every generation must be done (no pending, no orphan, the active one `gens::done`), then
/// the cap (6b L1; never the leader), then the conversation must be free (occupy-3; o3 call 5).
fn gates(
    tx: &Tx<'_>,
    lazy: &crate::hc::Lazy<'_>,
    caller: &Caller,
    id: &str,
    g: Gen,
    seen: &Seen,
    occupancy: Option<&str>,
) -> Res<(Gen, Option<u64>)> {
    let n = g.active().ok_or_else(unreadable)?;
    let (g, released) = if seen.life == Some(Life::Lost) && !seen.released {
        let address = text(g.record().get("hcAddress"));
        let cause = super::super::release::Cause::Recovery;
        super::super::release::release(tx, lazy, id, n, &address, cause)?;
        (life_of(tx, id)?, Some(n))
    } else {
        (g, None)
    };
    let name = text(g.record().get("name"));
    let busy = |k: u64, at: String| {
        refuse::<()>(
            2,
            "running",
            format!(
                "generation {k} of {name} may still run{at}; run `mux-agent stop {name} -Generation` first"
            ),
        )
    };
    if let Some(p) = g.pending() {
        busy(p, String::new())?;
    }
    if let Some(o) = g.orphans()?.into_iter().min() {
        busy(o, String::new())?;
    }
    if !super::super::gens::done(g.folder(), n)? {
        busy(n, format!(" at {}", text(g.record().get("hcAddress"))))?;
    }
    if !super::super::tree::is_leader(g.record()) {
        let anchor = super::super::reconcile::anchor_caller(caller, &tx.scope);
        let snapshot = super::super::membership::Snapshot::for_spawn(tx.ctx, &anchor, lazy.get()?)?;
        super::super::tree::limit(&snapshot)?;
    }
    let client = super::super::spawn::client_named(&text(g.record().get("client")))
        .ok_or_else(|| crate::error::Refusal::internal("the member names no known client"))?;
    if let Some(session) = occupancy
        && let Some(hold) = super::super::occupancy::held(tx.ctx, session, client, None)
    {
        refuse::<()>(2, "running", hold.message())?;
    }
    Ok((g, released))
}

/// Call 15: X among the anchor session's lifecycle members only (`session` is the Tx's, `id` names its folder), by
/// exact id, else the newest (`createdAt`) of that name. Any unreadable member may be X: `evidence` (call 25).
pub(crate) fn subject(tx: &Tx<'_>, target: &str) -> Res<Subject> {
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

/// Ambiguity 5: a new conversation id; it reaches `pending` and `launch.json` only through `forward`.
fn fresh_session() -> Res<String> {
    let id = crate::util::guid()?.to_ascii_lowercase();
    if crate::checks::session_id(&id) {
        Ok(id)
    } else {
        Err(crate::error::Refusal::internal(
            "a new session id is not valid",
        ))
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
