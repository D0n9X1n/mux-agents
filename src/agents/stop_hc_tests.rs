use super::{
    core_fixture as cf,
    fixture::{ID, World},
    message, registry,
};
use serde_json::{Value, json};
fn call(w: &World) -> crate::error::Res<serde_json::Map<String, Value>> {
    message::stop_hc(&w.ctx, &w.request(&["stop", "worker"]))
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
