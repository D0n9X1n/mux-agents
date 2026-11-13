use super::{
    core_fixture as cf,
    fixture::{ID, World},
    registry,
};
use serde_json::{Value, json};
fn call(w: &World) -> crate::error::Res<serde_json::Map<String, Value>> {
    let args = ["stop", "worker"].map(str::to_string);
    let (def, request) = crate::cli::parse(&args).unwrap();
    (def.handler)(&w.ctx, &request)
}
#[test]
fn stop_uses_hc_only() {
    for (mode, life) in [
        ("killed", "ended"),
        ("running", "running"),
        ("shell", "lost"),
        ("server_stop", "unknown"),
        ("mismatch", "unknown"),
        ("failure", "unknown"),
    ] {
        let w = World::human("");
        let r = cf::member(&w, ID, json!({}));
        let c = w.caller();
        let folder = w.folder(ID);
        let original = std::fs::read(folder.join("record.json")).unwrap();
        let f = w.fakes.clone();
        let member = format!("member|{}|{ID}", c.server_key);
        let notice = format!("notice|{}|{ID}", c.server_key);
        w.fakes.hc_probe(move |_| {
            for key in [&member, &notice] {
                assert_eq!(
                    f.locks().iter().rev().find(|e| e.ends_with(key.as_str())),
                    Some(&format!("+{key}"))
                );
            }
        });
        for _ in 0..2 {
            cf::online(&w);
            cf::answer(&w, &["kill", "taaaaaaaaaaaa", "--json"], "{}");
            let mut fact = cf::wire(&r);
            match mode {
                "killed" | "server_stop" => {
                    fact["state"] = json!("finished");
                    fact["reason"] = json!(mode);
                }
                "shell" => fact["argv"] = json!(["/bin/sh", "-l"]),
                "mismatch" => fact["labels"]["mux.gen"] = json!("9"),
                _ => {}
            }
            if mode == "failure" {
                w.fakes.hc_expect(
                    &["get", "taaaaaaaaaaaa", "--json"],
                    Err(crate::error::Refusal::internal("unreachable")),
                );
            } else {
                cf::get(&w, "taaaaaaaaaaaa", fact);
            }
            let got = call(&w).unwrap();
            let mut cli = serde_json::Map::new();
            cli.insert("ok".into(), json!(true));
            cli.insert("verb".into(), json!("stop"));
            cli.extend(got);
            assert_eq!(
                crate::json::to_stdout(&Value::Object(cli)),
                format!(
                    r#"{{"ok":true,"verb":"stop","name":"worker","hcAddress":"taaaaaaaaaaaa","life":"{life}"}}"#
                )
            );
            w.fakes.hc_done().unwrap();
        }
        assert_eq!(std::fs::read(folder.join("record.json")).unwrap(), original);
        assert_eq!(
            std::fs::read_to_string(w.state().join("mail/pane-1.jsonl"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert!(
            !w.fakes
                .os_calls()
                .iter()
                .any(|a| a == "table" || a.starts_with("kill ") || a == "open 300")
        );
        assert!(!w.fakes.mux_calls().iter().any(|a| {
            a.iter()
                .any(|s| s.contains("kill-pane") || s == "list-panes")
        }));
        let locks = w.fakes.locks();
        assert_eq!(
            &locks[..2],
            &[
                format!("+member|{}|{ID}", c.server_key),
                format!("+notice|{}|{ID}", c.server_key)
            ]
        );
        w.fakes.hc_probe(|_| {});
        let mut fact = cf::wire(&r);
        fact["state"] = json!("finished");
        fact["reason"] = json!("killed");
        let id = super::core_identity::Identity::read(&r, ID, r["team"].as_str().unwrap()).unwrap();
        cf::online(&w);
        cf::get(&w, "taaaaaaaaaaaa", fact);
        let t = crate::hc::connect(&w.ctx)
            .unwrap()
            .get("taaaaaaaaaaaa")
            .unwrap();
        w.fakes.hc_done().unwrap();
        let snapshot = super::membership::Snapshot {
            facts: super::core_facts::Snapshot {
                local: vec![super::core_facts::Observation {
                    identity: id,
                    terminal: Some(t),
                    life: super::lifecycle::Life::Ended,
                }],
                reserved: vec![],
            },
            records: Default::default(),
            views: Default::default(),
            core_error: None,
            view_error: None,
        };
        assert!(!registry::reap_hc(&w.ctx, &c, &snapshot).unwrap());
    }
}

use super::fixture::write;
#[test]
fn stop_errors_and_authorization() {
    for mode in [
        "failure",
        "uncertain",
        "denied",
        "identity",
        "parent",
        "uncommitted",
        "aborted",
    ] {
        let w = World::agent("");
        let r = cf::member(
            &w,
            ID,
            json!({"parentPane":if mode=="denied" {"%88"} else {"%1"}}),
        );
        let folder = w.folder(ID);
        w.fakes.hc_install("hc");
        if mode == "uncommitted" {
            std::fs::remove_file(folder.join("commit.json")).unwrap();
        }
        if mode == "aborted" {
            let path = folder.join("commit.json");
            w.fakes.lock_probe(move |event| {
                if event.starts_with("+notice|") {
                    write(&path, json!({"aborted":true}));
                }
                String::new()
            });
        }
        if mode == "identity" || mode == "parent" {
            let path = folder.join("record.json");
            let mut changed = r.clone();
            changed[if mode == "identity" {
                "hcAddress"
            } else {
                "parentPane"
            }] = json!("changed");
            w.fakes.lock_probe(move |event| {
                if event.starts_with("+notice|") {
                    write(&path, changed.clone());
                }
                String::new()
            });
        }
        if mode == "failure" || mode == "uncertain" {
            cf::online(&w);
            let result = if mode == "failure" {
                Ok(crate::spawn::Out {
                    code: 1,
                    ..Default::default()
                })
            } else {
                Err(crate::error::Refusal::internal("timeout"))
            };
            w.fakes
                .hc_expect(&["kill", "taaaaaaaaaaaa", "--json"], result);
        }
        assert!(call(&w).is_err(), "{mode}");
        assert!(!folder.join("notice.json").exists());
        assert!(!w.state().join("mail/pane-1.jsonl").exists());
        w.fakes.hc_done().unwrap();
    }
}

#[test]
fn stop_rejects_uncommitted_member_during_named_selection() {
    let w = World::agent("");
    cf::member(&w, ID, json!({"parentPane":"%1"}));
    let folder = w.folder(ID);
    std::fs::remove_file(folder.join("commit.json")).unwrap();
    w.fakes.hc_install("hc");
    let error = call(&w).unwrap_err();
    assert_eq!(error.check, "target");
    assert!(w.fakes.locks().is_empty());
    assert!(w.fakes.hc_calls().is_empty());
    assert!(!folder.join("notice.json").exists());
    assert!(!w.state().join("mail/pane-1.jsonl").exists());
    w.fakes.hc_done().unwrap();
}
