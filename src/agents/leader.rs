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
    state::fs::{sync_dir, write_json, write_json_durable},
    util::{guid, hex_id, utc_o},
};

pub struct Leader {
    pub team: String,
    /// the person's client flags, byte for byte
    pub flags: Vec<String>,
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
        write_json(
            &spec,
            &json!({"schema": 1, "stateDirectory": utf8(&state)?, "id": id, "client": client_name(client),
                "flags": leader.flags, "parentPane": null,
                "args": client_args(client, Recipe::First(None), &session, None), "generation": "0"}),
        )?;
        let mut record = json!({"schema": 1, "id": id, "name": slot.name, "color": slot.color,
            "client": client_name(client), "sessionId": session, "context": "brief", "reason": "leader",
            "session": caller.session, "parentPane": null, "parentAgent": null, "depth": 0, "upTeam": leader.team,
            "directory": utf8(directory)?, "taskLine": "", "createdAt": utc_o(ctx.clock.now())});
        write_json_durable(&folder.join("record.json"), &record)?;
        sync_dir(&state.join("agents"))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::fixture::World;
    use serde_json::Value;

    fn slot() -> CoreSlot {
        CoreSlot {
            name: "leader".into(),
            color: crate::paint::identity(None, "leader"),
        }
    }

    #[test]
    fn a_leader_record_is_flushed_with_no_parent_and_depth_zero() {
        let w = World::human("");
        let l = Leader {
            team: "0123456789abcdef".into(),
            flags: vec!["--model".into(), "a b".into()],
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
}
