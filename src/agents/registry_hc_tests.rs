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
use crate::agents::{fixture::write, membership};
fn observed(w: &World, case: &str) -> membership::Snapshot {
    let r = cf::member(w, ID, json!({}));
    write(&w.folder(ID).join("launcher.json"), json!({"exitCode":99}));
    let mut t = cf::wire(&r);
    t["state"] = json!("finished");
    t["reason"] = json!(case);
    if case == "exited" {
        t["exit_code"] = json!(7);
    }
    if case == "killed" {
        t["exit_code"] = json!(137);
    }
    if case == "null" {
        t["reason"] = json!("exited");
    }
    if case == "live" {
        t["state"] = json!("live");
    }
    if case == "lost" {
        t["state"] = json!("live");
        t["argv"] = json!(["/bin/sh", "-l"]);
    }
    if case == "mismatch" {
        t["reason"] = json!("exited");
        t["labels"]["mux.gen"] = json!("1");
    }
    cf::online(w);
    cf::listing(w, &membership::team(&w.caller()), &[]);
    match case {
        "missing" => w.fakes.hc_expect(
            &["get", "taaaaaaaaaaaa", "--json"],
            Ok(crate::spawn::Out {
                code: 1,
                ..Default::default()
            }),
        ),
        "failure" => w.fakes.hc_expect(
            &["get", "taaaaaaaaaaaa", "--json"],
            Err(crate::error::Refusal::internal("get failed")),
        ),
        _ => cf::get(w, "taaaaaaaaaaaa", t),
    }
    w.panes(&[]);
    membership::Snapshot::take(&w.ctx, &w.caller()).unwrap()
}
#[test]
fn hc_reap_notices_once() {
    for case in [
        "exited",
        "null",
        "killed",
        "server_stop",
        "missing",
        "failure",
        "mismatch",
        "live",
        "lost",
    ] {
        let w = World::human("");
        let c = w.caller();
        let s = observed(&w, case);
        let want = ["exited", "null", "killed"].contains(&case);
        let marker = w.folder(ID).join("notice.json");
        w.fakes.clear_log();
        assert_eq!(reap_hc(&w.ctx, &c, &s).unwrap(), want, "{case}");
        if want {
            let keys = ["member", "notice", "mail"]
                .map(|k| crate::agents::lock_key(k, &c, if k == "mail" { "pane-1" } else { ID }));
            assert_eq!(
                w.fakes.locks(),
                keys.iter()
                    .map(|k| format!("+{k}"))
                    .chain(keys.iter().rev().map(|k| format!("-{k}")))
                    .collect::<Vec<_>>()
            );
            let n = read_json(&marker).unwrap().unwrap();
            assert_eq!(
                n["kind"],
                if case == "killed" {
                    "stopped"
                } else {
                    "exited"
                }
            );
            assert_eq!(
                n["exitCode"],
                if case == "exited" {
                    json!(7)
                } else {
                    Value::Null
                }
            );
        }
        assert!(!reap_hc(&w.ctx, &c, &s).unwrap());
        assert!(w.fakes.mux_calls().is_empty());
        assert_eq!(messages(&w).len(), usize::from(want));
        w.fakes.hc_done().unwrap();
    }
}
