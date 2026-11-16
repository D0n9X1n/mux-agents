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
                leader: None,
                parents: &HashMap::new(),
                logical: &Default::default(),
                marked: &[],
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

// S7, R1 (af434f IMPORTANT 10; audit C10): a non-running lifecycle member's notice is its generation's, from the
// membership snapshot; a torn one refuses `evidence`, never shown as none.
#[test]
fn the_board_shows_the_active_generations_notice() {
    use crate::{agents::gens, state::evidence::publish};
    let world = |torn: bool| {
        let w = World::human("");
        let c = w.caller();
        let r = cf::member(
            &w,
            ID,
            json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd"}),
        );
        let f = w.folder(ID);
        publish(&gens::path(&f, 1, "commit"), &json!({"committed": true})).unwrap();
        write(
            &f.join("notice.json"),
            json!({"kind": "lost", "closedBy": "folder"}),
        );
        publish(
            &gens::path(&f, 0, "notice"),
            &json!({"kind": "lost", "closedBy": "gen0"}),
        )
        .unwrap();
        if torn {
            std::fs::write(gens::path(&f, 1, "notice"), b"{\"ki").unwrap();
        } else {
            publish(
                &gens::path(&f, 1, "notice"),
                &json!({"kind": "lost", "closedBy": "pane"}),
            )
            .unwrap();
        }
        let mut t = cf::wire(&r);
        t["argv"] = json!(["/bin/sh", "-l"]);
        cf::online(&w);
        cf::listing(&w, &membership::team(&c), &[]);
        cf::get(&w, r["hcAddress"].as_str().unwrap(), t);
        w.panes(&[]);
        (w, c)
    };
    let (w, c) = world(false);
    let got = board(&w.ctx, &c, false, false).unwrap();
    assert_eq!(
        got["agents"][0]["closedBy"], "pane",
        "neither the folder's nor generation 0's notice"
    );
    let (w, c) = world(true);
    assert_eq!(
        board(&w.ctx, &c, false, false).map_err(|e| e.check).err(),
        Some("evidence"),
        "torn notice"
    );
}

// a3b call 42 (TL #145 F1): the first `list` after a natural exit of a member launched through the production adapter
// reaps its recorded end and renders it: no row in a plain list; one Ended row in `list -All`. No bar or reaper pass
// runs between the exit and the list. Two snapshot rounds are queued: the reap records the end, then the board reads
// the member again.
#[test]
fn the_first_list_after_a_natural_exit_shows_the_recorded_end() {
    use crate::agents::{
        gens,
        spawn::{CoreRequest, CoreSlot, launch_hc},
    };
    for all in [false, true] {
        let w = World::human("");
        let c = w.caller();
        w.fakes.hc_install("herdr-core");
        cf::answer(&w, &["--version"], "hc 0.1.21");
        w.fakes.hc_answer(Ok(crate::spawn::Out {
            stdout: r#"{"address":"t1"}"#.into(),
            ..Default::default()
        }));
        for verb in ["new-window", "split-window"] {
            w.fakes.mux_rule(verb, 0, "%9|@5|exact,#} attach t1\n", "");
        }
        w.launches_hold();
        w.fakes
            .mux_rule("-t %9 #{pane_id}|#{@agent-id}", 0, "|\n", "");
        let request = CoreRequest {
            client: crate::parse::Client::Claude,
            task: "task",
            brief: None,
            directory: &w.ctx.cwd,
            model: None,
            no_fork: true,
            effort: None,
            split: None,
            resume: None,
            leader: None,
        };
        let r = launch_hc(&w.ctx, &c, request, |_| {
            Ok(CoreSlot {
                name: "worker".into(),
                color: "blue",
                placed: None,
                debit: false,
            })
        })
        .unwrap();
        let id = r["id"].as_str().unwrap().to_string();
        let f = w.folder(&id);
        let rec = crate::state::fs::read_json(&f.join("record.json"))
            .unwrap()
            .unwrap();
        let mut t = cf::wire(&rec);
        (t["name"], t["state"], t["reason"], t["exit_code"]) = (
            rec["hcName"].clone(),
            json!("finished"),
            json!("exited"),
            json!(5),
        );
        for _ in 0..2 {
            cf::online(&w);
            cf::listing(&w, &membership::team(&c), &[]);
            cf::get(&w, "t1", t.clone());
        }
        w.panes(&[]);
        let got = board(&w.ctx, &c, all, false).unwrap();
        let rows: Vec<&Value> = got["agents"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["name"] == "worker")
            .collect();
        if all {
            assert_eq!(rows.len(), 1, "{all}: {got:?}");
            assert_eq!(
                (&rows[0]["life"], &rows[0]["state"], &rows[0]["exitCode"]),
                (&json!("ended"), &json!("exited 5"), &json!(5)),
                "{all}: {got:?}"
            );
            assert_eq!(got["endedTotal"], json!(1), "{all}: {got:?}");
        } else {
            assert!(
                rows.is_empty(),
                "{all}: a recorded end leaves the list: {got:?}"
            );
        }
        assert_eq!(got["running"], json!(0), "{all}");
        assert!(gens::read(&f, 0, "ended").known().unwrap().is_some());
        assert!(gens::read(&f, 0, "notice").known().unwrap().is_some());
        w.fakes.hc_done().unwrap();
    }
}
