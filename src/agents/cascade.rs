//! cascade-3d: a stop records `stopped` for a member's whole sub-tree, closes it, then ends it bottom-up; a journal
//! under `teams/<sha16(session)>.cascade/` lets the next stop, `up` or bar tick finish a cascade cut short.
use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use super::gens;
use super::team_state::Tx;
use crate::error::{Refusal, Res};

/// state-0 S2: `desired.json` `{desired, at}`; absent reads `running` (C1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desired {
    Running,
    Stopped,
}

/// C1, R2, R5: through the walk from the state root. Absent: Running. A word other than the two: `internal`.
/// Unreadable (torn, a folder, a link on the way): `evidence`, never Running.
pub fn desired_of(member: &Path) -> Res<Desired> {
    match gens::walk(member, &["desired.json"]).known()? {
        None => Ok(Desired::Running),
        Some(v) => match v.get("desired").and_then(Value::as_str) {
            Some("running") => Ok(Desired::Running),
            Some("stopped") => Ok(Desired::Stopped),
            _ => Err(Refusal::internal("desired.json names no desired state")),
        },
    }
}

struct Node {
    name: String,
    parent: Option<String>,
    from: Option<String>,
    void: bool,
    stopped: bool,
}

/// C0, C2, C-R2: the session's lifecycle members read once (record, desired state, and for a `resumedFrom`
/// lifecycle member its void evidence). Members of another session are skipped before their desired state is read;
/// an unreadable member under the caller's key refuses (A3a call 6). Every member's successor and parent links are
/// validated here (call 17): a loop refuses, naming every member on it (call 18).
pub struct Tree {
    nodes: BTreeMap<String, Node>,
}

fn plain(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .filter(|s| super::agent_id(s))
        .map(String::from)
}

fn node(record: &Value, void: bool, stopped: bool) -> Node {
    Node {
        name: super::text(record.get("name")),
        parent: plain(record.get("parentAgent").and_then(|p| p.get("id"))),
        from: plain(record.get("resumedFrom")),
        void,
        stopped,
    }
}

fn looped(code: u8, check: &'static str, message: String) -> Refusal {
    Refusal {
        code,
        check,
        message,
        reason: None,
        leftover: None,
        details: Default::default(),
    }
}

impl Tree {
    pub fn read(tx: &Tx<'_>) -> Res<Tree> {
        let mut nodes = BTreeMap::new();
        for (id, folder, member) in gens::members(&tx.scope.agents(&tx.ctx.state_root))? {
            // prelife-1c call 1 (G10): a pre-lifecycle member is not in the tree; nothing here reads or writes it.
            let g = match &member {
                gens::Member::Absent | gens::Member::Pre(_) => continue,
                gens::Member::Unreadable(e) => return Err(e.clone()),
                gens::Member::Life(g) => g,
            };
            let record = g.record();
            if record.get("session").and_then(Value::as_str) != Some(tx.scope.session.as_str()) {
                continue;
            }
            // Call 17: unreadable evidence refuses (`?`), never counts as void.
            let void = match plain(record.get("resumedFrom")) {
                Some(_) => g.never_ran_recorded(&g.status()?)?,
                None => false,
            };
            let stopped = desired_of(&folder)? == Desired::Stopped;
            nodes.insert(id, node(record, void, stopped));
        }
        let t = Tree { nodes };
        t.check()?;
        Ok(t)
    }

    /// Call 17: every member's successors and parents are validated once, at read.
    fn check(&self) -> Res<()> {
        for id in self.nodes.keys() {
            self.current(id)?;
        }
        for id in self.nodes.keys() {
            self.up(id, &mut vec![id.clone()])?;
        }
        Ok(())
    }

    /// Call 18: the members of a loop, by name, sorted.
    fn names(&self, ids: &[String]) -> String {
        let mut n: Vec<String> = ids.iter().map(|i| self.name(i)).collect();
        n.sort();
        n.join(", ")
    }

    fn up(&self, id: &str, path: &mut Vec<String>) -> Res<()> {
        for p in self.parents(id)? {
            if let Some(i) = path.iter().position(|x| *x == p) {
                let names = self.names(&path[i..]);
                return Err(looped(
                    2,
                    "authority",
                    format!("the parent chain loops: {names}"),
                ));
            }
            path.push(p.clone());
            self.up(&p, path)?;
            path.pop();
        }
        Ok(())
    }

    pub fn has(&self, id: &str) -> bool {
        self.nodes.contains_key(id)
    }

    pub fn name(&self, id: &str) -> String {
        self.nodes
            .get(id)
            .map_or_else(|| id.to_string(), |n| n.name.clone())
    }

    /// C0, call 17: the members that stand for `id` now: every non-void successor naming it in `resumedFrom`,
    /// followed forward, whether or not `id` still has a record; `id` itself when none.
    pub fn current(&self, id: &str) -> Res<Vec<String>> {
        let mut out = Vec::new();
        self.forward(id, &mut Vec::new(), &mut out)?;
        Ok(out)
    }

    fn forward(&self, id: &str, path: &mut Vec<String>, out: &mut Vec<String>) -> Res<()> {
        if let Some(i) = path.iter().position(|p| p == id) {
            let names = self.names(&path[i..]);
            return Err(looped(
                1,
                "internal",
                format!("a resumedFrom chain loops: {names}"),
            ));
        }
        let next: Vec<String> = (self.nodes.iter())
            .filter(|(_, n)| n.from.as_deref() == Some(id) && !n.void)
            .map(|(k, _)| k.clone())
            .collect();
        if next.is_empty() {
            if !out.iter().any(|o| o == id) {
                out.push(id.to_string());
            }
            return Ok(());
        }
        path.push(id.to_string());
        for n in &next {
            self.forward(n, path, out)?;
        }
        path.pop();
        Ok(())
    }

    /// The logical parents of `id` (C0), in id order; none for a leader or a member not in the tree. A parent
    /// with no lifecycle record (absent, or pre-lifecycle: prelife-1c call 1) and no non-void successor ends the
    /// chain (ambiguity 3).
    pub fn parents(&self, id: &str) -> Res<Vec<String>> {
        let Some(p) = self.nodes.get(id).and_then(|n| n.parent.clone()) else {
            return Ok(Vec::new());
        };
        Ok(self
            .current(&p)?
            .into_iter()
            .filter(|c| self.nodes.contains_key(c))
            .collect())
    }

    /// Call 18: `id` and every logical ancestor, breadth-first by distance, once each (loops refused at read).
    pub fn chain(&self, id: &str) -> Res<Vec<String>> {
        let mut out = vec![id.to_string()];
        let mut i = 0;
        while i < out.len() {
            let at = out[i].clone();
            for p in self.parents(&at)? {
                if !out.contains(&p) {
                    out.push(p);
                }
            }
            i += 1;
        }
        Ok(out)
    }

    /// C4.1: every member whose chain reaches `root`, `root` included, deepest first (a descendant's chain holds
    /// its ancestor's), then by id.
    pub fn below(&self, root: &str) -> Res<Vec<String>> {
        let mut found = Vec::new();
        for id in self.nodes.keys() {
            let chain = self.chain(id)?;
            if chain.iter().any(|c| c == root) {
                found.push((chain.len(), id.clone()));
            }
        }
        found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        Ok(found.into_iter().map(|(_, id)| id).collect())
    }

    /// C2, call 18: the nearest member of `id`'s chain (reflexive, successor-aware, breadth-first) that is desired
    /// stopped or that a journal lists (`listed`); None: open.
    pub fn closed_by(&self, id: &str, listed: &dyn Fn(&str) -> bool) -> Res<Option<String>> {
        let stopped = |c: &str| self.nodes.get(c).is_some_and(|n| n.stopped);
        Ok(self
            .chain(id)?
            .into_iter()
            .find(|c| listed(c) || stopped(c)))
    }
}

#[cfg(test)]
#[path = "cascade_tests.rs"]
mod tests;
