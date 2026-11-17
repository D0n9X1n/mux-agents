//! adopt-3e: when a member ends on its own, its nearest surviving ancestor adopts its live and pending children
//! ("Adoption", "Ended leader"). Journal `teams/<sha16>.adopt/<seq>.json`; committed `topology.json`.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::cascade::level;
use super::team_state::{Guard, Scope, Tx};
use crate::ctx::Ctx;
use crate::error::{Refusal, Res};
use crate::state::evidence;
use crate::state::fs::{sync_dir, sync_file, write_json_durable};

/// A4 replay: two journals, a gap, a live tombstone, another body of a seq (tests assert this, never io text).
pub const OUT_OF_ORDER: &str = "adoption journals are out of order; nothing was changed";
/// AD-R5: a failed journal, seq or removal write or flush.
pub const NOT_WRITTEN: &str = "the adoption journal could not be written";

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
fn not_written() -> Refusal {
    refusal("state", NOT_WRITTEN)
}
fn flush(dir: &Path) -> Res<()> {
    sync_dir(dir).map_err(|_| not_written())
}
/// Cascade's `make` (call 6: one folder rule), its failed create reported as adoption's.
fn made(dir: &Path) -> Res<()> {
    super::cascade::make(dir).map_err(|e| if e.check == "state" { not_written() } else { e })
}
/// `teams/<sha16(session)>.topology.json`.
pub fn seq_path(ctx: &Ctx, scope: &Scope) -> PathBuf {
    super::team::path(&ctx.state_root.join(&scope.key), &scope.session)
        .with_extension("topology.json")
}
/// `teams/<sha16(session)>.adopt`.
pub fn dir_at(ctx: &Ctx, scope: &Scope) -> PathBuf {
    super::team::path(&ctx.state_root.join(&scope.key), &scope.session).with_extension("adopt")
}
/// `<key>`, `teams`, `.adopt`: the three levels below the trusted state root (call 15).
fn levels(ctx: &Ctx, scope: &Scope) -> Res<(PathBuf, PathBuf, PathBuf)> {
    let dir = dir_at(ctx, scope);
    let teams = dir.parent().ok_or_else(evidence::unreadable)?.to_path_buf();
    let key = teams
        .parent()
        .ok_or_else(evidence::unreadable)?
        .to_path_buf();
    Ok((key, teams, dir))
}

/// AD-R3: one member's absolute topology.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub id: String,
    pub parent_agent: Value,
    pub parent_pane: Value,
    pub depth: u64,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Journal {
    pub seq: u64,
    pub from: String,
    pub to: String,
    pub changes: Vec<Change>,
}
fn keys(o: &Map<String, Value>) -> BTreeSet<&str> {
    o.keys().map(String::as_str).collect()
}
fn plain(x: Option<&Value>) -> Option<String> {
    x.and_then(Value::as_str)
        .filter(|s| super::agent_id(s))
        .map(String::from)
}
impl Journal {
    pub fn body(&self) -> Value {
        let changes: Vec<Value> = (self.changes.iter())
            .map(|c| json!({"id": c.id, "parentAgent": c.parent_agent, "parentPane": c.parent_pane, "depth": c.depth}))
            .collect();
        json!({"seq": self.seq, "from": self.from, "to": self.to, "changes": changes})
    }
    /// AD-R4: exactly the keys (checked before any field is read, so a missing key is a refusal, never a panic),
    /// typed; `seq` is the file's.
    fn parse(seq: u64, v: &Value) -> Option<Journal> {
        let o = v.as_object()?;
        if keys(o) != BTreeSet::from(["seq", "from", "to", "changes"])
            || o.get("seq")?.as_u64() != Some(seq)
        {
            return None;
        }
        let mut changes = Vec::new();
        for c in o.get("changes")?.as_array()? {
            let co = c.as_object()?;
            if keys(co) != BTreeSet::from(["id", "parentAgent", "parentPane", "depth"]) {
                return None;
            }
            let (pa, pp) = (co.get("parentAgent")?, co.get("parentPane")?);
            if plain(pa.get("id")).is_none() || !(pp.is_null() || pp.is_string()) {
                return None;
            }
            let (id, depth) = (plain(co.get("id"))?, co.get("depth")?.as_u64()?);
            changes.push(Change {
                id,
                parent_agent: pa.clone(),
                parent_pane: pp.clone(),
                depth,
            });
        }
        Some(Journal {
            seq,
            from: plain(o.get("from"))?,
            to: plain(o.get("to"))?,
            changes,
        })
    }
}

/// What `.adopt` holds, by name only (call 14: a lock-free reader never opens a journal).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    pub journals: Vec<u64>,
    pub tombstones: Vec<u64>,
}
enum Named {
    Journal(u64),
    Tombstone(u64),
    Temp,
}
/// `<seq>.json`, `<seq>.removed`, `<seq>.json.<32 hex>.tmp` (decimal, no leading zero, at least 1); else None.
fn named(name: &str) -> Option<Named> {
    let (num, rest) = name.split_once('.')?;
    if num.is_empty() || num.starts_with('0') || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u64 = num.parse().ok()?;
    match rest {
        "json" => Some(Named::Journal(n)),
        "removed" => Some(Named::Tombstone(n)),
        _ => {
            let h = rest.strip_prefix("json.")?.strip_suffix(".tmp")?;
            (h.len() == 32 && h.bytes().all(|b| b.is_ascii_hexdigit())).then_some(Named::Temp)
        }
    }
}
/// Call 15: a skipped name counts only as a regular file; one that vanished since the listing is gone (call 14).
/// Call 27: also every final name before a write: Ok(true) a regular file, Ok(false) absent, else `evidence`.
pub(crate) fn kept(path: &Path) -> Res<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_file() => Ok(true),
        Ok(_) => Err(evidence::unreadable()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(evidence::unreadable()),
    }
}
/// The walk from the state root: Ok(false) at the first absent level, `evidence` at a link or a file.
fn present(ctx: &Ctx, scope: &Scope) -> Res<bool> {
    let (key, teams, dir) = levels(ctx, scope)?;
    Ok(level(&key)? && level(&teams)? && level(&dir)?)
}
/// AD-R4: the names in `.adopt`, walked from the state root; anything unnamed is `evidence`.
pub fn listing(ctx: &Ctx, scope: &Scope) -> Res<Listing> {
    let mut out = Listing::default();
    if !present(ctx, scope)? {
        return Ok(out);
    }
    let dir = dir_at(ctx, scope);
    for e in std::fs::read_dir(&dir).map_err(|_| evidence::unreadable())? {
        let name = (e.map_err(|_| evidence::unreadable())?.file_name())
            .to_string_lossy()
            .into_owned();
        match named(&name).ok_or_else(evidence::unreadable)? {
            Named::Journal(n) => out.journals.push(n),
            Named::Tombstone(n) if kept(&dir.join(&name))? => out.tombstones.push(n),
            Named::Tombstone(_) => {}
            Named::Temp => drop(kept(&dir.join(&name))?),
        }
    }
    out.journals.sort_unstable();
    out.tombstones.sort_unstable();
    Ok(out)
}
/// One journal, by its name's seq, read strictly through the walk (`evidence::read` opens regular files only).
pub fn read_journal(ctx: &Ctx, scope: &Scope, seq: u64) -> Res<Journal> {
    if !present(ctx, scope)? {
        return Err(evidence::unreadable());
    }
    let path = dir_at(ctx, scope).join(format!("{seq}.json"));
    let v = evidence::read(&path)
        .known()?
        .ok_or_else(evidence::unreadable)?;
    Journal::parse(seq, &v).ok_or_else(evidence::unreadable)
}
/// The committed seq: absent 0; anything but exactly `{seq: <u64>}` in a regular file is `evidence`.
pub fn committed(ctx: &Ctx, scope: &Scope) -> Res<u64> {
    let (key, teams, _) = levels(ctx, scope)?;
    if !(level(&key)? && level(&teams)?) {
        return Ok(0);
    }
    match evidence::read(&seq_path(ctx, scope)).known()? {
        None => Ok(0),
        Some(v) => (v.as_object().filter(|o| o.len() == 1))
            .and_then(|o| o.get("seq"))
            .and_then(Value::as_u64)
            .ok_or_else(evidence::unreadable),
    }
}
/// A4.2, AD-R5: create-once; every flush on every call. An identical journal already there is this one (a retry):
/// its own content is flushed again (call 26); another body of the seq refuses `topology`. A folder or a link at
/// the name is `evidence` (call 27).
pub fn publish(tx: &Tx<'_>, j: &Journal) -> Res<()> {
    let (key, teams, dir) = levels(tx.ctx, &tx.scope)?;
    if !level(&key)? {
        return Err(evidence::unreadable()); // `state_dir` makes the server folder; adoption never does
    }
    made(&teams)?;
    flush(&key)?;
    made(&dir)?;
    flush(&teams)?;
    let path = dir.join(format!("{}.json", j.seq));
    // A folder or a link at the name is `Exists` here, then `evidence` from `read_journal` (call 27).
    let fresh = evidence::publish(&path, &j.body()).map_err(|_| not_written())?
        == evidence::Published::Published;
    if !fresh {
        if read_journal(tx.ctx, &tx.scope, j.seq)? != *j {
            return Err(refusal("topology", OUT_OF_ORDER));
        }
        sync_file(&path).map_err(|_| not_written())?;
    }
    flush(&dir)
}
/// A4.3: one change, absolute (AD-R3); a record gone or not lifecycle is skipped (`record_write` answers None).
pub fn apply(tx: &Tx<'_>, c: &Change) -> Res<()> {
    tx.record_write(&c.id, Guard::Life, |r| {
        r.insert("parentAgent".into(), c.parent_agent.clone());
        r.insert("parentPane".into(), c.parent_pane.clone());
        r.insert("depth".into(), json!(c.depth));
        Ok(())
    })?;
    Ok(())
}
/// A4.4, AD-R5: `{seq}` durably (content, then `teams`, by `write_json_durable`); `teams` is made and `<key>` flushed
/// on every call. What is there must read as a seq first (call 27): torn content, a folder or a link refuses.
pub fn commit(tx: &Tx<'_>, seq: u64) -> Res<()> {
    let (key, teams, _) = levels(tx.ctx, &tx.scope)?;
    if !level(&key)? {
        return Err(evidence::unreadable());
    }
    made(&teams)?;
    flush(&key)?;
    committed(tx.ctx, &tx.scope)?;
    write_json_durable(&seq_path(tx.ctx, &tx.scope), &json!({"seq": seq}))
        .map_err(|_| not_written())
}
/// A4.5, AD-R5: rename to the tombstone, flush, delete it, flush; each also when already done, so a retry repeats
/// every flush. Both names are checked first: a folder or a link at either is `evidence`, never renamed (call 27).
pub fn remove(tx: &Tx<'_>, seq: u64) -> Res<()> {
    if !present(tx.ctx, &tx.scope)? {
        return Ok(());
    }
    let dir = dir_at(tx.ctx, &tx.scope);
    let (file, stone) = (
        dir.join(format!("{seq}.json")),
        dir.join(format!("{seq}.removed")),
    );
    let (journal, tombstone) = (kept(&file)?, kept(&stone)?);
    if journal {
        std::fs::rename(&file, &stone).map_err(|_| not_written())?;
    }
    flush(&dir)?;
    if journal || tombstone {
        std::fs::remove_file(&stone).map_err(|_| not_written())?;
    }
    // Call 26 (checklist 3): until the delete is flushed the tombstone stands, so replay repeats this flush.
    flush(&dir).inspect_err(|_| {
        let _ = std::fs::write(&stone, b"");
    })
}

/// Calls 6, 14, 26: finish an unfinished publication or seq flush (`<key>`, `teams`, the seq file's content,
/// `.adopt`) before anything is classified, applied or removed. A flush that still fails stops replay.
fn settle(tx: &Tx<'_>) -> Res<()> {
    let (key, teams, dir) = levels(tx.ctx, &tx.scope)?;
    flush(&key)?;
    flush(&teams)?;
    let seq = seq_path(tx.ctx, &tx.scope);
    if kept(&seq)? {
        sync_file(&seq).map_err(|_| not_written())?;
    }
    flush(&dir)
}
/// A4 replay: the first `team_state` hook; also `replay_held` (Task 8) and `adopt_from`. Names first (call 7):
/// stale journals and tombstones are removed unparsed; the one journal at seq + 1 has its content flushed (call
/// 26), is read strictly and finished from step 3; anything else refuses before a file is touched (ambiguity 10).
pub fn replay(tx: &Tx<'_>) -> Res<()> {
    let l = listing(tx.ctx, &tx.scope)?;
    if l.journals.is_empty() && l.tombstones.is_empty() {
        return Ok(());
    }
    settle(tx)?;
    let s = committed(tx.ctx, &tx.scope)?;
    let live: Vec<u64> = l.journals.iter().copied().filter(|n| *n > s).collect();
    // Call 28 (checklist 7): a seq with no successor refuses before any stale name is removed.
    let next = s.checked_add(1).ok_or_else(overflow)?;
    // Two live journals always include one that is not seq + 1 (the `two` case).
    if live.iter().any(|n| *n != next) || l.tombstones.iter().any(|n| *n > s) {
        return Err(refusal("topology", OUT_OF_ORDER));
    }
    let stale: BTreeSet<u64> = (l.journals.iter().chain(&l.tombstones))
        .copied()
        .filter(|n| *n <= s)
        .collect();
    for n in stale {
        remove(tx, n)?;
        super::release::halt("adopt-stale")?;
    }
    let Some(n) = live.first() else {
        return Ok(());
    };
    let path = dir_at(tx.ctx, &tx.scope).join(format!("{n}.json"));
    if kept(&path)? {
        sync_file(&path).map_err(|_| not_written())?;
    }
    finish(tx, &read_journal(tx.ctx, &tx.scope, *n)?)
}
/// A4.3-5: each change, the seq, the removal; a halt point after each (AD-R6).
fn finish(tx: &Tx<'_>, j: &Journal) -> Res<()> {
    for c in &j.changes {
        apply(tx, c)?;
        super::release::halt(&format!("adopt-change-{}", c.id))?;
    }
    commit(tx, j.seq)?;
    super::release::halt("adopt-seq")?;
    remove(tx, j.seq)?;
    super::release::halt("adopt-removed")
}

/// AD-R4, call 7: a seq or depth that would overflow (tests assert this, never io text).
pub const OVERFLOW: &str = "an adoption seq or depth is out of range; nothing was changed";
fn overflow() -> Refusal {
    refusal("topology", OVERFLOW)
}
/// What one source's pass did (call 9: a blocked source says why; prelife-1c call 2).
#[derive(Debug, Clone, PartialEq)]
pub enum Adopted {
    Moved(String),
    Nothing,
    Blocked(&'static str),
}
/// A4 for one source E: replay, then revalidate E (A2), find what moves (nothing: done, ambiguity 9), choose A (A3),
/// plan, journal, apply, commit, remove. Every number is checked before the journal is published.
pub fn adopt_from(tx: &Tx<'_>, from: &str) -> Res<Adopted> {
    replay(tx)?;
    let v = View::read(tx)?;
    if !v.source(from)? {
        return Ok(Adopted::Nothing);
    }
    let moving = v.moving(from)?;
    if moving.is_empty() {
        return Ok(Adopted::Nothing);
    }
    let to = match v.adopter(from)? {
        Up::To(to) => to,
        Up::Blocked(why) => return Ok(Adopted::Blocked(why)),
    };
    let changes = v.plan(tx, &moving, &to)?;
    let seq = (committed(tx.ctx, &tx.scope)?.checked_add(1)).ok_or_else(overflow)?;
    let j = Journal {
        seq,
        from: from.into(),
        to: to.clone(),
        changes,
    };
    publish(tx, &j)?;
    super::release::halt("adopt-journal")?;
    finish(tx, &j)?;
    Ok(Adopted::Moved(to))
}
/// A8: opens a Tx first.
pub fn adopt(ctx: &Ctx, scope: &Scope, from: &str) -> Res<Adopted> {
    super::team_state::open(ctx, scope, |tx| adopt_from(tx, from))
}
/// Call 13: for a reader that holds the team lock (the spawn lock) and decides from what it reads next: finish an
/// outstanding adoption first (hook-free: the lock is held).
pub fn replay_held(ctx: &Ctx, caller: &crate::caller::Caller) -> Res<()> {
    replay(&Tx {
        ctx,
        scope: Scope::of(caller),
    })
}

mod view;
pub use view::{LEAVES, SEVERAL, Up, View};

#[path = "adopt_bar.rs"]
mod bar;
pub use bar::{MOVING, Swept, adopt_locked, candidates, stable, sweep, tick};

#[path = "adopt_route.rs"]
mod routes;
pub use routes::{Route, notice_key, notice_route, pane_key, route};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod replay_tests;

#[cfg(test)]
mod from_tests;

#[cfg(test)]
mod order_tests;

#[cfg(test)]
mod bar_tests;

#[cfg(test)]
mod route_tests;

#[cfg(test)]
mod crash;
