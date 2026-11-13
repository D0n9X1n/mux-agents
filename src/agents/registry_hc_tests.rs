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
#[test]
fn hc_reap_rereads_identity_under_both_locks() {
    for key in [
        "generation",
        "hcAddress",
        "team",
        "sessionId",
        "launcherArgv",
        "id",
        "name",
        "session",
        "removed",
        "broken",
        "parentPane",
    ] {
        let w = World::human("");
        let c = w.caller();
        let s = observed(&w, "exited");
        let file = w.folder(ID).join("record.json");
        let mut r = s.records[ID].record.clone();
        let notice = crate::agents::lock_key("notice", &c, ID);
        r[key] = match key {
            "launcherArgv" => json!(["other"]),
            "parentPane" => json!("%2"),
            _ => json!("changed"),
        };
        w.fakes.lock_probe(move |event| {
            if event == format!("+{notice}") {
                match key {
                    "removed" => std::fs::remove_file(&file).unwrap(),
                    "broken" => std::fs::write(&file, "{").unwrap(),
                    _ => write(&file, r.clone()),
                }
            }
            String::new()
        });
        let got = reap_hc(&w.ctx, &c, &s);
        if key == "broken" {
            assert!(got.is_err());
        } else {
            assert_eq!(got.unwrap(), key == "parentPane", "stale identity {key}");
        }
        assert!(messages(&w).is_empty());
        if key == "parentPane" {
            assert!(w.state().join("mail/pane-2.jsonl").is_file());
        }
        w.fakes.hc_done().unwrap();
    }
}
#[test]
fn hc_reap_delivery_failure_is_retryable() {
    for parent in ["%1", "%2"] {
        let w = World::human("");
        let c = w.caller();
        let s = observed(&w, "null");
        let mailbox = w.state().join("mail/pane-1.jsonl");
        std::fs::create_dir(&mailbox).unwrap();
        assert!(reap_hc(&w.ctx, &c, &s).is_err());
        assert!(!w.folder(ID).join("notice.json").exists());
        std::fs::remove_dir(&mailbox).unwrap();
        let mut r = s.records[ID].record.clone();
        r["parentPane"] = json!(parent);
        write(&w.folder(ID).join("record.json"), r);
        assert!(reap_hc(&w.ctx, &c, &s).unwrap());
        assert!(!reap_hc(&w.ctx, &c, &s).unwrap());
        assert_eq!(messages(&w).len(), usize::from(parent == "%1"));
        if parent == "%2" {
            assert_eq!(
                std::fs::read_to_string(w.state().join("mail/pane-2.jsonl"))
                    .unwrap()
                    .lines()
                    .count(),
                1
            );
        }
        w.fakes.hc_done().unwrap();
    }
}
#[test]
fn hc_reap_stop_contention() {
    fn until(ready: impl Fn() -> bool) {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ready() {
            assert!(std::time::Instant::now() < end, "contention timeout");
            std::thread::yield_now();
        }
    }
    for stop_first in [false, true] {
        let w = World::human("");
        let c = w.caller();
        let s = observed(&w, "exited");
        let gate = std::sync::Arc::new(crate::testing::Gate::default());
        let key = crate::agents::lock_key("member", &c, ID);
        let root = w.ctx.state_root.clone();
        w.fakes.share_locks(gate.clone());
        std::thread::scope(|scope| {
            let stop = scope.spawn(|| {
                let mut other = World::human("");
                other.ctx.state_root = root;
                other.fakes.share_locks(gate.clone());
                let c = other.caller();
                if stop_first {
                    let (g, k) = (gate.clone(), key.clone());
                    other.fakes.lock_probe(move |e| {
                        if e == format!("+{k}") {
                            until(|| g.waiting(&k) > 0);
                        }
                        String::new()
                    });
                } else {
                    until(|| gate.held(&key));
                }
                with_member_notice(&other.ctx, &c, ID, || {
                    let r = record(&other.ctx, &c, ID)?.unwrap();
                    add_notice(&other.ctx, &c, ID, &r, Notice::Stopped)
                })
                .unwrap();
                other.fakes.hc_done().unwrap();
            });
            if stop_first {
                until(|| gate.held(&key));
            } else {
                let (g, k) = (gate.clone(), key.clone());
                w.fakes.lock_probe(move |e| {
                    if e == format!("+{k}") {
                        until(|| g.waiting(&k) > 0);
                    }
                    String::new()
                });
            }
            reap_hc(&w.ctx, &c, &s).unwrap();
            stop.join().unwrap();
        });
        assert_eq!(gate.waited(&key), 1);
        let n = read_json(&w.folder(ID).join("notice.json"))
            .unwrap()
            .unwrap();
        let m = messages(&w);
        assert_eq!(m.len(), 1);
        assert_eq!(n["kind"], if stop_first { "stopped" } else { "exited" });
        assert_eq!(
            (&m[0]["kind"], &m[0]["exitCode"]),
            (&n["kind"], &n["exitCode"])
        );
        w.fakes.hc_done().unwrap();
    }
}
