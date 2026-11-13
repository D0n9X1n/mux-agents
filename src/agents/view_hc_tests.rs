use super::{
    core_fixture as cf,
    fixture::{ID, World, write},
    member_target,
};
use serde_json::json;
#[test]
fn member_target_rechecks_complete_identity() {
    let w = World::human("");
    let r = cf::member(&w, ID, json!({}));
    let c = w.caller();
    let id = member_target::named(&w.ctx, &c, "worker").unwrap();
    write(&w.folder(ID).join("commit.json"), json!({"aborted":true}));
    assert!(member_target::current(&w.ctx, &c, &id).is_err());
    write(
        &w.folder(ID).join("commit.json"),
        json!({"committedAt":"x"}),
    );
    for (field, value) in [
        ("hcAddress", json!("other")),
        ("generation", json!("1")),
        ("sessionId", json!("0f11fc2f-2032-48ef-a920-4af2a8e5af51")),
        ("launcherArgv", json!(["other"])),
    ] {
        let mut changed = r.clone();
        changed[field] = value;
        write(&w.folder(ID).join("record.json"), changed);
        assert!(member_target::current(&w.ctx, &c, &id).is_err());
    }
    write(&w.folder(ID).join("record.json"), r);
    assert!(member_target::current(&w.ctx, &c, &id).is_ok());
    cf::member(&w, "bbbbbbbbbbbb", json!({}));
    assert!(member_target::named(&w.ctx, &c, "worker").is_err());
    assert!(member_target::named(&w.ctx, &c, "bad/name").is_err());
    w.fakes.hc_done().unwrap();
}
