//! A team's side bar (phase 5): the team as a tree, leader first, each member's children under it.

use serde_json::{Map, Value};

use super::pick::Target;
use super::*;

/// One drawn row of the tree: its board row, its drawn depth, and whether its chain is broken (drawn at 1 with `^`).
pub(super) struct Node<'a> {
    pub row: &'a Map<String, Value>,
    pub depth: usize,
    pub broken: bool,
}

/// Whether the board is a team's feed (B1).
pub(super) fn is_team(board: Option<&Map<String, Value>>) -> bool {
    board.and_then(|b| b.get("team")) == Some(&Value::Bool(true))
}

fn rows(board: &Map<String, Value>) -> Vec<&Map<String, Value>> {
    board
        .get("agents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .collect()
}

/// A running sibling's W6 rank, as the board ranks: needs you, on call, done, working, idle.
fn live_rank(row: &Map<String, Value>) -> u8 {
    match Kind::live(row) {
        Kind::Blocked => 0,
        Kind::OnCall => 1,
        Kind::Done => 2,
        Kind::Working => 3,
        _ => 4,
    }
}

/// `kids`, oldest first, with the running ones re-ordered by `live_rank` (age within a rank) in the places running rows
/// hold; a row that is not running keeps its place, so an ended member's subtree never moves.
fn rank_siblings<'a>(kids: &mut [&'a Map<String, Value>]) {
    let running = |r: &Map<String, Value>| clean(r.get("state")) == "running";
    let slots: Vec<usize> = (0..kids.len()).filter(|&i| running(kids[i])).collect();
    let mut live: Vec<&'a Map<String, Value>> = slots.iter().map(|&i| kids[i]).collect();
    // Stable: rows of one rank keep their age order.
    live.sort_by_key(|r| live_rank(r));
    for (slot, row) in slots.into_iter().zip(live) {
        kids[slot] = row;
    }
}

/// B2/B3: the leader, then each member's children directly under it, oldest `createdAt` first (ties by id), the running
/// ones in their W6 rank (`rank_siblings`). A row whose parent is not in the team, or whose chain repeats or ends
/// elsewhere than the leader, follows at depth 1, broken, oldest first; its own children are broken too. Every row is
/// placed once, so this always ends.
pub(super) fn order(board: &Map<String, Value>) -> Vec<Node<'_>> {
    let all = rows(board);
    let id = |r: &Map<String, Value>| clean(r.get("id"));
    let parent = |r: &Map<String, Value>| clean(r.get("parentId"));
    let key = |r: &&Map<String, Value>| (clean(r.get("createdAt")), id(r));
    let leader = all
        .iter()
        .copied()
        .find(|r| r.get("leader") == Some(&Value::Bool(true)));
    let attached = |r: &Map<String, Value>| {
        let (mut at, mut seen) = (r, Vec::new());
        loop {
            if leader.is_some_and(|l| std::ptr::eq(l, at)) {
                return true;
            }
            if seen.iter().any(|s| std::ptr::eq(*s, at)) {
                return false;
            }
            seen.push(at);
            match all
                .iter()
                .copied()
                .find(|p| id(p) == parent(at) && !parent(at).is_empty())
            {
                Some(p) => at = p,
                None => return false,
            }
        }
    };
    let mut out = Vec::new();
    if let Some(l) = leader {
        let mut stack = vec![(l, 0)];
        while let Some((r, depth)) = stack.pop() {
            out.push(Node {
                row: r,
                depth,
                broken: false,
            });
            let mut kids: Vec<_> = all
                .iter()
                .copied()
                .filter(|k| !std::ptr::eq(*k, l) && parent(k) == id(r) && attached(k))
                .collect();
            kids.sort_by_key(key);
            rank_siblings(&mut kids);
            stack.extend(kids.into_iter().rev().map(|k| (k, depth + 1)));
        }
    }
    let mut broken: Vec<_> = all.iter().copied().filter(|r| !attached(r)).collect();
    broken.sort_by_key(key);
    out.extend(broken.into_iter().map(|row| Node {
        row,
        depth: 1,
        broken: true,
    }));
    out
}

/// Every agent of a team board in tree order, folded or not: what Up/Down walk (B10).
pub(super) fn walk(board: &Map<String, Value>) -> Vec<Target> {
    order(board).iter().map(|n| Target::of(n.row)).collect()
}

/// What the bar marks (B4, B5): the pane the session shows, and the selected member id.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Focus<'a> {
    pub shown: Option<&'a str>,
    pub selected: Option<&'a str>,
}

struct Member {
    line: Row,
    doing: Option<Row>,
    target: Target,
    depth: usize,
    ended: bool,
    parent: Option<usize>,
    kids: Vec<usize>,
    need: usize,
}

/// One tree row (B2-B5, B8 and the PM narrow-width rule in `bar-plan.md`). Mandatory: column 0 (`●`/`#` on the shown agent,
/// else a space), the indent (exactly 2 columns per depth; `^` first on a broken row), the glyph, the whole state word of an
/// ended, lost, unknown or recovery row (exit code included) and the unread badge. Optional, dropped in order: the doing line,
/// the model, the age, the name (cut with `…`, then removed); below 40 columns a live row shows only its age, at 40 and wider
/// its word is mandatory too. `need` is the bar width the mandatory parts take, column 0 and the free last column included.
fn member(node: &Node, width: usize, shown: Option<&str>, look: Look) -> Member {
    let (row, cols, narrow) = (
        node.row,
        width.saturating_sub(2),
        width < MIN_WIDTH as usize,
    );
    let (target, name, age) = (
        Target::of(row),
        clean(row.get("name")),
        format_age(row.get("age")),
    );
    // A running recovery row's bare pill (call 21), tried only after its fuller word fails even without the name.
    let mut bare = None;
    // (glyph, name, model, badge, the sides fullest first, whether the barest side is mandatory, the doing text)
    let (glyph, label, model, badge, sides, kept, doing) =
        if row.get("needsRecovery") == Some(&Value::Bool(true)) {
            let hue = paint::hue(paint::identity(
                row.get("color").and_then(Value::as_str),
                &name,
            ));
            let running = row.get("recoveryRunning") == Some(&Value::Bool(true));
            let pill = vec![span("recover", Kind::Blocked.word_style())];
            // Calls 10, 21: a running row keeps `(still running)` while it can (its name cut first), then the pill.
            let sides = if narrow {
                vec![pill]
            } else if running {
                let word = "recover (still running)";
                bare = Some(pill);
                vec![
                    right(Kind::Blocked, word, &age, false),
                    vec![span(word, Kind::Blocked.word_style())],
                ]
            } else {
                // restore-6c checklist 18 (P1): the reason beside `recover` while it fits, then the bare word.
                let reason = clean(row.get("recoveryReason"));
                let mut sides = Vec::new();
                if !reason.is_empty() {
                    sides.push(right(
                        Kind::Blocked,
                        &format!("recover: {reason}"),
                        &age,
                        false,
                    ));
                }
                sides.extend([right(Kind::Blocked, "recover", &age, false), pill]);
                sides
            };
            (
                span("!", Kind::Blocked.style()),
                span(
                    own_glyphs(&name),
                    Style {
                        color: hue,
                        ..PLAIN
                    },
                ),
                Vec::new(),
                unread_badge(row, look),
                sides,
                true,
                String::new(),
            )
        } else if clean(row.get("state")) == "running" {
            let p = live_parts(row, look);
            let bare = vec![span(
                format!("{:>4}", p.age),
                if p.muted { DIM } else { PLAIN },
            )];
            let sides = if narrow {
                vec![bare, Vec::new()]
            } else {
                vec![
                    right(p.kind, p.kind.word(), &p.age, p.muted),
                    vec![span(p.kind.word(), p.kind.word_style())],
                ]
            };
            // B5: a live row's word is optional only below 40 columns; at 40 and wider it is mandatory.
            (p.glyph, p.name, p.model, p.badge, sides, !narrow, p.doing)
        } else {
            let (kind, word) = ended_kind(row);
            let sides = vec![
                right(kind, &word, &age, false),
                vec![span(word.clone(), kind.word_style())],
            ];
            (
                span(kind.glyph(look.ascii), kind.style()),
                span(own_glyphs(&name), DIM),
                Vec::new(),
                unread_badge(row, look),
                sides,
                true,
                String::new(),
            )
        };
    let pad = 2 * node.depth;
    let floor = 2
        + row_width(&badge)
        + if kept {
            2 + bare.as_ref().or(sides.last()).map_or(0, |s| row_width(s))
        } else {
            0
        };
    let parts = RowParts {
        glyph,
        name: label,
        model,
        badge,
        sides,
        bare,
        kept,
        doing: paint::width(&doing),
    };
    let (fitted, with_doing) = fit_row(&parts, cols.saturating_sub(pad), look.ascii);
    let mut line = margin(fitted);
    // Column 0 is the selection's; the member on screen is banded (`sidebar::shade`), never marked here.
    let _ = shown;
    line[0] = span(" ", PLAIN);
    line.insert(
        1,
        span(
            if node.broken {
                format!("^{}", " ".repeat(pad.saturating_sub(1)))
            } else {
                " ".repeat(pad)
            },
            PLAIN,
        ),
    );
    let ended = target.life == "ended";
    if ended {
        line.iter_mut().for_each(|(_, s)| s.dim = true);
    }
    let doing = with_doing.then(|| {
        let mut d = indented(&doing, cols - pad);
        d.insert(0, span(" ".repeat(pad), PLAIN));
        d
    });
    Member {
        line,
        doing,
        target,
        depth: node.depth,
        ended,
        parent: None,
        kids: Vec::new(),
        need: pad + floor + 2,
    }
}

/// One agent line's parts for `fit_row` (A5: one argument, not eight).
struct RowParts {
    glyph: Span,
    name: Span,
    model: Row,
    badge: Row,
    sides: Vec<Row>,
    /// Tried after every side and the name cut and removed beside the last: a running recovery row's bare pill (call
    /// 21), which is then also the floor.
    bare: Option<Row>,
    kept: bool,
    doing: usize,
}

/// One agent line in exactly `cols` columns and whether its doing line stays, in the one drop order (A1, the PM
/// narrow-width rule): everything; without the doing line; without the model; each barer side (the age, then a live row's
/// word below 40 columns); the name cut with `…` (`.` in ASCII); the name removed; last the mandatory floor, which may be
/// wider than `cols` (the bar then asks to be widened).
fn fit_row(p: &RowParts, cols: usize, ascii: bool) -> (Row, bool) {
    let last = p.sides.last().cloned().unwrap_or_default();
    let build = |text: &str, model: &[Span], side: &Row| -> (usize, Row) {
        let gap = if side.is_empty() { 0 } else { 2 };
        let used =
            2 + paint::width(text) + row_width(model) + row_width(&p.badge) + gap + row_width(side);
        let mut out = vec![p.glyph.clone(), span(" ", PLAIN)];
        if !text.is_empty() {
            out.push(span(text, p.name.1));
        }
        out.extend(model.iter().cloned());
        out.extend(p.badge.iter().cloned());
        out.push(span(" ".repeat(cols.saturating_sub(used) + gap), PLAIN));
        out.extend(side.iter().cloned());
        (used, out)
    };
    let full = build(&p.name.0, &p.model, &p.sides[0]);
    // A1: the doing line goes first: it stays only beside the whole row, and only when its text fits whole.
    if full.0 <= cols && p.doing > 0 && p.doing + 2 <= cols {
        return (full.1, true);
    }
    let cut = cut_name(&p.name.0, cols, &p.badge, &last, ascii);
    let mut tries = vec![full];
    tries.extend(p.sides.iter().map(|s| build(&p.name.0, &[], s)));
    tries.extend([build(&cut, &[], &last), build("", &[], &last)]);
    if let Some(bare) = &p.bare {
        let cut = cut_name(&p.name.0, cols, &p.badge, bare, ascii);
        tries.extend([
            build(&p.name.0, &[], bare),
            build(&cut, &[], bare),
            build("", &[], bare),
        ]);
    }
    let floor = p.bare.as_ref().unwrap_or(&last);
    let row = tries
        .into_iter()
        .find(|(used, _)| *used <= cols)
        .map(|(_, row)| row)
        .unwrap_or_else(|| build("", &[], &if p.kept { floor.clone() } else { Vec::new() }).1);
    (row, false)
}

/// `name` cut to what is left beside the glyph, the badge and `side`, ending in `…` (`.` in ASCII); empty when not one
/// character and the cut mark fit.
fn cut_name(name: &str, cols: usize, badge: &Row, side: &Row, ascii: bool) -> String {
    let gap = if side.is_empty() { 0 } else { 2 };
    let room = cols.saturating_sub(2 + row_width(badge) + gap + row_width(side));
    if room < 2 {
        return String::new();
    }
    format!(
        "{}{}",
        paint::fit(name, room - 1),
        if ascii { "." } else { "\u{2026}" }
    )
}

/// What a frame keeps (B5): the blank line, each doing line, each parent's ended rows folded, each hidden sub-tree, notes.
#[derive(Clone)]
struct Cut {
    gap: bool,
    doing: Vec<bool>,
    folded: Vec<bool>,
    hidden: Vec<bool>,
    notes: usize,
}

struct Team {
    header: Row,
    members: Vec<Member>,
    roots: Vec<usize>,
    protected: Vec<bool>,
    notes: Vec<Row>,
    cols: usize,
}

impl Team {
    fn emit(&self, cut: &Cut) -> Laid {
        let mut laid = Laid {
            rows: vec![self.header.clone()],
            ..Laid::default()
        };
        if cut.gap {
            laid.rows.push(Row::new());
        }
        for &r in &self.roots {
            self.visit(r, cut, &mut laid);
        }
        if cut.notes > 0 {
            laid.rows.push(Row::new());
            laid.rows.extend(self.notes[..cut.notes].iter().cloned());
        }
        laid
    }

    fn visit(&self, at: usize, cut: &Cut, laid: &mut Laid) {
        let m = &self.members[at];
        laid.agents.push((m.target.clone(), laid.rows.len()));
        laid.rows.push(m.line.clone());
        if let Some(d) = m.doing.as_ref().filter(|_| cut.doing[at]) {
            laid.more.push((m.target.id.clone(), laid.rows.len()));
            laid.rows.push(d.clone());
        }
        let (mut ended, mut more) = (0, 0);
        for &k in &m.kids {
            if cut.hidden[k] {
                more += self.size(k)
            } else if cut.folded[at] && self.foldable(k) {
                ended += 1
            } else {
                self.visit(k, cut, laid)
            }
        }
        if ended > 0 {
            laid.rows
                .push(self.summary(m.depth + 1, &format!("+{ended} ended")));
        }
        if more > 0 {
            laid.rows
                .push(self.summary(m.depth + 1, &format!("+{more} more")));
        }
    }

    fn foldable(&self, k: usize) -> bool {
        self.members[k].ended && self.members[k].kids.is_empty() && !self.protected[k]
    }

    fn size(&self, at: usize) -> usize {
        1 + self.members[at]
            .kids
            .iter()
            .map(|&k| self.size(k))
            .sum::<usize>()
    }

    /// A fold line: dim, indented under its parent; no agent row, so it opens nothing (B6).
    fn summary(&self, depth: usize, text: &str) -> Row {
        let pad = (2 * depth + 3).min(self.cols);
        vec![
            span(" ".repeat(pad), PLAIN),
            span(paint::cut(text, self.cols + 1 - pad), DIM),
        ]
    }

    /// The PM narrow-width rule over the whole ordered team, before any height folding (A4): a member on a branch the
    /// height folds away still counts. Too narrow: the header and the first widen line that fits (the whole line, then
    /// `widen: <n> columns`, then `<n>`, so <n> is never cut, then `<n>` unindented when only the margin does not fit;
    /// nothing fits: the header alone) and no row; keys still walk the whole tree.
    fn too_narrow(&self, width: usize) -> Option<(Laid, usize)> {
        let need = self.members.iter().map(|m| m.need).max()?;
        if need <= width {
            return None;
        }
        let note = [
            format!("widen the bar: needs {need} columns"),
            format!("widen: {need} columns"),
            need.to_string(),
        ]
        .into_iter()
        .find(|t| paint::width(t) <= self.cols)
        .map(|n| margin(vec![span(n, PLAIN)]))
        // No room for the margin: the whole number unindented, the last column still free; else the header alone.
        .or_else(|| {
            (paint::width(&need.to_string()) < width).then(|| vec![span(need.to_string(), PLAIN)])
        });
        let mut rows = vec![self.header.clone()];
        rows.extend(note);
        Some((
            Laid {
                rows,
                ..Laid::default()
            },
            0,
        ))
    }

    fn full(&self) -> Cut {
        let n = self.members.len();
        Cut {
            gap: true,
            doing: vec![true; n],
            folded: vec![false; n],
            hidden: vec![false; n],
            notes: self.notes.len(),
        }
    }

    /// B5: drop the blank line, doing lines bottom-up, each parent's ended rows, the deepest unprotected sub-trees, notes;
    /// still too tall, the last resort.
    fn fit(
        &self,
        height: usize,
        focus: Option<usize>,
        leader: Option<usize>,
        ascii: bool,
    ) -> (Laid, usize) {
        let mut cut = self.full();
        loop {
            let laid = self.emit(&cut);
            if laid.rows.len() <= height {
                return (laid, cut.notes);
            }
            if !self.shrink(&mut cut) {
                return (self.last_resort(height, focus, leader, ascii), 0);
            }
        }
    }

    fn shrink(&self, cut: &mut Cut) -> bool {
        let n = self.members.len();
        let deepest = |ok: &dyn Fn(usize) -> bool| {
            (0..n)
                .filter(|&a| ok(a))
                .max_by_key(|&a| (self.members[a].depth, a))
        };
        if cut.gap {
            cut.gap = false;
            return true;
        }
        if let Some(a) = (0..n)
            .rev()
            .find(|&a| cut.doing[a] && self.members[a].doing.is_some())
        {
            cut.doing[a] = false;
            return true;
        }
        let fold =
            deepest(&|a| !cut.folded[a] && self.members[a].kids.iter().any(|&k| self.foldable(k)));
        if let Some(a) = fold {
            cut.folded[a] = true;
            return true;
        }
        let open = |a: usize| {
            let mut p = Some(a);
            while let Some(x) = p {
                if cut.hidden[x] {
                    return false;
                }
                p = self.members[x].parent;
            }
            true
        };
        let rows = self.emit(cut).rows.len();
        // Hiding never grows the frame; a folded ended leaf is already in `+N ended`.
        let hide = deepest(&|a| {
            !self.protected[a]
                && open(a)
                && !self.members[a]
                    .parent
                    .is_some_and(|p| cut.folded[p] && self.foldable(a))
                && {
                    let mut t = cut.clone();
                    t.hidden[a] = true;
                    self.emit(&t).rows.len() <= rows
                }
        });
        if let Some(a) = hide {
            cut.hidden[a] = true;
            return true;
        }
        if cut.notes > 0 {
            cut.notes -= 1;
            return true;
        }
        false
    }

    /// The leader, `… N above` (N: the rows between), then the focus row; below 3 rows, the focus row alone.
    fn last_resort(
        &self,
        height: usize,
        focus: Option<usize>,
        leader: Option<usize>,
        ascii: bool,
    ) -> Laid {
        let mut laid = Laid::default();
        let Some(f) = focus.or(leader).or(self.roots.first().copied()) else {
            laid.rows.push(self.header.clone());
            return laid;
        };
        if let Some(l) = leader.filter(|&l| l < f && height >= 3) {
            laid.agents.push((self.members[l].target.clone(), 0));
            laid.rows.push(self.members[l].line.clone());
            laid.rows.push(self.summary(
                0,
                &format!(
                    "{} {} above",
                    if ascii { "..." } else { "\u{2026}" },
                    f - l - 1
                ),
            ));
        }
        laid.agents
            .push((self.members[f].target.clone(), laid.rows.len()));
        laid.rows.push(self.members[f].line.clone());
        laid
    }
}

/// A team board at `size` (B2-B5, B7): the header counting only the team, the tree, the notes; and how many notes it kept.
pub(super) fn lay_team(
    board: &Map<String, Value>,
    notes: &[(&str, Style)],
    size: Size,
    look: Look,
    focus: Focus,
) -> (Laid, usize) {
    let (cols, nodes) = (size.width.saturating_sub(2), order(board));
    let mut members: Vec<Member> = nodes
        .iter()
        .map(|n| member(n, size.width, focus.shown, look))
        .collect();
    let mut roots = Vec::new();
    for at in 0..members.len() {
        // In tree order a row's parent is the nearest earlier row one level up (a broken row's: the leader).
        let parent = (0..at)
            .rev()
            .find(|&p| members[p].depth + 1 == members[at].depth);
        members[at].parent = parent;
        match parent {
            Some(p) => members[p].kids.push(at),
            None => roots.push(at),
        }
    }
    let shown = focus.shown.and_then(|p| {
        members
            .iter()
            .position(|m| !m.target.pane.is_empty() && m.target.pane == p)
    });
    let selected = focus
        .selected
        .and_then(|id| members.iter().position(|m| m.target.id == id));
    let leader = nodes
        .iter()
        .position(|n| n.row.get("leader") == Some(&Value::Bool(true)));
    let mut protected = vec![false; members.len()];
    for start in [leader, shown, selected].into_iter().flatten() {
        let mut at = Some(start);
        while let Some(a) = at {
            protected[a] = true;
            at = members[a].parent;
        }
    }
    let notes = notes
        .iter()
        .map(|(n, s)| margin(vec![span(paint::cut(&own_glyphs(n), cols), *s)]))
        .collect();
    let team = Team {
        header: cut_row(
            header(&counts(board), cols),
            size.width.saturating_sub(1).max(1),
        ),
        members,
        roots,
        protected,
        notes,
        cols,
    };
    team.too_narrow(size.width)
        .unwrap_or_else(|| team.fit(size.height, selected.or(shown), leader, look.ascii))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use serde_json::json;

    // Calls 9-10, finding 2: a failed generation whose client still runs keeps "still running" on every wide team
    // bar: `recover (still running) <age>` at 60, `recover (still running)` at the default width 40 (the name cut
    // first), and only the narrow bar (30) shows the bare `recover` pill.
    #[test]
    fn a_failed_generation_still_running_says_so_on_a_wide_bar() {
        let mut board = golden();
        let rows = board.get_mut("agents").unwrap().as_array_mut().unwrap();
        let fixer = rows.iter_mut().find(|r| r["id"] == "F").unwrap();
        fixer["recoveryRunning"] = json!(true);
        let look = || Look {
            color: false,
            ascii: false,
            dumb: false,
        };
        let focus = || Focus {
            shown: Some("%13"),
            selected: None,
        };
        let row = |width| {
            draw(&board, width, 20, look(), focus())
                .into_iter()
                .find(|l| l.contains("\u{2709} 2"))
                .unwrap_or_default()
        };
        assert_eq!(
            row(60),
            line(
                " ",
                "  ! fixer  \u{2709} 2",
                "recover (still running)   1m",
                60
            )
        );
        assert!(
            row(40).trim_end().ends_with("recover (still running)"),
            "40: {:?}",
            row(40)
        );
        assert!(row(40).starts_with(" "), "40: {:?}", row(40));
        assert_eq!(row(30), line(" ", "  ! fixer  \u{2709} 2", "recover", 30));
    }

    // Build checklist 2 (call 21): a deep chain at the default width 40 with a running recovery row and an unread
    // badge: no room for `recover (still running)` even without the name, so the row falls back to the bare `recover`
    // pill; the bar never asks to be widened for it.
    #[test]
    fn a_deep_running_recovery_row_falls_back_to_the_bare_pill() {
        let board = team(vec![
            member("L", None, 1, json!({"name": "lead"})),
            member("A", Some("L"), 2, json!({})),
            member("B", Some("A"), 3, json!({})),
            member("C", Some("B"), 4, json!({})),
            member(
                "D",
                Some("C"),
                5,
                json!({"name": "deep", "needsRecovery": true, "recoveryRunning": true, "unread": 2}),
            ),
        ]);
        let look = Look {
            color: false,
            ascii: false,
            dumb: false,
        };
        let focus = Focus {
            shown: None,
            selected: None,
        };
        let drawn = draw(&board, 40, 20, look, focus);
        assert!(
            !drawn.iter().any(|l| l.contains("widen the bar")),
            "{drawn:#?}"
        );
        let d = drawn
            .iter()
            .find(|l| l.contains("\u{2709} 2"))
            .unwrap_or_else(|| panic!("no recovery row: {drawn:#?}"));
        assert!(d.trim_end().ends_with("recover"), "{d:?}");
        assert!(!d.contains("still running"), "{d:?}");
    }

    // restore-6c checklist 18 (P1): a member that needs recovery shows its reason beside `recover` where it fits
    // (60); the narrow bar (30) keeps the bare `recover` pill.
    #[test]
    fn a_recovery_row_shows_its_reason_where_it_fits() {
        let mut board = golden();
        let rows = board.get_mut("agents").unwrap().as_array_mut().unwrap();
        let fixer = rows.iter_mut().find(|r| r["id"] == "F").unwrap();
        fixer["recoveryReason"] = json!("exited 1");
        let look = || Look {
            color: false,
            ascii: false,
            dumb: false,
        };
        let focus = || Focus {
            shown: Some("%13"),
            selected: None,
        };
        let row = |width| {
            draw(&board, width, 20, look(), focus())
                .into_iter()
                .find(|l| l.contains("\u{2709} 2"))
                .unwrap_or_default()
        };
        assert_eq!(
            row(60),
            line(" ", "  ! fixer  \u{2709} 2", "recover: exited 1   1m", 60)
        );
        assert_eq!(row(30), line(" ", "  ! fixer  \u{2709} 2", "recover", 30));
    }

    /// A team row: id, name, parent id, `createdAt` minute, merged with `extra`.
    pub(crate) fn member(id: &str, parent: Option<&str>, minute: u32, extra: Value) -> Value {
        let mut row = json!({"id": id, "name": id, "parentId": parent, "leader": parent.is_none() && id == "L",
            "createdAt": format!("2026-10-29T11:{minute:02}:00.0000000Z"), "state": "running", "life": "running",
            "pane": format!("%{}", 10 + minute), "age": 60});
        for (k, v) in extra.as_object().unwrap() {
            row[k] = v.clone();
        }
        row
    }
    pub(crate) fn team(rows: Vec<Value>) -> Map<String, Value> {
        json!({"team": true, "running": rows.len(), "agents": rows})
            .as_object()
            .unwrap()
            .clone()
    }
    fn shape(board: &Map<String, Value>) -> Vec<(String, usize, bool)> {
        order(board)
            .iter()
            .map(|n| (super::super::clean(n.row.get("id")), n.depth, n.broken))
            .collect()
    }

    #[test]
    fn order_puts_children_under_parents_oldest_first_and_breaks_bad_chains() {
        let board = team(vec![
            member("B", Some("L"), 5, json!({})),
            member("G", Some("A"), 6, json!({})),
            member("A", Some("L"), 2, json!({})),
            member("L", None, 1, json!({})),
            member("X", Some("gone"), 3, json!({})),
            member("Y", Some("Z"), 7, json!({})),
            member("Z", Some("Y"), 8, json!({})),
            member("R", None, 9, json!({})),
            member("K", Some("X"), 4, json!({})),
        ]);
        let s = |id: &str, d: usize, b: bool| (id.to_string(), d, b);
        assert_eq!(
            shape(&board),
            [
                s("L", 0, false),
                s("A", 1, false),
                s("G", 2, false),
                s("B", 1, false),
                s("X", 1, true),
                s("K", 1, true),
                s("Y", 1, true),
                s("Z", 1, true),
                s("R", 1, true)
            ]
        );
        let ids: Vec<_> = walk(&board).into_iter().map(|t| t.id).collect();
        assert_eq!(
            ids,
            ["L", "A", "G", "B", "X", "K", "Y", "Z", "R"],
            "keys walk the whole tree in order"
        );
        // No leader at all: every row is broken at depth 1, and ordering still ends.
        let lone = team(vec![
            member("Y", Some("Z"), 1, json!({})),
            member("Z", Some("Y"), 2, json!({})),
        ]);
        assert_eq!(shape(&lone), [s("Y", 1, true), s("Z", 1, true)]);
    }

    #[test]
    fn siblings_follow_the_w6_rank_then_age() {
        // W6 among the running: needs you, on call, done, working, idle, oldest first within a rank; E, stopped, keeps
        // its age place.
        let board = team(vec![
            member("L", None, 1, json!({})),
            member(
                "E",
                Some("L"),
                2,
                json!({"state": "stopped", "life": "ended"}),
            ),
            member("W", Some("L"), 3, json!({"activity": "working"})),
            member("I", Some("L"), 4, json!({"activity": "idle"})),
            member("D", Some("L"), 5, json!({"activity": "done"})),
            member("O", Some("L"), 6, json!({"activity": "on call"})),
            member("N", Some("L"), 7, json!({"activity": "blocked"})),
            member("P", Some("L"), 8, json!({"activity": "on call"})),
            member("c", Some("W"), 9, json!({"activity": "working"})),
            member("b", Some("W"), 10, json!({"activity": "on call"})),
        ]);
        let ids: Vec<_> = walk(&board).into_iter().map(|t| t.id).collect();
        assert_eq!(ids, ["L", "E", "N", "O", "P", "D", "W", "b", "c", "I"]);
    }

    fn golden() -> Map<String, Value> {
        team(vec![
            member("L", None, 1, json!({"name": "lead", "doing": "plan it"})),
            member(
                "A",
                Some("L"),
                2,
                json!({"name": "alpha", "activity": "idle"}),
            ),
            member(
                "G",
                Some("A"),
                3,
                json!({"name": "gamma", "activity": "blocked"}),
            ),
            member(
                "B",
                Some("L"),
                4,
                json!({"name": "beta", "state": "stopped", "life": "ended", "pane": null}),
            ),
            member(
                "F",
                Some("L"),
                5,
                json!({"name": "fixer", "state": "lost", "life": "lost", "pane": null, "needsRecovery": true, "unread": 2}),
            ),
        ])
    }
    pub(crate) fn draw(
        board: &Map<String, Value>,
        width: usize,
        height: usize,
        look: Look,
        focus: Focus,
    ) -> Vec<String> {
        lay_team(board, &[], Size { width, height }, look, focus)
            .0
            .rows
            .iter()
            .map(|r| render(r, look.color))
            .collect()
    }
    /// A tree row `width` wide: `mark`, `left`, spaces, `right`, ending one column short of the edge.
    fn line(mark: &str, left: &str, right: &str, width: usize) -> String {
        format!(
            "{mark}{left}{}{right}",
            " ".repeat(width - 2 - left.chars().count() - right.chars().count())
        )
    }
    #[test]
    fn a_team_draws_as_a_tree_with_the_shown_mark() {
        let (board, look, shown) = (
            golden(),
            Look {
                color: false,
                ascii: false,
                dumb: false,
            },
            Focus {
                shown: Some("%13"),
                selected: None,
            },
        );
        let wide = [
            line(
                " ",
                "mux-agents",
                "1 need you / 1 working / 1 idle / 2 ended",
                60,
            ),
            String::new(),
            line(" ", "\u{25c9} lead", "working   1m", 60),
            "   plan it".into(),
            line(" ", "  \u{25cc} alpha", "idle   1m", 60),
            line(" ", "    ! gamma", "needs you   1m", 60),
            line(" ", "  - beta", "stopped   1m", 60),
            line(" ", "  ! fixer  \u{2709} 2", "recover   1m", 60),
        ];
        let narrow = [
            line(" ", "mux-agents", "1 need you", 30),
            String::new(),
            line(" ", "\u{25c9} lead", "  1m", 30),
            "   plan it".into(),
            line(" ", "  \u{25cc} alpha", "  1m", 30),
            line(" ", "    ! gamma", "  1m", 30),
            line(" ", "  - beta", "stopped   1m", 30),
            line(" ", "  ! fixer  \u{2709} 2", "recover", 30),
        ];
        for (width, want) in [(60, &wide), (30, &narrow)] {
            assert_eq!(
                draw(&board, width, 24, look, shown),
                want.to_vec(),
                "{width}"
            );
            let ascii: Vec<String> = want
                .iter()
                .map(|l| {
                    l.replace("\u{25c9}", "*")
                        .replace("\u{25cc}", "o")
                        .replace("\u{2709}", "m")
                })
                .collect();
            assert_eq!(
                draw(
                    &board,
                    width,
                    24,
                    Look {
                        ascii: true,
                        ..look
                    },
                    shown
                ),
                ascii,
                "{width} ascii"
            );
        }
        // B8 in color: the reversed pill; B2: the ended row dimmed whole; no state headings.
        let laid = lay_team(
            &board,
            &[],
            Size {
                width: 60,
                height: 24,
            },
            Look {
                color: true,
                ..look
            },
            shown,
        )
        .0;
        assert!(
            laid.rows[7]
                .iter()
                .any(|(t, s)| t == "recover" && s.reverse)
                && laid.rows[6].iter().all(|(_, s)| s.dim)
        );
        assert!(
            !draw(&board, 60, 24, look, shown)
                .iter()
                .any(|r| r.contains("Needs you") || r.contains("Working"))
        );
        // B4: one column (equal widths), none without a shown pane, it moves, the selection replaces it then gives it back.
        let widths = |f: Focus| {
            lay_team(
                &board,
                &[],
                Size {
                    width: 60,
                    height: 24,
                },
                look,
                f,
            )
            .0
            .rows
            .iter()
            .map(|r| row_width(r))
            .collect::<Vec<_>>()
        };
        assert_eq!(widths(Focus::default()), widths(shown));
        // In color the member on screen is banded (its whole block: name line, and doing line when it has one), and the
        // band follows the shown pane.
        let color = Look {
            color: true,
            ..look
        };
        let banded = |f: Focus| -> Vec<usize> {
            let mut laid = lay_team(
                &board,
                &[],
                Size {
                    width: 60,
                    height: 24,
                },
                color,
                f,
            )
            .0;
            crate::sidebar::shade(&mut laid, f.shown, 60, color);
            (0..laid.rows.len())
                .filter(|&at| laid.rows[at].iter().any(|(_, s)| s.bg.is_some()))
                .collect()
        };
        assert_eq!(banded(shown), [5]);
        assert_eq!(banded(Focus::default()), Vec::<usize>::new());
        let moved = Focus {
            shown: Some("%12"),
            selected: None,
        };
        assert_eq!(banded(moved), [4]);
        // The leader's doing line is drawn under it: on screen, both of its rows are banded, and none of its children.
        let lead_shown = Focus {
            shown: Some(board["agents"][0]["pane"].as_str().unwrap_or("%10")),
            selected: None,
        };
        let lead_rows = banded(lead_shown);
        let drawn = draw(&board, 60, 24, look, lead_shown);
        assert!(
            lead_rows.len() == 2 && drawn[lead_rows[1]].contains("plan it"),
            "{lead_rows:?} {drawn:?}"
        );
        let mut sel = lay_team(
            &board,
            &[],
            Size {
                width: 60,
                height: 24,
            },
            look,
            shown,
        )
        .0;
        mark(&mut sel, "G", 60, look);
        assert!(
            render(&sel.rows[5], false).starts_with('>')
                && draw(&board, 60, 24, look, shown)[5].starts_with(' ')
        );
        // In color the selection takes the band from the shown member: one mark, no `>`.
        let mut lit = lay_team(
            &board,
            &[],
            Size {
                width: 60,
                height: 24,
            },
            color,
            moved,
        )
        .0;
        crate::sidebar::shade(&mut lit, moved.shown, 60, color);
        mark(&mut lit, "G", 60, color);
        let lit_rows: Vec<usize> = (0..lit.rows.len())
            .filter(|&at| lit.rows[at].iter().any(|(_, s)| s.bg.is_some()))
            .collect();
        assert_eq!(lit_rows, [5]);
        assert!(lit.rows.iter().all(|r| !render(r, false).starts_with('>')));
        // B1/B3: an ended leader still heads its child; a missing parent and a self-loop draw at depth 1 with `^`.
        let broken = team(vec![
            member(
                "L",
                None,
                1,
                json!({"state": "stopped", "life": "ended", "pane": null}),
            ),
            member("X", Some("L"), 2, json!({})),
            member("O", Some("gone"), 3, json!({})),
            member("S", Some("S"), 4, json!({})),
        ]);
        let rows = draw(&broken, 40, 24, look, Focus::default());
        assert!(
            rows[2].starts_with(" - L")
                && rows[3].starts_with("   \u{25c9} X")
                && rows[4].starts_with(" ^ \u{25c9} O")
                && rows[5].starts_with(" ^ \u{25c9} S"),
            "{rows:?}"
        );
    }
    #[test]
    fn narrow_bars_keep_every_mandatory_part_or_ask_to_be_widened() {
        let ended = |state: &str, code: i64, unread: u64, name: &str| json!({"name": name, "state": state, "life": "ended", "exitCode": code, "pane": null, "unread": unread});
        let board = team(vec![
            member("L", None, 1, json!({})),
            member("A", Some("L"), 2, json!({})),
            member("G", Some("A"), 3, json!({"unread": 1})),
            member("D", Some("G"), 4, json!({})),
            member(
                "R",
                Some("G"),
                5,
                json!({"state": "lost", "life": "lost", "pane": null, "needsRecovery": true, "unread": 2}),
            ),
            member("E", Some("L"), 6, ended("exited 7", 7, 3, "elephant")),
            member("X", Some("A"), 7, ended("exited 255", 255, 4, "xylophone")),
            member(
                "K",
                Some("L"),
                8,
                json!({"state": "lost", "life": "lost", "pane": null, "unread": 5}),
            ),
            member(
                "U",
                Some("L"),
                9,
                json!({"state": "unknown", "life": "unknown", "pane": null, "unread": 6}),
            ),
            member("O", Some("gone"), 10, json!({})),
        ]);
        // (id, depth, the whole state word, unread): live at depths 0-3, recovery at 3, ended, lost, unknown, broken at 1.
        let want = [
            ("L", 0, "", 0),
            ("A", 1, "", 0),
            ("G", 2, "", 1),
            ("D", 3, "", 0),
            ("R", 3, "recover", 2),
            ("E", 1, "exited 7", 3),
            ("X", 2, "exited 255", 4),
            ("K", 1, "lost", 5),
            ("U", 1, "unknown", 6),
            ("O", 1, "", 0),
        ];
        for ascii in [false, true] {
            let look = Look {
                ascii,
                ..Look::default()
            };
            let (mail, cut) = if ascii {
                ("m", ".")
            } else {
                ("\u{2709}", "\u{2026}")
            };
            // 25 is this board's minimum: X (depth 2, `exited 255`, a badge) needs 1 + 4 + 2 + 5 + 2 + 10 + 1 columns.
            for width in [60, 40, 30, 25] {
                let laid = lay_team(
                    &board,
                    &[],
                    Size { width, height: 24 },
                    look,
                    Focus {
                        shown: Some("%13"),
                        selected: None,
                    },
                )
                .0;
                let rows: Vec<String> = laid.rows.iter().map(|r| render(r, false)).collect();
                assert_eq!(
                    laid.agents.len(),
                    10,
                    "{width}: every agent is drawn: {rows:?}"
                );
                assert!(
                    laid.rows.iter().all(|r| row_width(r) < width),
                    "{width}: {rows:?}"
                );
                let at = |id: &str| {
                    rows[laid.agents.iter().find(|(t, _)| t.id == id).unwrap().1].clone()
                };
                for (id, depth, word, unread) in want {
                    let row = at(id);
                    let col = row
                        .chars()
                        .enumerate()
                        .skip(1)
                        .find(|(_, c)| *c != ' ' && *c != '^')
                        .unwrap()
                        .0;
                    assert_eq!(
                        col,
                        1 + 2 * depth,
                        "{width} {id}: the exact glyph column: {row:?}"
                    );
                    assert!(
                        row.contains(word),
                        "{width} {id}: the whole state word: {row:?}"
                    );
                    assert!(
                        unread == 0 || row.contains(&format!("{mail} {unread}")),
                        "{width} {id}: the badge: {row:?}"
                    );
                }
                assert!(
                    at("G").starts_with(' ') && at("O").chars().nth(1) == Some('^'),
                    "{width}: no mark in column 0, and the caret: {rows:?}"
                );
                // The name goes last of the optional parts: whole, then cut, then removed.
                let x = at("X");
                let named = (
                    x.contains("xylophone"),
                    x.contains(&format!("xylo{cut}")),
                    x.contains("xy"),
                );
                assert_eq!(
                    named,
                    match width {
                        60 | 40 => (true, false, true),
                        30 => (false, true, true),
                        _ => (false, false, false),
                    },
                    "{width}: {x:?}"
                );
                let e: String = if width == 25 {
                    format!("ele{cut}")
                } else {
                    "elephant".into()
                };
                assert!(at("E").contains(&e), "{width}: {rows:?}");
            }
            // The minimum minus 1: the header and the widen line, no row; the line keeps <n> where the whole text cannot fit.
            let small = lay_team(
                &board,
                &[],
                Size {
                    width: 24,
                    height: 24,
                },
                look,
                Focus::default(),
            )
            .0;
            let rows: Vec<String> = small.rows.iter().map(|r| render(r, false)).collect();
            assert!(
                small.agents.is_empty() && rows.len() == 2 && rows[1] == " widen: 25 columns",
                "{rows:?}"
            );
        }
        // A deeper board whose minimum leaves the whole line room: 34 draws `exited 2147483647` whole, 33 asks to be widened.
        let deep = team(vec![
            member("L", None, 1, json!({})),
            member("A", Some("L"), 2, json!({})),
            member("G", Some("A"), 3, json!({})),
            member(
                "Z",
                Some("G"),
                4,
                json!({"name": "zed", "state": "exited 2147483647", "life": "ended", "exitCode": 2147483647, "pane": null, "unread": 9}),
            ),
        ]);
        let wide = draw(&deep, 34, 24, Look::default(), Focus::default());
        assert!(
            wide.iter()
                .any(|r| r.contains("exited 2147483647") && r.contains("\u{2709} 9")),
            "{wide:?}"
        );
        let small = lay_team(
            &deep,
            &[],
            Size {
                width: 33,
                height: 24,
            },
            Look::default(),
            Focus::default(),
        )
        .0;
        assert_eq!(
            small
                .rows
                .iter()
                .map(|r| render(r, false))
                .collect::<Vec<_>>(),
            [
                render(&small.rows[0], false),
                " widen the bar: needs 34 columns".to_string()
            ]
        );
        assert!(small.agents.is_empty(), "nothing is drawn to click");
        // Keys still move the selection: they walk the whole tree, not the drawn rows.
        let mut pick = crate::sidebar::pick::Pick::default();
        pick.key(crate::sidebar::keys::Event::Down, &walk(&deep));
        assert_eq!(pick.selected(), Some("L"));
    }

    #[test]
    fn one_reducer_drops_doing_then_model_then_age_then_the_name() {
        // A live row at 40 columns or wider: a 40-column doing text, a model, the word with its age, the word alone.
        let parts = RowParts {
            glyph: span("*", PLAIN),
            name: span("lead-worker", PLAIN),
            model: vec![span(" [opus]", DIM)],
            badge: Vec::new(),
            sides: vec![
                right(Kind::Working, "working", "1m", false),
                vec![span("working", PLAIN)],
            ],
            bare: None,
            kept: true,
            doing: 40,
        };
        for cols in 1..=50 {
            let (row, doing) = fit_row(&parts, cols, false);
            let text: String = row.iter().map(|(t, _)| t.as_str()).collect();
            assert!(
                text.contains("working"),
                "{cols}: the mandatory word: {text:?}"
            );
            if cols >= 11 {
                assert_eq!(paint::width(&text), cols, "{cols}: exactly cols: {text:?}");
            }
            let name = if text.contains("lead-worker") {
                "whole"
            } else if text.contains("\u{2026}") {
                "cut"
            } else {
                "gone"
            };
            // Full row 34 columns; the doing line needs 40 + 2. Without the model 27; without the age 22; then the name is cut
            // (13-21) and removed (11-12); under 11 the floor.
            let want = match cols {
                42.. => (true, true, true, "whole"),
                34..=41 => (false, true, true, "whole"),
                27..=33 => (false, false, true, "whole"),
                22..=26 => (false, false, false, "whole"),
                13..=21 => (false, false, false, "cut"),
                _ => (false, false, false, "gone"),
            };
            assert_eq!(
                (doing, text.contains("[opus]"), text.contains("1m"), name),
                want,
                "{cols}: {text:?}"
            );
        }
    }

    pub(crate) fn room() -> Map<String, Value> {
        let ended = || json!({"state": "stopped", "life": "ended", "pane": null});
        team(vec![
            member("L", None, 1, json!({"doing": "plan"})),
            member("A", Some("L"), 2, ended()),
            member("B", Some("L"), 3, ended()),
            member("C", Some("L"), 4, json!({"doing": "build"})),
            member("C1", Some("C"), 5, json!({})),
            member("C2", Some("C"), 6, json!({"pane": null})),
            member("D", Some("L"), 7, json!({})),
            member("D1", Some("D"), 8, json!({})),
        ])
    }
    #[test]
    fn short_of_height_the_tree_folds_in_order_and_keeps_the_focus_chain() {
        let labels = |h: usize, f: Focus| {
            draw(&room(), 40, h, Look::default(), f)
                .iter()
                .map(|r| r.split_whitespace().collect::<Vec<_>>().join(" "))
                .collect::<Vec<_>>()
        };
        let c1 = Focus {
            shown: Some("%15"),
            selected: None,
        };
        let full = [
            "mux-agents 6 working / 2 ended",
            "",
            "\u{25c9} L working 1m",
            "plan",
            "- A stopped 1m",
            "- B stopped 1m",
            "\u{25c9} C working 1m",
            "build",
            "\u{25c9} C1 working 1m",
            "\u{25c9} C2 working 1m",
            "\u{25c9} D working 1m",
            "\u{25c9} D1 working 1m",
        ];
        let drop = |gone: &[usize]| {
            full.iter()
                .enumerate()
                .filter(|(i, _)| !gone.contains(i))
                .map(|(_, s)| s.to_string())
                .collect::<Vec<_>>()
        };
        // The blank line, then doing lines bottom-up, then each parent's ended rows, then the deepest sub-trees.
        assert_eq!(
            (
                labels(12, c1),
                labels(11, c1),
                labels(10, c1),
                labels(9, c1)
            ),
            (drop(&[]), drop(&[1]), drop(&[1, 7]), drop(&[1, 3, 7]))
        );
        assert_eq!(
            labels(8, c1),
            [
                full[0], full[2], full[6], full[8], full[9], full[10], full[11], "+2 ended"
            ]
        );
        assert_eq!(
            labels(7, c1),
            [
                full[0], full[2], full[6], full[8], "+1 more", "+2 ended", "+2 more"
            ]
        );
        // Still too tall: the leader, `… N above`, the focus row (the selection when active); below 3 rows the focus row alone.
        let tail = |n: usize, f: &str| {
            vec![
                full[2].to_string(),
                format!("\u{2026} {n} above"),
                f.to_string(),
            ]
        };
        assert_eq!(
            (labels(6, c1), labels(3, c1), labels(2, c1)),
            (
                tail(3, full[8]),
                tail(3, full[8]),
                vec![full[8].to_string()]
            )
        );
        assert_eq!(
            labels(
                7,
                Focus {
                    selected: Some("D1"),
                    ..c1
                }
            ),
            tail(6, full[11]),
            "the selection on another branch"
        );
        assert_eq!(
            labels(
                3,
                Focus {
                    shown: None,
                    selected: Some("D1")
                }
            ),
            tail(6, full[11]),
            "no agent shown"
        );
    }
    #[test]
    fn team_rows_hit_their_member_id_and_folds_hit_nothing() {
        let mut rows = room()["agents"].as_array().unwrap().clone();
        rows[0]["doing"] = json!("twin plan");
        rows[4]["name"] = json!("twin");
        rows[5]["name"] = json!("twin");
        rows.push(member("O", Some("gone"), 9, json!({})));
        for height in [24, 7] {
            let laid = lay_team(
                &team(rows.clone()),
                &[],
                Size { width: 40, height },
                Look::default(),
                Focus {
                    shown: Some("%15"),
                    selected: None,
                },
            )
            .0;
            let lines: Vec<String> = laid.rows.iter().map(|r| render(r, false)).collect();
            let answer = format!("40\n{}\n", lines.join("\n"));
            let hits = crate::sidebar::drawn(&laid);
            for (at, line) in lines.iter().enumerate() {
                // A member's name line and its doing line open it; folds open nothing.
                let want = (laid.agents.iter().map(|(t, r)| (t.id.as_str(), *r)))
                    .chain(laid.more.iter().map(|(id, r)| (id.as_str(), *r)))
                    .find(|(_, r)| *r == at)
                    .map(|(id, _)| id);
                let got = crate::sidebar::pick::frame_hit(
                    &answer,
                    40,
                    u16::try_from(at + 1).unwrap(),
                    &hits,
                );
                assert_eq!(got, want, "{height} {at}: {}", line);
                assert!(got.is_none() || !line.contains('+'), "folds open nothing");
                if line.contains("twin plan") {
                    assert_eq!(
                        got,
                        Some(laid.agents[0].0.id.as_str()),
                        "the doing line opens its member"
                    );
                }
            }
            if height == 24 {
                assert!(
                    lines.iter().any(|l| l.starts_with(" ^ ")),
                    "the broken row is drawn, and hit as O"
                );
            }
        }
    }

    #[test]
    fn the_width_check_covers_the_whole_team_before_folding() {
        let mut rows = room()["agents"].as_array().unwrap().clone();
        rows.push(member(
            "Z",
            Some("C2"),
            9,
            json!({"name": "zed", "state": "exited 2147483647", "life": "ended",
            "exitCode": 2147483647, "pane": null, "unread": 9}),
        ));
        let board = team(rows);
        // Z needs 34 columns: 1 + 6 (depth 3) + 2 + 5 (its badge) + 2 + 17 + 1; at height 3 the tree folds Z's branch away.
        let small = lay_team(
            &board,
            &[],
            Size {
                width: 30,
                height: 3,
            },
            Look::default(),
            Focus::default(),
        )
        .0;
        let text: Vec<String> = small.rows.iter().map(|r| render(r, false)).collect();
        assert_eq!(
            text.get(1).map(String::as_str),
            Some(" widen: 34 columns"),
            "{text:?}"
        );
        assert!(small.agents.is_empty(), "nothing drawn to click: {text:?}");
        let fits = lay_team(
            &board,
            &[],
            Size {
                width: 34,
                height: 3,
            },
            Look::default(),
            Focus::default(),
        )
        .0;
        assert!(!fits.agents.is_empty(), "at its need the folded tree draws");
    }
}
