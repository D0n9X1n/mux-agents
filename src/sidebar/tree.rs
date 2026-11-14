//! A team's side bar (#103 phase 5): the team as a tree, leader first, each member's children under it.

use serde_json::{Map, Value};

use super::pick::Target;
use super::*;

/// One drawn row of the tree: its board row, its drawn depth, and whether its chain is broken (drawn at 1 with `^`).
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "shortcut: no caller until the team bar's layout (Task 11, #103); remove this then"
    )
)]
pub(super) struct Node<'a> {
    pub row: &'a Map<String, Value>,
    pub depth: usize,
    pub broken: bool,
}

/// Whether the board is a team's feed (#103 B1).
#[expect(
    dead_code,
    reason = "shortcut: no caller until the team bar's layout (Task 11, #103); remove this then"
)]
pub(super) fn is_team(board: Option<&Map<String, Value>>) -> bool {
    board.and_then(|b| b.get("team")) == Some(&Value::Bool(true))
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "shortcut: no caller until the team bar's layout (Task 11, #103); remove this then"
    )
)]
fn rows(board: &Map<String, Value>) -> Vec<&Map<String, Value>> {
    board
        .get("agents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .collect()
}

/// B2/B3: the leader, then each member's children directly under it, oldest `createdAt` first (ties by id). A row whose
/// parent is not in the team, or whose chain repeats or ends elsewhere than the leader, follows at depth 1, broken;
/// its own children are broken too. Every row is placed once, so this always ends.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "shortcut: no caller until the team bar's layout (Task 11, #103); remove this then"
    )
)]
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

/// Every agent of a team board in tree order, folded or not: what Up/Down walk (#103 B10).
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "shortcut: no caller until the team bar's layout (Task 11, #103); remove this then"
    )
)]
pub(super) fn walk(board: &Map<String, Value>) -> Vec<Target> {
    order(board).iter().map(|n| Target::of(n.row)).collect()
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use serde_json::json;

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
}
