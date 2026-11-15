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
// `session`. The wait is bounded and polls on the injected clock, after every lock is released.
fn stopping(w: &World, lifecycle: bool) {
    let r = cf::member(
        w,
        ID,
        if lifecycle {
            json!({"lifecycle": 1})
        } else {
            json!({})
        },
    );
    if lifecycle {
        let path = super::gens::path(&w.folder(ID), 0, "client");
        crate::state::evidence::publish(&path, &json!({"pid": 150, "start": 1500})).unwrap();
    } else {
        cf::proven(w, ID, 31337, 150);
    }
    w.fakes.process(crate::testing::row(150, 31337, 1500));
    cf::online(w);
    cf::answer(w, &["kill", "taaaaaaaaaaaa", "--json"], "{}");
    let mut t = cf::wire(&r);
    t["state"] = json!("finished");
    t["reason"] = json!("killed");
    cf::get(w, "taaaaaaaaaaaa", t);
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
        assert_eq!(
            got, r#"{"name":"worker","hcAddress":"taaaaaaaaaaaa","life":"ended"}"#,
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
    // A client that reappears under its pid with another start is not the recorded one: it has exited.
    let w = World::human("");
    stopping(&w, false);
    w.fakes.forget(150);
    w.fakes.process(crate::testing::row(150, 1, 9999));
    let got = call(&w).unwrap();
    assert_eq!((got.get("clientExited"), w.fakes.slept()), (None, vec![]));
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
