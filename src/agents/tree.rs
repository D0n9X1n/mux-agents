//! A team's tree (#103 phase 3): whether the caller is in a team, where it stands (D1), where its child goes (D2-D6),
//! and whom it may stop, read or resume.
use super::lifecycle::Life;
use super::membership::Snapshot;
use super::team::{self, Team};
use super::{server_dir, text};
use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::error::{Res, refuse};
use crate::hc::Terminal;
use crate::parse::Client;
use crate::proc::Row;
use crate::state::root::OsKind;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
pub const MAX_DEPTH: u64 = 3;
pub const MAX_MEMBERS: usize = 6;
/// Outside a team 0.2 holds; a launched leader without a team record refuses every spawn (D2).
pub enum Scope {
    Outside,
    Leaderless,
    Team(Team),
}
pub fn scope(ctx: &Ctx, caller: &Caller) -> Res<Scope> {
    let server = server_dir(ctx, caller);
    if let Some(found) = team::read(&team::path(&server, &caller.session))? {
        return Ok(Scope::Team(found));
    }
    let entries = match std::fs::read_dir(server.join("agents")) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Scope::Outside),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let folder = entry?.path();
        let Some(record) = crate::state::fs::read_json(&folder.join("record.json"))? else {
            continue;
        };
        if is_leader(&record)
            && text(record.get("session")) == caller.session
            && !super::up::discover::ran_nothing(&folder)
        {
            return Ok(Scope::Leaderless);
        }
    }
    Ok(Scope::Outside)
}
/// The leader `up` launched: depth 0 with `upTeam`.
pub fn is_leader(record: &Value) -> bool {
    record.get("depth").and_then(Value::as_u64) == Some(0) && record.get("upTeam").is_some()
}
/// D3: leader 0, a child at its parent's depth + 1, at most 3.
pub fn check_depth(child: u64) -> Res<()> {
    if child > MAX_DEPTH {
        return refuse(
            2,
            "depth",
            format!("a child at depth {child} exceeds the team's depth limit of 3"),
        );
    }
    Ok(())
}
/// D4: live and pending members besides the leader, read under the team lock.
pub fn limit(snapshot: &Snapshot) -> Res<()> {
    let leaders = snapshot
        .facts
        .local
        .iter()
        .filter(|o| o.life != Life::Ended)
        .filter(|o| {
            snapshot
                .records
                .get(&o.identity.id)
                .is_some_and(|s| is_leader(&s.record))
        })
        .count();
    if snapshot.facts.count() - leaders >= MAX_MEMBERS {
        return refuse(
            2,
            "limit",
            "the team already has 6 members besides the leader",
        );
    }
    Ok(())
}
/// Every local record by id.
pub fn records_of(snapshot: &Snapshot) -> BTreeMap<String, Value> {
    snapshot
        .records
        .iter()
        .map(|(id, s)| (id.clone(), s.record.clone()))
        .collect()
}
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub id: String,
    pub record: Value,
    pub client: Client,
    pub pid: u32,
    pub start: i64,
    pub flags: Vec<String>,
}
/// D1, the one proof: caller > shells only > X, every row readable and no newer than its child. X is the C that member
/// `id`'s launcher.json recorded, or (Unix only) the native copilot whose parent is that C, the npm loader; C's parent is
/// the recorded L; both pids and starts match; `t`, M's own terminal by address and labels, shows X, C or L in front.
pub fn prove_member(ctx: &Ctx, state: &Path, team: &str, id: &str, t: &Terminal) -> Option<Member> {
    let folder = state.join("agents").join(id);
    let read = |f: &str| crate::state::fs::read_json(&folder.join(f)).ok().flatten();
    let (record, launcher) = (read("record.json")?, read("launcher.json")?);
    let pid_start = |v: &Value| {
        Some((
            u32::try_from(v.get("pid")?.as_u64()?).ok()?,
            v.get("start")?.as_i64()?,
        ))
    };
    let ((l_pid, l_start), (c_pid, c_start)) =
        (pid_start(&launcher)?, pid_start(launcher.get("client")?)?);
    let identity = super::core_identity::Identity::read(&record, id, team)?;
    if !identity.matches(t) {
        return None;
    }
    let parent_of = |row: &Row| {
        ctx.os
            .row(row.ppid)
            .filter(|p| p.start.is_some() && p.start <= row.start)
    };
    let mut row = ctx.os.row(ctx.pid).filter(|r| r.start.is_some())?;
    let mut found = None;
    for _ in 0..64 {
        let parent = parent_of(&row)?;
        if let Some(client) = ctx.os.client_of(&parent) {
            found = Some((parent, client));
            break;
        }
        if !ctx.os.is_shell(&parent) {
            return None;
        }
        row = parent;
    }
    let (x, x_client) = found?;
    let named = super::spawn::client_named(record.get("client")?.as_str()?)?;
    let c = if x.pid == c_pid {
        x.clone()
    } else {
        let loader = parent_of(&x).filter(|p| p.pid == c_pid)?;
        (named == Client::Copilot
            && copilot_pair(&loader, &x, crate::state::root::HOST, |r| {
                ctx.os.client_of(r) == Some(Client::Copilot)
            }))
        .then_some(loader)?
    };
    let l = parent_of(&c)?;
    let same = |pid: u32, start: i64, row: &Row| {
        ctx.os
            .open(pid)
            .is_some_and(|o| ctx.os.same_start(o.start, start))
            && row.start.is_some_and(|s| ctx.os.same_start(s, start))
    };
    let front = t
        .foreground
        .as_ref()
        .and_then(|f| u32::try_from(f.pid).ok());
    let proven = l.pid == l_pid
        && x_client == named
        && same(c_pid, c_start, &c)
        && same(l_pid, l_start, &l)
        && front.is_some_and(|f| f == x.pid || f == c_pid || f == l_pid);
    if !proven {
        return None;
    }
    let flags = super::resume::recorded_flags(&folder).ok()?;
    Some(Member {
        id: id.into(),
        record,
        client: named,
        pid: c_pid,
        start: c_start,
        flags,
    })
}
/// The one pair that counts as C (D1), Unix only: `native` (an executable named `copilot`) is the direct child of
/// `loader` (an executable named `node`), and `is_copilot(loader)` is the canonical Unix client recognition
/// (`Os::client_of` -> `parse::unix_client`: links resolved, relative scripts against the cwd), never the raw spelling.
pub fn copilot_pair(
    loader: &Row,
    native: &Row,
    os: OsKind,
    is_copilot: impl Fn(&Row) -> bool,
) -> bool {
    let file = |r: &Row| {
        r.path
            .as_deref()
            .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
    };
    os == OsKind::Unix
        && native.ppid == loader.pid
        && file(loader).as_deref() == Some("node")
        && file(native).as_deref() == Some("copilot")
        && is_copilot(loader)
}
#[cfg(test)]
#[path = "tree_tests.rs"]
pub(crate) mod tests;
