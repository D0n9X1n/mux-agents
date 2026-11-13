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

use crate::state::fs::read_json;
#[test]
fn focus_uses_current_caller() {
    for (stored, code, verb, flag, target) in [
        ("h", 0, "split-window", Some("-h"), "%7"),
        ("v", 0, "split-window", Some("-v"), "%7"),
        ("tab", 0, "new-window", None, "$3:"),
        ("bad", 0, "new-window", None, "$3:"),
        ("", 1, "new-window", None, "$3:"),
    ] {
        let w = World::human("");
        let r = cf::member(&w, ID, json!({"parentPane":"%88"}));
        let mut c = w.caller();
        c.pane = "%7".into();
        w.panes(&[]);
        w.launches_hold();
        w.fakes.mux_rule("@mux-agents-placement", code, stored, "");
        w.fakes
            .mux_rule(verb, 0, "%19|@8|hc attach taaaaaaaaaaaa\n", "");
        w.fakes.mux_rule("#{window_active}", 0, "%19|1|1|1\n", "");
        cf::online(&w);
        let core = hc::connect(&w.ctx).unwrap();
        assert_eq!(
            view::focus_hc(&w.ctx, &core, &c, "worker", None)
                .unwrap()
                .pane,
            "%19"
        );
        let calls = w.fakes.mux_calls();
        let creates: Vec<_> = calls.iter().filter(|a| a[1] == verb).collect();
        assert_eq!(creates.len(), 1);
        let a = creates[0];
        assert_eq!(a[a.iter().position(|v| v == "-t").unwrap() + 1], target);
        if let Some(flag) = flag {
            assert!(a.iter().any(|v| v == flag));
        }
        assert_eq!(&a[a.len() - 2..], ["attach", "taaaaaaaaaaaa"]);
        let mut want = r;
        want["pane"] = json!("%19");
        want["window"] = json!("@8");
        want["startCommand"] = json!("hc attach taaaaaaaaaaaa");
        assert_eq!(
            read_json(&w.folder(ID).join("record.json"))
                .unwrap()
                .unwrap(),
            want
        );
        w.fakes.hc_done().unwrap();
    }
}
#[test]
fn failed_attachment_cleans_only_new_view() {
    for broken_write in [false, true] {
        let w = World::human("");
        let r = cf::member(&w, ID, json!({}));
        let path = w.folder(ID).join("record.json");
        let c = w.caller();
        w.panes(&[]);
        w.launches_hold();
        let p = path.clone();
        w.fakes.mux_script("new-window", move |_| {
            if broken_write {
                std::fs::remove_file(&p).unwrap();
                std::fs::create_dir(&p).unwrap();
            }
            Ok(crate::mux::Output {
                code: 0,
                stdout: "%19|@8|hc attach taaaaaaaaaaaa\n".into(),
                stderr: String::new(),
            })
        });
        if !broken_write {
            w.fakes.mux_first("#{pane_id}|#{==:", 0, "%19|0\n", "");
        }
        w.fakes.mux_rule("#{pane_id}|#{@agent-id}", 0, "|\n", "");
        cf::online(&w);
        let core = hc::connect(&w.ctx).unwrap();
        assert!(view::focus_hc(&w.ctx, &core, &c, "worker", None).is_err());
        let kills: Vec<_> = w
            .fakes
            .mux_calls()
            .into_iter()
            .filter(|a| a.last().is_some_and(|s| s.starts_with("kill-pane")))
            .collect();
        assert_eq!(kills.len(), 1);
        assert_eq!(kills[0].last().unwrap(), "kill-pane -t %19");
        if !broken_write {
            assert_eq!(read_json(&path).unwrap().unwrap(), r);
        }
        w.fakes.hc_done().unwrap();
    }
}
