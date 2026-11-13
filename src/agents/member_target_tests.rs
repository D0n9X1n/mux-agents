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
