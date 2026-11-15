use super::{core_identity::Identity, membership};
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
        details: Default::default(),
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
    let mut live_ids = Vec::new();
    for member in &found {
        let id = &member.identity;
        // R3: a lifecycle candidate's end is its record's; hc decides only for a pre-lifecycle one.
        let live = match member.finished {
            Some(finished) => !finished,
            None => {
                let fact = core.get(&id.address).ok().filter(|t| id.matches(t));
                super::lifecycle::classify(fact.as_ref(), &id.launcher)
                    != super::lifecycle::Life::Ended
            }
        };
        if live {
            live_ids.push(id);
        }
    }
    match live_ids.as_slice() {
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
/// The committed, unsuperseded member whose id is `id`: the bar opens rows by id, never by name (#103 B6).
pub(crate) fn by_id(ctx: &Ctx, caller: &Caller, id: &str) -> Res<Identity> {
    if !super::agent_id(id) {
        return Err(changed());
    }
    let local = membership::locals(ctx, caller)?;
    let superseded = membership::superseded_locals(&local);
    local
        .into_iter()
        .find(|l| l.identity.id == id && l.stored.committed && !superseded.contains(&l.identity.id))
        .map(|l| l.identity)
        .ok_or_else(changed)
}
/// R1, R4: one `member_of` and one `status`; the identity and record of the subject generation, built by
/// `membership::subject_of` as membership built the identity `by_id` or `named` returned, so they agree.
pub(crate) fn current(ctx: &Ctx, caller: &Caller, id: &Identity) -> Res<Value> {
    use super::gens::Member;
    let folder = super::server_dir(ctx, caller).join("agents").join(&id.id);
    let team = membership::team(caller);
    let (record, identity, committed) = match super::gens::member_of(&folder) {
        Member::Absent => return Err(changed()),
        Member::Unreadable(e) => return Err(e),
        Member::Pre(r) => {
            let identity = Identity::read(&r, &id.id, &team);
            (r, identity, super::folder_committed(&folder))
        }
        Member::Life(g) => {
            let s = g.status()?; // R2: an unreadable marker refuses
            if s.subject.is_none() {
                return Err(changed());
            }
            let (identity, record) = membership::subject_of(&g, &s, &id.id, &team)?;
            (record, Some(identity), s.committed)
        }
    };
    if !record.is_object() || !committed || identity.as_ref() != Some(id) {
        return Err(changed());
    }
    Ok(record)
}

#[cfg(test)]
#[path = "member_target_tests.rs"]
mod tests;
