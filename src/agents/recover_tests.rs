//! restore-6c Task 1: a person's retry of a member that needs recovery, its subject and its evidence.
use super::core_launch::forward_tests as ft;
use super::fixture::World;
use super::reconcile_restore_tests::L;
use super::reconcile_tests::{X, found, member, member_at, team_record_at, wire_of};
use super::reconcile_verb_tests::{person, team};
use super::resume::{Action, recover};
use super::{gens, life_fixture as lf, prelife_fixture::bytes};
use crate::state::evidence::publish;
use serde_json::{Value, json};
use std::path::Path;

/// Generation n failed (outcome-2's record); nothing has ended it yet.
pub(crate) fn failed(folder: &Path, n: u64) {
    let body = json!({"outcome": "failed", "reason": "client exited before readiness", "at": "t"});
    publish(&gens::path(folder, n, "outcome"), &body).unwrap();
}
/// Generation n failed and its end is recorded: done; `observe` reports the recorded `failed` with no hc call.
pub(crate) fn failed_ended(folder: &Path, n: u64) {
    failed(folder, n);
    let end = json!({"address": "a", "hcName": lf::hc_name(n), "labels": {}, "state": "finished",
        "reason": "exited", "exitCode": 1, "at": "t"});
    publish(&gens::path(folder, n, "ended"), &end).unwrap();
}
/// A verb through the real dispatcher.
pub(crate) fn cli(w: &World, args: &[&str]) -> (u8, Value) {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let (code, out) = crate::cli::run(&args, || Ok(&w.ctx));
    (code, serde_json::from_str(&out).unwrap())
}
pub(crate) fn spec(f: &Path, n: u64) -> Option<Value> {
    gens::read_flushed(f, n, "spec").unwrap()
}
pub(crate) fn stopped(f: &Path) {
    publish(&f.join("desired.json"), &json!({"desired": "stopped"})).unwrap();
}
/// The cap's `Snapshot::for_spawn` (Task 2) when X is the only member besides L: the team listing (nothing live),
/// then one `get` per recorded address, both gone.
pub(crate) fn both_gone(w: &World) {
    super::reconcile_await_tests::snapshot(w, &[], &[&format!("a{L}"), &format!("a{X}")]);
}
/// X under L (an open parent), failed and ended.
pub(crate) fn child(w: &World) -> std::path::PathBuf {
    let f = member(w, X, json!({"parentAgent": {"id": L}}));
    failed_ended(&f, 1);
    f
}

/// Spec "same-id resume" (retry leg), through the dispatcher: n+1 of the same id, kind `retry`, `--resume <the
/// failed generation's conversation>`, no task. The one hc connection is `person()`'s queued `--version`.
#[test]
fn a_retry_resumes_the_same_id_as_the_next_generation() {
    let w = person();
    team(&w);
    let f = child(&w);
    both_gone(&w);
    w.fakes.hc_answer(ft::address("t2"));
    let (code, reply) = cli(&w, &["resume", "mbbb"]);
    assert_eq!(code, 0, "{reply}");
    assert_eq!(
        reply["agent"],
        json!({"id": X, "name": "mbbb", "generation": 2, "kind": "retry",
        "sessionId": lf::SID}),
        "reply"
    );
    let r = lf::record(&f);
    assert_eq!(
        (&r["generation"], &r["sessionId"]),
        (&json!("2"), &json!(lf::SID)),
        "same id"
    );
    let s = spec(&f, 2).expect("spec.json");
    assert_eq!(
        (&s["kind"], &s["args"]),
        (&json!("retry"), &json!(["--resume", lf::SID])),
        "resume recipe"
    );
    assert!(s.get("prompt").is_none(), "no task resent (6c-R4)");
    w.fakes.hc_done().unwrap();
}

/// A member spawned with -Role stores the role's bound tokens in its options and its body in role.md. After the role
/// file is changed or deleted, retry and fresh relaunch with exactly the stored options: the file is never read again.
#[test]
fn a_roles_member_recovers_with_its_stored_options_whatever_became_of_the_role_file() {
    let options = json!([
        "--dangerously-skip-permissions",
        "--effort",
        "high",
        "--model=haiku",
        "--tools=Read,Grep"
    ]);
    for action in ["retry", "fresh"] {
        for changed in [true, false] {
            let w = person();
            team(&w);
            let file = w.ctx.cwd.join(".claude").join("agents").join("x.md");
            let f = member(
                &w,
                X,
                json!({"parentAgent": {"id": L}, "options": options, "role": "x", "roleFile": file,
                    "roleClient": "claude", "model": "haiku"}),
            );
            failed_ended(&f, 1);
            std::fs::write(f.join("role.md"), "You review code.\n").unwrap();
            if changed {
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(
                    &file,
                    "---\nname: x\nmodel: opus\ntools: Bash\n---\nother\n",
                )
                .unwrap();
            }
            both_gone(&w);
            w.fakes.hc_answer(ft::address("t2"));
            let mut args = vec!["resume", "mbbb"];
            if action == "fresh" {
                args.push("-Fresh");
            }
            let (code, reply) = cli(&w, &args);
            assert_eq!(code, 0, "{action} {changed}: {reply}");
            let s = spec(&f, 2).expect("spec.json");
            assert_eq!(
                s["flags"], options,
                "{action} {changed}: the stored options"
            );
            assert_eq!(
                std::fs::read_to_string(f.join("role.md")).unwrap(),
                "You review code.\n",
                "{action} {changed}: role.md kept"
            );
            let r = lf::record(&f);
            for k in ["options", "role", "roleFile", "roleClient"] {
                assert_ne!(r[k], Value::Null, "{action} {changed}: {k} kept");
            }
            assert_eq!(r["options"], options);
            w.fakes.hc_done().unwrap();
        }
    }
}

/// The new windows a test world opened, each as its argv joined.
fn new_windows(w: &World) -> Vec<String> {
    (w.fakes.mux_calls().into_iter())
        .filter(|c| c.iter().any(|a| a == "new-window"))
        .map(|c| c.join(" "))
        .collect()
}

/// `-Tab` on a member that needs recovery (the CLI's `resume <name> -Tab`, and the side bar's `resume_by_id` with
/// `tab`): its next generation opens its view in a new window, as an ordinary resume's `-Tab` does, and the reply says
/// so.
#[test]
fn a_recovery_with_tab_opens_the_next_generation_as_a_tab() {
    for how in ["cli", "by id"] {
        let w = person();
        team(&w);
        // As spawn records it: the view's tags read `context` and `reason`.
        let f = member(
            &w,
            X,
            json!({"parentAgent": {"id": L}, "context": "brief", "reason": "human"}),
        );
        failed_ended(&f, 1);
        both_gone(&w);
        w.fakes.hc_answer(ft::address("t2"));
        let reply = match how {
            "cli" => {
                let (code, reply) = cli(&w, &["resume", "mbbb", "-Tab"]);
                assert_eq!(code, 0, "{how}: {reply}");
                reply
            }
            _ => Value::Object(
                super::resume::resume_by_id(&w.ctx, &w.caller(), X, true, false).unwrap(),
            ),
        };
        assert_eq!(reply["agent"]["id"], json!(X), "{how}: {reply}");
        let opened = new_windows(&w);
        assert_eq!(opened.len(), 1, "{how}: {opened:?}");
        assert!(opened[0].ends_with("attach t2"), "{how}: {opened:?}");
        assert_eq!(reply["view"], json!("attached"), "{how}: {reply}");
        w.fakes.hc_done().unwrap();
    }
}

/// 6c-R3: a member caller (hc environment) is refused 2 `tagged` with nothing written; an agent pane through the
/// dispatcher likewise, with no hc call.
#[test]
fn only_a_person_recovers() {
    let mut w = person();
    team(&w);
    let f = child(&w);
    let caller = w.caller();
    w.ctx.env = crate::ctx::Env::from_pairs([("HC_TERMINAL_ID", "t1")]);
    let before = bytes(&f);
    let e = recover(&w.ctx, &caller, Action::Retry, X).unwrap_err();
    assert_eq!((e.code, e.check), (2, "tagged"), "member: {}", e.message);
    assert_eq!(bytes(&f), before, "member: nothing written");
    let a = World::agent("");
    a.state();
    team(&a);
    failed_ended(&member(&a, X, json!({"parentAgent": {"id": L}})), 1);
    a.fakes.hc_install("hc");
    let (code, reply) = cli(&a, &["resume", X]);
    assert_eq!(
        (code, &reply["check"]),
        (2, &json!("tagged")),
        "agent pane: {reply}"
    );
    assert!(a.fakes.hc_calls().is_empty(), "agent pane: no hc call");
}

/// Ambiguities 1-2, call 15: only a `failed` verdict is a recovery. Ended `ok`/`void`: `None` (today's resume).
/// Running and undecided: 2 `state`. A member of another session under the same key, by id or name: 2 `target`,
/// untouched, nothing asked.
#[test]
fn recover_decides_from_failed_alone_and_only_in_the_anchor_session() {
    for v in [
        super::gens::Verdict::Ok,
        super::gens::Verdict::Void("never ran".into()),
    ] {
        let w = person();
        team(&w);
        let f = member(&w, X, json!({}));
        publish(&gens::path(&f, 1, "outcome"), &v.body("t")).unwrap();
        publish(&gens::path(&f, 1, "ended"), &json!({"at": "t"})).unwrap();
        assert!(
            recover(&w.ctx, &w.caller(), Action::Retry, X)
                .unwrap()
                .is_none(),
            "{v:?}"
        );
    }
    let w = person();
    team(&w);
    let f = member(&w, X, json!({}));
    let deadline = crate::util::utc_o(w.ctx.clock.now() + std::time::Duration::from_secs(86_400));
    publish(
        &gens::path(&f, 1, "spec"),
        &json!({"deadline": deadline, "sessionId": lf::SID}),
    )
    .unwrap();
    found(&w, X, 1, &wire_of(&w, X, 1, &lf::ARGV)); // Running, no verdict yet
    let e = recover(&w.ctx, &w.caller(), Action::Retry, X).unwrap_err();
    assert_eq!((e.code, e.check), (2, "state"), "undecided: {}", e.message);
    w.fakes.hc_done().unwrap();
    for target in [X, "mbbb"] {
        let w = person();
        team(&w);
        let foreign = member_at(&w, &w.caller().server_key, X, json!({"session": "$99"}));
        failed_ended(&foreign, 1);
        let before = bytes(&foreign);
        w.fakes.hc_install("hc");
        let (code, reply) = cli(&w, &["resume", target]);
        assert_eq!(
            (code, &reply["check"]),
            (2, &json!("target")),
            "foreign {target}: {reply}"
        );
        assert_eq!(bytes(&foreign), before, "foreign {target}: untouched");
        assert_eq!(
            lf::count(&w, "run"),
            0,
            "foreign {target}: nothing launched"
        );
    }
}

/// Call 25 (1-4 F2), checklist 1, 10: a stopped member's verdict is its recorded outcome. A stopped failed child of
/// an open parent reopens and retries; under a stopped leader it refuses `closed`; a stopped failed root reopens
/// itself (c3d call 35). A stopped, Ended `ok` child is not a recovery (`None`: today's resume).
#[test]
fn a_stopped_failed_member_recovers_from_its_recorded_outcome() {
    for case in ["child", "closed", "root"] {
        let w = person();
        team(&w);
        let f = if case == "root" {
            let f = member(&w, X, json!({}));
            failed_ended(&f, 1);
            f
        } else {
            child(&w)
        };
        stopped(&f);
        both_gone(&w);
        if case == "closed" {
            stopped(&w.folder(L));
            let (code, reply) = cli(&w, &["resume", X]);
            assert_eq!(
                (code, &reply["check"]),
                (2, &json!("closed")),
                "{case}: {reply}"
            );
            assert!(spec(&f, 2).is_none(), "{case}: nothing launched");
            w.fakes.hc_done().unwrap();
            continue;
        }
        w.fakes.hc_answer(ft::address("t2"));
        let (code, reply) = cli(&w, &["resume", X]);
        assert_eq!(
            (code, &reply["agent"]["generation"]),
            (0, &json!(2)),
            "{case}: {reply}"
        );
        assert_eq!(
            super::cascade::desired_of(&f).unwrap(),
            super::cascade::Desired::Running,
            "{case}"
        );
        w.fakes.hc_done().unwrap();
    }
    let w = person();
    team(&w);
    let f = member(&w, X, json!({"parentAgent": {"id": L}}));
    super::reconcile_tests::ok(&f, 1);
    publish(&gens::path(&f, 1, "ended"), &json!({"at": "t"})).unwrap();
    stopped(&f);
    assert!(
        recover(&w.ctx, &w.caller(), Action::Retry, X)
            .unwrap()
            .is_none(),
        "stopped ended ok"
    );
}

/// Call 25, checklist 4, 6, 7: unreadable or malformed evidence is never absent. Each case refuses 1 `evidence`
/// and launches nothing.
#[test]
fn recovery_refuses_unreadable_evidence() {
    let mut cases = vec![
        "sibling",
        "leader",
        "outcome-dir",
        "outcome-{}",
        "outcome-null",
        "outcome-word",
        "outcome-no-reason",
        "team-absent",
        "team-torn",
        "team-dir",
        "team-foreign",
    ];
    if cfg!(unix) {
        cases.extend(["team-link", "team-fifo"]);
    }
    for case in cases {
        let w = person();
        team(&w);
        let f = child(&w);
        let c = w.caller();
        let tf = super::team::path(&w.ctx.state_root.join(&c.server_key), &c.session);
        let outcome = gens::path(&f, 1, "outcome");
        let body = |b: Value| {
            std::fs::remove_file(&outcome).unwrap();
            publish(&outcome, &b).unwrap();
        };
        match case {
            "sibling" => {
                let s = w.folder("eeeeeeeeeeee");
                std::fs::create_dir_all(&s).unwrap();
                std::fs::write(s.join("record.json"), "{torn").unwrap();
            }
            "leader" => std::fs::write(w.folder(L).join("record.json"), "{torn").unwrap(),
            "outcome-dir" => {
                stopped(&f);
                std::fs::remove_file(&outcome).unwrap();
                std::fs::create_dir_all(&outcome).unwrap();
            }
            "outcome-{}" => body(json!({})),
            "outcome-null" => body(Value::Null),
            "outcome-word" => body(json!({"outcome": "maybe", "at": "t"})),
            "outcome-no-reason" => body(json!({"outcome": "failed", "at": "t"})),
            "team-absent" => std::fs::remove_file(&tf).unwrap(),
            "team-torn" => std::fs::write(&tf, "{torn").unwrap(),
            "team-dir" => {
                std::fs::remove_file(&tf).unwrap();
                std::fs::create_dir_all(&tf).unwrap();
            }
            "team-foreign" => {
                let mut t: Value = serde_json::from_slice(&std::fs::read(&tf).unwrap()).unwrap();
                t["session"] = json!("$99");
                std::fs::write(&tf, t.to_string()).unwrap();
            }
            #[cfg(unix)]
            "team-link" => {
                let real = tf.with_extension("real");
                std::fs::rename(&tf, &real).unwrap();
                std::os::unix::fs::symlink(&real, &tf).unwrap();
            }
            #[cfg(unix)]
            "team-fifo" => {
                std::fs::remove_file(&tf).unwrap();
                let p = std::ffi::CString::new(tf.to_str().unwrap()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(p.as_ptr(), 0o600) }, 0);
            }
            other => panic!("{other}"),
        }
        w.fakes.hc_install("hc");
        if case.starts_with("team-") {
            both_gone(&w); // the cap runs before the launch reads the team record
        }
        let (code, reply) = cli(&w, &["resume", "mbbb"]);
        assert_eq!(
            (code, &reply["check"]),
            (1, &json!("evidence")),
            "{case}: {reply}"
        );
        assert!(
            spec(&f, 2).is_none() && lf::count(&w, "run") == 0,
            "{case}: nothing launched"
        );
    }
}

/// Call 25, checklist 6, 23 (rows 1.14, 1.15, 1.18): each unreadable-evidence guard decides alone, where no later
/// reader would refuse first. A torn member named by its folder id; a torn leader beside a team record; a stopped,
/// failed, not-ended generation (hc shows it Running) whose outcome is a folder.
#[test]
fn each_unreadable_guard_refuses_on_its_own() {
    for case in ["torn-target", "torn-leader", "stopped-open-outcome"] {
        let w = person();
        team(&w);
        let f = member(&w, X, json!({"parentAgent": {"id": L}}));
        let target = match case {
            "torn-target" => {
                failed_ended(&f, 1);
                let s = w.folder("eeeeeeeeeeee");
                std::fs::create_dir_all(&s).unwrap();
                std::fs::write(s.join("record.json"), "{torn").unwrap();
                "eeeeeeeeeeee"
            }
            "torn-leader" => {
                failed_ended(&f, 1);
                std::fs::write(w.folder(L).join("record.json"), "{torn").unwrap();
                X
            }
            _ => {
                stopped(&f);
                let o = gens::path(&f, 1, "outcome");
                std::fs::create_dir_all(&o).unwrap();
                found(&w, X, 1, &wire_of(&w, X, 1, &lf::ARGV));
                X
            }
        };
        let e = recover(&w.ctx, &w.caller(), Action::Retry, target).unwrap_err();
        assert_eq!((e.code, e.check), (1, "evidence"), "{case}: {}", e.message);
        assert!(spec(&f, 2).is_none(), "{case}: nothing launched");
    }
}

/// Checklist 8: the conversation a retry resumes is the failed generation's valid `launch.json` `sessionId`, which
/// the record must name too. Missing, null, wrong-type, invalid or mismatched: 1 `evidence`, for the leader as well.
#[test]
fn a_retry_resumes_only_a_valid_agreed_conversation() {
    let other = super::reconcile_restore_tests::sid_of("999999999999");
    for (who, field) in [
        ("leader", "absent"),
        ("leader", "null"),
        ("member", "number"),
        ("member", "invalid"),
        ("member", "record-other"),
        ("leader", "launch-other"),
    ] {
        let w = person();
        let (id, f) = if who == "leader" {
            let f = member(&w, L, json!({"depth": 0, "upTeam": "T", "name": "leader"}));
            failed_ended(&f, 1);
            (L, f)
        } else {
            team(&w);
            (X, child(&w))
        };
        let mut r = lf::record(&f);
        match field {
            "absent" => drop(r.as_object_mut().unwrap().remove("sessionId")),
            "null" => r["sessionId"] = Value::Null,
            "number" => r["sessionId"] = json!(7),
            "invalid" => r["sessionId"] = json!("not-a-uuid"),
            "record-other" => r["sessionId"] = json!(other),
            _ => {
                let l = gens::path(&f, 1, "launch");
                let mut launch: Value =
                    serde_json::from_slice(&std::fs::read(&l).unwrap()).unwrap();
                launch["sessionId"] = json!(other);
                std::fs::remove_file(&l).unwrap();
                publish(&l, &launch).unwrap();
            }
        }
        crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
        w.fakes.hc_install("hc");
        let e = recover(&w.ctx, &w.caller(), Action::Retry, id).unwrap_err();
        assert_eq!(
            (e.code, e.check),
            (1, "evidence"),
            "{who} {field}: {}",
            e.message
        );
        assert!(
            spec(&f, 2).is_none() && lf::count(&w, "run") == 0,
            "{who} {field}: nothing launched"
        );
    }
}

/// Checklist 19: recovery takes its subject from the caller's anchor (`scope::state`), not the session it is shown
/// in. Both pairs hold a failed leader L (a leader takes no cap slot); only the anchor's advances.
#[test]
fn recovery_takes_its_subject_from_the_anchor_not_the_shown_session() {
    let w = person();
    let shown = w.caller();
    let here = member(&w, L, json!({"depth": 0, "upTeam": "T", "name": "leader"}));
    failed_ended(&here, 1);
    let key = "00000000000000a2";
    let there = member_at(
        &w,
        key,
        L,
        json!({"depth": 0, "upTeam": "T", "name": "leader", "session": "$5"}),
    );
    team_record_at(&w, key, "$5");
    failed_ended(&there, 1);
    let before = bytes(&here);
    let scope = super::team_state::Scope {
        key: key.into(),
        session: "$5".into(),
    };
    let anchored = super::reconcile::anchor_caller(&shown, &scope);
    w.fakes.hc_answer(ft::address("t2"));
    let reply = recover(&w.ctx, &anchored, Action::Retry, L)
        .unwrap()
        .expect("a recovery");
    assert_eq!(reply["agent"]["generation"], 2, "{reply:?}");
    assert_eq!(
        lf::record(&there)["generation"],
        json!("2"),
        "the anchor's L advanced"
    );
    assert_eq!(bytes(&here), before, "the shown session's L untouched");
    w.fakes.hc_done().unwrap();
}
