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
    let second = cf::member(&w, "bbbbbbbbbbbb", json!({}));
    cf::online(&w);
    let first = read_json(&w.folder(ID).join("record.json"))
        .unwrap()
        .unwrap();
    cf::get(&w, "taaaaaaaaaaaa", cf::wire(&first));
    cf::get(&w, "tbbbbbbbbbbbb", cf::wire(&second));
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

#[test]
fn focus_reopens_once() {
    use std::{
        sync::{Arc, mpsc},
        time::{Duration, Instant},
    };
    let w = World::human("");
    cf::member(&w, ID, json!({}));
    let c = w.caller();
    let gate = Arc::new(crate::testing::Gate::default());
    w.fakes.share_locks(gate.clone());
    let key = super::lock_key("member", &c, ID);
    let key2 = key.clone();
    let g = gate.clone();
    let (tx, rx) = mpsc::channel();
    w.fakes.mux_script("new-window", move |_| {
        tx.send(()).unwrap();
        let end = Instant::now() + Duration::from_secs(3);
        while g.waiting(&key2) == 0 {
            assert!(Instant::now() < end, "second focus never waited");
            std::thread::yield_now();
        }
        Ok(crate::mux::Output {
            code: 0,
            stdout: "%19|@8|hc attach taaaaaaaaaaaa\n".into(),
            stderr: String::new(),
        })
    });
    w.panes(&[]);
    w.launches_hold();
    w.fakes.mux_rule("#{window_active}", 0, "%19|1|1|1\n", "");
    let root = w.ctx.state_root.clone();
    let folder = w.folder(ID);
    let g = gate.clone();
    let child = std::thread::spawn(move || {
        let mut other = World::human("");
        other.ctx.state_root = root;
        other.fakes.share_locks(g);
        other.fakes.mux_script("list-panes", move |argv| {
            let r = read_json(&folder.join("record.json")).unwrap().unwrap();
            let mut row = super::fixture::row_of("%19|@8|300||", &[]);
            row.command = r["startCommand"].as_str().unwrap().into();
            Ok(crate::mux::Output {
                code: 0,
                stdout: super::fixture::listed_rows(argv, &[row], "\n"),
                stderr: String::new(),
            })
        });
        other
            .fakes
            .mux_rule("#{window_active}", 0, "%19|1|1|1\n", "");
        cf::online(&other);
        let core = hc::connect(&other.ctx).unwrap();
        let c = other.caller();
        rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(
            view::focus_hc(&other.ctx, &core, &c, "worker", None)
                .unwrap()
                .pane,
            "%19"
        );
        other.fakes.hc_done().unwrap();
        assert!(
            !other
                .fakes
                .mux_calls()
                .iter()
                .any(|a| a[1] == "new-window" || a[1] == "split-window")
        );
    });
    cf::online(&w);
    let core = hc::connect(&w.ctx).unwrap();
    view::focus_hc(&w.ctx, &core, &c, "worker", None).unwrap();
    child.join().unwrap();
    assert_eq!(gate.waited(&key), 1);
    assert_eq!(
        w.fakes
            .mux_calls()
            .iter()
            .filter(|a| a[1] == "new-window")
            .count(),
        1
    );
    w.fakes.hc_done().unwrap();
}
#[test]
fn focus_rechecks_under_member_lock() {
    let w = World::human("");
    let r = cf::member(&w, ID, json!({}));
    let c = w.caller();
    rows(&w, &r);
    let path = w.folder(ID).join("record.json");
    let mut changed = r;
    changed["generation"] = json!("1");
    w.fakes.lock_probe(move |e| {
        if e.starts_with("+member|") {
            write(&path, changed.clone());
        }
        String::new()
    });
    cf::online(&w);
    let core = hc::connect(&w.ctx).unwrap();
    assert!(view::focus_hc(&w.ctx, &core, &c, "worker", None).is_err());
    assert!(
        !w.fakes
            .mux_calls()
            .iter()
            .any(|a| a[1] == "list-panes" || a[1] == "new-window" || a[1] == "if-shell")
    );
    w.fakes.hc_done().unwrap();
}

#[test]
fn focus_rechecks_aborted_commit_under_member_lock() {
    let w = World::human("");
    let r = cf::member(&w, ID, json!({}));
    let c = w.caller();
    rows(&w, &r);
    let path = w.folder(ID).join("commit.json");
    w.fakes.lock_probe(move |event| {
        if event.starts_with("+member|") {
            write(&path, json!({"aborted":true}));
        }
        String::new()
    });
    cf::online(&w);
    let core = hc::connect(&w.ctx).unwrap();
    assert!(view::focus_hc(&w.ctx, &core, &c, "worker", None).is_err());
    assert!(
        !w.fakes
            .mux_calls()
            .iter()
            .any(|a| a[1] == "list-panes" || a[1] == "new-window" || a[1] == "if-shell")
    );
    w.fakes.hc_done().unwrap();
}

#[test]
fn capture_requires_view_and_never_reopens() {
    for present in [false, true] {
        let w = World::human("");
        let r = cf::member(&w, ID, json!({}));
        let c = w.caller();
        if present {
            rows(&w, &r);
        } else {
            w.panes(&[]);
        }
        w.fakes.mux_rule("show-buffer", 0, "one\ntwo\n", "");
        for expected in [None, Some("%9")] {
            let got = super::message::capture_hc(&w.ctx, &c, "worker", expected, 2, None);
            if present {
                assert_eq!(got.unwrap().1, "one\ntwo");
            } else {
                assert!(got.unwrap_err().message.contains("focus worker"));
            }
        }
        assert!(super::message::capture_hc(&w.ctx, &c, "worker", Some("%8"), 2, None).is_err());
        assert!(!w.fakes.mux_calls().iter().any(|a| a[1] == "new-window"
            || a[1] == "split-window"
            || (!present && a.iter().any(|s| s.contains("capture-pane")))));
        w.fakes.hc_done().unwrap();
    }
}
