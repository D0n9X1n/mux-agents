//! restore-6c Tasks 5-6 (#103): `stop X -Generation` ends a member's open generations and nothing else; `drop X`.
use super::core_fixture as cf;
use super::core_launch::forward_tests as ft;
use super::fixture::World;
use super::reconcile_restore_tests::{L, other, released_by_hc};
use super::reconcile_tests::{X, found, member, member_at, wire_of};
use super::reconcile_verb_tests::{person, team};
use super::recover_crash_tests::recorded_wire;
use super::recover_tests::{cli, failed, failed_ended};
use super::{gens, life_fixture as lf, prelife_fixture::bytes};
use serde_json::{Value, json};
use std::path::PathBuf;

const Y: &str = "dddddddddddd";

/// `gens::terminate` of a Running generation n: its own connection (`first`: `person()`'s queued `--version`),
/// `find` (label-filtered `ls`, then `get`), the reread right before the kill, then `kill`.
fn terminated(w: &World, n: u64, wire: &Value, first: bool) {
    if !first {
        cf::online(w);
    }
    let a = wire["address"].as_str().unwrap();
    let by_gen = format!("mux.gen={n}");
    cf::answer(
        w,
        &[
            "ls",
            "--all",
            "--label",
            &format!("mux.id={X}"),
            "--label",
            &by_gen,
            "--json",
        ],
        &json!({"terminals": [{"address": a}]}).to_string(),
    );
    cf::answer(w, &["get", a, "--json"], &wire.to_string());
    cf::answer(w, &["get", a, "--json"], &wire.to_string());
    cf::answer(w, &["kill", a, "--json"], r#"{"ok":true}"#);
}
/// X under L: generation 1 failed and Running at `a<X>`; generation 2 a committed Running pending at `t2`.
fn two_open(w: &World) -> PathBuf {
    let f = member(w, X, json!({"parentAgent": {"id": L}}));
    failed(&f, 1);
    lf::seed_gen(&f, 2, Some("run"), lf::committed());
    let mut r = lf::record(&f);
    r["pending"] = json!({"gen": 2, "kind": "retry", "hcName": lf::hc_name(2), "sessionId": lf::SID,
        "launcherArgv": lf::ARGV, "hcAddress": "t2"});
    crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
    f
}
fn kills(w: &World) -> Vec<String> {
    w.fakes
        .hc_calls()
        .into_iter()
        .filter(|c| c.1[0] == "kill")
        .map(|c| c.1[1].clone())
        .collect()
}

/// Spec "generation stop, real dispatcher": X's open generations end newest first (pending 2, then active 1);
/// n+1 is never promoted; child Y, X's desired state and the cascade journals are untouched. All done: `state`.
/// A retry is then eligible.
#[test]
fn stop_generation_ends_open_generations_newest_first() {
    let w = person();
    team(&w);
    let f = two_open(&w);
    let y = other(&w, Y);
    let mut ry = lf::record(&y);
    (ry["parentAgent"], ry["depth"], ry["name"]) = (json!({"id": X}), json!(2), json!("mddd"));
    crate::state::fs::write_json(&y.join("record.json"), &ry).unwrap();
    let (desired, y_before) = (std::fs::read(f.join("desired.json")).ok(), bytes(&y));
    terminated(&w, 2, &recorded_wire(&w, &f, 2, None), true);
    terminated(&w, 1, &wire_of(&w, X, 1, &lf::ARGV), false);
    let (code, reply) = cli(&w, &["stop", "mbbb", "-Generation"]);
    assert_eq!(code, 0, "{reply}");
    assert_eq!(
        (&reply["name"], &reply["ended"], &reply["unresolved"]),
        (&json!("mbbb"), &json!([2, 1]), &json!([])),
        "newest first: {reply}"
    );
    assert_eq!(
        kills(&w),
        vec!["t2".to_string(), format!("a{X}")],
        "kill order"
    );
    let r = lf::record(&f);
    assert_eq!(
        (&r["generation"], r.get("pending")),
        (&json!("1"), None),
        "n+1 never promoted"
    );
    assert_eq!(
        std::fs::read(f.join("desired.json")).ok(),
        desired,
        "desired untouched"
    );
    assert_eq!(bytes(&y), y_before, "Y untouched");
    let journals = super::cascade::dir_at(&w.ctx, &super::scope::state(&w.caller()));
    assert!(
        std::fs::read_dir(&journals).map_or(true, |mut d| d.next().is_none()),
        "no cascade journal"
    );
    w.fakes.hc_done().unwrap();
    let (code, again) = cli(&w, &["stop", "mbbb", "-Generation"]);
    assert_eq!(
        (code, &again["check"]),
        (2, &json!("state")),
        "every generation done: {again}"
    );
    cf::online(&w); // the retry's Lazy
    super::reconcile_await_tests::snapshot(
        &w,
        &[],
        &[&format!("a{L}"), &format!("a{X}"), &format!("a{Y}")],
    );
    w.fakes.hc_answer(ft::address("t3"));
    let (code, retried) = cli(&w, &["resume", "mbbb"]);
    assert_eq!(
        (code, &retried["agent"]["generation"]),
        (0, &json!(3)),
        "retry eligible: {retried}"
    );
    w.fakes.hc_done().unwrap();
}

/// Ambiguity 6: newest first, pending 2's terminal is unknown to hc (unresolved, no kill), and the stop goes on to
/// active 1, which is Lost: released `cause: lost`, never killed (release-1d R2). The reply names both.
#[test]
fn stop_generation_releases_a_lost_one_and_reports_an_unresolved_one() {
    let w = person();
    team(&w);
    let f = two_open(&w);
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
    cf::online(&w); // terminate(1): find Lost, then R1 (reread, one relabel)
    found(&w, X, 1, &wire_of(&w, X, 1, lf::LOST_ARGVS[0]));
    released_by_hc(&w, X, lf::LOST_ARGVS[0]);
    let (code, reply) = cli(&w, &["stop", "mbbb", "-Generation"]);
    assert_eq!(code, 0, "{reply}");
    assert_eq!(reply["ended"], json!([1]), "{reply}");
    assert_eq!(reply["unresolved"][0]["generation"], 2, "{reply}");
    assert_eq!(
        gens::read_flushed(&f, 1, "release").unwrap().unwrap()["cause"],
        "lost"
    );
    assert!(kills(&w).is_empty(), "no kill");
    w.fakes.hc_done().unwrap();
}

/// Checklist 28: unreadable evidence of one generation (an orphan's or the pending one's `ended.json` is a folder)
/// is reported unresolved, and the stop still ends the independent active generation.
#[test]
fn stop_generation_reports_unreadable_generations_and_goes_on() {
    for case in ["orphan", "pending"] {
        let w = person();
        team(&w);
        let f = member(&w, X, json!({"parentAgent": {"id": L}}));
        failed(&f, 1);
        let bad = if case == "orphan" { 0 } else { 2 };
        lf::seed_gen(&f, bad, Some("run"), lf::committed());
        if case == "pending" {
            let mut r = lf::record(&f);
            r["pending"] = lf::pending(2);
            crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
        }
        std::fs::create_dir_all(gens::path(&f, bad, "ended")).unwrap();
        terminated(&w, 1, &wire_of(&w, X, 1, &lf::ARGV), true);
        let (code, reply) = cli(&w, &["stop", X, "-Generation"]);
        assert_eq!(code, 0, "{case}: {reply}");
        assert_eq!(reply["ended"], json!([1]), "{case}: {reply}");
        assert_eq!(
            (
                &reply["unresolved"][0]["generation"],
                &reply["unresolved"][0]["check"]
            ),
            (&json!(bad), &json!("evidence")),
            "{case}: {reply}"
        );
        w.fakes.hc_done().unwrap();
    }
}

/// Calls 15, 25 (checklist 13, 29): `stop -Generation`, `resume -Fresh`, `resume` and `drop` from team `$3`
/// never reach a member of `$99` under the same key: by id, by name, or through a record copied into another
/// folder (its `id` does not name its folder). Each answers 2 `target`; the folders are unchanged. With a local
/// member of the same name, the local one is the subject and the foreign one is untouched.
#[test]
fn stop_and_recovery_never_reach_another_sessions_member() {
    for case in ["id", "name", "copied", "twin"] {
        for verb in [
            &["stop", "-Generation"][..],
            &["resume", "-Fresh"],
            &["resume"],
            &["drop"],
        ] {
            if case == "twin" && !matches!(verb[0], "stop" | "drop") {
                continue;
            }
            let w = person();
            team(&w);
            let key = w.caller().server_key;
            let foreign = member_at(&w, &key, X, json!({"session": "$99"}));
            failed(&foreign, 1);
            let copy = w
                .ctx
                .state_root
                .join(&key)
                .join("agents")
                .join("cccccccccccd");
            if case == "copied" {
                std::fs::create_dir_all(&copy).unwrap();
                let mut r = lf::record(&foreign);
                r["session"] = json!(w.caller().session);
                crate::state::fs::write_json(&copy.join("record.json"), &r).unwrap();
            }
            let local = (case == "twin").then(|| {
                let l = member(&w, Y, json!({"parentAgent": {"id": L}, "name": "mbbb"}));
                failed_ended(&l, 1);
                l
            });
            let before = bytes(&foreign);
            let copied = copy.exists().then(|| bytes(&copy));
            w.fakes.hc_install("hc");
            let target = match case {
                "name" | "twin" => "mbbb",
                "copied" => "cccccccccccd",
                _ => X,
            };
            let args: Vec<&str> = [&[verb[0], target][..], &verb[1..]].concat();
            let (code, reply) = cli(&w, &args);
            let at = format!("{case} {verb:?}");
            if let Some(l) = &local
                && verb[0] == "drop"
            {
                assert_eq!(code, 0, "{at}: the local twin is dropped: {reply}");
                assert_eq!(
                    super::cascade::desired_of(l).unwrap(),
                    super::cascade::Desired::Stopped,
                    "{at}"
                );
            } else {
                let want = if local.is_some() { "state" } else { "target" };
                assert_eq!((code, &reply["check"]), (2, &json!(want)), "{at}: {reply}");
            }
            assert_eq!(bytes(&foreign), before, "{at}: untouched");
            assert_eq!(
                copy.exists().then(|| bytes(&copy)),
                copied,
                "{at}: the copy untouched"
            );
            assert!(
                kills(&w).is_empty() && lf::count(&w, "run") == 0,
                "{at}: no effect"
            );
        }
    }
}

/// 6c-R3: a member is refused 2 `tagged`, before anything ends or hc is asked.
#[test]
fn stop_generation_is_a_person_action() {
    let w = World::agent("");
    w.state();
    team(&w);
    failed(&member(&w, X, json!({"parentAgent": {"id": L}})), 1);
    w.fakes.hc_install("hc");
    let (code, reply) = cli(&w, &["stop", X, "-Generation"]);
    assert_eq!((code, &reply["check"]), (2, &json!("tagged")), "{reply}");
    assert!(w.fakes.hc_calls().is_empty(), "nothing asked");
}

/// Spec P2 drop, checklist 25-27: `drop X` is the cascade stop (cascade-3d) for a person, on X of the anchor session
/// by exact identity. A Lost generation is released `cause: lost`, relabelled kept (never `kill`), and views-6f's
/// `kept` verb still lists the shell afterwards (V5). Agents: `tagged`.
#[test]
fn drop_is_a_person_stop_and_its_shell_stays_kept() {
    for argv in lf::LOST_ARGVS {
        let w = person();
        team(&w);
        let f = member(&w, X, json!({"parentAgent": {"id": L}}));
        failed(&f, 1);
        found(&w, X, 1, &wire_of(&w, X, 1, argv));
        released_by_hc(&w, X, argv);
        let (code, reply) = cli(&w, &["drop", "mbbb"]);
        assert_eq!(
            (code, &reply["stopped"]),
            (0, &json!(true)),
            "{argv:?}: the cascade stop: {reply}"
        );
        assert_eq!(
            gens::read_flushed(&f, 1, "release").unwrap().unwrap()["cause"],
            "lost",
            "{argv:?}"
        );
        assert!(
            gens::read_flushed(&f, 1, "released").unwrap().is_some(),
            "{argv:?}: done"
        );
        assert_eq!(
            super::cascade::desired_of(&f).unwrap(),
            super::cascade::Desired::Stopped,
            "{argv:?}"
        );
        assert!(kills(&w).is_empty(), "{argv:?}: no kill");
        w.fakes.hc_done().unwrap();
        let a = format!("a{X}");
        cf::online(&w);
        cf::kept_listing(&w, &lf::team(&w), &[&a]);
        let kept = json!({"address": a, "name": lf::hc_name(1), "state": "live", "reason": null, "exit_code": null,
            "argv": argv, "labels": {"mux.kept": lf::team(&w), "mux.keptName": "mbbb"}});
        cf::answer(&w, &["get", &a, "--json"], &kept.to_string());
        let (code, listed) = cli(&w, &["kept"]);
        assert_eq!(
            (code, &listed["kept"]),
            (
                0,
                &json!([{"address": a, "name": "mbbb", "team": lf::team(&w)}])
            ),
            "{argv:?}: {listed}"
        );
        assert!(kills(&w).is_empty(), "{argv:?}: nothing closed");
        w.fakes.hc_done().unwrap();
    }
    let w = World::agent("");
    w.state();
    team(&w);
    member(&w, X, json!({"parentAgent": {"id": L}}));
    w.fakes.hc_install("hc");
    let (code, reply) = cli(&w, &["drop", X]);
    assert_eq!(
        (code, &reply["check"]),
        (2, &json!("tagged")),
        "agent: {reply}"
    );
    assert!(w.fakes.hc_calls().is_empty(), "agent: nothing asked");
}

/// Call 25 (5-9 F1), checklist 27: `drop` names its subject by exact id. A member whose name is another member's id
/// is not the one stopped; and an identity that changed between selection and the locked stop is refused (`target`)
/// by `stop_life`'s revalidation, with nothing written.
#[test]
fn drop_stops_by_exact_identity_and_revalidates_under_the_lock() {
    let w = person();
    team(&w);
    let x = member(&w, X, json!({"parentAgent": {"id": L}}));
    failed_ended(&x, 1);
    let y = member(
        &w,
        Y,
        json!({"parentAgent": {"id": L}, "name": "bbbbbbbbbbbb"}),
    );
    failed_ended(&y, 1);
    let y_before = bytes(&y);
    let (code, reply) = cli(&w, &["drop", X]);
    assert_eq!(
        (code, &reply["name"]),
        (0, &json!("mbbb")),
        "the id names X: {reply}"
    );
    assert_eq!(
        super::cascade::desired_of(&x).unwrap(),
        super::cascade::Desired::Stopped,
        "X stopped"
    );
    assert_eq!(bytes(&y), y_before, "Y, named like X's id, untouched");
    let w = person();
    team(&w);
    let x = member(&w, X, json!({"parentAgent": {"id": L}}));
    failed_ended(&x, 1);
    let before = bytes(&x);
    let caller = crate::caller::resolve_tree(&w.ctx).unwrap();
    let mut stale = super::member_target::by_id(&w.ctx, &caller, X).unwrap();
    stale.generation = "9".into(); // the member changed after it was selected
    let e = super::message::stop_with(&w.ctx, |_, _| {
        Ok(super::member_target::StopTarget::Member(stale.clone()))
    })
    .unwrap_err();
    assert_eq!((e.code, e.check), (2, "target"), "{}", e.message);
    assert_eq!(bytes(&x), before, "nothing written");
}
