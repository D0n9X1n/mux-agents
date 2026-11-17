//! restore-6b: reconcile every lifecycle member of a team; a Lost member whose last run succeeded comes back as
//! a new generation of the same id and conversation (#103).
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

mod restore;
use restore::Done;

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
    let mut s = Seen {
        stopped: super::cascade::desired_of(g.folder())? == super::cascade::Desired::Stopped,
        pending: g.pending().is_some(),
        ..Seen::default()
    };
    let Some(n) = g.active() else {
        return Ok((s, None));
    };
    if s.stopped {
        return Ok((s, None));
    }
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
                s.verdict = judged(tx, &id, n, None)?;
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
        s.verdict = judged(tx, &id, n, None)?;
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
    s.verdict = judged(tx, &id, n, seen.as_ref())?;
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

/// L2 for the team `anchor` names (ambiguity 7), under the team Tx; `restore` and `up` take `up|T` first.
pub fn run(ctx: &Ctx, anchor: &Caller) -> Res<Map<String, Value>> {
    let scope = super::team_state::Scope::of(anchor);
    super::team_state::open(ctx, &scope, |tx| {
        let lazy = Lazy::new(tx.ctx);
        let mut reply = Reply::default();
        preamble(tx, &lazy, anchor, &mut reply)?;
        for (id, _, member) in super::gens::members(&scope.agents(&ctx.state_root))? {
            let g = match member {
                // 6b-R2, p1c call 6: never read past its record, never written.
                super::gens::Member::Pre(_) | super::gens::Member::Absent => continue,
                super::gens::Member::Unreadable(e) => {
                    reply
                        .unknown
                        .push(json!({"id": id, "name": null, "check": e.check}));
                    continue;
                }
                super::gens::Member::Life(g) => g,
            };
            if g.record().get("session").and_then(Value::as_str) != Some(scope.session.as_str()) {
                continue; // r6b call 31
            }
            if g.active().is_none() && g.pending().is_none() {
                continue; // never ran
            }
            match observe(tx, &lazy, &g) {
                Ok((seen, _)) => {
                    // r6b call 16: a member that fails here is Unknown; only a crash fails the pass.
                    if let Err(e) = decide(tx, &lazy, anchor, &id, &g, &seen, &mut reply) {
                        if e.check == "halted" {
                            return Err(e);
                        }
                        reply.unknown.push(failed(&id, g.record(), &e));
                    }
                }
                Err(e) if e.check == "halted" => return Err(e),
                Err(e) => reply.unknown.push(failed(&id, g.record(), &e)),
            }
        }
        // r6b call 17: best effort; a refusal (an unreadable member in `Tree`) is reported, never fatal.
        match super::adopt::adopt_locked(tx, anchor) {
            Ok(_) => {}
            Err(e) if e.check == "halted" => return Err(e),
            Err(e) => reply.adopt = Some(e.check),
        }
        Ok(reply.map())
    })
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
fn session_team(ctx: &Ctx, scope: &super::team_state::Scope) -> Res<Option<String>> {
    for (_, _, member) in super::gens::members(&scope.agents(&ctx.state_root))? {
        let super::gens::Member::Life(g) = member else {
            continue;
        };
        let r = g.record();
        if r.get("session").and_then(Value::as_str) == Some(scope.session.as_str())
            && super::tree::is_leader(r)
        {
            return Ok(r.get("upTeam").and_then(Value::as_str).map(str::to_owned));
        }
    }
    Ok(None)
}

/// The person caller moved to the anchor scope (ambiguity 7): its key and session; no pane, chain or client.
pub fn anchor_caller(person: &Caller, scope: &super::team_state::Scope) -> Caller {
    Caller {
        kind: crate::caller::Kind::Human,
        client: None,
        agent: None,
        chain: Vec::new(),
        pane: String::new(),
        window: String::new(),
        session: scope.session.clone(),
        server_key: scope.key.clone(),
        member: None,
        candidate: None,
        ..person.clone()
    }
}

/// L2 preamble, after `Tx::open` replayed its hooks: cascades (3d C8), every pending launch (settle-1b G3, `normal`),
/// every half-done release of an active generation (release-1d R1 replay), then the registry sweep (report-2b K2).
/// A member that fails here is left for `observe`, which answers it Unknown; a `halted` refusal is a crash and passes.
fn preamble(tx: &Tx<'_>, lazy: &Lazy<'_>, anchor: &Caller, reply: &mut Reply) -> Res<()> {
    super::cascade::finish(tx, anchor)?;
    for (id, g) in session_members(tx)? {
        if g.pending().is_some() {
            pass_halt(tx.settle(&id, super::settle::Mode::Normal).map(|_| ()))?;
        }
        // reread: settle may have promoted (R3), making another generation active.
        if let super::gens::Member::Life(g) = super::gens::member_of(g.folder())
            && let Some(n) = g.active()
        {
            pass_halt(super::release::finish(tx, lazy, &id, n).map(|_| ()))?;
        }
    }
    if let Err(e) = crate::state::live::sweep(tx.ctx) {
        reply.sweep = Some(e.check);
    }
    Ok(())
}

/// Only a crash (`halted`, group B b-calls 36) stops the pass; any other refusal leaves the member to `observe`.
fn pass_halt(r: Res<()>) -> Res<()> {
    match r {
        Err(e) if e.check == "halted" => Err(e),
        _ => Ok(()),
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
