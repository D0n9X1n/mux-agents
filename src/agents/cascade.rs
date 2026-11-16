//! cascade-3d: a stop records `stopped` for a member's whole sub-tree, closes it, then ends it bottom-up; a journal
//! under `teams/<sha16(session)>.cascade/` lets the next stop, `up` or bar tick finish a cascade cut short.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::gens;
use super::registry::{self, Notice};
use super::team_state::{Scope, Tx};
use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::error::{Refusal, Res};
use crate::state::evidence;
use crate::state::fs::{sync_dir, write_json_durable};

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
            let mut n = node(record, false, false);
            // Call 17: unreadable evidence refuses (`?`), never counts as void. Call 19: from the marks read once.
            if n.from.is_some() {
                let mut marks = BTreeMap::new();
                n.void = void_of(&folder, &n, &mut marks)?;
                n.marks = marks;
            }
            n.stopped = desired_of(&folder)? == Desired::Stopped;
            nodes.insert(id, n);
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

    /// Call 11: a member that records no parent (the team's root) and is desired stopped stays closed.
    fn root_stopped(&self, id: &str) -> bool {
        self.nodes
            .get(id)
            .is_some_and(|n| n.parent.is_none() && n.stopped)
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

/// A failed journal write, flush or removal (C-R5; tests assert this, never io text).
pub const NOT_WRITTEN: &str = "the cascade journal could not be written";

/// C4.2, ambiguity 1: one member of a cascade, fixed when the journal is published (C-R3).
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    /// In-journal logical parents (C0), so an unresolved entry holds only its own ancestors.
    pub parents: Vec<String>,
    pub gens: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Journal {
    pub root: String,
    /// Deepest first; the root last.
    pub entries: Vec<Entry>,
    pub prepared: bool,
    /// Call 29: read from a `<root>.removed` tombstone, an unfinished removal. It still fences its sub-tree until
    /// `remove` or `finish`'s sweep completes it. Never written into the body.
    pub removing: bool,
}

impl Journal {
    pub fn lists(&self, id: &str) -> bool {
        self.entries.iter().any(|e| e.id == id)
    }

    fn body(&self) -> Value {
        let entries: Vec<Value> = (self.entries.iter())
            .map(|e| json!({"id": e.id, "name": e.name, "parents": e.parents, "gens": e.gens}))
            .collect();
        json!({"root": self.root, "entries": entries, "prepared": self.prepared})
    }

    /// Call 16: entries non-empty, ids unique, the root last with no parents, every other entry with a parent,
    /// every parent listed after its child (deepest first, so no cycle).
    fn well_formed(&self) -> bool {
        let at = |id: &str| self.entries.iter().position(|e| e.id == id);
        let Some(last) = self.entries.last() else {
            return false;
        };
        let unique = (self.entries.iter().enumerate()).all(|(i, e)| at(&e.id) == Some(i));
        let shaped = (self.entries.iter().enumerate()).all(|(i, e)| {
            let root = e.id == self.root;
            root == (i + 1 == self.entries.len())
                && root == e.parents.is_empty()
                && e.parents.iter().all(|p| at(p).is_some_and(|j| j > i))
        });
        last.id == self.root && unique && shaped
    }

    /// Strict (C-R5, call 16, checklist 21): every field typed, every id plain, the root the file's own, the shape
    /// well formed.
    fn parse(root: &str, v: &Value) -> Option<Journal> {
        let id = |x: &Value| x.as_str().filter(|s| super::agent_id(s)).map(String::from);
        if v.get("root").and_then(id).as_deref() != Some(root) {
            return None;
        }
        let mut entries = Vec::new();
        for e in v.get("entries")?.as_array()? {
            let parents = (e.get("parents")?.as_array()?.iter())
                .map(id)
                .collect::<Option<Vec<_>>>()?;
            let gens = (e.get("gens")?.as_array()?.iter())
                .map(Value::as_u64)
                .collect::<Option<Vec<_>>>()?;
            entries.push(Entry {
                id: id(e.get("id")?)?,
                name: e.get("name")?.as_str()?.to_string(),
                parents,
                gens,
            });
        }
        let j = Journal {
            root: root.into(),
            entries,
            prepared: v.get("prepared")?.as_bool()?,
            removing: false,
        };
        j.well_formed().then_some(j)
    }
}

fn not_written() -> Refusal {
    Refusal {
        code: 1,
        check: "state",
        message: NOT_WRITTEN.into(),
        reason: None,
        leftover: None,
        details: Default::default(),
    }
}

fn flush(dir: &Path) -> Res<()> {
    sync_dir(dir).map_err(|_| not_written())
}

/// `teams/<sha16(session)>.cascade` of the scope (state-0 S6), beside the team record.
pub fn dir_at(ctx: &Ctx, scope: &Scope) -> PathBuf {
    super::team::path(&ctx.state_root.join(&scope.key), &scope.session).with_extension("cascade")
}

/// Call 15, as `gens::walk`: Ok(true) a real folder, Ok(false) absent; a link, junction or anything else `evidence`.
fn level(path: &Path) -> Res<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_dir() => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        _ => Err(evidence::unreadable()),
    }
}

/// `<key>`, `teams`, `.cascade`: the three levels below the trusted state root.
fn levels(ctx: &Ctx, scope: &Scope) -> Res<(PathBuf, PathBuf, PathBuf)> {
    let dir = dir_at(ctx, scope);
    let teams = dir.parent().ok_or_else(evidence::unreadable)?.to_path_buf();
    let key = teams
        .parent()
        .ok_or_else(evidence::unreadable)?
        .to_path_buf();
    Ok((key, teams, dir))
}

/// Call 15: the walk from the state root; Ok(false) at the first absent level.
fn present(ctx: &Ctx, scope: &Scope) -> Res<bool> {
    let (key, teams, dir) = levels(ctx, scope)?;
    Ok(level(&key)? && level(&teams)? && level(&dir)?)
}

/// C8's lock-free hint: the journal folder holds an entry (a journal or a tombstone), or the walk refuses (then
/// `finish` refuses too).
pub fn waiting(ctx: &Ctx, scope: &Scope) -> bool {
    match present(ctx, scope) {
        Ok(false) => false,
        Err(_) => true,
        Ok(true) => match std::fs::read_dir(dir_at(ctx, scope)) {
            Ok(mut d) => d.next().is_some(),
            Err(_) => true,
        },
    }
}

/// Call 15: the one name the reader skips, when (and only when) it is a regular file: a cut-short durable write
/// (`<id>.json.<32 hex>.tmp`, fs.rs). A tombstone is read (call 29).
fn temporary(name: &str, path: &Path) -> bool {
    let (root, rest) = name.split_once('.').unwrap_or((name, ""));
    let hex = rest
        .strip_prefix("json.")
        .and_then(|r| r.strip_suffix(".tmp"));
    super::agent_id(root)
        && hex.is_some_and(|h| h.len() == 32 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        && std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

/// Every journal of the team, sorted by root, a tombstone after its root's journal (C-R5, R2, R5, calls 15, 16,
/// 29). A `<root>.removed` tombstone is a journal whose removal is unfinished (`removing`).
pub fn journals(tx: &Tx<'_>) -> Res<Vec<Journal>> {
    if !present(tx.ctx, &tx.scope)? {
        return Ok(Vec::new());
    }
    let dir = dir_at(tx.ctx, &tx.scope);
    let mut out = Vec::new();
    for e in std::fs::read_dir(&dir).map_err(|_| evidence::unreadable())? {
        let name = (e.map_err(|_| evidence::unreadable())?.file_name())
            .to_string_lossy()
            .into_owned();
        let path = dir.join(&name);
        if temporary(&name, &path) {
            continue;
        }
        let (root, removing) = match (name.strip_suffix(".json"), name.strip_suffix(".removed")) {
            (Some(r), _) => (r, false),
            (_, Some(r)) => (r, true),
            _ => return Err(evidence::unreadable()),
        };
        if !super::agent_id(root) {
            return Err(evidence::unreadable());
        }
        // `evidence::read` opens regular files only: a folder or a link here is `evidence`.
        let v = evidence::read(&path)
            .known()?
            .ok_or_else(evidence::unreadable)?;
        let mut j = Journal::parse(root, &v).ok_or_else(evidence::unreadable)?;
        j.removing = removing;
        out.push(j);
    }
    out.sort_by(|a, b| (a.root.as_str(), a.removing).cmp(&(b.root.as_str(), b.removing)));
    Ok(out)
}

/// Makes `dir` when absent; a present one must be a real folder (call 15).
fn make(dir: &Path) -> Res<()> {
    if !level(dir)? {
        std::fs::create_dir(dir).map_err(|_| not_written())?;
        if !level(dir)? {
            return Err(evidence::unreadable());
        }
    }
    Ok(())
}

/// C4.2, call 14: create-once; every flush on every call (`<key>` after `teams` is checked or made, `teams` after
/// `.cascade`, the content before the name, `.cascade` after it). An identical journal already there is this one
/// (a retry); another refuses `internal` (callers run `finish` first).
pub fn publish(tx: &Tx<'_>, j: &Journal) -> Res<()> {
    let (key, teams, dir) = levels(tx.ctx, &tx.scope)?;
    if !level(&key)? {
        return Err(evidence::unreadable()); // `state_dir` makes the server folder; the cascade never does
    }
    make(&teams)?;
    flush(&key)?;
    make(&dir)?;
    flush(&teams)?;
    let path = dir.join(format!("{}.json", j.root));
    if evidence::publish(&path, &j.body()).map_err(|_| not_written())?
        == evidence::Published::Exists
    {
        let v = evidence::read(&path)
            .known()?
            .ok_or_else(evidence::unreadable)?;
        if Journal::parse(&j.root, &v).as_ref() != Some(j) {
            return Err(Refusal::internal("a cascade journal of this member exists"));
        }
    }
    flush(&dir)
}

/// C4.4, calls 14, 30: the same journal, `prepared: true`, written whole after `<key>` and `teams` are flushed;
/// `write_json_durable` flushes its content, then `.cascade`.
pub fn mark_prepared(tx: &Tx<'_>, j: &Journal) -> Res<Journal> {
    if !present(tx.ctx, &tx.scope)? {
        return Err(Refusal::internal("the cascade journal folder is gone"));
    }
    let (key, teams, dir) = levels(tx.ctx, &tx.scope)?;
    flush(&key)?;
    flush(&teams)?;
    let p = Journal {
        prepared: true,
        ..j.clone()
    };
    write_json_durable(&dir.join(format!("{}.json", j.root)), &p.body())
        .map_err(|_| not_written())?;
    Ok(p)
}

/// C7, calls 14, 29, 30: rename `<root>.json` to the tombstone `<root>.removed`, then `tombstoned`; each also when
/// already done, so a retry repeats every flush.
pub fn remove(tx: &Tx<'_>, root: &str) -> Res<()> {
    if !present(tx.ctx, &tx.scope)? {
        return Ok(());
    }
    let dir = dir_at(tx.ctx, &tx.scope);
    match std::fs::rename(
        dir.join(format!("{root}.json")),
        dir.join(format!("{root}.removed")),
    ) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(not_written()),
    }
    tombstoned(tx, root)
}

/// Call 29: completes a removal from its tombstone: the rename's flushes (`<key>`, `teams`, `.cascade`), the halt
/// `cascade-tombstoned`, the tombstone's deletion, and `.cascade` flushed again. A failed flush keeps the tombstone.
fn tombstoned(tx: &Tx<'_>, root: &str) -> Res<()> {
    let (key, teams, dir) = levels(tx.ctx, &tx.scope)?;
    flush(&key)?;
    flush(&teams)?;
    flush(&dir)?;
    super::release::halt("cascade-tombstoned")?;
    match std::fs::remove_file(dir.join(format!("{root}.removed"))) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(not_written()),
    }
    flush(&dir)
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
    fn open_gens(&self, member: &Path, id: &str) -> Res<Vec<u64>> {
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

/// C4, under the team lock after the replay hooks; never waits for pending launches. C4.1 entries as they stand
/// (one Tree, C-R2, call 19), C4.2 published, then C4.3-4.4. A failed write kills nothing (C-R4).
pub fn prepare(tx: &Tx<'_>, root: &str) -> Res<Journal> {
    let t = Tree::read(tx)?;
    if !t.has(root) {
        return Err(Refusal::internal(
            "the stopped member is not a lifecycle member of this session",
        ));
    }
    let ids = t.below(root)?;
    let mut entries = Vec::new();
    for id in &ids {
        let parents = if id == root {
            Vec::new()
        } else {
            (t.parents(id)?.into_iter())
                .filter(|p| ids.contains(p))
                .collect()
        };
        let gens = t.open_gens(&tx.folder(id), id)?;
        entries.push(Entry {
            id: id.clone(),
            name: t.name(id),
            parents,
            gens,
        });
    }
    let j = Journal {
        root: root.into(),
        entries,
        prepared: false,
        removing: false,
    };
    publish(tx, &j)?;
    super::release::halt("cascade-journal")?;
    close(tx, &j, &[])
}

/// C5 for an unprepared journal: C4.3-4.4 from the journal alone (C-R3). p1c call 19: each entry's record is read
/// once first, and a pre-lifecycle entry is skipped before any write (no desired.json; Task 5 reports it).
pub fn desire(tx: &Tx<'_>, j: &Journal) -> Res<Journal> {
    let mut pre = Vec::new();
    for e in &j.entries {
        match gens::member_of(&tx.folder(&e.id)) {
            gens::Member::Pre(_) => pre.push(e.id.clone()),
            gens::Member::Unreadable(r) => return Err(r),
            gens::Member::Absent | gens::Member::Life(_) => {}
        }
    }
    close(tx, j, &pre)
}

/// C4.3-4.4: each entry but `skip` desired stopped (halt `cascade-desired-<id>` after each), then the journal
/// rewritten prepared (halt `cascade-prepared`).
fn close(tx: &Tx<'_>, j: &Journal, skip: &[String]) -> Res<Journal> {
    for e in &j.entries {
        if skip.contains(&e.id) {
            continue;
        }
        tx.desired(&e.id, false)?;
        super::release::halt(&format!("cascade-desired-{}", e.id))?;
    }
    let p = mark_prepared(tx, j)?;
    super::release::halt("cascade-prepared")?;
    Ok(p)
}

/// C6: the stable reason a reply carries when a notice did not land (tests assert it).
pub const NOT_NOTICED: &str = "its Stopped notice is not recorded yet";

/// C9: one entry whose own end did not complete.
#[derive(Debug, Clone, PartialEq)]
pub struct Unresolved {
    pub name: String,
    pub check: &'static str,
    pub reason: String,
}

/// One journal's pass: its entries, whether the journal was removed (every entry complete), and what is not.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub root: String,
    pub ids: Vec<String>,
    pub stopped: bool,
    pub unresolved: Vec<Unresolved>,
}

impl Report {
    pub fn lists(&self, id: &str) -> bool {
        self.ids.iter().any(|i| i == id)
    }
    /// C9: `stopped` and `unresolved: [{name, check, reason}]`.
    pub fn reply(&self, reply: &mut serde_json::Map<String, Value>) {
        let u: Vec<Value> = (self.unresolved.iter())
            .map(|u| json!({"name": u.name, "check": u.check, "reason": u.reason}))
            .collect();
        reply.insert("stopped".into(), self.stopped.into());
        reply.insert("unresolved".into(), u.into());
    }
}

fn refusal(check: &'static str, message: &str) -> Refusal {
    Refusal {
        code: 1,
        check,
        message: message.into(),
        reason: None,
        leftover: None,
        details: Default::default(),
    }
}

/// C6, prepared journals only, deepest first (C-R3). A failed entry is unresolved and holds its in-journal
/// ancestors (never asked of hc this pass); a `halted` refusal is a crash and passes through (C-R1). C7: every
/// entry complete with its marker, then the journal is removed.
pub fn execute(tx: &Tx<'_>, caller: &Caller, j: &Journal) -> Res<Report> {
    if !j.prepared {
        return Err(Refusal::internal(
            "an unprepared cascade journal is never executed",
        ));
    }
    let (mut held, mut unresolved) = (Vec::new(), Vec::new());
    for e in &j.entries {
        if held.contains(&e.id) {
            continue;
        }
        if let Err(r) = end_entry(tx, caller, e) {
            if r.check == "halted" {
                return Err(r);
            }
            unresolved.push(Unresolved {
                name: e.name.clone(),
                check: r.check,
                reason: r.message,
            });
            hold(j, &e.id, &mut held);
        }
    }
    let stopped = unresolved.is_empty();
    if stopped {
        remove(tx, &j.root)?;
        super::release::halt("cascade-removed")?;
    }
    Ok(Report {
        root: j.root.clone(),
        ids: j.entries.iter().map(|e| e.id.clone()).collect(),
        stopped,
        unresolved,
    })
}

/// Every in-journal ancestor of `id` (the stored parent links; C4.2 refused loops).
fn hold(j: &Journal, id: &str, held: &mut Vec<String>) {
    let Some(e) = j.entries.iter().find(|e| e.id == id) else {
        return;
    };
    for p in &e.parents {
        if !held.contains(p) {
            held.push(p.clone());
            hold(j, p, held);
        }
    }
}

/// C6 for one entry. p1c call 19 (checklist 26): the record is read once; a pre-lifecycle entry is unresolved
/// `prelife` before any write or hc call. Every listed, named, active or pending generation, newest first, through
/// `terminate` (C-R1; ambiguity 4: the first unresolved one stops the entry, so an unresolved pending launch keeps
/// the older generation running); then the record is read once more (the
/// terminations changed it, C-R2) and the Stopped notice of the highest generation is posted (message, then
/// marker; a delivered message without its marker is retried with the same id).
fn end_entry(tx: &Tx<'_>, caller: &Caller, e: &Entry) -> Res<()> {
    let folder = tx.folder(&e.id);
    let no_record = || Refusal::internal("the member has no record");
    let r = match gens::member_of(&folder) {
        gens::Member::Life(g) => g.record().clone(),
        gens::Member::Pre(_) => {
            return Err(refusal(
                "prelife",
                "a pre-lifecycle member is never ended by a cascade",
            ));
        }
        gens::Member::Unreadable(r) => return Err(r),
        gens::Member::Absent => return Err(no_record()),
    };
    let mut all = e.gens.clone();
    all.extend(gens::named(&folder)?);
    all.extend(gens::active(&r));
    all.extend(gens::pending(&r));
    all.sort_unstable();
    all.dedup();
    for n in all.iter().rev() {
        gens::terminate(tx, &e.id, *n)?;
    }
    let Some(&last) = all.last() else {
        return Ok(());
    };
    let r = gens::record(&folder).known()?.ok_or_else(no_record)?;
    registry::with_member_notice(tx.ctx, caller, &e.id, || {
        registry::add_notice_gen(tx.ctx, caller, &e.id, &r, last, Notice::Stopped)
    })?;
    if gens::read(&folder, last, "notice").known()?.is_none() {
        return Err(refusal("notice", NOT_NOTICED));
    }
    super::release::halt(&format!("cascade-noticed-{}", e.id))
}

/// C5, C8: every journal of the team, under the team lock. Call 29: every tombstone's removal is completed first;
/// then an unprepared journal finishes C4.3-4.4, and each executes. `Unreadable` stops with `evidence`.
pub fn finish(tx: &Tx<'_>, caller: &Caller) -> Res<Vec<Report>> {
    let all = journals(tx)?;
    for j in all.iter().filter(|j| j.removing) {
        tombstoned(tx, &j.root)?;
    }
    let mut out = Vec::new();
    for j in all.into_iter().filter(|j| !j.removing) {
        let j = if j.prepared { j } else { desire(tx, &j)? };
        out.push(execute(tx, caller, &j)?);
    }
    Ok(out)
}

/// C4, ambiguity 6: `kept` is this lock hold's `finish`; a report of `root` answers, else prepare and execute.
pub fn stop(tx: &Tx<'_>, caller: &Caller, root: &str, kept: &[Report]) -> Res<Report> {
    if let Some(r) = kept.iter().find(|r| r.root == root) {
        return Ok(r.clone());
    }
    let j = prepare(tx, root)?;
    execute(tx, caller, &j)
}

/// C2's refusal: 2 `closed`, naming the stopped (or listed) member that closes the sub-tree.
fn closed(name: &str) -> Refusal {
    Refusal {
        code: 2,
        check: "closed",
        message: format!("{name} is stopped; its sub-tree takes no new members"),
        reason: None,
        leftover: None,
        details: Default::default(),
    }
}

/// C2: a new member under `parent` (C0: each member standing for it now) needs every one open: none of their
/// chains desired stopped or listed by a journal (a tombstone included, call 29). A parent with no lifecycle record
/// in the session is not checked.
pub fn admit(tx: &Tx<'_>, parent: &str) -> Res<()> {
    let t = Tree::read(tx)?;
    if !t.has(parent) {
        return Ok(());
    }
    let js = journals(tx)?;
    let listed = |x: &str| js.iter().any(|j| j.lists(x));
    for p in t.current(parent)? {
        if let Some(by) = t.closed_by(&p, &listed)? {
            return Err(closed(&t.name(&by)));
        }
    }
    Ok(())
}

/// C3: X may be resumed, retried or freshened only when no journal (a tombstone included) lists X, a member of X's
/// chain or a member below X (nearest first, then deepest first), and X's parent is open. X's own `stopped` does
/// not refuse: that is the reopen.
pub fn reopen_check(tx: &Tx<'_>, id: &str) -> Res<()> {
    let t = Tree::read(tx)?;
    if t.root_stopped(id) {
        return Err(closed(&t.name(id)));
    }
    let js = journals(tx)?;
    let listed = |x: &str| js.iter().any(|j| j.lists(x));
    let mut near = t.chain(id)?;
    near.extend(t.below(id)?.into_iter().filter(|b| b != id));
    if let Some(by) = near.iter().find(|x| listed(x)) {
        return Err(closed(&t.name(by)));
    }
    for p in t.parents(id)? {
        if let Some(by) = t.closed_by(&p, &listed)? {
            return Err(closed(&t.name(&by)));
        }
    }
    Ok(())
}

/// C3, same id (retry, fresh): the reopen check, `tx.desired(X, running)`, then the launch (launch-1 G1, `pending`
/// first). Death between them: X is `running` with its old generation done or failed, so reconcile never relaunches
/// it (restore-6b rows 5-6) and a person repeats the action. Death after `pending`: settle-1b G3 replays it.
/// X's descendants stay stopped.
pub fn reopen_same<T>(tx: &Tx<'_>, id: &str, launch: impl FnOnce() -> Res<T>) -> Res<T> {
    reopen_check(tx, id)?;
    tx.desired(id, true)?;
    super::release::halt("reopened")?;
    launch()
}

/// C8, the bar: a lock-free look for the journal folder, then one try of the team lock (state-0 S9: the bar
/// try-locks). Busy: false at once. A refusal (an `Unreadable` journal) is returned; the board ignores it.
pub fn tick(ctx: &Ctx, caller: &Caller) -> Res<bool> {
    let scope = Scope::of(caller);
    if !waiting(ctx, &scope) {
        return Ok(false);
    }
    let ran = super::try_lock(ctx, &scope.team_lock(), || {
        super::team_state::held(ctx, &scope, |tx| finish(tx, caller))
    })?;
    Ok(ran.is_some())
}

#[cfg(test)]
#[path = "cascade_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "cascade_crash_tests.rs"]
mod crash;
