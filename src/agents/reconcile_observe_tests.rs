//! restore-6b Task 2: `observe` reads a member once and asks hc only about a generation not done (6b-R1).
use super::core_fixture as cf;
use super::fixture::World;
use super::gens::Verdict;
use super::life_fixture as lf;
use super::lifecycle::Life;
use super::reconcile::{Row, classify, observe};
use super::reconcile_tests::{X, found, member, observed, ok, wire_of};
use super::team_state::{self, Scope};
use crate::hc::Lazy;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

fn outcome_of(f: &Path) -> Option<Value> {
    super::gens::read(f, 1, "outcome").known().unwrap()
}
fn gen_locks(w: &World) -> Vec<String> {
    w.fakes
        .locks()
        .into_iter()
        .filter(|l| l.contains("gen|"))
        .collect()
}

#[test]
fn a_done_or_stopped_generation_asks_hc_nothing() {
    for case in ["released", "ended", "stopped"] {
        let w = World::human("");
        w.state();
        let f = member(&w, X, json!({}));
        if case == "stopped" {
            crate::state::fs::write_json(
                &f.join("desired.json"),
                &json!({"desired": "stopped", "at": "t"}),
            )
            .unwrap();
        } else {
            crate::state::evidence::publish(&super::gens::path(&f, 1, case), &json!({"at": "t"}))
                .unwrap();
        }
        w.fakes.hc_install("hc"); // installed, nothing scripted: a stray call is logged (finding 12)
        let got = observed(&w, X).unwrap();
        assert!(lf::calls(&w).is_empty(), "{case}: {:?}", lf::calls(&w));
        assert_eq!(
            classify(&got),
            if case == "stopped" {
                Row::Stopped
            } else {
                Row::Ended
            },
            "{case}"
        );
        // r6b call 22 (checklist 8): a stopped member is not adjudicated: no gen lock, no outcome.
        assert!(gen_locks(&w).is_empty(), "{case}: {:?}", gen_locks(&w));
        assert!(outcome_of(&f).is_none(), "{case}");
    }
}

/// r6b call 29 (checklist 27): a done generation reports its recorded verdict, without hc or a second adjudication.
#[test]
fn a_done_generation_reports_its_recorded_verdict() {
    for case in ["ended", "recovery"] {
        let w = World::human("");
        w.state();
        let f = member(&w, X, json!({}));
        let publish = |file: &str, body: Value| {
            crate::state::evidence::publish(&super::gens::path(&f, 1, file), &body).unwrap();
        };
        if case == "ended" {
            publish("ended", json!({"at": "t"}));
        } else {
            publish(
                "release",
                json!({"address": "a", "hcName": lf::hc_name(1), "cause": "recovery", "at": "t"}),
            );
            publish("released", json!({"at": "t"}));
        }
        publish(
            "failure",
            json!({"reason": "terminal lost", "evidence": {}, "at": "t"}),
        );
        publish(
            "outcome",
            json!({"outcome": "failed", "reason": "terminal lost", "at": "t"}),
        );
        w.fakes.hc_install("hc");
        let got = observed(&w, X).unwrap();
        assert_eq!(
            got.verdict,
            Some(Verdict::Failed("terminal lost".into())),
            "{case}"
        );
        assert!(lf::calls(&w).is_empty(), "{case}: {:?}", lf::calls(&w));
        assert!(
            gen_locks(&w).is_empty(),
            "{case}: no second adjudication {:?}",
            gen_locks(&w)
        );
    }
}

#[test]
fn an_ended_terminal_is_recorded_before_it_counts() {
    let w = World::human("");
    w.state();
    let f = member(&w, X, json!({}));
    cf::online(&w);
    let mut t = wire_of(&w, X, 1, &lf::ARGV);
    (t["state"], t["reason"], t["exit_code"]) = (json!("finished"), json!("exited"), json!(0));
    found(&w, X, 1, &t);
    assert_eq!(classify(&observed(&w, X).unwrap()), Row::Ended);
    let ended = super::gens::read(&f, 1, "ended")
        .known()
        .unwrap()
        .expect("ended.json recorded");
    assert_eq!(
        (&ended["hcName"], &ended["reason"]),
        (&json!(lf::hc_name(1)), &json!("exited"))
    );
    // r6b call 23: the end is recorded, then adjudicated from it: an end before readiness fails.
    let outcome = outcome_of(&f).expect("adjudicated");
    assert_eq!(outcome["outcome"], "failed");
    w.fakes.hc_done().unwrap();
}

/// r6b call 13: every active generation not done is adjudicated, whatever its row: Running with `ready.json` is `ok`.
#[test]
fn a_running_generation_not_done_is_adjudicated_too() {
    let w = World::human("");
    w.state();
    let f = member(&w, X, json!({}));
    crate::state::evidence::publish(&super::gens::path(&f, 1, "ready"), &json!({"at": "t"}))
        .unwrap();
    cf::online(&w);
    found(&w, X, 1, &wire_of(&w, X, 1, &lf::ARGV));
    assert_eq!(classify(&observed(&w, X).unwrap()), Row::Running);
    assert_eq!(outcome_of(&f).expect("adjudicated")["outcome"], "ok");
    w.fakes.hc_done().unwrap();
}

/// r6b call 24 (checklist 3): an unresolved pending n+1 keeps the member Unknown, its active n is still adjudicated.
#[test]
fn a_pending_member_still_has_its_active_generation_adjudicated() {
    let w = World::human("");
    w.state();
    let f = member(&w, X, json!({"pending": lf::pending(2)}));
    crate::state::evidence::publish(&super::gens::path(&f, 1, "ready"), &json!({"at": "t"}))
        .unwrap();
    w.fakes.hc_install("hc"); // nothing scripted: pending asks hc nothing
    let got = observed(&w, X).unwrap();
    assert_eq!(classify(&got), Row::Unknown("pending".into()));
    assert_eq!(outcome_of(&f).expect("adjudicated")["outcome"], "ok");
    assert!(lf::calls(&w).is_empty(), "{:?}", lf::calls(&w));
}

/// Checklist 7: an hc-Unknown generation is adjudicated with `seen: None` (outcome-2's deadline legs decide it).
#[test]
fn an_hc_unknown_generation_is_adjudicated_by_its_deadline() {
    let w = World::human("");
    w.state();
    let f = member(&w, X, json!({}));
    let spec = json!({"id": X, "generation": "1", "sessionId": lf::SID, "deadline": "1970-01-01T00:00:00.000Z"});
    crate::state::evidence::publish(&super::gens::path(&f, 1, "spec"), &spec).unwrap();
    cf::online(&w);
    lf::fails(
        &w,
        &[
            "ls",
            "--all",
            "--label",
            &format!("mux.id={X}"),
            "--label",
            "mux.gen=1",
            "--json",
        ],
    );
    assert_eq!(
        classify(&observed(&w, X).unwrap()),
        Row::Unknown("hc".into())
    );
    assert_eq!(outcome_of(&f).expect("adjudicated")["outcome"], "failed");
    w.fakes.hc_done().unwrap();
}

/// r6b call 28 (checklist 25): a generation hc does not list (`Found::Missing`) is Unknown, never Lost or relaunched.
#[test]
fn a_generation_hc_does_not_list_is_unknown() {
    let w = World::human("");
    w.state();
    let f = member(&w, X, json!({}));
    ok(&f, 1);
    cf::online(&w);
    cf::answer(
        &w,
        &[
            "ls",
            "--all",
            "--label",
            &format!("mux.id={X}"),
            "--label",
            "mux.gen=1",
            "--json",
        ],
        &json!({"terminals": []}).to_string(),
    );
    let got = observed(&w, X).unwrap();
    assert_eq!(got.life, Some(Life::Unknown));
    assert_eq!(classify(&got), Row::Unknown("hc".into()));
    lf::no_effect(&w);
    assert_eq!(lf::count(&w, "run"), 0);
    w.fakes.hc_done().unwrap();
}

/// r6b call 14: unreadable completion evidence of the active generation refuses before hc is asked.
#[test]
fn unreadable_active_evidence_refuses_before_hc() {
    let w = World::human("");
    w.state();
    let f = member(&w, X, json!({}));
    ok(&f, 1);
    let commit = super::gens::path(&f, 1, "commit");
    std::fs::remove_file(&commit).unwrap();
    std::fs::create_dir_all(&commit).unwrap(); // a folder where the file belongs
    w.fakes.hc_install("hc"); // nothing scripted: any call is logged
    assert_eq!(observed(&w, X).unwrap_err().check, "evidence");
    assert!(lf::calls(&w).is_empty(), "{:?}", lf::calls(&w));
}

/// r6b calls 19, 25 (checklist 4): `observe` reads each evidence file once, absent reads included. The named
/// exceptions are other writers' own reads: outcome-2's `fresh` (claim, commit) and failure legs (ended, launch in
/// `lost`), and `record_end`'s launch read. The full list is pinned.
#[test]
fn observe_reads_each_evidence_file_once() {
    let w = World::human("");
    w.state();
    let f = member(&w, X, json!({}));
    cf::online(&w);
    let mut t = wire_of(&w, X, 1, &lf::ARGV);
    (t["state"], t["reason"], t["exit_code"]) = (json!("finished"), json!("exited"), json!(0));
    found(&w, X, 1, &t);
    let s = Scope::of(&w.caller());
    let reads = team_state::open(&w.ctx, &s, |tx| {
        let lazy = Lazy::new(tx.ctx);
        let super::gens::Member::Life(g) = super::gens::member_of(&tx.folder(X)) else {
            panic!("member")
        };
        let (got, reads) = crate::state::evidence::record_evidence_reads(|| observe(tx, &lazy, &g));
        got.map(|_| reads)
    })
    .unwrap();
    let mut count: BTreeMap<String, usize> = BTreeMap::new();
    for p in reads {
        let rel = p
            .strip_prefix(&f)
            .unwrap_or(&p)
            .to_string_lossy()
            .replace('\\', "/");
        *count.entry(rel).or_default() += 1;
    }
    let want: BTreeMap<String, usize> = OBSERVE_READS
        .iter()
        .map(|(p, n)| (p.to_string(), *n))
        .collect();
    assert_eq!(count, want);
    w.fakes.hc_done().unwrap();
}
/// Each file read while `observe` decides an Ended generation 1, and how often. `observe`'s own reads are one each
/// (desired, then `marks`: claim, commit, ended, released; then release, launch). The rest belong to other writers,
/// named (r6b call 25): `record_end` reads launch, and its publisher reads the `ended.json` leaf before linking;
/// outcome-2's `promote_held` reads outcome, failure, ready, report; `fresh` rereads claim and commit; the failure
/// legs read exit and ended; its publisher reads the failure and outcome leaves before linking.
const OBSERVE_READS: &[(&str, usize)] = &[
    ("desired.json", 1),
    ("gens/1.claim", 2),        // marks; fresh
    ("gens/1/commit.json", 2),  // marks; fresh
    ("gens/1/ended.json", 3),   // marks; record_end's publisher; failure leg
    ("gens/1/exit.json", 1),    // failure leg
    ("gens/1/failure.json", 2), // promote_held; its publisher
    ("gens/1/launch.json", 2),  // observe; record_end
    ("gens/1/outcome.json", 2), // promote_held; its publisher
    ("gens/1/ready.json", 1),   // promote_held
    ("gens/1/release.json", 1),
    ("gens/1/released.json", 1),
    ("gens/1/report.json", 1), // promote_held
];

#[test]
fn a_lost_terminal_is_adjudicated_with_what_hc_showed() {
    for argv in lf::LOST_ARGVS {
        for with_ok in [true, false] {
            let w = World::human("");
            w.state();
            let f = member(&w, X, json!({}));
            if with_ok {
                ok(&f, 1);
            }
            cf::online(&w);
            found(&w, X, 1, &wire_of(&w, X, 1, argv));
            let want = if with_ok {
                Row::Lost
            } else {
                Row::Recover("terminal lost".into())
            };
            assert_eq!(
                classify(&observed(&w, X).unwrap()),
                want,
                "{argv:?} ok={with_ok}"
            );
            lf::no_effect(&w);
            w.fakes.hc_done().unwrap();
        }
    }
}

#[test]
fn an_unreadable_release_is_unknown() {
    let w = World::human("");
    w.state();
    let f = member(&w, X, json!({}));
    std::fs::create_dir_all(super::gens::path(&f, 1, "release")).unwrap(); // a folder where the file belongs
    w.fakes.hc_install("hc"); // installed, nothing scripted: a stray call is logged
    assert_eq!(observed(&w, X).unwrap_err().check, "evidence");
    assert!(lf::calls(&w).is_empty());
}

/// r6b call 22 (checklist 8): hc unreachable is Unknown, never Lost, and nothing is adjudicated.
#[test]
fn hc_unreachable_is_unknown_never_lost() {
    let w = World::human("");
    w.state();
    let f = member(&w, X, json!({}));
    crate::state::evidence::publish(&super::gens::path(&f, 1, "ready"), &json!({"at": "t"}))
        .unwrap();
    w.fakes.hc_install("hc");
    lf::fails(&w, &["--version"]);
    assert_eq!(
        classify(&observed(&w, X).unwrap()),
        Row::Unknown("hc".into())
    );
    w.fakes.hc_done().unwrap(); // the failing `--version` was asked (finding 12)
    assert!(gen_locks(&w).is_empty(), "{:?}", gen_locks(&w));
    assert!(outcome_of(&f).is_none());
}
