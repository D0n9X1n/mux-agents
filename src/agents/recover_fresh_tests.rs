//! restore-6c Task 3: `resume X -Fresh` starts a new conversation on the same member.
use super::core_launch::forward_tests as ft;
use super::reconcile_restore_tests::{L, released_by_hc, unmanaged_claude};
use super::reconcile_tests::{X, found, member, member_at, wire_of};
use super::reconcile_verb_tests::{person, team};
use super::recover_gate_tests::gate_case;
use super::recover_tests::{both_gone, child, cli, failed, failed_ended, spec, stopped};
use super::resume::{Action, recover};
use super::{gens, life_fixture as lf, prelife_fixture::bytes};
use crate::state::evidence::publish;
use serde_json::{Value, json};

/// Spec P2 fresh: n+1, kind `fresh`, a new uuid in `pending`, `launch.json` and the spec, no `--resume`, no task;
/// the predecessor's stored options launched (checklist 17); the same id, name, parent, depth and options;
/// `record.sessionId` changes only at promotion. Occupancy is not asked (a live unmanaged Claude does not hold it).
#[test]
fn a_fresh_start_takes_a_new_conversation_on_the_same_id() {
    let w = person();
    team(&w);
    let f = child(&w);
    unmanaged_claude(&w, 4321);
    let before = lf::record(&f);
    both_gone(&w);
    w.fakes.hc_answer(ft::address("t2"));
    let (code, reply) = cli(&w, &["resume", "mbbb", "-Fresh"]);
    assert_eq!(code, 0, "{reply}");
    let new = reply["agent"]["sessionId"].as_str().unwrap().to_string();
    assert!(
        crate::checks::session_id(&new) && new != lf::SID,
        "a new conversation: {new}"
    );
    let kind = (&reply["agent"]["kind"], &reply["agent"]["generation"]);
    assert_eq!(kind, (&json!("fresh"), &json!(2)), "{reply}");
    let launch = gens::read_flushed(&f, 2, "launch").unwrap().unwrap();
    assert_eq!(launch["sessionId"], new, "launch.json names it");
    let s = spec(&f, 2).expect("spec.json");
    assert_eq!(
        (&s["kind"], &s["args"]),
        (&json!("fresh"), &json!(["--session-id", new])),
        "no --resume, no task"
    );
    assert_eq!(s["flags"], before["options"], "the stored options launched");
    assert!(s.get("prompt").is_none(), "6c-R4");
    let r = lf::record(&f);
    assert_eq!(
        (&r["sessionId"], &r["generation"]),
        (&json!(new), &json!("2")),
        "promoted"
    );
    for k in ["id", "name", "parentAgent", "depth", "client", "options"] {
        assert_eq!(r.get(k), before.get(k), "{k} kept");
    }
    w.fakes.hc_done().unwrap();
}

/// P2: fresh meets every shared gate (live client, older open generation, the cap); occupancy alone is retry's.
/// A Lost failed generation, stopped or not, is released `cause: recovery` and launched fresh. A member: `tagged`.
#[test]
fn fresh_meets_every_shared_gate() {
    for (case, check) in [
        ("running", "running"),
        ("running-stopped", "running"),
        ("older", "running"),
        ("capped", "limit"),
    ] {
        let (w, f, got) = gate_case(case, Action::Fresh);
        assert_eq!(
            got.map_err(|e| (e.code, e.check)).err(),
            Some((2, check)),
            "{case}"
        );
        assert!(spec(&f, 2).is_none(), "{case}: nothing launched");
        w.fakes.hc_done().unwrap();
    }
    let (w, _, got) = gate_case("occupied", Action::Fresh);
    assert_eq!(
        got.unwrap().expect("occupied")["agent"]["kind"],
        "fresh",
        "occupied: not asked"
    );
    w.fakes.hc_done().unwrap();
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
            let (code, reply) = cli(&w, &["resume", X, "-Fresh"]);
            let at = format!("{argv:?} stopped {stop}");
            assert_eq!(
                (code, &reply["agent"]["kind"], &reply["released"]),
                (0, &json!("fresh"), &json!(1)),
                "{at}: {reply}"
            );
            assert_eq!(
                gens::read_flushed(&f, 1, "release").unwrap().unwrap()["cause"],
                "recovery",
                "{at}"
            );
            lf::no_effect(&w);
            w.fakes.hc_done().unwrap();
        }
    }
    let mut w = person();
    team(&w);
    child(&w);
    let caller = w.caller();
    w.ctx.env = crate::ctx::Env::from_pairs([("HC_TERMINAL_ID", "t1")]);
    let e = recover(&w.ctx, &caller, Action::Fresh, X).unwrap_err();
    assert_eq!((e.code, e.check), (2, "tagged"), "member");
}

/// Ambiguity 2, call 15, checklist 1, 5, 12: `-Fresh` is only for a member of this session that needs recovery.
/// Ended `ok`/`void` (a valid Void body), and a stopped Ended `ok` child: 2 `state`. No team: 1 `team`. Another
/// session's member by id or name: 2 `target`, untouched. A pre-lifecycle member: 2 `state`. An unresolved pending
/// generation: 2 `running`; a torn one 1 `evidence`.
#[test]
fn fresh_is_only_for_a_member_of_this_session_that_needs_recovery() {
    let void = gens::Verdict::Void("never ran".into());
    for (case, v) in [
        ("ok", gens::Verdict::Ok),
        ("void", void),
        ("stopped ok", gens::Verdict::Ok),
    ] {
        let w = person();
        team(&w);
        let f = member(&w, X, json!({"parentAgent": {"id": L}}));
        publish(&gens::path(&f, 1, "outcome"), &v.body("t")).unwrap();
        publish(&gens::path(&f, 1, "ended"), &json!({"at": "t"})).unwrap();
        if case == "stopped ok" {
            stopped(&f);
        }
        w.fakes.hc_install("hc");
        let (code, reply) = cli(&w, &["resume", X, "-Fresh"]);
        assert_eq!(
            (code, &reply["check"]),
            (2, &json!("state")),
            "{case}: {reply}"
        );
    }
    let w = person(); // no team here
    w.fakes.hc_install("hc");
    let (code, reply) = cli(&w, &["resume", X, "-Fresh"]);
    assert_eq!((code, &reply["check"]), (1, &json!("team")), "{reply}");
    for target in [X, "mbbb"] {
        let w = person();
        team(&w);
        let foreign = member_at(&w, &w.caller().server_key, X, json!({"session": "$99"}));
        failed_ended(&foreign, 1);
        let before = bytes(&foreign);
        w.fakes.hc_install("hc");
        let (code, reply) = cli(&w, &["resume", target, "-Fresh"]);
        assert_eq!(
            (code, &reply["check"]),
            (2, &json!("target")),
            "foreign {target}: {reply}"
        );
        assert_eq!(bytes(&foreign), before, "foreign {target} untouched");
        assert_eq!(lf::count(&w, "run"), 0);
    }
    let w = person();
    team(&w);
    let pre = w.folder("eeeeeeeeeeee");
    std::fs::create_dir_all(&pre).unwrap();
    let r = json!({"id": "eeeeeeeeeeee", "name": "old", "session": w.caller().session, "client": "claude"});
    crate::state::fs::write_json(&pre.join("record.json"), &r).unwrap();
    w.fakes.hc_install("hc");
    let (code, reply) = cli(&w, &["resume", "old", "-Fresh"]);
    assert_eq!(
        (code, &reply["check"]),
        (2, &json!("state")),
        "pre-lifecycle: {reply}"
    );
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
            let gen2 = [
                "ls",
                "--all",
                "--label",
                &format!("mux.id={X}"),
                "--label",
                "mux.gen=2",
                "--json",
            ];
            super::core_fixture::answer(&w, &gen2, r#"{"terminals":[]}"#);
        }
        let (code, reply) = cli(&w, &["resume", X, "-Fresh"]);
        let want: (u8, Value) = if torn {
            (1, json!("evidence"))
        } else {
            (2, json!("running"))
        };
        assert_eq!(
            (code, reply["check"].clone()),
            want,
            "pending torn {torn}: {reply}"
        );
        assert_eq!(
            lf::count(&w, "run"),
            0,
            "pending torn {torn}: nothing launched"
        );
    }
}

/// Checklist 16: a fresh start killed before its commit (claim absent: settle row 3 voids it) and then refused by a
/// gate (the cap) leaves the record naming the predecessor's conversation: the abort never copies the pending one.
#[test]
fn a_voided_fresh_start_keeps_the_predecessors_conversation() {
    let w = person();
    team(&w);
    let f = child(&w);
    lf::seed_gen(&f, 2, None, None);
    let mut r = lf::record(&f);
    r["pending"] = json!({"gen": 2, "kind": "fresh", "hcName": lf::hc_name(2),
        "sessionId": "0f0f0f0f-2032-48ef-a920-0f0f0f0f0f0f", "launcherArgv": lf::ARGV, "hcAddress": "t2"});
    crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
    let live: Vec<String> = super::reconcile_await_tests::six_running(&w)
        .iter()
        .map(|id| format!("a{id}"))
        .collect();
    let live: Vec<&str> = live.iter().map(String::as_str).collect();
    super::reconcile_await_tests::snapshot(&w, &live, &[&format!("a{L}"), &format!("a{X}")]);
    let (code, reply) = cli(&w, &["resume", X, "-Fresh"]);
    assert_eq!((code, &reply["check"]), (2, &json!("limit")), "{reply}");
    let r = lf::record(&f);
    assert!(
        r.get("pending").is_none(),
        "the claim-less generation was voided: {r}"
    );
    assert_eq!(
        (&r["sessionId"], &r["generation"]),
        (&json!(lf::SID), &json!("1")),
        "the predecessor's conversation"
    );
    w.fakes.hc_done().unwrap();
}
