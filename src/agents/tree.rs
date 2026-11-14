//! A team's tree (#103 phase 3): whether the caller is in a team, where it stands (D1), where its child goes (D2-D6),
//! and whom it may stop, read or resume.
use super::lifecycle::Life;
use super::membership::Snapshot;
use super::team::{self, Team};
use super::{server_dir, text};
use crate::caller::Caller;
use crate::caller::Kind;
use crate::ctx::Ctx;
use crate::error::Refusal;
use crate::error::{Res, refuse};
use crate::hc::Core;
use crate::hc::Terminal;
use crate::parse::Client;
use crate::proc::Row;
use crate::state::root::OsKind;
use serde_json::Value;
use serde_json::json;
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
/// `record` is M's record as the caller already read it, inside the state root; launcher.json and spec.json are read
/// the same way, so no file of the proof is reached through a link out of the root (#104).
pub fn prove_member(
    ctx: &Ctx,
    team: &str,
    id: &str,
    record: &Value,
    t: &Terminal,
) -> Option<Member> {
    let (key, _) = team.split_once(':')?;
    let read = |f: &str| {
        crate::caller::hc::within_root(&ctx.state_root, &[key, "agents", id, f])
            .and_then(|p| crate::state::fs::read_json(&p).ok().flatten())
    };
    let launcher = read("launcher.json")?;
    let record = record.clone();
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
    let flags = read("spec.json").and_then(|spec| {
        spec.get("flags")?
            .as_array()?
            .iter()
            .map(|f| f.as_str().map(String::from))
            .collect::<Option<Vec<_>>>()
    })?;
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
pub const TAGGED: &str = "this caller cannot be proven a member";
pub fn tagged() -> Refusal {
    Refusal {
        code: 2,
        check: "tagged",
        message: TAGGED.into(),
        reason: None,
        leftover: None,
    }
}
/// The verbs that re-prove a candidate under the team lock (`resolve_tree`, then `standing`).
pub fn candidate_verb(verb: &str, request: &crate::cli::Request) -> bool {
    matches!(verb, "spawn" | "stop" | "resume")
        || (verb == "read" && !request.positional.is_empty())
}
#[derive(Debug, Clone, PartialEq)]
pub enum Standing {
    Person,
    Member(Member),
}
/// D1 under the team lock: an hc member or candidate is proven again (a failure is 2 `tagged`, never a fallback); a
/// person is a human in an untagged pane; a tmux-pane agent keeps 2 `caller`.
pub fn standing(ctx: &Ctx, caller: &Caller, core: Option<&Core<'_>>) -> Res<Standing> {
    if let Some(id) = caller.member.as_ref().or(caller.candidate.as_ref()) {
        let connected;
        let core = match core {
            Some(core) => core,
            None => {
                connected = crate::hc::connect(ctx)?;
                &connected
            }
        };
        let team = super::membership::team(caller);
        let rel = [caller.server_key.as_str(), "agents", id, "record.json"];
        let record = crate::caller::hc::within_root(&ctx.state_root, &rel)
            .and_then(|p| crate::state::fs::read_json(&p).ok().flatten());
        let address = record
            .as_ref()
            .and_then(|r| super::core_identity::Identity::read(r, id, &team))
            .map(|i| i.address);
        let t = address.and_then(|a| core.get(&a).ok());
        return t
            .zip(record)
            .and_then(|(t, r)| prove_member(ctx, &team, id, &r, &t))
            .map(Standing::Member)
            .ok_or_else(tagged);
    }
    match caller.kind {
        Kind::Human => {
            super::registry::untagged(ctx, caller, TAGGED)?;
            Ok(Standing::Person)
        }
        Kind::Agent => refuse(
            2,
            "caller",
            "an agent in a tmux or rmux pane cannot be proven a member",
        ),
    }
}
/// The ids above member `id`, nearest first, ending at the depth-0 leader; every link readable, one deeper than its
/// parent, no cycle. Anything else refuses 2 `authority`.
pub fn ancestors(records: &BTreeMap<String, Value>, id: &str) -> Res<Vec<String>> {
    let name = records
        .get(id)
        .map_or(id.to_string(), |r| text(r.get("name")));
    let broken = || {
        refuse(
            2,
            "authority",
            format!("the parent chain of {name} cannot be read"),
        )
    };
    let depth = |r: &Value| r.get("depth").and_then(Value::as_u64);
    let (mut chain, mut at) = (Vec::<String>::new(), id.to_string());
    loop {
        let Some(r) = records.get(&at) else {
            return broken();
        };
        if is_leader(r) {
            return Ok(chain);
        }
        let parent = r
            .get("parentAgent")
            .and_then(|p| p.get("id"))
            .and_then(Value::as_str);
        let (Some(d), Some(parent)) = (depth(r), parent) else {
            return broken();
        };
        let up = records.get(parent).and_then(depth);
        if parent == id
            || chain.iter().any(|c| c == parent)
            || up.map(|u| u + 1) != Some(d)
            || chain.len() > MAX_DEPTH as usize
        {
            return broken();
        }
        chain.push(parent.to_string());
        at = parent.to_string();
    }
}
/// D6: the highest level `client` expresses at or below min(parent, ceiling); `bound` still guards.
pub fn child_flags(
    client: Client,
    parent: team::Policy,
    ceiling: team::Policy,
    effort: Option<&str>,
) -> Res<Vec<String>> {
    let flags = super::spawn::launch_flags(client, Some(parent.min(ceiling)), effort);
    bound(team::policy(client, &flags), parent, ceiling)?;
    Ok(flags)
}
pub fn bound(child: team::Policy, parent: team::Policy, ceiling: team::Policy) -> Res<()> {
    if !team::within(ceiling, child) {
        return refuse(
            2,
            "ceiling",
            "the child's flags exceed the team's permission ceiling",
        );
    }
    if !team::within(parent, child) {
        return refuse(2, "ceiling", "the child's flags exceed its parent's policy");
    }
    Ok(())
}
pub fn no_team<T>() -> Res<T> {
    refuse(
        1,
        "team",
        "the team record is missing; this team cannot spawn until up repairs it (2a-iii-b)",
    )
}
/// The team record, read first under the team lock: None outside a team; a launched leader without it refuses 1 `team`
/// before any hc call.
pub fn team_of(ctx: &Ctx, caller: &Caller) -> Res<Option<Team>> {
    match scope(ctx, caller)? {
        Scope::Outside => Ok(None),
        Scope::Leaderless => no_team(),
        Scope::Team(t) => Ok(Some(t)),
    }
}
/// A proven caller's child, from local records only (no snapshot, no reconcile), its depth checked.
#[derive(Debug)]
pub struct Admitted {
    pub team: Team,
    pub member: bool,
    pub depth: u64,
    pub parent: Value,
    pub parent_pane: String,
    pub level: team::Policy,
    pub directory: Option<String>,
}
pub fn admit(ctx: &Ctx, caller: &Caller, found: &Team, standing: Standing) -> Res<Admitted> {
    let records: BTreeMap<String, Value> = super::membership::locals(ctx, caller)?
        .into_iter()
        .map(|l| (l.identity.id, l.stored.record))
        .collect();
    let a = match standing {
        Standing::Person => {
            let leader = records
                .get(&found.leader)
                .ok_or_else(|| Refusal::internal("the team's leader has no record"))?;
            let pane = text(leader.get("pane"));
            Admitted {
                team: found.clone(),
                member: false,
                depth: 1,
                level: found.policy,
                directory: None,
                parent: json!({"client": text(leader.get("client")), "pid": null, "start": null,
                    "sessionId": leader.get("sessionId"), "id": found.leader}),
                parent_pane: if super::pane_id(&pane) {
                    pane
                } else {
                    caller.pane.clone()
                },
            }
        }
        Standing::Member(m) => Admitted {
            team: found.clone(),
            member: true,
            depth: ancestors(&records, &m.id)?.len() as u64 + 1,
            level: team::policy(m.client, &m.flags),
            directory: m
                .record
                .get("directory")
                .and_then(Value::as_str)
                .map(String::from),
            parent: json!({"client": super::spawn::client_name(m.client), "pid": m.pid, "start": m.start,
                "sessionId": m.record.get("sessionId"), "id": m.id}),
            parent_pane: caller.pane.clone(),
        },
    };
    check_depth(a.depth)?;
    Ok(a)
}
/// Where an admitted child goes, and its name.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub depth: u64,
    pub parent: Value,
    pub parent_pane: String,
    pub flags: Vec<String>,
    pub directory: Option<String>,
    pub name: String,
}
/// After the snapshot: limit, ceiling, name, then a member's token last, so a refusal spends nothing.
pub fn place(
    ctx: &Ctx,
    caller: &Caller,
    a: &Admitted,
    snapshot: &Snapshot,
    client: Client,
    effort: Option<&str>,
    name: impl FnOnce() -> Res<String>,
) -> Res<Placed> {
    limit(snapshot)?;
    let flags = child_flags(client, a.level, a.team.policy, effort)?;
    let name = name()?;
    if a.member {
        let file = super::budget::path(&server_dir(ctx, caller), &caller.session);
        super::budget::spend(
            &file,
            &super::budget::rate(ctx, caller),
            super::budget::now_secs(ctx),
        )?;
    }
    Ok(Placed {
        depth: a.depth,
        parent: a.parent.clone(),
        parent_pane: a.parent_pane.clone(),
        flags,
        directory: a.directory.clone(),
        name,
    })
}
#[cfg(test)]
#[path = "tree_tests.rs"]
pub(crate) mod tests;
