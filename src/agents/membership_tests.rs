use super::*;
use crate::agents::{
    core_fixture as f,
    fixture::{ID, World, write},
};
use serde_json::json;

#[test]
fn local_members_ignore_legacy_without_writes() {
    let w = World::human("");
    let caller = w.caller();
    w.fakes.clear_log();
    assert!(locals(&w.ctx, &caller).unwrap().is_empty());
    assert!(!w.ctx.state_root.exists());
    w.agent_record("bbbbbbbbbbbb", json!({}));
    f::member(&w, ID, json!({}));
    f::member(
        &w,
        "cccccccccccc",
        json!({"session":"$4","team":"other:$4"}),
    );
    let folder = w.folder(ID);
    std::fs::remove_file(folder.join("commit.json")).unwrap();
    let before = std::fs::read(folder.join("record.json")).unwrap();
    w.fakes.clear_log();
    let local = locals(&w.ctx, &caller).unwrap();
    assert_eq!(
        local
            .iter()
            .map(|l| l.identity.id.as_str())
            .collect::<Vec<_>>(),
        [ID]
    );
    assert!(!local[0].stored.committed);
    assert!(!local[0].stored.noticed);
    assert_eq!(std::fs::read(folder.join("record.json")).unwrap(), before);
    assert!(w.fakes.hc_calls().is_empty());
    assert!(w.fakes.mux_calls().is_empty());
    assert!(w.fakes.locks().is_empty());
    for value in [json!([]), json!({"hcAddress":"t1"})] {
        write(&folder.join("record.json"), value);
        assert!(locals(&w.ctx, &caller).is_err());
    }
    std::fs::write(folder.join("record.json"), "{").unwrap();
    assert!(locals(&w.ctx, &caller).is_err());
}
