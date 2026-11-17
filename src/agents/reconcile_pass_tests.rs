//! restore-6c Task 7 (#103): reconcile's one pass: every step refusal reported, the leader from 6a (calls 17, 18, 25).
use super::core_launch::forward_tests as ft;
use super::fixture::World;
use super::life_fixture as lf;
use super::prelife_fixture::bytes;
use super::reconcile::{Pass, PassFor, faults, pass};
use super::reconcile_restore_tests::{L, other, released_by_hc};
use super::reconcile_run_tests::ids;
use super::reconcile_tests::{X, found, member, ok, wire_of};
use super::reconcile_verb_tests::{person, restore, team};
use super::team_state::Tx;
use crate::error::Res;
use crate::hc::Lazy;
use serde_json::{Value, json};
use std::cell::Cell;
use std::path::{Path, PathBuf};

thread_local! { static STEPS: Cell<u32> = const { Cell::new(0) }; }
/// A leader step that only counts its calls (5-9 F5: the forced tests prove the step ran).
fn counted(_: &Tx<'_>, _: &Lazy<'_>, _: &str) -> Res<Option<Value>> {
    STEPS.with(|s| s.set(s.get() + 1));
    Ok(None)
}
/// A leader step that refuses as an unknown client would (call 19).
fn refusing(_: &Tx<'_>, _: &Lazy<'_>, _: &str) -> Res<Option<Value>> {
    crate::error::refuse(1, "client", "unknown client")
}
fn steps() -> u32 {
    STEPS.with(|s| s.replace(0))
}
fn up_for(step: super::leader::LeaderStep) -> PassFor<'static> {
    PassFor::Up {
        leader: Some(L),
        step: Some(step),
    }
}
fn pass_with(w: &World, step: super::leader::LeaderStep) -> Value {
    match pass(&w.ctx, &w.caller(), up_for(step)).unwrap() {
        Pass::Done(m) => Value::Object(m),
        Pass::Stopped => panic!("stopped"),
    }
}
fn folder(w: &World, id: &str) -> PathBuf {
    w.ctx
        .state_root
        .join(w.caller().server_key)
        .join("agents")
        .join(id)
}
fn ended(f: &Path, code: i64) {
    let end = json!({"address": "a", "hcName": lf::hc_name(1), "labels": {}, "state": "finished",
        "reason": "exited", "exitCode": code, "at": "t"});
    crate::state::evidence::publish(&super::gens::path(f, 1, "ended"), &end).unwrap();
}
/// The leader L, generation 1 ready, `ok` and ended: decided with no hc call, so its step runs.
fn leader(w: &World) -> PathBuf {
    let lead = member(w, L, json!({"depth": 0, "upTeam": "T", "name": "leader"}));
    ok(&lead, 1);
    ended(&lead, 0);
    lead
}
/// X Lost: found, then released (R1) and relaunched as generation 2.
fn lost_x(w: &World) {
    ok(&member(w, X, json!({})), 1);
    found(w, X, 1, &wire_of(w, X, 1, lf::LOST_ARGVS[0]));
    released_by_hc(w, X, lf::LOST_ARGVS[0]);
    w.fakes.hc_answer(ft::address("t2"));
}

/// Spec "leader through reconcile": `restore` restores the Lost `ok` leader as n+1 (no task, no cap slot: no
/// listing is scripted); an Ended leader is untouched.
#[test]
fn restore_restores_a_lost_leader_without_a_slot_and_leaves_an_ended_one() {
    for argv in lf::LOST_ARGVS {
        let w = person();
        let lead = member(&w, L, json!({"depth": 0, "upTeam": "T", "name": "leader"}));
        ok(&lead, 1);
        super::core_fixture::online(&w); // the pass (discovery took `person()`'s `--version`)
        found(&w, L, 1, &wire_of(&w, L, 1, argv));
        released_by_hc(&w, L, argv);
        w.fakes.hc_answer(ft::address("t2"));
        let (code, reply) = restore(&w);
        assert_eq!(code, 0, "{argv:?} {reply}");
        assert_eq!(
            ids(reply.as_object().unwrap(), "restored"),
            vec![L.to_string()],
            "{argv:?}"
        );
        let spec = super::gens::read_flushed(&lead, 2, "spec")
            .unwrap()
            .unwrap();
        assert!(spec.get("prompt").is_none(), "{argv:?}");
        w.fakes.hc_done().unwrap();
    }
    let w = person();
    team(&w);
    let before = bytes(&folder(&w, L));
    let (code, reply) = restore(&w);
    assert_eq!(
        (code, reply["restored"].as_array().unwrap().len()),
        (0, 0),
        "{reply}"
    );
    assert_eq!(
        bytes(&folder(&w, L)),
        before,
        "restore never resumes an Ended leader (P4)"
    );
    w.fakes.hc_done().unwrap();
}

/// P4.1-2: a leader desired `stopped` (the real `Tx::desired`) ends the pass after the cascade finish, before any
/// settle: X's pending generation is untouched, the step never runs and hc is never asked.
#[test]
fn a_stopped_leader_stops_the_pass_before_any_settle() {
    let w = person();
    team(&w);
    let scope = super::scope::state(&w.caller());
    super::team_state::open(&w.ctx, &scope, |tx| tx.desired(L, false)).unwrap();
    let f = member(&w, X, json!({}));
    lf::seed_gen(&f, 2, None, None);
    let mut r = lf::record(&f);
    r["pending"] = lf::pending(2);
    crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
    let before = bytes(&f);
    w.fakes.hc_install("hc");
    let got = pass(&w.ctx, &w.caller(), up_for(counted)).unwrap();
    assert!(matches!(got, Pass::Stopped), "stopped");
    assert_eq!(bytes(&f), before, "nothing settled");
    assert_eq!(steps(), 0, "no leader step");
    assert!(w.fakes.hc_calls().is_empty(), "no hc call");
}

/// Call 18: every step refusal other than the lock, a crash and P6 is reported and the pass goes on. Each case breaks
/// one step; the Lost child X is still decided afterwards.
#[test]
fn each_step_refusal_is_reported_and_the_next_step_runs() {
    for (case, key) in [
        ("adoption", "adoptReplayError"),
        ("cascade", "cascadeError"),
        ("stopped", "stoppedError"),
        ("leader", "leaderResume"),
    ] {
        let w = person();
        let scope = super::scope::state(&w.caller());
        let lead = if case == "leader" {
            team(&w);
            folder(&w, L)
        } else {
            leader(&w)
        };
        match case {
            "adoption" => {
                let d = super::adopt::dir_at(&w.ctx, &scope);
                std::fs::create_dir_all(&d).unwrap();
                std::fs::write(d.join("1.json"), "{torn").unwrap();
            }
            "cascade" => {
                let d = super::cascade::dir_at(&w.ctx, &scope);
                std::fs::create_dir_all(&d).unwrap();
                std::fs::write(d.join("aaaaaaaaaaaa.json"), "{torn").unwrap();
            }
            "stopped" => std::fs::create_dir_all(lead.join("desired.json")).unwrap(),
            _ => std::fs::create_dir_all(super::gens::path(&lead, 1, "outcome")).unwrap(),
        }
        if matches!(case, "cascade" | "stopped") {
            // the torn journal (or L's unreadable desired.json) is what X's reopen gate reads: X is decided
            // Unknown and nothing is released (5-9 F11)
            ok(&member(&w, X, json!({})), 1);
            found(&w, X, 1, &wire_of(&w, X, 1, lf::LOST_ARGVS[0]));
        } else {
            lost_x(&w);
        }
        let reply = pass_with(&w, counted);
        assert!(reply.get(key).is_some(), "{case}: {key} reported: {reply}");
        let ran = u32::from(matches!(case, "adoption" | "cascade"));
        assert_eq!(
            steps(),
            ran,
            "{case}: the step runs when its leader was observed"
        );
        let gate = matches!(case, "cascade" | "stopped");
        let got = ids(
            reply.as_object().unwrap(),
            if gate { "unknown" } else { "restored" },
        );
        assert!(
            got.contains(&X.to_string()),
            "{case}: the loop still ran: {reply}"
        );
        w.fakes.hc_done().unwrap();
    }
}

/// 5-9 F5-F7: a forced settle refusal is reported as `settleError`, the leader step runs and X is restored; a forced
/// member-listing refusal is reported as `membersError` and adoption still runs on its own boundary (a torn record
/// makes it refuse, so `adoptError` proves it ran).
#[test]
fn a_forced_settle_or_members_refusal_is_reported_and_the_pass_goes_on() {
    for (step, key) in [("settle", "settleError"), ("members", "membersError")] {
        let w = person();
        leader(&w);
        if step == "settle" {
            lost_x(&w);
        } else {
            let torn = folder(&w, "dddddddddddd");
            std::fs::create_dir_all(&torn).unwrap();
            std::fs::write(torn.join("record.json"), "{torn").unwrap();
        }
        faults::at(Some(step));
        let reply = pass_with(&w, counted);
        faults::at(None);
        assert_eq!(reply[key]["check"], "evidence", "{step}: {reply}");
        assert_eq!(steps(), 1, "{step}: the leader step ran");
        let restored = ids(reply.as_object().unwrap(), "restored");
        if step == "settle" {
            assert_eq!(
                restored,
                vec![X.to_string()],
                "the members step ran: {reply}"
            );
            w.fakes.hc_done().unwrap();
        } else {
            assert!(restored.is_empty(), "{reply}");
            assert!(
                reply.get("adoptError").is_some(),
                "adoption ran after the refused listing: {reply}"
            );
        }
    }
}

/// 5-9 F6: a real settle refusal (a pending generation whose claim is neither run nor abandoned) is reported as
/// `settleError` naming the member; the member stays Unknown.
#[test]
fn a_real_settle_refusal_is_reported_with_its_member() {
    let w = person();
    leader(&w);
    let f = member(&w, X, json!({}));
    ok(&f, 1);
    lf::seed_gen(&f, 2, Some("bogus"), None);
    let mut r = lf::record(&f);
    r["pending"] = lf::pending(2);
    crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
    let reply = pass_with(&w, counted);
    let e = &reply["settleError"];
    assert_eq!(e["check"], "internal", "{reply}");
    assert!(
        e["message"].as_str().unwrap().starts_with(X),
        "names X: {reply}"
    );
    assert!(
        ids(reply.as_object().unwrap(), "unknown").contains(&X.to_string()),
        "{reply}"
    );
    assert_eq!(steps(), 1);
}

/// 5-9 F9, F10: a Running leader is observed once per pass, whatever its step answers: the member loop decides it
/// from the carried observation (only one `find` is scripted; a second would be Unknown `hc`).
#[test]
fn the_leader_is_observed_once_per_pass() {
    for refused in [false, true] {
        let w = person();
        let lead = member(&w, L, json!({"depth": 0, "upTeam": "T", "name": "leader"}));
        let deadline =
            crate::util::utc_o(w.ctx.clock.now() + std::time::Duration::from_secs(86_400));
        let spec = json!({"deadline": deadline, "sessionId": lf::SID});
        crate::state::evidence::publish(&super::gens::path(&lead, 1, "spec"), &spec).unwrap();
        found(&w, L, 1, &wire_of(&w, L, 1, &lf::ARGV));
        let reply = pass_with(&w, if refused { refusing } else { counted });
        if refused {
            assert_eq!(reply["leaderResume"]["refused"], "client", "{reply}");
        } else {
            assert!(reply.get("leaderResume").is_none(), "Running: {reply}");
            assert_eq!(steps(), 1);
        }
        let unknown = ids(reply.as_object().unwrap(), "unknown");
        assert!(
            !unknown.contains(&L.to_string()),
            "refused {refused}: decided once: {reply}"
        );
        w.fakes.hc_done().unwrap();
    }
}

/// 5-9 F8, checklist 33: generation 1 ended before readiness with no outcome. The pass records `failed` durably
/// before the step, which halts; the outcome survives the halt.
fn halting_step(tx: &Tx<'_>, _: &Lazy<'_>, id: &str) -> Res<Option<Value>> {
    let o = super::gens::read_flushed(&tx.folder(id), 1, "outcome")?;
    assert_eq!(
        o.as_ref().and_then(|o| o.get("outcome")),
        Some(&json!("failed")),
        "adjudicated before the step"
    );
    crate::error::refuse(1, "halted", "test halt")
}

#[test]
fn the_leader_is_adjudicated_durably_before_its_step() {
    let w = person();
    let lead = member(&w, L, json!({"depth": 0, "upTeam": "T", "name": "leader"}));
    ended(&lead, 1);
    let got = pass(&w.ctx, &w.caller(), up_for(halting_step));
    assert!(
        matches!(&got, Err(e) if e.check == "halted"),
        "the halt passes through"
    );
    let o = super::gens::read_flushed(&lead, 1, "outcome")
        .unwrap()
        .expect("outcome survives the halt");
    assert_eq!(o["outcome"], "failed");
}

/// 5-9 F11: one member's refusal never skips the next. A (sorted before X) has unreadable marks and is Unknown; X is
/// still decided after it, for `up` and for `restore`. Occupancy fails closed on A's unreadable evidence, so X is
/// released and then held `occupied` (no relaunch is scripted).
#[test]
fn an_unreadable_member_never_skips_the_next_one() {
    const A: &str = "aaaaaaaaaaaa";
    for up in [true, false] {
        let w = person();
        leader(&w);
        let a = other(&w, A); // its own conversation
        std::fs::create_dir_all(super::gens::path(&a, 1, "ended")).unwrap();
        ok(&member(&w, X, json!({})), 1);
        found(&w, X, 1, &wire_of(&w, X, 1, lf::LOST_ARGVS[0]));
        released_by_hc(&w, X, lf::LOST_ARGVS[0]);
        let reply = if up {
            pass_with(&w, counted)
        } else {
            Value::Object(super::reconcile::run(&w.ctx, &w.caller()).unwrap())
        };
        let r = reply.as_object().unwrap();
        assert!(
            ids(r, "unknown").contains(&A.to_string()),
            "up {up}: {reply}"
        );
        assert_eq!(
            ids(r, "held"),
            vec![X.to_string()],
            "up {up}: X decided after A: {reply}"
        );
        w.fakes.hc_done().unwrap();
        steps();
    }
}
