use super::core_identity::Identity;
use crate::{
    caller::Caller,
    ctx::Ctx,
    error::{Refusal, Res},
};
use serde_json::Value;

#[derive(Debug)]
pub struct Stored {
    pub record: Value,
    pub committed: bool,
    pub noticed: bool,
}

#[derive(Debug)]
pub struct Local {
    pub identity: Identity,
    pub stored: Stored,
}

pub fn team(caller: &Caller) -> String {
    format!("{}:{}", caller.server_key, caller.session)
}

pub fn locals(ctx: &Ctx, caller: &Caller) -> Res<Vec<Local>> {
    let entries = match std::fs::read_dir(super::server_dir(ctx, caller).join("agents")) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut local = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !super::agent_id(&id) || !entry.file_type()?.is_dir() {
            continue;
        }
        let folder = entry.path();
        let Some(record) = crate::state::fs::read_json(&folder.join("record.json"))? else {
            continue;
        };
        if !record.is_object() {
            return Err(Refusal::internal("invalid local hc record"));
        }
        if !["hcAddress", "generation", "team", "launcherArgv"]
            .iter()
            .any(|k| record.get(k).is_some())
        {
            continue;
        }
        if record
            .get("session")
            .and_then(Value::as_str)
            .is_some_and(|s| s != caller.session)
        {
            continue;
        }
        let identity = Identity::read(&record, &id, &team(caller))
            .ok_or_else(|| Refusal::internal("invalid local hc identity"))?;
        local.push(Local {
            identity,
            stored: Stored {
                record,
                committed: super::committed(&folder),
                noticed: folder.join("notice.json").exists(),
            },
        });
    }
    local.sort_by(|a, b| a.identity.id.cmp(&b.identity.id));
    Ok(local)
}

#[cfg(test)]
#[path = "membership_tests.rs"]
mod tests;
