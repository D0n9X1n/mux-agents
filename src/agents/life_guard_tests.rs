//! adopt-3e AD-R1: `Guard::Life`, the topology writer's guard.
use super::*;
use crate::agents::fixture::{ID, World};
use serde_json::json;

fn world() -> (World, Scope) {
    let w = World::human("");
    w.state();
    let s = Scope::of(&w.caller());
    (w, s)
}
fn record(w: &World) -> Value {
    crate::state::fs::read_json(&w.folder(ID).join("record.json"))
        .unwrap()
        .unwrap()
}

// adopt-3e AD-R1: `Guard::Life` admits a lifecycle record of the id whatever its `pending` or `completing`;
// never a pre-lifecycle record, another id's record (a3b call 35) or no record.
#[test]
fn life_admits_any_lifecycle_record_of_the_id() {
    assert!(!Guard::Life.admits(ID, None), "absent");
    let (w, s) = world();
    let cases: [(&str, Value, bool); 5] = [
        ("plain", json!({"lifecycle": 1, "id": ID}), true),
        (
            "pending",
            json!({"lifecycle": 1, "id": ID, "pending": {"gen": 2}}),
            true,
        ),
        (
            "completing",
            json!({"lifecycle": 1, "id": ID, "completing": {"gen": 1}}),
            true,
        ),
        ("pre-lifecycle", json!({"id": ID}), false),
        (
            "other id",
            json!({"lifecycle": 1, "id": "bbbbbbbbbbbb"}),
            false,
        ),
    ];
    for (case, r, ok) in cases {
        let m = w.folder(ID);
        std::fs::create_dir_all(&m).unwrap();
        crate::state::fs::write_json(&m.join("record.json"), &r).unwrap();
        assert_eq!(Guard::Life.admits(ID, Some(&r)), ok, "{case}: admits");
        let got = open(&w.ctx, &s, |tx| {
            tx.record_write(ID, Guard::Life, |f| {
                f.insert("depth".into(), json!(2));
                Ok(())
            })
        })
        .unwrap();
        assert_eq!(got.is_some(), ok, "{case}");
        assert_eq!(record(&w).get("depth") == Some(&json!(2)), ok, "{case}");
    }
}
