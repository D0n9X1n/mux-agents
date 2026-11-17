//! restore-6c Task 8 (#103): an Ended leader resumes as the next generation of its id (P4.3; calls 17-19, 25).
use super::core_fixture as cf;
use super::core_launch::forward_tests as ft;
use super::fixture::World;
use super::life_fixture as lf;
use super::reconcile::{Pass, PassFor, pass};
use super::reconcile_restore_tests::{L, released_by_hc, sid_of, unmanaged_claude};
use super::reconcile_run_tests::ids;
use super::reconcile_tests::{X, found, member, ok, wire_of};
use super::reconcile_verb_tests::person;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn pass_with(w: &World) -> Value {
    let who = PassFor::Up {
        leader: Some(L),
        step: Some(super::leader::resume_ended),
    };
    match pass(&w.ctx, &w.caller(), who).unwrap() {
        Pass::Done(m) => Value::Object(m),
        Pass::Stopped => panic!("stopped"),
    }
}

/// The Ended leader L of this scope: generation 1 ready, `ok`, ended (`exit-<L>-1`), desired running. Its team
/// record is the one `member` writes (5-9 F14).
fn ended_leader(w: &World) -> PathBuf {
    let lead = member(w, L, json!({"depth": 0, "upTeam": "T", "name": "leader"}));
    ok(&lead, 1);
    let end = json!({"address": format!("a{L}"), "hcName": lf::hc_name(1), "labels": {}, "state": "finished",
        "reason": "exited", "exitCode": 0, "at": "t"});
    crate::state::evidence::publish(&super::gens::path(&lead, 1, "ended"), &end).unwrap();
    lead
}

/// 5-9 F13: X, a Copilot child with its own conversation in its record and `launch.json` and valid Copilot options,
/// Lost: found, released (R1), then relaunched as generation 2 (its `hc run` is the caller's to queue).
fn lost_copilot_child(w: &World) -> PathBuf {
    let f = member(
        w,
        X,
        json!({"client": "copilot", "sessionId": sid_of(X),
        "options": ["--reasoning-effort", "high"]}),
    );
    ok(&f, 1);
    let l = super::gens::path(&f, 1, "launch");
    std::fs::remove_file(&l).unwrap();
    let launch =
        json!({"hcName": lf::hc_name(1), "launcherArgv": lf::ARGV, "sessionId": sid_of(X)});
    crate::state::evidence::publish(&l, &launch).unwrap();
    found(w, X, 1, &wire_of(w, X, 1, lf::LOST_ARGVS[0]));
    released_by_hc(w, X, lf::LOST_ARGVS[0]);
    f
}

fn spec(f: &Path, n: u64) -> Value {
    super::gens::read_flushed(f, n, "spec")
        .unwrap()
        .expect("spec")
}

/// X's successor resumes X's own conversation, never the leader's.
fn child_resumed(f: &Path) {
    let args = spec(f, 2)["args"].clone();
    let args: Vec<String> = serde_json::from_value(args).unwrap();
    assert!(
        args.contains(&sid_of(X)),
        "X resumes its own conversation: {args:?}"
    );
    assert!(
        !args.iter().any(|a| a == lf::SID),
        "never the leader's: {args:?}"
    );
}

/// Spec "same-id resume" (leader leg): n+1 of the same id, kind `leader-resume`, the resume recipe, no prompt;
/// still the root; no cap slot (no listing is scripted). The only hc call for L is forward's `hc run`.
#[test]
fn up_resumes_an_ended_leader_as_its_next_generation() {
    let w = person();
    let lead = ended_leader(&w);
    w.fakes.hc_answer(ft::address("t2"));
    let reply = pass_with(&w);
    assert_eq!(
        reply["leaderResume"],
        json!({"resumed": {"generation": 2}}),
        "{reply}"
    );
    let r = lf::record(&lead);
    assert_eq!(
        (
            &r["id"],
            &r["generation"],
            &r["sessionId"],
            &r["depth"],
            r.get("parentAgent"),
            &r["upTeam"]
        ),
        (
            &json!(L),
            &json!("2"),
            &json!(lf::SID),
            &json!(0),
            None,
            &json!("T")
        ),
        "the same root"
    );
    let s = spec(&lead, 2);
    assert_eq!(
        (&s["kind"], &s["args"]),
        (&json!("leader-resume"), &json!(["--resume", lf::SID])),
        "recipe"
    );
    assert!(s.get("prompt").is_none(), "no prompt (6c-R4)");
    w.fakes.hc_done().unwrap();
}

/// Ambiguity 9, calls 19, 25: held (an occupied conversation; an older open generation; an Ended generation still
/// `completing`), refused (an unknown client; a real launch refusal), or nothing (not Ended). A refusal never hides
/// the rest of the pass: the Lost Copilot child X is still restored, on its own conversation.
#[test]
fn an_ended_leader_is_held_refused_or_left() {
    for case in [
        "occupied",
        "older",
        "completing",
        "client",
        "session",
        "launch",
        "running",
    ] {
        let w = person();
        let lead = if case == "running" {
            let l = member(&w, L, json!({"depth": 0, "upTeam": "T", "name": "leader"}));
            let deadline =
                crate::util::utc_o(w.ctx.clock.now() + std::time::Duration::from_secs(86_400));
            let s = json!({"deadline": deadline, "sessionId": lf::SID});
            crate::state::evidence::publish(&super::gens::path(&l, 1, "spec"), &s).unwrap();
            l
        } else {
            ended_leader(&w)
        };
        match case {
            "occupied" => unmanaged_claude(&w, 4321),
            "older" => {
                lf::seed_gen(&lead, 0, Some("run"), lf::committed());
                // the fence's own Lazy at `Tx::open`: hc lists nothing for gen 0 -> Unknown, reserved
                cf::answer(
                    &w,
                    &[
                        "ls",
                        "--all",
                        "--label",
                        &format!("mux.id={L}"),
                        "--label",
                        "mux.gen=0",
                        "--json",
                    ],
                    r#"{"terminals":[]}"#,
                );
                cf::online(&w); // the pass Lazy: the fence's own Lazy took `person()`'s `--version`
            }
            "completing" | "client" | "session" => {
                let mut r = lf::record(&lead);
                if case == "completing" {
                    // 5-9 F12: nothing but this record's `completing` keeps the leader from resuming
                    r["completing"] = json!({"gen": 1});
                } else if case == "client" {
                    r["client"] = json!("nosuch");
                } else {
                    r["sessionId"] = json!("not-a-conversation");
                }
                crate::state::fs::write_json(&lead.join("record.json"), &r).unwrap();
            }
            "running" => found(&w, L, 1, &wire_of(&w, L, 1, &lf::ARGV)), // observed once (5-9 F9)
            _ => {}
        }
        if case == "launch" {
            w.fakes.hc_answer(Ok(crate::spawn::Out {
                code: 1,
                stderr: "spawn failed".into(),
                ..Default::default()
            }));
        }
        let x = lost_copilot_child(&w);
        w.fakes.hc_answer(ft::address("t2"));
        let reply = pass_with(&w);
        let got = &reply["leaderResume"];
        match case {
            "occupied" => assert!(
                got["held"] == "occupied"
                    && got["holder"].as_str().unwrap().starts_with("process 4321"),
                "{reply}"
            ),
            "older" => assert_eq!(got, &json!({"held": "older"}), "{reply}"),
            "completing" => assert_eq!(got, &json!({"held": "completing"}), "{reply}"),
            "client" => assert_eq!(got["refused"], "internal", "{reply}"),
            "session" => assert_eq!(got["refused"], "evidence", "{reply}"),
            "launch" => {
                assert!(got["refused"].is_string(), "{case}: {reply}");
                assert_eq!(
                    lf::count(&w, "run"),
                    2,
                    "the leader's hc run was tried, then X's"
                );
            }
            _ => assert!(reply.get("leaderResume").is_none(), "not Ended: {reply}"),
        }
        if case != "launch" {
            assert_eq!(lf::count(&w, "run"), 1, "{case}: X's hc run only");
            assert_eq!(
                lf::record(&lead)["generation"],
                json!("1"),
                "{case}: no successor"
            );
            assert!(
                lf::record(&lead).get("pending").is_none(),
                "{case}: nothing allocated"
            );
        }
        assert_eq!(
            ids(reply.as_object().unwrap(), "restored"),
            vec![X.to_string()],
            "{case}: {reply}"
        );
        child_resumed(&x);
        w.fakes.hc_done().unwrap();
    }
}

/// Spec "`exit-<id>-1` recorded once" (call 6). The leader has no parent pane, so `registry::post` mails nothing
/// and records the generation's notice. The resumed leader's generation 2 notice is published once
/// (`gens/2/notice.json`); a second post changes nothing. Generation 1's notice is its own.
#[test]
fn a_resumed_leaders_exit_is_recorded_once_per_generation() {
    let w = person();
    let lead = ended_leader(&w);
    w.fakes.hc_answer(ft::address("t2"));
    assert_eq!(
        pass_with(&w)["leaderResume"],
        json!({"resumed": {"generation": 2}})
    );
    w.fakes.hc_done().unwrap();
    let end = json!({"address": "t2", "hcName": lf::hc_name(2), "labels": {}, "state": "finished",
        "reason": "exited", "exitCode": 0, "at": "t"});
    crate::state::evidence::publish(&super::gens::path(&lead, 2, "ended"), &end).unwrap();
    let record = lf::record(&lead);
    let post = |n: u64| {
        super::registry::add_notice_gen(
            &w.ctx,
            &w.caller(),
            L,
            &record,
            n,
            super::registry::Notice::Exited(json!(0)),
        )
        .unwrap()
    };
    assert!(post(1), "generation 1 is noticed");
    assert!(post(2), "generation 2 is noticed on its own");
    let first = std::fs::read(super::gens::path(&lead, 2, "notice")).expect("gens/2/notice.json");
    assert!(!post(2), "a second post of generation 2 is refused");
    assert_eq!(
        std::fs::read(super::gens::path(&lead, 2, "notice")).unwrap(),
        first,
        "unchanged"
    );
    // 5-9 F18: `preserve_order` keeps the writer's order `kind, exitCode, at`
    for n in [1, 2] {
        let notice = super::gens::read_flushed(&lead, n, "notice")
            .unwrap()
            .expect("notice");
        let keys: Vec<&String> = notice.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["kind", "exitCode", "at"], "generation {n}: {notice}");
        assert_eq!(notice["exitCode"], json!(0), "generation {n}");
    }
}

/// 5-9 F17: killed after `commit` of the leader's generation 2. The next pass promotes the committed pending
/// (settle row 6) and resumes nothing; a pass after it changes nothing: one `hc run` in all.
#[test]
#[ignore = "a child of a_leader_resume_killed_after_commit_is_promoted_once"]
fn child_leader_resume() {
    let w = person();
    ended_leader(&w);
    w.fakes.hc_answer(ft::address("t2"));
    println!("CRASH-ROOT {}", w.ctx.state_root.display());
    println!("CRASH-CWD {}", w.ctx.cwd.display());
    println!("CRASH-TEMP {}", w._dir.path().display());
    let _ = pass_with(&w);
}

#[test]
fn a_leader_resume_killed_after_commit_is_promoted_once() {
    use super::core_launch::crash;
    use super::recover_crash_tests::{ChildDir, person_at, recorded_wire};
    let kv = crash::killed_at("agents::leader_resume_tests::child_leader_resume", "commit");
    let _tmp = ChildDir::new(crash::printed(&kv, "TEMP"));
    let w = person_at(&crash::printed(&kv, "ROOT"), &crash::printed(&kv, "CWD"));
    let lead = w
        .ctx
        .state_root
        .join(w.caller().server_key)
        .join("agents")
        .join(L);
    assert_eq!(
        lf::record(&lead)["pending"]["kind"],
        json!("leader-resume"),
        "the crash left the pending"
    );
    crate::state::evidence::publish(&super::gens::claim_path(&lead, 2), &json!("run")).unwrap();
    let wire = recorded_wire(&w, &lead, 2, None);
    found(&w, L, 2, &wire); // settle's Lazy: row 6 Running -> promote
    cf::online(&w); // the pass's Lazy (settle's took `person()`'s `--version`)
    found(&w, L, 2, &wire); // the leader's one observation: Running, so the step leaves it
    let reply = pass_with(&w);
    assert!(
        reply.get("leaderResume").is_none(),
        "promoted, not resumed again: {reply}"
    );
    assert_eq!(lf::record(&lead)["generation"], json!("2"), "promoted once");
    assert!(
        lf::record(&lead).get("pending").is_none(),
        "nothing pending"
    );
    assert_eq!(lf::count(&w, "run"), 0, "no second hc run");
    w.fakes.hc_done().unwrap();
    // 5-9 F20: a repeated pass finds the promoted generation 2 Running and launches nothing
    let before = super::prelife_fixture::bytes(&lead);
    cf::online(&w);
    found(&w, L, 2, &wire);
    let again = pass_with(&w);
    assert!(
        again.get("leaderResume").is_none(),
        "the repeat resumes nothing: {again}"
    );
    assert_eq!(
        super::prelife_fixture::bytes(&lead),
        before,
        "the repeat changes nothing"
    );
    assert_eq!(lf::count(&w, "run"), 0, "still no hc run");
    w.fakes.hc_done().unwrap();
}

/// Row 8.13 (5-9 F20): a pending generation that settle cannot resolve (an unknown claim) stays after settle; the
/// leader waits as `older`, and nothing is allocated over it.
#[test]
fn an_ended_leader_with_an_unresolved_pending_waits() {
    let w = person();
    let lead = ended_leader(&w);
    lf::seed_gen(&lead, 2, Some("bogus"), None);
    let mut r = lf::record(&lead);
    r["pending"] = lf::pending(2);
    crate::state::fs::write_json(&lead.join("record.json"), &r).unwrap();
    let reply = pass_with(&w);
    assert!(
        reply.get("settleError").is_some(),
        "settle refused it: {reply}"
    );
    assert_eq!(reply["leaderResume"], json!({"held": "older"}), "{reply}");
    assert_eq!(lf::count(&w, "run"), 0, "no hc run");
    assert_eq!(
        lf::record(&lead)["pending"]["gen"],
        json!(2),
        "the pending is untouched"
    );
}

/// Row 8.16 (equivalent; the step's own desired check was removed): a leader whose desired state cannot be read is
/// never resumed: the pass reports `stoppedError` and the leader's observation refuses `evidence`, so no gate (here
/// an occupied conversation) answers and nothing launches (never read as running, call 25).
#[test]
fn an_ended_leader_with_unreadable_desired_state_is_refused() {
    let w = person();
    let lead = ended_leader(&w);
    std::fs::create_dir_all(lead.join("desired.json")).unwrap();
    // an occupied conversation: only the desired check answers before occupancy would report a hold
    unmanaged_claude(&w, 4321);
    let reply = pass_with(&w);
    assert!(reply.get("stoppedError").is_some(), "{reply}");
    assert_eq!(reply["leaderResume"]["refused"], "evidence", "{reply}");
    assert_eq!(lf::count(&w, "run"), 0, "no hc run");
    assert!(w.fakes.hc_calls().is_empty(), "hc is never asked");
}
