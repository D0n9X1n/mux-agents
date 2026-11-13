use super::{core_identity::Identity, membership, registry};
use crate::{
    caller::Caller,
    ctx::Ctx,
    error::{Refusal, Res},
};
use serde_json::Value;
fn changed() -> Refusal {
    Refusal {
        code: 2,
        check: "target",
        message: "no unique committed member has that identity".into(),
        reason: None,
    }
}
pub(crate) fn named(ctx: &Ctx, caller: &Caller, name: &str) -> Res<Identity> {
    if !super::agent_name(name) {
        return Err(changed());
    }
    let mut found = membership::locals(ctx, caller)?
        .into_iter()
        .filter(|l| l.identity.name == name && l.stored.committed);
    let first = found.next().ok_or_else(changed)?;
    if found.next().is_some() {
        return Err(changed());
    }
    Ok(first.identity)
}
pub(crate) fn current(ctx: &Ctx, caller: &Caller, id: &Identity) -> Res<Value> {
    let record = registry::record(ctx, caller, &id.id)?.ok_or_else(changed)?;
    let folder = super::server_dir(ctx, caller).join("agents").join(&id.id);
    if !super::committed(&folder)
        || Identity::read(&record, &id.id, &membership::team(caller)).as_ref() != Some(id)
    {
        return Err(changed());
    }
    Ok(record)
}
