//! A team's tree (#103 phase 3): whether the caller is in a team, where it stands (D1), where its child goes (D2-D6),
//! and whom it may stop, read or resume.
use super::lifecycle::Life;
use super::membership::Snapshot;
use super::team::{self, Team};
use super::{server_dir, text};
use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::error::{Res, refuse};
use serde_json::Value;
use std::collections::BTreeMap;
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
#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;
