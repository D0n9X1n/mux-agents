//! adopt-3e A2, A3 (calls 8-10, 25): one `View` of the team; the source and its adopter, or why there is none.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{Value, json};

use super::{Change, overflow};
use crate::agents::cascade::Tree;
use crate::agents::gens;
use crate::agents::team_state::Tx;
use crate::error::{Refusal, Res};

/// A3, call 9: several logical parents at one level (a member resumed twice).
pub const SEVERAL: &str = "a parent has several successors; nothing was adopted";
/// A3, calls 8, 9, 25: the chain ends outside the team tree (an absent or pre-lifecycle member, a parentless
/// non-leader).
pub const LEAVES: &str = "the parent chain leaves the team tree; nothing was adopted";

/// One member's facts, from the record `Tree::read` holds and each generation's marks read once (AD-R2, call 10).
struct Facts {
    pending: bool,
    /// Every generation done (ambiguity 2).
    done: bool,
    /// The active generation's `ended` mark (ambiguity 1, checklist 8).
    ended: bool,
    /// The team's depth-0 `upTeam` leader (call 8).
    root: bool,
    depth: Option<u64>,
}
/// Checklist 1: the marks `Tree::read` already read (`known`) are reused, never read again.
fn facts_of(folder: &Path, r: &Value, known: &BTreeMap<u64, gens::Marks>) -> Res<Facts> {
    let active = gens::active(r);
    let completing = r
        .get("completing")
        .and_then(|c| c.get("gen"))
        .and_then(Value::as_u64);
    let mut all: BTreeSet<u64> = gens::named(folder)?.into_iter().collect();
    all.extend(active);
    let (mut done, mut ended) = (true, false);
    for n in all {
        let m = match known.get(&n) {
            Some(m) => *m,
            None => gens::marks(folder, n)?,
        };
        done &= m.any() && completing != Some(n);
        ended |= Some(n) == active && m.ended;
    }
    Ok(Facts {
        pending: r.get("pending").is_some(),
        done,
        ended,
        root: crate::agents::tree::is_leader(r),
        depth: r.get("depth").and_then(Value::as_u64),
    })
}

/// A3's answer: the adopter, or why there is none (call 9).
#[derive(Debug, Clone, PartialEq)]
pub enum Up {
    To(String),
    Blocked(&'static str),
}

/// A1-A3: cascade's `Tree` (C0, C2, its records) plus each member's facts, under the team lock (AD-R2).
pub struct View {
    tree: Tree,
    facts: BTreeMap<String, Facts>,
    listed: BTreeSet<String>,
}
impl View {
    pub fn read(tx: &Tx<'_>) -> Res<View> {
        let tree = Tree::read(tx)?;
        let listed = (crate::agents::cascade::journals(tx)?.iter())
            .flat_map(|j| j.entries.iter().map(|e| e.id.clone()))
            .collect();
        let mut facts = BTreeMap::new();
        for id in tree.ids() {
            let missing = || Refusal::internal("a tree member has no record");
            let r = tree.record(id).ok_or_else(missing)?;
            let known = tree.marks(id).ok_or_else(missing)?;
            facts.insert(id.clone(), facts_of(&tx.folder(id), r, known)?);
        }
        Ok(View {
            tree,
            facts,
            listed,
        })
    }
    /// Not superseded (C0: no non-void successor).
    fn current(&self, id: &str) -> Res<bool> {
        Ok(self.tree.current(id)? == [id])
    }
    /// Ambiguity 3.
    fn proven_ended(&self, id: &str) -> Res<bool> {
        let Some(f) = self.facts.get(id) else {
            return Ok(false);
        };
        Ok(f.ended && f.done && !f.pending && self.current(id)?)
    }
    fn root(&self, id: &str) -> bool {
        self.facts.get(id).is_some_and(|f| f.root)
    }
    /// A2: proven Ended, not the root (call 8), not closed (ambiguity 16).
    pub fn source(&self, id: &str) -> Res<bool> {
        if !self.proven_ended(id)? || self.root(id) {
            return Ok(false);
        }
        let listed = |x: &str| self.listed.contains(x);
        Ok(self.tree.closed_by(id, &listed)?.is_none())
    }
    /// The one logical parent of `id`, or why there is none.
    fn parent(&self, id: &str) -> Res<Result<String, &'static str>> {
        Ok(match self.tree.parents(id)?.as_slice() {
            [p] => Ok(p.clone()),
            [] => Err(LEAVES),
            _ => Err(SEVERAL),
        })
    }
    /// A3, for a source (never the root): the nearest logical ancestor not proven Ended; the root even when Ended. Call 25: accepted only when its
    /// own chain reaches the root through `Tree`; else the reason (call 9).
    pub fn adopter(&self, id: &str) -> Res<Up> {
        let mut at = id.to_string();
        let mut adopter = None;
        loop {
            if self.root(&at) {
                return Ok(Up::To(adopter.unwrap_or(at)));
            }
            let p = match self.parent(&at)? {
                Ok(p) => p,
                Err(why) => return Ok(Up::Blocked(why)),
            };
            if adopter.is_none() && !self.proven_ended(&p)? {
                adopter = Some(p.clone());
            }
            at = p;
        }
    }
    /// Whether `id`'s one logical parent is `p` (ambiguity 7).
    fn under(&self, id: &str, p: &str) -> Res<bool> {
        Ok(matches!(self.parent(id)?, Ok(q) if q == p))
    }
    /// Ambiguity 6: E's current children, live or pending.
    pub fn moving(&self, from: &str) -> Res<Vec<String>> {
        let mut out = Vec::new();
        for (id, f) in &self.facts {
            if self.under(id, from)? && self.current(id)? && (f.pending || !f.done) {
                out.push(id.clone());
            }
        }
        Ok(out)
    }
    /// A4.2's changes: `moving` under `to`, then every current member below them at its new depth, its own parent
    /// kept (ambiguities 6-8). Depths are checked (call 7).
    pub fn plan(&self, tx: &Tx<'_>, moving: &[String], to: &str) -> Res<Vec<Change>> {
        let none = || Refusal::internal("the adopter has no record");
        let (t, r) = (
            self.facts.get(to).ok_or_else(none)?,
            self.tree.record(to).ok_or_else(none)?,
        );
        let base = t
            .depth
            .ok_or_else(|| Refusal::internal("the adopter has no depth"))?;
        let parent_agent = json!({"client": r.get("client"), "pid": null, "start": null,
            "sessionId": r.get("sessionId"), "id": to});
        let shown = crate::agents::team_state::presented(&tx.folder(to), r.clone())?;
        let pane = (shown.get("pane"))
            .filter(|p| p.as_str().is_some_and(crate::agents::pane_id))
            .cloned()
            .unwrap_or(Value::Null);
        let first = base.checked_add(1).ok_or_else(overflow)?;
        let mut changes: Vec<Change> = (moving.iter())
            .map(|id| Change {
                id: id.clone(),
                parent_agent: parent_agent.clone(),
                parent_pane: pane.clone(),
                depth: first,
            })
            .collect();
        let mut placed: Vec<(String, u64)> = moving.iter().map(|id| (id.clone(), first)).collect();
        let mut i = 0;
        while let Some((p, d)) = placed.get(i).cloned() {
            i += 1;
            for id in self.facts.keys() {
                let seen = placed.iter().any(|(x, _)| x == id);
                if seen || !self.under(id, &p)? || !self.current(id)? {
                    continue;
                }
                let own = (self.tree.record(id))
                    .ok_or_else(|| Refusal::internal("a tree member has no record"))?;
                let depth = d.checked_add(1).ok_or_else(overflow)?;
                let own_pane = own.get("parentPane").filter(|v| v.is_string()).cloned();
                changes.push(Change {
                    id: id.clone(),
                    parent_agent: own["parentAgent"].clone(),
                    parent_pane: own_pane.unwrap_or(Value::Null),
                    depth,
                });
                placed.push((id.clone(), depth));
            }
        }
        Ok(changes)
    }
}

#[cfg(test)]
#[path = "adopt_view_tests.rs"]
mod tests;
