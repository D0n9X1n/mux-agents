use super::*;
use crate::agents::{
    core_fixture as cf,
    fixture::{ID, World, write},
    membership,
};
use serde_json::json;
#[test]
fn hc_board_fields_append() {
    for (case, state, life) in [
        ("working", "running", "running"),
        ("blocked", "running", "running"),
        ("done", "running", "running"),
        ("idle", "running", "running"),
        ("lost", "lost", "lost"),
        ("unknown", "unknown", "unknown"),
        ("exited", "exited 7", "ended"),
        ("null", "exited", "ended"),
        ("killed", "stopped", "ended"),
    ] {
        for watch in [false, true] {
            let w = World::human("");
            let c = w.caller();
            let r = cf::member(&w, ID, json!({}));
            let mut t = cf::wire(&r);
            if life == "ended" {
                t["state"] = json!("finished");
                t["reason"] = json!(if case == "killed" { "killed" } else { "exited" });
            }
            if case == "exited" {
                t["exit_code"] = json!(7);
            }
            if case == "lost" {
                t["argv"] = json!(["/bin/sh", "-l"]);
            }
            if case == "unknown" {
                t["state"] = json!("finished");
                t["reason"] = json!("server_stop");
            }
            cf::online(&w);
            cf::listing(&w, &membership::team(&c), &[]);
            cf::get(&w, "taaaaaaaaaaaa", t);
            w.panes(&[]);
            let s = membership::Snapshot::take(&w.ctx, &c).unwrap();
            let o = &s.facts.local[0];
            let since = "1970-01-01T00:00:00.0000000Z";
            write(
                &w.folder(ID).join("activity.json"),
                json!({"state":if ["done", "idle"].contains(&case) {"idle"} else {case}, "since":since}),
            );
            if case == "idle" {
                write(&w.folder(ID).join("seen.json"), json!({"since":since}));
            }
            let rows = Rows {
                ctx: &w.ctx,
                caller: &c,
                unread: HashMap::new(),
                reader: Some("pane-1".into()),
                running: &[],
                now: w.ctx.clock.now(),
                watch,
            };
            let mut base = rows
                .row(
                    ID,
                    &r,
                    "%9".into(),
                    "@5",
                    if life == "running" { "running" } else { state },
                )
                .unwrap();
            if life == "ended" {
                base.insert("closedBy".into(), json!("operator"));
                base.insert("exitCode".into(), Value::Null);
            }
            let keys: Vec<_> = base
                .keys()
                .cloned()
                .chain(["life", "view", "hcAddress"].map(str::to_string))
                .collect();
            let view = o
                .identity
                .view(
                    &r,
                    &[registry::Row {
                        pane: "%9".into(),
                        window: "@8".into(),
                        pid: "300".into(),
                        dead: false,
                        command: r["startCommand"].as_str().unwrap().into(),
                        tag: String::new(),
                        name: String::new(),
                    }],
                )
                .unwrap();
            w.fakes.clear_log();
            for v in [None, Some(&view)] {
                let got = hc_row(base.clone(), o, v);
                assert_eq!(got.keys().cloned().collect::<Vec<_>>(), keys);
                let mut expected = base.clone();
                expected.insert("state".into(), json!(state));
                expected.insert("pane".into(), v.map_or(Value::Null, |v| json!(v.pane)));
                expected.insert("window".into(), v.map_or(Value::Null, |v| json!(v.window)));
                if life == "ended" {
                    expected.insert(
                        "exitCode".into(),
                        if case == "exited" {
                            json!(7)
                        } else {
                            Value::Null
                        },
                    );
                }
                expected.extend([
                    ("life".into(), json!(life)),
                    (
                        "view".into(),
                        json!(if v.is_some() { "attached" } else { "detached" }),
                    ),
                    ("hcAddress".into(), json!("taaaaaaaaaaaa")),
                ]);
                assert_eq!(
                    serde_json::to_string(&got).unwrap(),
                    serde_json::to_string(&expected).unwrap()
                );
                if life == "running" {
                    assert_eq!(got["activity"], case);
                }
            }
            assert!(w.fakes.mux_calls().is_empty() && w.fakes.locks().is_empty());
            w.fakes.hc_done().unwrap();
        }
    }
}
