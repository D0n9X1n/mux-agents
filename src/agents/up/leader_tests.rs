//! restore-6c Task 9: `up` on an existing team: the cascade finished, settled, the leader resumed, held or
//! refused, the team restored and shown (P4; calls 17-19, 25; checklist 31, 32).
use crate::agents::core_fixture as cf;
use crate::agents::core_launch::forward_tests as ft;
use crate::agents::fixture::World;
use crate::agents::life_fixture as lf;
use crate::agents::reconcile_restore_tests::{L, released_by_hc, sid_of, unmanaged_claude};
use crate::agents::reconcile_run_tests::ids;
use crate::agents::reconcile_tests::{X, found, member, ok, seed_index, wire_of};
use crate::agents::reconcile_verb_tests::person;
use serde_json::{Value, json};
use std::path::PathBuf;

/// `mux-agent up -Client claude` through the real dispatcher.
fn up(w: &World) -> (u8, Value) {
    let args: Vec<String> = ["up", "-Client", "claude"].map(String::from).to_vec();
    let (code, out) = crate::cli::run(&args, || Ok(&w.ctx));
    (code, serde_json::from_str(&out).unwrap())
}
/// The team T `up` derives for this folder, client and empty name.
fn team_t(w: &World) -> String {
    let folder = super::identity::folder(&w.ctx).unwrap();
    super::identity::team(&w.ctx, &folder, crate::parse::Client::Claude, "").unwrap()
}
/// The team an earlier `up` made (index and team record at the caller's key and session), its leader L Ended:
/// generation 1 ready, `ok`, ended, desired running.
fn ended_team(w: &World) -> PathBuf {
    let t = team_t(w);
    let lead = member(w, L, json!({"depth": 0, "upTeam": t, "name": "leader"}));
    ok(&lead, 1);
    let end = json!({"address": format!("a{L}"), "hcName": lf::hc_name(1), "labels": {}, "state": "finished",
        "reason": "exited", "exitCode": 0, "at": "t"});
    crate::state::evidence::publish(&crate::agents::gens::path(&lead, 1, "ended"), &end).unwrap();
    seed_index(w, &t, &w.caller().server_key, &w.caller().session);
    lead
}
/// 5-9 F13: X, a Copilot child of L with its own conversation (record and `launch.json`) and Copilot options.
fn copilot_child(w: &World) -> PathBuf {
    let f = member(
        w,
        X,
        json!({"client": "copilot", "sessionId": sid_of(X),
        "options": ["--reasoning-effort", "high"], "parentAgent": {"id": L}}),
    );
    ok(&f, 1);
    let l = crate::agents::gens::path(&f, 1, "launch");
    std::fs::remove_file(&l).unwrap();
    let launch =
        json!({"hcName": lf::hc_name(1), "launcherArgv": lf::ARGV, "sessionId": sid_of(X)});
    crate::state::evidence::publish(&l, &launch).unwrap();
    f
}
/// The member loop: X Lost, released (R1), relaunched as generation 2.
fn lost_child(w: &World) {
    found(w, X, 1, &wire_of(w, X, 1, lf::LOST_ARGVS[0]));
    released_by_hc(w, X, lf::LOST_ARGVS[0]);
    w.fakes.hc_answer(ft::address("t2"));
}
/// Re-entry's own `hc::Lazy` after the pass: X's generation 2 is listed nowhere (Unknown, no view).
fn reentry_unlisted(w: &World) {
    cf::online(w);
    cf::answer(
        w,
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

/// Spec "held leader, Lost child", call 19, checklist 31-32, through `up`:
/// - `held`: an unmanaged Claude holds the Ended leader's conversation;
/// - `refused`: the leader's `hc run` fails;
/// - `unreadable`: the leader's generation 1 outcome cannot be read (never read as absent: 5-9 F8).
///
/// In each, the Lost Copilot child X is restored on its own conversation, re-entry presents the team, `up` exits 0.
#[test]
fn up_reports_a_held_or_refused_leader_and_still_restores() {
    for case in ["held", "refused", "unreadable"] {
        let w = person();
        let lead = ended_team(&w);
        let x = copilot_child(&w);
        cf::online(&w); // the pass's Lazy (`up`'s eager connection took `person()`'s)
        match case {
            "held" => unmanaged_claude(&w, 4321),
            "refused" => w.fakes.hc_answer(Ok(crate::spawn::Out {
                code: 1,
                stderr: "spawn failed".into(),
                ..Default::default()
            })),
            _ => {
                let o = crate::agents::gens::path(&lead, 1, "outcome");
                std::fs::remove_file(&o).unwrap();
                std::fs::create_dir_all(&o).unwrap();
            }
        }
        lost_child(&w);
        reentry_unlisted(&w);
        let (code, reply) = up(&w);
        assert_eq!(code, 0, "{case}: {reply}");
        assert_eq!(
            (&reply["created"], &reply["reentered"]),
            (&json!(false), &json!(true)),
            "{case}: {reply}"
        );
        let got = &reply["leaderResume"];
        match case {
            "held" => assert!(
                got["held"] == "occupied"
                    && got["holder"].as_str().unwrap().starts_with("process 4321"),
                "{reply}"
            ),
            "refused" => assert!(
                got["refused"].is_string(),
                "reported, not returned: {reply}"
            ),
            _ => assert_eq!(got["refused"], "evidence", "{reply}"),
        }
        assert_eq!(
            reply["leader"]["id"], L,
            "{case}: the leader is not absent: {reply}"
        );
        assert_eq!(
            ids(reply.as_object().unwrap(), "restored"),
            vec![X.to_string()],
            "{case}: {reply}"
        );
        let args: Vec<String> = serde_json::from_value(
            crate::agents::gens::read_flushed(&x, 2, "spec")
                .unwrap()
                .expect("spec")["args"]
                .clone(),
        )
        .unwrap();
        assert!(
            args.contains(&sid_of(X)),
            "{case}: X resumes its own conversation: {args:?}"
        );
        assert!(
            reply["views"].is_array() && reply.get("members").is_some(),
            "{case}: presentation ran: {reply}"
        );
        assert_eq!(
            lf::record(&lead)["generation"],
            json!("1"),
            "{case}: L not resumed"
        );
        w.fakes.hc_done().unwrap();
    }
}

/// 5-9 F18, F22, decision 9 (checklist 71): leader selection is deterministic. An eligible Ended leader of another
/// session under the same key is never resumed or touched; a torn record in the agents folder (its session unknown)
/// makes `up` refuse `evidence` before the replay loop and create nothing.
#[test]
fn up_resumes_only_this_sessions_leader_and_refuses_an_unknown_one() {
    const E: &str = "eeeeeeeeeeee";
    for case in ["foreign", "torn"] {
        let w = person();
        let lead = ended_team(&w);
        let other = w
            .ctx
            .state_root
            .join(w.caller().server_key)
            .join("agents")
            .join(E);
        if case == "foreign" {
            let f = crate::agents::reconcile_tests::member_at(
                &w,
                &w.caller().server_key,
                E,
                json!({"depth": 0, "upTeam": team_t(&w), "session": "$99", "name": "leader"}),
            );
            ok(&f, 1);
            let end = json!({"address": "aE", "hcName": lf::hc_name(1), "labels": {}, "state": "finished",
                "reason": "exited", "exitCode": 0, "at": "t"});
            crate::state::evidence::publish(&crate::agents::gens::path(&f, 1, "ended"), &end)
                .unwrap();
        } else {
            std::fs::create_dir_all(&other).unwrap();
            std::fs::write(other.join("record.json"), "{torn").unwrap();
        }
        let before = crate::agents::prelife_fixture::bytes(&other);
        if case == "foreign" {
            cf::online(&w); // the pass
            w.fakes.hc_answer(ft::address("t2"));
            cf::online(&w); // re-entry: L's generation 2 listed nowhere
            cf::answer(
                &w,
                &[
                    "ls",
                    "--all",
                    "--label",
                    &format!("mux.id={L}"),
                    "--label",
                    "mux.gen=2",
                    "--json",
                ],
                r#"{"terminals":[]}"#,
            );
            let (code, reply) = up(&w);
            assert_eq!(code, 0, "{reply}");
            assert_eq!(
                reply["leaderResume"],
                json!({"resumed": {"generation": 2}}),
                "{reply}"
            );
            assert_eq!(
                lf::record(&lead)["generation"],
                json!("2"),
                "this session's leader resumed"
            );
            assert_eq!(lf::count(&w, "run"), 1, "only this session's leader");
            w.fakes.hc_done().unwrap();
        } else {
            let (code, reply) = up(&w);
            assert_eq!(
                (code, &reply["check"]),
                (1, &json!("evidence")),
                "torn: {reply}"
            );
            assert_eq!(lf::count(&w, "run"), 0, "torn: nothing launched");
            assert_eq!(
                lf::record(&lead)["generation"],
                json!("1"),
                "torn: L untouched"
            );
        }
        assert_eq!(
            crate::agents::prelife_fixture::bytes(&other),
            before,
            "{case}: the other record untouched"
        );
    }
}

/// Re-entry's own `hc::Lazy` after the pass: L's generation 2 is listed nowhere (Unknown, no view).
fn reentry_lead_unlisted(w: &World) {
    cf::online(w);
    cf::answer(
        w,
        &[
            "ls",
            "--all",
            "--label",
            &format!("mux.id={L}"),
            "--label",
            "mux.gen=2",
            "--json",
        ],
        r#"{"terminals":[]}"#,
    );
}

/// Calls 16-17 for the leader, checklist 41 (5-9 F17): two concurrent `up`s of a team whose leader Ended. A pauses
/// after its leader's promotion while holding its locks; B is proven waiting (`when_held`), then finds generation 2
/// Running and resumes nothing. One `hc run` across both; each thread's hc script is fully used.
#[test]
fn concurrent_ups_resume_the_leader_once() {
    use crate::agents::recover_crash_tests::{person_at, recorded_wire};
    use std::sync::mpsc::channel;
    let w = person();
    let lead = ended_team(&w);
    let (root, cwd) = (w.ctx.state_root.clone(), w.ctx.cwd.clone());
    let (paused, at_promote) = channel();
    let (go, wait_go) = channel::<()>();
    let a = {
        let (root, cwd) = (root.clone(), cwd.clone());
        std::thread::spawn(move || {
            let w = person_at(&root, &cwd);
            cf::online(&w); // the pass
            w.fakes.hc_answer(ft::address("t2"));
            reentry_lead_unlisted(&w);
            crate::agents::core_launch::hooks::after(Some(Box::new(move |s: &str| {
                if s == "promote" {
                    paused.send(()).unwrap();
                    wait_go.recv().unwrap();
                }
            })));
            let (code, reply) = up(&w);
            crate::agents::core_launch::hooks::after(None);
            w.fakes.hc_done().unwrap();
            (code, reply["leaderResume"].clone(), lf::count(&w, "run"))
        })
    };
    at_promote
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("A reached promote");
    let (waited, saw_wait) = channel();
    let b = std::thread::spawn(move || {
        let w = person_at(&root, &cwd);
        let f = w
            .ctx
            .state_root
            .join(w.caller().server_key)
            .join("agents")
            .join(L);
        let wire = recorded_wire(&w, &f, 2, None);
        cf::online(&w); // the pass: L's generation 2 observed once (Running: no step)
        found(&w, L, 2, &wire);
        reentry_lead_unlisted(&w);
        crate::state::lock::hooks::when_held(Some(Box::new(move || {
            let _ = waited.send(());
        })));
        let (code, reply) = up(&w);
        crate::state::lock::hooks::when_held(None);
        w.fakes.hc_done().unwrap();
        (
            code,
            reply.get("leaderResume").cloned(),
            lf::count(&w, "run"),
        )
    });
    saw_wait
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("B waits while A holds its locks");
    go.send(()).unwrap();
    let (code_a, ra, runs_a) = a.join().unwrap();
    let (code_b, rb, runs_b) = b.join().unwrap();
    assert_eq!(
        (code_a, ra),
        (0, json!({"resumed": {"generation": 2}})),
        "A resumed L"
    );
    assert_eq!(
        (code_b, rb),
        (0, None),
        "B found L Running and resumed nothing"
    );
    assert_eq!(runs_a + runs_b, 1, "one hc run");
    assert_eq!(lf::record(&lead)["generation"], json!("2"), "promoted once");
}
