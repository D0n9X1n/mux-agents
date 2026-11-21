//! `ping <name>` asks a member for its status; `answer <pingId> -BodyFile <path>` is that member's reply.
//!
//! The ping is a record, `agents/<id>/ping.json` `{id, askedAt, by, generation, sent}`; the mail that tells the member
//! is ordinary team mail and decides nothing. A ping is answered when the member's own `answer.json` names its id, so
//! no clock decides it. A record of another generation than the member's current one is no ping of this member: a
//! resumed or respawned member never inherits it.
use std::path::Path;
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value, json};

use crate::caller::Caller;
use crate::cli::Request;
use crate::ctx::Ctx;
use crate::error::{Res, refuse};
use crate::util::{parse_utc, utc_o};

/// How long an open ping waits before the views flag `noAnswer`. The board's clock decides only when the flag shows,
/// never whether the ping was answered.
pub(crate) const PING_DEADLINE: Duration = Duration::from_secs(600);

/// The row fields `list` adds after every field it had before, in this order.
const LAST: [&str; 3] = ["statusAt", "pingedAt", "noAnswer"];

/// Whole seconds from `asked_at` to `now`; 0 when the clock is behind it (it moved backward).
pub(crate) fn age(asked_at: &str, now: SystemTime) -> u64 {
    parse_utc(asked_at)
        .and_then(|at| now.duration_since(at).ok())
        .map_or(0, |d| d.as_secs())
}

/// Whether an open ping asked at `asked_at` has waited `PING_DEADLINE` by `now`.
pub(crate) fn late(asked_at: &str, now: SystemTime) -> bool {
    age(asked_at, now) >= PING_DEADLINE.as_secs()
}

/// `row` with `statusAt`, `pingedAt` and `noAnswer` moved to its end, so every field before them keeps its place
/// (and its `-Text` column) whatever a later step added.
pub(crate) fn last(mut row: Map<String, Value>) -> Map<String, Value> {
    for key in LAST {
        if let Some(value) = row.shift_remove(key) {
            row.insert(key.into(), value);
        }
    }
    row
}

/// `list -Text`: each open ping's `pingedAt` as `pinged 4m`, or as `no answer 12m` once its row has `noAnswer`, aged
/// by `now`. The text renderer has no clock, so the age is put in here; the JSON keeps the time.
pub(crate) fn label(reply: &mut Map<String, Value>, now: SystemTime) {
    let Some(Value::Array(rows)) = reply.get_mut("agents") else {
        return;
    };
    for row in rows.iter_mut().filter_map(Value::as_object_mut) {
        let Some(at) = row
            .get("pingedAt")
            .and_then(Value::as_str)
            .map(String::from)
        else {
            continue;
        };
        let word = if row.get("noAnswer") == Some(&Value::Bool(true)) {
            "no answer"
        } else {
            "pinged"
        };
        let age = crate::sidebar::format_age(Some(&Value::from(age(&at, now))));
        row.insert("pingedAt".into(), format!("{word} {age}").into());
    }
}

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

/// Runs `f` with member `id`'s current `generation` (null when its record has none) under one hold of its member lock,
/// then its agent lock. Resume and recovery change `generation` only under the member lock
/// (`team_state::Tx::record_write`), so the generation `f` checks stays the member's until `f` returns; `status`,
/// `report` and the board read and write the files `f` writes under the agent lock. The order is the team lock (when
/// held), the member lock, then the agent lock; nothing takes them the other way round.
fn held<T>(ctx: &Ctx, caller: &Caller, id: &str, f: impl FnOnce(&Value) -> Res<T>) -> Res<T> {
    let folder = super::server_dir(ctx, caller).join("agents").join(id);
    super::lock(ctx, &super::lock_key("member", caller, id), || {
        let generation = match super::gens::member_of(&folder) {
            super::gens::Member::Pre(r) => r.get("generation").cloned(),
            super::gens::Member::Life(g) => g.record().get("generation").cloned(),
            super::gens::Member::Unreadable(e) => return Err(e),
            super::gens::Member::Absent => None,
        };
        let generation = generation.unwrap_or(Value::Null);
        super::lock(ctx, &super::lock_key("agent", caller, id), || {
            f(&generation)
        })
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
    crash_point("published")?;
    let notified = !p.open.sent;
    if notified {
        notify(ctx, caller, &p)?;
        crash_point("notified")?;
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

/// `answer <pingId> -BodyFile <path>`: the pinged member's own reply, never mail (a reply by `send -Re` cannot reach a
/// person and names no ping).
pub fn answer(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let Some(body_file) = request.value("BodyFile") else {
        return Err(super::usage("answer"));
    };
    let caller = crate::caller::resolve(ctx)?;
    answer_as(ctx, &caller, super::view::first(request), body_file)
}

/// `answer` for a resolved caller: only a running member itself (as `status` proves it), and only for the open ping
/// its own folder holds at its current generation. In one hold of its member, then agent lock (`held`) it checks that
/// ping, writes `status.json` (the doing line shows the answer), then `answer.json {ping, text, at}`, which closes the
/// ping; a retry after a crash between the two finds it still open.
pub(crate) fn answer_as(
    ctx: &Ctx,
    caller: &Caller,
    id: &str,
    body_file: &str,
) -> Res<Map<String, Value>> {
    let Some(me) = super::view::own_agent(ctx, caller)? else {
        return refuse(2, "target", "only a member answers a ping");
    };
    let raw = crate::state::fs::read_text(&ctx.cwd.join(body_file), crate::state::fs::BODY_MAX)?;
    let text = super::mail::doing_line(&raw);
    if text.is_empty() {
        return refuse(2, "usage", "the answer is empty");
    }
    let folder = super::state_dir(ctx, caller)?.join("agents").join(&me.key);
    let at = utc_o(ctx.clock.now());
    // The generation is read again here, under the locks: the one `own_agent` proved may have changed since.
    held(ctx, caller, &me.key, |generation| {
        if open(&folder, generation).is_none_or(|o| o.id != id) {
            return refuse(2, "ping", "no open ping with that id");
        }
        crate::state::fs::write_json(
            &folder.join("status.json"),
            &json!({"text": text, "at": at}),
        )?;
        crate::state::fs::write_json_durable(
            &folder.join("answer.json"),
            &json!({"ping": id, "text": text, "at": at}),
        )?;
        // An answer is work, as a status is: the member no longer waits.
        super::message::prune_leases(ctx, &folder);
        Ok(())
    })?;
    let mut reply = Map::new();
    reply.insert("name".into(), me.name.into());
    reply.insert("ping".into(), id.into());
    reply.insert("text".into(), text.into());
    Ok(reply)
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
thread_local! {
    static CRASH_AFTER: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}

/// Test only: until reset with `None`, every ping on this thread stops right after `point` (`published`: the record is
/// durable, no notice sent; `notified`: the notice is delivered, the record not yet `sent`), as a crash at that boundary
/// would.
#[cfg(test)]
pub fn crash_after(point: Option<&'static str>) {
    CRASH_AFTER.with(|c| c.set(point));
}

#[cfg_attr(not(test), allow(unused_variables))]
fn crash_point(point: &str) -> Res<()> {
    #[cfg(test)]
    if CRASH_AFTER.with(|c| c.get()) == Some(point) {
        return Err(crate::error::Refusal::internal("crash after a ping step"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
