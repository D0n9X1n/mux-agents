//! restore-6b Task 4: row 4, a Lost member whose run succeeded comes back as n+1 of its conversation (L3-L5).
use super::core_fixture as cf;
use super::core_launch::forward_tests as ft;
use super::fixture::World;
use super::life_fixture as lf;
use super::reconcile_run_tests::{ids, pass};
use super::reconcile_tests::{X, found, member, ok, wire_of};
use serde_json::{Value, json};
use std::path::PathBuf;

pub(super) const L: &str = "cccccccccccc";
const Y: &str = "dddddddddddd";

/// R1 of release-1d as hc sees it for member `id` gen 1: the reread of the shell, then the one relabel.
pub(super) fn released_by_hc(w: &World, id: &str, argv: &[&str]) {
    let a = format!("a{id}");
    cf::answer(
        w,
        &["get", &a, "--json"],
        &wire_of(w, id, 1, argv).to_string(),
    );
    lf::relabel(w, &a);
}
/// An unmanaged client of the kind (occupy-3 H4.4), on another conversation.
pub(super) fn unmanaged_claude(w: &World, pid: u32) {
    let (path, image) = if cfg!(windows) {
        ("C:/bin/claude.exe", "claude.exe")
    } else {
        ("/usr/bin/claude", "claude")
    };
    let other = "0e11fc2f-2032-48ef-a920-4af2a8e5af59";
    w.fakes.process(crate::proc::Row {
        path: Some(path.into()),
        argv: Some(vec![path.into(), "--session-id".into(), other.into()]),
        image: Some(image.into()),
        ..crate::testing::row(pid, 1, 99)
    });
    w.fakes.client(pid, crate::parse::Client::Claude);
}
/// A conversation id of member `id` alone (r6b call 11: unrelated members never share one).
pub(super) fn sid_of(id: &str) -> String {
    format!("{}-2032-48ef-a920-{id}", &id[..8])
}
/// A member unrelated to X: its record and its generation 1 `launch.json` name `sid_of(id)`.
pub(super) fn other(w: &World, id: &str) -> PathBuf {
    let f = member(w, id, json!({"sessionId": sid_of(id)}));
    let l = super::gens::path(&f, 1, "launch");
    std::fs::remove_file(&l).unwrap();
    let launch =
        json!({"hcName": lf::hc_name(1), "launcherArgv": lf::ARGV, "sessionId": sid_of(id)});
    crate::state::evidence::publish(&l, &launch).unwrap();
    f
}
fn gen_file(f: &std::path::Path, n: u64, file: &str) -> Option<Value> {
    super::gens::read(f, n, file).known().unwrap()
}

#[test]
fn a_lost_ok_member_comes_back_as_the_next_generation_of_its_conversation() {
    for argv in lf::LOST_ARGVS {
        let w = World::human("");
        w.state();
        let f = member(&w, X, json!({}));
        ok(&f, 1);
        cf::online(&w);
        found(&w, X, 1, &wire_of(&w, X, 1, argv));
        released_by_hc(&w, X, argv);
        w.fakes.hc_answer(ft::address("t2"));
        let reply = pass(&w);
        let want = json!([{"id": X, "name": "mbbb", "generation": 2, "sessionId": lf::SID}]);
        assert_eq!(reply["restored"], want, "{reply:?}");
        let r = lf::record(&f);
        let got = (&r["generation"], &r["sessionId"], &r["hcAddress"]);
        assert_eq!(got, (&json!("2"), &json!(lf::SID), &json!("t2")));
        assert_ne!(
            r["hcName"],
            json!(lf::hc_name(1)),
            "a new hc name (launch-1 G2)"
        );
        let spec = gen_file(&f, 2, "spec").unwrap();
        assert_eq!(spec["args"], json!(["--resume", lf::SID]));
        assert_eq!(spec["flags"], json!(["--effort", "high"]));
        assert_eq!(spec["kind"], "restore");
        assert!(spec.get("prompt").is_none(), "no task resent");
        assert_eq!(gen_file(&f, 1, "release").unwrap()["cause"], "restore");
        assert!(gen_file(&f, 1, "released").is_some());
        assert_eq!(lf::count(&w, "run"), 1);
        lf::no_effect(&w);
        w.fakes.hc_done().unwrap();
    }
}

#[test]
fn a_closed_member_is_held_and_nothing_is_released() {
    let w = World::human("");
    w.state();
    let lead = member(&w, L, json!({"depth": 0, "upTeam": "T"}));
    let stopped = json!({"desired": "stopped", "at": "t"});
    crate::state::fs::write_json(&lead.join("desired.json"), &stopped).unwrap();
    let f = member(&w, X, json!({"parentAgent": {"id": L}}));
    ok(&f, 1);
    cf::online(&w);
    found(&w, X, 1, &wire_of(&w, X, 1, lf::LOST_ARGVS[0]));
    let reply = pass(&w);
    assert_eq!(
        reply["held"],
        json!([{"id": X, "name": "mbbb", "reason": "closed"}]),
        "{reply:?}"
    );
    assert!(gen_file(&f, 1, "release").is_none());
    assert_eq!(lf::count(&w, "label") + lf::count(&w, "run"), 0);
    w.fakes.hc_done().unwrap();
}

/// L4: every older generation done. An orphan generation 0 that the fence cannot settle holds X.
#[test]
fn an_older_generation_not_done_holds_and_nothing_is_released() {
    let w = World::human("");
    w.state();
    let f = member(&w, X, json!({}));
    lf::seed_gen(&f, 0, Some("run"), lf::committed());
    ok(&f, 1);
    cf::online(&w);
    let (by_id, by_gen) = (format!("mux.id={X}"), "mux.gen=0".to_string());
    let ls = [
        "ls", "--all", "--label", &by_id, "--label", &by_gen, "--json",
    ];
    cf::answer(&w, &ls, r#"{"terminals": []}"#);
    cf::online(&w); // `run`'s own `Lazy` connects again (the fence's connection was its own)
    found(&w, X, 1, &wire_of(&w, X, 1, lf::LOST_ARGVS[0]));
    let reply = pass(&w);
    assert_eq!(
        reply["held"],
        json!([{"id": X, "name": "mbbb", "reason": "older"}]),
        "{reply:?}"
    );
    assert!(gen_file(&f, 1, "release").is_none());
    assert_eq!(lf::count(&w, "label") + lf::count(&w, "run"), 0);
    w.fakes.hc_done().unwrap();
}

/// Ambiguity 5, r6b call 18: occupancy is asked after the release; every occupy-3 hold rule holds X, naming the
/// holder and rule; X stays released (row 0) and is retried next pass. Y is desired `stopped` (row 1: no hc).
#[test]
fn every_occupancy_hold_holds_the_restore_after_the_release() {
    use crate::parse::Client;
    let cases = [
        ("unmanaged", "it is a client this team did not start"),
        ("argv", "its command line names this conversation"),
        ("managed", "last reported this conversation"),
        (
            "reserved",
            "a launch of it that has not ended reserves this conversation",
        ),
        ("table", "the process table cannot be read"),
        ("unbound", "it cannot be read and may be a client"),
        ("evidence", "its evidence cannot be read"),
    ];
    for (case, phrase) in cases {
        let w = World::human("");
        w.state();
        crate::testing::private_tree(&w.ctx.state_root, &w.ctx.state_root);
        let f = member(&w, X, json!({}));
        ok(&f, 1);
        let stopped_y = || {
            let y = member(&w, Y, json!({})); // gen 1 not done, `sessionId` lf::SID: reserves X's conversation
            let stopped = json!({"desired": "stopped", "at": "t"});
            crate::state::fs::write_json(&y.join("desired.json"), &stopped).unwrap();
            y
        };
        match case {
            "unmanaged" => unmanaged_claude(&w, 4321),
            "argv" => w.fakes.process(crate::proc::Row {
                argv: Some(vec![
                    "/usr/bin/node".into(),
                    "--resume".into(),
                    lf::SID.into(),
                ]),
                ..crate::testing::row(4321, 1, 99)
            }),
            "managed" => {
                // checklist 5: Y's proven client (launcher 31337, its child 4321) reports X's conversation.
                let y = stopped_y();
                let publish = |file: &str, body: Value| {
                    crate::state::evidence::publish(&super::gens::path(&y, 1, file), &body)
                        .unwrap();
                };
                // occupy-3 reads the effective session from spec.json and the launch time from `startedAt`.
                publish("spec", json!({"sessionId": lf::SID}));
                let at = crate::util::utc_o(w.ctx.clock.now());
                publish(
                    "launcher",
                    json!({"pid": 31337, "start": 1000, "startedAt": at}),
                );
                publish("client", json!({"pid": 4321, "start": 1500}));
                w.fakes.process(crate::testing::row(31337, 1, 1000));
                w.fakes.process(crate::testing::row(4321, 31337, 1500));
                w.fakes.client(4321, Client::Claude);
                crate::state::live::report(&w.ctx, Client::Claude, 4321, 1500, lf::SID, "startup")
                    .unwrap();
            }
            "reserved" => {
                stopped_y();
            }
            "table" => w.fakes.table_fails(),
            "unbound" => w.fakes.process(crate::proc::Row {
                path: None,
                argv: None,
                image: None,
                ..crate::testing::row(4321, 1, 99)
            }),
            _ => {
                // checklist 6: an input occupancy reads (Y's spec.json) is unreadable: a folder in its place.
                let y = stopped_y();
                std::fs::create_dir_all(super::gens::path(&y, 1, "spec")).unwrap();
            }
        }
        cf::online(&w);
        found(&w, X, 1, &wire_of(&w, X, 1, lf::LOST_ARGVS[0]));
        released_by_hc(&w, X, lf::LOST_ARGVS[0]);
        let reply = pass(&w);
        let held = reply["held"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["id"] == X);
        let held = held.unwrap_or_else(|| panic!("{case}: {reply:?}")).clone();
        assert_eq!(held["reason"], "occupied", "{case}: {reply:?}");
        let holder = held["holder"].as_str().unwrap();
        assert!(holder.contains(phrase), "{case}: {holder}");
        if !matches!(case, "table" | "reserved" | "evidence") {
            assert!(holder.contains("4321"), "{case}: {holder}");
        }
        assert!(gen_file(&f, 1, "released").is_some(), "{case}");
        assert_eq!(lf::count(&w, "run"), 0, "{case}");
        w.fakes.hc_done().unwrap();
    }
}

/// r6b call 14: unreadable completion evidence of the active generation: Unknown, and nothing is released.
#[test]
fn unreadable_active_evidence_leaves_x_unknown_and_nothing_released() {
    let w = World::human("");
    w.state();
    let f = member(&w, X, json!({}));
    ok(&f, 1);
    let commit = super::gens::path(&f, 1, "commit");
    std::fs::remove_file(&commit).unwrap();
    std::fs::create_dir_all(&commit).unwrap();
    w.fakes.hc_install("hc");
    let reply = pass(&w);
    let x = &reply["unknown"][0];
    assert_eq!((&x["id"], &x["check"]), (&json!(X), &json!("evidence")));
    assert!(gen_file(&f, 1, "release").is_none());
    assert!(lf::calls(&w).is_empty(), "{:?}", lf::calls(&w));
}

/// r6b call 15: no valid conversation id in the authoritative launch.json: Unknown evidence, nothing released.
#[test]
fn a_lost_member_without_a_valid_conversation_id_is_unknown_and_kept() {
    let cases = [
        ("null", json!(null)),
        ("number", json!(7)),
        ("empty", json!("")),
        ("bad", json!("x-y")),
        ("missing", Value::Null),
    ];
    for (case, sid) in cases {
        let w = World::human("");
        w.state();
        let f = member(&w, X, json!({}));
        ok(&f, 1);
        let l = super::gens::path(&f, 1, "launch");
        let mut launch = json!({"hcName": lf::hc_name(1), "launcherArgv": lf::ARGV});
        if case != "missing" {
            launch["sessionId"] = sid;
        }
        std::fs::remove_file(&l).unwrap();
        crate::state::evidence::publish(&l, &launch).unwrap();
        cf::online(&w);
        found(&w, X, 1, &wire_of(&w, X, 1, lf::LOST_ARGVS[0]));
        let reply = pass(&w);
        let x = &reply["unknown"][0];
        assert_eq!(
            (&x["id"], &x["check"]),
            (&json!(X), &json!("evidence")),
            "{case}: {reply:?}"
        );
        assert!(gen_file(&f, 1, "release").is_none(), "{case}");
        assert_eq!(lf::count(&w, "run"), 0, "{case}");
        w.fakes.hc_done().unwrap();
    }
}

/// r6b calls 15, 21: the record sessionId differs from generation 1 launch.json; the restore resumes the launch
/// session (the conversation that ran).
#[test]
fn the_restore_resumes_the_launch_session_not_the_record() {
    let w = World::human("");
    w.state();
    let f = member(&w, X, json!({"sessionId": sid_of("999999999999")}));
    ok(&f, 1);
    cf::online(&w);
    found(&w, X, 1, &wire_of(&w, X, 1, lf::LOST_ARGVS[0]));
    released_by_hc(&w, X, lf::LOST_ARGVS[0]);
    w.fakes.hc_answer(ft::address("t2"));
    let reply = pass(&w);
    assert_eq!(
        reply["restored"][0]["sessionId"],
        json!(lf::SID),
        "{reply:?}"
    );
    assert_eq!(
        gen_file(&f, 2, "spec").unwrap()["args"],
        json!(["--resume", lf::SID])
    );
    w.fakes.hc_done().unwrap();
}

/// r6b call 16: the release of X fails (its validating get refuses); X is Unknown with its check and reason, and
/// Y, after X in the walk, on its own conversation, is still restored.
#[test]
fn a_failure_restoring_one_member_leaves_it_unknown_and_the_next_is_restored() {
    let w = World::human("");
    w.state();
    let f = member(&w, X, json!({}));
    ok(&f, 1);
    let y = other(&w, Y);
    ok(&y, 1);
    cf::online(&w);
    found(&w, X, 1, &wire_of(&w, X, 1, lf::LOST_ARGVS[0]));
    lf::fails(&w, &["get", &format!("a{X}"), "--json"]);
    found(&w, Y, 1, &wire_of(&w, Y, 1, lf::LOST_ARGVS[0]));
    released_by_hc(&w, Y, lf::LOST_ARGVS[0]);
    w.fakes.hc_answer(ft::address("t2"));
    let reply = pass(&w);
    assert_eq!(ids(&reply, "restored"), [Y], "{reply:?}");
    let x = &reply["unknown"][0];
    assert_eq!(x["id"], X, "{reply:?}");
    assert!(x["check"].is_string() && x["reason"].is_string(), "{x}");
    assert_eq!(lf::count(&w, "run"), 1);
    w.fakes.hc_done().unwrap();
}

/// Row 5: Lost and failed is never restored; its shell is not released by reconcile.
#[test]
fn a_lost_failed_member_needs_recovery_and_keeps_its_shell() {
    let w = World::human("");
    w.state();
    let f = member(&w, X, json!({}));
    let failed = json!({"outcome": "failed", "reason": "no readiness report", "at": "t"});
    crate::state::evidence::publish(&super::gens::path(&f, 1, "outcome"), &failed).unwrap();
    cf::online(&w);
    found(&w, X, 1, &wire_of(&w, X, 1, lf::LOST_ARGVS[0]));
    let reply = pass(&w);
    let want = json!([{"id": X, "name": "mbbb", "reason": "no readiness report"}]);
    assert_eq!(reply["recover"], want, "{reply:?}");
    assert_eq!(lf::count(&w, "label") + lf::count(&w, "run"), 0);
    w.fakes.hc_done().unwrap();
}
