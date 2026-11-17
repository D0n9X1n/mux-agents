//! restore-6c Task 2 (#103): a retry's gates. Every generation done, the cap, a free conversation; a Lost failed
//! generation (or one a crash left half released) is released `cause: recovery`, never killed.
use super::core_fixture as cf;
use super::core_launch::forward_tests as ft;
use super::fixture::World;
use super::reconcile_await_tests::{six_running, snapshot};
use super::reconcile_restore_tests::{L, released_by_hc, unmanaged_claude};
use super::reconcile_tests::{X, found, member, wire_of};
use super::reconcile_verb_tests::{person, team};
use super::recover_tests::{both_gone, child, cli, failed, failed_ended, spec, stopped};
use super::resume::{Action, recover};
use super::{gens, life_fixture as lf, prelife_fixture::bytes};
use serde_json::{Map, Value, json};
use std::path::PathBuf;

type Got = crate::error::Res<Option<Map<String, Value>>>;

/// One gate scenario in the real call order. X is L's child; `person()`'s queued `--version` is the first
/// connection.
pub(crate) fn gate_case(case: &str, action: Action) -> (World, PathBuf, Got) {
    let w = person();
    team(&w);
    let f = member(&w, X, json!({"parentAgent": {"id": L}}));
    match case {
        // observe: generation 1 failed, not done, and hc shows it Running (the client still lives)
        "running" | "running-stopped" => {
            failed(&f, 1);
            if case == "running-stopped" {
                stopped(&f);
            }
            found(&w, X, 1, &wire_of(&w, X, 1, &lf::ARGV));
        }
        // the fence at `Tx::open`: committed orphan 0, hc lists no terminal for it: it stays reserved
        "older" => {
            failed_ended(&f, 1);
            lf::seed_gen(&f, 0, Some("run"), lf::committed());
            cf::answer(
                &w,
                &[
                    "ls",
                    "--all",
                    "--label",
                    &format!("mux.id={X}"),
                    "--label",
                    "mux.gen=0",
                    "--json",
                ],
                r#"{"terminals":[]}"#,
            );
        }
        // the cap: six Running members hold every slot
        "capped" => {
            failed_ended(&f, 1);
            let live: Vec<String> = six_running(&w).iter().map(|id| format!("a{id}")).collect();
            let live: Vec<&str> = live.iter().map(String::as_str).collect();
            snapshot(&w, &live, &[&format!("a{L}"), &format!("a{X}")]);
        }
        // the conversation is held by a live unmanaged Claude (occupy-3 H4.4), asked after the cap
        "occupied" => {
            failed_ended(&f, 1);
            unmanaged_claude(&w, 4321);
            both_gone(&w);
            if action == Action::Fresh {
                w.fakes.hc_answer(ft::address("t2")); // fresh asks no occupancy and launches
            }
        }
        other => panic!("{other}"),
    }
    let got = recover(&w.ctx, &w.caller(), action, X);
    (w, f, got)
}

/// P2, ambiguity 3: each gate refuses before anything is launched, and asks hc only what it scripted.
#[test]
fn a_retry_meets_every_gate() {
    for (case, check, text) in [
        (
            "running",
            "running",
            "generation 1 of mbbb may still run at abbbbbbbbbbbb",
        ),
        (
            "running-stopped",
            "running",
            "generation 1 of mbbb may still run",
        ),
        ("older", "running", "generation 0 of mbbb"),
        ("capped", "limit", ""),
        ("occupied", "running", "process 4321"),
    ] {
        let (w, f, got) = gate_case(case, Action::Retry);
        let e = got.unwrap_err();
        assert_eq!((e.code, e.check), (2, check), "{case}: {}", e.message);
        assert!(e.message.contains(text), "{case}: {}", e.message);
        assert!(spec(&f, 2).is_none(), "{case}: nothing launched");
        w.fakes.hc_done().unwrap();
    }
}

/// P2, checklist 3, 14: a failed generation hc shows Lost (every `LOST_ARGVS` shape), stopped or not, is released
/// `cause: recovery` through R1 (one reread, one relabel, never `kill`), then retried as n+1.
#[test]
fn a_lost_failed_member_is_released_for_recovery_and_retried() {
    for argv in lf::LOST_ARGVS {
        for stop in [false, true] {
            let w = person();
            team(&w);
            let f = member(&w, X, json!({"parentAgent": {"id": L}}));
            failed(&f, 1);
            if stop {
                stopped(&f);
            }
            found(&w, X, 1, &wire_of(&w, X, 1, argv));
            released_by_hc(&w, X, argv);
            both_gone(&w);
            w.fakes.hc_answer(ft::address("t2"));
            let (code, reply) = cli(&w, &["resume", X]);
            let at = format!("{argv:?} stopped {stop}");
            assert_eq!(
                (code, &reply["agent"]["generation"], &reply["released"]),
                (0, &json!(2), &json!(1)),
                "{at}: {reply}"
            );
            let rel = gens::read_flushed(&f, 1, "release")
                .unwrap()
                .expect("release.json");
            assert_eq!(rel["cause"], "recovery", "{at}: release-1d R2");
            assert!(
                gens::read_flushed(&f, 1, "released").unwrap().is_some(),
                "{at}: done"
            );
            lf::no_effect(&w);
            w.fakes.hc_done().unwrap();
        }
    }
}

/// Checklist 2: a recovery killed after its Lost release wrote `release.json`, before `released.json`, is
/// finished by the next recovery itself (before any `restore`): `cause: recovery`, no kill, one successor.
#[test]
fn a_recovery_replays_its_own_interrupted_lost_release() {
    let w = person();
    team(&w);
    let f = member(&w, X, json!({"parentAgent": {"id": L}}));
    failed(&f, 1);
    found(&w, X, 1, &wire_of(&w, X, 1, lf::LOST_ARGVS[0]));
    super::release::halt_after(Some("release"));
    let (code, first) = cli(&w, &["resume", X]);
    super::release::halt_after(None);
    assert_eq!((code, &first["check"]), (1, &json!("halted")), "{first}");
    assert!(
        gens::read_flushed(&f, 1, "released").unwrap().is_none(),
        "half released"
    );
    w.fakes.hc_done().unwrap();
    cf::online(&w);
    released_by_hc(&w, X, lf::LOST_ARGVS[0]); // the replay concludes from step 3: the reread, the one relabel
    both_gone(&w);
    w.fakes.hc_answer(ft::address("t2"));
    let (code, second) = cli(&w, &["resume", X]);
    assert_eq!(
        (code, &second["agent"]["generation"]),
        (0, &json!(2)),
        "{second}"
    );
    let rel = gens::read_flushed(&f, 1, "release")
        .unwrap()
        .expect("release.json");
    assert_eq!(rel["cause"], "recovery");
    assert!(
        gens::read_flushed(&f, 1, "released").unwrap().is_some(),
        "finished"
    );
    assert_eq!(lf::count(&w, "run"), 1, "one successor");
    lf::no_effect(&w);
    w.fakes.hc_done().unwrap();
}

/// 1-4 F10, checklist 5: a pending generation settle cannot decide (claim `run`, committed, hc lists nothing)
/// holds the retry `running` with the pending evidence unchanged; a torn pending launch refuses `evidence`.
#[test]
fn a_retry_waits_for_an_unresolved_pending_generation() {
    for torn in [false, true] {
        let w = person();
        team(&w);
        let f = child(&w);
        lf::seed_gen(&f, 2, Some("run"), lf::committed());
        let mut r = lf::record(&f);
        r["pending"] = lf::pending(2);
        crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
        if torn {
            let l = gens::path(&f, 2, "launch");
            std::fs::remove_file(&l).unwrap();
            std::fs::write(&l, "{torn").unwrap();
        } else {
            cf::answer(
                &w,
                &[
                    "ls",
                    "--all",
                    "--label",
                    &format!("mux.id={X}"),
                    "--label",
                    "mux.gen=2",
                    "--json",
                ],
                r#"{"terminals":[]}"#,
            );
        }
        let before = bytes(&f);
        let (code, reply) = cli(&w, &["resume", X]);
        let want = if torn {
            (1, "evidence")
        } else {
            (2, "running")
        };
        assert_eq!(
            (code, &reply["check"]),
            (want.0, &json!(want.1)),
            "torn {torn}: {reply}"
        );
        assert_eq!(
            bytes(&f),
            before,
            "torn {torn}: the pending evidence is unchanged"
        );
        assert_eq!(lf::count(&w, "run"), 0, "torn {torn}: nothing launched");
        if !torn {
            w.fakes.hc_done().unwrap();
        }
    }
}

/// 6b L1: the leader takes no cap slot; its retry launches beside six Running members with no listing.
#[test]
fn the_leaders_retry_takes_no_slot() {
    let w = person();
    let lead = member(&w, L, json!({"depth": 0, "upTeam": "T", "name": "leader"}));
    failed_ended(&lead, 1);
    six_running(&w);
    w.fakes.hc_answer(ft::address("t2")); // the one connection is forward's
    let reply = recover(&w.ctx, &w.caller(), Action::Retry, L)
        .unwrap()
        .expect("a recovery");
    assert_eq!(reply["agent"]["generation"], 2);
    w.fakes.hc_done().unwrap();
}
