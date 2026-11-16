//! views-6f V5: the bar lists kept shells before its members; Enter asks `close? y/n`; only `y` closes.
use super::keys::{self, Event};
use super::pick::{Pick, Target};
use super::tree::tests::{member, team};
use super::{
    Laid, Look, PLAIN, Shown, Size, Style, act, click, drawn, lay_focus, mark, tree, walk_of,
};
use crate::agents::core_fixture as cf;
use crate::agents::fixture::World;
use crate::mux::Output;
use serde_json::{Map, Value, json};
use std::time::{Duration, SystemTime};

const LOOK: Look = Look {
    color: false,
    ascii: false,
    dumb: false,
};
const ASKED: [(&str, Style); 1] = [("close? y/n", PLAIN)];

fn read(event: Event) -> keys::Read {
    keys::Read {
        event,
        first: SystemTime::UNIX_EPOCH,
    }
}
fn shown(targets: Vec<Target>) -> Shown {
    Shown {
        written: SystemTime::UNIX_EPOCH,
        targets,
        drawn: Vec::new(),
        width: 40,
        looking: None,
        team: false,
    }
}
fn kept_target(address: &str, name: &str) -> Target {
    Target::kept(
        json!({"address": address, "name": name})
            .as_object()
            .unwrap(),
    )
}
fn text(rows: &[super::Row]) -> Vec<String> {
    rows.iter()
        .map(|r| r.iter().map(|(t, _)| t.as_str()).collect())
        .collect()
}
fn kills(w: &World) -> usize {
    w.fakes
        .hc_calls()
        .iter()
        .filter(|c| c.1.first().map(String::as_str) == Some("kill"))
        .count()
}
/// No multiplexer call and no hc call since the log was cleared.
fn quiet(w: &World) -> bool {
    w.fakes.mux_calls().is_empty() && w.fakes.hc_calls().is_empty()
}
/// hc's reply for kept shell `k1` of `team`.
fn kept_wire(team: &str, state: &str) -> Value {
    json!({"address": "k1", "name": "m00000000", "state": state, "argv": ["/bin/sh"],
        "labels": {"mux.kept": team, "mux.keptName": "a"}})
}
/// A team board (leader `L`, child `C`) with `n` kept shells.
fn team_with_kept(n: usize) -> Map<String, Value> {
    let mut b = team(vec![
        member("L", None, 1, json!({})),
        member("C", Some("L"), 2, json!({})),
    ]);
    let kept: Vec<Value> = (1..=n)
        .map(|i| json!({"address": format!("k{i}"), "name": format!("s{i}")}))
        .collect();
    b.insert("kept".into(), json!(kept));
    b
}
/// `board` laid at width 40 and `height`, with `notes`, the pane shown and the selected id.
fn lay(
    board: Option<&Map<String, Value>>,
    notes: &[(&str, Style)],
    height: usize,
    shown: Option<&str>,
    selected: Option<&str>,
) -> Laid {
    lay_focus(
        board,
        notes,
        Size { width: 40, height },
        LOOK,
        tree::Focus { shown, selected },
    )
    .0
}
/// The bar answers a click read with `laid`'s rows at width 40.
fn clicks(w: &World, laid: &Laid) {
    let answer = format!("40\n{}\n", text(&laid.rows).join("\n"));
    w.fakes.mux_script("capture-pane", move |_| {
        Ok(Output {
            code: 0,
            stdout: answer.clone(),
            stderr: String::new(),
        })
    });
}
/// The frame `laid` of `board` on screen, as the watch loop records it.
fn frame(board: Option<&Map<String, Value>>, laid: &Laid, team: bool) -> Shown {
    let targets = walk_of(board, laid, team);
    Shown {
        written: SystemTime::UNIX_EPOCH,
        targets,
        drawn: drawn(laid),
        width: 40,
        looking: None,
        team,
    }
}
/// Ask the question on `k1`: the first Enter selects it, the second asks.
fn ask(w: &World, pick: &mut Pick, s: &mut Shown) {
    for _ in 0..2 {
        act(&w.ctx, &w.caller(), LOOK, read(Event::Enter), pick, s);
    }
    assert_eq!(pick.note().as_deref(), Some("close? y/n"));
}

// Checklist 10: a kept row's identity is its address, its name is cleaned, it has no pane and its own class.
#[test]
fn a_kept_target_is_its_address_with_a_clean_name_and_no_pane() {
    let t = kept_target("k1", "a\u{1b}[2Jb\nc");
    assert_eq!(
        (t.id.as_str(), t.name.as_str(), t.pane.as_str()),
        ("k1", "a[2Jbc", "")
    );
    assert!(!super::clickable(&t), "a kept row takes no click");
    assert!(
        super::clickable(&Target::new("worker", "%9")),
        "a member does"
    );
}

#[test]
fn y_is_yes_and_any_other_byte_is_other() {
    let mut d = keys::Decoder::default();
    let bytes = b"yxnj\x1b[C\x1b[5~\x1bOP\x1b[A";
    let events: Vec<Event> = d
        .push(bytes, SystemTime::UNIX_EPOCH)
        .into_iter()
        .map(|r| r.event)
        .collect();
    let want = [
        Event::Yes,
        Event::Other,
        Event::Next,
        Event::Down,
        Event::Other,
        Event::Other,
        Event::Other,
        Event::Up,
    ];
    assert_eq!(
        events, want,
        "Right Arrow, Page Up and SS3 P are Other (v6f call 12); n stays herdr's next agent"
    );
}

// v6f call 11: the question and the selected row get their lines first; overflow kept shells sit behind `+N more`.
#[test]
fn kept_rows_share_the_height_so_the_question_and_the_selection_stay() {
    let laid = lay(Some(&team_with_kept(2)), &ASKED, 6, None, None);
    let rows = text(&laid.rows);
    assert!(rows.len() <= 6, "{rows:?}");
    assert!(
        rows.last().is_some_and(|r| r.contains("close? y/n")),
        "the question has the last line: {rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.contains("kept shell s1"))
            && rows.iter().any(|r| r.contains("+1 more kept")),
        "{rows:?}"
    );
    let laid = lay(Some(&team_with_kept(1)), &ASKED, 4, None, Some("C"));
    let rows = text(&laid.rows);
    assert!(
        laid.agents.iter().any(|(t, _)| t.id == "C"),
        "the selected member stays: {rows:?}"
    );
    assert!(
        rows.last().is_some_and(|r| r.contains("close? y/n")),
        "height 4 keeps the question: {rows:?}"
    );
    let full = lay(Some(&team_with_kept(2)), &[], 24, None, None);
    assert!(
        text(&full.rows)
            .iter()
            .any(|r| r.contains("kept shells \u{b7} 2")),
        "room: the heading and every shell"
    );
}

// Checklist 12: at height 5 the question and the selected kept row `k1` are reserved before headings and members.
#[test]
fn the_selected_kept_row_stays_beside_the_question_at_height_5() {
    let laid = lay(Some(&team_with_kept(2)), &ASKED, 5, None, Some("k1"));
    let rows = text(&laid.rows);
    assert!(rows.len() <= 5, "{rows:?}");
    assert!(
        laid.agents.iter().any(|(t, _)| t.id == "k1"),
        "k1 stays visible: {rows:?}"
    );
    assert!(
        rows.last().is_some_and(|r| r.contains("close? y/n")),
        "{rows:?}"
    );
    // Ambiguity 15: with one row of room and no kept shell selected, no kept block (no lone row without its heading).
    let laid = lay(Some(&team_with_kept(2)), &ASKED, 5, None, None);
    let rows = text(&laid.rows);
    assert!(
        !rows.iter().any(|r| r.contains("kept shell")),
        "one row of room, nothing selected: {rows:?}"
    );
}

// v6f call 11: every kept shell stays in the keys' walk; `j` reaches `k2`, and selecting it brings it on screen.
#[test]
fn every_kept_shell_is_reachable_behind_more() {
    let board = team_with_kept(2);
    let draw = |selected: Option<&str>| lay(Some(&board), &ASKED, 6, None, selected);
    let mut pick = Pick::default();
    pick.set_wrap(true);
    let walk = walk_of(Some(&board), &draw(None), true);
    pick.follow(&walk);
    pick.key(Event::Down, &walk);
    pick.key(Event::Down, &walk);
    assert_eq!(pick.selected(), Some("k2"), "j twice reaches k2");
    let laid = draw(pick.selected());
    assert!(
        laid.agents.iter().any(|(t, _)| t.id == "k2"),
        "the window holds the selected shell: {:?}",
        text(&laid.rows)
    );
}

// F1: a selected kept row survives the watch loop's redraw (`walk_of` feeds `pick.follow`), so Enter asks.
#[test]
fn a_selected_kept_row_survives_a_redraw() {
    let board = team_with_kept(1);
    let draw = || lay(Some(&board), &[], 24, None, None);
    let mut pick = Pick::default();
    pick.set_wrap(true);
    let walk = walk_of(Some(&board), &draw(), true);
    pick.follow(&walk);
    pick.key(Event::Down, &walk);
    assert_eq!(pick.selected(), Some("k1"));
    let walk = walk_of(Some(&board), &draw(), true);
    pick.follow(&walk);
    assert_eq!(
        pick.selected(),
        Some("k1"),
        "still the kept row after the redraw"
    );
    assert!(matches!(pick.key(Event::Enter, &walk), super::pick::Act::Focus(t) if t.id == "k1"));
}

// Checklist 13: a headerless `last_resort` frame (chain L-A-B-C, shown C, L selected) keeps the kept rows and the
// members on rows of their own: the mark lands on L's row and a click on each row hits its own target.
#[test]
fn a_headerless_frame_never_shares_a_row_between_kept_and_member() {
    let mut board = team(vec![
        member("L", None, 1, json!({})),
        member("A", Some("L"), 2, json!({})),
        member("B", Some("A"), 3, json!({})),
        member("C", Some("B"), 4, json!({})),
    ]);
    board.insert("kept".into(), json!([{"address": "k1", "name": "a"}]));
    let mut laid = lay(Some(&board), &ASKED, 6, Some("%14"), Some("L"));
    let rows: Vec<usize> = laid.agents.iter().map(|(_, at)| *at).collect();
    let mut unique = rows.clone();
    unique.dedup();
    assert_eq!(rows, unique, "{:?}", text(&laid.rows));
    assert!(
        laid.agents.iter().any(|(t, _)| t.id == "k1"),
        "{:?}",
        text(&laid.rows)
    );
    mark(&mut laid, "L", LOOK);
    let at = |id: &str| laid.agents.iter().find(|(t, _)| t.id == id).unwrap().1;
    let drawn_rows = text(&laid.rows);
    assert!(
        drawn_rows[at("L")].starts_with('\u{25b8}'),
        "the mark is on L's row: {drawn_rows:?}"
    );
    assert!(
        drawn_rows[at("L")].contains(" L"),
        "L's row shows L: {drawn_rows:?}"
    );
    let s = frame(Some(&board), &laid, true);
    let w = World::human("");
    clicks(&w, &laid);
    let y = |id: &str| (at(id) + 1) as u16;
    assert_eq!(
        click(&w.ctx, &w.caller(), y("k1"), &s).map(|t| t.id),
        None,
        "the kept row takes no click"
    );
    assert_eq!(
        click(&w.ctx, &w.caller(), y("L"), &s).map(|t| t.id),
        Some("L".to_string()),
        "L's row is L's"
    );
}

// Spec: `hc kill` only after `y`; any other key (F4: `x` too, through `Decoder::push`) cancels; nothing is focused
// or selected. Setup's own mux calls are cleared before the answer (F2).
#[test]
fn enter_asks_and_only_y_closes() {
    for (bytes, closes) in [
        (&b"y"[..], true),
        (&b"x"[..], false),
        (&b"n"[..], false),
        (&b"\r"[..], false),
        (&b"j"[..], false),
        (&b"\x1b[C"[..], false),
        (&b"\x1b[5~"[..], false),
    ] {
        let w = World::human("");
        let c = w.caller();
        let team = crate::agents::membership::team(&c);
        let mut s = shown(vec![kept_target("k1", "a"), Target::new("worker", "%9")]);
        let mut pick = Pick::default();
        ask(&w, &mut pick, &mut s);
        pick.ticked();
        pick.ticked();
        assert_eq!(
            pick.note().as_deref(),
            Some("close? y/n"),
            "a pending question outlives ticks"
        );
        if closes {
            cf::online(&w);
            cf::get(&w, "k1", kept_wire(&team, "live"));
            cf::answer(&w, &["kill", "k1", "--json"], "{}");
        }
        w.fakes.clear_log();
        let mut d = keys::Decoder::default();
        for r in d.push(bytes, SystemTime::UNIX_EPOCH) {
            act(&w.ctx, &c, LOOK, r, &mut pick, &mut s);
        }
        assert_eq!(pick.note(), None, "{bytes:?}: the question is answered");
        assert_eq!(kills(&w), usize::from(closes), "{bytes:?}");
        assert!(
            w.fakes.mux_calls().is_empty(),
            "{bytes:?}: no focus, no select"
        );
        if closes {
            w.fakes.hc_done().unwrap();
        }
    }
}

// Checklist 15: a press while `close? y/n` is pending answers it: over a member row or a kept row it focuses and
// closes nothing, and a following `y` closes nothing.
#[test]
fn a_press_while_asked_cancels_and_a_later_y_closes_nothing() {
    let board = json!({"agents": [{"name": "worker", "id": "worker", "state": "running", "pane": "%9"}],
        "kept": [{"address": "k1", "name": "a"}]});
    let laid = lay(board.as_object(), &[], 24, None, None);
    for over in ["worker", "k1"] {
        let w = World::human("");
        let c = w.caller();
        clicks(&w, &laid);
        let mut s = frame(board.as_object(), &laid, false);
        let mut pick = Pick::default();
        ask(&w, &mut pick, &mut s);
        w.fakes.clear_log();
        let y = laid.agents.iter().find(|(t, _)| t.id == over).unwrap().1 as u16 + 1;
        let report = format!("\x1b[<0;3;{y}M");
        let mut d = keys::Decoder::default();
        for mut r in d.push(report.as_bytes(), SystemTime::UNIX_EPOCH) {
            r.first = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
            act(&w.ctx, &c, LOOK, r, &mut pick, &mut s);
        }
        assert_eq!(pick.note(), None, "{over}: the press answers the question");
        act(&w.ctx, &c, LOOK, read(Event::Yes), &mut pick, &mut s);
        assert_eq!(pick.note(), None, "{over}");
        assert!(quiet(&w), "{over}: nothing focused or closed");
    }
}

// A refused close (the shell finished meanwhile) is shown; nothing is killed.
#[test]
fn a_failed_close_is_shown() {
    let w = World::human("");
    let c = w.caller();
    let team = crate::agents::membership::team(&c);
    let mut s = shown(vec![kept_target("k1", "a")]);
    let mut pick = Pick::default();
    ask(&w, &mut pick, &mut s);
    cf::online(&w);
    cf::get(&w, "k1", kept_wire(&team, "finished"));
    act(&w.ctx, &c, LOOK, read(Event::Yes), &mut pick, &mut s);
    assert_eq!(pick.note().as_deref(), Some("close failed: a"));
    assert_eq!(kills(&w), 0);
}

// F9: a kept row is no agent. With only a kept row drawn, peek and herdr's next and previous do nothing; a kept target
// reaching them would be focused as a member and fail visibly (`focus failed: a`). On a team bar with the kept row
// selected, `p` previews nothing and `n`/`N` select only members.
#[test]
fn peek_and_steps_skip_kept_rows() {
    for e in [Event::Peek, Event::Next, Event::Prev] {
        let w = World::human("");
        let mut s = shown(vec![kept_target("k1", "a")]);
        let mut pick = Pick::default();
        let c = w.caller();
        w.fakes.clear_log();
        act(&w.ctx, &c, LOOK, read(e), &mut pick, &mut s);
        assert_eq!(pick.note(), None, "{e:?}");
        assert!(quiet(&w), "{e:?}");
    }
    for e in [Event::Peek, Event::Next, Event::Prev] {
        let w = World::human("");
        let mut s = shown(vec![kept_target("k1", "a"), Target::new("worker", "%9")]);
        s.team = true;
        let mut pick = Pick::default();
        pick.set_wrap(true);
        pick.key(Event::Down, &s.targets);
        assert_eq!(pick.selected(), Some("k1"));
        let c = w.caller();
        w.fakes.clear_log();
        act(&w.ctx, &c, LOOK, read(e), &mut pick, &mut s);
        assert_eq!(pick.note(), None, "team {e:?}");
        assert!(quiet(&w), "team {e:?}");
        if e != Event::Peek {
            assert_eq!(
                pick.selected(),
                Some("worker"),
                "team {e:?}: a member is selected"
            );
        }
    }
}
