//! prelife-1c G10: a member folder whose `record.json` lacks `lifecycle: 1` was made by 0.2.x or a v0.3 dev build.
//! It keeps today's snapshot, `list` and validated `stop`; no lifecycle path reads past its record or writes it.
use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{Map, Value};

use crate::agents::core_identity::Identity;
use crate::agents::gens;
use crate::agents::membership::{Local, Stored};
use crate::caller::Caller;
use crate::error::{Refusal, Res};
use crate::state::evidence::Evidence;

/// Ambiguity 2: the folder notice kinds that prove a pre-lifecycle member Ended (0.2.x writes `exited` only from a
/// recorded launcher exit code). `lost` proves nothing.
pub(crate) const ENDED_KINDS: [&str; 2] = ["stopped", "exited"];
pub(crate) const NOT_RUNNING: &str = "is not running where hc can show it: its terminal cannot be read or matched, so nothing is killed or noticed; retry once hc answers, or close its shell with hc or its pane";
pub(crate) const PRE_LIFECYCLE: &str =
    "predates lifecycle records (pre-lifecycle) and is never resumed";
pub(crate) const LIFECYCLE_ONLY: &str =
    "this member predates lifecycle records; lifecycle state is never read or written for it";
/// Ambiguity 14: what frees a held pre-lifecycle member, named in the `limit` refusal (`{name}` replaced).
pub(crate) const HELD_LOST: &str = "run mux-agent stop {name} once its client is gone";
pub(crate) const HELD_RUNNING: &str = "running; mux-agent stop {name} stops it";
pub(crate) const HELD_UNKNOWN: &str = "hc cannot show its terminal; it stays held";
pub(crate) const HELD_UNREADABLE: &str = "its record names no usable hc terminal; it stays held";

fn refusal(check: &'static str, message: String) -> Refusal {
    Refusal {
        code: 2,
        check,
        message,
        reason: None,
        leftover: None,
        details: Default::default(),
    }
}

/// G10c, p1c-calls 4: hc cannot show the legacy member's terminal (Unknown); nothing is killed or noticed.
pub(crate) fn not_running(name: &str) -> Refusal {
    refusal("state", format!("{name} {NOT_RUNNING}"))
}

/// G10d: a pre-lifecycle member never resumes.
pub(crate) fn pre_lifecycle(name: &str) -> Refusal {
    refusal("state", format!("{name} {PRE_LIFECYCLE}"))
}

/// G10d, calls 16, 20: refuses `resume <target>` of a pre-lifecycle member from `record.json` alone, before any hc
/// call or snapshot. An exact id decides first, over every member: a pre-lifecycle folder of the caller's session
/// with that id is refused, a lifecycle one passes. Else by name: refused when a same-session pre-lifecycle record
/// has the name and no lifecycle record does (a lifecycle twin goes on to `admit`, which checks the subject it
/// selects). Another session's records, absent and unreadable folders are left to the snapshot.
pub(crate) fn refuse_resume(ctx: &crate::ctx::Ctx, caller: &Caller, target: &str) -> Res<()> {
    use crate::agents::gens::Member;
    let agents = crate::agents::server_dir(ctx, caller).join("agents");
    let mut mine = Vec::new();
    for (id, _, member) in gens::members(&agents)? {
        let (record, life) = match member {
            Member::Pre(r) => (r, false),
            Member::Life(g) => (g.record().clone(), true),
            Member::Absent | Member::Unreadable(_) => continue,
        };
        let other = record.get("session").and_then(Value::as_str);
        if other.is_some_and(|s| s != super::scope::state(caller).session) {
            continue;
        }
        mine.push((id, record, life));
    }
    let name = |r: &Value| r.get("name").and_then(Value::as_str).map(str::to_owned);
    if let Some((id, record, life)) = mine.iter().find(|(id, _, _)| id == target) {
        return match life {
            true => Ok(()),
            false => Err(pre_lifecycle(&name(record).unwrap_or_else(|| id.clone()))),
        };
    }
    let named: Vec<bool> = mine
        .iter()
        .filter(|(_, r, _)| name(r).as_deref() == Some(target))
        .map(|(_, _, life)| *life)
        .collect();
    match named.is_empty() || named.contains(&true) {
        true => Ok(()),
        false => Err(pre_lifecycle(target)),
    }
}

/// G10a: the folder notice, read through the walk from the state root, is whole and its kind is in `ENDED_KINDS`.
/// Absent, torn, a folder, a link anywhere on the walk, another kind: false (it holds its slot).
pub(crate) fn ended_by_notice(folder: &Path) -> bool {
    match gens::walk(folder, &["notice.json"]) {
        Evidence::Present(n) => n
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|k| ENDED_KINDS.contains(&k)),
        _ => false,
    }
}

/// Ambiguity 11: the backstop of a lifecycle-only writer, run on the record it already read.
pub(crate) fn lifecycle_only(record: &Value) -> Res<()> {
    if gens::lifecycle(record) {
        return Ok(());
    }
    Err(refusal("prelife", LIFECYCLE_ONLY.into()))
}

/// What a pre-lifecycle folder is to membership (`Local` boxed: clippy `large_enum_variant`).
pub(crate) enum Slot {
    Local(Box<Local>),
    Held,
    Free,
}

/// G10a (ambiguities 2, 5; calls 14, 15, 21, 22): a readable identity is today's `Local`; anything else holds one
/// slot unless its folder notice proves an end or recorded evidence proves it never ran. The caller has skipped
/// another session's record already. Every read is walk-checked and regular-file-only.
pub(crate) fn slot(folder: &Path, record: &Value, id: &str, caller: &Caller) -> Slot {
    if let Some(l) = record
        .is_object()
        .then(|| local(folder, record, id, caller))
        .flatten()
    {
        return Slot::Local(Box::new(l));
    }
    if ended_by_notice(folder) || never_ran(folder, record) {
        return Slot::Free;
    }
    Slot::Held
}

/// Main's reading of a pre-lifecycle record (moved from membership): its identity, the folder commit and notice.
/// None when the record names no usable identity (G10a: held or free, never `invalid local hc identity`).
fn local(folder: &Path, record: &Value, id: &str, caller: &Caller) -> Option<Local> {
    let team = crate::agents::membership::team(caller);
    let hc = ["hcAddress", "generation", "team", "launcherArgv"]
        .iter()
        .any(|k| record.get(k).is_some());
    let identity = if hc {
        Identity::read(record, id, &team)?
    } else {
        // A resume's successor whose hc run reply was lost after its launcher claimed the run: a member whose life
        // comes from hc. Without a claim (or with an abandoned one) it never ran and is no member. Call 14: the
        // claim is read through the walk; an Unreadable one (FIFO, link, torn) counts as claimed.
        let claimed = match gens::claim(folder, 0) {
            Evidence::Present(c) => c.as_str() != Some("abandoned"),
            Evidence::Absent => false,
            Evidence::Unreadable => true,
        };
        Identity::pending(record, id, &team).filter(|_| claimed)?
    };
    Some(Local {
        identity,
        stored: Stored {
            record: record.clone(),
            committed: committed(folder),
            noticed: folder.join("notice.json").exists(),
            notice: None,
            active_done: None,
        },
        held: false,
        finished: None,
    })
}

/// Call 22: main's `folder_committed` through the walk: a whole regular `commit.json` that is no launcher's give-up.
fn committed(folder: &Path) -> bool {
    matches!(gens::walk(folder, &["commit.json"]),
        Evidence::Present(c) if c.get("aborted") != Some(&Value::Bool(true)))
}

/// Calls 15, 21: recorded never-ran evidence. The claim of the generation the record names (absent: 0; present but
/// not a canonical string: nothing is proven) is `"abandoned"`; or no `commit.json` and an `abort.json` in a
/// writer's exact shape (`writer_abort`). Every read is walk-checked: a FIFO, link, folder or torn file proves nothing.
pub(crate) fn never_ran(folder: &Path, record: &Value) -> bool {
    let n = match record.get("generation") {
        None => 0,
        Some(g) => match g.as_str().and_then(gens::plain_gen) {
            Some(n) => n,
            None => return false,
        },
    };
    if matches!(gens::claim(folder, n), Evidence::Present(c) if c == "abandoned") {
        return true;
    }
    if !matches!(gens::walk(folder, &["commit.json"]), Evidence::Absent) {
        return false;
    }
    match gens::walk(folder, &["abort.json"]) {
        Evidence::Present(Value::Object(a)) => writer_abort(&a, folder, record),
        _ => false,
    }
}

/// Call 21: the two abort shapes a writer makes, with their field types. v0.2.x: `{abortedAt: string, pane: string
/// or null}`. v0.3-dev's rollback: `{hcAddress, identity, kill, pane, viewCleanup}` naming this record's
/// `hcAddress` and this folder's id, `kill` `"acknowledged"` (a pending or uncertain kill may have left it running).
fn writer_abort(a: &Map<String, Value>, folder: &Path, record: &Value) -> bool {
    let keys: BTreeSet<&str> = a.keys().map(String::as_str).collect();
    let text = |k: &str| a.get(k).and_then(Value::as_str);
    let pane = a.get("pane").is_some_and(|p| p.is_null() || p.is_string());
    if keys == BTreeSet::from(["abortedAt", "pane"]) {
        return pane && text("abortedAt").is_some();
    }
    let id = folder.file_name().and_then(|n| n.to_str());
    let identity = a
        .get("identity")
        .and_then(|i| i.get("id"))
        .and_then(Value::as_str);
    keys == BTreeSet::from(["hcAddress", "identity", "kill", "pane", "viewCleanup"])
        && pane
        && text("kill") == Some("acknowledged")
        && a.get("viewCleanup")
            .is_some_and(|v| v.is_string() || v.is_object())
        && text("hcAddress")
            .is_some_and(|h| record.get("hcAddress").and_then(Value::as_str) == Some(h))
        && identity.is_some_and(|i| Some(i) == id)
}

/// All-calls 14: a 0.2.x pane-based member (its record names no hc or lifecycle field). `stop` ends it by its pane.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PaneMember {
    pub id: String,
    pub name: String,
    pub pane: String,
    pub command: String,
}

const HC_KEYS: [&str; 5] = [
    "hcAddress",
    "generation",
    "team",
    "launcherArgv",
    "lifecycle",
];
const PANE_UNREADABLE: &str = "is a 0.2.x pane and the multiplexer cannot list its panes, so nothing is closed or noticed; retry once it answers";
const PANE_UNPROVEN: &str = "is a 0.2.x pane that cannot be proven to run its recorded launch, so nothing is closed or noticed; close its pane yourself, then stop it again";

/// `record` at folder `id` as a pane-based member of the caller's session: no hc field, its name, pane and exact
/// `startCommand`. A 0.2.0-0.2.2 record (no `startCommand`) is none: nothing proves its pane.
fn pane_of(record: &Value, id: &str, caller: &Caller) -> Option<PaneMember> {
    let s = |k: &str| record.get(k)?.as_str().filter(|v| !v.is_empty());
    if HC_KEYS.iter().any(|k| record.get(k).is_some())
        || s("id")? != id
        || s("session")? != super::scope::state(caller).session
    {
        return None;
    }
    Some(PaneMember {
        id: id.into(),
        name: s("name")?.into(),
        pane: s("pane")?.into(),
        command: s("startCommand")?.into(),
    })
}

/// All-calls 14: the one committed pane-based member of the caller's session named `name` that no `stopped` or
/// `exited` notice ended; none when there is none or more than one.
pub(crate) fn pane_named(
    ctx: &crate::ctx::Ctx,
    caller: &Caller,
    name: &str,
) -> Res<Option<PaneMember>> {
    if !crate::agents::agent_name(name) {
        return Ok(None);
    }
    let mut found: Vec<_> = (pane_members(ctx, caller)?.into_iter())
        .map(|(p, _)| p)
        .filter(|p| p.name == name)
        .collect();
    Ok(if found.len() == 1 { found.pop() } else { None })
}

/// Every committed pane-based member of the caller's session that no `stopped` or `exited` notice ended, with its record.
fn pane_members(ctx: &crate::ctx::Ctx, caller: &Caller) -> Res<Vec<(PaneMember, Value)>> {
    let agents = crate::agents::server_dir(ctx, caller).join("agents");
    let mut found = Vec::new();
    for (id, folder, member) in gens::members(&agents)? {
        let gens::Member::Pre(r) = member else {
            continue;
        };
        if let Some(p) = pane_of(&r, &id, caller)
            && committed(&folder)
            && !ended_by_notice(&folder)
        {
            found.push((p, r));
        }
    }
    Ok(found)
}

/// The pane-based members `list` and the bar show as running, as 0.2.8 did, so a person still sees a 0.2.x agent
/// running after the upgrade (it holds a team slot): each whose pane is proven its launch (`proven`), with its record
/// and the window it is in now. Display only: a multiplexer that cannot be read, or
/// a pane not proven, shows none, and nothing is killed or written.
pub(crate) fn live_panes(
    ctx: &crate::ctx::Ctx,
    caller: &Caller,
) -> Vec<(PaneMember, Value, String)> {
    let Ok(members) = pane_members(ctx, caller) else {
        return Vec::new();
    };
    if members.is_empty() {
        return Vec::new(); // no multiplexer call without a pane-based member
    }
    let Ok(rows) = crate::agents::registry::rows(ctx, caller) else {
        return Vec::new();
    };
    members
        .into_iter()
        .filter_map(|(p, r)| {
            let (row, _) = proven(ctx, caller, &p, &rows).ok()??;
            let window = row.window.clone();
            Some((p, r, window))
        })
        .collect()
}

/// `p`'s record, read again under its locks: still that pane-based member, committed; else `target` refuses.
pub(crate) fn pane_current(ctx: &crate::ctx::Ctx, caller: &Caller, p: &PaneMember) -> Res<Value> {
    let folder = crate::agents::server_dir(ctx, caller)
        .join("agents")
        .join(&p.id);
    match gens::member_of(&folder) {
        gens::Member::Pre(r)
            if pane_of(&r, &p.id, caller).as_ref() == Some(p) && committed(&folder) =>
        {
            Ok(r)
        }
        gens::Member::Unreadable(e) => Err(e),
        _ => Err(crate::agents::member_target::changed()),
    }
}

/// All-calls 14: closes `p`'s pane when it is live and proven its launch: the row at its pane runs exactly its
/// `startCommand` with launcher.json's pid, which the process table lists with launcher.json's start. The kill is the
/// server's conditional one on that command and pid (never by name), and a fresh list must no longer show the launch.
/// True: closed; false: no pane at its pane, nothing killed. A multiplexer that cannot be read, or a pane there that is
/// not proven, refuses 2 `state`: nothing is killed or noticed.
pub(crate) fn close_pane(ctx: &crate::ctx::Ctx, caller: &Caller, p: &PaneMember) -> Res<bool> {
    use crate::agents::registry::{self, Launch};
    let unreadable = |_: Refusal| refusal("state", format!("{} {PANE_UNREADABLE}", p.name));
    let unproven = || refusal("state", format!("{} {PANE_UNPROVEN}", p.name));
    let rows = registry::rows(ctx, caller).map_err(unreadable)?;
    let Some((row, pid)) = proven(ctx, caller, p, &rows).map_err(|()| unproven())? else {
        return Ok(false);
    };
    // The kill is bound to the very pid the proof checked, never read again: a pane respawned in between is spared.
    let launch = Launch {
        command: p.command.clone(),
        pid: Some(pid),
    };
    let argv = registry::bound(&p.pane, launch.live(), format!("kill-pane -t {}", p.pane));
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    crate::mux::native(ctx, &argv, false).map_err(unreadable)?;
    let after = registry::rows(ctx, caller).map_err(unreadable)?;
    if after
        .iter()
        .any(|r| r.pane == p.pane && r.command == p.command && r.pid == row.pid)
    {
        return Err(unproven());
    }
    Ok(true)
}

/// The row at `p`'s pane in `rows`, when it is live and proven `p`'s launch: it runs exactly its `startCommand` with
/// launcher.json's pid, which the process table lists with launcher.json's start; with that pid. None: no pane there.
/// Err: a pane there that is not proven.
fn proven<'a>(
    ctx: &crate::ctx::Ctx,
    caller: &Caller,
    p: &PaneMember,
    rows: &'a [crate::agents::registry::Row],
) -> Result<Option<(&'a crate::agents::registry::Row, u32)>, ()> {
    let Some(row) = rows.iter().find(|r| r.pane == p.pane) else {
        return Ok(None);
    };
    match launcher_pid(ctx, caller, &p.id) {
        Some(pid) if !row.dead && row.command == p.command && row.pid == pid.to_string() => {
            Ok(Some((row, pid)))
        }
        _ => Err(()),
    }
}

/// launcher.json's pid (walk-checked), when the process table lists it with launcher.json's start.
fn launcher_pid(ctx: &crate::ctx::Ctx, caller: &Caller, id: &str) -> Option<u32> {
    let folder = crate::agents::server_dir(ctx, caller)
        .join("agents")
        .join(id);
    let Evidence::Present(l) = gens::walk(&folder, &["launcher.json"]) else {
        return None;
    };
    let pid = u32::try_from(l.get("pid")?.as_u64()?).ok()?;
    let start = l.get("start")?.as_i64()?;
    ctx.os
        .row(pid)
        .and_then(|r| r.start)
        .is_some_and(|listed| ctx.os.same_start(listed, start))
        .then_some(pid)
}

#[cfg(test)]
mod tests;
