//! restore-6b Task 3: `run` settles, finishes and sweeps before any member is classified; rows 1-3, 5, 6; a
//! pre-lifecycle folder is never touched (6b-R2).
use super::core_fixture as cf;
use super::fixture::World;
use super::life_fixture as lf;
use super::reconcile::run;
use super::reconcile_tests::{X, member, wire_of};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Every file below `dir`, path to bytes (6b-R2: a byte-identical folder).
pub(super) fn bytes(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut all = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            all.extend(bytes(&p));
        } else {
            all.insert(p.clone(), std::fs::read(&p).unwrap());
        }
    }
    all
}
/// One reconcile pass at the caller scope (its own anchor).
pub(super) fn pass(w: &World) -> serde_json::Map<String, Value> {
    run(&w.ctx, &w.caller()).unwrap()
}
pub(super) fn ids(reply: &serde_json::Map<String, Value>, key: &str) -> Vec<String> {
    reply[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect()
}
/// A pre-lifecycle folder (no `lifecycle: 1`) holding generation evidence, as prelife-1c G10 describes it.
fn prelife(w: &World, id: &str) -> PathBuf {
    let f = w.folder(id);
    std::fs::create_dir_all(&f).unwrap();
    let record = json!({"id": id, "name": "old", "session": w.caller().session, "hcAddress": "t9"});
    crate::state::fs::write_json(&f.join("record.json"), &record).unwrap();
    lf::seed_gen(&f, 1, Some("run"), lf::committed());
    f
}
fn publish(f: &Path, n: u64, file: &str, body: Value) {
    crate::state::evidence::publish(&super::gens::path(f, n, file), &body).unwrap();
}

#[test]
fn quiet_rows_answer_without_hc_and_a_prelife_folder_is_untouched() {
    let w = World::human("");
    w.state();
    let stopped = member(&w, "cccccccccccc", json!({}));
    let desired = json!({"desired": "stopped", "at": "t"});
    crate::state::fs::write_json(&stopped.join("desired.json"), &desired).unwrap();
    let rec = member(&w, "dddddddddddd", json!({}));
    let body = json!({"address": "add", "hcName": lf::hc_name(1), "cause": "recovery", "at": "t"});
    publish(&rec, 1, "release", body);
    publish(&rec, 1, "released", json!({"at": "t"}));
    let ended = member(&w, "eeeeeeeeeeee", json!({}));
    publish(&ended, 1, "ended", json!({"at": "t"}));
    let torn = w.folder("ffffffffffff");
    std::fs::create_dir_all(&torn).unwrap();
    std::fs::write(torn.join("record.json"), b"{\"lifecycle\": 1, \"id\"").unwrap();
    let pre = prelife(&w, "999999999999");
    let before = bytes(&pre);
    w.fakes.hc_install("hc"); // installed, nothing scripted: a stray call is logged (finding 12)
    let reply = pass(&w);
    assert_eq!(ids(&reply, "recover"), ["dddddddddddd"]);
    assert_eq!(reply["recover"][0]["reason"], "recovery");
    assert_eq!(ids(&reply, "unknown"), ["ffffffffffff"]);
    assert_eq!(reply["unknown"][0]["check"], "evidence");
    // shortcut: adopt-3e's `adopt_locked` is not merged yet; its `adoptError` assertion lands with it (r6b call 17).
    assert!(ids(&reply, "restored").is_empty() && ids(&reply, "held").is_empty());
    assert!(lf::calls(&w).is_empty(), "{:?}", lf::calls(&w));
    assert_eq!(
        bytes(&pre),
        before,
        "a pre-lifecycle folder is never written (p1c call 6)"
    );
}

/// release-1d R1 replay before classifying: `release.json` without `released.json` is finished from step 3 first.
#[test]
fn an_interrupted_release_is_finished_before_the_member_is_classified() {
    for cause in ["lost", "recovery"] {
        let w = World::human("");
        w.state();
        let f = member(&w, X, json!({}));
        let addr = format!("a{X}");
        let body = json!({"address": addr, "hcName": lf::hc_name(1), "cause": cause, "at": "t"});
        publish(&f, 1, "release", body);
        cf::online(&w);
        let shell = wire_of(&w, X, 1, lf::LOST_ARGVS[0]).to_string();
        cf::answer(&w, &["get", &addr, "--json"], &shell);
        lf::relabel(&w, &addr);
        let reply = pass(&w);
        let released = super::gens::read(&f, 1, "released").known().unwrap();
        assert!(released.is_some(), "{cause}");
        let want: Vec<String> = if cause == "recovery" {
            vec![X.into()]
        } else {
            vec![]
        };
        assert_eq!(ids(&reply, "recover"), want, "{cause}");
        assert_eq!(lf::count(&w, "label"), 1, "{cause}");
        lf::no_effect(&w);
        w.fakes.hc_done().unwrap();
    }
}

/// settle-1b G3 row 3 gap: `abandoned` published, `pending` not yet dropped. Two passes: dropped once, nothing runs.
#[test]
fn the_abandonment_gap_clears_pending_once_and_launches_nothing() {
    let w = World::human("");
    w.state();
    let pending = json!({"gen": 2, "kind": "restore", "hcName": lf::hc_name(2), "sessionId": lf::SID,
        "launcherArgv": lf::ARGV});
    let f = member(&w, X, json!({"pending": pending}));
    publish(&f, 1, "ended", json!({"at": "t"}));
    crate::state::evidence::publish(&super::gens::claim_path(&f, 2), &json!("abandoned")).unwrap();
    for _ in 0..2 {
        let reply = pass(&w);
        assert!(
            ids(&reply, "restored").is_empty() && ids(&reply, "unknown").is_empty(),
            "{reply:?}"
        );
    }
    assert!(lf::record(&f).get("pending").is_none());
    assert_eq!(
        super::gens::claim(&f, 2).known().unwrap(),
        Some(json!("abandoned"))
    );
    assert_eq!(lf::count(&w, "run"), 0);
}

/// report-2b K2 via L2: reconcile sweeps the registry; a dead client entry goes, a live one stays (real locks).
#[test]
fn reconcile_sweeps_dead_registry_entries_only() {
    use crate::parse::Client;
    use crate::state::live;
    let w = World::human("");
    w.state();
    crate::testing::private_tree(&w.ctx.state_root, &w.ctx.state_root);
    w.fakes.real_locks(&w.ctx.state_root);
    for pid in [7100, 7200] {
        w.fakes.process(crate::testing::row(pid, 1, 1500));
        live::report(&w.ctx, Client::Claude, pid, 1500, lf::SID, "startup").unwrap();
    }
    w.fakes.forget(7100);
    let reply = pass(&w);
    assert!(reply.get("sweepError").is_none(), "{reply:?}");
    assert!(!live::path(&w.ctx, Client::Claude, 7100, 1500).exists());
    assert!(live::path(&w.ctx, Client::Claude, 7200, 1500).is_file());
}

/// ambiguity 11: a failed sweep is reported and reconcile goes on.
#[test]
fn a_failed_sweep_is_reported_and_reconcile_goes_on() {
    let w = World::human("");
    w.state();
    crate::testing::private_tree(&w.ctx.state_root, &w.ctx.state_root);
    std::fs::write(crate::state::live::dir(&w.ctx), b"not a folder").unwrap();
    let reply = pass(&w);
    assert_eq!(reply["sweepError"], "evidence");
    assert!(reply["restored"].as_array().unwrap().is_empty());
}

/// r6b call 21 (finding 14): a crash in the preamble's release replay stops the pass with `halted`.
#[test]
fn a_halt_in_the_preamble_stops_the_pass() {
    let w = World::human("");
    w.state();
    let f = member(&w, X, json!({}));
    let addr = format!("a{X}");
    let body = json!({"address": addr, "hcName": lf::hc_name(1), "cause": "lost", "at": "t"});
    publish(&f, 1, "release", body);
    cf::online(&w);
    let shell = wire_of(&w, X, 1, lf::LOST_ARGVS[0]).to_string();
    cf::answer(&w, &["get", &addr, "--json"], &shell);
    lf::relabel(&w, &addr);
    super::release::halt_after(Some("relabel"));
    let got = run(&w.ctx, &w.caller());
    super::release::halt_after(None);
    assert_eq!(got.map(|_| ()).unwrap_err().check, "halted");
    assert!(
        super::gens::read(&f, 1, "released")
            .known()
            .unwrap()
            .is_none()
    );
    w.fakes.hc_done().unwrap();
}

/// r6b call 31 (checklist 29): another session's records at the same server key are not this team: never observed,
/// never asked of hc, never in the reply.
#[test]
fn another_session_is_skipped() {
    let w = World::human("");
    w.state();
    let f = member(&w, X, json!({"session": "$99"}));
    // a half release: a preamble that walked this member would finish it through hc (logged, refused)
    let body =
        json!({"address": format!("a{X}"), "hcName": lf::hc_name(1), "cause": "lost", "at": "t"});
    publish(&f, 1, "release", body);
    w.fakes.hc_install("hc"); // installed, nothing scripted: a stray call is logged
    let reply = pass(&w);
    for key in ["restored", "held", "unknown", "recover"] {
        assert!(ids(&reply, key).is_empty(), "{key}: {reply:?}");
    }
    assert!(lf::calls(&w).is_empty(), "{:?}", lf::calls(&w));
}
