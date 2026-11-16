use super::*;
use crate::agents::lifecycle::Life;
use crate::agents::{
    core_fixture as f,
    fixture::{ID, World, write},
};
use serde_json::json;

#[test]
fn local_members_ignore_legacy_without_writes() {
    let w = World::human("");
    let caller = w.caller();
    w.fakes.clear_log();
    assert!(locals(&w.ctx, &caller).unwrap().is_empty());
    assert!(!w.ctx.state_root.exists());
    w.agent_record("bbbbbbbbbbbb", json!({}));
    f::member(&w, ID, json!({}));
    f::member(
        &w,
        "cccccccccccc",
        json!({"session":"$4","team":"other:$4"}),
    );
    let folder = w.folder(ID);
    std::fs::remove_file(folder.join("commit.json")).unwrap();
    let before = std::fs::read(folder.join("record.json")).unwrap();
    w.fakes.clear_log();
    let local = locals(&w.ctx, &caller).unwrap();
    assert_eq!(
        local
            .iter()
            .map(|l| l.identity.id.as_str())
            .collect::<Vec<_>>(),
        [ID]
    );
    assert!(!local[0].stored.committed);
    assert!(!local[0].stored.noticed);
    assert_eq!(std::fs::read(folder.join("record.json")).unwrap(), before);
    assert!(w.fakes.hc_calls().is_empty());
    assert!(w.fakes.mux_calls().is_empty());
    assert!(w.fakes.locks().is_empty());
    for value in [json!([]), json!({"hcAddress":"t1"})] {
        write(&folder.join("record.json"), value);
        assert!(locals(&w.ctx, &caller).is_err());
    }
    std::fs::write(folder.join("record.json"), "{").unwrap();
    assert!(locals(&w.ctx, &caller).is_err());
}

#[test]
fn hc_snapshot_survives_view_loss() {
    for mode in [
        "view",
        "closed",
        "rotated",
        "replaced",
        "dead",
        "mux-error",
        "get-error",
        "missing-core",
        "old-core",
        "ls-error",
        "generation",
        "server-stop",
        "ended",
    ] {
        let w = World::human("");
        let r = f::member(&w, ID, json!({}));
        let caller = w.caller();
        let address = "taaaaaaaaaaaa";
        let mut terminal = f::wire(&r);
        match mode {
            "generation" => terminal["labels"]["mux.gen"] = json!("1"),
            "server-stop" => {
                terminal["state"] = json!("finished");
                terminal["reason"] = json!("server_stop");
            }
            "ended" => {
                terminal["state"] = json!("finished");
                terminal["reason"] = json!("exited");
                terminal["exit_code"] = json!(7);
            }
            _ => {}
        }
        if mode == "old-core" {
            w.fakes.hc_install("hc");
            f::answer(&w, &["--version"], "hc 0.1.20");
        } else if mode != "missing-core" {
            f::online(&w);
            if mode == "ls-error" {
                w.fakes.hc_expect(
                    &[
                        "ls",
                        "--all",
                        "--label",
                        "mux.managed=1",
                        "--label",
                        &format!("mux.team={}", team(&caller)),
                        "--json",
                    ],
                    Err(Refusal::internal("ls failed")),
                );
            } else {
                f::listing(&w, &team(&caller), &[address, address]);
                if mode == "get-error" {
                    w.fakes.hc_expect(
                        &["get", address, "--json"],
                        Err(Refusal::internal("get failed")),
                    );
                } else {
                    f::get(&w, address, terminal);
                }
            }
        }
        let mut row = crate::agents::fixture::row_of("%9|@5|300|bbbbbbbbbbbb|wrong", &[]);
        row.command = "hc attach taaaaaaaaaaaa".into();
        row.dead = mode == "dead";
        if mode == "replaced" {
            row.command = "human shell".into();
        }
        w.pane_rows(if mode == "closed" {
            &[]
        } else {
            std::slice::from_ref(&row)
        });
        if mode == "mux-error" {
            w.fakes.mux_first("list-panes", 1, "", "failed");
        }
        w.fakes.clear_log();
        let s = Snapshot::take(&w.ctx, &caller).unwrap();
        let life = match mode {
            "get-error" | "missing-core" | "old-core" | "ls-error" | "generation"
            | "server-stop" => Life::Unknown,
            "ended" => Life::Ended,
            _ => Life::Running,
        };
        assert_eq!(s.facts.local[0].life, life, "{mode}");
        assert_eq!(
            s.views.contains_key(ID),
            !["closed", "replaced", "dead", "mux-error"].contains(&mode),
            "{mode}"
        );
        assert_eq!(s.view_error.is_some(), mode == "mux-error");
        assert_eq!(
            s.core_error.is_some(),
            ["missing-core", "old-core", "ls-error"].contains(&mode)
        );
        let gets = usize::from(!["missing-core", "old-core", "ls-error"].contains(&mode));
        assert_eq!(
            w.fakes
                .hc_calls()
                .iter()
                .filter(|(_, a, _)| a[0] == "get")
                .count(),
            gets
        );
        assert!(
            !crate::agents::server_dir(&w.ctx, &caller)
                .join("agents")
                .join(ID)
                .join("notice.json")
                .exists()
        );
        assert!(w.fakes.locks().is_empty());
        assert_eq!(w.fakes.mux_calls().len(), 1);
        w.fakes.hc_done().unwrap();
    }
}

#[test]
fn spawn_snapshot_refuses_unverified_reservations() {
    for mode in [
        "local-get",
        "ls",
        "foreign-get",
        "foreign-team",
        "legacy-tags",
        "duplicate-address",
    ] {
        let w = World::human("");
        let r = f::member(&w, ID, json!({}));
        let caller = w.caller();
        if mode == "duplicate-address" {
            f::member(&w, "bbbbbbbbbbbb", json!({"hcAddress":"taaaaaaaaaaaa"}));
        }
        f::online(&w);
        let core = hc::connect(&w.ctx).unwrap();
        if mode == "ls" {
            w.fakes.hc_expect(
                &[
                    "ls",
                    "--all",
                    "--label",
                    "mux.managed=1",
                    "--label",
                    &format!("mux.team={}", team(&caller)),
                    "--json",
                ],
                Err(Refusal::internal("ls failed")),
            );
        } else if mode != "duplicate-address" {
            f::listing(
                &w,
                &team(&caller),
                if mode.starts_with("foreign") {
                    &["z1"]
                } else {
                    &[]
                },
            );
            if mode == "local-get" {
                w.fakes.hc_expect(
                    &["get", "taaaaaaaaaaaa", "--json"],
                    Err(Refusal::internal("gone")),
                );
            } else {
                f::get(&w, "taaaaaaaaaaaa", f::wire(&r));
            }
            if mode == "foreign-get" {
                w.fakes
                    .hc_expect(&["get", "z1", "--json"], Err(Refusal::internal("gone")));
            }
            if mode == "foreign-team" {
                let mut t = f::wire(&r);
                t["address"] = json!("z1");
                t["labels"]["mux.team"] = json!("other:$3");
                f::get(&w, "z1", t);
            }
        }
        w.panes(&["%8|@4|300|bbbbbbbbbbbb|tag-only"]);
        let result = Snapshot::for_spawn(&w.ctx, &caller, &core);
        if ["local-get", "legacy-tags"].contains(&mode) {
            let s = result.unwrap();
            assert_eq!(s.facts.count(), 1);
            assert!(s.facts.reserved.is_empty());
        } else {
            assert!(result.is_err(), "{mode}");
        }
        assert_eq!(
            w.fakes
                .hc_calls()
                .iter()
                .filter(|(_, a, _)| a[0] == "--version")
                .count(),
            1
        );
        w.fakes.hc_done().unwrap();
    }
}

#[test]
fn only_ended_releases_name_and_capacity() {
    for (mode, count) in [
        ("running", 6),
        ("lost", 6),
        ("unknown", 6),
        ("ended", 5),
        ("uncommitted", 6),
    ] {
        let w = World::human("");
        let caller = w.caller();
        let r = f::member(&w, ID, json!({}));
        if mode == "uncommitted" {
            std::fs::remove_file(w.folder(ID).join("commit.json")).unwrap();
        }
        for foreign in [false, true] {
            f::online(&w);
            f::listing(
                &w,
                &team(&caller),
                if foreign {
                    &["x5", "x1", "x2", "x3", "x4", "x1", "taaaaaaaaaaaa"]
                } else {
                    &[]
                },
            );
            let mut t = f::wire(&r);
            match mode {
                "lost" => t["argv"] = json!(["/bin/zsh", "-l"]),
                "unknown" => t["labels"]["mux.gen"] = json!("old"),
                "ended" => {
                    t["state"] = json!("finished");
                    t["reason"] = json!("killed");
                }
                _ => {}
            }
            f::get(&w, "taaaaaaaaaaaa", t);
            if foreign {
                for n in 1..=5 {
                    let mut t = f::wire(&r);
                    let a = format!("x{n}");
                    t["address"] = json!(a);
                    t["labels"]["mux.id"] = json!(format!("{n:012x}"));
                    t["labels"]["mux.name"] = json!(format!("other-{n}"));
                    f::get(&w, &a, t);
                }
            }
            w.fakes.clear_log();
            let s = Snapshot::take(&w.ctx, &caller).unwrap();
            assert_eq!(
                s.facts.count(),
                if foreign { count } else { count - 5 },
                "{mode}"
            );
            let got = s.admit_name(Some("worker"), crate::parse::Client::Claude, "task");
            if mode == "ended" {
                assert_eq!(got.unwrap(), "worker");
                if foreign {
                    assert_eq!(
                        s.admit_name(Some("other-1"), crate::parse::Client::Claude, "task")
                            .unwrap_err()
                            .message,
                        "agent name is taken, try other-1-2"
                    );
                }
            } else {
                assert_eq!(
                    got.unwrap_err().message,
                    if foreign {
                        "the session already has 6 live agents"
                    } else {
                        "agent name is taken, try worker-2"
                    }
                );
            }
            assert_eq!(
                w.fakes
                    .hc_calls()
                    .iter()
                    .filter(|(_, a, _)| a[0] == "get")
                    .count(),
                if foreign { 6 } else { 1 }
            );
            w.fakes.hc_done().unwrap();
        }
    }
    let w = World::human("");
    let caller = w.caller();
    f::member(&w, ID, json!({}));
    let s = Snapshot::take(&w.ctx, &caller).unwrap();
    assert!(
        s.admit_name(None, crate::parse::Client::Claude, "task")
            .is_err()
    );
    w.fakes.hc_done().unwrap();
}

#[test]
fn reader_uses_hc_life_commit_and_view_not_tags_or_notices() {
    for case in [
        "root",
        "tag-only",
        "running",
        "noticed",
        "uncommitted",
        "ended",
        "lost",
        "unknown",
        "detached",
        "duplicate",
        "view-error",
    ] {
        let w = World::human("");
        let mut caller = w.caller();
        let r = f::member(&w, ID, json!({"pane":"%1","window":"@1"}));
        let mut t = f::wire(&r);
        match case {
            "ended" => {
                t["state"] = json!("finished");
                t["reason"] = json!("exited");
            }
            "lost" => t["argv"] = json!(["/bin/sh"]),
            "unknown" => t["labels"]["mux.gen"] = json!("1"),
            "noticed" => write(&w.folder(ID).join("notice.json"), json!({"kind":"stopped"})),
            "uncommitted" => std::fs::remove_file(w.folder(ID).join("commit.json")).unwrap(),
            _ => {}
        }
        if ["root", "tag-only"].contains(&case) {
            caller.pane = "%2".into();
        }
        caller.agent_tag = if case == "tag-only" {
            ID.into()
        } else {
            "bbbbbbbbbbbb".into()
        };
        if case == "root" {
            caller.agent_tag.clear();
        }
        let mut row = crate::agents::fixture::row_of("%1|@1|100|bbbbbbbbbbbb|wrong", &[]);
        row.command = "hc attach taaaaaaaaaaaa".into();
        w.pane_rows(if case == "detached" {
            &[]
        } else {
            std::slice::from_ref(&row)
        });
        if case == "view-error" {
            w.fakes.mux_first("list-panes", 1, "", "down");
        }
        f::online(&w);
        f::listing(&w, &team(&caller), &[]);
        f::get(&w, "taaaaaaaaaaaa", t);
        if case == "duplicate" {
            let b = f::member(&w, "bbbbbbbbbbbb", json!({"pane":"%1","window":"@1"}));
            f::get(&w, "tbbbbbbbbbbbb", f::wire(&b));
        }
        let snapshot = Snapshot::take(&w.ctx, &caller).unwrap();
        let result = snapshot.reader(&caller);
        match case {
            "root" => assert_eq!(
                result.unwrap(),
                registry::Me {
                    name: "root".into(),
                    key: "pane-2".into(),
                    record: None
                }
            ),
            "running" | "noticed" => assert_eq!(
                result.unwrap(),
                registry::Me {
                    name: "worker".into(),
                    key: ID.into(),
                    record: Some(r)
                }
            ),
            "tag-only" => assert_eq!(result.unwrap_err().message, "this agent pane has no record"),
            "view-error" => assert_eq!(result.unwrap_err(), snapshot.view_error.unwrap()),
            _ => {
                let e = result.unwrap_err();
                assert_eq!(
                    (e.code, e.check, e.message.as_str()),
                    (
                        2,
                        "parent",
                        "this pane's agent cannot be proven to run here"
                    ),
                    "{case}"
                );
            }
        }
        assert!(w.fakes.locks().is_empty());
        w.fakes.hc_done().unwrap();
    }
}

#[test]
fn board_view_order_preserves_enumeration_without_promoting_tags() {
    for mode in ["ordered", "duplicate", "view-error"] {
        let w = World::human("");
        let a = f::member(&w, ID, json!({"pane":"%9"}));
        let b = f::member(&w, "bbbbbbbbbbbb", json!({"name":"second","pane":"%10"}));
        let c = f::member(&w, "cccccccccccc", json!({"name":"detached","pane":"%2"}));
        let mut rows = vec![
            crate::agents::fixture::row_of("%10|@7|301|aaaaaaaaaaaa|wrong", &[]),
            crate::agents::fixture::row_of("%9|@6|300|bbbbbbbbbbbb|wrong", &[]),
        ];
        rows[0].command = "hc attach tbbbbbbbbbbbb".into();
        rows[1].command = "hc attach taaaaaaaaaaaa".into();
        if mode == "duplicate" {
            rows.push(rows[1].clone());
        }
        w.pane_rows(&rows);
        if mode == "view-error" {
            w.fakes.mux_first("list-panes", 1, "", "down");
        }
        let caller = w.caller();
        f::online(&w);
        f::listing(&w, &team(&caller), &[]);
        for r in [&a, &b, &c] {
            f::get(&w, r["hcAddress"].as_str().unwrap(), f::wire(r));
        }
        w.fakes.clear_log();
        let snapshot = Snapshot::take(&w.ctx, &caller).unwrap();
        let want: &[&str] = match mode {
            "ordered" => &["bbbbbbbbbbbb", "aaaaaaaaaaaa"],
            "duplicate" => &["bbbbbbbbbbbb"],
            _ => &[],
        };
        assert_eq!(snapshot.view_order, want, "{mode}");
        assert!(!snapshot.views.contains_key("cccccccccccc"));
        assert_eq!(snapshot.facts.count(), 3);
        assert_eq!(w.fakes.mux_calls().len(), 1);
        assert!(crate::agents::fixture::list_call(&w.fakes.mux_calls()[0]));
        assert_eq!(
            w.fakes
                .hc_calls()
                .iter()
                .map(|(_, a, _)| a[0].as_str())
                .collect::<Vec<_>>(),
            ["--version", "ls", "get", "get", "get"]
        );
        w.fakes.hc_done().unwrap();
    }
}

// #104 phase 1c: a worker proven through its own hc terminal is that member, whichever pane shows it now: a closed
// or moved view, or a multiplexer that does not answer, never changes whose mailbox it reads.
#[test]
fn reader_takes_a_hc_proven_member_by_id_not_by_its_view() {
    for case in [
        "detached",
        "moved",
        "view-error",
        "ended",
        "uncommitted",
        "other",
    ] {
        let w = World::human("");
        let mut caller = w.caller();
        let r = f::member(&w, ID, json!({"pane":"%1","window":"@1"}));
        let mut t = f::wire(&r);
        if case == "ended" {
            t["state"] = json!("finished");
            t["reason"] = json!("exited");
        }
        if case == "uncommitted" {
            std::fs::remove_file(w.folder(ID).join("commit.json")).unwrap();
        }
        caller.member = Some(if case == "other" { "bbbbbbbbbbbb" } else { ID }.into());
        caller.agent_tag = ID.into();
        let mut row = crate::agents::fixture::row_of("%5|@1|100|bbbbbbbbbbbb|wrong", &[]);
        row.command = "hc attach taaaaaaaaaaaa".into();
        w.pane_rows(if case == "moved" {
            std::slice::from_ref(&row)
        } else {
            &[]
        });
        if case == "view-error" {
            w.fakes.mux_first("list-panes", 1, "", "down");
        }
        f::online(&w);
        f::listing(&w, &team(&caller), &[]);
        f::get(&w, "taaaaaaaaaaaa", t);
        let snapshot = Snapshot::take(&w.ctx, &caller).unwrap();
        let result = snapshot.reader(&caller);
        match case {
            "detached" | "moved" | "view-error" => assert_eq!(
                result.unwrap(),
                registry::Me {
                    name: "worker".into(),
                    key: ID.into(),
                    record: Some(r.clone())
                },
                "{case}"
            ),
            _ => {
                let e = result.unwrap_err();
                assert_eq!(
                    (e.code, e.check, e.message.as_str()),
                    (2, "parent", "this agent cannot be proven to run"),
                    "{case}"
                );
            }
        }
        assert!(w.fakes.locks().is_empty());
        w.fakes.hc_done().unwrap();
    }
}

// #104 phase 1c review: send, status and report take a proven member by id only while hc shows it running, its
// record committed and with no end notice; read alone accepts a noticed member. Never another member's id.
#[test]
fn member_is_the_proven_id_only_while_running_and_unnoticed() {
    for case in ["running", "noticed", "ended", "uncommitted", "other"] {
        let w = World::human("");
        let caller = w.caller();
        let r = f::member(&w, ID, json!({"pane":"%1","window":"@1"}));
        let mut t = f::wire(&r);
        match case {
            "ended" => {
                t["state"] = json!("finished");
                t["reason"] = json!("exited");
            }
            "noticed" => write(&w.folder(ID).join("notice.json"), json!({"kind":"stopped"})),
            "uncommitted" => std::fs::remove_file(w.folder(ID).join("commit.json")).unwrap(),
            _ => {}
        }
        w.pane_rows(&[]);
        f::online(&w);
        f::listing(&w, &team(&caller), &[]);
        f::get(&w, "taaaaaaaaaaaa", t);
        let snapshot = Snapshot::take(&w.ctx, &caller).unwrap();
        let id = if case == "other" { "bbbbbbbbbbbb" } else { ID };
        let strict = snapshot.member(id, false);
        let reading = snapshot.member(id, true);
        let me = registry::Me {
            name: "worker".into(),
            key: ID.into(),
            record: Some(r.clone()),
        };
        let refused = |result: Res<registry::Me>| {
            let e = result.unwrap_err();
            (e.code, e.check, e.message)
        };
        let no = (
            2,
            "parent",
            "this agent cannot be proven to run".to_string(),
        );
        match case {
            "running" => {
                assert_eq!(strict.unwrap(), me);
                assert_eq!(reading.unwrap(), me);
            }
            "noticed" => {
                assert_eq!(refused(strict), no);
                assert_eq!(reading.unwrap(), me);
            }
            _ => {
                assert_eq!(refused(strict), no, "{case}");
                assert_eq!(refused(reading), no, "{case}");
            }
        }
        w.fakes.hc_done().unwrap();
    }
}

const OTHER_SID: &str = "11111111-2222-4333-8444-555555555555";

/// hc's terminal at `address` labelled generation `n` of ID, `state` live (argv `claude`, not a launcher) or
/// finished (exited 0).
fn term(w: &World, address: &str, n: &str, state: &str) -> Value {
    let team = team(&w.caller());
    let mut t = json!({"address": address, "state": state, "argv": ["claude"], "labels": {"mux.managed": "1",
        "mux.team": team, "mux.id": ID, "mux.gen": n, "mux.name": "worker"}});
    if state == "finished" {
        (t["reason"], t["exit_code"]) = (json!("exited"), json!(0));
    }
    t
}

/// What admission (`for_spawn`) or the snapshot (`take`) observes: (count, lives, names).
fn observe(
    w: &World,
    spawn: bool,
    listed: &[&str],
    gets: &[(&str, Value)],
) -> Res<(usize, Vec<Life>, Vec<String>)> {
    let caller = w.caller();
    f::online(w);
    f::listing(w, &team(&caller), listed);
    for (address, t) in gets {
        f::get(w, address, t.clone());
    }
    w.panes(&[]);
    let s = if spawn {
        let core = crate::hc::connect(&w.ctx)?;
        Snapshot::for_spawn(&w.ctx, &caller, &core)?
    } else {
        Snapshot::take(&w.ctx, &caller)?
    };
    Ok((
        s.facts.count(),
        s.facts.local.iter().map(|o| o.life).collect(),
        s.facts.names(),
    ))
}

/// A lifecycle record with no active generation and `pending` (gen 1, hcName m0123abcd, sessionId OTHER_SID).
fn held_world(address: Option<&str>) -> World {
    let w = World::human("");
    let mut pending = json!({"gen": 1, "hcName": "m0123abcd", "sessionId": OTHER_SID});
    if let Some(a) = address {
        pending["hcAddress"] = json!(a);
    }
    w.uncommitted(ID, json!({"lifecycle": 1, "pending": pending}));
    w
}

// R3, R4 (audits A2, A6, af434f CRITICAL 1): a pending generation not done holds its name and slot.
#[test]
fn a_held_generation_keeps_its_name_and_slot_until_done() {
    let held = || (1, vec![Life::Unknown], vec!["worker".to_string()]);
    for spawn in [true, false] {
        let w = held_world(Some("t9"));
        let got = observe(&w, spawn, &[], &[("t9", term(&w, "t9", "1", "finished"))]).unwrap();
        assert_eq!(
            got,
            held(),
            "spawn {spawn}: hc's Ended never frees an undecided generation"
        );
    }
    let w = held_world(None);
    let got = observe(&w, true, &["t9"], &[("t9", term(&w, "t9", "1", "live"))]).unwrap();
    assert_eq!(
        got,
        held(),
        "before hc run's reply: found by its labels, never Running"
    );
    for spawn in [true, false] {
        let w = held_world(None);
        let got = observe(&w, spawn, &[], &[]).unwrap();
        assert_eq!(
            got,
            held(),
            "spawn {spawn}: before any terminal exists, it still holds its name and slot"
        );
    }
    let w = held_world(Some("t9"));
    let ok = json!({"committed": true});
    crate::state::evidence::publish(&crate::agents::gens::path(&w.folder(ID), 1, "commit"), &ok)
        .unwrap();
    let l = locals(&w.ctx, &w.caller()).unwrap();
    assert_eq!(
        (l[0].held, l[0].stored.committed),
        (true, true),
        "committed, not promoted: its own commit"
    );
    let w = held_world(Some("t9"));
    let ab = json!({"aborted": true});
    crate::state::evidence::publish(&crate::agents::gens::path(&w.folder(ID), 1, "commit"), &ab)
        .unwrap();
    assert_eq!(
        observe(&w, true, &[], &[]).unwrap(),
        (0, vec![], vec![]),
        "void: no member"
    );
    let w = held_world(Some("t9"));
    std::fs::create_dir_all(crate::agents::gens::dir(&w.folder(ID), 1)).unwrap();
    std::fs::write(
        crate::agents::gens::path(&w.folder(ID), 1, "commit"),
        b"{\"abo",
    )
    .unwrap();
    assert_eq!(
        observe(&w, true, &[], &[]).map_err(|e| e.check).err(),
        Some("evidence"),
        "torn: refused"
    );
}

// R4 (audit A1): a held identity is its pending generation's alone, never the active one's commit or view.
#[test]
fn a_held_identity_is_its_pending_generation_only() {
    let w = World::human("");
    let mut r = f::member(&w, ID, json!({"lifecycle": 1, "generation": "0"}));
    let folder = w.folder(ID);
    crate::state::evidence::publish(
        &crate::agents::gens::path(&folder, 0, "commit"),
        &json!({"committed": true}),
    )
    .unwrap();
    r["pending"] =
        json!({"gen": 1, "hcName": "m0123abcd", "sessionId": OTHER_SID, "hcAddress": "t9"});
    write(&folder.join("record.json"), r.clone());
    // A valid view of active generation 0, so a held identity that presented would show it (row 2.8).
    let name = format!("m{}", &ID[..8]);
    crate::state::evidence::publish(
        &crate::agents::gens::path(&folder, 0, "launch"),
        &json!({"hcName": name}),
    )
    .unwrap();
    write(
        &folder.join("view.json"),
        json!({"pane": "%9", "window": "@5", "startCommand": "hc attach x",
        "gen": 0, "hcAddress": r["hcAddress"], "hcName": name}),
    );
    let one = || locals(&w.ctx, &w.caller()).unwrap().remove(0);
    let l = one();
    let i = &l.identity;
    let seen = (
        i.generation.as_str(),
        i.address.as_str(),
        i.session_id.as_str(),
        l.held,
        l.stored.committed,
    );
    assert_eq!(
        seen,
        ("1", "t9", OTHER_SID, true, false),
        "never generation 0's commit"
    );
    assert!(i.launcher.is_empty(), "never Running");
    assert_eq!(
        l.stored.record.get("pane"),
        None,
        "the active generation's view is not the held identity's"
    );
    let ok = json!({"committed": true});
    crate::state::evidence::publish(&crate::agents::gens::path(&folder, 1, "commit"), &ok).unwrap();
    assert!(one().stored.committed, "its own commit");
}

// R3 (audit A2): an active generation ends only by its recorded end; once recorded, hc forgetting it changes nothing.
#[test]
fn an_active_generation_ends_only_by_its_recorded_end() {
    let w = World::human("");
    let r = f::member(
        &w,
        ID,
        json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd"}),
    );
    let address = r["hcAddress"].as_str().unwrap().to_string();
    let mut t = f::wire(&r);
    (t["state"], t["reason"], t["exit_code"]) = (json!("finished"), json!("exited"), json!(0));
    let got = observe(&w, true, &[], &[(&address, t)]).unwrap();
    assert_eq!(
        got,
        (1, vec![Life::Unknown], vec!["worker".to_string()]),
        "hc's Ended alone frees nothing"
    );
    let end = json!({"state": "finished"});
    crate::state::evidence::publish(&crate::agents::gens::path(&w.folder(ID), 1, "ended"), &end)
        .unwrap();
    let caller = w.caller();
    f::online(&w);
    f::listing(&w, &team(&caller), &[]);
    w.fakes.hc_expect(
        &["get", &address, "--json"],
        Ok(crate::spawn::Out {
            code: 1,
            ..Default::default()
        }),
    );
    w.panes(&[]);
    let core = crate::hc::connect(&w.ctx).unwrap();
    let s = Snapshot::for_spawn(&w.ctx, &caller, &core).unwrap();
    let seen = (
        s.facts.count(),
        s.facts.local.iter().map(|o| o.life).collect::<Vec<_>>(),
    );
    assert_eq!(
        seen,
        (0, vec![Life::Ended]),
        "recorded end, hc forgot it: Ended, slot free"
    );
}

// R4 (audit A7): every guard of a held identity refuses; a slot is never freed on an invalid block.
#[test]
fn a_held_identity_refuses_an_invalid_block() {
    for (case, key, value) in [
        ("record id", "id", json!("bbbbbbbbbbbb")),
        ("no session", "session", Value::Null),
        ("name", "name", json!("")),
        ("pending sessionId", "sessionId", json!("x")),
        ("pending hcName", "hcName", json!("worker")),
    ] {
        let w = held_world(Some("t9"));
        let folder = w.folder(ID);
        let mut r = crate::state::fs::read_json(&folder.join("record.json"))
            .unwrap()
            .unwrap();
        if ["sessionId", "hcName"].contains(&key) {
            r["pending"][key] = value;
        } else {
            r[key] = value;
        }
        write(&folder.join("record.json"), r);
        let got = locals(&w.ctx, &w.caller()).map_err(|e| e.message).err();
        assert_eq!(
            got.as_deref(),
            Some("unexpected error (invalid held generation)"),
            "{case}"
        );
    }
}

// R2, R5 (audits A3, af434f CRITICAL 5): an unreadable lifecycle member refuses admission; it is never skipped.
#[test]
fn an_unreadable_lifecycle_member_refuses_admission() {
    let check = |w: &World| locals(&w.ctx, &w.caller()).map_err(|e| e.check).err();
    let w = World::human("");
    f::member(&w, ID, json!({"lifecycle": 1, "generation": "01"}));
    assert_eq!(check(&w), Some("internal"), "a non-canonical generation");
    for file in ["commit", "notice"] {
        let w = World::human("");
        f::member(&w, ID, json!({"lifecycle": 1, "generation": "0"}));
        std::fs::create_dir_all(crate::agents::gens::dir(&w.folder(ID), 0)).unwrap();
        std::fs::write(crate::agents::gens::path(&w.folder(ID), 0, file), b"{\"x").unwrap();
        assert_eq!(check(&w), Some("evidence"), "a torn active {file}");
    }
    // Call 20: on Windows too (a directory symlink, else a junction); never skipped.
    let w = held_world(Some("t9"));
    let folder = w.folder(ID);
    let moved = w.state().join("moved");
    std::fs::rename(&folder, &moved).unwrap();
    crate::testing::link_dir(&moved, &folder);
    assert_eq!(
        check(&w),
        Some("evidence"),
        "a linked member folder is never skipped"
    );
    let w = held_world(Some("t9"));
    let folder = w.folder(ID);
    let elsewhere = w.state().join("elsewhere.json");
    std::fs::rename(folder.join("record.json"), &elsewhere).unwrap();
    crate::testing::link_dir(&elsewhere, &folder.join("record.json"));
    assert_eq!(check(&w), Some("evidence"), "a linked record");
}

// R1, R4: committed and noticed are the subject generation's, never the folder's or generation 0's.
#[test]
fn mixed_generations_use_the_active_one() {
    use crate::{agents::gens, state::evidence::publish};
    let w = World::human("");
    let caller = w.caller();
    f::member(
        &w,
        ID,
        json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd"}),
    );
    let folder = w.folder(ID);
    write(&folder.join("notice.json"), json!({"kind": "stopped"}));
    publish(
        &gens::path(&folder, 0, "commit"),
        &json!({"committed": true}),
    )
    .unwrap();
    publish(
        &gens::path(&folder, 0, "notice"),
        &json!({"kind": "stopped"}),
    )
    .unwrap();
    let one = || {
        locals(&w.ctx, &caller)
            .unwrap()
            .into_iter()
            .find(|l| l.identity.id == ID)
            .unwrap()
    };
    let got = one();
    assert_eq!(
        (got.stored.committed, got.stored.noticed, got.held),
        (false, false, false)
    );
    assert_eq!(got.identity.generation, "1");
    publish(
        &gens::path(&folder, 1, "commit"),
        &json!({"committed": true}),
    )
    .unwrap();
    publish(
        &gens::path(&folder, 1, "notice"),
        &json!({"kind": "stopped"}),
    )
    .unwrap();
    let got = one();
    assert_eq!((got.stored.committed, got.stored.noticed), (true, true));
    assert_eq!(
        got.stored.notice.unwrap()["kind"],
        "stopped",
        "the board shows this read (Task 5)"
    );
}

// R3 (re-audit a9703b8f CRITICAL): a generation done by recorded evidence holds no name and no slot, even when hc
// still lists its live launcher or recovery shell and no local identity resolves that terminal.
#[test]
fn a_done_generations_terminal_reserves_nothing() {
    use crate::{agents::gens, state::evidence::publish};
    let ab = json!({"aborted": true});
    for spawn in [true, false] {
        let w = held_world(Some("t9"));
        publish(&gens::path(&w.folder(ID), 1, "commit"), &ab).unwrap();
        let got = observe(&w, spawn, &["t9"], &[("t9", term(&w, "t9", "1", "live"))]).unwrap();
        assert_eq!(
            got,
            (0, vec![], vec![]),
            "spawn {spawn}: pending generation 1 void, its shell still listed"
        );
    }
    let w = World::human("");
    let mut r = f::member(
        &w,
        ID,
        json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd"}),
    );
    let folder = w.folder(ID);
    r["pending"] =
        json!({"gen": 2, "hcName": "m4567cdef", "sessionId": OTHER_SID, "hcAddress": "t8"});
    write(&folder.join("record.json"), r.clone());
    publish(
        &gens::path(&folder, 1, "ended"),
        &json!({"state": "finished"}),
    )
    .unwrap();
    publish(&gens::path(&folder, 2, "commit"), &ab).unwrap();
    let address = r["hcAddress"].as_str().unwrap().to_string();
    let mut gone = f::wire(&r);
    (gone["state"], gone["reason"], gone["exit_code"]) =
        (json!("finished"), json!("exited"), json!(0));
    // hc gets in address order: t7 (a stray shell labelled generation 1), t8 (generation 2's shell), then 1's own.
    let gets = [
        ("t7", term(&w, "t7", "1", "live")),
        ("t8", term(&w, "t8", "2", "live")),
        (address.as_str(), gone),
    ];
    let got = observe(&w, true, &["t7", "t8"], &gets).unwrap();
    assert_eq!(
        got,
        (0, vec![Life::Ended], vec![]),
        "1 ended, 2 void: neither generation's listed shell holds"
    );
}

// R3, PM call 11 (final re-audit): `freed` frees only a done generation this member's record names. A live shell of
// another member at the same generation, or of this member at a generation its record does not name, stays
// reserved; so does one of a named generation that is not done.
#[test]
fn freed_frees_only_this_members_done_named_generations() {
    use crate::{agents::gens, state::evidence::publish};
    let shell = |w: &World, id: &str, n: &str| {
        let mut t = term(w, "t7", n, "live");
        t["labels"]["mux.id"] = json!(id);
        t
    };
    for (case, id, n) in [
        ("another member, generation 1", "bbbbbbbbbbbb", "1"),
        ("generation 0, not named", ID, "0"),
    ] {
        let w = World::human("");
        let r = f::member(
            &w,
            ID,
            json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd"}),
        );
        publish(
            &gens::path(&w.folder(ID), 1, "ended"),
            &json!({"state": "finished"}),
        )
        .unwrap();
        let address = r["hcAddress"].as_str().unwrap().to_string();
        let mut gone = f::wire(&r);
        (gone["state"], gone["reason"], gone["exit_code"]) =
            (json!("finished"), json!("exited"), json!(0));
        // hc gets in address order: t7, then generation 1's own.
        let gets = [("t7", shell(&w, id, n)), (address.as_str(), gone)];
        let got = observe(&w, true, &["t7"], &gets).unwrap();
        assert_eq!(
            got,
            (1, vec![Life::Ended], vec!["worker".to_string()]),
            "{case}: its shell stays reserved"
        );
    }
    let w = held_world(Some("t9"));
    let gets = [
        ("t7", term(&w, "t7", "1", "live")),
        ("t9", term(&w, "t9", "1", "finished")),
    ];
    let got = observe(&w, true, &["t7", "t9"], &gets).unwrap();
    let want = (
        2,
        vec![Life::Unknown],
        vec!["worker".to_string(), "worker".to_string()],
    );
    assert_eq!(
        got, want,
        "pending 1 undecided: a stray of it stays reserved"
    );
}

// R3 (re-audit): recorded completion applies when hc cannot be read; `Snapshot::take`'s error arm settles too.
#[test]
fn a_recorded_end_frees_the_slot_when_hc_is_unavailable() {
    let w = World::human("");
    f::member(
        &w,
        ID,
        json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd"}),
    );
    let end = json!({"state": "finished"});
    crate::state::evidence::publish(&crate::agents::gens::path(&w.folder(ID), 1, "ended"), &end)
        .unwrap();
    w.panes(&[]); // no `f::online`: hc is not installed, so `hc::connect` fails
    let s = Snapshot::take(&w.ctx, &w.caller()).unwrap();
    assert!(s.core_error.is_some(), "hc unavailable");
    let seen = (
        s.facts.count(),
        s.facts.local.iter().map(|o| o.life).collect::<Vec<_>>(),
        s.facts.names(),
    );
    assert_eq!(
        seen,
        (0, vec![Life::Ended], vec![]),
        "Ended by its record: name and slot free"
    );
}

// R1 (re-audit): `locals` reads each evidence file of a lifecycle member once; record.json once through the walk and
// once more only for `completing`, when a done marker is present (PM call 4).
#[test]
fn membership_reads_each_lifecycle_file_once() {
    use crate::{agents::gens, state::evidence::publish};
    for ended in [false, true] {
        let w = World::human("");
        let caller = w.caller();
        let r = f::member(
            &w,
            ID,
            json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd"}),
        );
        let folder = w.folder(ID);
        publish(
            &gens::path(&folder, 1, "commit"),
            &json!({"committed": true}),
        )
        .unwrap();
        publish(
            &gens::path(&folder, 1, "notice"),
            &json!({"kind": "stopped"}),
        )
        .unwrap();
        publish(
            &gens::path(&folder, 1, "launch"),
            &json!({"hcName": "m0123abcd"}),
        )
        .unwrap();
        write(
            &folder.join("view.json"),
            json!({"pane": "%9", "gen": 1, "hcAddress": r["hcAddress"],
            "hcName": "m0123abcd"}),
        );
        if ended {
            publish(
                &gens::path(&folder, 1, "ended"),
                &json!({"state": "finished"}),
            )
            .unwrap();
        }
        let (got, reads) = crate::state::fs::record_reads(|| locals(&w.ctx, &caller));
        got.unwrap();
        let others: Vec<_> = reads
            .iter()
            .filter(|p| !p.ends_with("record.json"))
            .collect();
        let mut seen = others.clone();
        seen.sort();
        seen.dedup();
        assert_eq!(
            seen.len(),
            others.len(),
            "ended {ended}: a file read twice: {reads:?}"
        );
        let records = reads.iter().filter(|p| p.ends_with("record.json")).count();
        assert_eq!(
            records,
            1 + usize::from(ended),
            "ended {ended}: record.json reads: {reads:?}"
        );
    }
}

// b-calls 11 (F11): an orphan-only lifecycle member whose generation 0 is not done by recorded evidence holds its
// name and one slot whatever hc shows: nothing listed, the fence undecided, hc unreachable, or its own terminal
// listed Running (counted once). Done by recorded evidence: nothing held. A stale `completing` naming it beside its
// ended.json: held (b-calls 33). Torn: the snapshot refuses `evidence`.
#[test]
fn an_unresolved_orphan_holds_its_name_and_slot() {
    use crate::agents::{core_fixture as cf, fixture::ID, gens, life_fixture as lf};
    let done = ["abandoned", "aborted", "ended", "released"];
    for case in [
        "nothing listed",
        "fence undecided",
        "listed running",
        "hc unreachable",
        "abandoned",
        "aborted",
        "ended",
        "released",
        "stale completing",
        "torn",
    ] {
        let (w, s) = lf::world();
        let stale = json!({"completing": {"gen": 0, "intent": "ended", "facts": {}}});
        let m = lf::life(
            &w,
            if case == "stale completing" {
                stale
            } else {
                json!({})
            },
        );
        match case {
            "abandoned" => lf::seed_gen(&m, 0, Some("abandoned"), None),
            "aborted" => lf::seed_gen(&m, 0, Some("run"), Some(json!({"aborted": true}))),
            "torn" => {
                lf::seed_gen(&m, 0, Some("run"), None);
                std::fs::write(gens::path(&m, 0, "commit"), b"{").unwrap();
            }
            _ => lf::seed_gen(&m, 0, Some("run"), lf::committed()),
        }
        if ["ended", "released", "stale completing"].contains(&case) {
            let file = if case == "released" {
                "released"
            } else {
                "ended"
            };
            gens::publish_once(&m, 0, file, &json!({})).unwrap();
        }
        if case == "fence undecided" {
            cf::online(&w);
            let id = format!("mux.id={ID}");
            lf::fails(
                &w,
                &[
                    "ls",
                    "--all",
                    "--label",
                    id.as_str(),
                    "--label",
                    "mux.gen=0",
                    "--json",
                ],
            );
            crate::agents::team_state::open(&w.ctx, &s, |_| Ok(())).unwrap();
            assert!(
                !gens::path(&m, 0, "ended").exists(),
                "the fence decided nothing"
            );
        }
        if case != "hc unreachable" && case != "torn" {
            cf::online(&w);
            let listed: &[&str] = if case == "listed running" {
                &["t0"]
            } else {
                &[]
            };
            cf::listing(&w, &lf::team(&w), listed);
            if case == "listed running" {
                lf::get(&w, "t0", &lf::running(&w, 0));
            }
        }
        let got = Snapshot::take(&w.ctx, &w.caller());
        if case == "torn" {
            assert_eq!(
                got.map(|_| ()).map_err(|e| e.check),
                Err("evidence"),
                "{case}"
            );
            continue;
        }
        let snap = got.unwrap();
        let held = !done.contains(&case);
        assert_eq!(snap.facts.count(), usize::from(held), "{case}: slots");
        assert_eq!(
            snap.facts.names().contains(&"worker".to_string()),
            held,
            "{case}: name"
        );
    }
}

// b-calls 37 (R12, R13): admission (`Snapshot::for_spawn`) reserves an unresolved orphan exactly as `take` does. A
// live active generation 1 beside a Running orphan 0: each holds its own slot under the one name (call 24). Ending
// the orphan (`gens::terminate`) frees its slot only; the active one keeps its slot in both readers.
#[test]
fn admission_reserves_orphans_and_an_active_generation_keeps_its_slot() {
    use crate::agents::team_state::Tx;
    use crate::agents::{core_fixture as cf, fixture::ID, gens, life_fixture as lf};
    use serde_json::Value;
    /// One snapshot (admission or `take`) over hc's team listing; (slots, the name is held).
    fn slots(w: &World, spawn: bool, listed: &[&str], gets: &[(&str, Value)]) -> (usize, bool) {
        cf::online(w);
        cf::listing(w, &lf::team(w), listed);
        for (a, t) in gets {
            lf::get(w, a, t);
        }
        w.panes(&[]);
        let snap = if spawn {
            let core = crate::hc::connect(&w.ctx).unwrap();
            Snapshot::for_spawn(&w.ctx, &w.caller(), &core).unwrap()
        } else {
            Snapshot::take(&w.ctx, &w.caller()).unwrap()
        };
        w.fakes.hc_done().unwrap();
        (
            snap.facts.count(),
            snap.facts.names().contains(&"worker".to_string()),
        )
    }
    let (w, _) = lf::world();
    let m = lf::life(&w, json!({}));
    lf::seed_gen(&m, 0, Some("run"), lf::committed());
    assert_eq!(
        slots(&w, true, &[], &[]),
        (1, true),
        "for_spawn: the orphan holds its name and slot"
    );
    let (w, s) = lf::world();
    // Checklist item 2: the active record carries its launcher argv, so hc's Running terminal is this generation's.
    let m = lf::life(
        &w,
        json!({"generation": "1", "hcAddress": "t1", "hcName": lf::hc_name(1), "launcherArgv": lf::ARGV}),
    );
    lf::seed_gen(&m, 0, Some("run"), lf::committed());
    lf::seed_gen(&m, 1, Some("run"), lf::committed());
    let both = [("t0", lf::running(&w, 0)), ("t1", lf::running(&w, 1))];
    let only = [("t1", lf::running(&w, 1))];
    for spawn in [true, false] {
        assert_eq!(
            slots(&w, spawn, &["t1"], &only),
            (2, true),
            "spawn {spawn}: an orphan hc does not list"
        );
        assert_eq!(
            slots(&w, spawn, &["t0", "t1"], &both),
            (2, true),
            "spawn {spawn}: active and orphan"
        );
    }
    cf::online(&w);
    lf::found(&w, 0, &lf::running(&w, 0));
    lf::get(&w, "t0", &lf::running(&w, 0));
    cf::answer(&w, &["kill", "t0", "--json"], "{}");
    gens::terminate(
        &Tx {
            ctx: &w.ctx,
            scope: s.clone(),
        },
        ID,
        0,
    )
    .unwrap();
    w.fakes.hc_done().unwrap();
    let killed = lf::wire(&w, 0, "finished", Some("killed"), &lf::ARGV);
    let after = [("t0", killed.clone()), ("t1", lf::running(&w, 1))];
    for spawn in [true, false] {
        assert_eq!(
            slots(&w, spawn, &["t0", "t1"], &after),
            (1, true),
            "spawn {spawn}: the active slot stays"
        );
    }
}

// Call 61 (TL #144 F3, QA #144 F3): a done orphan (settle aborted or abandoned it, so it is no longer pending) whose
// terminal hc still lists Running holds no name and no slot, in `take` and in `for_spawn`.
#[test]
fn a_done_orphan_hc_still_lists_holds_nothing() {
    use crate::agents::settle::{Mode, Outcome};
    use crate::agents::team_state::Tx;
    use crate::agents::{core_fixture as cf, fixture::ID, gens, life_fixture as lf};
    for (case, claim) in [("aborted", Some("run")), ("abandoned", None)] {
        for spawn in [true, false] {
            let (w, s) = lf::world();
            let m = lf::life(&w, json!({"pending": lf::pending(0)}));
            lf::seed_gen(&m, 0, claim, None);
            let got = Tx {
                ctx: &w.ctx,
                scope: s.clone(),
            }
            .settle(ID, Mode::Fence)
            .unwrap();
            assert_eq!(got, Outcome::Done, "{case}");
            assert!(gens::done(&m, 0).unwrap(), "{case}: done");
            assert!(
                lf::record(&m).get("pending").is_none(),
                "{case}: pending dropped"
            );
            cf::online(&w);
            cf::listing(&w, &lf::team(&w), &["t0"]);
            lf::get(&w, "t0", &lf::running(&w, 0));
            w.panes(&[]);
            let snap = if spawn {
                let core = crate::hc::connect(&w.ctx).unwrap();
                Snapshot::for_spawn(&w.ctx, &w.caller(), &core).unwrap()
            } else {
                Snapshot::take(&w.ctx, &w.caller()).unwrap()
            };
            let c = format!("{case} spawn {spawn}");
            assert_eq!(snap.facts.count(), 0, "{c}: slots");
            assert!(
                !snap.facts.names().contains(&"worker".to_string()),
                "{c}: name"
            );
        }
    }
}
