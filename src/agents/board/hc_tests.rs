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
                    &crate::agents::core_identity::Key::of(&c),
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

// outcome-2 Task 6 fixture: a lifecycle member `id` named `name` at active generation 1, claim `run`, commit, launch.json,
// plus `extra` files; hc shows it Lost (POSIX recovery shell) or, with `ended`, finished exit 1. Answers its record and
// its hc wire.
fn life(w: &World, id: &str, name: &str, extra: &[(&str, Value)], ended: bool) -> (Value, Value) {
    use crate::{agents::gens, state::evidence::publish};
    let hc = format!("m{}", &id[4..]);
    let r = cf::member(
        w,
        id,
        json!({"lifecycle": 1, "generation": "1", "hcName": hc, "name": name}),
    );
    let f = w.folder(id);
    gens::publish_claim_once(&f, 1, &json!("run")).unwrap();
    publish(&gens::path(&f, 1, "commit"), &json!({"committed": true})).unwrap();
    let launch =
        json!({"hcName": hc, "launcherArgv": r["launcherArgv"], "sessionId": r["sessionId"]});
    publish(&gens::path(&f, 1, "launch"), &launch).unwrap();
    for (file, body) in extra {
        publish(&gens::path(&f, 1, file), body).unwrap();
    }
    let mut t = cf::wire(&r);
    t["name"] = json!(hc);
    if ended {
        t["state"] = json!("finished");
        t["reason"] = json!("exited");
        t["exit_code"] = json!(1);
    } else {
        t["argv"] = json!(["/bin/sh", "-l"]);
    }
    (r, t)
}
/// `ended.json` of the member `r` (first facts stand: group B's `record_end` keeps it).
fn end_facts(r: &Value) -> Value {
    json!({"address": r["hcAddress"], "hcName": r["hcName"], "labels": {}, "state": "finished", "reason": "exited",
        "exitCode": 1, "at": "x"})
}
/// Member `id` Ended by recorded evidence (its `ended.json`), so it stays Ended past hc forgetting it.
fn recorded_end(w: &World, id: &str, r: &Value) {
    let at = crate::agents::gens::path(&w.folder(id), 1, "ended");
    crate::state::evidence::publish(&at, &end_facts(r)).unwrap();
}
fn script(w: &World, members: &[&(Value, Value)]) {
    cf::online(w);
    cf::listing(w, &membership::team(&w.caller()), &[]);
    for (r, t) in members {
        cf::get(w, r["hcAddress"].as_str().unwrap(), t.clone());
    }
    w.panes(&[]);
}
fn row<'a>(got: &'a Map<String, Value>, name: &str) -> &'a Value {
    got["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .unwrap_or(&Value::Null)
}
fn flags(row: &Value) -> (&Value, &Value, &Value, &Value) {
    (
        &row["needsRecovery"],
        &row["recoveryReason"],
        &row["recoveryRunning"],
        &row["recoveryError"],
    )
}
fn exit_1() -> (&'static str, Value) {
    (
        "exit",
        json!({"exitCode": 1, "reason": "exited", "exitedAt": "x"}),
    )
}
fn outcome_ok() -> (&'static str, Value) {
    ("outcome", json!({"outcome": "ok", "at": "x"}))
}
fn torn_outcome(w: &World, id: &str) {
    let at = crate::agents::gens::path(&w.folder(id), 1, "outcome");
    std::fs::write(at, b"{\"out").unwrap();
}

// O6, ambiguity 14, finding #12: a failed active generation is flagged with its reason and `recoveryRunning`
// (Status.active_done); a torn outcome.json is `recoveryError: "evidence"` on that row only. The healthy sibling's row
// carries no recovery field either way, and the board still answers.
#[test]
fn the_board_flags_a_failed_active_generation_for_recovery() {
    use crate::agents::outcome::EXITED;
    for torn in [false, true] {
        let w = World::human("");
        let bad = life(&w, "aaaaaaaaaaaa", "bad", &[exit_1()], false);
        let good = life(&w, "bbbbbbbbbbbb", "good", &[outcome_ok()], false);
        if torn {
            torn_outcome(&w, "aaaaaaaaaaaa");
        }
        script(&w, &[&bad, &good]);
        let got = board(&w.ctx, &w.caller(), false, false).unwrap();
        let want = if torn {
            (&Value::Null, &Value::Null, &Value::Null, &json!("evidence"))
        } else {
            (&json!(true), &json!(EXITED), &json!(true), &Value::Null)
        };
        assert_eq!(flags(row(&got, "bad")), want, "torn {torn}: bad");
        let none = (&Value::Null, &Value::Null, &Value::Null, &Value::Null);
        assert_eq!(
            flags(row(&got, "good")),
            none,
            "torn {torn}: the healthy sibling"
        );
    }
}

// Ambiguity 14, finding #13: an Ended member that needs recovery, or whose recovery cannot be decided (torn
// outcome.json → `recoveryError`), stays on the board without -All.
#[test]
fn an_ended_member_that_needs_recovery_or_has_an_error_stays_on_the_board() {
    use crate::agents::outcome::EXITED;
    for torn in [false, true] {
        let w = World::human("");
        let e = life(&w, "aaaaaaaaaaaa", "e", &[exit_1()], true);
        recorded_end(&w, "aaaaaaaaaaaa", &e.0);
        if torn {
            torn_outcome(&w, "aaaaaaaaaaaa");
        }
        script(&w, &[&e]);
        let got = board(&w.ctx, &w.caller(), false, false).unwrap();
        assert!(
            !row(&got, "e").is_null(),
            "torn {torn}: listed without -All"
        );
        let want = if torn {
            (&Value::Null, &Value::Null, &Value::Null, &json!("evidence"))
        } else {
            (&json!(true), &json!(EXITED), &json!(false), &Value::Null)
        };
        assert_eq!(flags(row(&got, "e")), want, "torn {torn}");
    }
}

// Call 11, finding #3: a lifecycle record's stored `needsRecovery: true` is ignored (its active generation is ok);
// only `needs_recovery` decides. (A pre-lifecycle record keeps the stored flag: main's board.rs test, unchanged.)
#[test]
fn a_stale_stored_flag_on_a_lifecycle_record_is_ignored() {
    let w = World::human("");
    let m = life(&w, "aaaaaaaaaaaa", "stale", &[outcome_ok()], false);
    let at = w.folder("aaaaaaaaaaaa").join("record.json");
    let mut r: Value = serde_json::from_slice(&std::fs::read(&at).unwrap()).unwrap();
    r["needsRecovery"] = json!(true);
    write(&at, r);
    script(&w, &[&m]);
    let got = board(&w.ctx, &w.caller(), false, true).unwrap();
    assert_eq!(
        row(&got, "stale")["needsRecovery"],
        Value::Null,
        "stale stored flag"
    );
}

// Call 12, finding #4: active generation 1 failed, pending generation 2 unfinished and uncommitted (a held subject,
// `Stored.committed` false): recovery comes from the active generation before the committed-subject filter, so the
// member is listed as needing recovery.
#[test]
fn a_held_member_whose_active_generation_failed_is_listed() {
    use crate::agents::outcome::EXITED;
    let w = World::human("");
    let (r, _) = life(&w, "aaaaaaaaaaaa", "held", &[exit_1()], false);
    let at = w.folder("aaaaaaaaaaaa").join("record.json");
    let mut stored: Value = serde_json::from_slice(&std::fs::read(&at).unwrap()).unwrap();
    stored["pending"] = json!({"gen": 2, "kind": "resume", "hcName": "m00000002", "sessionId": r["sessionId"],
        "launcherArgv": r["launcherArgv"], "hcAddress": "t2"});
    write(&at, stored);
    let mut t2 = cf::wire(&r);
    t2["address"] = json!("t2");
    t2["name"] = json!("m00000002");
    t2["labels"]["mux.gen"] = json!("2");
    let held = (json!({"hcAddress": "t2"}), t2);
    script(&w, &[&held]);
    let got = board(&w.ctx, &w.caller(), false, false).unwrap();
    assert!(!row(&got, "held").is_null(), "held member listed");
    let (needs, reason, running, _) = flags(row(&got, "held"));
    assert_eq!(
        (needs, reason, running),
        (&json!(true), &json!(EXITED), &json!(true))
    );
}

// Call 13, finding #5: eleven Ended members that need recovery are all listed (exempt from ENDED_MAX = 10).
#[test]
fn every_ended_member_that_needs_recovery_is_listed_past_ended_max() {
    let w = World::human("");
    let all: Vec<(Value, Value)> = (0..11)
        .map(|k| {
            let id = format!("{:012x}", 0xa00 + k);
            let m = life(&w, &id, &format!("e{k}"), &[exit_1()], true);
            recorded_end(&w, &id, &m.0);
            m
        })
        .collect();
    script(&w, &all.iter().collect::<Vec<_>>());
    let got = board(&w.ctx, &w.caller(), false, false).unwrap();
    let rows = got["agents"].as_array().unwrap();
    assert_eq!(rows.len(), 11, "every member that needs recovery is listed");
    assert!(rows.iter().all(|r| r["needsRecovery"] == true), "{rows:?}");
}

// Build checklist 7 (call 13): eleven Ended members whose recovery cannot be decided (torn outcome.json) are all
// listed, each with `recoveryError` (errors are exempt from ENDED_MAX too, not only decided recoveries).
#[test]
fn every_ended_member_with_a_recovery_error_is_listed_past_ended_max() {
    let w = World::human("");
    let all: Vec<(Value, Value)> = (0..11)
        .map(|k| {
            let id = format!("{:012x}", 0xa00 + k);
            let m = life(&w, &id, &format!("e{k}"), &[exit_1()], true);
            recorded_end(&w, &id, &m.0);
            torn_outcome(&w, &id);
            m
        })
        .collect();
    script(&w, &all.iter().collect::<Vec<_>>());
    let got = board(&w.ctx, &w.caller(), false, false).unwrap();
    let rows = got["agents"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        11,
        "every member with a recovery error is listed"
    );
    assert!(
        rows.iter().all(|r| r["recoveryError"] == "evidence"),
        "{rows:?}"
    );
}

// Ambiguity 3: the board passes the terminal it observed; generation 1's Lost recovery shell fails it (no deadline).
#[test]
fn the_board_passes_its_observation_as_lost_evidence() {
    use crate::agents::outcome::LOST;
    let w = World::human("");
    let m = life(&w, "aaaaaaaaaaaa", "lost", &[], false);
    script(&w, &[&m]);
    let got = board(&w.ctx, &w.caller(), false, false).unwrap();
    let (needs, reason, _, _) = flags(row(&got, "lost"));
    assert_eq!((needs, reason), (&json!(true), &json!(LOST)));
}

/// Member `id`'s `activity.json` (the live row's activity word).
fn activity(w: &World, id: &str, state: &str) {
    write(
        &w.folder(id).join("activity.json"),
        json!({"state": state, "since": "1970-01-01T00:00:00.0000000Z"}),
    );
}

// o2 calls 28, 30 (Tech Lead #146): from the board through the flat bar's renderer. A failed Running member whose
// activity is `working` beside a healthy `blocked` sibling (width 60, height 40: section headings) still shows
// `recover` under its section; the failed member alone at width 30 too.
#[test]
fn the_flat_bar_shows_recover_from_the_board_in_every_layout() {
    for (case, width, sibling) in [
        ("sections", 60, true),
        ("narrow", 30, false),
        // o2 call 34: the pill outlasts the name.
        ("narrowest", 16, false),
    ] {
        let w = World::human("");
        let mut bad = life(&w, "aaaaaaaaaaaa", "fixer", &[exit_1()], false);
        bad.1["argv"] = bad.0["launcherArgv"].clone();
        activity(&w, "aaaaaaaaaaaa", "working");
        let mut members = vec![bad];
        if sibling {
            let mut good = life(&w, "bbbbbbbbbbbb", "good", &[outcome_ok()], false);
            good.1["argv"] = good.0["launcherArgv"].clone();
            activity(&w, "bbbbbbbbbbbb", "blocked");
            members.push(good);
        }
        script(&w, &members.iter().collect::<Vec<_>>());
        let got = board(&w.ctx, &w.caller(), false, true).unwrap();
        let fixer = row(&got, "fixer");
        assert_eq!(
            (
                &fixer["state"],
                &fixer["needsRecovery"],
                &fixer["recoveryRunning"]
            ),
            (&json!("running"), &json!(true), &json!(true)),
            "{case}: the board flags a Running member"
        );
        let text = crate::sidebar::flat_text(&got, width, 40);
        let line = text
            .iter()
            .skip(1)
            .find(|l| l.contains("fi"))
            .cloned()
            .unwrap_or_default();
        assert!(line.contains("recover"), "{case}: {text:#?}");
        if sibling {
            assert!(
                text.iter().any(|l| l.contains("Working")),
                "{case}: no section heading: {text:#?}"
            );
        }
    }
}

// o2 call 34: from the board through the flat bar: a Lost member that needs recovery at width 16, and a Running one
// with 12 unread lines at width 20, each keep the `recover` pill (the name is cut, the badge yields, before it goes).
#[test]
fn the_flat_bar_keeps_recover_at_the_narrowest_widths() {
    for (case, width, running, unread) in
        [("lost", 16, false, 0), ("running, unread 12", 20, true, 12)]
    {
        let w = World::human("");
        let caller = w.caller();
        let mut bad = life(&w, "aaaaaaaaaaaa", "fixer", &[exit_1()], false);
        if running {
            bad.1["argv"] = bad.0["launcherArgv"].clone();
            activity(&w, "aaaaaaaaaaaa", "working");
        }
        // Its row is keyed `fixer|<pane>`; with no view a lifecycle member's presented pane is empty.
        for _ in 0..unread {
            crate::agents::mail::add_mail(&w.ctx, &caller, "pane-1", "fixer", "", "x", None)
                .unwrap();
        }
        script(&w, &[&bad]);
        let got = board(&w.ctx, &caller, false, false).unwrap();
        let fixer = row(&got, "fixer");
        assert_eq!(
            (&fixer["needsRecovery"], &fixer["unread"]),
            (&json!(true), &json!(unread)),
            "{case}: the board's row"
        );
        let text = crate::sidebar::flat_text(&got, width, 40);
        let line = text
            .iter()
            .skip(1)
            .find(|l| l.contains("recover"))
            .cloned()
            .unwrap_or_default();
        assert!(line.contains("recover"), "{case}: {text:#?}");
    }
}
