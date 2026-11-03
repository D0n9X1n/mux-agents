//! What a click or a key in the side bar picks: the agent on the clicked row, and the keyboard selection.
#![allow(dead_code)] // Until T9 gives every item a caller (#61).

use serde_json::{Map, Value};

use super::keys::Event;

/// A live agent the bar can focus: its name and its pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub pane: String,
}

/// The answer of `display-message -p '#{pane_width}' ; capture-pane -p`: its first line is the width, then the rows.
/// None unless that width is `width` (the frame on screen) and row `y` (1-based) is a name line. A pane of another
/// width reflowed the frame, so its rows are not the rows the user clicked.
pub fn screen_hit(answer: &str, width: usize, y: u16) -> Option<&str> {
    let (head, rows) = answer.split_once('\n')?;
    if head.trim_end_matches('\r').parse::<usize>().ok()? != width || y == 0 {
        return None;
    }
    row_hit(rows.lines().nth(usize::from(y) - 1)?)
}

/// The name on a bar name line: margin (' ', '▸', '>'), '◉' or '◌', ' ', then the name to the next space.
pub fn row_hit(line: &str) -> Option<&str> {
    let mut chars = line.chars();
    let margin = matches!(chars.next()?, ' ' | '\u{25b8}' | '>');
    let glyph = matches!(chars.next()?, '\u{25c9}' | '\u{25cc}');
    if !margin || !glyph || chars.next()? != ' ' {
        return None;
    }
    let rest = chars.as_str();
    let name = &rest[..rest.find(' ').unwrap_or(rest.len())];
    (!name.is_empty()).then_some(name)
}

/// The running agent `shown` names, each board field cleaned as the bar drew it: by prefix when `shown` ends in
/// `...` (a cut name), else exactly. None unless exactly one row matches.
pub fn resolve(board: &Map<String, Value>, shown: &str) -> Option<Target> {
    let rows = board.get("agents").and_then(Value::as_array);
    let cut = shown.strip_suffix("...");
    let mut found = rows
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(Value::as_object)
        .filter(|row| super::clean(row.get("state")) == "running")
        .map(|row| Target {
            name: super::clean(row.get("name")),
            pane: super::clean(row.get("pane")),
        })
        .filter(|target| match cut {
            Some(head) => target.name.starts_with(head),
            None => target.name == shown,
        });
    let one = found.next()?;
    found.next().is_none().then_some(one)
}

/// What the bar does after a key: nothing, draw again, or focus an agent.
#[derive(Debug, PartialEq, Eq)]
pub enum Act {
    Nothing,
    Redraw,
    Focus(Target),
}

/// The keyboard selection (a name and its ordinal among the drawn agents) and the `focus failed` note (a name and
/// the ticks it has been shown).
#[derive(Debug, Default)]
pub struct Pick {
    chosen: Option<(String, usize)>,
    failed: Option<(String, u8)>,
}

impl Pick {
    /// A key against the agents drawn now. Down and Up select the top or bottom agent first, then move one at a time
    /// with no wrap; Enter selects the top agent first, then focuses the selected one; Esc clears. Every key dismisses
    /// the note and draws again; any other event does nothing.
    pub fn key(&mut self, event: Event, drawn: &[Target]) -> Act {
        self.follow(drawn);
        let at = self.chosen.as_ref().map(|(_, at)| *at);
        let next = match (event, at, drawn.len().checked_sub(1)) {
            (Event::Press { .. } | Event::Release { .. } | Event::Interrupt, _, _) => {
                return Act::Nothing;
            }
            (Event::Esc, _, _) | (_, _, None) => None,
            (Event::Enter, Some(at), _) => Some(at),
            (Event::Down | Event::Enter, None, _) => Some(0),
            (Event::Down, Some(at), Some(last)) => Some((at + 1).min(last)),
            (Event::Up, None, Some(last)) => Some(last),
            (Event::Up, Some(at), _) => Some(at.saturating_sub(1)),
        };
        self.failed = None;
        self.chosen = next.map(|at| (drawn[at].name.clone(), at));
        match (event, at.and_then(|at| drawn.get(at))) {
            (Event::Enter, Some(target)) => Act::Focus(target.clone()),
            _ => Act::Redraw,
        }
    }

    /// A mouse press: it dismisses the note.
    pub fn pressed(&mut self) {
        self.failed = None;
    }

    /// The selection against the agents drawn now: the same name where it is now, else the agent at its former
    /// ordinal, else the last; with none drawn it clears.
    pub fn follow(&mut self, drawn: &[Target]) {
        let Some((name, at)) = self.chosen.take() else {
            return;
        };
        let at = drawn
            .iter()
            .position(|target| target.name == name)
            .or_else(|| drawn.len().checked_sub(1).map(|last| at.min(last)));
        self.chosen = at.map(|at| (drawn[at].name.clone(), at));
    }

    /// The selected agent's name.
    pub fn selected(&self) -> Option<&str> {
        self.chosen.as_ref().map(|(name, _)| name.as_str())
    }

    /// The selected agent was focused: the selection clears.
    pub fn focused(&mut self) {
        self.chosen = None;
    }

    /// Focusing `name` failed: the note shows it until a key, a press or its second tick.
    pub fn failed(&mut self, name: &str) {
        self.failed = Some((name.to_string(), 0));
    }

    /// One tick of the bar: the note clears at its second.
    pub fn ticked(&mut self) {
        if let Some((_, ticks)) = self.failed.as_mut() {
            *ticks += 1;
            if *ticks >= 2 {
                self.failed = None;
            }
        }
    }

    /// The note the bar draws while a focus failure is shown.
    pub fn note(&self) -> Option<String> {
        self.failed
            .as_ref()
            .map(|(name, _)| format!("focus failed: {name}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // A row of a 40-column bar: `left` after the margin, `right` ending one column short of the edge.
    fn pad(left: &str, right: &str) -> String {
        let used = left.chars().count() + right.chars().count();
        format!(" {left}{}{right}", " ".repeat(38 - used))
    }

    // The bar at 40 columns as `capture-pane -p` gives it back (`sidebar::tests::board_rows` is private).
    fn frame() -> String {
        [
            pad("mux-agents", "2 running / 1 ended"),
            String::new(),
            pad("\u{25c9} worker", "working   4m"),
            "   testing the parser".to_string(),
            String::new(),
            pad("\u{25cc} writer-two", "waiting   3m"),
            String::new(),
            format!(" ended {}", "-".repeat(31)),
            pad("\u{2713} docs-usage", "done  18m"),
        ]
        .join("\n")
    }

    fn target(name: &str) -> Target {
        Target {
            name: name.into(),
            pane: format!("%{}", name.len()),
        }
    }

    fn drawn(names: &[&str]) -> Vec<Target> {
        names.iter().map(|name| target(name)).collect()
    }

    #[test]
    fn screen_hit_needs_the_frames_width() {
        let answer = format!("40\n{}\n", frame());
        assert_eq!(screen_hit(&answer, 40, 3), Some("worker"));
        assert_eq!(screen_hit(&answer, 40, 6), Some("writer-two"));
        // The header, the doing line, a blank line and an ended row are no name lines.
        for y in [1, 2, 4, 9] {
            assert_eq!(screen_hit(&answer, 40, y), None, "y={y}");
        }
        // The pane is narrower or wider than the frame on screen.
        assert_eq!(screen_hit(&format!("30\n{}\n", frame()), 40, 3), None);
        assert_eq!(screen_hit(&format!("55\n{}\n", frame()), 40, 3), None);
        assert_eq!(screen_hit(&format!("forty\n{}\n", frame()), 40, 3), None);
        assert_eq!(screen_hit(&answer, 40, 0), None);
        assert_eq!(screen_hit(&answer, 40, 10), None);
        assert_eq!(screen_hit("", 40, 1), None);
        // The frame at 40 shows `writer-two`; a capture at width 9 reflows its head to ` ◉ writer`, a whole name line
        // on its own, and is refused for its width.
        let split = "9\n mux-agen\nts  2 run\n \u{25c9} writer\n-two   wa\niting   3m\n";
        assert_eq!(row_hit(" \u{25c9} writer"), Some("writer"));
        assert_eq!(screen_hit(split, 40, 3), None);
        assert_eq!(screen_hit(split, 9, 3), Some("writer"));
    }

    #[test]
    fn row_hit_on_captured_lines() {
        for (line, name) in [
            (" \u{25c9} worker      working   4m", "worker"),
            (
                "\u{25b8}\u{25cc} test-calc  \u{2709} 1  waiting  3m",
                "test-calc",
            ),
            (">\u{25c9} worker", "worker"),
            (" \u{25c9} work...", "work..."),
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
            "\u{25c9} worker",
            " \u{25c9} ",
            " \u{25c9}  worker",
        ] {
            assert_eq!(row_hit(line), None, "{line:?}");
        }
    }

    #[test]
    fn resolve_unique_cut_and_ambiguous() {
        let board = |agents: Value| json!({ "agents": agents }).as_object().unwrap().clone();
        let worker = json!({"name": "worker", "state": "running", "pane": "%9"});
        let word = json!({"name": "word", "state": "running", "pane": "%4"});
        let ended = json!({"name": "worker", "state": "exited", "pane": "%3"});
        let found = Some(Target {
            name: "worker".into(),
            pane: "%9".into(),
        });
        let one = board(json!([worker, ended]));
        assert_eq!(resolve(&one, "worker"), found);
        assert_eq!(resolve(&one, "wor..."), found);
        // Only a cut name (ending in `...`) matches by prefix; any other must be the whole name.
        assert_eq!(resolve(&one, "work"), None);
        assert_eq!(resolve(&board(json!([worker, word])), "wor..."), None);
        assert_eq!(
            resolve(&board(json!([worker, word])), "word"),
            Some(Target {
                name: "word".into(),
                pane: "%4".into(),
            })
        );
        assert_eq!(resolve(&board(json!([ended])), "worker"), None);
        assert_eq!(resolve(&one, "nobody"), None);
        // A board field is cleaned before it is compared, as the bar drew it.
        let dirty = board(json!([{"name": "work\u{1b}er", "state": "running\n", "pane": "%9"}]));
        assert_eq!(resolve(&dirty, "worker"), found);
    }

    #[test]
    fn selection_rules() {
        let three = drawn(&["a", "bb", "ccc"]);
        let mut pick = Pick::default();
        assert_eq!(pick.key(Event::Down, &three), Act::Redraw);
        assert_eq!(pick.selected(), Some("a"));
        pick.key(Event::Down, &three);
        pick.key(Event::Down, &three);
        assert_eq!(pick.key(Event::Down, &three), Act::Redraw);
        assert_eq!(pick.selected(), Some("ccc"), "no wrap at the bottom");

        let mut pick = Pick::default();
        assert_eq!(pick.key(Event::Up, &three), Act::Redraw);
        assert_eq!(pick.selected(), Some("ccc"));
        pick.key(Event::Up, &three);
        pick.key(Event::Up, &three);
        assert_eq!(pick.key(Event::Up, &three), Act::Redraw);
        assert_eq!(pick.selected(), Some("a"), "no wrap at the top");

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

        pick.failed("worker");
        assert_eq!(pick.key(Event::Down, &drawn(&["a"])), Act::Redraw);
        assert_eq!(pick.note(), None);

        pick.failed("worker");
        pick.pressed();
        assert_eq!(pick.note(), None);
    }
}
