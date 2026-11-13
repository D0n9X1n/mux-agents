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

use super::view;
use crate::hc;
fn rows(w: &World, r: &serde_json::Value) {
    let mut row = super::fixture::row_of("%9|@5|300||", &[]);
    row.command = r["startCommand"].as_str().unwrap().into();
    w.pane_rows(&[row]);
}
#[test]
fn focus_selects_proven_view_and_refuses_expected_mismatch() {
    let w = World::human("");
    let r = cf::member(&w, ID, json!({}));
    rows(&w, &r);
    let c = w.caller();
    cf::online(&w);
    let core = hc::connect(&w.ctx).unwrap();
    w.fakes.mux_rule("#{window_active}", 0, "%9|1|1|1\n", "");
    assert!(view::focus_hc(&w.ctx, &core, &c, "worker", Some("%8")).is_err());
    assert!(!w.fakes.mux_calls().iter().any(|a| a[1] == "if-shell"));
    assert_eq!(
        view::focus_hc(&w.ctx, &core, &c, "worker", Some("%9"))
            .unwrap()
            .pane,
        "%9"
    );
    w.fakes.mux_first("#{window_active}", 0, "%9|1|1|0\n", "");
    assert!(view::focus_hc(&w.ctx, &core, &c, "worker", None).is_err());
    w.panes(&[]);
    assert!(view::focus_hc(&w.ctx, &core, &c, "worker", Some("%9")).is_err());
    assert!(
        !w.fakes
            .mux_calls()
            .iter()
            .any(|a| a[1] == "new-window" || a[1] == "split-window")
    );
    w.fakes.hc_done().unwrap();
}
