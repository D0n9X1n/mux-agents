//! cascade-3d's journal files under `teams/<sha16(session)>.cascade/` (C4, C5, C7; C-R5; calls 14-16, 29, 30).
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::agents::team_state::{Scope, Tx};
use crate::ctx::Ctx;
use crate::error::{Refusal, Res};
use crate::state::evidence;
use crate::state::fs::{sync_dir, write_json_durable};

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
        let id = |x: &Value| {
            x.as_str()
                .filter(|s| crate::agents::agent_id(s))
                .map(String::from)
        };
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
    crate::agents::team::path(&ctx.state_root.join(&scope.key), &scope.session)
        .with_extension("cascade")
}

/// Call 15, as `gens::walk`: Ok(true) a real folder, Ok(false) absent; a link, junction or anything else `evidence`.
pub(crate) fn level(path: &Path) -> Res<bool> {
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
    crate::agents::agent_id(root)
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
        if !crate::agents::agent_id(root) {
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
pub(crate) fn make(dir: &Path) -> Res<()> {
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
pub(super) fn tombstoned(tx: &Tx<'_>, root: &str) -> Res<()> {
    let (key, teams, dir) = levels(tx.ctx, &tx.scope)?;
    flush(&key)?;
    flush(&teams)?;
    flush(&dir)?;
    crate::agents::release::halt("cascade-tombstoned")?;
    match std::fs::remove_file(dir.join(format!("{root}.removed"))) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(not_written()),
    }
    flush(&dir)
}
