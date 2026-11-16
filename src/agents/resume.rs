//! resume: starts an ended member X again with its conversation (`<client> <flags> --resume <uuid>`), never
//! resending its task, as a new member Y from spawn's launch path. X's folder is never written.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::gens::Member;
use super::lifecycle::Life;
use super::membership::Snapshot;
use super::placement::{DEFAULT as DEFAULT_PLACEMENT, read_placement};
use super::spawn::{CoreRequest, CoreSlot, Resumed, client_named, keep_bar, launch_hc_locked};
use super::view::first;
use super::{lock, lock_key, registry, server_dir, text, usage};
use crate::caller::Kind;
use crate::cli::Request;
use crate::ctx::Ctx;
use crate::error::{Refusal, Res, refuse};
use crate::state::fs::read_json;

pub fn resume(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let target = first(request);
    if !super::agent_id(target) && !super::agent_name(target) {
        return Err(usage("resume"));
    }
    let caller = crate::caller::resolve_tree(ctx)?;
    crate::cli::member_views(ctx, &caller)?;
    if matches!(
        super::tree::scope(ctx, &caller)?,
        super::tree::Scope::Outside
    ) {
        // Outside a team, a person only: an agent never resumes a member.
        if caller.kind == Kind::Agent {
            return refuse(2, "tagged", "an agent cannot resume agents");
        }
        registry::untagged(ctx, &caller, "an agent pane cannot resume agents")?;
    } else {
        // A leaderless team refuses before any hc call.
        lock(ctx, &super::team_lock(&caller), || {
            super::tree::team_of(ctx, &caller)
        })?;
    }
    // prelife-1c G10d (call 16): a pre-lifecycle subject is refused from its record, before any hc call.
    super::prelife::refuse_resume(ctx, &caller, target)?;
    let split = match read_placement(ctx, &caller) {
        Ok(Some(stored)) if stored != DEFAULT_PLACEMENT => Some(stored),
        _ => None,
    };
    let core = crate::hc::connect(ctx)?;
    let (from, record) = lock(ctx, &super::team_lock(&caller), || {
        // adopt-3e call 13: every input of this decision (standing, snapshot, records) is read after replay.
        super::adopt::replay_held(ctx, &caller)?;
        let gate = match super::tree::team_of(ctx, &caller)? {
            Some(t) => Some((t, super::tree::standing(ctx, &caller, Some(&core))?)),
            None if caller.kind == Kind::Agent => {
                return refuse(2, "tagged", "an agent cannot resume agents");
            }
            None => None,
        };
        let snapshot = Snapshot::for_spawn(ctx, &caller, &core)?;
        let resumed = admit(ctx, &caller, &snapshot, target, gate.as_ref())?;
        // A member's resume takes its token as the launch's last step before `hc run` (D5).
        let debit = matches!(&gate, Some((_, super::tree::Standing::Member(_))));
        // The mailbox barrier: a reader of X's mailbox that holds its lock finishes before Y exists; one after it
        // finds X superseded (or not yet running Y) and reads nothing. No mailbox lock is held across the launch.
        let mailbox = super::mail::member_mailbox(&resumed.from, &resumed.record);
        lock(ctx, &lock_key("mail", &caller, &mailbox), || Ok(()))?;
        let x = &resumed.record;
        let client = client_named(&text(x.get("client")))
            .ok_or_else(|| Refusal::internal("the ended member names no known client"))?;
        let directory = text(x.get("directory"));
        let name = text(x.get("name"));
        let color = crate::paint::identity(x.get("color").and_then(Value::as_str), &name);
        let record = launch_hc_locked(
            ctx,
            &caller,
            &core,
            CoreRequest {
                client,
                task: "",
                brief: None,
                directory: Path::new(&directory),
                model: None,
                no_fork: true,
                effort: None,
                split,
                resume: Some(&resumed),
                leader: None,
            },
            |_| {
                Ok(CoreSlot {
                    name,
                    color,
                    placed: None,
                    debit,
                })
            },
        )?;
        Ok((resumed.from.clone(), record))
    })?;
    let mut reply = Map::new();
    reply.insert("from".into(), from.into());
    reply.insert(
        "agent".into(),
        json!({"id": record["id"], "name": record["name"], "hcAddress": record["hcAddress"]}),
    );
    reply.extend(super::spawn::view_reply(&record));
    // Spawn's own post-commit bar step, so a change to the bar changes both verbs.
    keep_bar(ctx, &caller, &record, &mut reply);
    Ok(reply)
}

/// X as `target` names it, checked under the spawn lock: `<id>` is that member, `<name>` the newest (by
/// `createdAt`) of that name that no resume superseded. X must be proven ended, not taken, no leader, its session
/// live nowhere (`occupancy::held`) and no member of its name running. Outside a team the session stays under its cap; in
/// a team the caller's authority, the tree's limit, the parent's and the team's ceiling, the name and a member's budget
/// are checked in that order, and the budget is only checked here: the launch spends it (#103 D5).
fn admit(
    ctx: &Ctx,
    caller: &crate::caller::Caller,
    snapshot: &Snapshot,
    target: &str,
    gate: Option<&(super::team::Team, super::tree::Standing)>,
) -> Res<Resumed> {
    if let Some(e) = &snapshot.core_error {
        return Err(e.clone());
    }
    let superseded = snapshot.superseded();
    let local = &snapshot.facts.local;
    let x = match local.iter().find(|o| o.identity.id == target) {
        Some(o) => o,
        None => local
            .iter()
            .filter(|o| o.identity.name == target && !superseded.contains(&o.identity.id))
            .max_by_key(|o| {
                let created = snapshot.records[&o.identity.id]
                    .record
                    .get("createdAt")
                    .and_then(Value::as_str)
                    .and_then(crate::util::parse_utc);
                (created, std::cmp::Reverse(o.identity.id.clone()))
            })
            .ok_or_else(|| Refusal {
                code: 2,
                check: "target",
                message: "no member has that name or id".into(),
                reason: None,
                leftover: None,
                details: Default::default(),
            })?,
    };
    let id = &x.identity.id;
    if let Some((_, s)) = gate {
        // Call 10: over the session's forward-resolved tree; the spawn lock (the team lock) is held.
        super::tree::authority(ctx, caller, s, id, "resume")?;
    }
    let record = snapshot.records[id].record.clone();
    // prelife-1c G10d (call 20): the subject this admission selected is lifecycle, whatever the name also names.
    if !super::gens::lifecycle(&record) {
        return Err(super::prelife::pre_lifecycle(&x.identity.name));
    }
    let life = match x.life {
        Life::Ended => None,
        Life::Running => Some("running"),
        Life::Lost => Some("lost"),
        Life::Unknown => Some("unknown"),
    };
    if let Some(life) = life {
        return refuse(
            2,
            "life",
            format!(
                "{} is {life}: only an ended member resumes",
                x.identity.name
            ),
        );
    }
    if let Some(successor) = taken(ctx, caller, id)? {
        return refuse(
            2,
            "taken",
            format!(
                "this member was already resumed as {successor}: run `mux-agent resume {successor}`"
            ),
        );
    }
    // cascade-3d C3: no journal around X, and X's parent open. Under the spawn lock (the team lock); read only.
    let tx = super::team_state::Tx {
        ctx,
        scope: super::team_state::Scope::of(caller),
    };
    super::cascade::reopen_check(&tx, id)?;
    let name_check = || -> Res<()> {
        let others = local
            .iter()
            .filter(|o| o.identity.id != *id && o.life != Life::Ended);
        if others.clone().any(|o| o.identity.name == x.identity.name)
            || snapshot
                .facts
                .reserved
                .iter()
                .any(|(_, n)| *n == x.identity.name)
        {
            return refuse(2, "name", "a member of that name is running");
        }
        Ok(())
    };
    if gate.is_none() {
        name_check()?;
    }
    // occupy-3 H4: any live process that may hold X's conversation refuses, named by pid, image and rule (call 2).
    // The client caller proof established is exempt from H4.4/H4.5 (calls 1, 7): the member proof `standing` returned
    // under this lock (a candidate's re-proof included), else a pane caller's proven agent. A record naming no known
    // client skips this and is refused `internal` below, before any launch (ambiguity 9).
    if let Some(kind) = client_named(&text(record.get("client"))) {
        let own = super::occupancy::own_of(ctx, caller, gate.map(|(_, s)| s));
        if let Some(hold) = super::occupancy::held(ctx, &x.identity.session_id, kind, own.as_ref())
        {
            return refuse(2, "session", hold.message());
        }
    }
    // A leader is the depth-0 record `up` makes (leader.rs); it is never resumed.
    if record.get("depth").and_then(Value::as_u64) == Some(0) {
        return refuse(2, "leader", "a leader is never resumed");
    }
    let client = client_named(&text(record.get("client")))
        .ok_or_else(|| Refusal::internal("the ended member names no known client"))?;
    let flags = member_flags(
        &ctx.state_root,
        &super::scope::state(caller).key,
        id,
        &record,
    )?;
    // E2: a lifecycle member replays only its stored options, within the team ceiling, before any other gate.
    let flags = if super::gens::lifecycle(&record) {
        super::options::replay(client, &flags, ceiling(gate.map(|(team, _)| team)))?
    } else {
        flags
    };
    match gate {
        None if snapshot.facts.count() >= 6 => {
            return refuse(2, "cap", "the session already has 6 live agents");
        }
        None => {}
        Some((team, s)) => {
            super::tree::limit(snapshot)?;
            let level = super::tree::parent_level(
                ctx,
                caller,
                team,
                &super::tree::records_of(snapshot),
                id,
            )?;
            super::tree::bound(super::team::policy(client, &flags), level, team.policy)?;
            name_check()?;
            if matches!(s, super::tree::Standing::Member(_)) {
                let file = super::budget::path(
                    &server_dir(ctx, caller),
                    &super::scope::state(caller).session,
                );
                super::budget::check(
                    &file,
                    &super::budget::rate(ctx, caller),
                    super::budget::now_secs(ctx),
                )?;
            }
        }
    }
    Ok(Resumed {
        from: id.clone(),
        record,
        flags,
    })
}

/// The client flags member `id`'s launch spec recorded; X never resumes with flags its spec does not name. The spec is
/// read only inside the state root (`within_root`, #104): one reached through a link out of it names no flags.
pub(crate) fn recorded_flags(state_root: &Path, key: &str, id: &str) -> Res<Vec<String>> {
    let spec = crate::caller::hc::within_root(state_root, &[key, "agents", id, "spec.json"]);
    spec.map(|p| read_json(&p))
        .transpose()?
        .flatten()
        .and_then(|spec| {
            spec.get("flags")?
                .as_array()?
                .iter()
                .map(|f| f.as_str().map(String::from))
                .collect()
        })
        .ok_or_else(|| Refusal::internal("the ended member's launch spec names no client flags"))
}

/// The flags member `id` runs with: the stored `options` of a lifecycle record (options-6e E1), never its spec;
/// the folder-level launch spec of any other member. The one choice resume, member proof and parent level share.
pub(crate) fn member_flags(
    state_root: &Path,
    key: &str,
    id: &str,
    record: &Value,
) -> Res<Vec<String>> {
    if super::gens::lifecycle(record) {
        super::options::stored(record)
    } else {
        recorded_flags(state_root, key, id)
    }
}

/// E2: the ceiling replayed options are held to: the policy `up` fixed for the team; outside a team none (0.2
/// rules), and `replay` still refuses a prompt or a session choice.
pub(crate) fn ceiling(team: Option<&super::team::Team>) -> super::team::Policy {
    team.map_or(super::team::Policy::Bypass, |t| t.policy)
}

/// The member that took `id`: one naming `id` in `resumedFrom` that either claimed its run (`gens/0.claim` = `run`),
/// or is not yet proven dead: its launch neither aborted (abort.json) nor was abandoned. Every folder counts,
/// committed or not; an unreadable record or claim takes `id`, so a name is never reused on a guess. The refusal
/// names that member (its folder), so the person can resume it.
fn taken(ctx: &Ctx, caller: &crate::caller::Caller, id: &str) -> Res<Option<String>> {
    let agents = server_dir(ctx, caller).join("agents");
    for (successor, folder, member) in super::gens::members(&agents)? {
        let record = match &member {
            Member::Absent => continue,
            Member::Unreadable(_) => return Ok(Some(successor)), // R2, ambiguity 13
            Member::Pre(r) => r,
            Member::Life(g) => g.record(),
        };
        if record.get("resumedFrom").and_then(Value::as_str) != Some(id) {
            continue;
        }
        if let Member::Life(g) = &member {
            // R1, R2: one status; only a proven void first launch frees the member.
            // b-calls 10, 23: never ran only by recorded void evidence; an Unreadable answer takes it.
            if let Ok(true) = g.status().and_then(|s| g.never_ran_recorded(&s)) {
                continue;
            }
            return Ok(Some(successor));
        }
        if legacy_void(&folder) {
            continue;
        }
        return Ok(Some(successor));
    }
    Ok(None)
}

/// The legacy rule (`taken`, call 23): a pre-lifecycle successor never ran when its claim is `abandoned`, or when it
/// has no claim and abort.json exists. An unreadable claim ran, so a name is never reused on a guess.
pub(crate) fn legacy_void(folder: &Path) -> bool {
    let Ok(claim) = read_json(&folder.join("gens").join("0.claim")) else {
        return false;
    };
    match claim.as_ref().and_then(Value::as_str) {
        Some("abandoned") => true,
        None => claim.is_none() && folder.join("abort.json").exists(),
        _ => false,
    }
}

/// R1, R5: every member folder of every server folder of the state root, classified once, links kept.
pub(crate) fn every_member(ctx: &Ctx) -> Res<Vec<(PathBuf, super::gens::Member)>> {
    let servers = match std::fs::read_dir(&ctx.state_root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut all = Vec::new();
    for server in servers {
        let agents = server?.path().join("agents");
        if !agents.is_dir() {
            continue;
        }
        all.extend(
            super::gens::members(&agents)?
                .into_iter()
                .map(|(_, folder, m)| (folder, m)),
        );
    }
    Ok(all)
}

#[cfg(test)]
mod tests;
