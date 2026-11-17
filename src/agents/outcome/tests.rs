//! outcome-2 (O2, O4): one outcome per generation, decided under the gen lock.
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

fn unpublished() -> Result<Adjudged, (&'static str, String)> {
    Err(("state", gens::NOT_PUBLISHED.to_string()))
}
fn unflushed() -> Result<Adjudged, (&'static str, String)> {
    Err(("state", gens::NOT_FLUSHED.to_string()))
}
fn judged(
    g: &of::G,
    seen: Option<&crate::hc::Terminal>,
) -> Result<Adjudged, (&'static str, String)> {
    g.judge(1, seen).map_err(|e| (e.check, e.message))
}

// Ambiguities 11, 12: a failed gens/1 flush leaves failure.json, then outcome.json, linked but unflushed; while the
// flush keeps failing nothing decides (a reboot could lose the file); once it succeeds, the same verdict.
#[test]
fn an_unflushed_outcome_is_confirmed_before_it_decides() {
    let g = of::world();
    g.ran(1);
    exited(&g, "exited");
    g.now(10);
    crate::state::fs::fail_sync_dir(Some(&gens::dir(&g.folder, 1)));
    assert_eq!(
        judged(&g, None),
        unflushed(),
        "call 1: failure.json linked, unflushed"
    );
    assert!(g.file(1, "failure").is_some() && g.file(1, "outcome").is_none());
    assert_eq!(
        judged(&g, None),
        unflushed(),
        "call 2: outcome.json linked, unflushed"
    );
    assert!(g.file(1, "outcome").is_some());
    assert_eq!(
        judged(&g, None),
        unflushed(),
        "call 3: an unflushed outcome.json decides nothing"
    );
    crate::state::fs::fail_sync_dir(None);
    assert_eq!(g.judge(1, None), decided(Verdict::Failed(EXITED.into())));
}

// Spec "failure injection at failure.json": nothing linked; the next call reaches the same verdict.
#[test]
fn failure_injection_at_failure_json_gives_the_same_verdict_next_call() {
    let g = of::world();
    g.ran(1);
    exited(&g, "exited");
    g.now(10);
    crate::state::fs::fail_sync_file(Some(&gens::path(&g.folder, 1, "failure")));
    assert_eq!(judged(&g, None), unpublished());
    crate::state::fs::fail_sync_file(None);
    assert_eq!((g.file(1, "failure"), g.file(1, "outcome")), (None, None));
    assert_eq!(g.judge(1, None), decided(Verdict::Failed(EXITED.into())));
}

// Spec "no reversal": failure.json is created, the outcome.json write fails, then the launcher publishes a matching
// client.json; the next adjudication is still failed and no ready.json exists. The same with an earlier exit.json.
#[test]
fn no_reversal_after_a_lost_outcome_write() {
    for (case, secs, want) in [("report, no client", 210, NO_CLIENT), ("exit", 10, EXITED)] {
        let g = of::world();
        g.ran(1);
        g.report(1, 150);
        if case == "exit" {
            exited(&g, "exited");
        }
        g.now(secs);
        crate::state::fs::fail_sync_file(Some(&gens::path(&g.folder, 1, "outcome")));
        assert_eq!(judged(&g, None), unpublished(), "{case}");
        crate::state::fs::fail_sync_file(None);
        g.client(1);
        assert_eq!(
            g.judge(1, None),
            decided(Verdict::Failed(want.into())),
            "{case}"
        );
        assert_eq!(g.file(1, "ready"), None, "{case}");
        let recorded = g.file(1, "outcome").unwrap();
        assert_eq!(
            gens::promote_ready(&g.w.ctx, &g.key, ID, 1),
            Ok(gens::Ready::Outcome(recorded)),
            "{case}"
        );
    }
}

// Spec "Lost then expired": the Lost observation fails the generation; its hc record then expires (no observation):
// still failed, from failure.json.
#[test]
fn lost_then_expired_still_fails() {
    let g = of::world();
    g.ran(1);
    g.now(10);
    let lost = of::terminal(of::HC, ID, 1, LOST_ARGVS[1]);
    crate::state::fs::fail_sync_file(Some(&gens::path(&g.folder, 1, "outcome")));
    assert_eq!(judged(&g, Some(&lost)), unpublished());
    crate::state::fs::fail_sync_file(None);
    assert_eq!(
        g.judge(1, None),
        decided(Verdict::Failed(LOST.into())),
        "no observation any more"
    );
}

// Spec "torn evidence": a torn failure.json or outcome.json stops undecided (`evidence`), writes nothing and leaves
// its bytes; a whole file then gives the same verdict.
#[test]
fn torn_failure_or_outcome_stops_undecided_and_writes_nothing() {
    for file in ["failure", "outcome"] {
        let g = of::world();
        g.ran(1);
        g.now(400);
        let at = gens::path(&g.folder, 1, file);
        std::fs::write(&at, b"{\"rea").unwrap();
        assert_eq!(
            g.judge(1, None).map_err(|e| e.check),
            Err("evidence"),
            "{file}"
        );
        assert_eq!(
            std::fs::read(&at).unwrap(),
            b"{\"rea",
            "{file}: bytes untouched"
        );
        let other = if file == "failure" {
            "outcome"
        } else {
            "failure"
        };
        assert_eq!(
            (g.file(1, other), g.file(1, "ready")),
            (None, None),
            "{file}"
        );
        std::fs::remove_file(&at).unwrap();
        let whole = if file == "failure" {
            json!({"reason": NO_REPORT, "evidence": {}, "at": "x"})
        } else {
            json!({"outcome": "failed", "reason": NO_REPORT, "at": "x"})
        };
        g.put(1, file, whole);
        assert_eq!(
            g.judge(1, None),
            decided(Verdict::Failed(NO_REPORT.into())),
            "{file}"
        );
    }
}

// R2, R5 and the OS matrix: a folder at outcome.json or failure.json (every OS), gens/1 a link (testing::link_dir:
// Unix symlink; Windows directory symlink or junction) to a folder holding a whole failure.json, and a Unix FIFO at
// failure.json: `evidence`, nothing written anywhere, nothing opened that is not a regular file.
#[test]
fn wrong_file_types_refuse_and_write_nothing() {
    for file in ["outcome", "failure"] {
        let g = of::world();
        g.ran(1);
        g.now(400);
        std::fs::create_dir(gens::path(&g.folder, 1, file)).unwrap();
        assert_eq!(
            g.judge(1, None).map_err(|e| e.check),
            Err("evidence"),
            "a folder at {file}.json"
        );
        let other = if file == "outcome" {
            "failure"
        } else {
            "outcome"
        };
        assert_eq!(g.file(1, other), None, "{file}");
    }
    let g = of::world();
    g.ran(1);
    g.now(400);
    let outside = crate::testing::TempDir::new();
    let target = outside.path().join("1");
    std::fs::rename(gens::dir(&g.folder, 1), &target).unwrap();
    std::fs::write(
        target.join("failure.json"),
        br#"{"reason":"no readiness report","evidence":{},"at":"x"}"#,
    )
    .unwrap();
    crate::testing::link_dir(&target, &gens::dir(&g.folder, 1));
    assert_eq!(
        g.judge(1, None).map_err(|e| e.check),
        Err("evidence"),
        "linked gens/1"
    );
    assert!(
        !target.join("outcome.json").exists(),
        "nothing written through the link"
    );
    #[cfg(unix)]
    {
        let g = of::world();
        g.ran(1);
        g.now(400);
        let fifo = gens::path(&g.folder, 1, "failure");
        let mkfifo = std::path::Path::new("/usr/bin/mkfifo");
        let made = crate::spawn::captured(
            mkfifo,
            &[fifo.to_str().unwrap()],
            std::time::Duration::from_secs(10),
        );
        assert_eq!(made.map(|o| o.code).ok(), Some(0), "mkfifo");
        assert_eq!(
            g.judge(1, None).map_err(|e| e.check),
            Err("evidence"),
            "a FIFO at failure.json"
        );
        assert_eq!(g.file(1, "outcome"), None);
    }
}

// Ambiguity 10: a failure.json that lands between promote_held's read and the create (a writer outside the lock) makes
// the create find `Exists`: decide nothing, no outcome.json; the next call decides from the file that landed.
#[test]
fn a_file_landing_outside_the_lock_decides_nothing_on_that_call() {
    let g = of::world();
    g.ran(1);
    exited(&g, "exited");
    g.now(10);
    let at = gens::path(&g.folder, 1, "failure");
    let body = json!({"reason": NO_REPORT, "evidence": {}, "at": "x"});
    let once = std::rc::Rc::new(std::cell::Cell::new(false));
    let (flag, file, b) = (once.clone(), at.clone(), body.clone());
    // The first clock read under the lock is `decide`'s `now`, after promote_held and before the create.
    g.w.fakes.clock_probe(move |_| {
        if !flag.replace(true) {
            crate::state::fs::write_json(&file, &b).unwrap();
        }
    });
    assert_eq!(g.judge(1, None).map_err(|e| e.check), Err("evidence"));
    assert!(once.get(), "the file landed");
    assert_eq!(g.file(1, "outcome"), None);
    assert_eq!(
        g.judge(1, None),
        decided(Verdict::Failed(NO_REPORT.into())),
        "from the file that landed"
    );
}

/// A second party on the same state root and the same real per-user locks, its own fakes, at T0 + `secs`.
fn actor(
    root: &std::path::Path,
    locks: &std::path::Path,
    secs: u64,
) -> (crate::testing::Fakes, crate::ctx::Ctx) {
    let f = crate::testing::Fakes::default();
    f.process(crate::testing::row(100, 1, 1000));
    f.process(crate::testing::row(150, 100, 1500));
    f.real_locks(locks);
    let mut ctx = f.ctx();
    ctx.state_root = root.to_path_buf();
    let at = crate::util::parse_utc(of::T0).unwrap() + std::time::Duration::from_secs(secs);
    f.advance(at.duration_since(ctx.clock.now()).unwrap());
    (f, ctx)
}

/// report-2b K4.1's stand-in: under the gen lock, `report.json` only when neither `outcome.json` nor `failure.json`
/// exists, then K5 under the same hold.
fn report_under_lock(ctx: &crate::ctx::Ctx, key: &str) -> Res<gens::Ready> {
    let member = ctx.state_root.join(key).join("agents").join(ID);
    crate::agents::lock(ctx, &gen_lock_key(key, ID, 1), || {
        let decided = gens::read(&member, 1, "outcome").known()?.is_some()
            || gens::read(&member, 1, "failure").known()?.is_some();
        if !decided {
            let r = json!({"sessionId": of::SID, "pid": 150, "start": 1500, "at": "x"});
            gens::publish_once(&member, 1, "report", &r)?;
        }
        gens::promote_held(ctx, &member, 1).map(|h| h.ready)
    })
}

// Spec races on real per-user locks, every OS (Unix `flock`, Windows named mutex) under a 0700 lock root (o2 call 18:
// `testing::private_tree`). Report at 149 s, adjudicator at 151 s, both orders. Build checklist 11: contention is
// proven from inside the lock: the first holder keeps the gen lock until the real lock's own wait, on the second
// party's thread, finds it held (`state::lock::hooks::when_held`, which fires only from inside that wait), and records
// that it saw it before it releases. The outcome follows the lock order, never ready and failed both.
#[test]
fn the_lock_order_decides_the_outcome_never_both() {
    use crate::state::lock::hooks::when_held;
    use std::sync::{Arc, Mutex, mpsc};
    // Checklist 11: the hook is proof only because it fires inside the wait on a held lock: a free gen lock fires none.
    {
        let g = of::world();
        let locks = crate::testing::TempDir::new();
        let lp = locks.path().join("l");
        crate::testing::private_tree(locks.path(), &lp);
        g.w.fakes.real_locks(&lp);
        let fired = std::rc::Rc::new(std::cell::Cell::new(false));
        let f = fired.clone();
        when_held(Some(Box::new(move || f.set(true))));
        let got = g.judge(1, None);
        when_held(None);
        assert_eq!(
            (got, fired.get()),
            (Ok(Adjudged::Undecided), false),
            "a free gen lock fires no hook"
        );
    }
    for case in ["report first", "report first, client", "adjudicator first"] {
        let g = of::world();
        g.ran(1);
        if case.ends_with("client") {
            g.client(1);
        }
        let locks = crate::testing::TempDir::new();
        let lp = locks.path().join("l");
        crate::testing::private_tree(locks.path(), &lp);
        g.w.fakes.real_locks(&lp);
        g.now(151);
        let (root, key) = (g.w.ctx.state_root.clone(), g.key.clone());
        let (held_tx, held_rx) = mpsc::channel::<()>();
        let (tried_tx, tried_rx) = mpsc::channel::<()>();
        let contended = Arc::new(Mutex::new(false));
        let (seen, tried_rx) = (contended.clone(), Mutex::new(tried_rx));
        // The first holder: inside the gen lock, let the second party go, then hold until the second party's wait has
        // found the lock held (10 s at most), and record whether it did, before releasing.
        let holder = move |event: &str| {
            if event.starts_with("+gen|") {
                held_tx.send(()).unwrap();
                let tried = tried_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(10));
                *seen.lock().unwrap() = tried.is_ok();
            }
            String::new()
        };
        // The second party's thread: the real lock fires this from inside its wait on the held gen lock.
        let trying = move || {
            let _ = tried_tx.send(());
        };
        let (reported, judged) = std::thread::scope(|s| {
            if case == "adjudicator first" {
                g.w.fakes.lock_probe(holder);
                let racer = s.spawn(move || {
                    held_rx.recv().unwrap();
                    let (_f, ctx) = actor(&root, &lp, 149);
                    when_held(Some(Box::new(trying)));
                    let got = report_under_lock(&ctx, &key);
                    when_held(None);
                    got
                });
                let judged = g.judge(1, None);
                (racer.join().unwrap(), judged)
            } else {
                let racer = s.spawn(move || {
                    let (f, ctx) = actor(&root, &lp, 149);
                    f.lock_probe(holder);
                    report_under_lock(&ctx, &key)
                });
                held_rx.recv().unwrap();
                when_held(Some(Box::new(trying)));
                let judged = g.judge(1, None);
                when_held(None);
                (racer.join().unwrap(), judged)
            }
        });
        let both = g.file(1, "ready").is_some() && g.file(1, "failure").is_some();
        assert!(!both, "{case}: ready and failed both");
        match case {
            "report first" => {
                assert_eq!(
                    (reported, judged),
                    (Ok(gens::Ready::Undecided), Ok(Adjudged::Undecided)),
                    "{case}"
                );
                assert_eq!(g.file(1, "failure"), None, "{case}: never failed");
            }
            "report first, client" => {
                assert_eq!(
                    (reported, judged),
                    (Ok(gens::Ready::Ok), decided(Verdict::Ok)),
                    "{case}"
                );
            }
            _ => {
                assert_eq!(judged, decided(Verdict::Failed(NO_REPORT.into())), "{case}");
                assert!(
                    matches!(reported, Ok(gens::Ready::Outcome(_))),
                    "{case}: {reported:?}"
                );
                assert_eq!(
                    g.file(1, "report"),
                    None,
                    "{case}: the report created nothing"
                );
            }
        }
        assert!(
            *contended.lock().unwrap(),
            "{case}: the second party never waited on the held gen lock"
        );
    }
}

// R5, finding 1to4-I6: a link at failure.json or outcome.json itself (testing::link_dir: Unix symlink; Windows directory
// symlink, else junction), to a folder or dangling: `evidence`, never followed, the target untouched, nothing new.
#[test]
fn a_link_at_an_evidence_name_refuses_and_is_never_followed() {
    for file in ["failure", "outcome"] {
        for dangling in [false, true] {
            let g = of::world();
            g.ran(1);
            g.now(400);
            let outside = crate::testing::TempDir::new();
            let target = outside.path().join("t");
            if !dangling {
                std::fs::create_dir(&target).unwrap();
            }
            crate::testing::link_dir(&target, &gens::path(&g.folder, 1, file));
            let case = format!("{file}, dangling {dangling}");
            assert_eq!(
                g.judge(1, None).map_err(|e| e.check),
                Err("evidence"),
                "{case}"
            );
            assert_eq!(target.exists(), !dangling, "{case}: the target untouched");
            if !dangling {
                assert!(
                    std::fs::read_dir(&target).unwrap().next().is_none(),
                    "{case}: nothing written through it"
                );
            }
            let other = if file == "failure" {
                "outcome"
            } else {
                "failure"
            };
            assert_eq!(
                (g.file(1, other), g.file(1, "ready")),
                (None, None),
                "{case}"
            );
        }
    }
}

fn rec(n: &str) -> serde_json::Value {
    json!({"lifecycle": 1, "id": ID, "generation": n})
}

// Spec "newest wins": generation 1 ok, generation 2 promoted and then failed: the member needs recovery (the active
// generation decides; 1's success does not mask it). `running` follows the caller's `active_done` (R1, R3).
#[test]
fn newest_wins_needs_recovery() {
    let g = of::world();
    g.ran(1);
    g.report(1, 150);
    g.client(1);
    g.now(20);
    assert_eq!(g.judge(1, None), decided(Verdict::Ok));
    g.launched(2);
    g.ran(2);
    g.put(2, "exit", exit_body("exited"));
    g.now(30);
    let failed = |running| {
        Ok(Some(Recovery {
            generation: 2,
            reason: EXITED.into(),
            running,
        }))
    };
    assert_eq!(
        needs_recovery(&g.w.ctx, &g.key, ID, &rec("2"), Some(false), None),
        failed(true)
    );
    assert_eq!(
        needs_recovery(&g.w.ctx, &g.key, ID, &rec("2"), Some(true), None),
        failed(false)
    );
    assert_eq!(
        needs_recovery(&g.w.ctx, &g.key, ID, &rec("1"), Some(true), None),
        Ok(None),
        "an ok generation"
    );
}

// O6: a pre-lifecycle record, or a lifecycle record with no active generation: None, no lock taken, nothing read.
#[test]
fn needs_recovery_reads_nothing_without_an_active_lifecycle_generation() {
    let g = of::world();
    g.ran(1);
    g.put(1, "exit", exit_body("exited"));
    g.w.fakes.clear_log();
    let pending = json!({"lifecycle": 1, "id": ID, "pending": {"gen": 1}});
    for record in [json!({"id": ID, "generation": "1"}), pending] {
        let (got, reads) = crate::state::fs::record_reads(|| {
            needs_recovery(&g.w.ctx, &g.key, ID, &record, None, None)
        });
        assert_eq!((got, reads), (Ok(None), Vec::new()), "{record}");
    }
    assert!(g.w.fakes.locks().is_empty());
}

// Ambiguity 13: the board's try-lock finds the gen lock busy: the last outcome.json, read once, decides; absent or ok
// is no recovery; torn refuses `evidence` (R2, never "no recovery"). Nothing is adjudicated while busy.
#[test]
fn a_busy_lock_shows_the_last_outcome() {
    for (case, body) in [
        ("none", None),
        ("ok", Some(json!({"outcome": "ok", "at": "x"}))),
        (
            "failed",
            Some(json!({"outcome": "failed", "reason": NO_REPORT, "at": "x"})),
        ),
        ("torn", None),
    ] {
        let g = of::world();
        g.ran(1);
        g.put(1, "exit", exit_body("exited"));
        if let Some(b) = body {
            g.put(1, "outcome", b);
        }
        if case == "torn" {
            std::fs::write(gens::path(&g.folder, 1, "outcome"), b"{\"out").unwrap();
        }
        g.w.fakes.lock_timeout("gen|");
        g.now(30);
        let at = gens::path(&g.folder, 1, "outcome");
        let before = std::fs::read(&at).ok();
        let (got, reads) = crate::state::fs::record_reads(|| {
            needs_recovery(&g.w.ctx, &g.key, ID, &rec("1"), Some(true), None).map_err(|e| e.check)
        });
        let want = match case {
            "failed" => Ok(Some(Recovery {
                generation: 1,
                reason: NO_REPORT.into(),
                running: false,
            })),
            "torn" => Err("evidence"),
            _ => Ok(None),
        };
        assert_eq!(got, want, "{case}");
        // Finding 5to8-15: outcome.json opened exactly once when present (never when absent), and unchanged.
        let opened = reads.iter().filter(|p| **p == at).count();
        assert_eq!(
            opened,
            usize::from(case != "none"),
            "{case}: outcome.json reads {reads:?}"
        );
        assert_eq!(
            std::fs::read(&at).ok(),
            before,
            "{case}: outcome.json unchanged"
        );
        assert_eq!(
            g.file(1, "failure"),
            None,
            "{case}: nothing adjudicated while busy"
        );
    }
}

// O6, finding 5to8-16: only `failed` needs recovery. An active `void` generation (claim abandoned) is adjudicated
// void and needs none; an active undecided one (committed run, no leg fires yet) needs none and writes nothing.
#[test]
fn an_active_void_or_undecided_generation_needs_no_recovery() {
    let g = of::world();
    g.claim(1, json!("abandoned"));
    g.now(30);
    assert_eq!(
        needs_recovery(&g.w.ctx, &g.key, ID, &rec("1"), Some(true), None),
        Ok(None),
        "void"
    );
    let outcome = g
        .file(1, "outcome")
        .map(|o| (o["outcome"].clone(), o["reason"].clone()));
    assert_eq!(
        outcome,
        Some((json!("void"), json!(ABANDONED))),
        "adjudicated void, not skipped"
    );
    let g = of::world();
    g.ran(1);
    g.now(30);
    assert_eq!(
        needs_recovery(&g.w.ctx, &g.key, ID, &rec("1"), Some(false), None),
        Ok(None),
        "undecided"
    );
    assert_eq!(
        (g.file(1, "failure"), g.file(1, "outcome")),
        (None, None),
        "undecided writes nothing"
    );
}

// o2 call 27 (QA F1): with the gen lock busy, an outcome.json or failure.json that is visible but not yet flushed
// decides nothing (the flush is retried and refuses `state`), so the board never shows a failure a reboot can take
// back; once the names are gone and the client is published, the generation is ok. A flushed failure.json alone, the
// lock busy, still shows its recovery (authoritative, as in K5).
#[test]
fn a_busy_lock_never_shows_an_unflushed_outcome() {
    let g = of::world();
    g.ran(1);
    g.report(1, 150);
    g.now(210);
    crate::state::fs::fail_sync_dir(Some(&gens::dir(&g.folder, 1)));
    assert_eq!(
        judged(&g, None),
        unflushed(),
        "failure.json linked, unflushed"
    );
    assert_eq!(
        judged(&g, None),
        unflushed(),
        "outcome.json linked, unflushed"
    );
    g.w.fakes.lock_timeout("gen|");
    let busy = needs_recovery(&g.w.ctx, &g.key, ID, &rec("1"), Some(false), None)
        .map_err(|e| (e.check, e.message));
    crate::state::fs::fail_sync_dir(None);
    assert_eq!(
        busy,
        Err(("state", gens::NOT_FLUSHED.to_string())),
        "an unflushed outcome decides nothing"
    );
    // A reboot loses both unflushed names; the launcher then publishes the matching client.
    for file in ["failure", "outcome"] {
        std::fs::remove_file(gens::path(&g.folder, 1, file)).unwrap();
    }
    g.client(1);
    // The lock is still busy for this world's fakes, so K5 itself (no lock) shows what the next holder decides.
    let held = gens::promote_held(&g.w.ctx, &g.folder, 1).map(|h| h.ready);
    assert_eq!(held, Ok(gens::Ready::Ok), "the generation was never failed");
    let g = of::world();
    g.ran(1);
    g.put(
        1,
        "failure",
        json!({"reason": NO_REPORT, "evidence": {}, "at": "x"}),
    );
    g.w.fakes.lock_timeout("gen|");
    g.now(30);
    let got = needs_recovery(&g.w.ctx, &g.key, ID, &rec("1"), Some(true), None);
    let want = Recovery {
        generation: 1,
        reason: NO_REPORT.into(),
        running: false,
    };
    assert_eq!(got, Ok(Some(want)), "a flushed failure.json, lock busy");
    assert_eq!(g.file(1, "outcome"), None, "nothing written while busy");
    // failure.json alone, linked but unflushed (outcome.json never made), the lock busy: decides nothing either.
    let g = of::world();
    g.ran(1);
    exited(&g, "exited");
    g.now(10);
    crate::state::fs::fail_sync_dir(Some(&gens::dir(&g.folder, 1)));
    assert_eq!(
        judged(&g, None),
        unflushed(),
        "failure.json linked, unflushed"
    );
    g.w.fakes.lock_timeout("gen|");
    let busy = needs_recovery(&g.w.ctx, &g.key, ID, &rec("1"), Some(false), None)
        .map_err(|e| (e.check, e.message));
    crate::state::fs::fail_sync_dir(None);
    assert_eq!(
        busy,
        Err(("state", gens::NOT_FLUSHED.to_string())),
        "an unflushed failure.json decides nothing"
    );
}

// a3c call 47 (TL, production readiness): an empty or malformed launch.json hcName never matches a terminal
// carrying the same name, even with this member's labels and a recovery shell's argv. The adjudication refuses
// `evidence`: no Lost, no failure.json, no outcome.json.
#[test]
fn an_invalid_launch_name_is_never_lost() {
    for name in ["", "../x"] {
        let g = of::world();
        g.ran(1);
        let launch = json!({"hcName": name, "launcherArgv": of::LAUNCHER, "sessionId": of::SID});
        std::fs::write(gens::path(&g.folder, 1, "launch"), launch.to_string()).unwrap();
        g.now(10);
        let lost = of::terminal(name, ID, 1, LOST_ARGVS[0]);
        assert_eq!(
            g.judge(1, Some(&lost)).map_err(|e| e.check),
            Err("evidence"),
            "{name:?}"
        );
        assert_eq!(
            (g.file(1, "failure"), g.file(1, "outcome")),
            (None, None),
            "{name:?}: written"
        );
    }
}
