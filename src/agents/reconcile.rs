//! restore-6b: reconcile every lifecycle member of a team; a Lost member whose last run succeeded comes back as
//! a new generation of the same id and conversation.
use super::gens::{Gen, Verdict};
use super::lifecycle::Life;
use super::team_state::Tx;
use super::text;
use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::error::{Refusal, Res};
use crate::hc::{Lazy, Terminal};
use crate::util::utc_o;
use serde_json::{Map, Value, json};

mod pass;
mod restore;
#[cfg(test)]
pub(crate) use pass::faults;
pub(crate) use pass::{Pass, PassFor, pass};
use restore::Done;
pub(crate) use restore::{Next, launch_next};

/// What one pass read of a member (6b-R1); `classify` decides from this alone.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Seen {
    /// `desired.json` says `stopped` (cascade-3d C1).
    pub stopped: bool,
    /// `pending` is still set after settle (settle-1b rows 1, 6).
    pub pending: bool,
    /// The `cause` in `release.json` of the active generation n.
    pub cause: Option<String>,
    pub released: bool,
    pub ended: bool,
    /// The life hc shows for n by its `launch.json` identity; None when n is done or hc was not asked.
    pub life: Option<Life>,
    /// The outcome of n (outcome-2 O4); None when undecided or not asked.
    pub verdict: Option<Verdict>,
    /// n's `launch.json` `sessionId` when it is a valid conversation id (`checks::session_id`); None otherwise.
    /// Rows 0 and 4 refuse `evidence` on None (r6b call 15). Read once here (6b-R1), never reread.
    pub session: Option<String>,
}

/// The L2 rows: `Await` is row 0, `Lost` row 4 (both restore); `Held` is row 0 not yet decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Stopped,
    Await,
    Held(String),
    Unknown(String),
    Running,
    Lost,
    Recover(String),
    Ended,
}

/// L2, in the order of ambiguity 2: row 1, an unsettled pending (row 2), an unfinished release, row 0, then 2-6.
pub fn classify(s: &Seen) -> Row {
    if s.stopped {
        return Row::Stopped;
    }
    if s.pending {
        return Row::Unknown("pending".into());
    }
    if s.cause.is_some() && !s.released {
        return Row::Unknown("release".into());
    }
    match s.cause.as_deref() {
        Some("restore") => {
            return match &s.verdict {
                Some(Verdict::Ok) => Row::Await,
                Some(Verdict::Failed(r) | Verdict::Void(r)) => Row::Recover(r.clone()),
                None => Row::Held("outcome".into()),
            };
        }
        Some("recovery") => return Row::Recover("recovery".into()),
        _ => {}
    }
    if s.ended || s.released {
        return Row::Ended;
    }
    match s.life {
        None | Some(Life::Unknown) => Row::Unknown("hc".into()),
        Some(Life::Running) => Row::Running,
        Some(Life::Ended) => Row::Ended,
        Some(Life::Lost) => match &s.verdict {
            Some(Verdict::Ok) => Row::Lost,
            Some(Verdict::Failed(r) | Verdict::Void(r)) => Row::Recover(r.clone()),
            None => Row::Recover("outcome undecided".into()),
        },
    }
}

/// 6b-R1: one read of member `g` under the Tx: desired, pending, then generation n's marks (claim, commit, ended,
/// released: completion evidence before any decision, r6b call 14), `release.json` and `launch.json`, once each.
/// hc only when n is neither done nor pending. Every active generation not done is adjudicated (outcome-2's shared
/// contract, r6b calls 13, 24) with what hc showed, whatever its life; a member stopped, or whose hc is unreachable,
/// is not (r6b call 22). An Ended terminal is recorded first (`gens::record_end`), then adjudicated from the recorded
/// end, so an end before readiness fails (r6b call 23). A done generation reports its recorded flushed outcome, with
/// no hc and no second adjudication (r6b call 29); only a restore release without one is adjudicated (row 0 needs
/// its verdict).
pub fn observe(tx: &Tx<'_>, lazy: &Lazy<'_>, g: &Gen) -> Res<(Seen, Option<Terminal>)> {
    look(tx, lazy, g, false)
}

/// restore-6c (call 25, checklist 1-4): recovery's one observation. A stopped member is observed past its stop
/// (its marks, its life) and its verdict is only its recorded outcome, parsed (`Verdict::parse`), never a new
/// adjudication. hc unreachable for a generation not done is a refusal, never "no recovery".
pub(crate) fn observe_recovery(tx: &Tx<'_>, lazy: &Lazy<'_>, g: &Gen) -> Res<Seen> {
    look(tx, lazy, g, true).map(|(s, _)| s)
}

fn look(tx: &Tx<'_>, lazy: &Lazy<'_>, g: &Gen, recovery: bool) -> Res<(Seen, Option<Terminal>)> {
    let mut s = Seen {
        stopped: super::cascade::desired_of(g.folder())? == super::cascade::Desired::Stopped,
        pending: g.pending().is_some(),
        ..Seen::default()
    };
    let Some(n) = g.active() else {
        return Ok((s, None));
    };
    if s.stopped && !recovery {
        return Ok((s, None));
    }
    let stopped = s.stopped;
    let judge = |id: &str, seen: Option<&Terminal>| {
        if stopped {
            recorded(g.folder(), n) // a stopped member: its recorded outcome only (call 25)
        } else {
            judged(tx, id, n, seen)
        }
    };
    let marks = super::gens::marks(g.folder(), n)?; // Unreadable: Err before hc and before any release (call 14)
    let release = g.read(n, "release").known()?;
    s.cause = release
        .as_ref()
        .and_then(|r| r.get("cause"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    (s.released, s.ended) = (marks.released, marks.ended);
    let id = text(g.record().get("id"));
    if marks.any() {
        s.verdict = recorded(g.folder(), n)?;
        if s.cause.as_deref() == Some("restore") && s.released {
            // row 0 resumes the conversation that ran (r6b call 15)
            s.session = session_of(g.read(n, "launch").known()?.as_ref());
            if s.verdict.is_none() {
                s.verdict = judge(&id, None)?;
            }
        }
        return Ok((s, None));
    }
    if release.is_some() {
        return Ok((s, None)); // half a release: row 2 Unknown("release"); the preamble finishes it first
    }
    let launch = g.read(n, "launch").known()?;
    s.session = session_of(launch.as_ref());
    if s.pending {
        // r6b call 24: the member stays Unknown("pending"), its active generation is still adjudicated.
        s.verdict = judge(&id, None)?;
        return Ok((s, None));
    }
    let Ok(core) = lazy.get() else {
        return Ok((s, None)); // row 2: hc unreachable is Unknown, never adjudicated (call 22)
    };
    let launch = launch
        .as_ref()
        .ok_or_else(crate::state::evidence::unreadable)?;
    let argv: Vec<String> =
        serde_json::from_value(launch["launcherArgv"].clone()).unwrap_or_default();
    let mut seen = None;
    match super::gens::find(core, &id, n, &text(launch.get("hcName"))) {
        super::gens::Found::One(t) => {
            s.life = Some(super::lifecycle::classify(Some(&t), &argv));
            seen = Some(t);
        }
        // r6b call 28: Missing never becomes Lost, Ended, or permission to relaunch.
        super::gens::Found::Unknown(_) | super::gens::Found::Missing(_) => {
            s.life = Some(Life::Unknown)
        }
    }
    if let (Some(Life::Ended), Some(t)) = (s.life, seen.as_ref()) {
        super::gens::record_end(g.folder(), g.record(), n, t, &utc_o(tx.ctx.clock.now()))?;
        s.ended = true;
    }
    s.verdict = judge(&id, seen.as_ref())?;
    Ok((s, seen))
}

/// A `launch.json` `sessionId` that is a valid conversation id (`checks::session_id`).
fn session_of(launch: Option<&Value>) -> Option<String> {
    launch
        .and_then(|l| l.get("sessionId"))
        .and_then(Value::as_str)
        .filter(|sid| crate::checks::session_id(sid))
        .map(str::to_owned)
}

/// r6b call 29: a done generation's recorded outcome, through its publisher's flushes (`read_flushed`).
fn recorded(folder: &std::path::Path, n: u64) -> Res<Option<Verdict>> {
    super::gens::read_flushed(folder, n, "outcome")?
        .map(|body| Verdict::parse(&body))
        .transpose()
}

/// outcome-2 O4 under the gen lock, with what hc showed of n (or nothing).
fn judged(tx: &Tx<'_>, id: &str, n: u64, seen: Option<&Terminal>) -> Res<Option<Verdict>> {
    let adjudged =
        super::gens::adjudicate(tx.ctx, &tx.scope.key, id, n, super::gens::Wait::Block, seen)?;
    Ok(match adjudged {
        super::gens::Adjudged::Decided(v) => Some(v),
        _ => None,
    })
}

/// Display only (state-0 S6; the bar and `list` never decide): the active generation of the member at `folder`, whose
/// `record` the caller's snapshot already holds (r6b call 26: no reread), was released for restore (`cause: restore`
/// and `released.json`) and nothing is pending, so reconcile relaunches it (row 0).
pub fn waiting(folder: &std::path::Path, record: &Value) -> Res<bool> {
    let Some(n) = super::gens::active(record) else {
        return Ok(false);
    };
    if super::gens::pending(record).is_some() {
        return Ok(false);
    }
    let cause = super::gens::read(folder, n, "release")
        .known()?
        .and_then(|r| r.get("cause").cloned());
    Ok(cause == Some(Value::from("restore"))
        && super::gens::read(folder, n, "released").known()?.is_some())
}

/// L6: the four answers. Running, stopped and ended members are not listed.
#[derive(Default)]
struct Reply {
    restored: Vec<Value>,
    held: Vec<Value>,
    unknown: Vec<Value>,
    recover: Vec<Value>,
    sweep: Option<&'static str>,
    /// adopt-3e refused (r6b call 17): reported, reconcile goes on.
    adopt: Option<&'static str>,
    /// restore-6c Task 7: `up`'s step reports and `leaderResume`.
    extra: Map<String, Value>,
}

impl Reply {
    fn map(self) -> Map<String, Value> {
        let mut m = Map::new();
        for (k, v) in [
            ("restored", self.restored),
            ("held", self.held),
            ("unknown", self.unknown),
            ("recover", self.recover),
        ] {
            m.insert(k.into(), Value::Array(v));
        }
        if let Some(check) = self.sweep {
            m.insert("sweepError".into(), json!(check));
        }
        if let Some(check) = self.adopt {
            m.insert("adoptError".into(), json!(check));
        }
        m.extend(self.extra);
        m
    }
}

fn entry(id: &str, record: &Value, k: &str, v: Value) -> Value {
    json!({"id": id, "name": record.get("name"), k: v})
}

/// r6b call 16: a member whose deciding or restoring failed is Unknown, named with its check and reason.
fn failed(id: &str, record: &Value, e: &Refusal) -> Value {
    json!({"id": id, "name": record.get("name"), "check": e.check, "reason": e.message})
}

/// The lifecycle members of the Tx's session (r6b call 31: a retired team's records at the same key are not walked).
fn session_members(tx: &Tx<'_>) -> Res<Vec<(String, Gen)>> {
    let mut out = Vec::new();
    for (id, _, member) in super::gens::members(&tx.scope.agents(&tx.ctx.state_root))? {
        if let super::gens::Member::Life(g) = member
            && g.record().get("session").and_then(Value::as_str) == Some(tx.scope.session.as_str())
        {
            out.push((id, g));
        }
    }
    Ok(out)
}

/// L2 for the team `anchor` names (ambiguity 7), under the team Tx; `restore` and `up` take `up|T` first. `restore`
/// keeps restore-6b's meaning: no leader step, no stopped check, every refusal returned.
pub fn run(ctx: &Ctx, anchor: &Caller) -> Res<Map<String, Value>> {
    match pass(ctx, anchor, PassFor::Restore)? {
        Pass::Done(m) => Ok(m),
        Pass::Stopped => Err(Refusal::internal("restore has no stopped check")),
    }
}

/// A person verb (ambiguity 9): the team its session names, under `up|<upTeam>`; then the one discovery `up` uses
/// (`existing`, r6b call 6) must name this session as the anchor; then the team Tx.
pub fn restore(ctx: &Ctx, _: &crate::cli::Request) -> Res<Map<String, Value>> {
    let person = super::up::person_as(ctx, "run restore")?;
    let here = super::team_state::Scope::of(&person);
    let Some(team) = session_team(ctx, &here)? else {
        return no_team();
    };
    super::admit(ctx, &format!("up|{team}"), || {
        let core = crate::hc::connect(ctx)?;
        match super::up::discover::existing(ctx, &core, &here.key, &team)? {
            Some(scope) if scope == here => run(ctx, &anchor_caller(&person, &scope)),
            _ => no_team(),
        }
    })
}

fn no_team<T>() -> Res<T> {
    crate::error::refuse(
        1,
        "team",
        "no team in this session; run up from the team folder",
    )
}

/// The `upTeam` a lifecycle leader record of `scope` names. Whether it ran is decided by `existing` alone.
/// restore-6c checklist 6: an unreadable member may be that leader. When no leader is found and the session has a
/// team record (present or unreadable), the unreadable member's `evidence` is returned, never "no team".
pub(crate) fn session_team(ctx: &Ctx, scope: &super::team_state::Scope) -> Res<Option<String>> {
    let mut torn = None;
    for (_, _, member) in super::gens::members(&scope.agents(&ctx.state_root))? {
        let g = match member {
            super::gens::Member::Life(g) => g,
            super::gens::Member::Unreadable(e) => {
                torn.get_or_insert(e);
                continue;
            }
            _ => continue,
        };
        let r = g.record();
        if r.get("session").and_then(Value::as_str) == Some(scope.session.as_str())
            && super::tree::is_leader(r)
        {
            return Ok(r.get("upTeam").and_then(Value::as_str).map(str::to_owned));
        }
    }
    let file = super::team::path(&ctx.state_root.join(&scope.key), &scope.session);
    match torn {
        Some(e)
            if !matches!(
                crate::state::evidence::read(&file),
                crate::state::evidence::Evidence::Absent
            ) =>
        {
            Err(e)
        }
        _ => Ok(None),
    }
}

/// The person caller with its state at the anchor `scope` (re6a call 13, replacing restore-6b ambiguity 7's
/// rewrites): only `anchor` is set; its socket, session, pane, window and server key stay its own, so every
/// multiplexer call reaches the person's own server while every state read goes to the anchor.
pub fn anchor_caller(person: &Caller, scope: &super::team_state::Scope) -> Caller {
    Caller {
        anchor: Some(crate::caller::Anchor {
            key: scope.key.clone(),
            session: scope.session.clone(),
        }),
        ..person.clone()
    }
}

/// The reply of the row `seen` classifies to (rows 0 and 4 restore at the anchor).
fn decide(
    tx: &Tx<'_>,
    lazy: &Lazy<'_>,
    anchor: &Caller,
    id: &str,
    g: &Gen,
    seen: &Seen,
    reply: &mut Reply,
) -> Res<()> {
    let r = g.record();
    let done = match classify(seen) {
        Row::Stopped | Row::Running | Row::Ended => return Ok(()),
        Row::Unknown(check) => {
            reply.unknown.push(entry(id, r, "check", json!(check)));
            return Ok(());
        }
        Row::Recover(why) => {
            reply.recover.push(entry(id, r, "reason", json!(why)));
            return Ok(());
        }
        Row::Held(why) => Done::Held(why, None),
        Row::Lost => restore::restore_lost(tx, lazy, id, g, seen)?,
        Row::Await => restore::restore_awaiting(tx, lazy, anchor, id, g, seen)?,
    };
    match done {
        Done::Restored(n) => {
            let sid = restore::session(seen)?; // the conversation n resumed (the spec's --resume value)
            reply
                .restored
                .push(json!({"id": id, "name": r.get("name"), "generation": n, "sessionId": sid}));
        }
        Done::Held(why, holder) => {
            let mut e = entry(id, r, "reason", json!(why));
            if let Some(h) = holder {
                e["holder"] = json!(h);
            }
            reply.held.push(e);
        }
    }
    Ok(())
}
