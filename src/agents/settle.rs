//! settle-1b: the replay table (G3), the orphan fence (G8), and ending one generation (G11).
use std::path::Path;

use serde_json::json;

use super::gens::{self, Decision};
use super::release::{self, halt};
use super::team_state::{Guard, Tx};
use crate::error::{Refusal, Res};
use crate::hc::Lazy;
use crate::state::evidence::{Evidence, Published};

/// G3's two modes: `Normal` promotes a committed pending generation; `Fence` (G8, G11) never does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Fence,
}

/// What one settle decided (B-R4).
#[derive(Debug, Clone)]
pub enum Outcome {
    Done,
    Unknown(Refusal),
}

impl PartialEq for Outcome {
    fn eq(&self, o: &Outcome) -> bool {
        match (self, o) {
            (Outcome::Done, Outcome::Done) => true,
            (Outcome::Unknown(a), Outcome::Unknown(b)) => a.check == b.check,
            _ => false,
        }
    }
}

#[cfg(test)]
thread_local! {
    static RACE: std::cell::Cell<Option<fn(&Path)>> = const { std::cell::Cell::new(None) };
}
/// Test only: `f` runs once, between a row's read and its publish (rows 3 and 4, "lost: reread").
#[cfg(test)]
pub fn race_before_publish(f: Option<fn(&Path)>) {
    RACE.with(|r| r.set(f));
}
fn race(_member: &Path) {
    #[cfg(test)]
    if let Some(f) = RACE.with(|r| r.take()) {
        f(_member);
    }
}

/// One generation being settled. `Tx` and `Lazy` share the context lifetime `'c` (`Lazy` is invariant in it).
struct At<'a, 'c> {
    tx: &'a Tx<'c>,
    lazy: &'a Lazy<'c>,
    id: &'a str,
    n: u64,
    mode: Mode,
    pending: bool,
}

impl At<'_, '_> {
    /// G3 Done: drops `pending` only while it names n (`Guard::PendingGen`), so a replay drops it once.
    fn done(&self) -> Res<Outcome> {
        if self.pending {
            drop_field(
                self.tx,
                self.id,
                Guard::PendingGen { n: Some(self.n) },
                "pending",
            )?;
        }
        Ok(Outcome::Done)
    }
    /// G3 "lost: reread": the winner of a lost publication is read once more, through the whole table.
    fn again(&self) -> Res<Outcome> {
        settle_gen(self.tx, self.lazy, self.id, self.n, self.mode, self.pending)
    }
}

/// B-R2 over the one record writer: drops `field` while `guard` holds. A refused guard (`Ok(None)`) means the record
/// no longer names this generation there, so nothing is left to drop: a replay drops it once. A read or write
/// failure propagates.
fn drop_field(tx: &Tx<'_>, id: &str, guard: Guard<'_>, field: &'static str) -> Res<()> {
    tx.record_write(id, guard, |f| {
        f.remove(field);
        Ok(())
    })
    .map(|_| ())
}

/// B-R4: a continuation's refusal is Unknown (reserved); a halt (check `halted`, b-calls 36) stays an error.
fn unresolved(e: Refusal) -> Res<Outcome> {
    if e.check == "halted" {
        Err(e)
    } else {
        Ok(Outcome::Unknown(e))
    }
}

impl Tx<'_> {
    /// G3 for the `pending` the record names now; none is Done (ambiguity 12). The record is read through
    /// `gens::record`, the walk from the state root (b-calls 34), so a linked member folder refuses `evidence`. No
    /// record (a forward killed at `allocated`) is Done with nothing to do (b-calls 35).
    pub fn settle(&self, id: &str, mode: Mode) -> Res<Outcome> {
        let Some(r) = gens::record(&self.folder(id)).known()? else {
            return Ok(Outcome::Done);
        };
        match gens::pending(&r) {
            None => Ok(Outcome::Done),
            Some(n) => settle_gen(self, &Lazy::new(self.ctx), id, n, mode, true),
        }
    }
}

/// Call 49 (call 46 extended to settle): `ended`, `released` or `release` read once through `gens::read_flushed`, so
/// rows 0 and 0r act on it only once its folders are durable. `Ok`: Present (flushed), Absent, or Unreadable (row 1
/// decides). `Err`: the flush failed (check `state`, `gens::NOT_FLUSHED`).
fn flushed(member: &Path, n: u64, file: &str) -> Res<Evidence> {
    match gens::read_flushed(member, n, file) {
        Ok(Some(v)) => Ok(Evidence::Present(v)),
        Ok(None) => Ok(Evidence::Absent),
        Err(e) if e.check == "evidence" => Ok(Evidence::Unreadable),
        Err(e) => Err(e),
    }
}

/// G3, total and in the spec's order, for generation n (`pending`: it is the record's `pending`). Each of the seven
/// files is read once (ambiguity 4); `ended`, `released`, `release` through `flushed` (call 49). Used by `settle`,
/// `fence` (G8) and `terminate` (G11).
pub fn settle_gen<'c>(
    tx: &Tx<'c>,
    lazy: &Lazy<'c>,
    id: &str,
    n: u64,
    mode: Mode,
    pending: bool,
) -> Res<Outcome> {
    let at = At {
        tx,
        lazy,
        id,
        n,
        mode,
        pending,
    };
    let m = tx.folder(id);
    // Call 49: a failed flush stops the row with nothing done.
    let ended = match flushed(&m, n, "ended") {
        Ok(e) => e,
        Err(e) => return Ok(Outcome::Unknown(e)),
    };
    let released = match flushed(&m, n, "released") {
        Ok(e) => e,
        Err(e) => return Ok(Outcome::Unknown(e)),
    };
    // Row 0: a marker is decisive, even beside a torn sibling.
    if matches!(ended, Evidence::Present(_)) || matches!(released, Evidence::Present(_)) {
        return at.done();
    }
    let rel = match flushed(&m, n, "release") {
        Ok(e) => e,
        Err(e) => return Ok(Outcome::Unknown(e)),
    };
    // Row 0r: before row 1; finished from step 3 with the body read here (b-calls 15), never by a kill.
    if let (Evidence::Absent, Evidence::Present(body)) = (&released, &rel) {
        return match release::conclude(tx, lazy, id, n, body) {
            Ok(()) => at.done(),
            Err(e) => unresolved(e),
        };
    }
    let claim = gens::claim(&m, n);
    let commit = gens::read(&m, n, "commit");
    let term = gens::read(&m, n, "terminating");
    let launch = gens::read(&m, n, "launch");
    // Row 1.
    let all = [&ended, &released, &rel, &claim, &commit, &term, &launch];
    if all.iter().any(|e| **e == Evidence::Unreadable) {
        return Ok(Outcome::Unknown(crate::state::evidence::unreadable()));
    }
    let claim = match &claim {
        Evidence::Present(c) => Some(c.as_str()),
        _ => None,
    };
    match claim {
        // Row 3.
        None => {
            race(&m);
            match gens::publish_claim_once(&m, n, &json!("abandoned"))? {
                Published::Published => {
                    halt("abandoned")?;
                    at.done()
                }
                Published::Exists => at.again(),
            }
        }
        // Row 2.
        Some(Some("abandoned")) => at.done(),
        Some(Some("run")) => match commit {
            // Row 4.
            Evidence::Absent => {
                race(&m);
                match gens::publish_once(&m, n, "commit", &json!({"aborted": true}))? {
                    Published::Published => {
                        halt("aborted")?;
                        at.done()
                    }
                    Published::Exists => at.again(),
                }
            }
            Evidence::Present(c) => match gens::decide(&c) {
                // Row 5.
                Decision::Aborted => at.done(),
                Decision::Run => row6(&at, &term, &launch),
                // Ambiguity 3.
                Decision::Invalid => Ok(Outcome::Unknown(Refusal::internal(
                    "the generation's commit is invalid",
                ))),
            },
            Evidence::Unreadable => Ok(Outcome::Unknown(crate::state::evidence::unreadable())),
        },
        // Ambiguity 3: a claim neither run nor abandoned.
        Some(_) => Ok(Outcome::Unknown(Refusal::internal(
            "the generation's claim is invalid",
        ))),
    }
}

/// G3 row 6 (Task 9).
fn row6(_: &At<'_, '_>, _: &Evidence, _: &Evidence) -> Res<Outcome> {
    Ok(Outcome::Unknown(gens::unclear("row 6 is not settled yet")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::core_fixture as cf;
    use crate::agents::fixture::{ID, World};
    use crate::agents::life_fixture as lf;
    use crate::agents::team_state::Scope;
    use serde_json::Value;

    /// Every launch kind `pending.kind` can carry (spawn, resume, leader from A3b-1; retry, restore, fresh from
    /// A3b-2 and 6c). Settle never reads the kind; each row runs for each (F13).
    const KINDS: [&str; 6] = ["spawn", "resume", "leader", "retry", "restore", "fresh"];

    /// A Tx without `open`'s hooks, so each test scripts only the hc calls its own rows make.
    fn tx<'a>(w: &'a World, s: &Scope) -> Tx<'a> {
        Tx {
            ctx: &w.ctx,
            scope: s.clone(),
        }
    }
    fn settle(w: &World, s: &Scope, mode: Mode) -> Res<Outcome> {
        tx(w, s).settle(ID, mode)
    }
    /// A lifecycle member whose pending generation 1 is of `kind`.
    fn pending_of(w: &World, kind: &str) -> std::path::PathBuf {
        let mut p = lf::pending(1);
        p["kind"] = json!(kind);
        lf::life(w, json!({"pending": p}))
    }
    fn pending_member(w: &World) -> std::path::PathBuf {
        pending_of(w, "spawn")
    }
    /// Torn bytes at `at` (every OS).
    fn torn(at: &std::path::Path) {
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(at);
        std::fs::write(at, b"{").unwrap();
    }
    /// release-1d R1 step 1's body, as `release` publishes it.
    fn release_body() -> Value {
        json!({"address": "t1", "hcName": lf::hc_name(1), "cause": "lost", "at": "2026-11-15T00:00:00Z"})
    }

    // G3 rows 0, 2, 3, 4, 5 x each kind x each mode: Done, pending dropped once, no hc call.
    #[test]
    fn rows_zero_to_five_end_done_and_drop_pending_once() {
        let rows: [(&str, Option<&str>, Option<Value>, &str); 6] = [
            ("0 ended", Some("run"), lf::committed(), "ended"),
            ("0 released", Some("run"), lf::committed(), "released"),
            ("2 abandoned", Some("abandoned"), None, ""),
            ("3 no claim", None, None, ""),
            ("4 run, no commit", Some("run"), None, ""),
            ("5 aborted", Some("run"), Some(json!({"aborted": true})), ""),
        ];
        for (row, claim, commit, marker) in rows {
            for kind in KINDS {
                for mode in [Mode::Normal, Mode::Fence] {
                    let (w, s) = lf::world();
                    let m = pending_of(&w, kind);
                    lf::seed_gen(&m, 1, claim, commit.clone());
                    if !marker.is_empty() {
                        gens::publish_once(&m, 1, marker, &json!({})).unwrap();
                    }
                    let got = settle(&w, &s, mode).unwrap();
                    assert_eq!(got, Outcome::Done, "{row} {kind} {mode:?}");
                    assert!(lf::record(&m).get("pending").is_none(), "{row}: pending");
                    assert!(gens::done(&m, 1).unwrap(), "{row}: done");
                    assert!(lf::calls(&w).is_empty(), "{row}: hc called");
                    assert_eq!(
                        settle(&w, &s, mode).unwrap(),
                        Outcome::Done,
                        "{row}: replay"
                    );
                }
            }
        }
    }

    // G3 row 0r x each kind x each mode (F7, F8, F13; b-calls 15): a readable release.json without released.json is
    // finished from R1 step 3, before row 1, so a torn claim, commit, terminating or launch beside it never blocks
    // it. One relabel, Done, pending dropped; launch.json is never read and release.json is read once.
    #[test]
    fn a_half_done_release_is_finished_before_any_torn_sibling() {
        for sibling in ["none", "claim", "commit", "terminating", "launch"] {
            for kind in KINDS {
                for mode in [Mode::Normal, Mode::Fence] {
                    let (w, s) = lf::world();
                    let m = pending_of(&w, kind);
                    lf::seed_gen(&m, 1, Some("run"), lf::committed());
                    gens::publish_once(&m, 1, "release", &release_body()).unwrap();
                    match sibling {
                        "none" => {}
                        "claim" => torn(&gens::claim_path(&m, 1)),
                        f => torn(&gens::path(&m, 1, f)),
                    }
                    cf::online(&w);
                    lf::get(&w, "t1", &lf::lost(&w, 1, lf::LOST_ARGVS[0]));
                    lf::relabel(&w, "t1");
                    let (got, reads) = crate::state::fs::record_reads(|| settle(&w, &s, mode));
                    let case = format!("{sibling} {kind} {mode:?}");
                    assert_eq!(got.unwrap(), Outcome::Done, "{case}");
                    assert!(
                        matches!(gens::read(&m, 1, "released"), Evidence::Present(_)),
                        "{case}"
                    );
                    assert!(lf::record(&m).get("pending").is_none(), "{case}: pending");
                    assert_eq!(lf::count(&w, "label"), 1, "{case}");
                    lf::no_effect(&w);
                    w.fakes.hc_done().unwrap();
                    let n = |f: &str| reads.iter().filter(|p| **p == gens::path(&m, 1, f)).count();
                    assert_eq!(
                        (n("launch"), n("release")),
                        (0, 1),
                        "{case}: read once, no launch.json"
                    );
                }
            }
        }
    }

    // G3 row 1 x each kind x each mode (R7): any other Unreadable read (every OS: torn bytes) writes nothing further
    // and asks no hc. `release` first: with row 1 gone it would fall to row 4 (F18). A torn `released` beside a
    // readable `release` is row 1, not 0r. A marker Present is decisive beside a torn sibling.
    #[test]
    fn an_unreadable_read_writes_nothing() {
        for case in [
            "release",
            "released",
            "ended",
            "claim",
            "commit",
            "terminating",
            "launch",
        ] {
            for kind in KINDS {
                for mode in [Mode::Normal, Mode::Fence] {
                    let (w, s) = lf::world();
                    let m = pending_of(&w, kind);
                    lf::seed_gen(&m, 1, Some("run"), None);
                    match case {
                        "claim" => torn(&gens::claim_path(&m, 1)),
                        "released" => {
                            gens::publish_once(&m, 1, "release", &release_body()).unwrap();
                            torn(&gens::path(&m, 1, "released"));
                        }
                        f => torn(&gens::path(&m, 1, f)),
                    }
                    let c = format!("{case} {kind} {mode:?}");
                    let before = std::fs::read(m.join("record.json")).unwrap();
                    let got = settle(&w, &s, mode).unwrap();
                    assert!(
                        matches!(&got, Outcome::Unknown(e) if e.check == "evidence"),
                        "{c}: {got:?}"
                    );
                    assert_eq!(std::fs::read(m.join("record.json")).unwrap(), before, "{c}");
                    let commit = if case == "commit" {
                        Evidence::Unreadable
                    } else {
                        Evidence::Absent
                    };
                    assert_eq!(
                        gens::read(&m, 1, "commit"),
                        commit,
                        "{c}: nothing published"
                    );
                    assert!(lf::calls(&w).is_empty(), "{c}");
                }
            }
        }
        let (w, s) = lf::world();
        let m = pending_member(&w);
        lf::seed_gen(&m, 1, Some("run"), lf::committed());
        gens::publish_once(&m, 1, "released", &json!({})).unwrap();
        torn(&gens::path(&m, 1, "ended"));
        assert_eq!(
            settle(&w, &s, Mode::Normal).unwrap(),
            Outcome::Done,
            "a marker is decisive"
        );
    }

    // Ambiguity 3 x each kind x each mode (F13, R7): a claim neither run nor abandoned, or a commit that decides
    // nothing, is Unknown (`internal`), reserved: pending kept, nothing written, no hc. (Absent launch.json is row
    // 6, Task 9.)
    #[test]
    fn an_invalid_claim_or_commit_stays_reserved() {
        for (case, claim, commit) in [
            ("claim", "maybe", None),
            ("commit", "run", Some(json!({"x": 1}))),
        ] {
            for kind in KINDS {
                for mode in [Mode::Normal, Mode::Fence] {
                    let (w, s) = lf::world();
                    let m = pending_of(&w, kind);
                    lf::seed_gen(&m, 1, Some(claim), commit.clone());
                    let c = format!("{case} {kind} {mode:?}");
                    let before = std::fs::read(m.join("record.json")).unwrap();
                    let got = settle(&w, &s, mode).unwrap();
                    assert!(
                        matches!(&got, Outcome::Unknown(e) if e.check == "internal"),
                        "{c}: {got:?}"
                    );
                    assert_eq!(
                        std::fs::read(m.join("record.json")).unwrap(),
                        before,
                        "{c}: pending kept"
                    );
                    assert!(lf::calls(&w).is_empty(), "{c}");
                }
            }
        }
    }

    // b-calls 35 (R8): a forward killed at `allocated` left no record. settle is Done with nothing to do: nothing
    // made, no hc call.
    #[test]
    fn a_member_without_a_record_is_done() {
        for mode in [Mode::Normal, Mode::Fence] {
            let (w, s) = lf::world();
            assert_eq!(
                settle(&w, &s, mode).map_err(|e| e.check),
                Ok(Outcome::Done),
                "{mode:?}"
            );
            assert!(!w.folder(ID).exists(), "{mode:?}: nothing was made");
            assert!(lf::calls(&w).is_empty(), "{mode:?}");
        }
    }

    // settle-1b "Abandonment gap" and "Pending without spec": stopped after row 3's abandoned claim, before the
    // drop; three settles drop pending once; the claim is unchanged; nothing stays reserved.
    #[test]
    fn the_abandonment_gap_closes_once() {
        let (w, s) = lf::world();
        let m = pending_member(&w);
        release::halt_after(Some("abandoned"));
        let first = settle(&w, &s, Mode::Normal);
        release::halt_after(None);
        assert_eq!(first.map_err(|e| e.check), Err("halted"), "the halt");
        assert!(
            lf::record(&m).get("pending").is_some(),
            "pending survives the halt"
        );
        for _ in 0..3 {
            assert_eq!(settle(&w, &s, Mode::Normal).unwrap(), Outcome::Done);
        }
        assert_eq!(gens::claim(&m, 1), Evidence::Present(json!("abandoned")));
        assert!(lf::record(&m).get("pending").is_none());
        let gens::Member::Life(g) = gens::member_of(&m) else {
            panic!("not a lifecycle member")
        };
        assert!(g.status().unwrap().finished, "nothing reserved");
    }

    // G3 rows 3, 4, "lost: reread": the launcher's claim, or its give-up, lands between the read and the publish.
    #[test]
    fn a_lost_publish_is_reread() {
        let (w, s) = lf::world();
        let m = pending_member(&w);
        race_before_publish(Some(|m: &Path| {
            gens::publish_claim_once(m, 1, &json!("run")).unwrap();
        }));
        let got = settle(&w, &s, Mode::Normal);
        race_before_publish(None);
        assert_eq!(got.unwrap(), Outcome::Done);
        assert_eq!(
            gens::claim(&m, 1),
            Evidence::Present(json!("run")),
            "the launcher's claim stands"
        );
        assert_eq!(
            gens::read(&m, 1, "commit"),
            Evidence::Present(json!({"aborted": true})),
            "row 4 after reread"
        );
        // Row 4 lost to a commit: the reread reaches row 6 (Task 8: Unknown); pending kept.
        let (w, s) = lf::world();
        let m = pending_member(&w);
        lf::seed_gen(&m, 1, Some("run"), None);
        race_before_publish(Some(|m: &Path| {
            gens::publish_once(m, 1, "commit", &json!({"committed": true})).unwrap();
        }));
        let got = settle(&w, &s, Mode::Normal);
        race_before_publish(None);
        assert!(
            matches!(got.unwrap(), Outcome::Unknown(_)),
            "row 4: the committed winner is reread"
        );
        assert!(
            lf::record(&m).get("pending").is_some(),
            "row 4: pending kept"
        );
    }

    /// Fault armed on generation 1's folder flush for `f`, then cleared.
    fn armed<T>(m: &std::path::Path, f: impl FnOnce() -> T) -> T {
        crate::state::fs::fail_sync_dir(Some(&gens::dir(m, 1)));
        let got = f();
        crate::state::fs::fail_sync_dir(None);
        got
    }
    fn not_flushed(got: &Outcome) -> bool {
        matches!(got, Outcome::Unknown(e) if e.check == "state" && e.message == gens::NOT_FLUSHED)
    }

    // Call 49, row 0: a marker settles only once its folders flush. Armed: Unknown (`state`, NOT_FLUSHED), the
    // record (pending) unchanged, no hc. Cleared: Done, pending dropped once.
    #[test]
    fn a_marker_settles_only_once_flushed() {
        for marker in ["ended", "released"] {
            let (w, s) = lf::world();
            let m = pending_member(&w);
            lf::seed_gen(&m, 1, Some("run"), lf::committed());
            gens::publish_once(&m, 1, marker, &json!({})).unwrap();
            let before = std::fs::read(m.join("record.json")).unwrap();
            let got = armed(&m, || settle(&w, &s, Mode::Normal)).unwrap();
            assert!(not_flushed(&got), "{marker} armed: {got:?}");
            let after = std::fs::read(m.join("record.json")).unwrap();
            assert_eq!(after, before, "{marker} armed: the record changed");
            assert!(lf::calls(&w).is_empty(), "{marker} armed: hc");
            assert_eq!(
                settle(&w, &s, Mode::Normal).unwrap(),
                Outcome::Done,
                "{marker} cleared"
            );
            assert!(
                lf::record(&m).get("pending").is_none(),
                "{marker} cleared: pending"
            );
        }
    }

    // Call 49, row 0r: release.json is concluded only once its folders flush. Armed: Unknown (`state`), no hc call,
    // no relabel, no released.json, the record unchanged. Cleared: one relabel, released.json, Done.
    #[test]
    fn a_half_done_release_is_concluded_only_once_flushed() {
        let (w, s) = lf::world();
        let m = pending_member(&w);
        lf::seed_gen(&m, 1, Some("run"), lf::committed());
        gens::publish_once(&m, 1, "release", &release_body()).unwrap();
        cf::online(&w);
        lf::get(&w, "t1", &lf::lost(&w, 1, lf::LOST_ARGVS[0]));
        lf::relabel(&w, "t1");
        let before = std::fs::read(m.join("record.json")).unwrap();
        let got = armed(&m, || settle(&w, &s, Mode::Normal)).unwrap();
        assert!(
            lf::calls(&w).is_empty(),
            "armed: hc asked {:?}",
            lf::calls(&w)
        );
        assert!(not_flushed(&got), "armed: {got:?}");
        assert_eq!(
            gens::read(&m, 1, "released"),
            Evidence::Absent,
            "armed: released.json"
        );
        assert_eq!(
            std::fs::read(m.join("record.json")).unwrap(),
            before,
            "armed: the record changed"
        );
        assert_eq!(
            settle(&w, &s, Mode::Normal).unwrap(),
            Outcome::Done,
            "cleared"
        );
        assert_eq!(lf::count(&w, "label"), 1, "cleared: one relabel");
        assert!(
            matches!(gens::read(&m, 1, "released"), Evidence::Present(_)),
            "cleared: released.json"
        );
        assert!(lf::record(&m).get("pending").is_none(), "cleared: pending");
        lf::no_effect(&w);
        w.fakes.hc_done().unwrap();
    }
}
