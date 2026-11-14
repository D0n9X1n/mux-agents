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
        leftover: None,
    }
}
pub(crate) fn named(ctx: &Ctx, caller: &Caller, name: &str) -> Res<Identity> {
    named_with_core(ctx, caller, name, None)
}
pub(crate) fn named_with_core(
    ctx: &Ctx,
    caller: &Caller,
    name: &str,
    core: Option<&crate::hc::Core<'_>>,
) -> Res<Identity> {
    if !super::agent_name(name) {
        return Err(changed());
    }
    let local = membership::locals(ctx, caller)?;
    let superseded = membership::superseded_locals(&local);
    let found: Vec<_> = local
        .into_iter()
        .filter(|l| l.identity.name == name && l.stored.committed)
        .filter(|l| !superseded.contains(&l.identity.id))
        .collect();
    match found.as_slice() {
        [] => return Err(changed()),
        [one] => return Ok(one.identity.clone()),
        _ => {}
    }
    let addresses: std::collections::BTreeSet<_> =
        found.iter().map(|l| &l.identity.address).collect();
    let connected;
    let core = match core {
        Some(core) => core,
        None => {
            connected = crate::hc::connect(ctx)?;
            &connected
        }
    };
    if addresses.len() != found.len() {
        return Err(changed());
    }
    let mut live = Vec::new();
    for member in &found {
        let id = &member.identity;
        let fact = core.get(&id.address).ok().filter(|t| id.matches(t));
        if super::lifecycle::classify(fact.as_ref(), &id.launcher) != super::lifecycle::Life::Ended
        {
            live.push(id);
        }
    }
    match live.as_slice() {
        [one] => Ok((*one).clone()),
        [] => found
            .iter()
            .max_by_key(|l| {
                (
                    l.stored
                        .record
                        .get("createdAt")
                        .and_then(Value::as_str)
                        .and_then(crate::util::parse_utc),
                    std::cmp::Reverse(l.identity.id.as_str()),
                )
            })
            .map(|l| l.identity.clone())
            .ok_or_else(changed),
        _ => Err(changed()),
    }
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

#[cfg(test)]
#[path = "member_target_tests.rs"]
mod tests;
