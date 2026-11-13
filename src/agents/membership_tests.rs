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
