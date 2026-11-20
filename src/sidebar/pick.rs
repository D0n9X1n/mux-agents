//! What a click or a key in the side bar picks: the agent on the clicked row, and the keyboard selection.

use std::time::{Duration, SystemTime};

use serde_json::{Map, Value};

use super::keys::{Event, Read};

/// A kept shell's life: the bar asks before it closes one (views-6f).
pub const KEPT: &str = "kept";

/// The main chat's life: the pane the agents were spawned from, outside a team; it takes keys and clicks, never
/// peek, stop or `n`/`N`.
pub const MAIN: &str = "main";

/// The flat bar's ended fold line: Enter and Tab open and close it; it takes keys, never clicks, peek or `n`/`N`.
pub const FOLD: &str = "fold";

impl Target {
    /// The ended fold line's target: an id no member id or pane can take.
    pub fn fold() -> Target {
        Target {
            id: "fold:ended".into(),
            name: "Ended".into(),
            pane: String::new(),
            life: FOLD.into(),
            team: String::new(),
        }
    }
}

/// A main chat row: its pane is its identity, its name `main`, then `main 2`, `main 3` … in the board's order.
impl Target {
    pub fn main(pane: &str, nth: usize) -> Target {
        Target {
            id: format!("main:{pane}"),
            name: if nth == 0 {
                "main".into()
            } else {
                format!("main {}", nth + 1)
            },
            pane: pane.into(),
            life: MAIN.into(),
            team: String::new(),
        }
    }
}

/// The question Enter asks on a kept row; the next event answers it.
pub const ASK: &str = "close? y/n";

/// The question `q` asks; only a `y` read alone, `BAR_GAP` after the question showed, closes the bar.
pub const ASK_BAR: &str = "close bar? y/n";

/// How long `close bar? y/n` is on screen before a `y` counts: a `q` typed into the bar by mistake, then a `y`
/// typed fast for something else, never closes it.
pub const BAR_GAP: Duration = Duration::from_millis(250);

/// Whether `note` is a question waiting for its answer: it keeps the last line, and no hint takes it.
pub fn asking(note: &str) -> bool {
    note == ASK || note == ASK_BAR
}

/// The question pending: close a kept shell, or close the bar (with when the question was first drawn).
#[derive(Debug)]
enum Question {
    Kept(Target),
    Bar(Option<SystemTime>),
}

/// What an event made of the question pending: none was, it waits on, it was cancelled, or yes to closing the kept
/// shell or the bar.
#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    None,
    Wait,
    Cancel,
    Kept(Target),
    Bar,
}

/// An agent row the bar can open: its member id (its name on a feed without ids), name, pane and life.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub id: String,
    pub name: String,
    pub pane: String,
    pub life: String,
    /// A kept row's `mux.kept` label (restore-6c Task 11); empty for a member row.
    pub team: String,
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
            team: String::new(),
        }
    }

    /// A kept shell of the board (views-6f ambiguity 15): its hc address is its identity, its name is cleaned (the
    /// address when it has none), it has no pane and its life is `KEPT`, so it takes keys, never clicks.
    pub fn kept(row: &Map<String, Value>) -> Target {
        let (address, name) = (
            super::clean(row.get("address")),
            super::clean(row.get("name")),
        );
        Target {
            name: if name.is_empty() {
                address.clone()
            } else {
                name
            },
            id: address,
            pane: String::new(),
            life: KEPT.into(),
            team: super::clean(row.get("team")),
        }
    }

    #[cfg(test)]
    pub fn new(name: &str, pane: &str) -> Target {
        Target {
            id: name.into(),
            name: name.into(),
            pane: pane.into(),
            life: "running".into(),
            team: String::new(),
        }
    }
}

/// B10 `n`/`N`: the next (`forward`) or previous agent of `walk` after `from` with a live view, wrapping; from none,
/// the first (last) with one; none when no agent has one.
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

/// The agent row a press on row `y` (1-based) hit, from the drawn frame's metadata (B6): the answer of
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

/// The name on a bar name line: margin (' ', the selection chevron, '>'), a live glyph ('!', '✦', '◉', '◌'), ' ', then the name to
/// the next space.
#[cfg(test)]
pub fn row_hit(line: &str) -> Option<&str> {
    let mut chars = line.chars();
    let margin = matches!(chars.next()?, ' ' | '\u{f054}' | '>');
    let glyph = matches!(chars.next()?, '!' | '\u{2726}' | '\u{25c9}' | '\u{25cc}');
    if !margin || !glyph || chars.next()? != ' ' {
        return None;
    }
    let rest = chars.as_str();
    let name = &rest[..rest.find(' ').unwrap_or(rest.len())];
    (!name.is_empty()).then_some(name)
}

/// `g` / `G`: the index in `walk` of the target drawn highest (`first`) or lowest on the bar, by `rows` (each drawn
/// target's row); with none of them drawn, the walk's first or last. None for an empty walk.
pub fn edge(walk: &[Target], rows: &[(Target, usize)], first: bool) -> Option<usize> {
    let row = |t: &Target| rows.iter().find(|(d, _)| d.id == t.id).map(|(_, at)| *at);
    let drawn = (0..walk.len()).filter_map(|at| row(&walk[at]).map(|r| (r, at)));
    let picked = if first {
        drawn.min().map(|(_, at)| at)
    } else {
        drawn.max().map(|(_, at)| at)
    };
    picked.or_else(|| {
        if first {
            (!walk.is_empty()).then_some(0)
        } else {
            walk.len().checked_sub(1)
        }
    })
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

/// The keyboard selection (a member id and its ordinal among the drawn agents), whether `follow` put it on a row the
/// person never chose, the note (its text and the ticks it has been shown) and whether keys wrap.
#[derive(Debug, Default)]
pub struct Pick {
    chosen: Option<(String, usize)>,
    fell: bool,
    note: Option<(String, u8)>,
    wrap: bool,
    question: Option<Question>,
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
                | Event::Mail
                | Event::Next
                | Event::Prev
                | Event::Peek
                | Event::Yes
                | Event::Tab
                | Event::First
                | Event::Last
                | Event::Help
                | Event::Close
                | Event::Notes
                | Event::Placement
                | Event::Output
                | Event::Ack
                | Event::Other,
                _,
                _,
            ) => {
                return Act::Nothing;
            }
        };
        self.note = None;
        self.chosen = next.map(|at| (drawn[at].id.clone(), at));
        self.fell = false;
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

    /// `chosen`, only when the person chose it (a key, a click, `n`/`N`): a row the selection fell to when the one they
    /// chose left is none, so a key that marks or closes never acts on a row they did not pick.
    pub fn own<'a>(&self, drawn: &'a [Target]) -> Option<&'a Target> {
        self.chosen(drawn).filter(|_| !self.fell)
    }

    /// A mouse press: it dismisses the note.
    pub fn pressed(&mut self) {
        self.note = None;
    }

    /// The selection against the agents drawn now: the same member id where it is now, else the agent at its former
    /// ordinal, else the last, which the person did not choose (`own`); with none drawn it clears.
    pub fn follow(&mut self, drawn: &[Target]) {
        let Some((id, at)) = self.chosen.take() else {
            return;
        };
        let (at, fell) = match drawn.iter().position(|target| target.id == id) {
            Some(at) => (Some(at), self.fell),
            None => (drawn.len().checked_sub(1).map(|last| at.min(last)), true),
        };
        self.chosen = at.map(|at| (drawn[at].id.clone(), at));
        self.fell = fell && self.chosen.is_some();
    }

    /// The selected agent's member id.
    pub fn selected(&self) -> Option<&str> {
        self.chosen.as_ref().map(|(id, _)| id.as_str())
    }

    /// The selected agent was focused: the selection clears.
    pub fn focused(&mut self) {
        self.chosen = None;
        self.fell = false;
    }

    /// The person left the bar: the selection clears, unless a question waits for its answer.
    pub fn left(&mut self) {
        if self.question.is_none() {
            self.chosen = None;
            self.fell = false;
        }
    }

    /// Keys wrap at both ends: a team's bar, B10.
    pub fn set_wrap(&mut self, wrap: bool) {
        self.wrap = wrap;
    }

    /// Selects `drawn[at]` (`n`/`N` in a team), dismissing the note.
    pub fn select(&mut self, drawn: &[Target], at: usize) {
        self.note = None;
        self.chosen = drawn.get(at).map(|t| (t.id.clone(), at));
        self.fell = false;
    }

    /// Opening `name` showed nothing (B6).
    pub fn nothing(&mut self, name: &str) {
        self.note = Some((format!("nothing to open for {name}"), 0));
    }

    /// A note of the bar's own (a refused resume's reason, say), shown as `failed` is.
    pub fn say(&mut self, text: String) {
        self.note = Some((text, 0));
    }

    /// Focusing `name` failed: the note shows it until a key, a press or its second tick.
    pub fn failed(&mut self, name: &str) {
        self.note = Some((format!("focus failed: {name}"), 0));
    }

    /// One tick of the bar: the note clears at its second; a pending question stays until it is answered.
    pub fn ticked(&mut self) {
        if self.question.is_some() {
            return;
        }
        if let Some((_, ticks)) = self.note.as_mut() {
            *ticks += 1;
            if *ticks >= 2 {
                self.note = None;
            }
        }
    }

    /// Enter on a kept row asks first (views-6f ambiguity 15); the next event answers.
    pub fn ask(&mut self, target: Target) {
        self.note = Some((ASK.into(), 0));
        self.question = Some(Question::Kept(target));
    }

    /// `q` asks before the bar closes.
    pub fn ask_bar(&mut self) {
        self.note = Some((ASK_BAR.into(), 0));
        self.question = Some(Question::Bar(None));
    }

    /// A frame showing the question was written at `at`: the first such frame starts `BAR_GAP`.
    pub fn question_shown(&mut self, at: SystemTime) {
        if let Some(Question::Bar(shown @ None)) = self.question.as_mut() {
            *shown = Some(at);
        }
    }

    /// `read` against the question pending. A kept shell's question is answered by any event, yes only by `y`. The
    /// bar's is yes only by a `y` read alone at least `BAR_GAP` after it was first drawn; a `y` read sooner, or with
    /// other keys, leaves it asked; any other event cancels it.
    pub fn answer(&mut self, read: &Read) -> Answer {
        let yes = read.event == Event::Yes;
        let answer = match self.question.as_ref() {
            None => return Answer::None,
            Some(Question::Bar(shown)) if yes => {
                let late = shown.is_some_and(|at| read.first >= at + BAR_GAP);
                if !(read.alone && late) {
                    return Answer::Wait;
                }
                Answer::Bar
            }
            Some(Question::Kept(target)) if yes => Answer::Kept(target.clone()),
            Some(_) => Answer::Cancel,
        };
        self.question = None;
        self.note = None;
        answer
    }

    /// Closing kept shell (or the bar) `name` was refused.
    pub fn close_failed(&mut self, name: &str) {
        self.note = Some((format!("close failed: {name}"), 0));
    }

    /// The note the bar draws while one is shown: a focus failure, or nothing to open.
    pub fn note(&self) -> Option<String> {
        self.note.as_ref().map(|(text, _)| text.clone())
    }
}

#[cfg(test)]
mod tests;
