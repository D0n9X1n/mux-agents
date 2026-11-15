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

fn notice_once(w: &World, r: &Value, n: Notice) -> Res<bool> {
    let c = w.caller();
    let key = crate::agents::lock_key("notice", &c, ID);
    crate::agents::lock(&w.ctx, &key, || add_notice(&w.ctx, &c, ID, r, n))
}

// S5 (af434f CRITICAL 6): each generation has its own delivery id and marker.
#[test]
fn a_later_generation_never_reuses_an_earlier_delivery_id() {
    use crate::agents::gens;
    let w = World::human("");
    let r0 = cf::member(&w, ID, json!({"lifecycle": 1, "generation": "0"}));
    let f = w.folder(ID);
    let end = json!({"state": "finished"});
    crate::state::evidence::publish(&gens::path(&f, 0, "ended"), &end).unwrap(); // R3: each notice waits for it
    assert!(notice_once(&w, &r0, Notice::Stopped).unwrap());
    let mut r1 = r0.clone();
    (r1["generation"], r1["hcName"]) = (json!("1"), json!("m0123abcd"));
    crate::state::fs::write_json(&f.join("record.json"), &r1).unwrap();
    crate::state::evidence::publish(&gens::path(&f, 1, "ended"), &end).unwrap();
    assert!(notice_once(&w, &r1, Notice::Exited(json!(3))).unwrap());
    let ids: Vec<Value> = messages(&w).iter().map(|m| m["id"].clone()).collect();
    assert_eq!(
        ids,
        [json!(format!("exit-{ID}-0")), json!(format!("exit-{ID}-1"))]
    );
    let kind = |n| read_json(&gens::path(&f, n, "notice")).unwrap().unwrap()["kind"].clone();
    assert_eq!(
        (kind(0), kind(1)),
        (json!("stopped"), json!("exited")),
        "1 never takes 0's message"
    );
    assert!(!f.join("notice.json").exists(), "never the folder marker");
}

// R2 (ambiguities 9, 16; audit B9): a noticed generation posts nothing; a torn marker or an unreadable record
// refuses and posts nothing; no active generation has nothing to notice.
#[test]
fn a_noticed_or_unreadable_generation_posts_nothing() {
    use crate::agents::gens;
    for (case, want) in [
        ("noticed", Ok(false)),
        ("torn", Err("evidence")),
        ("01", Err("internal")),
        ("pending", Ok(false)),
    ] {
        let w = World::human("");
        let fields = match case {
            "01" => json!({"lifecycle": 1, "generation": "01"}),
            "pending" => json!({"lifecycle": 1, "pending": {"gen": 1}}),
            _ => json!({"lifecycle": 1, "generation": "0"}),
        };
        let mut r = cf::member(&w, ID, fields);
        if case == "pending" {
            // cf::member defaults generation "0"; a pending-only record has none.
            r.as_object_mut().unwrap().remove("generation");
            crate::state::fs::write_json(&w.folder(ID).join("record.json"), &r).unwrap();
        }
        let f = w.folder(ID);
        std::fs::create_dir_all(gens::dir(&f, 0)).unwrap();
        if ["noticed", "torn"].contains(&case) {
            // R3: generation 0 is done, so only the notice marker decides these two cases.
            crate::state::evidence::publish(
                &gens::path(&f, 0, "ended"),
                &json!({"state": "finished"}),
            )
            .unwrap();
        }
        match case {
            "noticed" => {
                crate::state::fs::write_json(&gens::path(&f, 0, "notice"), &json!({"kind": "lost"}))
                    .unwrap()
            }
            "torn" => std::fs::write(gens::path(&f, 0, "notice"), b"{\"ki").unwrap(),
            _ => {}
        }
        assert_eq!(
            notice_once(&w, &r, Notice::Stopped).map_err(|e| e.check),
            want,
            "{case}"
        );
        assert!(messages(&w).is_empty(), "{case}: no message");
        assert!(
            !f.join("notice.json").exists(),
            "{case}: never the folder marker"
        );
    }
}

// R3, PM call 7 (re-audit ad8b738 CRITICAL): a lifecycle notice waits for its generation's recorded completion;
// `completing.gen` holds it; an unreadable completion marker refuses.
#[test]
fn a_lifecycle_notice_waits_for_its_recorded_completion() {
    use crate::{agents::gens, state::evidence::publish};
    let w = World::human("");
    let mut r = cf::member(
        &w,
        ID,
        json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd"}),
    );
    let f = w.folder(ID);
    publish(&gens::path(&f, 1, "commit"), &json!({"committed": true})).unwrap();
    assert!(
        !notice_once(&w, &r, Notice::Exited(json!(3))).unwrap(),
        "nothing recorded: no notice"
    );
    publish(&gens::path(&f, 1, "ended"), &json!({"state": "finished"})).unwrap();
    r["completing"] = json!({"gen": 1});
    crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
    assert!(
        !notice_once(&w, &r, Notice::Exited(json!(3))).unwrap(),
        "completing.gen 1 holds it"
    );
    r.as_object_mut().unwrap().remove("completing");
    crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
    assert!(
        notice_once(&w, &r, Notice::Exited(json!(3))).unwrap(),
        "recorded end: posted"
    );
    assert_eq!(messages(&w)[0]["id"], json!(format!("exit-{ID}-1")));
    let w = World::human("");
    let r = cf::member(
        &w,
        ID,
        json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd"}),
    );
    std::fs::create_dir_all(gens::dir(&w.folder(ID), 1)).unwrap();
    std::fs::write(gens::path(&w.folder(ID), 1, "ended"), b"{\"st").unwrap();
    assert_eq!(
        notice_once(&w, &r, Notice::Stopped)
            .map_err(|e| e.check)
            .err(),
        Some("evidence"),
        "torn ended"
    );
    assert!(messages(&w).is_empty(), "nothing posted on a guess");
}

/// A committed lifecycle member at generation 1 whose view is pane %9, with a folder notice and a folder
/// launcher exit 9, generation 0's exit 8, generation 1's `exit` and (when `ended`) its recorded end; its pane gone.
/// Answers its snapshot Agent.
fn gone_life(w: &World, exit: Option<&[u8]>, ended: bool) -> Agent {
    use crate::{
        agents::{
            fixture::{launch_command, write},
            gens,
        },
        state::evidence::publish,
    };
    let r = cf::member(
        w,
        ID,
        json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd"}),
    );
    let f = w.folder(ID);
    write(&f.join("notice.json"), json!({"kind": "stopped"}));
    write(&f.join("launcher.json"), json!({"exitCode": 9}));
    publish(&gens::path(&f, 0, "exit"), &json!({"exitCode": 8})).unwrap();
    publish(&gens::path(&f, 1, "commit"), &json!({"committed": true})).unwrap();
    publish(
        &gens::path(&f, 1, "launch"),
        &json!({"hcName": "m0123abcd"}),
    )
    .unwrap();
    write(
        &f.join("view.json"),
        json!({"pane": "%9", "window": "@5", "startCommand": launch_command("%9"),
        "gen": 1, "hcAddress": r["hcAddress"], "hcName": "m0123abcd"}),
    );
    if let Some(bytes) = exit {
        std::fs::write(gens::path(&f, 1, "exit"), bytes).unwrap();
    }
    if ended {
        publish(&gens::path(&f, 1, "ended"), &json!({"state": "finished"})).unwrap();
    }
    w.panes(&[]);
    Snapshot::take(&w.ctx, &w.caller())
        .unwrap()
        .agents
        .into_iter()
        .find(|a| a.id == ID)
        .unwrap()
}

// R1, R2, R3 (audits B2, B6, B7; re-audit ad8b738 CRITICAL): post_if_gone posts for the snapshot's generation, and
// only once its completion is recorded; a promotion between the snapshot and the post never moves the notice; a torn
// active exit refuses, never Lost; the folder notice says nothing.
#[test]
fn a_gone_lifecycle_member_posts_its_snapshot_generations_exit() {
    use crate::agents::gens;
    let w = World::human("");
    let agent = gone_life(&w, Some(br#"{"exitCode": 3}"#), false);
    w.fakes.mux_script("#{pane_id}|#{@agent-id}", |_| {
        Ok(crate::mux::Output {
            code: 0,
            stdout: "|\n".into(),
            stderr: String::new(),
        })
    });
    assert!(
        !post_if_gone(&w.ctx, &w.caller(), &agent).unwrap(),
        "exit recorded, completion not: no notice"
    );
    assert!(messages(&w).is_empty(), "R3: the exit alone ends nothing");
    let w = World::human("");
    let agent = gone_life(&w, Some(br#"{"exitCode": 3}"#), true);
    let f = w.folder(ID);
    let promote = f.clone();
    w.fakes.mux_script("#{pane_id}|#{@agent-id}", move |_| {
        let mut r = crate::state::fs::read_json(&promote.join("record.json"))
            .unwrap()
            .unwrap();
        r["generation"] = json!("2");
        crate::state::fs::write_json(&promote.join("record.json"), &r).unwrap();
        Ok(crate::mux::Output {
            code: 0,
            stdout: "|\n".into(),
            stderr: String::new(),
        })
    });
    assert!(
        post_if_gone(&w.ctx, &w.caller(), &agent).unwrap(),
        "the folder notice says nothing"
    );
    let m = read_json(&gens::path(&f, 1, "notice")).unwrap().unwrap();
    assert_eq!(
        (m["kind"].clone(), m["exitCode"].clone()),
        (json!("exited"), json!(3))
    );
    assert_eq!(messages(&w)[0]["id"], json!(format!("exit-{ID}-1")));
    assert!(!gens::dir(&f, 2).exists(), "never generation 2's");
    let w = World::human("");
    let agent = gone_life(&w, Some(b"{\"exi"), true);
    w.fakes.mux_script("#{pane_id}|#{@agent-id}", |_| {
        Ok(crate::mux::Output {
            code: 0,
            stdout: "|\n".into(),
            stderr: String::new(),
        })
    });
    let e = post_if_gone(&w.ctx, &w.caller(), &agent).unwrap_err();
    assert_eq!(e.check, "evidence", "a torn exit refuses");
    assert!(messages(&w).is_empty(), "never Lost on a guess");
}

// R2 (ambiguity 10; audits B1, B8): the listing poll asks the active generation; any Unreadable lists.
#[test]
fn worth_listing_reads_the_active_generation() {
    use crate::{
        agents::{fixture::write, gens},
        state::evidence::publish,
    };
    let later = "2999-01-01T00:00:00.0000000Z";
    let world = || {
        let w = World::human("");
        let f = w.uncommitted(
            ID,
            json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd",
            "createdAt": later}),
        );
        write(&f.join("commit.json"), json!({"committedAt": "x"}));
        publish(&gens::path(&f, 0, "commit"), &json!({"committed": true})).unwrap();
        (w, f)
    };
    let (w, f) = world();
    let c = w.caller();
    assert!(
        !worth_listing(&w.ctx, &c).unwrap(),
        "neither the folder's nor generation 0's commit"
    );
    publish(&gens::path(&f, 1, "commit"), &json!({"committed": true})).unwrap();
    write(&f.join("notice.json"), json!({"kind": "stopped"}));
    assert!(
        worth_listing(&w.ctx, &c).unwrap(),
        "generation 1 committed, not noticed"
    );
    publish(&gens::path(&f, 1, "notice"), &json!({"kind": "stopped"})).unwrap();
    write(&f.join("launcher.json"), json!({"exitCode": 9}));
    publish(&gens::path(&f, 0, "exit"), &json!({"exitCode": 8})).unwrap();
    assert!(
        !worth_listing(&w.ctx, &c).unwrap(),
        "noticed; neither the folder's nor generation 0's exit"
    );
    publish(&gens::path(&f, 1, "exit"), &json!({"exitCode": 3})).unwrap();
    assert!(worth_listing(&w.ctx, &c).unwrap(), "generation 1 exited");
    for file in ["commit", "notice", "exit"] {
        let (w, f) = world();
        std::fs::create_dir_all(gens::dir(&f, 1)).unwrap();
        std::fs::write(gens::path(&f, 1, file), b"{\"x").unwrap();
        assert!(
            worth_listing(&w.ctx, &w.caller()).unwrap(),
            "a torn {file} lists"
        );
    }
    let (w, f) = world();
    let mut r = read_json(&f.join("record.json")).unwrap().unwrap();
    r["generation"] = json!("01");
    write(&f.join("record.json"), r);
    assert!(
        worth_listing(&w.ctx, &w.caller()).unwrap(),
        "an unreadable record lists"
    );
}

// R3 (ambiguity 17; final re-audit): hc's finished terminal posts no notice for a lifecycle member, before and after
// its completion is recorded; the second case passes `post`'s completion gate, so only `reap_hc`'s own guard holds.
#[test]
fn reap_posts_nothing_for_a_lifecycle_member() {
    use crate::agents::gens;
    let w = World::human("");
    let c = w.caller();
    let r = cf::member(
        &w,
        ID,
        json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd"}),
    );
    crate::state::evidence::publish(
        &gens::path(&w.folder(ID), 1, "commit"),
        &json!({"committed": true}),
    )
    .unwrap();
    let mut t = cf::wire(&r);
    (t["state"], t["reason"], t["exit_code"]) = (json!("finished"), json!("exited"), json!(0));
    cf::online(&w);
    cf::listing(&w, &membership::team(&c), &[]);
    cf::get(&w, r["hcAddress"].as_str().unwrap(), t.clone());
    w.panes(&[]);
    let s = membership::Snapshot::take(&w.ctx, &c).unwrap();
    assert!(!reap_hc(&w.ctx, &c, &s).unwrap(), "nothing recorded");
    assert!(messages(&w).is_empty() && !gens::path(&w.folder(ID), 1, "notice").exists());
    assert!(!w.folder(ID).join("notice.json").exists());
    crate::state::evidence::publish(
        &gens::path(&w.folder(ID), 1, "ended"),
        &json!({"state": "finished"}),
    )
    .unwrap();
    cf::online(&w);
    cf::listing(&w, &membership::team(&c), &[]);
    cf::get(&w, r["hcAddress"].as_str().unwrap(), t);
    w.panes(&[]);
    let s = membership::Snapshot::take(&w.ctx, &c).unwrap();
    assert!(
        !reap_hc(&w.ctx, &c, &s).unwrap(),
        "recorded end: reap_hc still posts no lifecycle notice"
    );
    assert!(
        messages(&w).is_empty() && !gens::path(&w.folder(ID), 1, "notice").exists(),
        "recorded end: nothing"
    );
}
