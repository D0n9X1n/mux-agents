//! restore-6b Task 6 (continued): spec cases through the real spawn, stop and dispatcher; a stop racing a restore.
use super::core_fixture as cf;
use super::core_launch::forward_tests as ft;
use super::life_fixture as lf;
use super::reconcile_restore_tests::{L, released_by_hc};
use super::reconcile_run_tests::ids;
use super::reconcile_tests::{X, found, member, ok, wire_of};
use super::reconcile_verb_tests::{person, restore, team};
use serde_json::{Value, json};

/// Spec abandonment gap through the real dispatcher (r6b call 10): `abandoned` published, `pending` not yet
/// dropped. Two `restore`s: dropped once, nothing runs, X never Unknown.
#[test]
fn the_abandonment_gap_through_restore_clears_pending_once() {
    let w = person();
    team(&w);
    let f = member(
        &w,
        X,
        json!({"pending": {"gen": 2, "kind": "restore", "hcName": lf::hc_name(2),
        "sessionId": lf::SID, "launcherArgv": lf::ARGV}}),
    );
    crate::state::evidence::publish(&super::gens::path(&f, 1, "ended"), &json!({"at": "t"}))
        .unwrap();
    crate::state::evidence::publish(&super::gens::claim_path(&f, 2), &json!("abandoned")).unwrap();
    for pass in 0..2 {
        if pass == 1 {
            cf::online(&w); // discovery (the first restore used the base answer); X asks hc nothing
        }
        let (code, reply) = restore(&w);
        assert_eq!(code, 0, "{reply}");
        let quiet = |k: &str| ids(reply.as_object().unwrap(), k).is_empty();
        assert!(
            quiet("restored") && quiet("unknown"),
            "pass {pass}: {reply}"
        );
    }
    assert!(lf::record(&f).get("pending").is_none());
    assert_eq!(
        super::gens::claim(&f, 2).known().unwrap(),
        Some(json!("abandoned"))
    );
    assert_eq!(lf::count(&w, "run"), 0);
    w.fakes.hc_done().unwrap();
}

/// r6b call 10: a person stop of the leader halted after its journal, before any desired write, leaves an
/// unfinished cascade; only the next `restore` (`cascade::finish`) stops X. X, Lost ok, is released `lost` by the
/// replay and never relaunched.
#[test]
fn an_interrupted_cascade_is_finished_by_restore_and_launches_nothing() {
    let w = person();
    team(&w);
    let f = member(&w, X, json!({"parentAgent": {"id": L}}));
    ok(&f, 1);
    let ax = format!("a{X}");
    super::release::halt_after(Some("cascade-journal"));
    let stop: Vec<String> = ["stop", "leader"].map(String::from).to_vec();
    let (code, out) = crate::cli::run(&stop, || Ok(&w.ctx)); // the base `cf::online`, if the stop connects
    super::release::halt_after(None);
    let out: Value = serde_json::from_str(&out).unwrap();
    assert_eq!((code, &out["check"]), (1, &json!("halted")), "{out}");
    let running = super::cascade::Desired::Running;
    assert_eq!(
        super::cascade::desired_of(&f).unwrap(),
        running,
        "the cascade is unfinished"
    );
    // checklist 17: the halted stop never connected, so the base answer serves discovery
    cf::online(&w); // the pass: cascade::finish terminates X (Lost: find, reread, release `cause: lost`)
    let lost = wire_of(&w, X, 1, lf::LOST_ARGVS[0]);
    found(&w, X, 1, &lost);
    cf::answer(&w, &["get", &ax, "--json"], &lost.to_string());
    lf::relabel(&w, &ax);
    let (code, reply) = restore(&w);
    assert_eq!(code, 0, "{reply}");
    let stopped = super::cascade::Desired::Stopped;
    assert_eq!(
        super::cascade::desired_of(&f).unwrap(),
        stopped,
        "X desired stopped by the replay"
    );
    let release = super::gens::read(&f, 1, "release")
        .known()
        .unwrap()
        .unwrap();
    assert_eq!(release["cause"], "lost");
    assert_eq!(lf::count(&w, "run"), 0);
    lf::no_effect(&w);
    w.fakes.hc_done().unwrap();
}

/// Spins until `ready`, bounded (as `registry_hc_tests` does).
fn until(ready: impl Fn() -> bool) {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !ready() {
        assert!(std::time::Instant::now() < end, "contention timeout");
        std::thread::yield_now();
    }
}

/// Spec L2 (r6b call 10): a stop racing a restore never relaunches a stopped member. Two real operations on one
/// state root through a shared `Gate`, each with its own fakes. `stop_first`: the stop holds the team lock until
/// the restore waits for it; the restore then reads X desired `stopped` (row 1) and launches nothing.
/// Otherwise the restore holds the team lock past its decision until the stop waits; it launches generation 2
/// once, the stop then ends it, and a third `restore` launches nothing.
#[test]
fn a_stop_racing_a_restore_never_relaunches_the_stopped_member() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for stop_first in [true, false] {
        let w = person();
        team(&w);
        let f = member(&w, X, json!({"parentAgent": {"id": L}}));
        ok(&f, 1);
        let gate = std::sync::Arc::new(crate::testing::Gate::default());
        w.fakes.share_locks(gate.clone());
        let (root, ax) = (w.ctx.state_root.clone(), format!("a{X}"));
        let stop_holds = std::sync::Arc::new(AtomicBool::new(false));
        if !stop_first {
            cf::online(&w); // the pass (the base answer serves discovery)
            found(&w, X, 1, &wire_of(&w, X, 1, lf::LOST_ARGVS[0]));
            released_by_hc(&w, X, lf::LOST_ARGVS[0]);
            w.fakes.hc_answer(ft::address("t2"));
            let g = gate.clone();
            w.fakes.lock_probe(move |e| {
                if let Some(k) = e.strip_prefix("-").filter(|k| k.starts_with("spawn|")) {
                    until(|| g.waiting(k) > 0); // the decision is made; hold until the stop waits
                }
                String::new()
            });
        }
        std::thread::scope(|scope| {
            let stopper = scope.spawn(|| {
                let mut s = super::up::tests::base(); // its `cf::online` serves the stop
                s.ctx.state_root = root.clone();
                s.fakes.share_locks(gate.clone());
                if stop_first {
                    let wire = wire_of(&s, X, 1, lf::LOST_ARGVS[0]);
                    found(&s, X, 1, &wire); // checklist 16: find, then release's reread
                    cf::answer(&s, &["get", &ax, "--json"], &wire.to_string());
                    lf::relabel(&s, &ax);
                    let (g, held) = (gate.clone(), stop_holds.clone());
                    s.fakes.lock_probe(move |e| {
                        if let Some(k) = e.strip_prefix("+").filter(|k| k.starts_with("spawn|")) {
                            held.store(true, Ordering::SeqCst);
                            until(|| g.waiting(k) > 0);
                        }
                        String::new()
                    });
                } else {
                    let folder = root.join(&s.caller().server_key).join("agents").join(X);
                    let launch = super::gens::path(&folder, 2, "launch");
                    until(|| launch.is_file());
                    let l = super::gens::read(&folder, 2, "launch")
                        .known()
                        .unwrap()
                        .unwrap();
                    let mut t = wire_of(&s, X, 2, &[]);
                    (t["name"], t["argv"], t["address"]) =
                        (l["hcName"].clone(), l["launcherArgv"].clone(), json!("t2"));
                }
                let stop: Vec<String> = ["stop", "mbbb"].map(String::from).to_vec();
                let (code, out) = crate::cli::run(&stop, || Ok(&s.ctx));
                assert_eq!(code, 0, "stop: {out}");
                if stop_first {
                    s.fakes.hc_done().unwrap();
                } else {
                    // generation 2 has no commit here (no launcher runs in a unit test): the merged stop answers
                    // life `unknown`, `stopped: true`, and connects to nothing.
                    let out: Value = serde_json::from_str(&out).unwrap();
                    assert_eq!(
                        (&out["life"], &out["stopped"]),
                        (&json!("unknown"), &json!(true))
                    );
                    assert!(s.fakes.hc_calls().is_empty(), "{:?}", s.fakes.hc_calls());
                }
            });
            if stop_first {
                until(|| stop_holds.load(Ordering::SeqCst));
            }
            let (code, reply) = restore(&w);
            assert_eq!(code, 0, "{reply}");
            stopper.join().unwrap();
        });
        let team_key = w
            .fakes
            .locks()
            .iter()
            .find_map(|e| {
                e.strip_prefix("+")
                    .filter(|k| k.starts_with("spawn|"))
                    .map(str::to_owned)
            })
            .expect("team lock");
        assert_eq!(
            gate.waited(&team_key),
            1,
            "stop_first {stop_first}: the two contended"
        );
        w.fakes.lock_probe(|_| String::new());
        cf::online(&w); // discovery of the third restore; X is desired stopped and asks hc nothing
        let (_, third) = restore(&w);
        assert!(
            ids(third.as_object().unwrap(), "restored").is_empty(),
            "{third}"
        );
        assert_eq!(
            super::cascade::desired_of(&f).unwrap(),
            super::cascade::Desired::Stopped
        );
        let runs = if stop_first { 0 } else { 1 };
        assert_eq!(lf::count(&w, "run"), runs, "stop_first {stop_first}");
        w.fakes.hc_done().unwrap();
    }
}
