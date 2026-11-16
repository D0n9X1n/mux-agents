//! outcome-2 (#103 O2, O4): one outcome per generation, decided under the gen lock.
use super::*;
use crate::agents::fixture::ID;
use crate::agents::life_fixture::LOST_ARGVS;
use crate::agents::outcome_fixture as of;
use crate::agents::team_state::gen_lock_key;
use serde_json::json;

fn decided(v: Verdict) -> Res<Adjudged> {
    Ok(Adjudged::Decided(v))
}

// O4.2, ambiguity 8: claim `abandoned` alone (the commit is not read) or commit `aborted` is void; no failure.json.
#[test]
fn void_from_recorded_void_evidence_only() {
    for (case, claim, commit, reason) in [
        ("abandoned", Some(json!("abandoned")), None, ABANDONED),
        (
            "abandoned, torn commit",
            Some(json!("abandoned")),
            Some("torn"),
            ABANDONED,
        ),
        ("aborted, no claim", None, Some("aborted"), ABORTED),
        (
            "aborted, run claim",
            Some(json!("run")),
            Some("aborted"),
            ABORTED,
        ),
    ] {
        let g = of::world();
        if let Some(c) = claim {
            g.claim(1, c);
        }
        match commit {
            Some("aborted") => g.put(1, "commit", json!({"aborted": true})),
            Some(_) => std::fs::write(gens::path(&g.folder, 1, "commit"), b"{\"ab").unwrap(),
            None => {}
        }
        g.now(400);
        assert_eq!(
            g.judge(1, None),
            decided(Verdict::Void(reason.into())),
            "{case}"
        );
        let outcome = g
            .file(1, "outcome")
            .map(|o| (o["outcome"].clone(), o["reason"].clone()));
        assert_eq!(outcome, Some((json!("void"), json!(reason))), "{case}");
        assert_eq!(g.file(1, "failure"), None, "{case}");
    }
}

// O4.1, O4.4: K5's answer decides under the same hold: ready → ok; a recorded failure.json → failed with its reason.
#[test]
fn a_ready_or_a_recorded_failure_decides_and_writes_outcome_json() {
    let g = of::world();
    g.ran(1);
    g.report(1, 150);
    g.client(1);
    g.now(10);
    assert_eq!(g.judge(1, None), decided(Verdict::Ok));
    assert!(
        g.file(1, "ready").is_some(),
        "promoted by K5 under the same hold"
    );
    assert_eq!(
        g.file(1, "outcome").map(|o| o["outcome"].clone()),
        Some(json!("ok"))
    );
    let g = of::world();
    g.ran(1);
    g.put(
        1,
        "failure",
        json!({"reason": NO_REPORT, "evidence": {}, "at": "x"}),
    );
    g.now(10);
    assert_eq!(g.judge(1, None), decided(Verdict::Failed(NO_REPORT.into())));
    let outcome = g
        .file(1, "outcome")
        .map(|o| (o["outcome"].clone(), o["reason"].clone()));
    assert_eq!(outcome, Some((json!("failed"), json!(NO_REPORT))));
    assert_eq!(g.file(1, "ready"), None);
}

// O4.1 step 1: an outcome.json decides; nothing else is written.
#[test]
fn an_existing_outcome_decides_and_nothing_else_is_written() {
    for (body, verdict) in [
        (json!({"outcome": "ok", "at": "x"}), Verdict::Ok),
        (
            json!({"outcome": "failed", "reason": EXITED, "at": "x"}),
            Verdict::Failed(EXITED.into()),
        ),
        (
            json!({"outcome": "void", "reason": ABORTED, "at": "x"}),
            Verdict::Void(ABORTED.into()),
        ),
    ] {
        let g = of::world();
        g.put(1, "outcome", body.clone());
        g.now(400);
        assert_eq!(g.judge(1, None), decided(verdict));
        assert_eq!(g.file(1, "outcome"), Some(body));
        assert_eq!((g.file(1, "failure"), g.file(1, "ready")), (None, None));
    }
}

// Ambiguity 11: a malformed outcome.json, or a failure.json without a reason, refuses `evidence`; nothing written.
#[test]
fn a_malformed_outcome_or_failure_refuses_and_writes_nothing() {
    for (file, body) in [
        ("outcome", json!({"outcome": "maybe"})),
        ("outcome", json!({"outcome": "failed"})),
        ("outcome", json!([])),
        ("failure", json!({"evidence": {}})),
    ] {
        let g = of::world();
        g.ran(1);
        g.put(1, file, body.clone());
        g.now(400);
        assert_eq!(
            g.judge(1, None).map_err(|e| e.check),
            Err("evidence"),
            "{file} {body}"
        );
        let other = if file == "outcome" {
            "failure"
        } else {
            "outcome"
        };
        assert_eq!(
            (g.file(1, other), g.file(1, "ready")),
            (None, None),
            "{file} {body}"
        );
    }
}

// O4.3 "anything else": no claim, an uncommitted or invalid commit, a claim that is not `run`: undecided, nothing
// written, even long past the deadline.
#[test]
fn an_uncommitted_or_unresolved_generation_is_undecided_and_writes_nothing() {
    for (case, claim, commit) in [
        ("nothing", None, None),
        ("claim only", Some(json!("run")), None),
        ("commit only", None, Some(json!({"committed": true}))),
        (
            "invalid commit",
            Some(json!("run")),
            Some(json!({"committed": "yes"})),
        ),
        (
            "other claim",
            Some(json!("weird")),
            Some(json!({"committed": true})),
        ),
    ] {
        let g = of::world();
        if let Some(c) = claim {
            g.claim(1, c);
        }
        if let Some(c) = commit {
            g.put(1, "commit", c);
        }
        g.now(1000);
        assert_eq!(g.judge(1, None), Ok(Adjudged::Undecided), "{case}");
        assert_eq!(
            (g.file(1, "failure"), g.file(1, "outcome")),
            (None, None),
            "{case}"
        );
    }
}

// O2: one hold of the gen lock per call (K5 inside it, never nested); Try on a busy lock answers Busy at once and
// reads nothing; Block refuses `lock`; unplain names refuse `internal` before any lock.
#[test]
fn adjudication_holds_the_gen_lock_once_and_try_answers_busy() {
    let g = of::world();
    g.claim(1, json!("abandoned"));
    let key = gen_lock_key(&g.key, ID, 1);
    g.w.fakes.clear_log();
    assert_eq!(g.judge(1, None), decided(Verdict::Void(ABANDONED.into())));
    assert_eq!(g.w.fakes.locks(), [format!("+{key}"), format!("-{key}")]);
    let g = of::world();
    g.w.fakes.lock_timeout("gen|");
    g.w.fakes.clear_log();
    let ((got, blocked), reads) = crate::state::fs::record_reads(|| {
        (
            adjudicate(&g.w.ctx, &g.key, ID, 1, Wait::Try, None),
            g.judge(1, None).map_err(|e| e.check),
        )
    });
    assert_eq!((got, blocked), (Ok(Adjudged::Busy), Err("lock")));
    assert!(reads.is_empty(), "read while busy: {reads:?}");
    assert_eq!(g.w.fakes.locks(), [format!("?{key}"), format!("!{key}")]);
    let refused = adjudicate(&g.w.ctx, "../x", ID, 1, Wait::Block, None).map_err(|e| e.check);
    assert_eq!(refused, Err("internal"));
}

fn exit_body(reason: &str) -> serde_json::Value {
    json!({"exitCode": 1, "reason": reason, "exitedAt": "x"})
}

fn exited(g: &of::G, reason: &str) {
    g.put(1, "exit", exit_body(reason));
}

fn ended_body() -> serde_json::Value {
    json!({"address": "t1", "state": "finished", "at": "x"})
}

// O4.3, each kind: failure.json = {reason, evidence, at} is created before outcome.json (an outcome.json fault leaves
// failure.json alone); the next call decides from failure.json, with no observation. Build checklist 13: each kind's
// evidence body (the exit or ended body, the Lost terminal's {address, argv}, the deadline legs' {deadline, now});
// checklist 14: with both exit.json and ended.json the exit leg wins (the spec's leg order).
#[test]
fn each_failure_kind_writes_failure_json_first() {
    let lost: Vec<crate::hc::Terminal> = LOST_ARGVS
        .iter()
        .map(|a| of::terminal(of::HC, ID, 1, a))
        .collect();
    for (case, secs, seen, want) in [
        ("exit", 10, None, EXITED),
        ("client-conflict", 10, None, EXITED),
        ("exit and ended", 10, None, EXITED),
        ("ended", 10, None, ENDED),
        ("lost posix", 10, Some(&lost[0]), LOST),
        ("lost windows", 10, Some(&lost[1]), LOST),
        ("no report", 150, None, NO_REPORT),
        ("no client", 210, None, NO_CLIENT),
    ] {
        let g = of::world();
        g.ran(1);
        match case {
            "exit" => exited(&g, "exited"),
            "client-conflict" => exited(&g, "client-conflict"),
            "exit and ended" => {
                exited(&g, "exited");
                g.put(1, "ended", ended_body());
            }
            "ended" => g.put(1, "ended", ended_body()),
            "no client" => g.report(1, 150),
            _ => {}
        }
        let evidence = match case {
            "exit" | "exit and ended" => exit_body("exited"),
            "client-conflict" => exit_body("client-conflict"),
            "ended" => ended_body(),
            "lost posix" => json!({"address": "t1", "argv": LOST_ARGVS[0]}),
            "lost windows" => json!({"address": "t1", "argv": LOST_ARGVS[1]}),
            _ => json!({"deadline": g.at(150), "now": g.at(secs)}),
        };
        g.now(secs);
        let outcome = gens::path(&g.folder, 1, "outcome");
        crate::state::fs::fail_sync_file(Some(&outcome));
        let first = g.judge(1, seen).map_err(|e| (e.check, e.message));
        crate::state::fs::fail_sync_file(None);
        let unpublished = Err(("state", gens::NOT_PUBLISHED.to_string()));
        assert_eq!(
            first, unpublished,
            "{case}: outcome.json failed after failure.json"
        );
        let failure = g
            .file(1, "failure")
            .unwrap_or_else(|| panic!("{case}: no failure.json"));
        assert_eq!(
            (&failure["reason"], &failure["at"]),
            (&json!(want), &json!(g.at(secs))),
            "{case}"
        );
        assert_eq!(failure["evidence"], evidence, "{case}: the evidence body");
        assert_eq!(g.file(1, "outcome"), None, "{case}");
        assert_eq!(
            g.judge(1, None),
            decided(Verdict::Failed(want.into())),
            "{case}: from failure.json"
        );
        // O2: no lock holder waits on hc; the Lost legs decide from the observation alone.
        assert!(
            g.w.fakes.hc_calls().is_empty(),
            "{case}: no hc call under the gen lock"
        );
    }
}

// Ambiguity 9: both deadline legs are inclusive, and nothing fails a second early.
#[test]
fn the_deadline_legs_are_inclusive_and_nothing_fails_early() {
    for (reported, secs, want) in [
        (false, 149, None),
        (false, 150, Some(NO_REPORT)),
        (true, 150, None),
        (true, 209, None),
        (true, 210, Some(NO_CLIENT)),
    ] {
        let g = of::world();
        g.ran(1);
        if reported {
            g.report(1, 150);
        }
        g.now(secs);
        let got = g.judge(1, None);
        match want {
            None => {
                assert_eq!(
                    got,
                    Ok(Adjudged::Undecided),
                    "reported {reported}, {secs} s"
                );
                assert_eq!((g.file(1, "failure"), g.file(1, "outcome")), (None, None));
            }
            Some(r) => assert_eq!(
                got,
                decided(Verdict::Failed(r.into())),
                "reported {reported}, {secs} s"
            ),
        }
    }
}

// O4.3 "anything else" (settle-1b row 6 unresolved), before the deadline: a Running terminal, a Lost-looking one of
// another name, id or generation, or none: undecided, nothing written. A seen terminal without launch.json: evidence.
#[test]
fn a_committed_run_with_unresolved_row_6_is_undecided() {
    let lost = LOST_ARGVS[0];
    let cases = [
        ("running", of::terminal(of::HC, ID, 1, &of::LAUNCHER)),
        ("other name", of::terminal("m00000002", ID, 1, lost)),
        ("other id", of::terminal(of::HC, "bbbbbbbbbbbb", 1, lost)),
        ("other generation", of::terminal(of::HC, ID, 2, lost)),
    ];
    for (case, t) in &cases {
        let g = of::world();
        g.ran(1);
        g.now(100);
        assert_eq!(g.judge(1, Some(t)), Ok(Adjudged::Undecided), "{case}");
        assert_eq!(
            (g.file(1, "failure"), g.file(1, "outcome")),
            (None, None),
            "{case}"
        );
    }
    let g = of::world();
    g.ran(1);
    g.now(100);
    assert_eq!(g.judge(1, None), Ok(Adjudged::Undecided), "no observation");
    std::fs::remove_file(gens::path(&g.folder, 1, "launch")).unwrap();
    let got = g.judge(1, Some(&cases[1].1)).map_err(|e| e.check);
    assert_eq!(got, Err("evidence"), "a seen terminal without launch.json");
    assert_eq!(g.file(1, "failure"), None);
}

// Ambiguities 7, 9: a torn, absent or deadline-less spec.json refuses `evidence` once the deadline legs are reached,
// nothing written; it never hides an earlier leg.
#[test]
fn an_unreadable_deadline_refuses_but_never_hides_an_exit() {
    for case in ["torn", "absent", "no deadline", "bad deadline"] {
        for exit in [false, true] {
            let g = of::world();
            g.ran(1);
            let spec = gens::path(&g.folder, 1, "spec");
            std::fs::remove_file(&spec).unwrap();
            match case {
                "torn" => std::fs::write(&spec, b"{\"dead").unwrap(),
                "no deadline" => g.put(1, "spec", json!({"id": ID})),
                "bad deadline" => g.put(1, "spec", json!({"id": ID, "deadline": "soon"})),
                _ => {}
            }
            if exit {
                exited(&g, "exited");
            }
            g.now(400);
            let want = if exit {
                Ok(Adjudged::Decided(Verdict::Failed(EXITED.into())))
            } else {
                Err("evidence")
            };
            assert_eq!(
                g.judge(1, None).map_err(|e| e.check),
                want,
                "{case}, exit {exit}"
            );
            if !exit {
                assert_eq!(g.file(1, "failure"), None, "{case}");
            }
        }
    }
}

// Spec "wrong-process report": another process wins report.json while the genuine client (150) stays live and proven;
// no pair matches, so nothing fails before deadline + 60 s, then `client identity not published`.
#[test]
fn a_report_from_another_process_fails_at_deadline_plus_60_s() {
    let g = of::world();
    g.ran(1);
    g.client(1);
    g.w.fakes.process(crate::testing::row(777, 1, 1500));
    g.report(1, 777);
    for secs in [10, 150, 209] {
        g.now(secs);
        assert_eq!(g.judge(1, None), Ok(Adjudged::Undecided), "{secs} s");
    }
    g.now(210);
    assert_eq!(g.judge(1, None), decided(Verdict::Failed(NO_CLIENT.into())));
    assert_eq!(g.file(1, "ready"), None);
}

// Spec "early report, end to end": client.json lands after report-2b's 10 s wait, before the deadline: the next
// adjudication is ok. It never lands: deadline + 60 s gives `client identity not published`.
#[test]
fn early_report_end_to_end() {
    let g = of::world();
    g.ran(1);
    g.report(1, 150);
    g.now(30);
    assert_eq!(
        g.judge(1, None),
        Ok(Adjudged::Undecided),
        "client.json not yet published"
    );
    g.client(1);
    g.now(100);
    assert_eq!(g.judge(1, None), decided(Verdict::Ok));
    let g = of::world();
    g.ran(1);
    g.report(1, 150);
    g.now(210);
    assert_eq!(g.judge(1, None), decided(Verdict::Failed(NO_CLIENT.into())));
}

// Spec: ready before the deadline, then Lost: ok. K5 decides at step 3, before any failure leg.
#[test]
fn ready_before_the_deadline_then_lost_stays_ok() {
    let g = of::world();
    g.ran(1);
    g.report(1, 150);
    g.client(1);
    g.now(20);
    assert_eq!(
        gens::promote_ready(&g.w.ctx, &g.key, ID, 1),
        Ok(gens::Ready::Ok),
        "the handshake promotes"
    );
    g.now(400);
    let lost = of::terminal(of::HC, ID, 1, LOST_ARGVS[0]);
    assert_eq!(g.judge(1, Some(&lost)), decided(Verdict::Ok));
    assert_eq!(g.file(1, "failure"), None);
}

// R1: under the lock each file is opened at most once, and nothing after the deciding leg is opened. Absent files
// are never opened (`evidence::read`), so the lists name only present files, in order.
#[test]
fn the_failure_legs_read_each_file_once() {
    let g = of::world();
    g.ran(1);
    exited(&g, "exited");
    g.now(10);
    let (got, reads) = crate::state::fs::record_reads(|| g.judge(1, None));
    assert_eq!(got, decided(Verdict::Failed(EXITED.into())));
    let p = |f: &str| gens::path(&g.folder, 1, f);
    let want = [gens::claim_path(&g.folder, 1), p("commit"), p("exit")];
    assert_eq!(reads, want, "no spec.json, launch.json or second read");
    let g = of::world();
    g.ran(1);
    g.now(150);
    let (got, reads) = crate::state::fs::record_reads(|| g.judge(1, None));
    assert_eq!(got, decided(Verdict::Failed(NO_REPORT.into())));
    let p = |f: &str| gens::path(&g.folder, 1, f);
    assert_eq!(
        reads,
        [gens::claim_path(&g.folder, 1), p("commit"), p("spec")]
    );
}

// Call 15 at the outcome level: a torn client.json or launcher.json at deadline + 60 s refuses `evidence`; no
// failure.json, no outcome.json (never `client identity not published` decided from unread evidence).
#[test]
fn an_unreadable_client_proof_at_deadline_plus_60_s_decides_nothing() {
    for torn in ["client", "launcher"] {
        let g = of::world();
        g.ran(1);
        g.client(1);
        g.report(1, 150);
        let at = gens::path(&g.folder, 1, torn);
        std::fs::remove_file(&at).unwrap();
        std::fs::write(&at, b"{\"pi").unwrap();
        g.now(210);
        assert_eq!(
            g.judge(1, None).map_err(|e| e.check),
            Err("evidence"),
            "torn {torn}.json"
        );
        assert_eq!(
            (g.file(1, "failure"), g.file(1, "outcome")),
            (None, None),
            "{torn}"
        );
    }
}

// Call 16, R1: a nonmatching report and an exit.json: one adjudication reads exit.json once (K5's read, handed back as
// `Held.exit`), and the exit leg decides from it.
#[test]
fn a_nonmatching_report_reads_exit_json_once() {
    let g = of::world();
    g.ran(1);
    g.client(1);
    g.w.fakes.process(crate::testing::row(777, 1, 1500));
    g.report(1, 777);
    exited(&g, "client-conflict");
    g.now(10);
    let exit = gens::path(&g.folder, 1, "exit");
    let (got, reads) = crate::state::fs::record_reads(|| g.judge(1, None));
    assert_eq!(got, decided(Verdict::Failed(EXITED.into())));
    assert_eq!(reads.iter().filter(|p| **p == exit).count(), 1, "{reads:?}");
}

// R2 on every reached path: a torn claim, commit, exit, ended or launch.json (a Lost terminal is seen, so launch.json
// is reached) while later evidence could decide (Lost, or no report at 400 s): `evidence`, nothing written.
#[test]
fn an_unreadable_input_on_a_reached_path_refuses_and_writes_nothing() {
    let lost = of::terminal(of::HC, ID, 1, LOST_ARGVS[0]);
    for torn in ["claim", "commit", "exit", "ended", "launch"] {
        let g = of::world();
        g.ran(1);
        let at = if torn == "claim" {
            gens::claim_path(&g.folder, 1)
        } else {
            gens::path(&g.folder, 1, torn)
        };
        let _ = std::fs::remove_file(&at);
        std::fs::write(&at, b"{\"to").unwrap();
        g.now(400);
        assert_eq!(
            g.judge(1, Some(&lost)).map_err(|e| e.check),
            Err("evidence"),
            "torn {torn}"
        );
        assert_eq!(
            (g.file(1, "failure"), g.file(1, "outcome")),
            (None, None),
            "{torn}"
        );
    }
}
