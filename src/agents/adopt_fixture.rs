//! adopt-3e fixtures over cascade's (`cx`) and group B's (`lf`). Each helper arrives with its first user.
use serde_json::{Value, json};

use super::adopt::{Change, Journal};
use super::cascade_fixture::{self as cx, A, B, L};
use super::fixture::World;
use super::life_fixture as lf;
use super::team_state::Scope;

pub const E: &str = "eeeeeeeeeee1";

/// The journal that moves B from E under A at depth 2.
pub fn to_a(seq: u64) -> Journal {
    let parent_agent =
        json!({"client": "claude", "pid": null, "start": null, "sessionId": lf::SID, "id": A});
    Journal {
        seq,
        from: E.into(),
        to: A.into(),
        changes: vec![Change {
            id: B.into(),
            parent_agent,
            parent_pane: Value::Null,
            depth: 2,
        }],
    }
}

/// Generation 1 of `id` Ended, as group B's end writer publishes it.
pub fn ended(w: &World, id: &str) {
    let facts = json!({"address": cx::addr(id), "hcName": lf::hc_name(1), "labels": {}, "state": "finished",
        "reason": "exited", "exitCode": 0, "at": "2026-11-15T00:00:00.0000000Z"});
    crate::state::evidence::publish(&super::gens::path(&w.folder(id), 1, "ended"), &facts).unwrap();
}
/// L (the leader: depth 0, `upTeam`) -> A -> E -> B, all live.
pub fn team_live(w: &World) {
    cx::member(w, L, None, 0, json!({"upTeam": "t", "client": "claude"}));
    cx::member(w, A, Some(L), 1, json!({"client": "claude"}));
    cx::member(w, E, Some(A), 2, json!({"client": "claude"}));
    cx::member(w, B, Some(E), 3, json!({"client": "claude"}));
}
/// L -> A -> E -> B; E Ended.
pub fn team(w: &World) {
    team_live(w);
    ended(w, E);
}
pub fn record(w: &World, id: &str) -> Value {
    lf::record(&w.folder(id))
}
pub fn parent_of(w: &World, id: &str) -> Option<String> {
    record(w, id)["parentAgent"]["id"]
        .as_str()
        .map(String::from)
}
pub fn depth_of(w: &World, id: &str) -> u64 {
    record(w, id)["depth"].as_u64().unwrap()
}
pub fn seq(w: &World, s: &Scope) -> u64 {
    super::adopt::committed(&w.ctx, s).unwrap()
}
/// Writes `<seq>.json` as `publish` would, its folders made.
pub fn put_journal(w: &World, s: &Scope, j: &Journal) {
    let dir = super::adopt::dir_at(&w.ctx, s);
    std::fs::create_dir_all(&dir).unwrap();
    crate::state::fs::write_json(&dir.join(format!("{}.json", j.seq)), &j.body()).unwrap();
}
