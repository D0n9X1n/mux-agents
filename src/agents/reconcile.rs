//! restore-6b: reconcile every lifecycle member of a team; a Lost member whose last run succeeded comes back as
//! a new generation of the same id and conversation (#103).
use super::gens::{Gen, Verdict};
use super::lifecycle::Life;
use super::team_state::Tx;
use super::text;
use crate::error::Res;
use crate::hc::{Lazy, Terminal};
use crate::util::utc_o;
use serde_json::Value;

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
        if s.verdict.is_none() && s.cause.as_deref() == Some("restore") && s.released {
            s.verdict = judged(tx, &id, n, None)?;
        }
        return Ok((s, None));
    }
    if release.is_some() {
        return Ok((s, None)); // half a release: row 2 Unknown("release"); the preamble finishes it first
    }
    let launch = g.read(n, "launch").known()?;
    s.session = launch
        .as_ref()
        .and_then(|l| l.get("sessionId"))
        .and_then(Value::as_str)
        .filter(|sid| crate::checks::session_id(sid))
        .map(str::to_owned);
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
