//! resume: starts an ended member X again with its conversation (`<client> <flags> --resume <uuid>`), never
//! resending its task, as a new member Y from spawn's launch path. X's folder is never written.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::core_identity::Identity;
use super::lifecycle::{Life, classify};
use super::membership::Snapshot;
use super::placement::{DEFAULT as DEFAULT_PLACEMENT, read_placement};
use super::spawn::{CoreRequest, CoreSlot, Resumed, client_named, keep_bar, launch_hc_locked};
use super::view::first;
use super::{lock, lock_key, registry, server_dir, text, usage};
use crate::caller::{Kind, resolve};
use crate::cli::Request;
use crate::ctx::Ctx;
use crate::error::{Refusal, Res, refuse};
use crate::hc::{Core, Terminal};
use crate::state::fs::read_json;

pub fn resume(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let target = first(request);
    if !super::agent_id(target) && !super::agent_name(target) {
        return Err(usage("resume"));
    }
    let caller = resolve(ctx)?;
    // A person only: an agent never resumes a member.
    if caller.kind == Kind::Agent {
        return refuse(2, "tagged", "an agent cannot resume agents");
    }
    registry::untagged(ctx, &caller, "an agent pane cannot resume agents")?;
    let split = match read_placement(ctx, &caller) {
        Ok(Some(stored)) if stored != DEFAULT_PLACEMENT => Some(stored),
        _ => None,
    };
    let core = crate::hc::connect(ctx)?;
    let (from, record) = lock(ctx, &lock_key("spawn", &caller, &caller.session), || {
        let snapshot = Snapshot::for_spawn(ctx, &caller, &core)?;
        let resumed = admit(ctx, &caller, &core, &snapshot, target)?;
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
    // Spawn's own post-commit bar step, so a change to the bar changes both verbs.
    keep_bar(ctx, &caller, &record, &mut reply);
    Ok(reply)
}

/// X as `target` names it, checked under the spawn lock: `<id>` is that member, `<name>` the newest (by
/// `createdAt`) of that name that no resume superseded. X must be proven ended, not taken, no leader, its session
/// live nowhere (`session_held`), no member of its name running, its flags within the team's ceiling, and the
/// session under its cap.
fn admit(
    ctx: &Ctx,
    caller: &crate::caller::Caller,
    core: &Core<'_>,
    snapshot: &Snapshot,
    target: &str,
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
            })?,
    };
    let id = &x.identity.id;
    let record = snapshot.records[id].record.clone();
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
    if let Some(why) = session_held(ctx, core, id, &x.identity.session_id) {
        return refuse(2, "session", why);
    }
    // shortcut: no record on this base sets `depth` (no `up` yet); a member whose record says depth 0 is a leader.
    if record.get("depth").and_then(Value::as_u64) == Some(0) {
        return refuse(2, "leader", "a leader is never resumed");
    }
    let flags = recorded_flags(&server_dir(ctx, caller).join("agents").join(id))?;
    let client = client_named(&text(record.get("client")))
        .ok_or_else(|| Refusal::internal("the ended member names no known client"))?;
    let team_file = super::team::path(&server_dir(ctx, caller), &caller.session);
    if let Some(team) = super::team::read(&team_file)?
        && !super::team::within(team.policy, super::team::policy(client, &flags))
    {
        return refuse(
            2,
            "ceiling",
            "its flags exceed the team's permission ceiling",
        );
    }
    if snapshot.facts.count() >= 6 {
        return refuse(2, "cap", "the session already has 6 live agents");
    }
    Ok(Resumed {
        from: id.clone(),
        record,
        flags,
    })
}

/// The client flags X's launch spec recorded; X never resumes with flags its spec does not name.
pub(crate) fn recorded_flags(folder: &Path) -> Res<Vec<String>> {
    read_json(&folder.join("spec.json"))?
        .and_then(|spec| {
            spec.get("flags")?
                .as_array()?
                .iter()
                .map(|f| f.as_str().map(String::from))
                .collect()
        })
        .ok_or_else(|| Refusal::internal("the ended member's launch spec names no client flags"))
}

/// The member that took `id`: one naming `id` in `resumedFrom` that either claimed its run (`gens/0.claim` = `run`),
/// or is not yet proven dead: its launch neither aborted (abort.json) nor was abandoned. Every folder counts,
/// committed or not; an unreadable record or claim takes `id`, so a name is never reused on a guess. The refusal
/// names that member (its folder), so the person can resume it.
fn taken(ctx: &Ctx, caller: &crate::caller::Caller, id: &str) -> Res<Option<String>> {
    let agents = server_dir(ctx, caller).join("agents");
    let entries = match std::fs::read_dir(&agents) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let folder = entry?.path();
        let Some(successor) = folder
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| super::agent_id(n))
            .map(str::to_owned)
        else {
            continue;
        };
        if !folder.is_dir() {
            continue;
        }
        let Ok(record) = read_json(&folder.join("record.json")) else {
            return Ok(Some(successor));
        };
        if record
            .as_ref()
            .and_then(|r| r.get("resumedFrom"))
            .and_then(Value::as_str)
            != Some(id)
        {
            continue;
        }
        let claim = match read_json(&folder.join("gens").join("0.claim")) {
            Ok(claim) => claim,
            Err(_) => return Ok(Some(successor)),
        };
        match claim.as_ref().and_then(Value::as_str) {
            Some("abandoned") => continue,
            None if claim.is_none() && folder.join("abort.json").exists() => continue,
            _ => return Ok(Some(successor)),
        }
    }
    Ok(None)
}

/// Why X's conversation may be live somewhere (amendment 1 on #103), or none when it is live nowhere:
/// - no process on this machine occupies it (`occupies`). This covers every client hc runs (an hc terminal's client
///   is a process of this user on this machine) and a client someone resumed by hand outside hc or the state root;
/// - no claude or copilot process on this machine has a command line that cannot be read;
/// - every other member record in any server folder of the state root that carries it is proven ended by hc. A
///   member a successor names in `resumedFrom` was proven ended when that successor was admitted.
///
/// Unclear evidence (a process table, hc or a record that cannot be read) is never free.
fn session_held(ctx: &Ctx, core: &Core<'_>, x: &str, session: &str) -> Option<String> {
    let live = || {
        "that conversation is live in another member or terminal, or cannot be proven ended"
            .to_string()
    };
    let held = || -> Res<Option<String>> {
        // A row with no argv is one the OS will not read for this user (another user's, or one this user cannot
        // open). When it is still known as a claude or copilot client (`client_of`, or its image's file name), it
        // may hold the conversation, so it refuses; one neither readable nor known as a client is skipped. Known
        // limits until slice 5: such an unknown row, and a client that switched into the conversation with
        // `/resume`, which shows no uuid.
        for row in ctx.os.table()? {
            match row.argv.as_deref() {
                Some(argv) if occupies(argv, session) => return Ok(Some(live())),
                Some(_) => {}
                None if unreadable_client(ctx, &row) => {
                    return Ok(Some(format!(
                        "process {} is a claude or copilot client whose command line cannot be read, so it may \
                         hold that conversation",
                        row.pid
                    )));
                }
                None => {}
            }
        }
        let records = every_record(ctx)?;
        let named: BTreeSet<&str> = records
            .iter()
            .filter_map(|(_, r)| r.get("resumedFrom").and_then(Value::as_str))
            .collect();
        for (folder, record) in &records {
            let id = folder.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if id == x || named.contains(id) || text(record.get("sessionId")) != session {
                continue;
            }
            if !ended(core, folder, id, record) {
                return Ok(Some(live()));
            }
        }
        Ok(None)
    };
    held().unwrap_or_else(|_| Some(live()))
}

/// Whether `row` (whose argv cannot be read) is a claude or copilot client: the OS says so, or its image's file
/// name is `claude` or `copilot` (with `.exe` on Windows).
fn unreadable_client(ctx: &Ctx, row: &crate::proc::Row) -> bool {
    if ctx.os.client_of(row).is_some() {
        return true;
    }
    row.path.as_deref().is_some_and(|path| {
        let file = path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(path)
            .to_ascii_lowercase();
        let file = file.strip_suffix(".exe").unwrap_or(&file);
        file == "claude" || file == "copilot"
    })
}

/// Whether a process with `argv` runs a client on `session`: the uuid follows `--session-id` (or
/// `--session-id=`), or `--resume`, `-r` (or `--resume=`) in a command line without `--fork-session`. A Claude fork
/// (`--resume <source> --fork-session --session-id <new>`) occupies only the new conversation; a Copilot fork resumes
/// its own copy under the new id, so its `--resume` already names only the new one. Words after `--` are no flags.
fn occupies(argv: &[String], session: &str) -> bool {
    let flags: Vec<&str> = argv
        .iter()
        .map(String::as_str)
        .take_while(|a| *a != "--")
        .collect();
    let fork = flags.contains(&"--fork-session");
    flags.iter().enumerate().any(|(i, a)| {
        let (name, inline) = match a.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (*a, None),
        };
        let resume = name == "--resume" || (name == "-r" && inline.is_none());
        if !(name == "--session-id" || (resume && !fork)) {
            return false;
        }
        inline.or_else(|| flags.get(i + 1).copied()) == Some(session)
    })
}

/// Every member record of every server folder of the state root, with its folder; one that cannot be read fails.
pub(crate) fn every_record(ctx: &Ctx) -> Res<Vec<(PathBuf, Value)>> {
    let mut all = Vec::new();
    let servers = match std::fs::read_dir(&ctx.state_root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(all),
        Err(e) => return Err(e.into()),
    };
    for server in servers {
        let agents = server?.path().join("agents");
        if !agents.is_dir() {
            continue;
        }
        for agent in std::fs::read_dir(&agents)? {
            let folder = agent?.path();
            let named = folder
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(super::agent_id);
            if !named || !folder.is_dir() {
                continue;
            }
            if let Some(record) = read_json(&folder.join("record.json"))? {
                all.push((folder, record));
            }
        }
    }
    Ok(all)
}

/// Whether the member recorded in `folder` is proven ended, asking hc about it alone. An hc member: its own
/// terminal, matched by address and labels, classifies Ended. A launch whose run named no address yet: its claim was
/// abandoned, or the one generation-0 terminal labelled with its id has finished, or it never claimed and aborted. A
/// pre-hc record: its end notice.
fn ended(core: &Core<'_>, folder: &Path, id: &str, record: &Value) -> bool {
    if record.get("hcAddress").is_some() {
        let identity = record
            .get("team")
            .and_then(Value::as_str)
            .and_then(|team| Identity::read(record, id, team));
        return identity.is_some_and(|i| {
            let t = core.get(&i.address).ok().filter(|t| i.matches(t));
            classify(t.as_ref(), &i.launcher) == Life::Ended
        });
    }
    match read_json(&folder.join("gens").join("0.claim")) {
        Ok(Some(claim)) if claim.as_str() == Some("abandoned") => true,
        Ok(Some(_)) => {
            let Ok(listed) = core.ls(&[("mux.managed", "1"), ("mux.id", id), ("mux.gen", "0")])
            else {
                return false;
            };
            let listed: BTreeSet<String> = listed.into_iter().collect();
            match listed.iter().collect::<Vec<_>>().as_slice() {
                [address] => core.get(address).is_ok_and(|t| {
                    t.address == **address
                        && labelled(&t, id)
                        && classify(Some(&t), &[]) == Life::Ended
                }),
                _ => false,
            }
        }
        Ok(None) => folder.join("abort.json").exists() || folder.join("notice.json").exists(),
        Err(_) => false,
    }
}

/// Whether `t` is managed generation 0 of member `id`, as hc run labelled it.
fn labelled(t: &Terminal, id: &str) -> bool {
    [("mux.managed", "1"), ("mux.id", id), ("mux.gen", "0")]
        .iter()
        .all(|(k, v)| t.labels.get(*k).map(String::as_str) == Some(*v))
}

#[cfg(test)]
#[path = "resume_tests.rs"]
mod tests;
