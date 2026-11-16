//! outcome-2 (#103 O2, O4): one outcome per generation, decided under the gen lock.
use super::*;
use crate::agents::fixture::ID;
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
