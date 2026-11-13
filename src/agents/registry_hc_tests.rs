use super::*;
use crate::agents::{
    core_fixture as cf,
    fixture::{ID, World},
};
use crate::state::fs::read_json;
fn messages(w: &World) -> Vec<Value> {
    std::fs::read_to_string(w.state().join("mail/pane-1.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}
#[test]
fn hc_notice_null_retry() {
    let w = World::human("");
    let r = cf::member(&w, ID, json!({}));
    let c = w.caller();
    let post = |n| {
        crate::agents::lock(&w.ctx, &crate::agents::lock_key("notice", &c, ID), || {
            add_notice(&w.ctx, &c, ID, &r, n)
        })
        .unwrap()
    };
    assert!(post(Notice::Exited(Value::Null)));
    assert_eq!(messages(&w)[0]["body"], "worker exited.");
    std::fs::remove_file(w.folder(ID).join("notice.json")).unwrap();
    assert!(post(Notice::Stopped));
    let n = read_json(&w.folder(ID).join("notice.json"))
        .unwrap()
        .unwrap();
    assert_eq!(
        (n["kind"].clone(), n["exitCode"].clone()),
        (json!("exited"), Value::Null)
    );
    assert_eq!(messages(&w).len(), 1);
    w.fakes.hc_done().unwrap();
}
