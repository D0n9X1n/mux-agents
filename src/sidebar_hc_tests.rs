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
                name: "worker".into(),
                pane: drawn.into(),
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
            let board = json!({"agents":[{"name":"worker","state":"running","pane":if present {json!("%9")} else {Value::Null}}]});
            let target = pick::resolve(board.as_object().unwrap(), "worker").unwrap();
            let mut shown = Shown {
                written: SystemTime::UNIX_EPOCH,
                targets: vec![target],
                width: 40,
                looking: None,
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
                board.as_object()
            ));
            assert_eq!(shown.looking.as_deref(), Some(pane));
            assert_eq!(shown.targets[0].pane, pane);
            assert!(pick.note().is_none());
            w.fakes.hc_done().unwrap();
        }
    }
}
