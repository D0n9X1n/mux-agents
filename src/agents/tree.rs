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
/// proof-0b K2: the launcher and the client, each as (pid, start), of lifecycle member `id`'s active generation,
/// from K1a only. The id and the generation are plain values before any join (redesign point 3); every file is
/// read through `gens`, whose walk opens only regular files below real folders (point 8, state-0 S5). Only a
/// commit that decides Run proves; Unreadable is unproven here, never a refusal.
fn lifecycle_client(
    ctx: &Ctx,
    key: &str,
    id: &str,
    record: &Value,
) -> Option<((u32, i64), (u32, i64))> {
    use super::gens;
    use crate::state::evidence::Evidence;
    if !gens::plain_id(id) {
        return None;
    }
    let n = gens::plain_gen(record.get("generation")?.as_str()?)?;
    let member = ctx.state_root.join(key).join("agents").join(id);
    if !matches!(gens::record(&member), Evidence::Present(_)) {
        return None;
    }
    if !gens::committed(&member, n).ok()? {
        return None;
    }
    let proven = gens::client(ctx, &member, n)?;
    Some((proven.launcher, proven.client))
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
    let record = record.clone();
    let pid_start = |v: &Value| {
        Some((
            u32::try_from(v.get("pid")?.as_u64()?).ok()?,
            v.get("start")?.as_i64()?,
        ))
    };
    let ((l_pid, l_start), (c_pid, c_start)) = if super::gens::lifecycle(&record) {
        lifecycle_client(ctx, key, id, &record)?
    } else {
        let launcher = read("launcher.json")?;
        (pid_start(&launcher)?, pid_start(launcher.get("client")?)?)
    };
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
    let flags = super::resume::member_flags(&ctx.state_root, key, id, &record).ok()?;
    Some(Member {
        id: id.into(),
        record,
        client: named,
        pid: c_pid,
        start: c_start,
        flags,
    })
}
/// Whether terminal `t` still shows member `identity`'s launch (D1): `committed` (commit.json there and not aborted), its
/// identity labels, and the recorded launcher running in it. Dispatch and the team lock ask it of every reading of the
/// terminal, the first and the one `sampled` returns, so the two can never check different things.
pub fn launch_holds(
    identity: &super::core_identity::Identity,
    t: &Terminal,
    committed: bool,
) -> bool {
    committed
        && identity.matches(t)
        && super::lifecycle::classify(Some(t), &identity.launcher) == Life::Running
}
/// How many times a terminal hc has not yet sampled is read again before the proof decides.
pub const SAMPLE_READS: usize = 20;
/// hc names a terminal's foreground only once it has sampled the terminal, about 100 ms after it starts: a member that
/// acts before that is read again, on the clock, up to `SAMPLE_READS` times. A terminal that never names one, or a
/// read that fails, is returned as it is, so the proof still refuses it (D1 stays fail-closed).
pub fn sampled(ctx: &Ctx, core: &Core<'_>, mut t: Terminal) -> Terminal {
    for _ in 0..SAMPLE_READS {
        if t.foreground.is_some() {
            break;
        }
        ctx.clock.sleep(std::time::Duration::from_millis(100));
        match core.get(&t.address) {
            Ok(next) => t = next,
            Err(_) => break,
        }
    }
    t
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
        details: Default::default(),
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
        let identity = record
            .as_ref()
            .and_then(|r| super::core_identity::Identity::read(r, id, &team));
        // What dispatch proved, proved again under the lock: the launch still committed (not aborted) and its terminal
        // still running the recorded launcher, read inside the state root.
        let rel = [caller.server_key.as_str(), "agents", id, "commit.json"];
        let committed = crate::caller::hc::within_root(&ctx.state_root, &rel)
            .and_then(|p| crate::state::fs::read_json(&p).ok().flatten())
            .is_some_and(|c| c.get("aborted") != Some(&Value::Bool(true)));
        let t = identity.as_ref().and_then(|i| {
            core.get(&i.address)
                .ok()
                .map(|t| sampled(ctx, core, t))
                .filter(|t| launch_holds(i, t, committed))
        });
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
/// The ids above member `id`, nearest first, ending at `leader`, the team record's own depth-0 leader; every link
/// readable, one deeper than its parent, no cycle. A chain that ends at any other root, even a depth-0 `upTeam` record,
/// and anything else, refuses 2 `authority`.
pub fn ancestors(records: &BTreeMap<String, Value>, id: &str, leader: &str) -> Res<Vec<String>> {
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
            return if at == leader { Ok(chain) } else { broken() };
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
            depth: ancestors(&records, &m.id, &found.leader)?.len() as u64 + 1,
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
/// After the snapshot: limit, ceiling, name, then a member's budget last. It is only checked here: the launch takes
/// the token as its last step before `hc run` (`CoreSlot::debit`), so no later refusal spends it (D5).
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
        super::budget::check(
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
/// stop, read <name>, resume: a person passes; a member only when its id is in the target's chain to the leader.
/// Records only: mail grants nothing.
pub fn authorize(
    records: &BTreeMap<String, Value>,
    leader: &str,
    standing: &Standing,
    target: &str,
    verb: &str,
) -> Res<()> {
    let Standing::Member(m) = standing else {
        return Ok(());
    };
    if ancestors(records, target, leader)?.contains(&m.id) {
        return Ok(());
    }
    let name = records
        .get(target)
        .map_or(target.to_string(), |r| text(r.get("name")));
    refuse(
        2,
        "authority",
        format!("only an ancestor or a person can {verb} {name}"),
    )
}
/// Runs `f` with the caller's standing under the team lock, held across the authority check and the action; None
/// outside a team (0.2 rules; a candidate there refuses 2 `tagged`).
pub fn with_standing<T>(
    ctx: &Ctx,
    caller: &Caller,
    f: impl FnOnce(Option<&Standing>) -> Res<T>,
) -> Res<T> {
    if matches!(scope(ctx, caller)?, Scope::Outside) {
        return if caller.candidate.is_some() {
            Err(tagged())
        } else {
            f(None)
        };
    }
    super::lock(
        ctx,
        &super::lock_key("spawn", caller, &caller.session),
        || f(Some(&standing(ctx, caller, None)?)),
    )
}
/// `authorize` over the session's local records.
pub fn authority(
    ctx: &Ctx,
    caller: &Caller,
    standing: &Standing,
    target: &str,
    verb: &str,
) -> Res<()> {
    // A person needs no team record and no ancestry: `up` may have launched its leader and stopped before writing the
    // record, and a person may still stop or read it. The target's identity and view are checked by the caller.
    if matches!(standing, Standing::Person) {
        return Ok(());
    }
    let Some(team) = team_of(ctx, caller)? else {
        return no_team();
    };
    let records = super::membership::locals(ctx, caller)?
        .into_iter()
        .map(|l| (l.identity.id, l.stored.record))
        .collect();
    authorize(&records, &team.leader, standing, target, verb)
}
/// The level X's parent may grant: the team's for the leader, else the parent's recorded flags (unreadable refuses).
pub fn parent_level(
    ctx: &Ctx,
    caller: &Caller,
    found: &Team,
    records: &BTreeMap<String, Value>,
    x: &str,
) -> Res<team::Policy> {
    let parent = records
        .get(x)
        .and_then(|r| r.get("parentAgent"))
        .and_then(|p| p.get("id"))
        .and_then(Value::as_str);
    let Some(p) = parent.filter(|p| *p != found.leader) else {
        return Ok(found.policy);
    };
    let broken = || Refusal {
        code: 2,
        check: "authority",
        reason: None,
        leftover: None,
        details: Default::default(),
        message: format!(
            "the parent chain of {} cannot be read",
            text(records[x].get("name"))
        ),
    };
    let parent_record = records.get(p).ok_or_else(broken)?;
    let client =
        super::spawn::client_named(&text(parent_record.get("client"))).ok_or_else(broken)?;
    let flags = super::resume::member_flags(&ctx.state_root, &caller.server_key, p, parent_record)
        .map_err(|_| broken())?;
    Ok(team::policy(client, &flags))
}
#[cfg(test)]
#[path = "tree_tests.rs"]
pub(crate) mod tests;
