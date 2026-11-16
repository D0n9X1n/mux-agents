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

pub const E2: &str = "eeeeeeeeeee2";
pub const A2: &str = "aaaaaaaaaaa2";
/// Sets one record field directly; `null` removes it (a fixture, as A3a's tests seed records).
pub fn set(w: &World, id: &str, key: &str, value: Value) {
    let mut r = record(w, id);
    match value {
        Value::Null => drop(r.as_object_mut().unwrap().remove(key)),
        v => r[key] = v,
    }
    crate::state::fs::write_json(&w.folder(id).join("record.json"), &r).unwrap();
}
/// Replaces `id`'s record with a pre-lifecycle one (prelife-1c G10); its bytes.
pub fn pre(w: &World, id: &str, parent: Option<&str>, depth: u64) -> Vec<u8> {
    let m = w.folder(id);
    std::fs::create_dir_all(&m).unwrap();
    let r = json!({"id": id, "name": "w-p", "session": w.caller().session, "depth": depth,
        "parentAgent": parent.map(|p| json!({"id": p})), "parentPane": "%5"});
    crate::state::fs::write_json(&m.join("record.json"), &r).unwrap();
    std::fs::read(m.join("record.json")).unwrap()
}
#[derive(Debug, Clone, Copy)]
pub enum Kind {
    Pending,
    Committed,
    Abandoned,
    Aborted,
    Released,
}
/// A resume's successor `id` of `from` under `parent` at `depth`, as A3b-2's `forward` leaves it.
pub fn successor(w: &World, id: &str, from: &str, parent: &str, depth: u64, kind: Kind) {
    if matches!(kind, Kind::Committed | Kind::Released) {
        cx::member(
            w,
            id,
            Some(parent),
            depth,
            json!({"resumedFrom": from, "client": "claude"}),
        );
        if matches!(kind, Kind::Released) {
            let path = super::gens::path(&w.folder(id), 1, "released");
            crate::state::evidence::publish(&path, &json!({"cause": "lost"})).unwrap();
        }
        return;
    }
    let m = w.folder(id);
    std::fs::create_dir_all(&m).unwrap();
    let r = json!({"lifecycle": 1, "id": id, "name": cx::name(id), "session": w.caller().session,
        "sessionId": lf::SID, "team": lf::team(w), "depth": depth, "parentPane": cx::PANE,
        "parentAgent": {"id": parent}, "resumedFrom": from, "pending": lf::pending(0)});
    crate::state::fs::write_json(&m.join("record.json"), &r).unwrap();
    match kind {
        Kind::Abandoned => lf::seed_gen(&m, 0, Some("abandoned"), None),
        // `gens::decide`'s Aborted is `{"aborted": true}` (gens.rs; finding 1-16).
        Kind::Aborted => lf::seed_gen(&m, 0, Some("run"), Some(json!({"aborted": true}))),
        _ => lf::seed_gen(&m, 0, Some("run"), None),
    }
}
