//! The leader `up` launches (#103): `<client> <flags> --session-id <uuid>`, depth 0, no parents, `upTeam`. Its folder
//! is flushed to disk before `forward` writes its record, which `forward` flushes before `hc run`.

use std::path::Path;

use serde_json::json;

use super::core_launch::Launch;
use super::spawn::{CoreSlot, Recipe, client_args, client_name, private_folder, utf8};
use crate::{
    caller::Caller,
    ctx::Ctx,
    error::Res,
    parse::Client,
    state::fs::{sync_ancestor, sync_dir, write_json},
    util::{guid, hex_id, utc_o},
};

/// restore-6c P4.3 (call 17): the leader step `up`'s pass runs for 6a's validated leader, after its adjudication.
pub(crate) type LeaderStep = fn(
    &crate::agents::team_state::Tx<'_>,
    &crate::hc::Lazy<'_>,
    &str,
) -> Res<Option<serde_json::Value>>;

pub struct Leader {
    pub team: String,
    /// The client options of the person (options-6e E1), byte for byte; a relaunch replays only these.
    pub options: Vec<String>,
    /// The prompt tokens of the person, byte for byte; run once, never replayed.
    pub prompt: Vec<String>,
}

/// Re6a call 24: the real `prepare` with a test slot and leader, returning the record it would create.
#[cfg(test)]
pub fn prepare_for_test(ctx: &Ctx, caller: &Caller) -> serde_json::Value {
    let slot = CoreSlot {
        name: "lead".into(),
        color: "blue",
        placed: None,
        debit: false,
    };
    let leader = Leader {
        team: "1111111111111111".into(),
        options: vec![],
        prompt: vec![],
    };
    let launch = prepare(ctx, caller, Client::Claude, &ctx.cwd, slot, &leader).unwrap();
    launch.create.unwrap()
}

pub fn prepare(
    ctx: &Ctx,
    caller: &Caller,
    client: Client,
    directory: &Path,
    slot: CoreSlot,
    leader: &Leader,
) -> Res<Launch> {
    let state = super::state_dir(ctx, caller)?;
    // Re6a call 12: only a launch on the anchor's own server binds the anchor's `server.json`.
    if super::scope::on_anchor(caller) {
        write_json(
            &state.join("server.json"),
            &json!({"socket":caller.socket,"serverPid":caller.server_pid,"serverStart":caller.server_start}),
        )?;
    }
    let (id, session) = (hex_id()?, guid()?);
    // The member folder and every folder above it are on disk before forward creates the record (#103).
    private_folder(ctx, &state.join("agents").join(&id))?;
    flush_to_root(&ctx.state_root, &state.join("agents"))?;
    // The prompt follows the recipe, so `--session-id <uuid>` ends any variadic option before it.
    let mut args = client_args(client, Recipe::First(None), &session, None);
    args.extend(leader.prompt.iter().cloned());
    let spec = json!({"schema": 1, "stateDirectory": utf8(&state)?, "client": client_name(client),
        "flags": leader.options, "parentPane": null, "args": args});
    let record = json!({"schema": 1, "name": slot.name, "color": slot.color,
        "client": client_name(client), "context": "brief", "reason": "leader",
        "session": super::scope::state(caller).session, "parentPane": null, "parentAgent": null, "depth": 0, "upTeam": leader.team,
        "options": leader.options, "prompt": leader.prompt,
        "directory": utf8(directory)?, "taskLine": "", "createdAt": utc_o(ctx.clock.now())});
    Ok(Launch {
        id,
        kind: "leader",
        session_id: session,
        create: Some(record),
        spec,
        team: super::membership::team(caller),
        name: slot.name,
        up_team: Some(leader.team.clone()),
        directory: utf8(directory)?.into(),
    })
}

/// Flushes the folders the record's path depends on before `hc run` (#103), as the launcher's `flush_claim` does for
/// its claim: `agents`, the server folder and the state root with `sync_dir`, then every folder above the root up to
/// the filesystem root with `sync_ancestor` (skipped only where this user cannot add an entry). Any failure is an
/// error, so nothing launches.
fn flush_to_root(root: &Path, agents: &Path) -> std::io::Result<()> {
    let depth = root.components().count();
    let mut dirs = agents.ancestors();
    for dir in dirs.by_ref() {
        sync_dir(dir)?;
        if dir.components().count() <= depth {
            break;
        }
    }
    for dir in dirs.filter(|d| !d.as_os_str().is_empty()) {
        sync_ancestor(dir)?;
    }
    Ok(())
}

/// restore-6c P4.3 (ambiguity 9; calls 17-19, 25): leader `id` (6a's `leader_of`), adjudicated by the pass just
/// before. The step applies when its active generation is Ended (`ended.json`, flushed read). It runs only for a
/// leader desired running: the pass ends at `Stopped` first, and an unreadable desired state fails the observation,
/// so the step never runs (row 8.16); `reopen_same` checks it again. Every generation must be done (`completing`
/// included) and the conversation free (no cap slot). Then n+1 of the same id, kind `leader-resume`, the resume
/// recipe, no prompt. A refusal is `Err`, which the pass reports as `leaderResume.refused` (6c-R5); `halted` passes.
pub(crate) fn resume_ended(
    tx: &crate::agents::team_state::Tx<'_>,
    lazy: &crate::hc::Lazy<'_>,
    id: &str,
) -> Res<Option<serde_json::Value>> {
    use crate::agents::{cascade, gens, reconcile, text};
    let folder = tx.folder(id);
    let gens::Member::Life(g) = gens::member_of(&folder) else {
        return Err(crate::state::evidence::unreadable());
    };
    let Some(n) = g.active() else { return Ok(None) };
    if gens::read_flushed(&folder, n, "ended")?.is_none() {
        return Ok(None);
    }
    // call 25 (5-9 F10): an Ended generation still `completing` is not done; the leader waits, nothing launches
    if !gens::done(&folder, n)? {
        return Ok(Some(json!({"held": "completing"})));
    }
    if g.pending().is_some() || !g.orphans()?.is_empty() {
        return Ok(Some(json!({"held": "older"})));
    }
    let session = text(g.record().get("sessionId"));
    if !crate::checks::session_id(&session) {
        return crate::error::refuse(1, "evidence", "the leader names no valid conversation");
    }
    let client = super::spawn::client_named(&text(g.record().get("client")))
        .ok_or_else(|| crate::error::Refusal::internal("the leader names no known client"))?;
    if let Some(hold) = super::occupancy::held(tx.ctx, &session, client, None) {
        return Ok(Some(json!({"held": "occupied", "holder": hold.message()})));
    }
    let n = cascade::reopen_same(tx, id, || {
        let next = reconcile::Next {
            kind: "leader-resume",
            session: &session,
            fresh: false,
        };
        reconcile::launch_next(tx, lazy.get()?, id, &g, next)
    })?;
    Ok(Some(json!({"resumed": {"generation": n}})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::fixture::World;
    use serde_json::Value;

    fn slot() -> CoreSlot {
        CoreSlot {
            name: "leader".into(),
            color: crate::paint::identity(None, "leader"),
            placed: None,
            debit: false,
        }
    }

    // The leader writes no record and no spec itself: forward does, after pending.
    #[test]
    fn a_leader_prepare_writes_no_record_and_no_spec() {
        let w = World::human("");
        let l = Leader {
            team: "0123456789abcdef".into(),
            options: vec![],
            prompt: vec!["go".into()],
        };
        let _launch = prepare(&w.ctx, &w.caller(), Client::Claude, &w.ctx.cwd, slot(), &l).unwrap();
        let agents = super::super::state_dir(&w.ctx, &w.caller())
            .unwrap()
            .join("agents");
        let written: Vec<String> = std::fs::read_dir(&agents)
            .unwrap()
            .flat_map(|e| std::fs::read_dir(e.unwrap().path()).unwrap())
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !written
                .iter()
                .any(|n| n == "record.json" || n == "spec.json"),
            "{written:?}"
        );
    }

    #[test]
    fn a_leader_record_is_flushed_with_no_parent_and_depth_zero() {
        let w = World::human("");
        let l = Leader {
            team: "0123456789abcdef".into(),
            options: vec!["--model".into(), "a b".into()],
            prompt: Vec::new(),
        };
        let p = prepare(&w.ctx, &w.caller(), Client::Claude, &w.ctx.cwd, slot(), &l).unwrap();
        let r = p.create.unwrap();
        assert_eq!(
            (
                &r["depth"],
                &r["parentPane"],
                &r["parentAgent"],
                &r["upTeam"]
            ),
            (&json!(0), &Value::Null, &Value::Null, &json!(l.team))
        );
        assert!(r.get("hcAddress").is_none());
        assert_eq!(
            (&p.spec["flags"], &p.spec["args"][0]),
            (&json!(["--model", "a b"]), &json!("--session-id"))
        );
        // The member folder is on disk, private, before forward writes the record.
        let agents = super::super::state_dir(&w.ctx, &w.caller())
            .unwrap()
            .join("agents");
        let folder = agents.join(&p.id);
        assert!(folder.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&folder).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "private");
        }
        crate::state::fs::fail_sync_dir(Some(&agents));
        let failed = prepare(&w.ctx, &w.caller(), Client::Claude, &w.ctx.cwd, slot(), &l);
        crate::state::fs::fail_sync_dir(None);
        assert!(failed.is_err(), "a failed folder flush launches nothing");
    }

    #[test]
    fn a_leader_records_options_and_prompt_apart() {
        let w = World::human("");
        let l = Leader {
            team: "0123456789abcdef".into(),
            options: vec!["--model".into(), "x".into()],
            prompt: vec!["fix".into()],
        };
        let p = prepare(&w.ctx, &w.caller(), Client::Claude, &w.ctx.cwd, slot(), &l).unwrap();
        let (r, spec) = (p.create.unwrap(), p.spec);
        assert_eq!(
            (&r["options"], &r["prompt"]),
            (&json!(["--model", "x"]), &json!(["fix"]))
        );
        assert!(r.get("flags").is_none());
        assert_eq!(spec["flags"], json!(["--model", "x"]));
        let args = spec["args"].as_array().unwrap();
        assert_eq!(
            (&args[0], args.last().unwrap()),
            (&json!("--session-id"), &json!("fix"))
        );
    }
}
