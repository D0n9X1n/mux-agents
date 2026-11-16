//! restore-6b Task 6 (r6b call 6): `existing`, the one team discovery of `restore` and (Task 7) `up`.
use super::discover::existing;
use crate::agents::{
    core_fixture as cf, fixture::World, gens, life_fixture as lf, team_state::Scope,
};
use serde_json::json;

const LEAD: &str = "cccccccccccc";

/// `existing` uses group B's full check (`never_ran_recorded`), never the partial `Status::never_ran`: generation 0
/// committed ran; a leader naming no generation and holding none never ran; a committed generation fenced and
/// released (no `generation`, no `pending`) ran; torn unnamed evidence refuses. Checklist 13: the test removes only
/// the `gens/` it created.
#[test]
fn existing_uses_the_full_never_ran_check() {
    let w = World::human("");
    w.state();
    let f = w.folder(LEAD);
    std::fs::create_dir_all(&f).unwrap();
    let never = json!({"lifecycle": 1, "id": LEAD, "name": "leader", "upTeam": "T", "depth": 0, "session": "$3"});
    let mut ran = never.clone();
    (ran["generation"], ran["hcName"], ran["launcherArgv"]) =
        (json!("0"), json!(lf::hc_name(0)), json!(lf::ARGV));
    crate::state::fs::write_json(&f.join("record.json"), &ran).unwrap();
    lf::seed_gen(&f, 0, Some("run"), lf::committed());
    cf::online(&w);
    let core = crate::hc::connect(&w.ctx).unwrap();
    let key = w.caller().server_key;
    let anchor = Some(Scope {
        key: key.clone(),
        session: "$3".into(),
    });
    assert_eq!(existing(&w.ctx, &core, &key, "T").unwrap(), anchor, "ran");
    std::fs::remove_dir_all(f.join("gens")).unwrap(); // the gens/ seed_gen created
    crate::state::fs::write_json(&f.join("record.json"), &never).unwrap();
    assert_eq!(
        existing(&w.ctx, &core, &key, "T").unwrap(),
        None,
        "never ran"
    );
    let publish = |file: &str, body: serde_json::Value| {
        crate::state::evidence::publish(&gens::path(&f, 0, file), &body).unwrap();
    };
    publish("commit", json!({"committed": true}));
    publish("released", json!({"at": "t"}));
    let fenced = existing(&w.ctx, &core, &key, "T").unwrap();
    assert_eq!(fenced, anchor, "a fenced committed generation ran");
    let commit = gens::path(&f, 0, "commit");
    std::fs::remove_file(&commit).unwrap();
    std::fs::create_dir_all(&commit).unwrap();
    let torn = existing(&w.ctx, &core, &key, "T").map_err(|e| e.check);
    assert_eq!(torn, Err("evidence"), "torn evidence refuses");
    let calls = w.fakes.hc_calls();
    assert!(
        calls.iter().all(|c| c.1[0] == "--version"),
        "a lifecycle leader asks hc nothing: {calls:?}"
    );
}
