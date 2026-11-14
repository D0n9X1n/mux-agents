//! What a click or a key in the side bar picks: the agent on the clicked row, and the keyboard selection.

use serde_json::{Map, Value};

use super::keys::Event;

/// An agent row the bar can open: its member id (its name on a feed without ids), name, pane and life.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub id: String,
    pub name: String,
    pub pane: String,
    pub life: String,
}

impl Target {
    /// A board row as the bar draws it, each field cleaned.
    pub fn of(row: &Map<String, Value>) -> Target {
        let (name, id) = (super::clean(row.get("name")), super::clean(row.get("id")));
        let life = match super::clean(row.get("life")) {
            life if !life.is_empty() => life,
            _ if super::clean(row.get("state")) == "running" => "running".into(),
            _ => "ended".into(),
        };
        Target {
            id: if id.is_empty() { name.clone() } else { id },
            pane: super::clean(row.get("pane")),
            life,
            name,
        }
    }

    #[cfg(test)]
    pub fn new(name: &str, pane: &str) -> Target {
        Target {
            id: name.into(),
            name: name.into(),
            pane: pane.into(),
            life: "running".into(),
        }
    }
}

/// B10 `n`/`N`: the next (`forward`) or previous agent of `walk` after `from` with a live view, wrapping; from none,
/// the first (last) with one; none when no agent has one.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "shortcut: no caller until the team bar's keys and open (Tasks 11-12, #103); remove this then"
    )
)]
pub fn step_live(walk: &[Target], from: Option<usize>, forward: bool) -> Option<usize> {
    let n = walk.len();
    (1..=n)
        .map(|k| match (from, forward) {
            (Some(f), true) => (f + k) % n,
            (Some(f), false) => (f + n - k) % n,
            (None, true) => k - 1,
            (None, false) => n - k,
        })
        .find(|&at| !walk[at].pane.is_empty())
}

/// The agent row a press on row `y` (1-based) hit, from the drawn frame's metadata (#103 B6): the answer of
/// `display-message -p '#{pane_width}' ; capture-pane -p` must be at `width` (the frame's) and its line `y` must still show
/// that row's text after column 0 (margin, `●` or selection). Header, doing, fold and note lines are in no `drawn`
/// entry, so they hit nothing, whatever their text names.
pub fn frame_hit<'a>(
    answer: &str,
    width: usize,
    y: u16,
    drawn: &'a [(String, usize, String)],
) -> Option<&'a str> {
    let (head, rows) = answer.split_once('\n')?;
    if head.trim_end_matches('\r').parse::<usize>().ok()? != width || y == 0 {
        return None;
    }
    let at = usize::from(y) - 1;
    let (id, _, text) = drawn.iter().find(|(_, row, _)| *row == at)?;
    let line = rows.lines().nth(at)?;
    let rest = line.char_indices().nth(1).map_or("", |(i, _)| &line[i..]);
    (rest.trim_end() == text.trim_end()).then_some(id.as_str())
}

/// The name on a bar name line: margin (' ', '▸', '>'), a live glyph ('!', '✦', '◉', '◌'), ' ', then the name to
/// the next space.
#[cfg(test)]
pub fn row_hit(line: &str) -> Option<&str> {
    let mut chars = line.chars();
    let margin = matches!(chars.next()?, ' ' | '\u{25b8}' | '>');
    let glyph = matches!(chars.next()?, '!' | '\u{2726}' | '\u{25c9}' | '\u{25cc}');
    if !margin || !glyph || chars.next()? != ' ' {
        return None;
    }
    let rest = chars.as_str();
    let name = &rest[..rest.find(' ').unwrap_or(rest.len())];
    (!name.is_empty()).then_some(name)
}

/// herdr's next (`forward`) or previous agent: the drawn agent after (before) the one on `looking` (the pane the
/// session shows), wrapping; from a pane that is no drawn agent's, the first (last). None with none drawn.
pub fn step<'a>(drawn: &'a [Target], looking: Option<&str>, forward: bool) -> Option<&'a Target> {
    let n = drawn.len();
    if n == 0 {
        return None;
    }
    let at = looking.and_then(|pane| drawn.iter().position(|t| t.pane == pane));
    let to = match (at, forward) {
        (Some(at), true) => (at + 1) % n,
        (Some(at), false) => (at + n - 1) % n,
        (None, true) => 0,
        (None, false) => n - 1,
    };
    drawn.get(to)
}

/// What the bar does after a key: nothing, draw again, or focus an agent.
#[derive(Debug, PartialEq, Eq)]
pub enum Act {
    Nothing,
    Redraw,
    Focus(Target),
}

/// The keyboard selection (a member id and its ordinal among the drawn agents), the note (its text and the ticks it has
/// been shown) and whether keys wrap.
#[derive(Debug, Default)]
pub struct Pick {
    chosen: Option<(String, usize)>,
    note: Option<(String, u8)>,
    wrap: bool,
}

impl Pick {
    /// A key against the agents drawn now. Down and Up select the top or bottom agent first, then move one at a time,
    /// wrapping only in a team (`set_wrap`); Enter selects the top agent first, then focuses the selected one; Esc
    /// clears. Every key dismisses the note and draws again; any other event does nothing.
    pub fn key(&mut self, event: Event, drawn: &[Target]) -> Act {
        if !matches!(event, Event::Up | Event::Down | Event::Enter | Event::Esc) {
            return Act::Nothing;
        }
        self.follow(drawn);
        let at = self.chosen.as_ref().map(|(_, at)| *at);
        let next = match (event, at, drawn.len().checked_sub(1)) {
            (Event::Esc, _, _) | (_, _, None) => None,
            (Event::Enter, Some(at), _) => Some(at),
            (Event::Down | Event::Enter, None, _) => Some(0),
            (Event::Down, Some(at), Some(last)) => Some(if self.wrap && at == last {
                0
            } else {
                (at + 1).min(last)
            }),
            (Event::Up, None, Some(last)) => Some(last),
            (Event::Up, Some(at), Some(last)) => Some(if self.wrap && at == 0 {
                last
            } else {
                at.saturating_sub(1)
            }),
            // Not a selection key: returned above.
            (
                Event::Press { .. }
                | Event::Release { .. }
                | Event::Interrupt
                | Event::Next
                | Event::Prev
                | Event::Peek,
                _,
                _,
            ) => {
                return Act::Nothing;
            }
        };
        self.note = None;
        self.chosen = next.map(|at| (drawn[at].id.clone(), at));
        match (event, at.and_then(|at| drawn.get(at))) {
            (Event::Enter, Some(target)) => Act::Focus(target.clone()),
            _ => Act::Redraw,
        }
    }

    /// The selected agent, if any, among `drawn`.
    pub fn chosen<'a>(&self, drawn: &'a [Target]) -> Option<&'a Target> {
        let (id, _) = self.chosen.as_ref()?;
        drawn.iter().find(|t| t.id == *id)
    }

    /// A mouse press: it dismisses the note.
    pub fn pressed(&mut self) {
        self.note = None;
    }

    /// The selection against the agents drawn now: the same member id where it is now, else the agent at its former
    /// ordinal, else the last; with none drawn it clears.
    pub fn follow(&mut self, drawn: &[Target]) {
        let Some((id, at)) = self.chosen.take() else {
            return;
        };
        let at = drawn
            .iter()
            .position(|target| target.id == id)
            .or_else(|| drawn.len().checked_sub(1).map(|last| at.min(last)));
        self.chosen = at.map(|at| (drawn[at].id.clone(), at));
    }

    /// The selected agent's member id.
    pub fn selected(&self) -> Option<&str> {
        self.chosen.as_ref().map(|(id, _)| id.as_str())
    }

    /// The selected agent was focused: the selection clears.
    pub fn focused(&mut self) {
        self.chosen = None;
    }

    /// Keys wrap at both ends: a team's bar, #103 B10.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "shortcut: no caller until the team bar's keys and open (Tasks 11-12, #103); remove this then"
        )
    )]
    pub fn set_wrap(&mut self, wrap: bool) {
        self.wrap = wrap;
    }

    /// Selects `drawn[at]` (`n`/`N` in a team), dismissing the note.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "shortcut: no caller until the team bar's keys and open (Tasks 11-12, #103); remove this then"
        )
    )]
    pub fn select(&mut self, drawn: &[Target], at: usize) {
        self.note = None;
        self.chosen = drawn.get(at).map(|t| (t.id.clone(), at));
    }

    /// Opening `name` showed nothing (#103 B6).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "shortcut: no caller until the team bar's keys and open (Tasks 11-12, #103); remove this then"
        )
    )]
    pub fn nothing(&mut self, name: &str) {
        self.note = Some((format!("nothing to open for {name}"), 0));
    }

    /// Focusing `name` failed: the note shows it until a key, a press or its second tick.
    pub fn failed(&mut self, name: &str) {
        self.note = Some((format!("focus failed: {name}"), 0));
    }

    /// One tick of the bar: the note clears at its second.
    pub fn ticked(&mut self) {
        if let Some((_, ticks)) = self.note.as_mut() {
            *ticks += 1;
            if *ticks >= 2 {
                self.note = None;
            }
        }
    }

    /// The note the bar draws while one is shown: a focus failure, or nothing to open.
    pub fn note(&self) -> Option<String> {
        self.note.as_ref().map(|(text, _)| text.clone())
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
                "\u{25b8}\u{25cc} test-calc  \u{2709} 1  waiting  3m",
                "test-calc",
            ),
            (">\u{25c9} worker", "worker"),
            (" \u{25c9} work...", "work..."),
            (" ! fix-ci       needs you  12s", "fix-ci"),
            (
                "\u{25b8}\u{2726} test-calc  \u{2709} 1  done  30s",
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
        let row = json!({"id": "x", "name": "e\u{1b}", "state": "stopped", "life": "ended", "pane": null});
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
        let screen = format!(" mux-agents\n\u{25cf}{line}\n\u{25b8}{line}\n   twin is doing it\n");
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
}
