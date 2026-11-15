use super::*;
use crate::agents::{
    core_fixture as cf,
    fixture::{ID, World, write},
};
use serde_json::json;
const NEW: &str = "bbbbbbbbbbbb";
fn pair(w: &World) -> [Value; 2] {
    [
        cf::member(w, ID, json!({"createdAt":"2026-11-13T10:00:00Z"})),
        cf::member(
            w,
            NEW,
            json!({"createdAt":"2026-11-13T10:01:00Z","pane":"%19"}),
        ),
    ]
}
fn fact(w: &World, r: &Value, mode: &str) {
    let address = r["hcAddress"].as_str().unwrap();
    if mode == "failure" {
        w.fakes.hc_expect(
            &["get", address, "--json"],
            Err(Refusal::internal("unreachable")),
        );
        return;
    }
    let mut t = cf::wire(r);
    match mode {
        "ended" => {
            t["state"] = json!("finished");
            t["reason"] = json!("killed");
        }
        "lost" => t["argv"] = json!(["/bin/sh", "-l"]),
        "mismatch" => t["labels"]["mux.gen"] = json!("other"),
        "mismatch-ended" => {
            t["state"] = json!("finished");
            t["reason"] = json!("killed");
            t["labels"]["mux.gen"] = json!("other");
        }
        "unknown" => t["argv"] = json!(["/other/launcher"]),
        _ => {}
    }
    cf::get(w, address, t);
}
#[test]
fn duplicate_addresses_and_unavailable_core_refuse() {
    for duplicate in [true, false] {
        let w = World::human("");
        pair(&w);
        let c = w.caller();
        if duplicate {
            cf::member(&w, NEW, json!({"hcAddress":"taaaaaaaaaaaa"}));
            cf::online(&w);
        }
        let e = named(&w.ctx, &c, "worker").unwrap_err();
        if duplicate {
            assert_eq!((e.code, e.check), (2, "target"));
            assert_eq!(
                w.fakes
                    .hc_calls()
                    .iter()
                    .map(|(_, a, _)| a.clone())
                    .collect::<Vec<_>>(),
                [vec!["--version"]]
            );
        } else {
            assert_eq!((e.code, e.check), (1, "hc"));
            assert!(e.message.contains("install or upgrade herdr-core"));
            assert!(w.fakes.hc_calls().is_empty());
        }
        w.fakes.hc_done().unwrap();
    }
}
#[test]
fn mismatched_finished_members_remain_unknown() {
    for (left, right) in [
        ("mismatch-ended", "running"),
        ("running", "mismatch-ended"),
        ("mismatch-ended", "unknown"),
        ("unknown", "mismatch-ended"),
    ] {
        let w = World::human("");
        let [a, b] = pair(&w);
        let c = w.caller();
        cf::online(&w);
        fact(&w, &a, left);
        fact(&w, &b, right);
        let e = named(&w.ctx, &c, "worker").unwrap_err();
        assert_eq!((e.code, e.check), (2, "target"), "{left}/{right}");
        assert_eq!(
            w.fakes
                .hc_calls()
                .iter()
                .map(|(_, a, _)| a.clone())
                .collect::<Vec<_>>(),
            [
                vec!["--version"],
                vec!["get", "taaaaaaaaaaaa", "--json"],
                vec!["get", "tbbbbbbbbbbbb", "--json"]
            ]
        );
        w.fakes.hc_done().unwrap();
    }
}

use crate::{parse::Client, state::fs::read_json};
#[test]
fn single_match_has_no_hc_calls() {
    let w = World::human("");
    cf::member(&w, ID, json!({}));
    let c = w.caller();
    assert_eq!(
        named(&w.ctx, &c, "worker").unwrap().address,
        "taaaaaaaaaaaa"
    );
    assert!(named(&w.ctx, &c, "bad/name").is_err());
    assert!(named(&w.ctx, &c, "absent").is_err());
    assert!(w.fakes.hc_calls().is_empty());
    w.fakes.hc_done().unwrap();
}
#[test]
fn reused_name_remains_addressable_after_reap_and_admission() {
    let w = World::human("");
    let c = w.caller();
    let old = cf::member(&w, ID, json!({}));
    cf::online(&w);
    cf::listing(&w, &membership::team(&c), &[]);
    fact(&w, &old, "ended");
    let core = crate::hc::connect(&w.ctx).unwrap();
    crate::agents::lock(
        &w.ctx,
        &crate::agents::lock_key("spawn", &c, &c.session),
        || {
            let s = membership::Snapshot::for_spawn(&w.ctx, &c, &core)?;
            registry::reap_hc(&w.ctx, &c, &s)?;
            let name = s.admit_name(Some("worker"), Client::Claude, "task")?;
            assert_eq!(name, "worker");
            cf::member(&w, NEW, json!({"name":name}));
            Ok(())
        },
    )
    .unwrap();
    assert!(w.folder(ID).join("notice.json").exists());
    let fresh = read_json(&w.folder(NEW).join("record.json"))
        .unwrap()
        .unwrap();
    cf::online(&w);
    fact(&w, &old, "ended");
    fact(&w, &fresh, "running");
    assert_eq!(
        named(&w.ctx, &c, "worker").unwrap().address,
        "tbbbbbbbbbbbb"
    );
    for id in [ID, NEW] {
        assert!(crate::agents::committed(&w.folder(id)));
    }
    assert_eq!(
        read_json(&w.folder(ID).join("record.json"))
            .unwrap()
            .unwrap(),
        old
    );
    w.fakes.hc_done().unwrap();
}
#[test]
fn duplicate_names_use_only_validated_fresh_gets() {
    for (left, right, wanted) in [
        ("ended", "running", Some(NEW)),
        ("running", "ended", Some(ID)),
        ("ended", "lost", Some(NEW)),
        ("ended", "failure", Some(NEW)),
        ("running", "running", None),
        ("lost", "running", None),
        ("failure", "running", None),
        ("ended", "mismatch", Some(NEW)),
        ("mismatch", "running", None),
        ("failure", "failure", None),
    ] {
        let w = World::human("");
        let [a, b] = pair(&w);
        let c = w.caller();
        write(
            &w.folder(NEW).join("notice.json"),
            json!({"kind":"stopped"}),
        );
        cf::online(&w);
        fact(&w, &a, left);
        fact(&w, &b, right);
        match (named(&w.ctx, &c, "worker"), wanted) {
            (Ok(id), Some(wanted)) => assert_eq!(id.id, wanted),
            (Err(e), None) => assert_eq!(
                (e.code, e.check, e.message.as_str()),
                (2, "target", "no unique committed member has that identity")
            ),
            (got, want) => panic!("{left}/{right}: {got:?}, expected {want:?}"),
        }
        assert_eq!(
            w.fakes
                .hc_calls()
                .iter()
                .map(|(_, a, _)| a.clone())
                .collect::<Vec<_>>(),
            [
                vec!["--version"],
                vec!["get", "taaaaaaaaaaaa", "--json"],
                vec!["get", "tbbbbbbbbbbbb", "--json"]
            ]
        );
        w.fakes.hc_done().unwrap();
    }
}
#[test]
fn all_ended_uses_launch_time_not_notice_or_directory_order() {
    for (a_at, b_at, wanted) in [
        ("2026-11-13T10:00:00Z", "2026-11-13T10:01:00Z", NEW),
        ("2026-11-13T10:02:00Z", "2026-11-13T10:01:00Z", ID),
        ("2026-11-13T10:00:00Z", "2026-11-13T10:00:00Z", ID),
        ("invalid", "2026-11-13T10:00:00Z", NEW),
        ("invalid", "invalid", ID),
    ] {
        let w = World::human("");
        let c = w.caller();
        let b = cf::member(&w, NEW, json!({"createdAt":b_at}));
        let a = cf::member(&w, ID, json!({"createdAt":a_at}));
        write(
            &w.folder(ID).join("notice.json"),
            json!({"kind":"stopped","at":"2099-01-01T00:00:00Z"}),
        );
        cf::online(&w);
        fact(&w, &a, "ended");
        fact(&w, &b, "ended");
        assert_eq!(named(&w.ctx, &c, "worker").unwrap().id, wanted);
        for (id, r) in [(ID, a), (NEW, b)] {
            assert_eq!(
                read_json(&w.folder(id).join("record.json"))
                    .unwrap()
                    .unwrap(),
                r
            );
            assert!(crate::agents::committed(&w.folder(id)));
        }
        w.fakes.hc_done().unwrap();
    }
}
#[test]
fn only_committed_same_name_matches_are_queried() {
    let w = World::human("");
    let [a, b] = pair(&w);
    let c = w.caller();
    cf::member(&w, "cccccccccccc", json!({"name":"other"}));
    cf::member(&w, "dddddddddddd", json!({}));
    std::fs::remove_file(w.folder("dddddddddddd").join("commit.json")).unwrap();
    cf::online(&w);
    fact(&w, &a, "ended");
    fact(&w, &b, "running");
    assert_eq!(named(&w.ctx, &c, "worker").unwrap().id, NEW);
    w.fakes.hc_done().unwrap();
}

#[test]
fn adapters_resolve_duplicates_without_running_a_core_terminal() {
    use crate::agents::{fixture::row_of, message, view};
    for verb in ["focus", "capture", "stop"] {
        let w = World::human("");
        let [a, b] = pair(&w);
        let c = w.caller();
        let mut row = row_of("%19|@8|300||", &[]);
        row.command = "hc attach tbbbbbbbbbbbb".into();
        w.pane_rows(&[row]);
        w.fakes.mux_rule("#{window_active}", 0, "%19|1|1|1\n", "");
        w.fakes.mux_rule("show-buffer", 0, "one\ntwo\n", "");
        cf::online(&w);
        let core = if verb == "focus" {
            Some(crate::hc::connect(&w.ctx).unwrap())
        } else {
            None
        };
        fact(&w, &a, "ended");
        fact(&w, &b, "running");
        if verb == "stop" {
            cf::online(&w);
            cf::answer(&w, &["kill", "tbbbbbbbbbbbb", "--json"], "{}");
            fact(&w, &b, "ended");
        }
        match verb {
            "focus" => assert_eq!(
                view::focus_hc(&w.ctx, core.as_ref().unwrap(), &c, "worker", Some("%19"))
                    .unwrap()
                    .pane,
                "%19"
            ),
            "capture" => assert_eq!(
                message::capture_hc(&w.ctx, &c, "worker", Some("%19"), 2, None)
                    .unwrap()
                    .1,
                "one\ntwo"
            ),
            _ => assert_eq!(
                message::stop_hc(&w.ctx, &w.request(&["stop", "worker"])).unwrap()["hcAddress"],
                "tbbbbbbbbbbbb"
            ),
        }
        assert_eq!(
            w.fakes
                .hc_calls()
                .iter()
                .map(|(_, a, _)| a[0].clone())
                .collect::<Vec<_>>(),
            if verb == "stop" {
                vec!["--version", "get", "get", "--version", "kill", "get"]
            } else {
                vec!["--version", "get", "get"]
            }
        );
        assert!(
            !w.fakes
                .mux_calls()
                .iter()
                .any(|a| a[1] == "new-window" || a[1] == "split-window")
        );
        w.fakes.hc_done().unwrap();
    }
}
// A member a committed resume names in `resumedFrom` is superseded: its name means its successor alone, so no hc
// answer about the old terminal (gone, unreadable or ended) is needed or can make the name ambiguous.
#[test]
fn a_name_skips_the_member_a_resume_superseded() {
    let w = World::human("");
    let c = w.caller();
    pair(&w);
    cf::member(
        &w,
        NEW,
        json!({"createdAt":"2026-11-13T10:01:00Z","pane":"%19","resumedFrom":ID}),
    );
    assert_eq!(named(&w.ctx, &c, "worker").unwrap().id, NEW);
    assert!(w.fakes.hc_calls().is_empty());
    // An uncommitted successor supersedes nothing.
    std::fs::remove_file(w.folder(NEW).join("commit.json")).unwrap();
    assert_eq!(named(&w.ctx, &c, "worker").unwrap().id, ID);
}

#[test]
fn by_id_finds_each_of_two_same_name_members() {
    use crate::agents::{core_fixture as cf, fixture::World};
    let w = World::human("");
    let c = w.caller();
    cf::member(&w, "aaaaaaaaaaaa", serde_json::json!({"name": "twin"}));
    cf::member(
        &w,
        "bbbbbbbbbbbb",
        serde_json::json!({"name": "twin", "pane": "%8"}),
    );
    for id in ["aaaaaaaaaaaa", "bbbbbbbbbbbb"] {
        assert_eq!(super::by_id(&w.ctx, &c, id).unwrap().id, id);
    }
    for id in ["eeeeeeeeeeee", "../x", ""] {
        assert_eq!(
            super::by_id(&w.ctx, &c, id).unwrap_err().check,
            "target",
            "{id}"
        );
    }
    assert!(w.fakes.hc_calls().is_empty(), "by id never asks hc");
}

#[test]
fn the_bar_reads_and_captures_by_id_never_by_name() {
    use crate::agents::{core_fixture as cf, fixture::World, message};
    let w = World::human("");
    let c = w.caller();
    let a = cf::member(&w, "aaaaaaaaaaaa", serde_json::json!({"name": "twin"}));
    let b = cf::member(&w, "bbbbbbbbbbbb", serde_json::json!({"name": "twin"}));
    // `retained` reads only a terminal that is still the member's own: hc `get` answers it first.
    let own = |id: &str| {
        if id == "aaaaaaaaaaaa" {
            cf::wire(&a)
        } else {
            cf::wire(&b)
        }
    };
    w.panes(&[]);
    let e = message::capture_member(&w.ctx, &c, "bbbbbbbbbbbb", None, 2).unwrap_err();
    assert_eq!(e.message, "the agent has no live view; run focus twin");
    for (id, text, want) in [
        ("aaaaaaaaaaaa", "old a\n", Some("old a\n")),
        ("bbbbbbbbbbbb", " \n", None),
    ] {
        cf::online(&w);
        cf::get(&w, &format!("t{id}"), own(id));
        cf::answer(&w, &["read", &format!("t{id}"), "--lines", "200"], text);
        assert_eq!(
            message::retained(&w.ctx, &c, id, 200).unwrap().as_deref(),
            want,
            "{id}"
        );
    }
    // A read that fails: hc's `not_found` (exit 3, as hc 0.1.21 answers) is nothing to show; hc unreachable is an error.
    let gone = r#"{"error":{"hint":"see `hc ls --all`","kind":"not_found","message":"no terminal \"taaaaaaaaaaaa\""}}"#;
    let out = |code: i32, stdout: &str| -> crate::error::Res<crate::spawn::Out> {
        Ok(crate::spawn::Out {
            code,
            stdout: stdout.into(),
            stderr: String::new(),
        })
    };
    for (code, reply, want) in [
        (3, gone, Some(None)),
        (5, r#"{"error":{"kind":"unavailable"}}"#, None),
    ] {
        cf::online(&w);
        cf::get(&w, "taaaaaaaaaaaa", own("aaaaaaaaaaaa"));
        w.fakes.hc_expect(
            &["read", "taaaaaaaaaaaa", "--lines", "200"],
            out(code, reply),
        );
        assert_eq!(
            message::retained(&w.ctx, &c, "aaaaaaaaaaaa", 200).ok(),
            want,
            "exit {code}"
        );
    }
    assert!(
        !w.fakes
            .mux_calls()
            .iter()
            .any(|a| a.iter().any(|s| s.contains("capture-pane")))
    );
    w.fakes.hc_done().unwrap();
}

#[test]
fn kept_member_tells_a_dropped_terminal_from_a_failure() {
    use crate::agents::{core_fixture as cf, fixture::World, message};
    let w = World::human("");
    let c = w.caller();
    let record = cf::member(&w, "aaaaaaaaaaaa", serde_json::json!({"name": "twin"}));
    let gone = r#"{"error":{"hint":"see `hc ls --all`","kind":"not_found","message":"no terminal \"taaaaaaaaaaaa\""}}"#;
    let out = |code: i32, stdout: &str| -> crate::error::Res<crate::spawn::Out> {
        Ok(crate::spawn::Out {
            code,
            stdout: stdout.into(),
            stderr: String::new(),
        })
    };
    cf::online(&w);
    let core = crate::hc::connect(&w.ctx).unwrap();
    cf::get(&w, "taaaaaaaaaaaa", cf::wire(&record));
    w.fakes
        .hc_expect(&["get", "taaaaaaaaaaaa", "--json"], out(3, gone));
    w.fakes.hc_expect(
        &["get", "taaaaaaaaaaaa", "--json"],
        out(5, r#"{"error":{"kind":"unavailable"}}"#),
    );
    assert_eq!(
        message::kept_member(&w.ctx, &c, &core, "aaaaaaaaaaaa").unwrap(),
        Some(true),
        "hc keeps it"
    );
    assert_eq!(
        message::kept_member(&w.ctx, &c, &core, "aaaaaaaaaaaa").unwrap(),
        Some(false),
        "hc dropped it"
    );
    assert_eq!(
        message::kept_member(&w.ctx, &c, &core, "aaaaaaaaaaaa")
            .unwrap_err()
            .check,
        "hc",
        "hc unreachable"
    );
    assert_eq!(
        message::kept_member(&w.ctx, &c, &core, "eeeeeeeeeeee").unwrap(),
        None,
        "no such member"
    );
    w.fakes.hc_done().unwrap();
}

#[test]
fn capture_member_reads_the_requested_twin_and_refuses_a_moved_pane() {
    use crate::agents::{core_fixture as cf, fixture::World, message};
    use std::{cell::RefCell, rc::Rc};
    let w = World::human("");
    let c = w.caller();
    cf::member(&w, "aaaaaaaaaaaa", json!({"name": "twin", "pane": "%8"}));
    cf::member(&w, "bbbbbbbbbbbb", json!({"name": "twin", "pane": "%9"}));
    let rows: Vec<_> = [
        ("%8|@5|300||", "aaaaaaaaaaaa"),
        ("%9|@6|301||", "bbbbbbbbbbbb"),
    ]
    .into_iter()
    .map(|(row, id)| {
        let mut row = crate::agents::fixture::row_of(row, &[]);
        row.command = format!("hc attach t{id}");
        row
    })
    .collect();
    w.pane_rows(&rows);
    // Each pane holds its own text: `show-buffer` answers with the pane the last `capture-pane` named.
    let captured = Rc::new(RefCell::new(String::new()));
    let seen = captured.clone();
    w.fakes.mux_script("capture-pane", move |argv| {
        let at = argv.iter().position(|a| a == "-t").expect("a target pane");
        *seen.borrow_mut() = argv[at + 1].clone();
        Ok(crate::mux::Output::default())
    });
    let shown = captured.clone();
    w.fakes.mux_script("show-buffer", move |_| {
        let text = match shown.borrow().as_str() {
            "%8" => "a one\na two\n",
            "%9" => "b one\nb two\n",
            _ => "",
        };
        Ok(crate::mux::Output {
            stdout: text.into(),
            ..Default::default()
        })
    });
    for (id, pane, text) in [
        ("bbbbbbbbbbbb", "%9", "b one\nb two"),
        ("aaaaaaaaaaaa", "%8", "a one\na two"),
    ] {
        for expected in [None, Some(pane)] {
            let (got, tail) = message::capture_member(&w.ctx, &c, id, expected, 2).unwrap();
            assert_eq!(
                (got.id.as_str(), got.pane.as_str(), tail.as_str()),
                (id, pane, text),
                "{id} {expected:?}"
            );
        }
    }
    let moved = message::capture_member(&w.ctx, &c, "bbbbbbbbbbbb", Some("%8"), 2).unwrap_err();
    assert_eq!(
        moved.message,
        "the agent is no longer on the pane the bar drew"
    );
    assert!(
        !w.fakes
            .mux_calls()
            .iter()
            .any(|a| a[1] == "new-window" || a[1] == "split-window")
    );
    w.fakes.hc_done().unwrap();
}

#[test]
fn retained_and_kept_never_trust_another_identitys_terminal() {
    use crate::agents::{core_fixture as cf, fixture::World, message};
    let w = World::human("");
    let c = w.caller();
    let record = cf::member(&w, "aaaaaaaaaaaa", serde_json::json!({"name": "twin"}));
    // The terminal at the member's address is labelled for another generation: the snapshot calls the row Unknown.
    let mut other = cf::wire(&record);
    other["labels"]["mux.gen"] = serde_json::json!("1");
    cf::online(&w);
    cf::get(&w, "taaaaaaaaaaaa", other.clone());
    assert_eq!(
        message::retained(&w.ctx, &c, "aaaaaaaaaaaa", 200).ok(),
        Some(None),
        "its output is never shown"
    );
    cf::online(&w);
    let core = crate::hc::connect(&w.ctx).unwrap();
    cf::get(&w, "taaaaaaaaaaaa", other);
    assert_eq!(
        message::kept_member(&w.ctx, &c, &core, "aaaaaaaaaaaa").ok(),
        Some(Some(false)),
        "it is never reattached"
    );
    assert!(
        w.fakes.hc_calls().iter().all(|(_, a, _)| a[0] != "read"),
        "another identity's terminal is never read"
    );
    w.fakes.hc_done().unwrap();
}
