//! reentry-6a E6 replay: the pure table. Every later `up` resumes from the first unfinished step.
use super::index::Index;

/// The team's leader at the anchor, in its session (ambiguity 9, call 7): none (or only void ones), one with
/// `pending`, one without.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leader {
    Absent,
    Pending,
    Active,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Repair,
    Session,
    Launch,
    Settle,
    Team,
    Reenter,
}

/// E6, calls 19, 21: a team record re-enters; an active leader this call launched writes the team record first; an
/// active leader it did not launch (a crash after promotion) re-enters, where reconcile runs and the record is made.
pub fn next(ix: &Index, leader: Leader, team_record: bool, launched: bool) -> Step {
    if team_record {
        return Step::Reenter;
    }
    match leader {
        Leader::Active if launched => Step::Team,
        Leader::Active => Step::Reenter,
        Leader::Pending => Step::Settle,
        Leader::Absent if ix.anchor.is_some() => Step::Launch,
        Leader::Absent if ix.pending.is_some() => Step::Repair,
        Leader::Absent => Step::Session,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::up::index::{Index, Inputs, Pending, Server, View};
    use crate::caller::Anchor;

    fn s() -> Server {
        Server {
            socket: "/s".into(),
            pid: 1,
            start: 1,
            key: crate::agents::up::index::key_of("/s", 1, 1),
        }
    }
    fn ix(anchor: bool, view: bool, pending: bool) -> Index {
        Index {
            team: "t".into(),
            inputs: Inputs {
                client: "claude".into(),
                options: vec![],
                prompt: vec![],
                name: String::new(),
                folder: "/p".into(),
            },
            anchor: anchor.then(|| Anchor {
                key: "k".into(),
                session: "$1".into(),
            }),
            view: view.then(|| View {
                server: s(),
                session: "$1".into(),
                name: "n".into(),
            }),
            pending: pending.then(|| Pending {
                server: s(),
                name: "n".into(),
                release: "/r".into(),
            }),
            aliases: vec![],
            releases: vec![],
        }
    }

    /// E6's table as the spec states it, row by row, independent of `next`'s code.
    fn spec_row(anchor: bool, pending: bool, leader: Leader, team: bool, launched: bool) -> Step {
        if team {
            return Step::Reenter; // "team record: re-entry (E4)"
        }
        match leader {
            Leader::Active if launched => Step::Team, // "leader active, no team record: step 4"
            Leader::Active => Step::Reenter, // call 19: a promotion this call did not make re-enters
            Leader::Pending => Step::Settle, // "leader with pending: settle-1b G3"
            Leader::Absent if anchor => Step::Launch, // "anchor, no leader record: step 3"
            Leader::Absent if pending => Step::Repair, // "index, pending, no anchor: E3.1"
            Leader::Absent => Step::Session, // "index with inputs only: step 2"
        }
    }

    // Call 21: `next` answers every combination the reader accepts (a view only under an anchor; a leader and a
    // team record only at an anchor), and each answer is the spec's row.
    #[test]
    fn the_replay_table_is_total_over_every_accepted_state() {
        let mut seen = 0;
        for anchor in [false, true] {
            for view in [false, true] {
                for pending in [false, true] {
                    for leader in [Leader::Absent, Leader::Pending, Leader::Active] {
                        for team in [false, true] {
                            for launched in [false, true] {
                                let accepted =
                                    anchor || (!view && leader == Leader::Absent && !team);
                                if !accepted {
                                    continue;
                                }
                                let got = next(&ix(anchor, view, pending), leader, team, launched);
                                let want = spec_row(anchor, pending, leader, team, launched);
                                assert_eq!(
                                    got, want,
                                    "anchor {anchor} view {view} pending {pending} {leader:?} team {team} launched {launched}"
                                );
                                seen += 1;
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(
            seen,
            4 + 48,
            "every accepted combination: 4 without an anchor, 48 with one"
        );
    }
}
