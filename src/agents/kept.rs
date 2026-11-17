//! views-6f V4-V6: a kept shell is only its hc labels (`mux.kept=<T>`, `mux.keptName=<name>`, group B R1.3). This
//! module lists and closes them; it writes no file (6f-R4).
use serde_json::{Map, Value, json};

use crate::cli::Request;
use crate::ctx::Ctx;
use crate::error::{Res, refuse};
use crate::hc::{Core, State, Terminal};

pub const KEPT: &str = "mux.kept";
pub const KEPT_NAME: &str = "mux.keptName";

/// One row: a terminal hc shows live with `mux.kept=<T>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shell {
    pub address: String,
    pub name: String,
    /// The `mux.kept` label it was listed by (restore-6c Task 11).
    pub team: String,
}

fn ours(t: &Terminal, team: &str) -> bool {
    t.state == State::Live && t.labels.get(KEPT).map(String::as_str) == Some(team)
}

/// Ambiguity 11, v6f calls 3, 7: one live `hc ls --label mux.kept=<T>` (hc filters the label; finished shells are
/// not listed), then one `get` per row for its name: hc 0.1.21's `ls` rows carry no labels and no name, so
/// `mux.keptName` needs the `get`. The `get` also re-checks live and `mux.kept=<T>`; a shell gone or finished in
/// between is skipped; another hc failure fails the list. Sorted by name, then address.
pub fn list(core: &Core<'_>, team: &str) -> Res<Vec<Shell>> {
    let mut shells = Vec::new();
    for address in core.ls_live(&[(KEPT, team)])? {
        let t = match core.get(&address) {
            Err(e) if e.check == "not_found" => continue,
            other => other?,
        };
        if !ours(&t, team) {
            continue;
        }
        let name = t
            .labels
            .get(KEPT_NAME)
            .filter(|n| !n.is_empty())
            .cloned()
            .unwrap_or_else(|| address.clone());
        shells.push(Shell {
            address,
            name,
            team: team.into(),
        });
    }
    shells.sort_by(|a, b| (&a.name, &a.address).cmp(&(&b.name, &b.address)));
    Ok(shells)
}

/// Ambiguity 12, 6f-R3: one re-read immediately before the kill, nothing between them.
pub fn close(core: &Core<'_>, team: &str, address: &str) -> Res<()> {
    let t = match core.get(address) {
        Err(e) if e.check == "not_found" => return refuse(2, "target", "no such kept shell"),
        other => other?,
    };
    if t.state == State::Finished {
        return refuse(2, "target", "the shell has already finished");
    }
    if !ours(&t, team) {
        return refuse(2, "target", "that shell is not a kept shell of this team");
    }
    core.kill(address)
}

/// restore-6c Task 11: every live kept shell of any of `teams`, each row naming the label it was listed by.
pub fn list_all(core: &Core<'_>, teams: &[String]) -> Res<Vec<Shell>> {
    let mut all = Vec::new();
    for t in teams {
        all.extend(list(core, t)?);
    }
    Ok(all)
}

/// `close` with the team whose `mux.kept` label hc shows, when it is one of `teams`; else 2 `target` (V5).
pub fn close_any(core: &Core<'_>, teams: &[String], address: &str) -> Res<()> {
    let t = match core.get(address) {
        Err(e) if e.check == "not_found" => return refuse(2, "target", "no such kept shell"),
        other => other?,
    };
    let label = t.labels.get(KEPT).map(String::as_str).unwrap_or("");
    match teams.iter().find(|x| x.as_str() == label) {
        Some(team) => close(core, team, address),
        None => refuse(2, "target", "that shell is not a kept shell of this team"),
    }
}

/// Ambiguity 14, call 12: the kept labels of the caller's team. A live index anchored at its state scope names T:
/// its own label, then each retired index of T's anchor (`retire::kept_teams`). Otherwise only its own label.
pub fn labels(ctx: &Ctx, caller: &crate::caller::Caller) -> Res<Vec<String>> {
    let scope = super::scope::state(caller);
    let anchored = |ix: &super::up::index::Index| {
        ix.anchor
            .as_ref()
            .is_some_and(|a| a.key == scope.key && a.session == scope.session)
    };
    match super::up::index::every(ctx)?.into_iter().find(anchored) {
        Some(ix) => super::up::retire::kept_teams(ctx, &scope, &ix.team),
        None => Ok(vec![super::membership::team(caller)]),
    }
}

/// The board watch's kept rows: `list_all` over `labels`, on a new connection.
pub fn watched(ctx: &Ctx, caller: &crate::caller::Caller) -> Res<Vec<Shell>> {
    let teams = labels(ctx, caller)?;
    list_all(&crate::hc::connect(ctx)?, &teams)
}

/// `list` on a new connection (the verb and the board's watch).
pub fn list_at(ctx: &Ctx, team: &str) -> Res<Vec<Shell>> {
    list(&crate::hc::connect(ctx)?, team)
}

/// A reply's rows, `[{address, name, team}]` (the verb and the board share it).
pub fn rows(shells: &[Shell]) -> Value {
    json!(
        shells
            .iter()
            .map(|s| json!({"address": s.address, "name": s.name, "team": s.team}))
            .collect::<Vec<_>>()
    )
}

/// `mux-agent kept [-Close <address>]`, a person verb (ambiguity 13): close one, then list what is left.
pub fn kept(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let caller = super::up::person_as(ctx, "manage kept shells")?;
    let teams = labels(ctx, &caller)?;
    let core = crate::hc::connect(ctx)?;
    let mut reply = Map::new();
    if let Some(address) = request.value("Close") {
        close_any(&core, &teams, address)?;
        reply.insert("closed".into(), address.into());
    }
    reply.insert("kept".into(), rows(&list_all(&core, &teams)?));
    Ok(reply)
}
