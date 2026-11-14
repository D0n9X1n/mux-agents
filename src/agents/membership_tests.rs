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
