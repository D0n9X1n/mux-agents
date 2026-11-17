//! outcome-2 Task 1 (; o2 calls 15-17): what adjudication reads from report-2b's `Held`, under its own hold.
use super::*;
use crate::agents::fixture::ID;
use crate::agents::gens;
use crate::agents::outcome_fixture as of;
use crate::agents::team_state::gen_lock_key;
use serde_json::json;

/// `promote_held` inside one hold of generation 1's gen lock, as `adjudicate` calls it.
fn held(g: &of::G) -> Res<Held> {
    crate::agents::lock(&g.w.ctx, &gen_lock_key(&g.key, ID, 1), || {
        promote_held(&g.w.ctx, &g.folder, 1)
    })
}

// Calls 16, R1: on the nonmatching-report path K5 reads exit.json once and hands it back with the report; a proven
// pair then promotes. Never a nested take (the fake lock fails one on every OS).
#[test]
fn promote_held_hands_back_the_report_and_the_exit_it_read() {
    let g = of::world();
    g.ran(1);
    g.client(1);
    g.w.fakes.process(crate::testing::row(777, 1, 1500));
    g.report(1, 777);
    g.put(
        1,
        "exit",
        json!({"exitCode": null, "reason": "client-conflict", "exitedAt": "x"}),
    );
    let exit = gens::path(&g.folder, 1, "exit");
    let (got, reads) = crate::state::fs::record_reads(|| held(&g));
    let got = got.unwrap();
    assert_eq!(got.ready, Ready::Undecided, "another process's report");
    assert_eq!(
        got.report.map(|r| r["pid"].clone()),
        Some(json!(777)),
        "the report it read"
    );
    assert_eq!(
        got.exit.map(|e| e["reason"].clone()),
        Some(json!("client-conflict")),
        "the exit it read"
    );
    assert_eq!(
        reads.iter().filter(|p| **p == exit).count(),
        1,
        "exit.json read once: {reads:?}"
    );
    let g = of::world();
    g.ran(1);
    g.client(1);
    g.report(1, 150);
    assert_eq!(held(&g).unwrap().ready, Ready::Ok);
    assert!(g.file(1, "ready").is_some());
    let g = of::world();
    g.put(
        1,
        "failure",
        json!({"reason": "no readiness report", "evidence": {}, "at": "x"}),
    );
    let got = held(&g).unwrap();
    assert_eq!(
        (got.ready, got.failure.map(|f| f["reason"].clone())),
        (Ready::Failed, Some(json!("no readiness report")))
    );
}

// Call 15: a torn client.json, launcher.json or exit.json on the report path refuses `evidence`: nothing decided,
// no ready.json. Never `Undecided` (which would let a deadline leg fail the generation on unread evidence).
#[test]
fn an_unreadable_client_proof_refuses_and_writes_nothing() {
    for torn in ["client", "launcher", "exit"] {
        let g = of::world();
        g.ran(1);
        g.client(1);
        g.report(1, 150);
        let at = gens::path(&g.folder, 1, torn);
        let _ = std::fs::remove_file(&at);
        std::fs::write(&at, b"{\"pi").unwrap();
        assert_eq!(
            held(&g).map(|h| h.ready).map_err(|e| e.check),
            Err("evidence"),
            "torn {torn}.json"
        );
        assert_eq!(g.file(1, "ready"), None, "{torn}");
    }
}
