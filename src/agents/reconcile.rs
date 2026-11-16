//! restore-6b: reconcile every lifecycle member of a team; a Lost member whose last run succeeded comes back as
//! a new generation of the same id and conversation (#103).
use super::gens::Verdict;
use super::lifecycle::Life;

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
