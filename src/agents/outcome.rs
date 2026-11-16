//! outcome-2 (#103 O2, O4, O6): each generation's one outcome, decided under its gen lock from durable evidence, and
//! the one predicate every surface shares. The only writer of `failure.json` and `outcome.json`.
use std::path::Path;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

use super::gens::{self, Decision, Ready};
use crate::ctx::Ctx;
use crate::error::{Refusal, Res};
use crate::hc::Terminal;
use crate::state::evidence::{self, Published};
use crate::util::utc_o;

pub const EXITED: &str = "client exited before readiness";
pub const ENDED: &str = "terminal ended";
pub const LOST: &str = "terminal lost";
pub const NO_REPORT: &str = "no readiness report";
pub const NO_CLIENT: &str = "client identity not published";
pub const ABANDONED: &str = "claim abandoned";
pub const ABORTED: &str = "commit aborted";
/// O4.3: how long past the deadline a `report.json` waits for its client identity.
pub const GRACE: Duration = Duration::from_secs(60);

/// A generation's one outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    Failed(String),
    Void(String),
}

impl Verdict {
    /// `outcome.json` (ambiguity 6).
    pub fn body(&self, at: &str) -> Value {
        match self {
            Verdict::Ok => json!({"outcome": "ok", "at": at}),
            Verdict::Failed(r) => json!({"outcome": "failed", "reason": r, "at": at}),
            Verdict::Void(r) => json!({"outcome": "void", "reason": r, "at": at}),
        }
    }
    /// An `outcome.json` body; any other shape is torn evidence (ambiguity 11).
    pub fn parse(body: &Value) -> Res<Verdict> {
        let reason = || {
            body.get("reason")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(evidence::unreadable)
        };
        match body.get("outcome").and_then(Value::as_str) {
            Some("ok") => Ok(Verdict::Ok),
            Some("failed") => Ok(Verdict::Failed(reason()?)),
            Some("void") => Ok(Verdict::Void(reason()?)),
            _ => Err(evidence::unreadable()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Adjudged {
    Decided(Verdict),
    /// Nothing decides yet; nothing was written.
    Undecided,
    /// `Wait::Try` found the gen lock held; nothing was read.
    Busy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    Block,
    /// The board: one attempt, never waiting (O4).
    Try,
}

/// O4: generation n's outcome, under `gen_lock_key(key, id, n)`, every file read once (R1). `seen` is the caller's
/// observation of n's terminal, the only Lost evidence (ambiguity 3); no hc call is made under the lock. `Err`:
/// decide nothing on this call (r2b call 6), whatever was or was not written.
pub fn adjudicate(
    ctx: &Ctx,
    key: &str,
    id: &str,
    n: u64,
    wait: Wait,
    seen: Option<&Terminal>,
) -> Res<Adjudged> {
    if !gens::plain_key(key) || !gens::plain_id(id) {
        return Err(Refusal::internal("adjudicate: not a plain member name"));
    }
    let member = ctx.state_root.join(key).join("agents").join(id);
    let lock = super::team_state::gen_lock_key(key, id, n);
    let run = || decide(ctx, &member, n, seen);
    match wait {
        Wait::Block => super::lock(ctx, &lock, run),
        Wait::Try => Ok(super::try_lock(ctx, &lock, run)?.unwrap_or(Adjudged::Busy)),
    }
}

/// One hold: K5 steps 1-4 (`promote_held`), then void and the failure legs (`fresh`), then `outcome.json` (O4.4).
fn decide(ctx: &Ctx, member: &Path, n: u64, seen: Option<&Terminal>) -> Res<Adjudged> {
    let gens::Held {
        ready,
        failure,
        report,
        exit,
    } = gens::promote_held(ctx, member, n)?;
    let now = ctx.clock.now();
    let verdict = match ready {
        Ready::Outcome(body) => return Ok(Adjudged::Decided(Verdict::parse(&body)?)),
        Ready::Failed => Verdict::Failed(reason(failure.as_ref())?),
        Ready::Ok => Verdict::Ok,
        Ready::Undecided => match fresh(member, n, now, seen, report.as_ref(), exit)? {
            Some(v) => v,
            None => return Ok(Adjudged::Undecided),
        },
    };
    create(member, n, "outcome", &verdict.body(&utc_o(now)))?;
    Ok(Adjudged::Decided(verdict))
}

/// A recorded `failure.json`'s reason; none is torn evidence.
fn reason(failure: Option<&Value>) -> Res<String> {
    failure
        .and_then(|f| f.get("reason"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(evidence::unreadable)
}

/// Create-once under the gen lock: `Exists` means a writer outside the lock; decide nothing (ambiguity 10).
fn create(member: &Path, n: u64, file: &str, body: &Value) -> Res<()> {
    match gens::publish_once(member, n, file, body)? {
        Published::Published => Ok(()),
        Published::Exists => Err(evidence::unreadable()),
    }
}

/// O4.2-O4.3: void from its own evidence; then, only for claim `run` and commit `Run`, the failure legs in the spec's
/// order. A leg that fires creates `failure.json` before `outcome.json` (O4.3). None: undecided, nothing written.
fn fresh(
    member: &Path,
    n: u64,
    now: SystemTime,
    seen: Option<&Terminal>,
    report: Option<&Value>,
    exit: Option<Value>,
) -> Res<Option<Verdict>> {
    let claim = gens::claim(member, n).known()?;
    if claim.as_ref().and_then(Value::as_str) == Some("abandoned") {
        return Ok(Some(Verdict::Void(ABANDONED.into())));
    }
    let commit = gens::read(member, n, "commit")
        .known()?
        .map(|c| gens::decide(&c));
    if commit == Some(Decision::Aborted) {
        return Ok(Some(Verdict::Void(ABORTED.into())));
    }
    if claim.as_ref().and_then(Value::as_str) != Some("run") || commit != Some(Decision::Run) {
        return Ok(None);
    }
    let Some((reason, facts)) = failure(member, n, now, seen, report.is_some(), exit)? else {
        return Ok(None);
    };
    let body = json!({"reason": reason, "evidence": facts, "at": utc_o(now)});
    create(member, n, "failure", &body)?;
    Ok(Some(Verdict::Failed(reason.into())))
}

/// The first failure leg that fires (ambiguity 7), each file read only when its leg is reached (R1).
fn failure(
    member: &Path,
    n: u64,
    now: SystemTime,
    seen: Option<&Terminal>,
    reported: bool,
    exit: Option<Value>,
) -> Res<Option<(&'static str, Value)>> {
    // Call 16 (R1): on the report path K5 already read exit.json (`Held.exit`); only without a report is it read here.
    let exit = if reported {
        exit
    } else {
        gens::read(member, n, "exit").known()?
    };
    if let Some(exit) = exit {
        return Ok(Some((EXITED, exit)));
    }
    if let Some(ended) = gens::read(member, n, "ended").known()? {
        return Ok(Some((ENDED, ended)));
    }
    if let Some(t) = seen
        && lost(member, n, t)?
    {
        return Ok(Some((LOST, json!({"address": t.address, "argv": t.argv}))));
    }
    let spec = gens::read(member, n, "spec")
        .known()?
        .ok_or_else(evidence::unreadable)?;
    let text = spec
        .get("deadline")
        .and_then(Value::as_str)
        .ok_or_else(evidence::unreadable)?;
    let deadline = crate::util::parse_utc(text).ok_or_else(evidence::unreadable)?;
    let facts = json!({"deadline": text, "now": utc_o(now)});
    if !reported && now >= deadline {
        return Ok(Some((NO_REPORT, facts)));
    }
    if reported && now >= deadline + GRACE {
        return Ok(Some((NO_CLIENT, facts)));
    }
    Ok(None)
}

/// Ambiguity 3: `t` is generation n's terminal shown Lost: its name is `launch.json hcName`, its labels name this
/// member and n, and `classify` against `launch.json launcherArgv` is Lost. launch.json absent or torn: `evidence`.
fn lost(member: &Path, n: u64, t: &Terminal) -> Res<bool> {
    let launch = gens::read(member, n, "launch")
        .known()?
        .ok_or_else(evidence::unreadable)?;
    let hc_name = launch
        .get("hcName")
        .and_then(Value::as_str)
        .ok_or_else(evidence::unreadable)?;
    let argv: Vec<String> = launch
        .get("launcherArgv")
        .and_then(Value::as_array)
        .ok_or_else(evidence::unreadable)?
        .iter()
        .map(|a| {
            a.as_str()
                .map(str::to_owned)
                .ok_or_else(evidence::unreadable)
        })
        .collect::<Res<_>>()?;
    let id = member.file_name().and_then(|f| f.to_str());
    let label = |k: &str| t.labels.get(k).map(String::as_str);
    let gen_label = n.to_string();
    Ok(t.name.as_deref() == Some(hc_name)
        && label("mux.id") == id
        && label("mux.gen") == Some(gen_label.as_str())
        && super::lifecycle::classify(Some(t), &argv) == super::lifecycle::Life::Lost)
}

#[cfg(test)]
#[path = "outcome_tests.rs"]
mod tests;
