use super::*;
use serde_json::json;

// A row of a 40-column bar: `left` after the margin, `right` ending one column short of the edge.
fn pad(left: &str, right: &str) -> String {
    let used = left.chars().count() + right.chars().count();
    format!(" {left}{}{right}", " ".repeat(38 - used))
}

fn target(name: &str) -> Target {
    Target::new(name, &format!("%{}", name.len()))
}

fn drawn(names: &[&str]) -> Vec<Target> {
    names.iter().map(|name| target(name)).collect()
}

#[test]
fn row_hit_on_captured_lines() {
    for (line, name) in [
        (" \u{25c9} worker      working   4m", "worker"),
        (
            "\u{f054}\u{25cc} test-calc  \u{2709} 1  waiting  3m",
            "test-calc",
        ),
        (">\u{25c9} worker", "worker"),
        (" \u{25c9} work...", "work..."),
        (" ! fix-ci       needs you  12s", "fix-ci"),
        (
            "\u{f054}\u{2726} test-calc  \u{2709} 1  done  30s",
            "test-calc",
        ),
        (" \u{25cc} idler        idle   3m", "idler"),
    ] {
        assert_eq!(row_hit(line), Some(name), "{line:?}");
    }
    for line in [
        "   testing the parser",
        "   +1 more running",
        "   +2 more  (list...",
        &pad("mux-agents", "2 running / 1 ended"),
        &pad("\u{2713} docs-usage", "done  18m"),
        &pad("\u{2717} bump-deps", "exit 101  25m"),
        &pad("- spike-rmux", "stopped   1h"),
        " refresh failed",
        " Enter: focus  Esc: clear",
        "",
        "working   4m",
        " working",
        " * worker      working   4m",
        " o worker      waiting   3m",
        // ASCII-only glyphs never hit (clicks are off in ASCII), nor a header that leads with a count.
        " @ worker      done  30s",
        " mux-agents  1 need you / 3 running",
        "\u{25c9} worker",
        " \u{25c9} ",
        " \u{25c9}  worker",
    ] {
        assert_eq!(row_hit(line), None, "{line:?}");
    }
}

#[test]
fn step_goes_next_and_prev_in_drawn_order_and_wraps() {
    let drawn = [
        Target::new("b", "%11"),
        Target::new("c", "%12"),
        Target::new("a", "%10"),
    ];
    let name = |t: Option<&Target>| t.map(|t| t.name.clone());
    assert_eq!(name(step(&drawn, None, true)), Some("b".into()));
    assert_eq!(name(step(&drawn, Some("%1"), false)), Some("a".into()));
    assert_eq!(name(step(&drawn, Some("%11"), true)), Some("c".into()));
    assert_eq!(name(step(&drawn, Some("%10"), true)), Some("b".into()));
    assert_eq!(name(step(&drawn, Some("%11"), false)), Some("a".into()));
    assert_eq!(name(step(&[], None, true)), None);
    // The selection keys never act on n, N or p.
    let mut pick = Pick::default();
    for e in [Event::Next, Event::Prev, Event::Peek] {
        assert_eq!(pick.key(e, &drawn), Act::Nothing);
    }
    assert_eq!(pick.chosen(&drawn), None);
    pick.key(Event::Down, &drawn);
    assert_eq!(pick.chosen(&drawn).map(|t| t.name.as_str()), Some("b"));
}

#[test]
fn selection_rules() {
    let three = drawn(&["a", "bb", "ccc"]);
    let mut pick = Pick::default();
    assert_eq!(pick.key(Event::Down, &three), Act::Redraw);
    assert_eq!(pick.selected(), Some("a"));
    assert_eq!(pick.key(Event::Down, &three), Act::Redraw);
    assert_eq!(pick.selected(), Some("bb"), "one step at a time");
    assert_eq!(pick.key(Event::Down, &three), Act::Redraw);
    assert_eq!(pick.selected(), Some("ccc"));
    assert_eq!(pick.key(Event::Down, &three), Act::Redraw);
    assert_eq!(pick.selected(), Some("ccc"), "no wrap at the bottom");

    let mut pick = Pick::default();
    assert_eq!(pick.key(Event::Up, &three), Act::Redraw);
    assert_eq!(pick.selected(), Some("ccc"));
    assert_eq!(pick.key(Event::Up, &three), Act::Redraw);
    assert_eq!(pick.selected(), Some("bb"), "one step at a time");
    assert_eq!(pick.key(Event::Up, &three), Act::Redraw);
    assert_eq!(pick.selected(), Some("a"));
    assert_eq!(pick.key(Event::Up, &three), Act::Redraw);
    assert_eq!(pick.selected(), Some("a"), "no wrap at the top");

    // A mouse event or Ctrl-C is not a key: nothing changes, not even when the drawn agents did.
    let mut pick = Pick::default();
    pick.key(Event::Down, &three);
    pick.failed("x");
    let moved = drawn(&["zz"]);
    for event in [
        Event::Press {
            button: 0,
            x: 1,
            y: 1,
        },
        Event::Release {
            button: 0,
            x: 1,
            y: 1,
        },
        Event::Interrupt,
    ] {
        assert_eq!(pick.key(event, &moved), Act::Nothing, "{event:?}");
        assert_eq!(pick.selected(), Some("a"), "{event:?}");
        assert!(pick.note().is_some(), "{event:?}");
    }

    let mut pick = Pick::default();
    assert_eq!(pick.key(Event::Enter, &three), Act::Redraw);
    assert_eq!(pick.selected(), Some("a"));
    pick.key(Event::Down, &three);
    assert_eq!(pick.key(Event::Enter, &three), Act::Focus(target("bb")));
    assert_eq!(pick.selected(), Some("bb"));
    pick.focused();
    assert_eq!(pick.selected(), None);

    pick.key(Event::Down, &three);
    assert_eq!(pick.key(Event::Esc, &three), Act::Redraw);
    assert_eq!(pick.selected(), None);

    // Nothing drawn: no key selects anything.
    for event in [Event::Down, Event::Up, Event::Enter, Event::Esc] {
        assert_eq!(pick.key(event, &[]), Act::Redraw);
        assert_eq!(pick.selected(), None);
    }
}

#[test]
fn selection_follows_name_then_ordinal() {
    let mut pick = Pick::default();
    pick.key(Event::Down, &drawn(&["a", "bb", "ccc"]));
    pick.key(Event::Down, &drawn(&["a", "bb", "ccc"]));
    // Reordered: the same name, at its new ordinal (the next Down moves on from there).
    let reordered = drawn(&["bb", "ccc", "a"]);
    pick.follow(&reordered);
    assert_eq!(pick.selected(), Some("bb"));
    pick.key(Event::Down, &reordered);
    assert_eq!(pick.selected(), Some("ccc"));

    // The second of three selected, then gone: the agent now at ordinal 1.
    let mut pick = Pick::default();
    pick.key(Event::Down, &drawn(&["a", "bb", "ccc"]));
    pick.key(Event::Down, &drawn(&["a", "bb", "ccc"]));
    pick.follow(&drawn(&["a", "ccc"]));
    assert_eq!(pick.selected(), Some("ccc"));

    // The last selected, then gone: the new last.
    let mut pick = Pick::default();
    pick.key(Event::Up, &drawn(&["a", "bb", "ccc"]));
    pick.follow(&drawn(&["a", "bb"]));
    assert_eq!(pick.selected(), Some("bb"));

    pick.follow(&[]);
    assert_eq!(pick.selected(), None);
    pick.follow(&drawn(&["a"]));
    assert_eq!(pick.selected(), None, "a cleared selection stays cleared");
}

#[test]
fn failed_note_lasts_two_ticks_or_one_key() {
    let mut pick = Pick::default();
    assert_eq!(pick.note(), None);
    pick.failed("worker");
    assert_eq!(pick.note().as_deref(), Some("focus failed: worker"));
    pick.ticked();
    assert_eq!(pick.note().as_deref(), Some("focus failed: worker"));
    pick.ticked();
    assert_eq!(pick.note(), None);

    // Every key dismisses it.
    for event in [Event::Down, Event::Up, Event::Enter, Event::Esc] {
        pick.failed("worker");
        assert_ne!(pick.key(event, &drawn(&["a"])), Act::Nothing, "{event:?}");
        assert_eq!(pick.note(), None, "{event:?}");
    }

    pick.failed("worker");
    pick.pressed();
    assert_eq!(pick.note(), None);
}

#[test]
fn selection_follows_the_member_id_not_the_name() {
    let a = Target {
        id: "aaaaaaaaaaaa".into(),
        ..Target::new("twin", "%1")
    };
    let b = Target {
        id: "bbbbbbbbbbbb".into(),
        ..Target::new("twin", "")
    };
    let mut pick = Pick::default();
    pick.key(Event::Down, &[a.clone(), b.clone()]);
    pick.key(Event::Down, &[a.clone(), b.clone()]);
    assert_eq!(pick.selected(), Some("bbbbbbbbbbbb"));
    let swapped = [b.clone(), a.clone()];
    pick.follow(&swapped);
    assert_eq!(pick.chosen(&swapped), Some(&b));
    assert_eq!(pick.key(Event::Enter, &swapped), Act::Focus(b.clone()));
    pick.key(Event::Up, &swapped);
    assert_eq!(
        pick.selected(),
        Some("bbbbbbbbbbbb"),
        "no wrap outside a team"
    );
    pick.set_wrap(true);
    pick.key(Event::Up, &swapped);
    assert_eq!(pick.selected(), Some("aaaaaaaaaaaa"), "a team wraps (B10)");
    pick.key(Event::Down, &swapped);
    assert_eq!(pick.selected(), Some("bbbbbbbbbbbb"));
    pick.select(&swapped, 1);
    assert_eq!(pick.selected(), Some("aaaaaaaaaaaa"));
    assert_eq!(
        (
            step_live(&swapped, Some(1), true),
            step_live(&swapped, None, false)
        ),
        (Some(1), Some(1))
    );
    assert_eq!(
        step_live(std::slice::from_ref(&b), None, true),
        None,
        "n/N skip agents without a live view"
    );
    pick.nothing("twin");
    assert_eq!(pick.note().as_deref(), Some("nothing to open for twin"));
    let row =
        json!({"id": "x", "name": "e\u{1b}", "state": "stopped", "life": "ended", "pane": null});
    let t = Target::of(row.as_object().unwrap());
    assert_eq!(
        (
            t.id.as_str(),
            t.name.as_str(),
            t.pane.as_str(),
            t.life.as_str()
        ),
        ("x", "e", "", "ended")
    );
}

#[test]
fn frame_hit_needs_the_width_the_row_and_its_text() {
    let line = "\u{25c9} twin           working   4m".to_string();
    let drawn = vec![
        ("aaaaaaaaaaaa".to_string(), 1, line.clone()),
        ("bbbbbbbbbbbb".to_string(), 2, line.clone()),
    ];
    let screen = format!(" mux-agents\n {line}\n\u{f054}{line}\n   twin is doing it\n");
    let answer = format!("40\n{screen}");
    assert_eq!(frame_hit(&answer, 40, 2, &drawn), Some("aaaaaaaaaaaa"));
    assert_eq!(
        frame_hit(&answer, 40, 3, &drawn),
        Some("bbbbbbbbbbbb"),
        "the clicked one of two names"
    );
    for y in [0, 1, 4, 9] {
        assert_eq!(
            frame_hit(&answer, 40, y, &drawn),
            None,
            "{y}: header, doing line, past the end"
        );
    }
    assert_eq!(
        frame_hit(&format!("41\n{screen}"), 40, 2, &drawn),
        None,
        "reflowed"
    );
    assert_eq!(
        frame_hit(
            "40\n mux-agents\n \u{25c9} other  working   4m\n",
            40,
            2,
            &drawn
        ),
        None,
        "redrawn"
    );
}
