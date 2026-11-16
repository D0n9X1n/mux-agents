//! cascade-3d's one snapshot of the session's tree (C0, C2, C-R2; calls 10, 17-19, 21): `Tree` and the bar's `Shown`.
use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use super::{Desired, desired_of};
use crate::agents::gens;
use crate::agents::team_state::Tx;
use crate::error::{Refusal, Res};

pub(super) struct Node {
    name: String,
    /// A malformed `parentAgent.id` is no parent here, so `ancestors` refuses it (call 24); absent is no parent.
    parent: Option<String>,
    depth: Option<u64>,
    leader: bool,
    from: Option<String>,
    pub(super) void: bool,
    stopped: bool,
    /// The record's `generation`, `pending.gen` and `completing.gen` (the one snapshot, call 19).
    active: Option<u64>,
    pending: Option<u64>,
    completing: Option<u64>,
    /// A `resumedFrom` member's marks, read once for its void evidence and reused by `open_gens` (checklist 19).
    marks: BTreeMap<u64, gens::Marks>,
}

/// C0, C2, C-R2: the session's lifecycle members read once (record, desired state, and for a `resumedFrom`
/// lifecycle member its void evidence). Members of another session are skipped before their desired state is read;
/// an unreadable member under the caller's key refuses (A3a call 6). Every member's successor and parent links are
/// validated here (call 17): a loop refuses, naming every member on it (call 18).
pub struct Tree {
    pub(super) nodes: BTreeMap<String, Node>,
}

fn plain(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .filter(|s| crate::agents::agent_id(s))
        .map(String::from)
}

fn node(record: &Value, void: bool, stopped: bool) -> Node {
    Node {
        name: crate::agents::text(record.get("name")),
        parent: plain(record.get("parentAgent").and_then(|p| p.get("id"))),
        depth: record.get("depth").and_then(Value::as_u64),
        leader: crate::agents::tree::is_leader(record),
        from: plain(record.get("resumedFrom")),
        void,
        stopped,
        active: gens::active(record),
        pending: gens::pending(record),
        completing: record
            .get("completing")
            .and_then(|c| c.get("gen"))
            .and_then(Value::as_u64),
        marks: BTreeMap::new(),
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

/// Which members a scan keeps, and what it does with one it cannot read (calls 10, 21).
#[derive(Clone, Copy, PartialEq)]
enum View {
    /// C2's closure: lifecycle members of the session, desired state read.
    Closure,
    /// Authority, parent policy, depth: every member (pre-lifecycle included), no desired state.
    All,
    /// The bar: as `All`, but a member that cannot be read is recorded, never refused.
    Bar,
}

impl Tree {
    pub fn read(tx: &Tx<'_>) -> Res<Tree> {
        let t = Self::scan(tx, View::Closure, &mut Vec::new())?;
        t.check()?;
        Ok(t)
    }

    /// Call 10: authority's view: every member of the session, pre-lifecycle included (main's authority covers them;
    /// a pre-lifecycle successor is void by `resume::taken`'s legacy rule, call 23), successors followed forward and
    /// validated as `read`. It reads no desired state, so it is never used for closure.
    pub fn all(tx: &Tx<'_>) -> Res<Tree> {
        let t = Self::scan(tx, View::All, &mut Vec::new())?;
        t.check()?;
        Ok(t)
    }

    /// Checklist 1 (call 21): the bar's view, which never refuses (`Shown`).
    pub fn shown(tx: &Tx<'_>) -> Shown {
        let mut bad = Vec::new();
        let Ok(tree) = Self::scan(tx, View::Bar, &mut bad) else {
            let tree = Tree {
                nodes: BTreeMap::new(),
            };
            return Shown {
                tree,
                bad,
                exact: false,
            };
        };
        let exact = bad.is_empty();
        let looping: Vec<String> = (tree.nodes.keys())
            .filter(|id| tree.current(id).is_err() || tree.on_loop(id))
            .cloned()
            .collect();
        bad.extend(looping);
        Shown { tree, bad, exact }
    }

    fn scan(tx: &Tx<'_>, view: View, bad: &mut Vec<String>) -> Res<Tree> {
        let mut nodes = BTreeMap::new();
        for (id, folder, member) in gens::members(&tx.scope.agents(&tx.ctx.state_root))? {
            let (record, g) = match &member {
                gens::Member::Absent => continue,
                // prelife-1c call 1 (G10): a pre-lifecycle member is not in the closure; nothing here writes it.
                gens::Member::Pre(_) if view == View::Closure => continue,
                gens::Member::Unreadable(_) if view == View::Bar => {
                    bad.push(id);
                    continue;
                }
                gens::Member::Unreadable(e) => return Err(e.clone()),
                gens::Member::Pre(r) => (r, None),
                gens::Member::Life(g) => (g.record(), Some(g)),
            };
            let session = record.get("session").and_then(Value::as_str);
            let other = match view {
                View::Closure => session != Some(tx.scope.session.as_str()),
                // As `membership::locals`: a record naming another session is skipped, one naming none kept.
                View::All | View::Bar => session.is_some_and(|s| s != tx.scope.session),
            };
            if other {
                continue;
            }
            let mut n = node(record, false, false);
            if n.from.is_some() {
                let void = match g {
                    // Call 17: unreadable evidence refuses, never counts as void. Call 19: the marks read once.
                    Some(_) => {
                        let mut marks = BTreeMap::new();
                        let v = void_of(&folder, &n, &mut marks);
                        n.marks = marks;
                        v
                    }
                    None => Ok(crate::agents::resume::legacy_void(&folder)),
                };
                match void {
                    Ok(v) => n.void = v,
                    Err(_) if view == View::Bar => {
                        bad.push(id);
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }
            if view == View::Closure {
                n.stopped = desired_of(&folder)? == Desired::Stopped;
            }
            nodes.insert(id, n);
        }
        Ok(Tree { nodes })
    }

    /// The pure authority rules (`tree_tests`): records as given, nothing void, nothing validated.
    #[cfg(test)]
    pub fn from_records(records: &BTreeMap<String, Value>) -> Tree {
        let nodes = (records.iter())
            .map(|(id, r)| (id.clone(), node(r, false, false)))
            .collect();
        Tree { nodes }
    }

    /// Call 10: main's strict authority walk over the forward-resolved tree: every logical ancestor of `id`,
    /// nearest first, ending only at `leader`; each link present, well formed (call 24) and one shallower than its
    /// child; a member with no parent must be `leader`; no loop; main's length bound. Else 2 `authority` "the parent
    /// chain of <name> cannot be read". Depth falls strictly along every link, so the walk ends on any input.
    pub fn ancestors(&self, id: &str, leader: &str) -> Res<Vec<String>> {
        let bad = || Err(broken(&self.name(id)));
        let mut seen = vec![id.to_string()];
        let mut i = 0;
        while i < seen.len() {
            let at = seen[i].clone();
            i += 1;
            let Some(n) = self.nodes.get(&at) else {
                return bad();
            };
            if n.leader {
                if at != leader {
                    return bad();
                }
                continue;
            }
            let ps = self.parents(&at)?;
            let deep = n
                .depth
                .is_none_or(|d| d > crate::agents::tree::MAX_DEPTH + 1);
            if ps.is_empty() || deep {
                return bad();
            }
            for p in ps {
                let up = self.nodes.get(&p).and_then(|x| x.depth);
                if p == id || up.map(|u| u + 1) != n.depth {
                    return bad();
                }
                if !seen.contains(&p) {
                    seen.push(p);
                }
            }
        }
        Ok(seen.split_off(1))
    }

    /// Call 21: `id` is on a parent loop (its own logical ancestors reach it). Never refuses: a `resumedFrom` loop on
    /// the way counts as no parent there.
    fn on_loop(&self, id: &str) -> bool {
        let mut seen: Vec<String> = Vec::new();
        let mut todo = self.parents(id).unwrap_or_default();
        while let Some(p) = todo.pop() {
            if p == id {
                return true;
            }
            if seen.contains(&p) {
                continue;
            }
            todo.extend(self.parents(&p).unwrap_or_default());
            seen.push(p);
        }
        false
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

fn broken(name: &str) -> Refusal {
    looped(
        2,
        "authority",
        format!("the parent chain of {name} cannot be read"),
    )
}

/// Checklist 1 (call 21): the bar's view of the tree. It never refuses: a member that cannot be read, or that sits on
/// a parent or `resumedFrom` loop, is `bad`; when any member cannot be read, no member is placed (`exact` false),
/// because the unreadable one may be a successor that would move another's parent.
pub struct Shown {
    tree: Tree,
    bad: Vec<String>,
    exact: bool,
}

impl Shown {
    /// The member the bar marks unreadable.
    pub fn bad(&self, id: &str) -> bool {
        self.bad.iter().any(|b| b == id)
    }
    /// `id`'s logical parents (C0); None: the bar falls back to the raw parent id.
    pub fn placed(&self, id: &str) -> Option<Vec<String>> {
        if !self.exact || self.bad(id) || !self.tree.has(id) {
            return None;
        }
        self.tree.parents(id).ok()
    }
}

/// Call 19, checklist 19: `Gen::never_ran_recorded` of `Gen::status`, decided from the snapshot: the record's own
/// `generation`, `pending` and `completing`, and each generation's marks read once into `marks`, which the node
/// keeps so `open_gens` reads them no second time. The same files in the same order as `status`, then
/// `never_ran_recorded`; an unreadable marker refuses where they would (never void on Unreadable, call 17).
fn void_of(folder: &Path, n: &Node, marks: &mut BTreeMap<u64, gens::Marks>) -> Res<bool> {
    let mut mark = |g: u64| -> Res<gens::Marks> {
        if let Some(m) = marks.get(&g) {
            return Ok(*m);
        }
        let m = gens::marks(folder, g)?;
        marks.insert(g, m);
        Ok(m)
    };
    let pm = n.pending.map(&mut mark).transpose()?;
    // `status` reads the active marks too: an unreadable one refuses here as there.
    n.active.map(&mut mark).transpose()?;
    let p_done = n
        .pending
        .zip(pm)
        .map(|(g, m)| m.any() && n.completing != Some(g));
    let p_void = pm.is_some_and(|m| m.abandoned || m.decision == Some(gens::Decision::Aborted));
    let never_ran = n.active.is_none() && (n.pending.is_none() || (p_void && p_done == Some(true)));
    if !never_ran {
        return Ok(false);
    }
    for g in gens::named(folder)? {
        if Some(g) == n.pending {
            continue;
        }
        let m = mark(g)?;
        if !(m.abandoned || m.decision == Some(gens::Decision::Aborted)) {
            return Ok(false);
        }
    }
    Ok(true)
}

impl Tree {
    /// State-0 S6 from the snapshot (call 19): every generation the files name (`gens::named`) or the record names
    /// (active, pending), minus those done: a decisive marker and not the record's `completing`. A successor's marks
    /// are the ones `Tree::read` kept; any other member's are read here once. R2: a generation whose markers cannot
    /// be read stays listed (C6's `terminate` answers it unresolved).
    pub(super) fn open_gens(&self, member: &Path, id: &str) -> Res<Vec<u64>> {
        let n = (self.nodes.get(id)).ok_or_else(|| Refusal::internal("not in the tree"))?;
        let mut all = gens::named(member)?;
        all.extend(n.active);
        all.extend(n.pending);
        all.sort_unstable();
        all.dedup();
        let done = |g: u64| {
            let m = match n.marks.get(&g) {
                Some(m) => Ok(*m),
                None => gens::marks(member, g),
            };
            m.is_ok_and(|m| m.any()) && n.completing != Some(g)
        };
        Ok(all.into_iter().filter(|g| !done(*g)).collect())
    }
}
