use super::{
    core_fixture as cf,
    fixture::{ID, World},
    gens, life_fixture as lf, registry,
};
use crate::agents::cascade_fixture as cx;
use crate::agents::team_state::Scope;
use serde_json::{Value, json};
fn call(w: &World) -> crate::error::Res<serde_json::Map<String, Value>> {
    let args = ["stop", "worker"].map(str::to_string);
    let (def, request) = crate::cli::parse(&args).unwrap();
    (def.handler)(&w.ctx, &request)
}

/// `stop <name>` for an `lf` member, by its record name.
fn stop_of(w: &World, m: &std::path::Path) -> crate::error::Res<serde_json::Map<String, Value>> {
    let name = lf::record(m)["name"].as_str().unwrap().to_string();
    let args = ["stop".to_string(), name];
    let (def, request) = crate::cli::parse(&args).unwrap();
    (def.handler)(&w.ctx, &request)
}

/// Active generation 0 at `t0`, with the top-level identity `current` validates (a3c call 36).
fn at0() -> Value {
    json!({"generation": "0", "hcAddress": "t0", "hcName": lf::hc_name(0), "launcherArgv": lf::ARGV})
}

/// B1's merged `terminate` of a Running generation n at `t<n>` (call 13, b-call 54): `--version`, the search (`ls`,
/// `get`), the exact reread (`get`), then `kill`, answered or refused. Nothing follows the kill.
fn kills(w: &World, n: u64, accepted: bool) {
    let a = format!("t{n}");
    cf::online(w);
    lf::found(w, n, &lf::running(w, n));
    lf::get(w, &a, &lf::running(w, n));
    if accepted {
        cf::answer(w, &["kill", &a, "--json"], "{}");
    } else {
        lf::fails(w, &["kill", &a, "--json"]);
    }
}

// PM calls 9, 11 (finding 5to8-3): the stop ends generation n through terminate and the notice is n. Held cases:
// pending 1 committed and running is stopped and noticed, never the active 0, whether 0 already ended (no notice) or
// still runs (call 32, checklist 6: `post` cannot decide generation 1 from the active generation's `done`).
#[test]
fn a_lifecycle_stop_ends_and_notices_the_generation_it_stopped() {
    for case in ["active", "held, active ended", "held, active running"] {
        let (w, _s) = lf::world();
        let held = case != "active";
        let mut fields = at0();
        if held {
            fields["pending"] = lf::pending(1);
        }
        let m = lf::life(&w, fields);
        lf::seed_gen(&m, 0, Some("run"), lf::committed());
        if case == "held, active ended" {
            gens::publish_once(&m, 0, "ended", &json!({"state": "finished"})).unwrap();
        }
        let n = if held {
            lf::seed_gen(&m, 1, Some("run"), lf::committed());
            1
        } else {
            0
        };
        kills(&w, n, true);
        if case == "held, active running" {
            kills(&w, 0, true); // cascade-3d ambiguity 4: every generation, newest first
        }
        let got = stop_of(&w, &m).unwrap();
        assert_eq!(
            (&got["life"], &got["hcAddress"]),
            (&json!("ended"), &json!(format!("t{n}"))),
            "{case}"
        );
        let end = gens::read(&m, n, "ended").known().unwrap().unwrap();
        assert_eq!(
            end["reason"],
            json!("terminated"),
            "{case}: the end of the stopped generation"
        );
        let notice = gens::read(&m, n, "notice").known().unwrap();
        assert_eq!(
            notice.map(|v| v["kind"].clone()),
            Some(json!("stopped")),
            "{case}: generation {n} is noticed"
        );
        if held {
            assert!(
                gens::read(&m, 0, "notice").known().unwrap().is_none(),
                "{case}: the active 0 is not this stop"
            );
            assert!(
                lf::record(&m).get("pending").is_none(),
                "{case}: terminate dropped pending 1"
            );
        }
        assert!(
            !m.join("notice.json").exists(),
            "{case}: never the folder marker"
        );
        let want = if case == "held, active running" { 2 } else { 1 };
        assert_eq!(lf::count(&w, "kill"), want, "{case}");
        w.fakes.hc_done().unwrap();
    }
}

// PM call 11, release-1d R2: a Lost generation is released, never killed; the reply says lost; the notice posts.
#[test]
fn a_lost_generation_is_released_by_stop_never_killed() {
    let (w, _s) = lf::world();
    let m = lf::life(&w, at0());
    lf::seed_gen(&m, 0, Some("run"), lf::committed());
    cf::online(&w);
    lf::found(&w, 0, &lf::lost(&w, 0, lf::LOST_ARGVS[0]));
    lf::get(&w, "t0", &lf::lost(&w, 0, lf::LOST_ARGVS[0]));
    lf::relabel(&w, "t0");
    let got = stop_of(&w, &m).unwrap();
    assert_eq!(got["life"], json!("lost"));
    assert_eq!(
        gens::read(&m, 0, "release").known().unwrap().unwrap()["cause"],
        "lost"
    );
    assert_eq!(
        lf::count(&w, "kill"),
        0,
        "a Lost generation is never killed"
    );
    let notice = gens::read(&m, 0, "notice").known().unwrap();
    assert_eq!(
        notice.map(|v| v["kind"].clone()),
        Some(json!("stopped")),
        "the released generation is noticed"
    );
    w.fakes.hc_done().unwrap();
}

// D3 Err (finding 5to8-7, call site): hc refuses the kill (b-call 54: no `get` follows a kill, so an accepted kill
// always records the end). Unresolved: the stop refuses, no end is recorded, no notice posts.
#[test]
fn an_unresolved_stop_posts_no_notice() {
    let (w, _s) = lf::world();
    let m = lf::life(&w, at0());
    lf::seed_gen(&m, 0, Some("run"), lf::committed());
    kills(&w, 0, false);
    let reply = stop_of(&w, &m).unwrap();
    assert_eq!(reply["stopped"], false);
    assert_eq!(reply["unresolved"][0]["check"], "hc");
    assert_eq!(reply["unresolved"][0]["name"], "worker");
    assert!(cx::journal_of(&w, ID).exists(), "kept for the next stop");
    assert!(gens::read(&m, 0, "ended").known().unwrap().is_none());
    assert!(
        gens::read(&m, 0, "notice").known().unwrap().is_none(),
        "no notice before the end is recorded"
    );
    w.fakes.hc_done().unwrap();
}

// R2 (finding 5to8-7, call site): an unreadable ended.json refuses before any hc call; no notice.
#[test]
fn a_stop_with_an_unreadable_end_refuses_before_any_kill() {
    let (w, _s) = lf::world();
    let m = lf::life(&w, at0());
    lf::seed_gen(&m, 0, Some("run"), lf::committed());
    std::fs::write(gens::path(&m, 0, "ended"), b"{").unwrap();
    w.fakes.hc_install("hc");
    let e = stop_of(&w, &m).unwrap_err();
    assert_eq!(e.check, "evidence", "{e:?}");
    assert!(
        lf::calls(&w).is_empty(),
        "nothing asked of hc on unreadable evidence"
    );
    assert!(gens::read(&m, 0, "notice").known().unwrap().is_none());
    w.fakes.hc_done().unwrap();
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
            // prelife-1c P-R3: the validating get (Running) is the last hc call before the kill.
            cf::get(&w, "taaaaaaaaaaaa", cf::wire(&r));
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
            assert!(
                !w.folder(ID).join("gens").exists(),
                "{mode}: a pre-lifecycle stop writes no generation file"
            );
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
            view_order: Vec::new(),
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
            held: Vec::new(),
            pre_ended: Default::default(),
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
            // prelife-1c P-R3: the validating get (Running) is the last hc call before the kill.
            cf::get(&w, "taaaaaaaaaaaa", cf::wire(&r));
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

#[test]
fn stop_ignores_view_and_launcher_process_evidence() {
    for (pid, command) in [
        ("0", "sh"),
        ("1", "sh"),
        ("300", "sh"),
        ("", "hc attach taaaaaaaaaaaa"),
    ] {
        let w = World::human("");
        let r = cf::member(&w, ID, json!({}));
        let mut row = super::fixture::row_of(&format!("%9|@5|{pid}|bbbbbbbbbbbb|other"), &[]);
        row.command = command.into();
        row.dead = pid.is_empty();
        w.pane_rows(&[row]);
        write(
            &w.folder(ID).join("launcher.json"),
            json!({"pid":300,"start":999,"exitCode":7}),
        );
        cf::online(&w);
        // prelife-1c P-R3: the validating get (Running) is the last hc call before the kill.
        cf::get(&w, "taaaaaaaaaaaa", cf::wire(&r));
        cf::answer(&w, &["kill", "taaaaaaaaaaaa", "--json"], "{}");
        let mut t = cf::wire(&r);
        t["state"] = json!("finished");
        t["reason"] = json!("killed");
        cf::get(&w, "taaaaaaaaaaaa", t);
        w.fakes.clear_log();
        assert_eq!(
            Value::Object(call(&w).unwrap()).to_string(),
            r#"{"name":"worker","hcAddress":"taaaaaaaaaaaa","life":"ended"}"#
        );
        assert_eq!(w.fakes.mux_calls().len(), 1);
        assert!(
            !w.fakes
                .os_calls()
                .iter()
                .any(|a| a == "table" || a.starts_with("kill ") || a == "open 300")
        );
        w.fakes.hc_done().unwrap();
    }
}
#[test]
fn stop_refuses_legacy_records_without_core_calls() {
    let w = World::human("");
    w.agent_record(ID, json!({}));
    w.fakes.hc_install("hc");
    let e = call(&w).unwrap_err();
    assert_eq!(
        (e.code, e.check, e.message.as_str()),
        (2, "target", "no unique committed member has that identity")
    );
    assert!(!w.folder(ID).join("notice.json").exists());
    assert!(w.fakes.hc_calls().is_empty());
    w.fakes.hc_done().unwrap();
}
#[test]
fn stop_notice_race_is_not_lifecycle_authority() {
    let w = World::human("");
    let r = cf::member(&w, ID, json!({}));
    let file = w.folder(ID).join("notice.json");
    let raced = file.clone();
    w.fakes.lock_probe(move |event| {
        if event.starts_with("+notice|") {
            write(&raced, json!({"kind":"exited","exitCode":7,"at":"x"}));
        }
        String::new()
    });
    cf::online(&w);
    // prelife-1c P-R3: the validating get (Running) is the last hc call before the kill.
    cf::get(&w, "taaaaaaaaaaaa", cf::wire(&r));
    cf::answer(&w, &["kill", "taaaaaaaaaaaa", "--json"], "{}");
    cf::get(&w, "taaaaaaaaaaaa", cf::wire(&r));
    assert_eq!(
        Value::Object(call(&w).unwrap()).to_string(),
        r#"{"name":"worker","hcAddress":"taaaaaaaaaaaa","life":"running"}"#
    );
    assert_eq!(
        std::fs::read_to_string(file).unwrap(),
        r#"{"kind":"exited","exitCode":7,"at":"x"}"#
    );
    assert!(!w.state().join("mail/pane-1.jsonl").exists());
    w.fakes.hc_done().unwrap();
}
#[test]
fn stop_resolves_reused_name_beyond_board_history_limit() {
    let w = World::human("");
    let a = cf::member(&w, ID, json!({"createdAt":"2026-11-13T10:00:00Z"}));
    let b = cf::member(
        &w,
        "bbbbbbbbbbbb",
        json!({"createdAt":"2026-11-13T10:01:00Z"}),
    );
    for n in 0..11 {
        cf::member(
            &w,
            &format!("{n:012x}"),
            json!({"name":format!("other{n}"),"createdAt":format!("2026-11-13T11:{n:02}:00Z")}),
        );
    }
    for mode in ["live", "ended"] {
        cf::online(&w);
        let mut old = cf::wire(&a);
        old["state"] = json!("finished");
        old["reason"] = json!("exited");
        let mut fresh = cf::wire(&b);
        if mode == "ended" {
            fresh["state"] = json!("finished");
            fresh["reason"] = json!("exited");
        }
        cf::get(&w, "taaaaaaaaaaaa", old);
        cf::get(&w, "tbbbbbbbbbbbb", fresh);
        cf::online(&w);
        // prelife-1c P-R3: the validating get (Running) is the last hc call before the kill.
        cf::get(&w, "tbbbbbbbbbbbb", cf::wire(&b));
        cf::answer(&w, &["kill", "tbbbbbbbbbbbb", "--json"], "{}");
        let mut killed = cf::wire(&b);
        killed["state"] = json!("finished");
        killed["reason"] = json!("killed");
        cf::get(&w, "tbbbbbbbbbbbb", killed);
        assert_eq!(
            Value::Object(call(&w).unwrap()).to_string(),
            r#"{"name":"worker","hcAddress":"tbbbbbbbbbbbb","life":"ended"}"#
        );
        w.fakes.hc_done().unwrap();
    }
}

// #137: `stop` replies once the member's recorded client has exited, so a `resume` right after it is not refused
// `session`. The wait is bounded and polls on the injected clock, after every lock is released. Checklist 4: a
// lifecycle member is seeded as group B builds it (claim `run`, `launch.json`, commit) and stopped through B1's
// merged `terminate` sequence; it answers at `t0`.
fn stopping(w: &World, lifecycle: bool) {
    if lifecycle {
        let m = lf::life(w, at0());
        lf::seed_gen(&m, 0, Some("run"), lf::committed());
        crate::state::evidence::publish(
            &gens::path(&m, 0, "client"),
            &json!({"pid": 150, "start": 1500}),
        )
        .unwrap();
        w.fakes.process(crate::testing::row(150, 31337, 1500));
        kills(w, 0, true);
        return;
    }
    let r = cf::member(w, ID, json!({}));
    cf::proven(w, ID, 31337, 150);
    w.fakes.process(crate::testing::row(150, 31337, 1500));
    cf::online(w);
    // prelife-1c P-R3: the validating get (Running) is the last hc call before the kill.
    cf::get(w, "taaaaaaaaaaaa", cf::wire(&r));
    cf::answer(w, &["kill", "taaaaaaaaaaaa", "--json"], "{}");
    let mut t = cf::wire(&r);
    t["state"] = json!("finished");
    t["reason"] = json!("killed");
    cf::get(w, "taaaaaaaaaaaa", t);
}

/// The address `stopping` answers at.
fn stopped_at(lifecycle: bool) -> &'static str {
    if lifecycle { "t0" } else { "taaaaaaaaaaaa" }
}

#[test]
fn stop_replies_once_the_recorded_client_has_exited() {
    for lifecycle in [false, true] {
        let w = World::human("");
        stopping(&w, lifecycle);
        let (f, member) = (
            w.fakes.clone(),
            format!("member|{}|{ID}", w.caller().server_key),
        );
        w.fakes.sleep_probe(move |n| {
            // No lock is held while it waits.
            assert_eq!(
                f.locks()
                    .iter()
                    .rev()
                    .find(|e| e.ends_with(member.as_str())),
                Some(&format!("-{member}"))
            );
            if n == 2 {
                f.forget(150);
            }
        });
        let got = Value::Object(call(&w).unwrap()).to_string();
        let at = stopped_at(lifecycle);
        // cascade-3d C9: a lifecycle stop also replies `stopped` and `unresolved`.
        let c9 = if lifecycle {
            r#","stopped":true,"unresolved":[]"#
        } else {
            ""
        };
        assert_eq!(
            got,
            format!(r#"{{"name":"worker","hcAddress":"{at}","life":"ended"{c9}}}"#),
            "lifecycle {lifecycle}"
        );
        assert_eq!(w.fakes.slept(), [250, 250], "lifecycle {lifecycle}");
        w.fakes.hc_done().unwrap();
    }
}

#[test]
fn stop_past_the_bound_still_replies_ok_with_client_exited_false() {
    let w = World::human("");
    stopping(&w, false);
    let got = call(&w).unwrap();
    assert_eq!(got["clientExited"], json!(false));
    assert_eq!(got["life"], json!("ended"));
    let slept: u64 = w.fakes.slept().iter().sum();
    assert_eq!(slept, 8000);
    w.fakes.hc_done().unwrap();
    // A client that reappears under its pid with another start is not the recorded one: it has exited.
    let w = World::human("");
    stopping(&w, false);
    w.fakes.forget(150);
    w.fakes.process(crate::testing::row(150, 1, 9999));
    let got = call(&w).unwrap();
    assert_eq!((got.get("clientExited"), w.fakes.slept()), (None, vec![]));
    w.fakes.hc_done().unwrap();
}

#[test]
fn stop_with_an_unreadable_process_table_waits_out_the_bound() {
    // A table that cannot be read cannot show the client gone: it waits the whole bound, then replies.
    let w = World::human("");
    stopping(&w, false);
    w.fakes.table_fails();
    let got = call(&w).unwrap();
    assert_eq!(got["clientExited"], json!(false));
    assert_eq!(w.fakes.slept().iter().sum::<u64>(), 8000);
    w.fakes.hc_done().unwrap();
}

// Call 9: the recorded client is read as typed evidence. Unreadable evidence (torn JSON, a folder in its place) cannot
// prove the client exited: `stop` replies at once, with `clientExited: false`, and never waits.
#[test]
fn stop_with_unreadable_client_evidence_replies_client_exited_false_at_once() {
    for (case, lifecycle) in [
        ("torn", false),
        ("torn", true),
        ("folder", false),
        ("folder", true),
    ] {
        let w = World::human("");
        stopping(&w, lifecycle);
        let at = if lifecycle {
            super::gens::path(&w.folder(ID), 0, "client")
        } else {
            w.folder(ID).join("launcher.json")
        };
        std::fs::remove_file(&at).unwrap();
        if case == "torn" {
            std::fs::write(&at, b"{\"pid\": 15").unwrap();
        } else {
            std::fs::create_dir(&at).unwrap();
        }
        let got = call(&w).unwrap();
        assert_eq!(
            (got.get("clientExited"), w.fakes.slept()),
            (Some(&json!(false)), vec![]),
            "{case} lifecycle {lifecycle}"
        );
        w.fakes.hc_done().unwrap();
    }
}

// Call 9: an absent client.json recorded no client: no wait and no `clientExited`.
#[test]
fn stop_with_no_recorded_client_neither_waits_nor_adds_client_exited() {
    let w = World::human("");
    stopping(&w, true);
    std::fs::remove_file(super::gens::path(&w.folder(ID), 0, "client")).unwrap();
    let got = call(&w).unwrap();
    assert_eq!((got.get("clientExited"), w.fakes.slept()), (None, vec![]));
    w.fakes.hc_done().unwrap();
}

// Call 9, Unix: a FIFO at client.json is never opened, so `stop` returns promptly. A watchdog releases a reader that
// blocks on the FIFO after 2 s (a non-blocking writer open fails at once with no reader, so it never blocks itself).
#[cfg(unix)]
#[test]
fn stop_with_a_fifo_at_client_json_returns_promptly() {
    use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::{Duration, Instant};
    let w = World::human("");
    stopping(&w, true);
    let at = super::gens::path(&w.folder(ID), 0, "client");
    std::fs::remove_file(&at).unwrap();
    let c = std::ffi::CString::new(at.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo");
    let done = Arc::new(AtomicBool::new(false));
    let (stop, fifo) = (done.clone(), at.clone());
    let dog = std::thread::spawn(move || {
        let start = Instant::now();
        while !stop.load(Ordering::SeqCst) {
            if start.elapsed() > Duration::from_secs(2) {
                let _ = std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&fifo);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let begun = Instant::now();
    let got = call(&w).unwrap();
    done.store(true, Ordering::SeqCst);
    dog.join().unwrap();
    assert!(
        begun.elapsed() < Duration::from_secs(2),
        "stop blocked on the FIFO"
    );
    assert_eq!(got.get("clientExited"), Some(&json!(false)));
    w.fakes.hc_done().unwrap();
}

// Call 10: a row with the recorded pid whose start cannot be read is unresolved, never exited: `stop` waits out the
// bound, then replies `clientExited: false`.
#[test]
fn stop_waits_out_the_bound_for_a_row_with_an_unknown_start() {
    let w = World::human("");
    stopping(&w, false);
    w.fakes.forget(150);
    w.fakes.process(crate::testing::unknown_start(150, 31337));
    let got = call(&w).unwrap();
    assert_eq!(got.get("clientExited"), Some(&json!(false)));
    assert_eq!(w.fakes.slept().iter().sum::<u64>(), 8000);
    w.fakes.hc_done().unwrap();
}

// A held identity (A3a R4): a pending generation not yet done is the one `stop` targets and terminates, so its
// client, not the active generation's, is the one `stop` waits for. Checklist 4: group B's seeding and B1's merged
// `terminate` of pending 1 at `t1`.
#[test]
fn stop_of_a_held_generation_waits_for_that_generations_client() {
    let w = World::human("");
    let mut fields = at0();
    fields["pending"] = lf::pending(1);
    let m = lf::life(&w, fields);
    lf::seed_gen(&m, 0, Some("run"), lf::committed());
    lf::seed_gen(&m, 1, Some("run"), lf::committed());
    crate::state::evidence::publish(&gens::path(&m, 0, "ended"), &json!({"at": "x"})).unwrap();
    crate::state::evidence::publish(
        &gens::path(&m, 1, "client"),
        &json!({"pid": 160, "start": 1600}),
    )
    .unwrap();
    w.fakes.process(crate::testing::row(160, 31337, 1600));
    kills(&w, 1, true);
    let f2 = w.fakes.clone();
    w.fakes.sleep_probe(move |n| {
        if n == 2 {
            f2.forget(160);
        }
    });
    let got = call(&w).unwrap();
    assert_eq!(
        (
            got.get("hcAddress"),
            got.get("clientExited"),
            w.fakes.slept()
        ),
        (Some(&json!("t1")), None, vec![250, 250])
    );
    w.fakes.hc_done().unwrap();
}

// a3b call 38: a lifecycle stop inside a tree. `with_standing` holds the session spawn lock (the team lock), so
// `stop_life` terminates under it through `team_state::held`, never taking it a second time; generation 0 is ended and
// noticed as outside a tree.
#[test]
fn a_lifecycle_stop_inside_a_tree_terminates_under_the_held_team_lock() {
    let (w, _s) = lf::world();
    crate::agents::tree::tests::team_record(&w, &[]);
    let m = lf::life(&w, at0());
    lf::seed_gen(&m, 0, Some("run"), lf::committed());
    kills(&w, 0, true);
    let got = stop_of(&w, &m).unwrap();
    assert_eq!(
        (&got["life"], &got["hcAddress"]),
        (&json!("ended"), &json!("t0"))
    );
    let spawn = format!("+{}", crate::agents::lock_key("spawn", &w.caller(), "$3"));
    let taken = w.fakes.locks().iter().filter(|l| **l == spawn).count();
    assert_eq!(
        taken,
        1,
        "the team lock is taken once, by with_standing: {:?}",
        w.fakes.locks()
    );
    let end = gens::read(&m, 0, "ended").known().unwrap().unwrap();
    assert_eq!(end["reason"], json!("terminated"));
    let notice = gens::read(&m, 0, "notice").known().unwrap();
    assert_eq!(notice.map(|v| v["kind"].clone()), Some(json!("stopped")));
    w.fakes.hc_done().unwrap();
}

/// A finished one-member journal of S (stopped, ended and noticed), as a cut-short stop leaves it.
fn kept_s(w: &World) {
    cx::member(w, cx::S, None, 0, json!({}));
    cx::done(w, cx::S);
    let s = Scope::of(&w.caller());
    crate::agents::team_state::open(&w.ctx, &s, |tx| tx.desired(cx::S, false)).unwrap();
    cx::seed(w, cx::S, &[cx::S], true);
}

/// A lifecycle `worker` with generation 1 committed at `t1`.
fn at1(w: &World) -> std::path::PathBuf {
    let m = lf::life(
        w,
        json!({"generation": "1", "hcAddress": "t1", "hcName": lf::hc_name(1),
        "launcherArgv": lf::ARGV}),
    );
    lf::seed_gen(&m, 1, Some("run"), lf::committed());
    m
}

// cascade-3d C9: a lifecycle stop replies `stopped: true`, `unresolved: []`, and leaves no journal.
#[test]
fn a_lifecycle_stop_replies_stopped_and_unresolved() {
    let (w, _) = lf::world();
    let m = at1(&w);
    kills(&w, 1, true);
    let reply = stop_of(&w, &m).unwrap();
    let got = (&reply["stopped"], &reply["unresolved"]);
    assert_eq!(got, (&json!(true), &json!([])));
    assert_eq!(reply["life"], "ended");
    assert!(!cx::journal_of(&w, ID).exists());
    w.fakes.hc_done().unwrap();
}

// cascade-3d C8: stop runs every journal of the team first, under the team lock, before it names its target: a kept
// journal of S is finished even when the target is refused.
#[test]
fn stop_finishes_every_journal_before_naming_its_target() {
    let (w, _) = lf::world();
    kept_s(&w);
    let got = super::message::stop_hc(&w.ctx, &w.request(&["stop", "nobody"]));
    assert!(got.is_err(), "{got:?}");
    assert!(
        !cx::journal_of(&w, cx::S).exists(),
        "S's journal finished first"
    );
}

// state-0 S9: from the stop on (the setup's own holds excluded), the team lock comes first (`finish` before naming),
// no team hold nests in another, and every member hold sits inside a team hold (the notice included).
#[test]
fn stop_takes_each_lock_once_team_first() {
    let (w, s) = lf::world();
    kept_s(&w);
    let m = at1(&w);
    kills(&w, 1, true);
    let before = w.fakes.locks().len();
    stop_of(&w, &m).unwrap();
    let log = w.fakes.locks()[before..].to_vec();
    let (take, give) = (format!("+{}", s.team_lock()), format!("-{}", s.team_lock()));
    assert_eq!(log[0], take, "the team lock first: {log:?}");
    let mut held = false;
    for e in &log {
        if *e == take {
            assert!(!held, "no nested team hold: {log:?}");
            held = true;
        } else if *e == give {
            held = false;
        } else if e.starts_with("+member|") {
            assert!(held, "every member hold inside a team hold: {e}: {log:?}");
        }
    }
    w.fakes.hc_done().unwrap();
}
