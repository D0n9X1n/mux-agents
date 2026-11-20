use super::*;
use crate::agents::{
    core_fixture as cf,
    fixture::{ID, World, row_of},
};
use serde_json::json;
#[test]
fn drawn_target_restores_detached_but_refuses_stale_attached() {
    for (drawn, present, want) in [
        ("", false, Some("%19")),
        ("%9", true, Some("%9")),
        ("%8", true, None),
        ("%9", false, None),
    ] {
        let w = World::human("");
        let c = w.caller();
        cf::member(&w, ID, json!({}));
        if present {
            let mut row = row_of("%9|@5|300||", &[]);
            row.command = "hc attach taaaaaaaaaaaa".into();
            w.pane_rows(&[row]);
        } else {
            w.panes(&[]);
        }
        w.launches_hold();
        w.fakes
            .mux_rule("new-window", 0, "%19|@8|hc attach taaaaaaaaaaaa\n", "");
        w.fakes.mux_rule(
            "#{window_active}",
            0,
            if present { "%9|1|1|1\n" } else { "%19|1|1|1\n" },
            "",
        );
        cf::online(&w);
        let got = focus_target_hc(
            &w.ctx,
            &c,
            &Target {
                id: ID.into(),
                ..Target::new("worker", drawn)
            },
        );
        assert_eq!(got.as_ref().ok().map(|f| f.pane.as_str()), want);
        let calls = w.fakes.mux_calls();
        assert_eq!(
            calls.iter().filter(|a| a[1] == "new-window").count(),
            usize::from(drawn.is_empty())
        );
        assert_eq!(
            calls
                .iter()
                .filter(|a| a.last().is_some_and(|s| s.starts_with("select-window")))
                .count(),
            usize::from(want.is_some())
        );
        assert_eq!(
            w.fakes
                .hc_calls()
                .iter()
                .map(|(_, a, _)| a.clone())
                .collect::<Vec<_>>(),
            [vec!["--version"]]
        );
        w.fakes.hc_done().unwrap();
    }
}
#[test]
fn click_next_prev_enter_reopen_detached_and_select_attached() {
    for present in [false, true] {
        for event in [
            Event::Press {
                button: 0,
                x: 3,
                y: 1,
            },
            Event::Next,
            Event::Prev,
            Event::Enter,
        ] {
            let w = World::human("");
            let c = w.caller();
            cf::member(&w, ID, json!({}));
            if present {
                let mut row = row_of("%9|@5|300||", &[]);
                row.command = "hc attach taaaaaaaaaaaa".into();
                w.pane_rows(&[row]);
            } else {
                w.panes(&[]);
            }
            w.launches_hold();
            w.fakes
                .mux_rule("new-window", 0, "%19|@8|hc attach taaaaaaaaaaaa\n", "");
            let pane = if present { "%9" } else { "%19" };
            w.fakes
                .mux_rule("#{window_active}", 0, &format!("{pane}|1|1|1\n"), "");
            w.fakes
                .mux_rule("capture-pane", 0, "40\n \u{25c9} worker\n", "");
            cf::online(&w);
            let board = json!({"agents":[{"id":ID,"name":"worker","state":"running","pane":if present {json!("%9")} else {Value::Null}}]});
            let target = Target::of(board["agents"][0].as_object().unwrap());
            let mut shown = Shown {
                written: SystemTime::UNIX_EPOCH,
                targets: vec![target],
                drawn: vec![(ID.to_string(), 0, "\u{25c9} worker".to_string())],
                width: 40,
                looking: None,
                team: false,
                done: Vec::new(),
            };
            let mut pick = Pick::default();
            if event == Event::Enter {
                pick.key(Event::Down, &shown.targets);
            }
            assert!(act(
                &w.ctx,
                &c,
                Look {
                    color: false,
                    ascii: false,
                    dumb: false
                },
                keys::Read {
                    event,
                    first: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
                    alone: true
                },
                &mut pick,
                &mut shown,
                &mut Panel::default(),
            ));
            assert_eq!(shown.looking.as_deref(), Some(pane));
            assert_eq!(shown.targets[0].pane, pane);
            assert!(pick.note().is_none());
            w.fakes.hc_done().unwrap();
        }
    }
}

#[test]
fn peek_detached_refuses_without_reopening_or_capturing() {
    let mut w = World::human("");
    if let Some(plain) = w
        .ctx
        .state_root
        .to_str()
        .and_then(|r| r.strip_prefix(r"\\?\"))
    {
        w.ctx.state_root = plain.into();
    }
    let c = w.caller();
    cf::member(&w, ID, json!({}));
    w.panes(&[]);
    let e = peek(
        &w.ctx,
        &c,
        &Target {
            id: ID.into(),
            ..Target::new("worker", "")
        },
    )
    .unwrap_err();
    assert_eq!(e.message, "the agent has no live view; run focus worker");
    assert!(
        !w.fakes
            .mux_calls()
            .iter()
            .any(|a| a.iter().any(|s| s == "new-window"
                || s == "split-window"
                || s == "run-shell"
                || s.contains("capture-pane")))
    );
    w.fakes.hc_done().unwrap();
}

#[test]
fn lifecycle_rows_are_listed_in_the_open_fold_in_ascii_and_unicode() {
    for ascii in [false, true] {
        for (state, life) in [
            ("lost", "lost"),
            ("unknown", "unknown"),
            ("exited 7", "ended"),
        ] {
            // The board's `endedTotal` counts its terminal-Ended members only.
            let total = u64::from(life == "ended");
            let board = json!({"running":0,"endedTotal":total,"agents":[{"name":"worker","state":state,"life":life,
                "view":"detached","hcAddress":"t1","pane":null,"window":null,"age":60,"endedAgo":60,"exitCode":7}]});
            let (size, look) = (
                Size {
                    width: 40,
                    height: 24,
                },
                Look {
                    color: false,
                    ascii,
                    dumb: false,
                },
            );
            // Closed, the fold line counts it.
            let closed = format_board(board.as_object(), &[], size, look);
            assert!(
                closed
                    .iter()
                    .any(|r| r.contains("Ended") && r.ends_with(" 1")),
                "{closed:?}"
            );
            // Open, it is listed; `exit N` keeps its word.
            let focus = tree::Focus {
                ended_open: true,
                ..tree::Focus::default()
            };
            let laid = lay_focus(board.as_object(), &[], size, look, focus).0;
            let rows: Vec<String> = laid.rows.iter().map(|r| render(r, false)).collect();
            let row = rows
                .iter()
                .find(|r| r.contains("worker"))
                .expect("lifecycle row hidden");
            assert_eq!(row.contains(state), life == "ended", "{row}");
            assert!(!rows.iter().any(|r| r.contains("older")), "{rows:?}");
            assert_ne!(
                Target::of(board["agents"][0].as_object().unwrap()).life,
                "running"
            );
        }
    }
}

#[test]
fn two_members_with_one_name_open_the_one_selected() {
    let w = World::human("");
    let c = w.caller();
    cf::member(&w, ID, json!({"name": "twin"}));
    cf::member(&w, "bbbbbbbbbbbb", json!({"name": "twin", "pane": "%8"}));
    let rows: Vec<_> = [("%9", ID), ("%8", "bbbbbbbbbbbb")]
        .iter()
        .map(|(p, id)| {
            let mut r = row_of(&format!("{p}|@5|300||"), &[]);
            r.command = format!("hc attach t{id}");
            r
        })
        .collect();
    w.pane_rows(&rows);
    w.fakes.mux_rule("#{window_active}", 0, "%8|1|1|1\n", "");
    let row = |id: &str, pane: &str, parent: Option<&str>, m: u32| {
        json!({"id": id, "name": if parent.is_some() { "twin" } else { "lead" },
        "parentId": parent, "leader": parent.is_none(), "createdAt": format!("2026-10-29T11:{m:02}:00.0000000Z"), "state": "running",
        "life": "running", "pane": pane, "age": 60})
    };
    let board = json!({"team": true, "running": 3, "agents": [row("cccccccccccc", "%7", None, 1), row(ID, "%9", Some("cccccccccccc"), 2),
        row("bbbbbbbbbbbb", "%8", Some("cccccccccccc"), 3)]});
    let board = board.as_object().unwrap();
    let walk = tree::walk(board);
    let mut pick = Pick::default();
    pick.set_wrap(true);
    for _ in 0..3 {
        pick.key(Event::Down, &walk);
    }
    // A redraw: the selection follows the id; its row, not its namesake's, carries the selection.
    pick.follow(&tree::walk(board));
    let look = Look::default();
    let mut laid = lay_focus(
        Some(board),
        &[],
        Size {
            width: 40,
            height: 24,
        },
        look,
        tree::Focus {
            shown: Some("%9"),
            selected: pick.selected(),
            ended_open: false,
        },
    )
    .0;
    mark(&mut laid, pick.selected().unwrap(), 40, look);
    let text: Vec<String> = laid.rows.iter().map(|r| render(r, false)).collect();
    assert!(
        text[3].starts_with("   \u{25c9} twin") && text[4].starts_with(">  \u{25c9} twin"),
        "{text:?}"
    );
    let mut shown = Shown {
        written: SystemTime::UNIX_EPOCH,
        targets: walk.clone(),
        drawn: drawn(&laid),
        width: 40,
        looking: Some("%9".into()),
        team: true,
        done: Vec::new(),
    };
    let read = keys::Read {
        event: Event::Enter,
        first: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        alone: true,
    };
    assert!(act(
        &w.ctx,
        &c,
        look,
        read,
        &mut pick,
        &mut shown,
        &mut Panel::default()
    ));
    assert_eq!(
        (shown.looking.as_deref(), pick.note()),
        (Some("%8"), None),
        "its id opens, not its namesake"
    );
    let after = lay_focus(
        Some(board),
        &[],
        Size {
            width: 40,
            height: 24,
        },
        look,
        tree::Focus {
            shown: shown.looking.as_deref(),
            selected: None,
            ended_open: false,
        },
    )
    .0;
    assert!(
        render(&after.rows[4], false).starts_with(' ')
            && render(&after.rows[3], false).starts_with(" "),
        "the mark follows"
    );
    w.fakes.hc_done().unwrap();
}

/// The peek file the popup left in the state folder (a fake multiplexer never runs the popup, which deletes it).
fn peek_text(w: &World) -> Option<String> {
    std::fs::read_dir(crate::agents::server_dir(&w.ctx, &w.caller()))
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with("peek-"))
        .and_then(|e| std::fs::read_to_string(e.path()).ok())
}
/// A world whose state root is a plain path (as `peek_detached_refuses_without_reopening_or_capturing` makes it).
fn plain_world() -> World {
    let mut w = World::human("");
    if let Some(plain) = w
        .ctx
        .state_root
        .to_str()
        .and_then(|r| r.strip_prefix(r"\\?\"))
    {
        w.ctx.state_root = plain.into();
    }
    w
}
#[test]
fn open_acts_by_life_never_restarts_and_always_answers() {
    let nothing = Some("nothing to open for worker");
    // (case, life, validated view, member exists, hc online, kept output or what hc answers, focus reads back, pane shown after, popups, note)
    let cases = [
        (
            "running view",
            "running",
            true,
            true,
            false,
            "",
            true,
            Some("%9"),
            0,
            None,
        ),
        (
            "running closed",
            "running",
            false,
            true,
            true,
            "listed",
            true,
            Some("%19"),
            0,
            None,
        ),
        (
            "running stale",
            "running",
            true,
            false,
            false,
            "",
            true,
            None,
            0,
            nothing,
        ),
        (
            "running failed",
            "running",
            true,
            true,
            false,
            "",
            false,
            None,
            0,
            Some("focus failed: worker"),
        ),
        (
            "lost view",
            "lost",
            true,
            true,
            false,
            "",
            true,
            Some("%9"),
            0,
            None,
        ),
        // review: a Lost row with no view reattaches its recovery terminal when hc keeps it; a reattach that does not
        // take shows its retained output; hc dropping it is nothing to open; hc unreachable is a failure.
        (
            "lost kept",
            "lost",
            false,
            true,
            true,
            "listed",
            true,
            Some("%19"),
            0,
            None,
        ),
        (
            "lost output",
            "lost",
            false,
            true,
            true,
            "kept-output",
            false,
            None,
            1,
            None,
        ),
        (
            "lost dropped",
            "lost",
            false,
            true,
            true,
            "dropped",
            true,
            None,
            0,
            nothing,
        ),
        (
            "lost hc down",
            "lost",
            false,
            true,
            true,
            "down",
            true,
            None,
            0,
            Some("focus failed: worker"),
        ),
        (
            "unknown view no hc",
            "unknown",
            true,
            true,
            false,
            "",
            true,
            Some("%9"),
            0,
            None,
        ),
        (
            "unknown output",
            "unknown",
            false,
            true,
            true,
            "old\n",
            true,
            None,
            1,
            None,
        ),
        (
            "unknown absent",
            "unknown",
            false,
            true,
            true,
            " \n",
            true,
            None,
            0,
            nothing,
        ),
        (
            "running dropped",
            "running",
            false,
            true,
            true,
            "dropped",
            true,
            None,
            0,
            nothing,
        ),
        (
            "running hc down",
            "running",
            false,
            true,
            true,
            "down",
            true,
            None,
            0,
            Some("focus failed: worker"),
        ),
        // review: an Unknown row whose terminal is labelled for another identity shows nothing of it.
        (
            "unknown mislabeled",
            "unknown",
            false,
            true,
            true,
            "mislabeled",
            true,
            None,
            0,
            nothing,
        ),
    ];
    for (case, life, present, exists, hc, kept, reads, want, popups, note) in cases {
        for press in [true, false] {
            let w = plain_world();
            let c = w.caller();
            let record = exists.then(|| cf::member(&w, ID, json!({})));
            if present {
                let mut row = row_of("%9|@5|300||", &[]);
                row.command = "hc attach taaaaaaaaaaaa".into();
                w.pane_rows(&[row]);
            } else {
                w.panes(&[]);
            }
            w.launches_hold();
            w.fakes
                .mux_rule("new-window", 0, "%19|@8|hc attach taaaaaaaaaaaa\n", "");
            w.fakes.mux_rule(
                "#{window_active}",
                0,
                &format!(
                    "{}|1|1|1\n",
                    if reads { want.unwrap_or("%9") } else { "%4" }
                ),
                "",
            );
            w.fakes
                .mux_rule("capture-pane", 0, "40\n \u{25c9} worker\n", "");
            if hc {
                cf::online(&w);
            }
            // hc 0.1.21's answer for a terminal it no longer keeps (exit 3, observed 2026-11-14), and an unreachable hc (exit 5).
            let gone = r#"{"error":{"hint":"see `hc ls --all`","kind":"not_found","message":"no terminal \"taaaaaaaaaaaa\""}}"#;
            let down =
                r#"{"error":{"kind":"unavailable","message":"core did not start within 15 s"}}"#;
            let out = |code: i32, stdout: &str| -> crate::error::Res<crate::spawn::Out> {
                Ok(crate::spawn::Out {
                    code,
                    stdout: stdout.into(),
                    stderr: String::new(),
                })
            };
            let (get, read) = (
                ["get", "taaaaaaaaaaaa", "--json"],
                ["read", "taaaaaaaaaaaa", "--lines", "200"],
            );
            // Every hc look first checks that the terminal is still the member's own (`hc get`, review).
            let own = || cf::wire(record.as_ref().unwrap());
            match kept {
                "" => {}
                // A Running row without a view: `hc get` finds its terminal (reopen), answers `not_found` (nothing to open) or fails.
                "listed" => cf::get(&w, "taaaaaaaaaaaa", own()),
                // Kept, but the reattach does not take: the check, then `retained` connects again, checks and reads.
                "kept-output" => {
                    cf::get(&w, "taaaaaaaaaaaa", own());
                    cf::online(&w);
                    cf::get(&w, "taaaaaaaaaaaa", own());
                    cf::answer(&w, &read, "old\n")
                }
                "mislabeled" => {
                    let mut t = own();
                    t["labels"]["mux.gen"] = json!("1");
                    cf::get(&w, "taaaaaaaaaaaa", t)
                }
                // hc dropped the terminal (`not_found`), or hc is unreachable, at that first look.
                "dropped" => w.fakes.hc_expect(&get, out(3, gone)),
                "down" => w.fakes.hc_expect(&get, out(5, down)),
                // An Ended or Unknown row whose own terminal hc keeps: its output, read after the check.
                text => {
                    cf::get(&w, "taaaaaaaaaaaa", own());
                    cf::answer(&w, &read, text)
                }
            }
            let other = Target {
                id: "cccccccccccc".into(),
                ..Target::new("other", "%5")
            };
            let worker = Target {
                id: ID.into(),
                life: life.into(),
                ..Target::new("worker", if present { "%9" } else { "" })
            };
            let mut shown = Shown {
                written: SystemTime::UNIX_EPOCH,
                targets: vec![other, worker],
                width: 40,
                looking: None,
                team: true,
                done: Vec::new(),
                drawn: vec![(ID.to_string(), 0, "\u{25c9} worker".to_string())],
            };
            let mut pick = Pick::default();
            pick.set_wrap(true);
            // A click on worker (ASCII rendering: a team bar takes it) while `other` is selected; or worker selected, then Enter.
            let (event, look) = if press {
                pick.select(&shown.targets, 0);
                (
                    Event::Press {
                        button: 0,
                        x: 3,
                        y: 1,
                    },
                    Look {
                        color: false,
                        ascii: true,
                        dumb: false,
                    },
                )
            } else {
                pick.select(&shown.targets, 1);
                (Event::Enter, Look::default())
            };
            assert!(act(
                &w.ctx,
                &c,
                look,
                keys::Read {
                    event,
                    first: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
                    alone: true
                },
                &mut pick,
                &mut shown,
                &mut Panel::default()
            ));
            let how = format!("{case} {}", if press { "press" } else { "enter" });
            assert_eq!(shown.looking.as_deref(), want, "{how}");
            assert_eq!(pick.note().as_deref(), note, "{how}");
            // A successful open (a view or the output) clears the selection; nothing to open or a failure keeps the
            // clicked row selected, so `p` or Enter then acts on it.
            assert_eq!(pick.selected(), note.is_some().then_some(ID), "{how}");
            let calls: Vec<String> = w.fakes.mux_calls().iter().map(|a| a.join(" ")).collect();
            assert_eq!(
                calls.iter().filter(|a| a.contains("display-popup")).count(),
                popups,
                "{how}"
            );
            assert_eq!(
                calls.iter().filter(|a| a.contains("new-window")).count(),
                usize::from(matches!(
                    case,
                    "running closed" | "lost kept" | "lost output"
                )),
                "{how}"
            );
            assert!(
                !calls
                    .iter()
                    .any(|a| a.contains("send-keys") || a.contains("respawn")),
                "{how}: nothing is typed or restarted"
            );
            assert!(
                w.fakes
                    .hc_calls()
                    .iter()
                    .all(|(_, a, _)| a[0] != "run" && a[0] != "kill"),
                "{how}: hc starts and stops nothing"
            );
            assert!(
                hc || w.fakes.hc_calls().is_empty(),
                "{how}: a validated view needs no hc"
            );
            if popups > 0 {
                assert_eq!(
                    peek_text(&w).as_deref(),
                    Some("old\n\n-- peek: worker (q closes) --\n"),
                    "{how}"
                );
            }
            w.fakes.hc_done().unwrap();
        }
    }
}

#[test]
fn p_previews_the_selected_id_and_changes_nothing() {
    // (case, B selected, B's life, B has a view, what hc keeps, the pane captured, popups, note)
    let cases = [
        ("live", true, "running", true, "", Some("%8"), 1, None),
        ("ended", true, "ended", false, "old\n", None, 1, None),
        (
            "blank",
            true,
            "ended",
            false,
            " \n",
            None,
            0,
            Some("nothing to open for twin"),
        ),
        (
            "dropped",
            true,
            "ended",
            false,
            "dropped",
            None,
            0,
            Some("nothing to open for twin"),
        ),
        (
            "down",
            true,
            "ended",
            false,
            "down",
            None,
            0,
            Some("focus failed: twin"),
        ),
        ("none", false, "running", true, "", Some("%9"), 1, None),
    ];
    for (case, selected, life, view, kept, captured, popups, note) in cases {
        let w = plain_world();
        let c = w.caller();
        cf::member(&w, ID, json!({"name": "twin"}));
        let b_record = cf::member(&w, "bbbbbbbbbbbb", json!({"name": "twin", "pane": "%8"}));
        let mut rows = Vec::new();
        for (pane, id) in [("%9", ID), ("%8", "bbbbbbbbbbbb")]
            .into_iter()
            .filter(|(p, _)| view || *p == "%9")
        {
            let mut row = row_of(&format!("{pane}|@5|300||"), &[]);
            row.command = format!("hc attach t{id}");
            rows.push(row);
        }
        w.pane_rows(&rows);
        w.fakes.mux_rule("show-buffer", 0, "screen text\n", "");
        let gone = r#"{"error":{"hint":"see `hc ls --all`","kind":"not_found","message":"no terminal \"tbbbbbbbbbbbb\""}}"#;
        let out = |code: i32, stdout: &str| -> crate::error::Res<crate::spawn::Out> {
            Ok(crate::spawn::Out {
                code,
                stdout: stdout.into(),
                stderr: String::new(),
            })
        };
        let read = ["read", "tbbbbbbbbbbbb", "--lines", "200"];
        let get = ["get", "tbbbbbbbbbbbb", "--json"];
        match kept {
            "" => {}
            // hc checks the terminal is still B's own first (review): `not_found` (exit 3) is nothing to show; hc
            // unreachable (exit 5) is a failure; B's own terminal is then read.
            "dropped" => {
                cf::online(&w);
                w.fakes.hc_expect(&get, out(3, gone))
            }
            "down" => {
                cf::online(&w);
                w.fakes
                    .hc_expect(&get, out(5, r#"{"error":{"kind":"unavailable"}}"#))
            }
            text => {
                cf::online(&w);
                cf::get(&w, "tbbbbbbbbbbbb", cf::wire(&b_record));
                cf::answer(&w, &read, text)
            }
        }
        let a = Target {
            id: ID.into(),
            ..Target::new("twin", "%9")
        };
        let b = Target {
            id: "bbbbbbbbbbbb".into(),
            life: life.into(),
            ..Target::new("twin", if view { "%8" } else { "" })
        };
        let mut shown = Shown {
            written: SystemTime::UNIX_EPOCH,
            targets: vec![a, b],
            drawn: Vec::new(),
            width: 40,
            looking: Some("%9".into()),
            team: true,
            done: Vec::new(),
        };
        let mut pick = Pick::default();
        if selected {
            pick.select(&shown.targets, 1);
        }
        let before = pick.selected().map(str::to_string);
        assert!(act(
            &w.ctx,
            &c,
            Look::default(),
            keys::Read {
                event: Event::Peek,
                first: SystemTime::UNIX_EPOCH,
                alone: true
            },
            &mut pick,
            &mut shown,
            &mut Panel::default()
        ));
        let calls: Vec<String> = w.fakes.mux_calls().iter().map(|a| a.join(" ")).collect();
        let took = |pane: &str| {
            calls
                .iter()
                .any(|a| a.contains("capture-pane") && a.contains(pane))
        };
        assert_eq!(
            (took("%8"), took("%9")),
            (captured == Some("%8"), captured == Some("%9")),
            "{case}: duplicate names preview the id"
        );
        assert_eq!(
            calls.iter().filter(|a| a.contains("display-popup")).count(),
            popups,
            "{case}"
        );
        assert_eq!(pick.note().as_deref(), note, "{case}");
        if case == "ended" {
            assert_eq!(
                peek_text(&w).as_deref(),
                Some("old\n\n-- peek: twin (q closes) --\n")
            );
        }
        assert_eq!(
            (
                pick.selected().map(str::to_string),
                shown.looking.as_deref()
            ),
            (before, Some("%9")),
            "{case}: nothing else changes"
        );
        assert!(
            !calls
                .iter()
                .any(|a| ["select-window", "new-window", "send-keys", "respawn"]
                    .iter()
                    .any(|v| a.contains(v))),
            "{case}"
        );
        assert!(
            w.fakes
                .hc_calls()
                .iter()
                .all(|(_, a, _)| a[0] != "run" && a[0] != "kill"),
            "{case}"
        );
        w.fakes.hc_done().unwrap();
    }
}

/// A frame with no rows: the keys that act on the bar itself, not on a row.
fn no_rows() -> Shown {
    Shown {
        written: SystemTime::UNIX_EPOCH,
        targets: Vec::new(),
        drawn: Vec::new(),
        width: 40,
        looking: None,
        team: false,
        done: Vec::new(),
    }
}

/// One key read alone at the epoch, against `shown`.
fn press(w: &World, event: Event, pick: &mut Pick, shown: &mut Shown) -> bool {
    press_in(w, event, pick, shown, &mut Panel::default())
}

/// `press` with `panel` kept across keys, so an action it starts off the loop is there to settle.
fn press_in(
    w: &World,
    event: Event,
    pick: &mut Pick,
    shown: &mut Shown,
    panel: &mut Panel,
) -> bool {
    let read = keys::Read {
        event,
        first: SystemTime::UNIX_EPOCH,
        alone: true,
    };
    act(
        &w.ctx,
        &w.caller(),
        Look::default(),
        read,
        pick,
        shown,
        panel,
    )
}

fn popups(w: &World) -> usize {
    (w.fakes.mux_calls().iter())
        .filter(|a| a.iter().any(|x| x.contains("display-popup")))
        .count()
}

// `i`: the team's notes in the popup, newest first, each its title then `author · at · id`. Outside a team, or with
// an empty notebook, the note line says `no notes` and nothing opens.
#[test]
fn i_shows_the_team_notes_newest_first() {
    let w = plain_world();
    let mut pick = Pick::default();
    assert!(press(&w, Event::Notes, &mut pick, &mut no_rows()));
    assert_eq!(pick.note().as_deref(), Some("no notes"), "outside a team");
    assert_eq!(popups(&w), 0);
    w.state();
    crate::agents::tree::tests::team_record(&w, &[]);
    let mut pick = Pick::default();
    press(&w, Event::Notes, &mut pick, &mut no_rows());
    assert_eq!(
        pick.note().as_deref(),
        Some("no notes"),
        "an empty notebook"
    );
    for title in ["plan", "found it"] {
        let argv: Vec<String> = ["note", "-Title", title, "-Body", "b"]
            .iter()
            .map(|t| t.to_string())
            .collect();
        assert_eq!(crate::cli::run(&argv, || Ok(&w.ctx)).0, 0, "{title}");
    }
    w.fakes.clear_log();
    let mut pick = Pick::default();
    press(&w, Event::Notes, &mut pick, &mut no_rows());
    assert_eq!((pick.note(), popups(&w)), (None, 1));
    let text = peek_text(&w).unwrap();
    let (newer, older) = (text.find("found it\n"), text.find("plan\n"));
    assert!(newer.is_some() && newer < older, "{text:?}");
    assert!(text.contains("person \u{b7} "), "{text:?}");
    assert!(text.ends_with("-- peek: notes (q closes) --\n"), "{text:?}");
}

// `P`: where new agents open cycles tab → split h → split v → tab, in this process as the bar; the note line says
// where. A refusal (an agent pane) shows its reason.
#[test]
fn capital_p_cycles_where_new_agents_open() {
    for (stored, note) in [
        ("tab\n", "opens in: split h"),
        ("h\n", "opens in: split v"),
        ("v\n", "opens in: tab"),
    ] {
        let w = World::human("");
        w.fakes.mux_rule("show-options", 0, stored, "");
        let mut pick = Pick::default();
        assert!(press(&w, Event::Placement, &mut pick, &mut no_rows()));
        assert_eq!(pick.note().as_deref(), Some(note), "{stored:?}");
    }
    let w = World::human(ID);
    let mut pick = Pick::default();
    press(&w, Event::Placement, &mut pick, &mut no_rows());
    assert_eq!(
        pick.note().as_deref(),
        Some("placement refused: an agent pane cannot choose where agents open")
    );
}

// `o`: the selected row's retained output (`hc read`) in the popup, live or ended, never its live view; with none
// selected, the first running agent's. Blank output is `nothing to open for <name>`; hc down is `focus failed`.
#[test]
fn o_shows_the_retained_output_of_the_selected_row() {
    let out = |code: i32, stdout: &str| -> crate::error::Res<crate::spawn::Out> {
        Ok(crate::spawn::Out {
            code,
            stdout: stdout.into(),
            stderr: String::new(),
        })
    };
    for (case, life, selected, kept, popup, note) in [
        ("live", "running", true, "live text\n", 1, None),
        ("ended", "ended", true, "old\n", 1, None),
        ("first", "running", false, "first\n", 1, None),
        (
            "blank",
            "ended",
            true,
            " \n",
            0,
            Some("nothing to open for twin"),
        ),
        ("down", "ended", true, "down", 0, Some("focus failed: twin")),
    ] {
        let w = plain_world();
        let record = cf::member(&w, ID, json!({"name": "twin"}));
        let get = ["get", "taaaaaaaaaaaa", "--json"];
        cf::online(&w);
        if kept == "down" {
            w.fakes
                .hc_expect(&get, out(5, r#"{"error":{"kind":"unavailable"}}"#));
        } else {
            cf::get(&w, "taaaaaaaaaaaa", cf::wire(&record));
            cf::answer(&w, &["read", "taaaaaaaaaaaa", "--lines", "200"], kept);
        }
        let mut shown = no_rows();
        shown.targets = vec![Target {
            id: ID.into(),
            life: life.into(),
            ..Target::new("twin", "%9")
        }];
        let mut pick = Pick::default();
        if selected {
            pick.select(&shown.targets, 0);
        }
        assert!(press(&w, Event::Output, &mut pick, &mut shown), "{case}");
        assert_eq!(
            (popups(&w), pick.note().as_deref()),
            (popup, note),
            "{case}"
        );
        let calls: Vec<String> = w.fakes.mux_calls().iter().map(|a| a.join(" ")).collect();
        assert!(
            !calls.iter().any(|a| a.contains("capture-pane")),
            "{case}: never the live view"
        );
        if popup == 1 {
            assert_eq!(
                peek_text(&w),
                Some(format!("{kept}\n-- peek: twin (q closes) --\n")),
                "{case}"
            );
        }
        w.fakes.hc_done().unwrap();
    }
}

// With nothing selected, `m`, `o` and the flat bar's `p` act on the agent on screen (the pane the session shows), else
// the first running one; a selection, ended included, always wins. Here no hc answers, so each one's note names the
// agent it acted on.
#[test]
fn unselected_mail_output_and_peek_act_on_the_agent_on_screen() {
    let row = |id: &str, name: &str, pane: &str, life: &str| Target {
        id: id.into(),
        life: life.into(),
        ..Target::new(name, pane)
    };
    let targets = vec![
        row("cccccccccccc", "gone", "", "ended"),
        row(ID, "worker", "%1", "running"),
        row("bbbbbbbbbbbb", "writer", "%2", "running"),
    ];
    for event in [Event::Mail, Event::Output, Event::Peek] {
        for (case, looking, selected, name) in [
            ("on screen", Some("%2"), None, "writer"),
            ("nothing on screen", None, None, "worker"),
            ("main on screen", Some("%3"), None, "worker"),
            ("selected", Some("%2"), Some(0), "gone"),
        ] {
            let w = World::human("");
            let mut shown = no_rows();
            shown.targets = targets.clone();
            shown.looking = looking.map(str::to_string);
            let mut pick = Pick::default();
            if let Some(at) = selected {
                pick.select(&shown.targets, at);
            }
            assert!(press(&w, event, &mut pick, &mut shown), "{event:?} {case}");
            let note = pick.note().unwrap_or_default();
            assert!(note.ends_with(name), "{event:?} {case}: {note:?}");
        }
    }
}

// Enter on a selected ended row resumes it as a tab, off the loop, and when the answer is settled focuses the
// successor's new view, by the id the resume answered (the ended member's own id names a superseded member, which has
// no view to focus).
#[test]
fn enter_on_an_ended_row_focuses_its_successor() {
    use crate::agents::resume::tests as rt;
    let w = World::human("");
    let x = rt::member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
    w.panes(&[]);
    rt::facts(&w, &[(&x, "exited")]);
    rt::launches(&w, "t1");
    // The successor's view (%9, attaching t1 with the installed hc) is listed once its window opened, as the
    // multiplexer lists a launched pane.
    let command = crate::agents::view_bind_tests::hc_attach(&w, "t1");
    let opened = std::rc::Rc::new(std::cell::Cell::new(false));
    let (made, answer) = (opened.clone(), format!("%9|@5|{command}\n"));
    w.fakes.mux_script("new-window", move |_| {
        made.set(true);
        Ok(crate::mux::Output {
            code: 0,
            stdout: answer.clone(),
            stderr: String::new(),
        })
    });
    w.fakes.mux_script("list-panes", move |argv| {
        let rows = match opened.get() {
            true => vec![crate::agents::fixture::Listed {
                command: command.clone(),
                ..row_of("%9|@5|300||", &[])
            }],
            false => Vec::new(),
        };
        Ok(crate::mux::Output {
            code: 0,
            stdout: crate::agents::fixture::listed_rows(argv, &rows, "\n"),
            stderr: String::new(),
        })
    });
    w.fakes.mux_rule("#{window_active}", 0, "%9|1|1|1\n", "");
    let mut shown = no_rows();
    shown.targets = vec![Target {
        id: ID.into(),
        life: "ended".into(),
        ..Target::new("worker", "")
    }];
    let mut pick = Pick::default();
    pick.select(&shown.targets, 0);
    let mut panel = Panel::default();
    assert!(press_in(
        &w,
        Event::Enter,
        &mut pick,
        &mut shown,
        &mut panel
    ));
    // The resume runs off the loop: nothing is focused until its answer is settled on a later pass.
    assert!(panel.jobs.pending(ID));
    assert_eq!(shown.looking, None);
    assert!(settle_jobs(
        &w.ctx,
        &w.caller(),
        &mut pick,
        &mut shown,
        &mut panel
    ));
    assert_eq!(pick.note(), None);
    assert_eq!(
        shown.looking.as_deref(),
        Some("%9"),
        "the successor's view is focused"
    );
    assert_eq!(pick.selected(), None, "a focus clears the selection");
    w.fakes.hc_done().unwrap();
}

// Enter on a selected ended row whose resume is refused (here a pre-lifecycle member): the resume runs off the loop, and
// its refusal says why on the note line, `resume refused: <reason>`. Nothing opens or launches.
#[test]
fn enter_on_an_ended_row_whose_resume_is_refused_says_why() {
    let w = plain_world();
    cf::member(&w, ID, json!({"name": "twin"}));
    let mut shown = no_rows();
    shown.targets = vec![Target {
        id: ID.into(),
        life: "ended".into(),
        ..Target::new("twin", "")
    }];
    let (mut pick, mut panel) = (Pick::default(), Panel::default());
    pick.select(&shown.targets, 0);
    assert!(press_in(
        &w,
        Event::Enter,
        &mut pick,
        &mut shown,
        &mut panel
    ));
    assert_eq!(pick.note(), None, "no answer yet");
    assert!(settle_jobs(
        &w.ctx,
        &w.caller(),
        &mut pick,
        &mut shown,
        &mut panel
    ));
    let note = pick.note().unwrap_or_default();
    assert!(note.starts_with("resume refused: twin "), "{note:?}");
    assert_eq!(popups(&w), 0, "nothing opens");
    assert!(!(w.fakes.hc_calls().iter()).any(|(_, a, _)| a[0] == "run"));
    w.fakes.hc_done().unwrap();
}

// A left click on a row does what Enter on that row does, on the flat bar and a team's: on an ended agent it resumes
// it off the loop (the row selected, so it stays the selection while it runs); on the fold line it opens the fold; on
// a kept shell it asks `close? y/n`.
#[test]
fn a_click_on_a_row_does_what_enter_on_it_does() {
    let fold = Target::fold();
    let kept = Target {
        id: "t9".into(),
        life: pick::KEPT.into(),
        ..Target::new("shell", "")
    };
    let ended = Target {
        id: ID.into(),
        life: "ended".into(),
        ..Target::new("twin", "")
    };
    let click = |y: u16| keys::Read {
        event: Event::Press { button: 0, x: 3, y },
        first: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        alone: true,
    };
    for team in [false, true] {
        let w = plain_world();
        cf::member(&w, ID, json!({"name": "twin"}));
        w.fakes.mux_rule(
            "capture-pane",
            0,
            " 40\n - twin\n \u{25be} Ended\n kept shell shell\n".trim_start(),
            "",
        );
        let mut shown = no_rows();
        shown.team = team;
        shown.targets = vec![kept.clone(), ended.clone(), fold.clone()];
        shown.drawn = vec![
            (ID.to_string(), 0, "- twin".to_string()),
            (fold.id.clone(), 1, "\u{25be} Ended".to_string()),
            ("t9".to_string(), 2, "kept shell shell".to_string()),
        ];
        let (mut pick, mut panel) = (Pick::default(), Panel::default());
        let (c, look) = (w.caller(), Look::default());
        assert!(act(
            &w.ctx,
            &c,
            look,
            click(1),
            &mut pick,
            &mut shown,
            &mut panel
        ));
        assert!(panel.jobs.pending(ID), "team {team}: the ended row resumes");
        assert_eq!(pick.selected(), Some(ID), "team {team}");
        assert!(act(
            &w.ctx,
            &c,
            look,
            click(2),
            &mut pick,
            &mut shown,
            &mut panel
        ));
        assert!(
            panel.ended_open,
            "team {team}: the fold line opens the fold"
        );
        assert!(act(
            &w.ctx,
            &c,
            look,
            click(3),
            &mut pick,
            &mut shown,
            &mut panel
        ));
        assert_eq!(
            pick.note().as_deref(),
            Some(pick::ASK),
            "team {team}: a kept shell asks"
        );
    }
}

// Space acts only on a row the person chose: A selected above done-unseen B, A ends and the selection falls to B, and
// Space marks nothing. Once the person moves onto B themselves, Space marks it.
#[test]
fn space_never_marks_a_row_the_selection_fell_to() {
    const SPELL: &str = "2026-10-29T11:58:00.0000000Z";
    let w = World::human("");
    w.clock("2026-10-29T12:00:00.0000000Z");
    let folder = w.agent_record(ID, json!({}));
    w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
    let a = Target {
        id: "cccccccccccc".into(),
        ..Target::new("first", "%7")
    };
    let b = Target {
        id: ID.into(),
        ..Target::new("worker", "%9")
    };
    let mut pick = Pick::default();
    pick.key(Event::Down, &[a, b.clone()]);
    assert_eq!(pick.selected(), Some("cccccccccccc"));
    let mut shown = no_rows();
    shown.targets = vec![b];
    shown.done = vec![(ID.into(), SPELL.into())];
    pick.follow(&shown.targets);
    assert_eq!(pick.selected(), Some(ID), "the selection fell to B");
    press(&w, Event::Ack, &mut pick, &mut shown);
    assert!(!folder.join("seen.json").exists(), "nothing marked");
    pick.key(Event::Down, &shown.targets);
    press(&w, Event::Ack, &mut pick, &mut shown);
    assert_eq!(
        std::fs::read_to_string(folder.join("seen.json")).ok(),
        Some(format!(r#"{{"since":"{SPELL}"}}"#)),
        "a row the person chose is marked"
    );
}

// Enter on a kept shell the selection fell to (the agent the person chose left) asks nothing: it takes the row as
// theirs, and only their next Enter asks `close? y/n`, so a `y` never closes a shell they did not pick.
#[test]
fn enter_never_asks_to_close_a_kept_shell_the_selection_fell_to() {
    let w = World::human("");
    let kept = Target {
        id: "t9".into(),
        life: pick::KEPT.into(),
        ..Target::new("shell", "")
    };
    let a = Target {
        id: ID.into(),
        ..Target::new("worker", "%9")
    };
    let mut pick = Pick::default();
    pick.key(Event::Down, &[kept.clone(), a.clone()]);
    pick.key(Event::Down, &[kept.clone(), a]);
    let mut shown = no_rows();
    shown.targets = vec![kept];
    pick.follow(&shown.targets);
    assert_eq!(
        pick.selected(),
        Some("t9"),
        "the selection fell to the shell"
    );
    assert!(press(&w, Event::Enter, &mut pick, &mut shown));
    assert_eq!(pick.note(), None, "nothing asked");
    assert_eq!(pick.selected(), Some("t9"));
    assert!(press(&w, Event::Enter, &mut pick, &mut shown));
    assert_eq!(
        pick.note().as_deref(),
        Some(pick::ASK),
        "the person's Enter asks"
    );
}

// Space on a done, unseen member with no view (its pane empty on the board) marks it by id: the pane path has nothing
// to resolve.
#[test]
fn space_marks_a_done_member_without_a_view() {
    const SPELL: &str = "2026-10-29T11:58:00.0000000Z";
    let w = World::human("");
    w.clock("2026-10-29T12:00:00.0000000Z");
    let folder = w.agent_record(ID, json!({}));
    w.panes(&[]);
    let mut shown = no_rows();
    shown.targets = vec![Target {
        id: ID.into(),
        ..Target::new("worker", "")
    }];
    shown.done = vec![(ID.into(), SPELL.into())];
    let mut pick = Pick::default();
    pick.select(&shown.targets, 0);
    assert!(press(&w, Event::Ack, &mut pick, &mut shown));
    assert_eq!(
        std::fs::read_to_string(folder.join("seen.json")).ok(),
        Some(format!(r#"{{"since":"{SPELL}"}}"#))
    );
}

// Space: the selected row, done and unseen, is marked seen as leaving its pane marks it (`board::mark_seen`), so it
// leaves Inbox on the next board. No selection (never the fallback row), or a selected row that is not done: nothing.
#[test]
fn space_marks_the_selected_done_row_seen() {
    const SPELL: &str = "2026-10-29T11:58:00.0000000Z";
    let board = json!({"agents": [
        {"id": ID, "name": "worker", "state": "running", "activity": "done", "changedAt": SPELL, "pane": "%9"},
        {"id": "bbbbbbbbbbbb", "name": "busy", "state": "running", "activity": "working",
         "changedAt": SPELL, "pane": "%8"},
        {"id": "cccccccccccc", "name": "gone", "state": "exited", "activity": "done", "changedAt": SPELL},
        {"name": "old", "state": "running", "activity": "done", "changedAt": SPELL, "pane": "%7"},
        {"id": "dddddddddddd", "name": "untimed", "state": "running", "activity": "done", "pane": "%6"}
    ]});
    assert_eq!(
        super::done_spells(board.as_object()),
        [
            (ID.to_string(), SPELL.to_string()),
            ("old".into(), SPELL.into())
        ]
    );
    for (case, selected, done) in [
        ("none selected", false, true),
        ("not done", true, false),
        ("done", true, true),
    ] {
        let w = World::human("");
        w.clock("2026-10-29T12:00:00.0000000Z");
        let folder = w.agent_record(ID, json!({}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        let mut shown = no_rows();
        shown.targets = vec![Target {
            id: ID.into(),
            ..Target::new("worker", "%9")
        }];
        if done {
            shown.done = vec![(ID.into(), SPELL.into())];
        }
        let mut pick = Pick::default();
        if selected {
            pick.select(&shown.targets, 0);
        }
        press(&w, Event::Ack, &mut pick, &mut shown);
        let seen = std::fs::read_to_string(folder.join("seen.json")).ok();
        let want = (case == "done").then(|| format!(r#"{{"since":"{SPELL}"}}"#));
        assert_eq!(seen, want, "{case}");
        assert_eq!(
            pick.selected().is_some(),
            selected,
            "{case}: the selection stays"
        );
    }
}
