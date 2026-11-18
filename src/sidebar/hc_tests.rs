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
                    first: SystemTime::UNIX_EPOCH + Duration::from_secs(1)
                },
                &mut pick,
                &mut shown,
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
fn lifecycle_words_are_visible_in_ascii_and_unicode() {
    for ascii in [false, true] {
        for (state, life) in [
            ("lost", "lost"),
            ("unknown", "unknown"),
            ("exited 7", "ended"),
        ] {
            let board = json!({"running":0,"endedTotal":1,"agents":[{"name":"worker","state":state,"life":life,
                "view":"detached","hcAddress":"t1","pane":null,"window":null,"age":60,"exitCode":7}]});
            let rows = format_board(
                board.as_object(),
                &[],
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
            let row = rows
                .iter()
                .find(|r| r.contains("worker"))
                .expect("lifecycle row hidden");
            assert!(row.contains(state), "{row}");
            assert!(!rows.iter().any(|r| r.contains("+1 more")));
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
        },
    )
    .0;
    mark(&mut laid, pick.selected().unwrap(), look);
    let text: Vec<String> = laid.rows.iter().map(|r| render(r, false)).collect();
    assert!(
        text[3].starts_with("   \u{25c9} twin") && text[4].starts_with("\u{f054}  \u{25c9} twin"),
        "{text:?}"
    );
    let mut shown = Shown {
        written: SystemTime::UNIX_EPOCH,
        targets: walk.clone(),
        drawn: drawn(&laid),
        width: 40,
        looking: Some("%9".into()),
        team: true,
    };
    let read = keys::Read {
        event: Event::Enter,
        first: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
    };
    assert!(act(&w.ctx, &c, look, read, &mut pick, &mut shown));
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
            "ended output",
            "ended",
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
            "ended absent",
            "ended",
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
        (
            "ended dropped",
            "ended",
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
            "ended hc down",
            "ended",
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
                    first: SystemTime::UNIX_EPOCH + Duration::from_secs(1)
                },
                &mut pick,
                &mut shown
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
                first: SystemTime::UNIX_EPOCH
            },
            &mut pick,
            &mut shown
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
