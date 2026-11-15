//! The leader `up` launches (#103): `<client> <flags> --session-id <uuid>`, depth 0, no parents, `upTeam`; its record
//! is flushed to disk before `hc run`, so a power loss never hides a leader; a failed flush launches nothing.

use std::path::Path;

use serde_json::json;

use super::core_launch::Prepared;
use super::spawn::{
    CoreSlot, Recipe, abort_evidence, client_args, client_name, private_folder, utf8,
};
use crate::{
    caller::Caller,
    ctx::Ctx,
    error::Res,
    parse::Client,
    state::fs::{sync_ancestor, sync_dir, write_json, write_json_durable},
    util::{guid, hex_id, utc_o},
};

pub struct Leader {
    pub team: String,
    /// The client options of the person (options-6e E1), byte for byte; a relaunch replays only these.
    pub options: Vec<String>,
    /// The prompt tokens of the person, byte for byte; run once, never replayed.
    pub prompt: Vec<String>,
}

pub fn prepare(
    ctx: &Ctx,
    caller: &Caller,
    client: Client,
    directory: &Path,
    slot: CoreSlot,
    leader: &Leader,
) -> Res<Prepared> {
    let state = super::state_dir(ctx, caller)?;
    write_json(
        &state.join("server.json"),
        &json!({"socket":caller.socket,"serverPid":caller.server_pid,"serverStart":caller.server_start}),
    )?;
    let id = hex_id()?;
    let folder = state.join("agents").join(&id);
    let run = || {
        private_folder(ctx, &folder)?;
        let (session, spec) = (guid()?, folder.join("spec.json"));
        // The prompt follows the recipe, so `--session-id <uuid>` ends any variadic option before it.
        let mut args = client_args(client, Recipe::First(None), &session, None);
        args.extend(leader.prompt.iter().cloned());
        write_json(
            &spec,
            &json!({"schema": 1, "stateDirectory": utf8(&state)?, "id": id, "client": client_name(client),
                "flags": leader.options, "parentPane": null,
                "args": args, "generation": "0"}),
        )?;
        let mut record = json!({"schema": 1, "id": id, "name": slot.name, "color": slot.color,
            "client": client_name(client), "sessionId": session, "context": "brief", "reason": "leader",
            "session": caller.session, "parentPane": null, "parentAgent": null, "depth": 0, "upTeam": leader.team,
            "options": leader.options, "prompt": leader.prompt,
            "directory": utf8(directory)?, "taskLine": "", "createdAt": utc_o(ctx.clock.now())});
        write_json_durable(&folder.join("record.json"), &record)?;
        flush_to_root(&ctx.state_root, &state.join("agents"))?;
        record["generation"] = "0".into();
        record["team"] = super::membership::team(caller).into();
        Ok(Prepared {
            folder: folder.clone(),
            record,
            argv: crate::launcher_scope::argv(ctx, &spec)?,
        })
    };
    run().map_err(|e| abort_evidence(&folder, e))
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

    #[test]
    fn a_leader_record_is_flushed_with_no_parent_and_depth_zero() {
        let w = World::human("");
        let l = Leader {
            team: "0123456789abcdef".into(),
            options: vec!["--model".into(), "a b".into()],
            prompt: Vec::new(),
        };
        let p = prepare(&w.ctx, &w.caller(), Client::Claude, &w.ctx.cwd, slot(), &l).unwrap();
        let r = crate::state::fs::read_json(&p.folder.join("record.json"))
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                &r["depth"],
                &r["parentPane"],
                &r["parentAgent"],
                &r["upTeam"]
            ),
            (&json!(0), &Value::Null, &Value::Null, &json!(l.team))
        );
        assert!(r.get("hcAddress").is_none() && !p.argv.is_empty());
        let spec = crate::state::fs::read_json(&p.folder.join("spec.json"))
            .unwrap()
            .unwrap();
        assert_eq!(
            (&spec["flags"], &spec["args"][0]),
            (&json!(["--model", "a b"]), &json!("--session-id"))
        );
        crate::state::fs::fail_durable_flush(Some(0));
        let failed = prepare(&w.ctx, &w.caller(), Client::Claude, &w.ctx.cwd, slot(), &l);
        crate::state::fs::fail_durable_flush(None);
        assert!(failed.is_err());
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
        let read = |f: &str| {
            crate::state::fs::read_json(&p.folder.join(f))
                .unwrap()
                .unwrap()
        };
        let (r, spec) = (read("record.json"), read("spec.json"));
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
