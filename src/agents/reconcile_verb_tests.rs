//! restore-6b Task 6: the `restore` verb through the real dispatcher (L2, L6; ambiguity 9).
use super::core_fixture as cf;
use super::core_launch::forward_tests as ft;
use super::fixture::World;
use super::life_fixture as lf;
use super::reconcile_restore_tests::{L, released_by_hc, unmanaged_claude};
use super::reconcile_run_tests::ids;
use super::reconcile_tests::{X, found, member, ok, wire_of};
use super::team_state::{self, Scope};
use crate::hc::Lazy;
use serde_json::{Value, json};

/// `mux-agent restore` through the real dispatcher (`cli::run`), from a person in the team session.
pub(super) fn restore(w: &World) -> (u8, Value) {
    let args: Vec<String> = vec!["restore".into()];
    let (code, out) = crate::cli::run(&args, || Ok(&w.ctx));
    (code, serde_json::from_str(&out).unwrap())
}
/// The team leader `L`, generation 1 ended: the team exists; the leader asks hc nothing.
pub(super) fn team(w: &World) {
    let lead = member(w, L, json!({"depth": 0, "upTeam": "T", "name": "leader"}));
    let ended = super::gens::path(&lead, 1, "ended");
    crate::state::evidence::publish(&ended, &json!({"at": "t"})).unwrap();
}
/// The person world `restore` admits: the `up` tests base (tmux endpoint, pane) with this state root.
pub(super) fn person() -> World {
    let w = super::up::tests::base();
    w.state();
    w
}
/// `Snapshot::for_spawn` in the person world: the seeded leader `L` is listed among the gone terminals, in sorted
/// order (checklist 15).
pub(super) fn snapshot(w: &World, live: &[&str], gone: &[&str]) {
    let al = format!("a{L}");
    let mut gone: Vec<&str> = gone.to_vec();
    gone.push(&al);
    super::reconcile_await_tests::snapshot(w, live, &gone);
}
/// The shell R1.3 already relabelled: only `mux.kept` (this team) and `mux.keptName`.
pub(super) fn kept_wire(w: &World, id: &str) -> Value {
    json!({"address": format!("a{id}"), "name": lf::hc_name(1), "state": "live", "argv": lf::LOST_ARGVS[0],
        "labels": {"mux.kept": lf::team(w), "mux.keptName": "worker"}})
}
fn restored(reply: &Value) -> Vec<String> {
    ids(reply.as_object().unwrap(), "restored")
}

/// Spec row 0 (a): a crash after R1.4, before the launch: the next `restore` launches n+1 once.
#[test]
fn a_crash_after_released_then_restore_launches_once() {
    let w = person();
    team(&w);
    let f = member(&w, X, json!({}));
    ok(&f, 1);
    cf::online(&w);
    found(&w, X, 1, &wire_of(&w, X, 1, lf::LOST_ARGVS[0]));
    released_by_hc(&w, X, lf::LOST_ARGVS[0]);
    super::release::halt_after(Some("released"));
    let (code, first) = restore(&w);
    super::release::halt_after(None);
    assert_eq!((code, &first["check"]), (1, &json!("halted")), "{first}");
    assert!(
        super::gens::read(&f, 1, "released")
            .known()
            .unwrap()
            .is_some()
    );
    cf::online(&w); // discovery
    cf::online(&w); // the pass
    snapshot(&w, &[], &[&format!("a{X}")]);
    w.fakes.hc_answer(ft::address("t2"));
    let (code, second) = restore(&w);
    assert_eq!(
        (code, restored(&second)),
        (0, vec![X.to_string()]),
        "{second}"
    );
    assert_eq!(lf::count(&w, "run"), 1);
    lf::no_effect(&w);
    w.fakes.hc_done().unwrap();
}

/// Row 0 (c)-(f) and r6b call 15, one member released by the real R1 writer each (r6b call 10): stopped meanwhile
/// (row 1), closed by a stopped leader, occupancy held, released with `cause: recovery`, `release.json` unreadable,
/// no valid conversation id. None launches. Only `occupied` reaches the pass connection (r6b call 11).
#[test]
fn a_released_member_that_cannot_launch_launches_nothing() {
    for case in [
        "stopped", "closed", "occupied", "recovery", "torn", "session",
    ] {
        let w = person(); // its `cf::online` serves the real writer below
        team(&w);
        let f = member(&w, X, json!({"parentAgent": {"id": L}}));
        ok(&f, 1);
        released_by_hc(&w, X, lf::LOST_ARGVS[0]);
        let cause = if case == "recovery" {
            super::release::Cause::Recovery
        } else {
            super::release::Cause::Restore
        };
        team_state::open(&w.ctx, &Scope::of(&w.caller()), |tx| {
            super::release::release(tx, &Lazy::new(tx.ctx), X, 1, &format!("a{X}"), cause)
        })
        .unwrap();
        cf::online(&w); // the discovery of `restore`
        let stopped = json!({"desired": "stopped", "at": "t"});
        match case {
            "stopped" => crate::state::fs::write_json(&f.join("desired.json"), &stopped).unwrap(),
            "closed" => {
                crate::state::fs::write_json(&w.folder(L).join("desired.json"), &stopped).unwrap()
            }
            "occupied" => {
                unmanaged_claude(&w, 4321);
                cf::online(&w); // the pass
                snapshot(&w, &[], &[&format!("a{X}")]);
            }
            "torn" => std::fs::write(super::gens::path(&f, 1, "release"), b"{ torn").unwrap(),
            "session" => {
                let l = super::gens::path(&f, 1, "launch");
                std::fs::remove_file(&l).unwrap();
                let body =
                    json!({"hcName": lf::hc_name(1), "launcherArgv": lf::ARGV, "sessionId": null});
                crate::state::evidence::publish(&l, &body).unwrap();
            }
            _ => {}
        }
        let (code, reply) = restore(&w);
        assert_eq!(code, 0, "{case}: {reply}");
        let got = |k: &str| ids(reply.as_object().unwrap(), k);
        match case {
            "stopped" => assert!(
                got("held").is_empty() && got("unknown").is_empty(),
                "{case}"
            ),
            "closed" => assert_eq!(reply["held"][0]["reason"], "closed", "{case}: {reply}"),
            "occupied" => {
                assert_eq!(reply["held"][0]["reason"], "occupied", "{case}: {reply}");
                assert!(
                    reply["held"][0]["holder"]
                        .as_str()
                        .unwrap()
                        .contains("4321")
                );
            }
            "recovery" => assert_eq!(
                reply["recover"],
                json!([{"id": X, "name": "mbbb", "reason": "recovery"}])
            ),
            _ => assert_eq!(
                (&reply["unknown"][0]["id"], &reply["unknown"][0]["check"]),
                (&json!(X), &json!("evidence")),
                "{case}: {reply}"
            ),
        }
        assert_eq!(lf::count(&w, "run"), 0, "{case}");
        lf::no_effect(&w);
        w.fakes.hc_done().unwrap();
    }
}

/// Generation 2 of X pending (`kind: restore`, session `sid2`), generation 1 released; gen 2 launched, claimed `run`,
/// committed and ready, adjudicated `ok`.
fn committed_next(w: &World, sid2: &str) -> std::path::PathBuf {
    let f = member(
        w,
        X,
        json!({"pending": {"gen": 2, "kind": "restore", "hcName": lf::hc_name(2),
        "sessionId": sid2, "launcherArgv": lf::ARGV, "hcAddress": format!("a{X}")}}),
    );
    let publish = |file: &str, body: Value| {
        crate::state::evidence::publish(&super::gens::path(&f, 2, file), &body).unwrap();
    };
    crate::state::evidence::publish(&super::gens::path(&f, 1, "released"), &json!({"at": "t"}))
        .unwrap();
    publish(
        "launch",
        json!({"hcName": lf::hc_name(2), "launcherArgv": lf::ARGV, "sessionId": sid2}),
    );
    crate::state::evidence::publish(&super::gens::claim_path(&f, 2), &json!("run")).unwrap();
    publish("commit", json!({"committed": true}));
    ok(&f, 2);
    f
}
const SID2: &str = "2e22fc2f-2032-48ef-a920-4af2a8e5af52";

/// Spec: commit before promotion, then core restart. n+1 (gen 2) committed and ready, not promoted; hc shows it
/// Lost. `restore`: settle promotes gen 2 and releases it (R3, `cause: restore`); row 0 launches gen 3 once from the
/// gen 2 session; no task.
#[test]
fn commit_before_promotion_then_core_restart_resumes_the_promoted_session() {
    let w = person();
    team(&w);
    let f = committed_next(&w, SID2);
    let ax = format!("a{X}");
    cf::online(&w);
    found(&w, X, 2, &wire_of(&w, X, 2, lf::LOST_ARGVS[0])); // settle row 6: exactly one, Lost
    cf::answer(
        &w,
        &["get", &ax, "--json"],
        &wire_of(&w, X, 2, lf::LOST_ARGVS[0]).to_string(),
    );
    lf::relabel(&w, &ax);
    cf::online(&w); // the pass (settle or `release::complete` connected first, checklist 14)
    snapshot(&w, &[], &[&ax]);
    w.fakes.hc_answer(ft::address("t3"));
    let (code, reply) = restore(&w);
    assert_eq!(
        (code, restored(&reply)),
        (0, vec![X.to_string()]),
        "{reply}"
    );
    let r = lf::record(&f);
    assert_eq!(
        (&r["generation"], &r["sessionId"]),
        (&json!("3"), &json!(SID2))
    );
    let release = super::gens::read(&f, 2, "release")
        .known()
        .unwrap()
        .unwrap();
    assert_eq!(release["cause"], "restore");
    let spec = super::gens::read(&f, 3, "spec").known().unwrap().unwrap();
    assert_eq!(spec["args"], json!(["--resume", SID2]));
    assert!(spec.get("prompt").is_none());
    assert_eq!(lf::count(&w, "run"), 1);
    w.fakes.hc_done().unwrap();
}

/// Spec: interrupted release, real dispatcher. `restore` killed inside L3 before the relabel, then (separately)
/// after it, before `released.json`. The next `restore` finishes the release first (one relabel, one
/// `released.json`), then launches n+1 exactly once.
#[test]
fn an_interrupted_release_is_finished_then_relaunched_once() {
    for step in ["release", "relabel"] {
        let w = person();
        team(&w);
        let f = member(&w, X, json!({}));
        ok(&f, 1);
        let ax = format!("a{X}");
        cf::online(&w);
        found(&w, X, 1, &wire_of(&w, X, 1, lf::LOST_ARGVS[0]));
        if step == "relabel" {
            released_by_hc(&w, X, lf::LOST_ARGVS[0]);
        }
        super::release::halt_after(Some(step));
        assert_eq!(restore(&w).1["check"], "halted", "{step}");
        super::release::halt_after(None);
        assert!(
            super::gens::read(&f, 1, "released")
                .known()
                .unwrap()
                .is_none(),
            "{step}"
        );
        cf::online(&w); // discovery
        cf::online(&w); // the pass
        if step == "release" {
            released_by_hc(&w, X, lf::LOST_ARGVS[0]);
        } else {
            cf::answer(&w, &["get", &ax, "--json"], &kept_wire(&w, X).to_string());
        }
        snapshot(&w, &[], &[&ax]);
        w.fakes.hc_answer(ft::address("t2"));
        let (code, reply) = restore(&w);
        assert_eq!(
            (code, restored(&reply)),
            (0, vec![X.to_string()]),
            "{step}: {reply}"
        );
        assert_eq!(
            (lf::count(&w, "label"), lf::count(&w, "run")),
            (1, 1),
            "{step}"
        );
        lf::no_effect(&w);
        w.fakes.hc_done().unwrap();
    }
}

/// Spec (release-1d R4): `restore` killed after R3 promote patch, before `release.json`. The next `restore` (its
/// `Tx::open` hook `release::complete`) finishes the release once, then launches n+2 from the n+1 session.
#[test]
fn promoted_not_released_is_finished_once_then_relaunched() {
    let w = person();
    team(&w);
    let f = committed_next(&w, SID2);
    let ax = format!("a{X}");
    cf::online(&w);
    found(&w, X, 2, &wire_of(&w, X, 2, lf::LOST_ARGVS[0]));
    super::release::halt_after(Some("promoted"));
    assert_eq!(restore(&w).1["check"], "halted");
    super::release::halt_after(None);
    assert_eq!(lf::record(&f)["completing"]["gen"], 2);
    assert!(
        super::gens::read(&f, 2, "release")
            .known()
            .unwrap()
            .is_none()
    );
    cf::online(&w); // discovery
    cf::online(&w); // the pass
    cf::answer(
        &w,
        &["get", &ax, "--json"],
        &wire_of(&w, X, 2, lf::LOST_ARGVS[0]).to_string(),
    );
    lf::relabel(&w, &ax);
    cf::online(&w); // the pass (settle or `release::complete` connected first, checklist 14)
    snapshot(&w, &[], &[&ax]);
    w.fakes.hc_answer(ft::address("t3"));
    let (_, reply) = restore(&w);
    assert_eq!(restored(&reply), [X], "{reply}");
    let r = lf::record(&f);
    let got = (&r["generation"], &r["sessionId"], r.get("completing"));
    assert_eq!(got, (&json!("3"), &json!(SID2), None));
    let spec = super::gens::read(&f, 3, "spec").known().unwrap().unwrap();
    assert_eq!(spec["args"], json!(["--resume", SID2]));
    assert_eq!((lf::count(&w, "label"), lf::count(&w, "run")), (1, 1));
    w.fakes.hc_done().unwrap();
}

/// Spec: release then stop. A crash after R1.3, then a person stops the team, then `restore`: the cascade finishes
/// the release (`released.json`), completes, and nothing launches.
#[test]
fn release_then_stop_finishes_the_release_and_launches_nothing() {
    let w = person();
    team(&w);
    let f = member(&w, X, json!({"parentAgent": {"id": L}}));
    ok(&f, 1);
    let ax = format!("a{X}");
    cf::online(&w);
    found(&w, X, 1, &wire_of(&w, X, 1, lf::LOST_ARGVS[0]));
    released_by_hc(&w, X, lf::LOST_ARGVS[0]);
    super::release::halt_after(Some("relabel"));
    assert_eq!(restore(&w).1["check"], "halted");
    super::release::halt_after(None);
    cf::online(&w);
    cf::answer(&w, &["get", &ax, "--json"], &kept_wire(&w, X).to_string()); // terminate: rows 0r, kept: skip
    let stop: Vec<String> = vec!["stop".into(), "leader".into()];
    let (code, out) = crate::cli::run(&stop, || Ok(&w.ctx));
    assert_eq!(code, 0, "{out}");
    cf::online(&w); // discovery of the last restore (nothing else asks hc: X is stopped)
    let (code, reply) = restore(&w);
    assert_eq!(code, 0, "{reply}");
    assert!(
        super::gens::read(&f, 1, "released")
            .known()
            .unwrap()
            .is_some()
    );
    assert_eq!(
        super::cascade::desired_of(&f).unwrap(),
        super::cascade::Desired::Stopped
    );
    assert_eq!((lf::count(&w, "label"), lf::count(&w, "run")), (1, 0));
    lf::no_effect(&w);
    w.fakes.hc_done().unwrap();
}

/// L2 row 2: a missing terminal leaves X Unknown, reserved; nothing released or launched.
#[test]
fn a_missing_terminal_leaves_x_unknown_and_reserved() {
    let w = person();
    team(&w);
    let f = member(&w, X, json!({}));
    ok(&f, 1);
    cf::online(&w);
    let (by_id, by_gen) = (format!("mux.id={X}"), "mux.gen=1".to_string());
    let ls = [
        "ls", "--all", "--label", &by_id, "--label", &by_gen, "--json",
    ];
    cf::answer(&w, &ls, r#"{"terminals": []}"#);
    let (_, reply) = restore(&w);
    assert_eq!(
        reply["unknown"],
        json!([{"id": X, "name": "mbbb", "check": "hc"}]),
        "{reply}"
    );
    assert!(
        super::gens::read(&f, 1, "release")
            .known()
            .unwrap()
            .is_none()
    );
    assert_eq!(lf::count(&w, "label") + lf::count(&w, "run"), 0);
    assert!(!super::gens::done(&f, 1).unwrap(), "still reserved");
    w.fakes.hc_done().unwrap();
}

/// state-0 S9: `up|T` before the team lock, each taken once (the fake lock panics on a nested take).
#[test]
fn restore_takes_up_then_team() {
    let w = person();
    team(&w);
    w.fakes.clear_log();
    assert_eq!(restore(&w).0, 0);
    let locks = w.fakes.locks();
    let up = locks
        .iter()
        .position(|k| k.starts_with("+up|T"))
        .expect("up|T taken");
    let spawn = locks
        .iter()
        .position(|k| k.starts_with("+spawn|"))
        .expect("team lock taken");
    assert!(up < spawn, "{locks:?}");
}

/// r6b call 6: a lifecycle leader that never ran is no team; the one rule is `up::discover::existing`.
#[test]
fn restore_where_the_leader_never_ran_is_no_team() {
    let w = person();
    let lead = w.folder(L);
    std::fs::create_dir_all(&lead).unwrap();
    let r = json!({"lifecycle": 1, "id": L, "name": "leader", "upTeam": "T", "depth": 0,
        "session": w.caller().session});
    crate::state::fs::write_json(&lead.join("record.json"), &r).unwrap();
    let (code, reply) = restore(&w);
    assert_eq!((code, &reply["check"]), (1, &json!("team")), "{reply}");
}

/// Ambiguity 9: an agent is refused 2 `tagged`; a person outside any team 1 `team`; nothing is read past it.
#[test]
fn restore_is_a_person_verb_in_the_team_session() {
    let w = person();
    let (code, reply) = restore(&w);
    assert_eq!((code, &reply["check"]), (1, &json!("team")), "{reply}");
    let a = World::agent("");
    a.state();
    let (code, reply) = restore(&a);
    assert_eq!((code, &reply["check"]), (2, &json!("tagged")), "{reply}");
}
