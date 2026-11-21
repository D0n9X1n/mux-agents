//! `ping <name>` asks a member for its status; `answer <pingId> -BodyFile <path>` is that member's reply.
//!
//! The ping is a record, `agents/<id>/ping.json` `{id, askedAt, by, generation, sent}`; the mail that tells the member
//! is ordinary team mail and decides nothing. A ping is answered when the member's own `answer.json` names its id, so
//! no clock decides it. A record of another generation than the member's current one is no ping of this member: a
//! resumed or respawned member never inherits it.
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::caller::Caller;
use crate::cli::Request;
use crate::ctx::Ctx;
use crate::error::{Res, refuse};
use crate::util::{parse_utc, utc_o};

/// A member's open ping: its `ping.json` at the member's current `generation`, unless its `answer.json` names that
/// ping. An unreadable or malformed record is none: the views never fail for it, and `ping` publishes a new one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Open {
    pub id: String,
    pub asked_at: String,
    pub sent: bool,
}

/// Member folder `folder`'s open ping at `generation` (the member record's `generation`, null when it has none). The
/// caller holds the member's agent lock, under which `ping` and `answer` write both files.
pub(crate) fn open(folder: &Path, generation: &Value) -> Option<Open> {
    let read = |name: &str| {
        crate::state::fs::read_json(&folder.join(name))
            .ok()
            .flatten()
    };
    let ping = read("ping.json")?;
    let id = ping
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())?;
    let asked_at =
        (ping.get("askedAt").and_then(Value::as_str)).filter(|at| parse_utc(at).is_some())?;
    if ping.get("generation").unwrap_or(&Value::Null) != generation {
        return None;
    }
    let answered =
        read("answer.json").is_some_and(|a| a.get("ping").and_then(Value::as_str) == Some(id));
    (!answered).then(|| Open {
        id: id.into(),
        asked_at: asked_at.into(),
        sent: ping.get("sent") == Some(&Value::Bool(true)),
    })
}

/// What `ping` published (or found open) for one member.
struct Published {
    name: String,
    member: String,
    folder: std::path::PathBuf,
    generation: Value,
    open: Open,
}

/// `ping <name>`: the team's leader or the person asks live member `<name>` for its status.
pub fn ping(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let caller = crate::caller::resolve_tree(ctx)?;
    ping_as(ctx, &caller, super::view::first(request))
}

/// `ping <name>` for a resolved caller: publish (or find) the record under the team and then the member's agent lock;
/// with no lock held, send the notice when the record is not yet `sent`; then mark it `sent` under the agent lock.
pub(crate) fn ping_as(ctx: &Ctx, caller: &Caller, name: &str) -> Res<Map<String, Value>> {
    let p = publish(ctx, caller, name)?;
    let notified = !p.open.sent;
    if notified {
        notify(ctx, caller, &p)?;
        confirm(ctx, caller, &p)?;
    }
    let mut reply = Map::new();
    reply.insert("name".into(), p.name.into());
    reply.insert("id".into(), p.open.id.into());
    reply.insert("askedAt".into(), p.open.asked_at.into());
    reply.insert("notified".into(), notified.into());
    Ok(reply)
}

fn publish(ctx: &Ctx, caller: &Caller, name: &str) -> Res<Published> {
    super::tree::with_standing(ctx, caller, |standing| {
        let Some(standing) = standing else {
            return refuse(2, "team", "ping works only inside a team");
        };
        let by = match standing {
            super::tree::Standing::Member(m) if super::tree::leads(ctx, caller, m)? => {
                Value::from(m.id.clone())
            }
            super::tree::Standing::Member(_) => {
                return refuse(
                    2,
                    "authority",
                    format!("only the team's leader or a person can ping {name}"),
                );
            }
            super::tree::Standing::Person => Value::Null,
        };
        let s = super::membership::Snapshot::take(ctx, caller)?;
        if let Some(e) = &s.core_error {
            return Err(e.clone());
        }
        let Some((id, record)) = s.running_named(name) else {
            return refuse(2, "target", super::team_mail::NO_NAME);
        };
        let generation = record.get("generation").cloned().unwrap_or(Value::Null);
        let folder = super::server_dir(ctx, caller).join("agents").join(&id);
        let open = super::lock(ctx, &super::lock_key("agent", caller, &id), || {
            if let Some(open) = open(&folder, &generation) {
                return Ok(open);
            }
            let open = Open {
                id: super::team_mail::new_id()?,
                asked_at: utc_o(ctx.clock.now()),
                sent: false,
            };
            crate::state::fs::write_json_durable(
                &folder.join("ping.json"),
                &json!({"id": open.id, "askedAt": open.asked_at, "by": by, "generation": generation,
                    "sent": false}),
            )?;
            Ok(open)
        })?;
        Ok(Published {
            name: name.into(),
            member: id,
            folder,
            generation,
            open,
        })
    })
}

/// The notice: ordinary team mail to the member by name, with no lock held, so the send takes its own team, then
/// agent lock order.
fn notify(ctx: &Ctx, caller: &Caller, p: &Published) -> Res<()> {
    let body = format!(
        "Status? Answer with: mux-agent answer {} -BodyFile <file: doing / done / blocked on what>",
        p.open.id
    );
    let request = Request {
        verb: "send",
        values: Vec::new(),
        switches: Vec::new(),
        positional: vec![p.name.clone()],
        text: false,
        rest: Vec::new(),
    };
    match super::team_mail::route(ctx, caller, &request, &body)? {
        super::team_mail::Routed::Sent(_) => Ok(()),
        super::team_mail::Routed::Legacy(_) => refuse(2, "target", super::team_mail::NO_NAME),
    }
}

/// Marks the record `sent` only while it is still this ping of this generation: one replaced meanwhile (a newer ping,
/// a new generation) is left as it is.
fn confirm(ctx: &Ctx, caller: &Caller, p: &Published) -> Res<()> {
    super::lock(ctx, &super::lock_key("agent", caller, &p.member), || {
        let file = p.folder.join("ping.json");
        let Some(mut record) = crate::state::fs::read_json(&file).ok().flatten() else {
            return Ok(());
        };
        let own = record.get("id").and_then(Value::as_str) == Some(p.open.id.as_str())
            && record.get("generation").unwrap_or(&Value::Null) == &p.generation;
        if own {
            record["sent"] = true.into();
            crate::state::fs::write_json_durable(&file, &record)?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests;
