use super::core_identity::Identity;
use super::{core_facts, registry};
use crate::hc;
use crate::hc::Core;
use crate::{
    caller::Caller,
    ctx::Ctx,
    error::{Refusal, Res},
};
use serde_json::Value;
use std::collections::BTreeMap;

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

#[derive(Debug)]
pub struct Snapshot {
    pub facts: core_facts::Snapshot,
    pub records: BTreeMap<String, Stored>,
    pub views: BTreeMap<String, registry::Pane>,
    pub core_error: Option<Refusal>,
    pub view_error: Option<Refusal>,
}

impl Snapshot {
    pub fn take(ctx: &Ctx, caller: &Caller) -> Res<Self> {
        let local = locals(ctx, caller)?;
        let ids: Vec<_> = local.iter().map(|l| l.identity.clone()).collect();
        let result = hc::connect(ctx).and_then(|core| core_facts::take(&core, &team(caller), &ids));
        let (facts, error) = match result {
            Ok(facts) => (facts, None),
            Err(e) => (
                core_facts::Snapshot {
                    local: ids
                        .into_iter()
                        .map(|identity| core_facts::Observation {
                            identity,
                            terminal: None,
                            life: super::lifecycle::Life::Unknown,
                        })
                        .collect(),
                    reserved: Vec::new(),
                },
                Some(e),
            ),
        };
        Ok(Self::assemble(ctx, caller, local, facts, error))
    }

    pub fn for_spawn(ctx: &Ctx, caller: &Caller, core: &Core<'_>) -> Res<Self> {
        let local = locals(ctx, caller)?;
        let ids: Vec<_> = local.iter().map(|l| l.identity.clone()).collect();
        let facts = core_facts::take(core, &team(caller), &ids)?;
        Ok(Self::assemble(ctx, caller, local, facts, None))
    }

    fn assemble(
        ctx: &Ctx,
        caller: &Caller,
        local: Vec<Local>,
        facts: core_facts::Snapshot,
        core_error: Option<Refusal>,
    ) -> Self {
        let (rows, view_error) = match registry::rows(ctx, caller) {
            Ok(rows) => (rows, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        let mut views = BTreeMap::new();
        let mut records = BTreeMap::new();
        for l in local {
            if let Some(view) = l.identity.view(&l.stored.record, &rows) {
                views.insert(l.identity.id.clone(), view);
            }
            records.insert(l.identity.id, l.stored);
        }
        Self {
            facts,
            records,
            views,
            core_error,
            view_error,
        }
    }
}

#[cfg(test)]
#[path = "membership_tests.rs"]
mod tests;
