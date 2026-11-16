//! cascade-3d: a stop records `stopped` for a member's whole sub-tree, closes it, then ends it bottom-up; a journal
//! under `teams/<sha16(session)>.cascade/` lets the next stop, `up` or bar tick finish a cascade cut short.
use std::path::Path;

use serde_json::{Value, json};

use super::gens;
use super::registry::{self, Notice};
use super::team_state::{Scope, Tx};
use crate::caller::Caller;
use crate::ctx::Ctx;
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
    // Integration (#150): the notice is posted from the presented record, as `stop_life` and `list` read the member:
    // its pane is the one view.json presents, the `<from>|<pane>` the board counts the row's unread under. An
    // unreadable member or view refuses, as the raw read did.
    let r = match gens::member_of(&folder) {
        gens::Member::Life(g) => super::team_state::presented(&g, &g.status()?)?,
        gens::Member::Unreadable(e) => return Err(e),
        _ => return Err(no_record()),
    };
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
/// not refuse: that is the reopen, a stopped root's included (c3d call 35: call 11 closes a stopped root to new
/// members, which its parent walk refuses here and `admit` refuses on spawn).
pub fn reopen_check(tx: &Tx<'_>, id: &str) -> Res<()> {
    let t = Tree::read(tx)?;
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

#[path = "cascade_tree.rs"]
mod tree;
pub use tree::{Shown, Tree};

#[path = "cascade_journal.rs"]
mod journal;
use journal::tombstoned;
pub use journal::{
    Entry, Journal, NOT_WRITTEN, dir_at, journals, mark_prepared, publish, remove, waiting,
};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod crash;
