//! send, read and stop (MuxAgents.ps1:1298-1333, 1354-1421).

use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value, json};

use super::mail::{add_mail, doing_line, message_text};
use super::registry::{self, Notice, add_notice, self_of};
use super::view::first;
use super::{FILES_ONLY, lock, lock_key, pane_id, server_dir, text, usage};
use crate::caller::{Caller, Kind, resolve};
use crate::cli::Request;
use crate::ctx::Ctx;
use crate::error::{Res, refuse};
use crate::mux::native;
use crate::state::fs::{BODY_MAX, read_json, read_text, write_json};

/// Appends a message to a live agent's, the parent's or a session pane's mailbox. Never types into a pane.
pub fn send(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    if request.value("Body").is_some() == request.value("BodyFile").is_some() {
        return Err(usage("send"));
    }
    let caller = resolve(ctx)?;
    send_as(ctx, &caller, request)
}

/// `send` for a resolved caller: the team path first (#103 4a), else 0.2.
pub fn send_as(
    ctx: &Ctx,
    caller: &crate::caller::Caller,
    request: &Request,
) -> Res<Map<String, Value>> {
    let (body, body_file) = (request.value("Body"), request.value("BodyFile"));
    if body.is_some() == body_file.is_some() {
        return Err(usage("send"));
    }
    let body = match (body, body_file) {
        (Some(_), _) if caller.kind == Kind::Agent => return refuse(2, "usage", FILES_ONLY),
        (Some(body), _) => body.to_string(),
        (None, file) => read_text(&ctx.cwd.join(file.unwrap_or_default()), BODY_MAX)?,
    };
    let body = message_text(&body);
    if body.len() as u64 > BODY_MAX {
        return refuse(2, "usage", "the message is larger than 64 KB");
    }
    match super::team_mail::route(ctx, caller, request, &body)? {
        super::team_mail::Routed::Sent(reply) => Ok(reply),
        super::team_mail::Routed::Legacy(members) => {
            send_legacy(ctx, caller, request, &body, members.map(|m| *m))
        }
    }
}

/// The pane must be a live pane of this session (0.2): one display-message probe.
pub(crate) fn live_pane(ctx: &Ctx, caller: &crate::caller::Caller, pane: &str) -> Res<()> {
    let probe = native(ctx, &["display-message", "-p", "-t", pane, RECIPIENT], true)?;
    let line = probe.stdout.trim_end_matches(['\r', '\n']);
    let live = probe.code == 0
        && line.split_once('|').is_some_and(|(at, session)| {
            at == pane && session == caller.session && pane_session(session)
        });
    if !live {
        return refuse(
            2,
            "target",
            "the recipient is not a live pane of this session",
        );
    }
    Ok(())
}

/// The 0.2 send: a non-member agent pane, or a person naming what the team lacks. `members` is the membership
/// snapshot the team path already took, if any.
fn send_legacy(
    ctx: &Ctx,
    caller: &crate::caller::Caller,
    request: &Request,
    body: &str,
    members: Option<super::membership::Snapshot>,
) -> Res<Map<String, Value>> {
    let target = first(request);
    // A malformed name refuses before any pane is listed.
    if target != "parent" && !pane_id(target) && !super::agent_name(target) {
        return refuse(2, "target", "no live agent has that name");
    }
    // One snapshot names the sender and the recipient; a tag never names either.
    let snapshot = registry::Snapshot::take(ctx, caller)?;
    let me = match super::membership::proven_self(ctx, caller) {
        Some(me) => me?,
        None => self_of(caller, &snapshot)?,
    };
    // Closing a view never stops an agent (#104): a member hc shows running is reachable by name with no live view;
    // its mailbox is its id and no pane is probed for it.
    // When hc cannot be read, that is the answer, never a false "no live agent".
    // A pane found by name is an hc member's only while it is the current member of that name (committed, not
    // superseded, hc running): the pane fast path stands when the member rule picks that same member; otherwise the
    // rule's own answer is used, and none refuses. A pane-only (pre-hc) record keeps its pane path.
    let by_name = target != "parent" && !pane_id(target);
    let fast = by_name.then(|| snapshot.find(target)).flatten();
    let viewless = if by_name
        && fast
            .as_ref()
            .is_none_or(|a| a.record.get("hcAddress").is_some())
    {
        let members = match members {
            Some(m) => m,
            None => super::membership::Snapshot::take(ctx, caller)?,
        };
        if let Some(error) = members.core_error {
            return Err(error);
        }
        match (members.running_named(target), &fast) {
            (Some((id, _)), Some(agent)) if id == agent.id => None,
            (Some(found), _) => Some(found),
            (None, Some(_)) => return refuse(2, "target", "no live agent has that name"),
            (None, None) => None,
        }
    } else {
        None
    };
    let (pane, key, to_session) = match viewless {
        Some((id, record)) => {
            let session = record
                .get("sessionId")
                .and_then(Value::as_str)
                .filter(|s| crate::checks::session_id(s))
                .filter(|_| record.get("client").and_then(Value::as_str) == Some("claude"))
                .map(String::from);
            let key = super::mail::member_mailbox(&id, &record);
            (text(record.get("pane")), key, session)
        }
        None => {
            let pane = if target == "parent" {
                match &me.record {
                    Some(record) => text(record.get("parentPane")),
                    None => return refuse(2, "parent", "only an agent has a parent"),
                }
            } else if pane_id(target) {
                target.to_string()
            } else {
                if !super::agent_name(target) {
                    return refuse(2, "target", "no live agent has that name");
                }
                match snapshot.find(target) {
                    Some(agent) => agent.pane,
                    None => return refuse(2, "target", "no live agent has that name"),
                }
            };
            // The pane must be a live pane of this session; its mailbox is the running agent's there, else `pane-<n>`.
            live_pane(ctx, caller, &pane)?;
            let key = registry::mailbox_of(&snapshot, &pane)?;
            // The recipient's Claude Code session, when it is one: a running Claude agent's own, or (to `parent`) the session
            // its spawner recorded. The sender's hooks wake it with it (#92).
            let claude = |record: &Value, field: &str| {
                let id = record.get(field).and_then(Value::as_str)?;
                crate::checks::session_id(id).then(|| id.to_string())
            };
            let to_session = match snapshot.at(&pane) {
                Some(agent)
                    if agent.record.get("client").and_then(Value::as_str) == Some("claude") =>
                {
                    claude(&agent.record, "sessionId")
                }
                Some(_) => None,
                None if target == "parent" => me
                    .record
                    .as_ref()
                    .and_then(|r| r.get("parentAgent"))
                    .filter(|p| p.get("client").and_then(Value::as_str) == Some("claude"))
                    .and_then(|p| claude(p, "sessionId")),
                None => None,
            };
            (pane, key, to_session)
        }
    };
    let deliver = || {
        // A member's line names the member it came from (#103 review: a team bar counts it by identity).
        let id = match &me.record {
            Some(_) => super::mail::add_member_mail(
                ctx,
                caller,
                &key,
                &me.name,
                &me.key,
                &caller.pane,
                body,
            )?,
            None => add_mail(ctx, caller, &key, &me.name, &caller.pane, body, None)?,
        };
        if me.record.is_some() {
            // Best effort, inside the sender lock, so last-sent.json names the last appended message.
            let file = server_dir(ctx, caller)
                .join("agents")
                .join(&me.key)
                .join("last-sent.json");
            let at = crate::util::utc_o(ctx.clock.now());
            let _ = write_json(&file, &json!({"line": doing_line(body), "at": at}));
            // A message sent is the agent's step done: it no longer waits.
            if let Some(folder) = file.parent() {
                prune_leases(ctx, folder);
            }
        }
        Ok(id)
    };
    // Lock order: the sender's agent lock, then (inside add_mail) the recipient's mail lock.
    let id = match me.record {
        Some(_) => lock(ctx, &lock_key("agent", caller, &me.key), deliver)?,
        None => deliver()?,
    };
    let mut reply = Map::new();
    reply.insert("to".into(), target.into());
    reply.insert("pane".into(), pane.into());
    reply.insert("id".into(), id.into());
    reply.insert("from".into(), me.name.clone().into());
    reply.insert(
        "toSessionId".into(),
        to_session.map_or(Value::Null, Value::from),
    );
    Ok(reply)
}

const RECIPIENT: &str = "#{pane_id}|#{session_id}";

fn pane_session(text: &str) -> bool {
    text.strip_prefix('$')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// .NET's int.TryParse: white space around, one sign, ASCII digits, in range.
fn int(text: &str) -> Option<i32> {
    let text = text.trim_matches(|c| matches!(c, '\t'..='\r' | ' '));
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

// A flag's value within bounds, its default when absent, `None` when malformed.
fn bounded(request: &Request, name: &str, default: i32, range: RangeInclusive<i32>) -> Option<i32> {
    match request.value(name) {
        None => Some(default),
        Some(value) => int(value).filter(|n| range.contains(n)),
    }
}

pub(crate) const NOTIFY_OPTION: &str = "@mux-agents-notify-send";
pub const DRAFT_WARNING: &str = "warning: hc's draft hold expires; a notice may land in a draft";

/// `notify -On|-Off` (W7): a person only (else 2 `authority`) sets the option on the team session.
pub fn notify(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    notify_as(ctx, &resolve(ctx)?, request, &mut |text: &str| {
        eprintln!("{text}")
    })
}

/// `warn` gets the draft warning before the option is on (F11); a failed set-option refuses 1 `notify` (F12).
pub fn notify_as(
    ctx: &Ctx,
    caller: &crate::caller::Caller,
    request: &Request,
    warn: &mut dyn FnMut(&str),
) -> Res<Map<String, Value>> {
    let (on, off) = (request.switch("On"), request.switch("Off"));
    if on == off {
        return Err(usage("notify"));
    }
    if caller.member.is_some() || caller.kind == Kind::Agent {
        return refuse(2, "authority", "only a person sets notices");
    }
    let value = if on { "on" } else { "off" };
    // Under the team lock (a send reads the option under it too): the warning first, then the setting (W7).
    lock(ctx, &lock_key("spawn", caller, &caller.session), || {
        if on {
            warn(DRAFT_WARNING);
        }
        let saved = native(
            ctx,
            &["set-option", "-t", &caller.session, NOTIFY_OPTION, value],
            true,
        )
        .is_ok_and(|o| o.code == 0);
        if !saved {
            return refuse(1, "notify", "the notice setting could not be saved");
        }
        Ok(())
    })?;
    let mut reply = Map::new();
    if on {
        reply.insert("warning".into(), DRAFT_WARNING.into());
    }
    reply.insert("notify".into(), value.into());
    Ok(reply)
}

/// `read [-Wait <s>]`: the caller's unread messages, waiting up to 600 s for one. `read <name> [-Lines n]`: the
/// pane's tail.
pub fn read(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let (Some(wait), Some(lines)) = (
        bounded(request, "Wait", 0, 0..=600),
        bounded(request, "Lines", 60, 1..=2000),
    ) else {
        return Err(usage("read"));
    };
    let named = !request.positional.is_empty();
    let (ack, id, deliver, count, cancel) = (
        request.value("Ack"),
        request.value("Id"),
        request.switch("Deliver"),
        request.switch("Count"),
        request.switch("Cancel"),
    );
    let hex = |v: &str| {
        (8..=32).contains(&v.len())
            && v.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    };
    let modes = [
        request.value("Wait").is_some(),
        deliver,
        count,
        id.is_some(),
        cancel,
    ]
    .into_iter()
    .filter(|m| *m)
    .count();
    if (named
        && (request.value("Wait").is_some()
            || ack.is_some()
            || deliver
            || count
            || cancel
            || id.is_some()))
        || (!named && request.value("Lines").is_some())
        || modes > 1
        || (count && ack.is_some())
        || (cancel && ack.is_some())
        || [ack, id].into_iter().flatten().any(|v| !hex(v))
    {
        return Err(usage("read"));
    }
    let mut reply = Map::new();
    if named {
        let caller = crate::caller::resolve_tree(ctx)?;
        let (pane, tail) = super::tree::with_standing(ctx, &caller, |s| {
            capture_hc(ctx, &caller, first(request), None, lines, s)
        })?;
        reply.insert("name".into(), pane.name.into());
        reply.insert("pane".into(), pane.pane.into());
        reply.insert("text".into(), tail.into());
        return Ok(reply);
    }
    let caller = resolve(ctx)?;
    // One membership snapshot names the reader and serves the first reconciliation.
    let snapshot = super::membership::Snapshot::take(ctx, &caller)?;
    let me = snapshot.reader(&caller)?;
    let mailbox = match &me.record {
        Some(record) => super::mail::member_mailbox(&me.key, record),
        None => me.key.clone(),
    };
    // A member reads behind the fence; a person's pane mailbox has no member to supersede.
    let identity = me.record.as_ref().and_then(|_| {
        snapshot
            .facts
            .local
            .iter()
            .find(|o| o.identity.id == me.key)
            .map(|o| o.identity.clone())
    });
    // PM call 8, P2: the owner the snapshot selected, never a second read of the folder.
    let owner = snapshot_owner(&me.key, identity.as_ref(), me.record.as_ref());
    let fence = || identity.as_ref().is_none_or(|i| current(ctx, &caller, i));
    let folder = server_dir(ctx, &caller).join("agents").join(&me.key);
    if cancel {
        if me.record.is_none() {
            return refuse(2, "target", "only an agent listens on call");
        }
        let end = super::mail::counts(ctx, &caller, &mailbox)?.end;
        lock(ctx, &lock_key("agent", &caller, &me.key), || {
            write_json(
                &folder.join("oncall-cancel.json"),
                &json!({"owner": {"id": owner.id, "gen": owner.generation}, "end": end}),
            )
        })?;
        reply.insert("cancelled".into(), true.into());
        return Ok(reply);
    }
    if count {
        let c = super::mail::counts(ctx, &caller, &mailbox)?;
        for (k, v) in [
            ("badge", json!(c.badge)),
            ("wake", json!(c.wake)),
            ("end", json!(c.end)),
            ("id", json!(owner.id)),
            ("gen", json!(owner.generation)),
            (
                "leader",
                json!(me.record.as_ref().is_some_and(super::tree::is_leader)),
            ),
            ("member", json!(me.record.is_some())),
        ] {
            reply.insert(k.into(), v);
        }
        return Ok(reply);
    }
    if let Some(batch) = ack {
        let done = super::mail::ack(ctx, &caller, &mailbox, &owner, batch, &fence)?
            == super::mail::Acked::Done;
        reply.insert("acked".into(), done.into());
        if !done {
            reply.insert("duplicate".into(), true.into());
        }
        if !(deliver || id.is_some() || request.value("Wait").is_some()) {
            return Ok(reply);
        }
    }
    // Removed on every return, mail, timeout or error, when this guard drops: after the reply is built.
    let _waiting = match &me.record {
        Some(_) if wait > 0 && id.is_none() => {
            Waiting::start(ctx, &caller, &me.key, &owner, wait as u64)
        }
        _ => None,
    };
    let served = match id {
        Some(id) => super::mail::serve_id(ctx, &caller, &mailbox, &owner, id, &fence)?,
        None => {
            let deadline = ctx.clock.now() + Duration::from_secs(wait as u64);
            let mut first_pass = Some(snapshot);
            loop {
                if wait > 0 && cancelled(ctx, &caller, &folder, &owner, &mailbox)? {
                    for (k, v) in [
                        ("cancelled", json!(true)),
                        ("batch", Value::Null),
                        ("ids", json!([])),
                        ("text", json!("")),
                    ] {
                        reply.insert(k.into(), v);
                    }
                    return Ok(reply);
                }
                let snapshot = match first_pass.take() {
                    Some(s) => s,
                    None => super::membership::Snapshot::take(ctx, &caller)?,
                };
                registry::reap_hc(ctx, &caller, &snapshot)?;
                let r = super::mail::serve(ctx, &caller, &mailbox, &owner, &fence)?;
                if r.is_some() || ctx.clock.now() >= deadline {
                    break r;
                }
                // File-watch waits are a non-goal; this polls as the reference did.
                ctx.clock.sleep(POLL);
            }
        }
    };
    // Computed before `served` is moved by the match (E0382); SKILL.md re-arms on it (W5).
    let timed_out = served.is_none() && wait > 0;
    match served {
        Some(r) => {
            // F2: `-Id` of another held message re-serves the receipt in flight, and says so.
            let asked_other = id.is_some_and(|i| !r.ids.iter().any(|x| x == i));
            reply.insert("batch".into(), r.batch.into());
            reply.insert("ids".into(), json!(r.ids));
            reply.insert("text".into(), r.text.into());
            if asked_other {
                reply.insert("inFlight".into(), json!(r.ids.first()));
                reply.insert("note".into(), "ack it first".into());
            }
        }
        None => {
            reply.insert("batch".into(), Value::Null);
            reply.insert("ids".into(), json!([]));
            reply.insert(
                "text".into(),
                super::mail::held_text(ctx, &caller, &mailbox)?.into(),
            );
        }
    }
    if timed_out {
        reply.insert("timeout".into(), true.into());
    }
    Ok(reply)
}

/// A cancel of this owner stands while the mailbox ends at or before its `end` (W5).
fn cancelled(
    ctx: &Ctx,
    caller: &crate::caller::Caller,
    folder: &Path,
    owner: &super::receipt::Owner,
    mailbox: &str,
) -> Res<bool> {
    let Some(c) = read_json(&folder.join("oncall-cancel.json")).ok().flatten() else {
        return Ok(false);
    };
    let mine =
        text(c["owner"].get("id")) == owner.id && text(c["owner"].get("gen")) == owner.generation;
    Ok(mine && super::mail::counts(ctx, caller, mailbox)?.end <= c["end"].as_u64().unwrap_or(0))
}

/// The read fence: whether member `id` is still the current owner of its mailbox: its record committed, superseded
/// by no committed resume, and its hc terminal running now. The reader holds the mailbox lock, so a resume's barrier
/// orders it; anything that cannot be proven is not current.
pub(super) fn current(
    ctx: &Ctx,
    caller: &crate::caller::Caller,
    id: &super::core_identity::Identity,
) -> bool {
    use super::lifecycle::{Life, classify};
    let Ok(local) = super::membership::locals(ctx, caller) else {
        return false;
    };
    let Some(mine) = local
        .iter()
        .find(|l| l.identity == *id && l.stored.committed)
    else {
        return false;
    };
    if super::membership::superseded_locals(&local).contains(&id.id) {
        return false;
    }
    if mine.finished == Some(true) {
        return false; // R3: ended by its record
    }
    let Ok(core) = crate::hc::connect(ctx) else {
        return false;
    };
    let fact = core.get(&id.address).ok().filter(|t| id.matches(t));
    classify(fact.as_ref(), &id.launcher) == Life::Running
}

/// The last `lines` lines of a running agent's pane, captured by the server into a private buffer only while the
/// pane is still its live launch (one `if-shell`), then read and deleted. No buffer (the check failed) refuses 2
/// `target`: a pane replaced in between is never read.
pub(crate) fn capture(ctx: &Ctx, pane: &registry::Pane, lines: i32) -> Res<String> {
    let buffer = format!("mux-agents-{}", crate::util::guid()?.replace('-', ""));
    let p = pane.pane.as_str();
    let command = format!("capture-pane -J -t {p} -b {buffer} -S -{lines}");
    let argv = registry::bound(p, pane.launch.live(), command);
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let captured =
        native(ctx, &argv, true).and_then(|_| native(ctx, &["show-buffer", "-b", &buffer], true));
    // Always, whatever happened: a buffer left behind would hold the pane's text.
    let _ = native(ctx, &["delete-buffer", "-b", &buffer], true);
    // No buffer, or a call that failed as a call: nothing proves the text is the agent's.
    let Some(captured) = captured.ok().filter(|c| c.code == 0) else {
        return refuse(2, "target", "the agent's pane is no longer its launch");
    };
    let all: Vec<&str> = captured
        .stdout
        .trim_end_matches(['\r', '\n'])
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    Ok(all[all.len().saturating_sub(lines as usize)..].join("\n"))
}

/// `agents/<id>/waiting-<pid>.json`: the agent's `read -Wait` run by process `pid` (S2.4).
fn waiting_file(folder: &Path, pid: u32) -> PathBuf {
    folder.join(format!("waiting-{pid}.json"))
}

/// The pid a waiting file's name holds; none for any other file.
fn waiting_pid(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("waiting-")?.strip_suffix(".json")?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

/// Every waiting file in an agent's folder, with the pid its name holds.
fn waiting_files(folder: &Path) -> Vec<(u32, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let pid = waiting_pid(e.file_name().to_str()?)?;
            Some((pid, e.path()))
        })
        .collect()
}

/// The mailbox owner of the agent of `folder` (A3a call 13), for the readers `on_call` and `prune_leases`: for a
/// lifecycle member, the generation its identity stands for (A3a `Status::subject`: a held pending generation, else
/// the active one, R4); for a pre-lifecycle record, its `generation` ("0" when none). Absent, or a lifecycle member
/// with no subject: no owner. Unreadable refuses (R2).
pub(crate) fn owner_of_folder(folder: &Path) -> Res<Option<super::receipt::Owner>> {
    let Some(id) = folder
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
    else {
        return Ok(None);
    };
    let generation = match super::gens::member_of(folder) {
        super::gens::Member::Absent => return Ok(None),
        super::gens::Member::Unreadable(e) => return Err(e),
        super::gens::Member::Life(g) => match g.status()?.subject {
            Some(n) => n.to_string(),
            None => return Ok(None),
        },
        super::gens::Member::Pre(r) => Some(text(r.get("generation")))
            .filter(|g| !g.is_empty())
            .unwrap_or_else(|| "0".into()),
    };
    Ok(Some(super::receipt::Owner { id, generation }))
}

/// PM call 8, P2: the owner a mailbox write stamps. The generation of the identity the snapshot selected (a held
/// identity: its pending generation, R4); else the record `generation`; else "0" (a person). Reads no file.
pub(crate) fn snapshot_owner(
    key: &str,
    identity: Option<&super::core_identity::Identity>,
    record: Option<&Value>,
) -> super::receipt::Owner {
    let generation = identity
        .map(|i| i.generation.clone())
        .or_else(|| record.map(|r| text(r.get("generation"))))
        .filter(|g| !g.is_empty())
        .unwrap_or_else(|| "0".into());
    super::receipt::Owner {
        id: key.to_string(),
        generation,
    }
}

/// A lease naming `owner`, a live pid with its recorded start and a future `until` (W6).
fn lease_live(ctx: &Ctx, pid: u32, marker: &Value, owner: &super::receipt::Owner) -> bool {
    let named = marker.get("pid").and_then(Value::as_u64) == Some(u64::from(pid));
    let start = marker.get("start").and_then(Value::as_i64);
    let alive = named && start.is_some() && ctx.os.row(pid).is_some_and(|row| row.start == start);
    let mine = marker
        .get("owner")
        .is_some_and(|o| text(o.get("id")) == owner.id && text(o.get("gen")) == owner.generation);
    let until = marker
        .get("until")
        .and_then(Value::as_str)
        .and_then(crate::util::parse_utc);
    alive && mine && until.is_some_and(|u| u > ctx.clock.now())
}

/// Whether the agent of `folder` is on call: a lease of its current owner is live. Reads only.
pub(crate) fn on_call(ctx: &Ctx, folder: &Path) -> bool {
    let Ok(Some(owner)) = owner_of_folder(folder) else {
        return false;
    };
    waiting_files(folder).into_iter().any(|(pid, f)| {
        read_json(&f)
            .ok()
            .flatten()
            .is_some_and(|m| lease_live(ctx, pid, &m, &owner))
    })
}

/// Removes only stale leases: dead or reused pids, other owners, past `until`. The caller holds the agent lock.
pub(crate) fn prune_leases(ctx: &Ctx, folder: &Path) {
    let owner = match owner_of_folder(folder) {
        Ok(owner) => owner.unwrap_or_default(),
        Err(_) => return, // ambiguity 12: an unreadable record frees no lease
    };
    for (pid, file) in waiting_files(folder) {
        if !read_json(&file)
            .ok()
            .flatten()
            .is_some_and(|m| lease_live(ctx, pid, &m, &owner))
        {
            let _ = std::fs::remove_file(file);
        }
    }
}

/// Whether the agent waits: some waiting file's pid is alive with its recorded start, and the file is newer than
/// `status_at` (none: no status). A file whose process is gone or reused, or that cannot be read, is deleted. The
/// caller holds the agent lock.
pub(crate) fn waiting_now(ctx: &Ctx, folder: &Path, status_at: Option<SystemTime>) -> bool {
    let mut waiting = false;
    for (pid, file) in waiting_files(folder) {
        let marker = read_json(&file).ok().flatten();
        let field = |name: &str| marker.as_ref().and_then(|m| m.get(name));
        let since = field("since")
            .and_then(Value::as_str)
            .and_then(crate::util::parse_utc);
        let start = field("start").and_then(Value::as_i64);
        let named = field("pid").and_then(Value::as_u64) == Some(u64::from(pid));
        let alive =
            named && start.is_some() && ctx.os.row(pid).is_some_and(|row| row.start == start);
        let Some(since) = since.filter(|_| alive) else {
            let _ = std::fs::remove_file(&file);
            continue;
        };
        if status_at.is_none_or(|at| since > at) {
            waiting = true;
        }
    }
    waiting
}

/// The caller's own waiting file while its `read -Wait` runs; removed under the agent lock when dropped.
struct Waiting<'a> {
    ctx: &'a Ctx,
    key: String,
    pub(crate) file: PathBuf,
}

impl<'a> Waiting<'a> {
    /// Writes `{pid, start, since, owner, until}` under the agent lock before the first poll. Best effort: no file without a
    /// known start time (it could not be told from a reused pid) or when the write fails.
    fn start(
        ctx: &'a Ctx,
        caller: &crate::caller::Caller,
        id: &str,
        owner: &super::receipt::Owner,
        wait: u64,
    ) -> Option<Waiting<'a>> {
        let start = ctx.os.row(ctx.pid)?.start?;
        let file = waiting_file(&server_dir(ctx, caller).join("agents").join(id), ctx.pid);
        let since = crate::util::utc_o(ctx.clock.now());
        let until = crate::util::utc_o(ctx.clock.now() + Duration::from_secs(wait));
        let key = lock_key("agent", caller, id);
        lock(ctx, &key, || {
            write_json(
                &file,
                &json!({"pid": ctx.pid, "start": start, "since": since,
                    "owner": {"id": owner.id, "gen": owner.generation}, "until": until}),
            )
        })
        .ok()?;
        Some(Waiting { ctx, key, file })
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        // While unwinding, the file goes without the lock, so a second panic cannot abort.
        if std::thread::panicking() {
            let _ = std::fs::remove_file(&self.file);
            return;
        }
        let removed = lock(self.ctx, &self.key, || {
            let _ = std::fs::remove_file(&self.file);
            Ok(())
        });
        if removed.is_err() {
            let _ = std::fs::remove_file(&self.file);
        }
    }
}

const POLL: Duration = Duration::from_millis(500);

/// Refuses an agent caller that is not the record's parent (outside a team). An hc member's authority is its proven
/// member identity matched against the record's parent member (`parentAgent.id`), never a pane: its pane is its view,
/// empty when views are down (a3c call 42). A pane caller is the parent when its own pane is the record's
/// `parentPane`; an empty pane matches nothing.
fn parent_or_human(caller: &crate::caller::Caller, record: &Value) -> Res<()> {
    let parent = match &caller.member {
        Some(member) => {
            record
                .get("parentAgent")
                .and_then(|p| p.get("id"))
                .and_then(Value::as_str)
                == Some(member.as_str())
        }
        None => {
            caller.kind != Kind::Agent
                || (!caller.pane.is_empty()
                    && record.get("parentPane").and_then(Value::as_str)
                        == Some(caller.pane.as_str()))
        }
    };
    if !parent {
        return refuse(
            2,
            "parent",
            "only the parent or a human can stop this agent",
        );
    }
    Ok(())
}

pub fn capture_hc(
    ctx: &Ctx,
    caller: &crate::caller::Caller,
    name: &str,
    expected: Option<&str>,
    lines: i32,
    standing: Option<&super::tree::Standing>,
) -> Res<(registry::Pane, String)> {
    if !(1..=2000).contains(&lines) {
        return Err(usage("read"));
    }
    let id = super::member_target::named(ctx, caller, name)?;
    capture_identity(ctx, caller, &id, expected, lines, standing)
}

/// `capture_hc` for the bar (#103 B10): member `id` by id, never by name. The bar acts for the person: no standing.
pub fn capture_member(
    ctx: &Ctx,
    caller: &crate::caller::Caller,
    id: &str,
    expected: Option<&str>,
    lines: i32,
) -> Res<(registry::Pane, String)> {
    if !(1..=2000).contains(&lines) {
        return Err(usage("read"));
    }
    let id = super::member_target::by_id(ctx, caller, id)?;
    capture_identity(ctx, caller, &id, expected, lines, None)
}

fn capture_identity(
    ctx: &Ctx,
    caller: &crate::caller::Caller,
    id: &super::core_identity::Identity,
    expected: Option<&str>,
    lines: i32,
    standing: Option<&super::tree::Standing>,
) -> Res<(registry::Pane, String)> {
    lock(ctx, &lock_key("member", caller, &id.id), || {
        let record = super::member_target::current(ctx, caller, id)?;
        // Phase 3: a member reads only down the tree, checked under the member lock (`read <name>`; the bar passes None).
        if let Some(s) = standing {
            super::tree::authority(ctx, caller, s, &id.id, "read")?;
        }
        let Some(pane) = id.view(&record, &registry::rows(ctx, caller)?) else {
            return refuse(
                2,
                "target",
                format!("the agent has no live view; run focus {}", id.name),
            );
        };
        if expected.is_some_and(|e| e != pane.pane) {
            return refuse(
                2,
                "target",
                "the agent is no longer on the pane the bar drew",
            );
        }
        let tail = capture(ctx, &pane, lines)?;
        Ok((pane, tail))
    })
}

/// The output hc keeps for member `id` (#103 B6), also after it ended: `hc read`, only of a terminal that is still the
/// member's own (`Identity::matches`: its address, team, id and generation labels). None when it is blank, when no
/// committed member has that id, when hc answers `not_found` (it no longer keeps the terminal), or when the terminal at
/// that address is another identity's (an Unknown row): its output is never shown. Any other failure is an error.
pub fn retained(
    ctx: &Ctx,
    caller: &crate::caller::Caller,
    id: &str,
    lines: u32,
) -> Res<Option<String>> {
    let id = match super::member_target::by_id(ctx, caller, id) {
        Ok(id) => id,
        Err(e) if e.check == "target" => return Ok(None),
        Err(e) => return Err(e),
    };
    let core = crate::hc::connect(ctx)?;
    if !kept(&core, &id)? {
        return Ok(None);
    }
    match core.read(&id.address, lines) {
        Ok(text) => Ok((!text.trim().is_empty()).then_some(text)),
        Err(e) if e.check == "not_found" => Ok(None),
        Err(e) => Err(e),
    }
}

/// Whether hc still keeps member `id`'s own terminal (`hc get` and `Identity::matches`), as the bar reopens only one it
/// keeps: false when hc dropped it (`not_found`) or the terminal at its address is another identity's. None when no
/// committed member has that id. Any other hc failure, unreachable included, is an error.
pub fn kept_member(
    ctx: &Ctx,
    caller: &crate::caller::Caller,
    core: &crate::hc::Core<'_>,
    id: &str,
) -> Res<Option<bool>> {
    let id = match super::member_target::by_id(ctx, caller, id) {
        Ok(id) => id,
        Err(e) if e.check == "target" => return Ok(None),
        Err(e) => return Err(e),
    };
    kept(core, &id).map(Some)
}

/// Whether the terminal at `id`'s address is still `id`'s own: false when hc dropped it or it carries another
/// identity's labels (#103 review); any other hc failure is an error.
fn kept(core: &crate::hc::Core<'_>, id: &super::core_identity::Identity) -> Res<bool> {
    match core.get(&id.address) {
        Ok(t) => Ok(id.matches(&t)),
        Err(e) if e.check == "not_found" => Ok(false),
        Err(e) => Err(e),
    }
}

/// How long `stop` waits for the member's recorded client to exit (#137), and how often it looks.
const CLIENT_EXIT_BOUND: Duration = Duration::from_millis(8000);
const CLIENT_EXIT_POLL: Duration = Duration::from_millis(250);

/// What member `id` recorded of its client (call 9): for a lifecycle record, the `client.json` of the generation `stop`
/// targets (the identity's: a held pending generation's own, else the active one, A3a R4); for any other, the
/// `client` its `launcher.json` names. Each is read as typed evidence (`gens::walk`: regular files only,
/// never a FIFO, a folder or a link, so the read never blocks).
#[derive(Debug, PartialEq)]
pub(super) enum Recorded {
    /// No such file, or a `launcher.json` that names no client: nothing to wait for.
    Absent,
    /// Torn, not a regular file, or without a plain pid and start: the exit cannot be proven.
    Unreadable,
    Client(u32, i64),
}

fn recorded_client(member: &Path, record: &Value, generation: &str) -> Recorded {
    use crate::state::evidence::Evidence;
    let client = if super::gens::lifecycle(record) {
        let Some(n) = super::gens::plain_gen(generation) else {
            return Recorded::Unreadable;
        };
        match super::gens::read(member, n, "client") {
            Evidence::Present(c) => c,
            Evidence::Absent => return Recorded::Absent,
            Evidence::Unreadable => return Recorded::Unreadable,
        }
    } else {
        match super::gens::walk(member, &["launcher.json"]) {
            Evidence::Present(l) => match l.get("client") {
                Some(c) => c.clone(),
                None => return Recorded::Absent,
            },
            Evidence::Absent => return Recorded::Absent,
            Evidence::Unreadable => return Recorded::Unreadable,
        }
    };
    let pid = client
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|p| u32::try_from(p).ok());
    match (pid, client.get("start").and_then(Value::as_i64)) {
        (Some(pid), Some(start)) => Recorded::Client(pid, start),
        _ => Recorded::Unreadable,
    }
}

/// Waits, on the injected clock, until the recorded client is gone from the OS process table: no row for its pid, or
/// one with another known start. False once `CLIENT_EXIT_BOUND` has passed with it still unresolved.
fn client_exited(ctx: &Ctx, (pid, start): (u32, i64)) -> bool {
    let deadline = ctx.clock.now() + CLIENT_EXIT_BOUND;
    while listed(ctx.os.as_ref(), pid, start) {
        if ctx.clock.now() >= deadline {
            return false;
        }
        ctx.clock.sleep(CLIENT_EXIT_POLL);
    }
    true
}

/// Whether the process `pid` that started at `start` is still in the OS process table, read as resume's session check
/// reads it (`Os::table`, not `Os::row`: on Windows a handle another process holds keeps an exited process openable).
/// A table that cannot be read cannot show it gone, so it counts as listed. So does a row with the pid whose start cannot
/// be read (call 10; Windows keeps such rows): only no row, or a row with another known start, is gone.
fn listed(os: &dyn crate::proc::Os, pid: u32, start: i64) -> bool {
    os.table().map_or(true, |rows| {
        rows.iter()
            .any(|row| row.pid == pid && row.start.is_none_or(|s| os.same_start(s, start)))
    })
}

/// `stop <name>`: kills the member's hc terminal under its locks, then, with every lock released, replies only once its
/// recorded client has exited (#137), so a `resume` right after is not refused `session`. Past the bound it still
/// replies, with `clientExited: false`.
pub fn stop_hc(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let caller = crate::caller::resolve_tree(ctx)?;
    let (mut reply, client) = super::tree::with_standing(ctx, &caller, |standing| {
        let id = super::member_target::named(ctx, &caller, first(request))?;
        let folder = server_dir(ctx, &caller).join("agents").join(&id.id);
        // Dispatch only: `stop_life` revalidates through `current` (R1). An unreadable record goes to `stop_pre`,
        // whose `current` refuses it.
        match super::gens::member_of(&folder) {
            super::gens::Member::Life(_) => stop_life(ctx, &caller, standing, &id),
            _ => stop_pre(ctx, &caller, standing, &id),
        }
    })?;
    let exited = match client {
        Recorded::Absent => true,
        Recorded::Unreadable => false,
        Recorded::Client(pid, start) => client_exited(ctx, (pid, start)),
    };
    if !exited {
        reply.insert("clientExited".into(), false.into());
    }
    Ok(reply)
}

/// PM calls 9, 11, P1: a lifecycle member is stopped by ending generation n, the generation of the identity
/// `current` revalidated, through group B's `gens::terminate` (D3: a Lost generation is released, never killed), then
/// the Stopped notice of n. Unresolved: the error is returned and nothing is noticed.
pub(super) fn stop_life(
    ctx: &Ctx,
    caller: &Caller,
    standing: Option<&super::tree::Standing>,
    id: &super::core_identity::Identity,
) -> Res<(Map<String, Value>, Recorded)> {
    let record = super::member_target::current(ctx, caller, id)?;
    match standing {
        Some(s) => super::tree::authority(ctx, caller, s, &id.id, "stop")?,
        None => parent_or_human(caller, &record)?,
    }
    let n = super::gens::plain_gen(&id.generation)
        .ok_or_else(|| crate::error::Refusal::internal("invalid stopped generation"))?;
    let scope = super::team_state::Scope::of(caller);
    let end = |tx: &super::team_state::Tx<'_>| super::gens::terminate(tx, &id.id, n);
    // Inside a tree `with_standing` holds the session spawn lock, which is the team lock (state-0 S1).
    match standing {
        Some(_) => super::team_state::held(ctx, &scope, end)?,
        None => super::team_state::open(ctx, &scope, end)?,
    }
    // Lock order (state-0 S9): the team lock taken here and the member lock `terminate`'s record write takes are
    // released before the notice lock.
    let folder = server_dir(ctx, caller).join("agents").join(&id.id);
    let life = if super::gens::read(&folder, n, "ended").known()?.is_some() {
        "ended"
    } else if super::gens::read(&folder, n, "release").known()?.is_some() {
        "lost"
    } else {
        "unknown"
    };
    registry::with_member_notice(ctx, caller, &id.id, || {
        registry::add_notice_gen(ctx, caller, &id.id, &record, n, Notice::Stopped)
    })?;
    let client = recorded_client(&folder, &record, &id.generation);
    let mut reply = Map::new();
    reply.insert("name".into(), id.name.clone().into());
    reply.insert("hcAddress".into(), id.address.clone().into());
    reply.insert("life".into(), life.into());
    Ok((reply, client))
}

/// A pre-lifecycle member: main's stop, unchanged (kill the recorded address, classify, notice).
fn stop_pre(
    ctx: &Ctx,
    caller: &Caller,
    standing: Option<&super::tree::Standing>,
    id: &super::core_identity::Identity,
) -> Res<(Map<String, Value>, Recorded)> {
    registry::with_member_notice(ctx, caller, &id.id, || {
        let record = super::member_target::current(ctx, caller, id)?;
        match standing {
            Some(s) => super::tree::authority(ctx, caller, s, &id.id, "stop")?,
            None => parent_or_human(caller, &record)?,
        }
        let core = crate::hc::connect(ctx)?;
        core.kill(&id.address)?;
        let fact = core.get(&id.address).ok().filter(|t| id.matches(t));
        use super::lifecycle::{Life, classify};
        let life = match classify(fact.as_ref(), &id.launcher) {
            Life::Running => "running",
            Life::Lost => "lost",
            Life::Ended => "ended",
            Life::Unknown => "unknown",
        };
        add_notice(ctx, caller, &id.id, &record, Notice::Stopped)?;
        let member = server_dir(ctx, caller).join("agents").join(&id.id);
        let client = recorded_client(&member, &record, &id.generation);
        let mut reply = Map::new();
        reply.insert("name".into(), id.name.clone().into());
        reply.insert("hcAddress".into(), id.address.clone().into());
        reply.insert("life".into(), life.into());
        Ok((reply, client))
    })
}

#[cfg(test)]
mod tests {
    use super::stop_hc as stop;
    use super::*;

    // #137, on the real process table of each OS (macOS, Linux, Windows): a client that was killed and reaped is no
    // longer listed, so `stop`'s wait ends. On Windows a handle another process holds keeps an exited process
    // openable, so only the table (as resume's session check reads it) shows it gone.
    #[test]
    fn a_reaped_client_is_no_longer_listed_on_this_os() {
        let os = crate::proc::host();
        #[cfg(windows)]
        let mut child = std::process::Command::new("ping")
            .args(["-n", "60", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        #[cfg(unix)]
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let pid = child.id();
        let start = os.row(pid).and_then(|r| r.start).unwrap();
        assert!(
            listed(os.as_ref(), pid, start),
            "a running client is listed"
        );
        child.kill().unwrap();
        child.wait().unwrap();
        drop(child);
        // Call 56: Windows Toolhelp can still list an exited process for a moment after `wait()`; stop's client wait
        // polls with a bound, so the test waits up to 10 s (100 ms steps) for the table to drop it, then asserts.
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while listed(os.as_ref(), pid, start) && std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(
            !listed(os.as_ref(), pid, start),
            "a reaped client is not listed"
        );
    }
    use crate::agents::core_fixture as cf;
    use crate::agents::fixture::{ID, World, launch_command, write};
    use crate::agents::lock_key;
    use crate::agents::mail::read_mail;
    use crate::agents::view::focus;
    use crate::error::Refusal;
    use crate::mux::Output;
    use serde_json::json;

    /// SKILL.md's large-message rule, run as it is written (4b-i protocol): `read -Id` before the batch that carried the
    /// reference is acked is refused (the id is not held yet); acking that batch, alone or combined with `-Id`, then
    /// acking the body's own batch serves the whole body once and leaves nothing held or pending.
    #[test]
    fn a_large_message_is_read_in_the_skills_order_and_retired() {
        for combined in [false, true] {
            let w = World::human("");
            w.panes(&[]);
            let body = "x".repeat(5000);
            let id = crate::agents::mail::add_mail(
                &w.ctx,
                &w.caller(),
                "pane-1",
                "a",
                "%5",
                &body,
                None,
            )
            .unwrap();
            let r = |args: &[&str]| read(&w.ctx, &w.request(args)).map(Value::Object);
            let first = r(&["read"]).unwrap();
            let stream = first["batch"].as_str().unwrap().to_string();
            assert!(
                first["text"]
                    .as_str()
                    .unwrap()
                    .contains(&format!("run mux-agent read -Id {id}")),
                "a reference, not the body: {first}"
            );
            // The old order: the reference's batch is not acked, so the id is not held.
            let early = r(&["read", "-Id", &id]).unwrap_err();
            assert_eq!(early.check, "usage", "{}", early.message);
            let got = if combined {
                r(&["read", "-Id", &id, "-Ack", &stream]).unwrap()
            } else {
                assert_eq!(r(&["read", "-Ack", &stream]).unwrap()["acked"], true);
                r(&["read", "-Id", &id]).unwrap()
            };
            assert_eq!(got["ids"], json!([id]), "{combined}: {got}");
            assert!(got["text"].as_str().unwrap().contains(&body), "{combined}");
            let body_batch = got["batch"].as_str().unwrap().to_string();
            assert_eq!(
                r(&["read", "-Ack", &body_batch]).unwrap(),
                json!({"acked": true})
            );
            let c = crate::agents::receipt::load(&w.state().join("mail"), "pane-1").unwrap();
            assert!(
                c.held.is_empty() && c.pending.is_none() && c.id_pending.is_none(),
                "{combined}: retired"
            );
            assert_eq!(
                r(&["read"]).unwrap(),
                json!({"batch": null, "ids": [], "text": ""}),
                "{combined}: nothing served again"
            );
        }
    }

    #[test]
    fn read_id_names_the_receipt_in_flight() {
        let w = World::human("");
        w.panes(&[]);
        let mail = w.state().join("mail");
        for id in ["1111111111111111", "3333333333333333"] {
            crate::agents::mail::add_once(
                &w.ctx,
                &w.caller(),
                "pane-1",
                "a",
                "%5",
                "x",
                id,
                &Map::new(),
            )
            .unwrap();
        }
        let mut c = crate::agents::receipt::load(&mail, "pane-1").unwrap();
        c.held = vec!["1111111111111111".into(), "3333333333333333".into()];
        crate::agents::receipt::commit(&mail, "pane-1", &c).unwrap();
        let r = |args: &[&str]| read(&w.ctx, &w.request(args)).map(Value::Object).unwrap();
        let first = r(&["read", "-Id", "1111111111111111"]);
        let again = r(&["read", "-Id", "3333333333333333"]);
        assert_eq!(again["batch"], first["batch"]);
        assert_eq!(
            (&again["inFlight"], &again["note"]),
            (&json!("1111111111111111"), &json!("ack it first"))
        );
        assert!(first.get("inFlight").is_none());
    }

    #[test]
    fn the_read_verb_serves_acks_counts_and_refuses_bad_flags() {
        let w = World::human("");
        w.panes(&[]);
        crate::agents::mail::add_mail(&w.ctx, &w.caller(), "pane-1", "root", "%1", "hi", None)
            .unwrap();
        let r = |args: &[&str]| read(&w.ctx, &w.request(args)).map(Value::Object);
        let first = r(&["read", "-Deliver"]).unwrap();
        assert_eq!(first["ids"].as_array().map(Vec::len), Some(1), "{first}");
        assert!(first["text"].as_str().unwrap().ends_with("]\nhi\n"));
        assert_eq!(
            r(&["read"]).unwrap()["batch"],
            first["batch"],
            "served again until acked"
        );
        let c = r(&["read", "-Count"]).unwrap();
        assert_eq!(
            (
                &c["badge"],
                &c["wake"],
                &c["gen"],
                &c["leader"],
                &c["member"]
            ),
            (
                &json!(1),
                &json!(1),
                &json!("0"),
                &json!(false),
                &json!(false)
            )
        );
        let b = first["batch"].as_str().unwrap().to_string();
        assert_eq!(
            r(&["read", "-Ack", b.as_str()]).unwrap(),
            json!({"acked": true})
        );
        assert_eq!(
            r(&["read", "-Ack", b.as_str()]).unwrap(),
            json!({"acked": false, "duplicate": true})
        );
        assert_eq!(
            r(&["read", "-Deliver"]).unwrap(),
            json!({"batch": null, "ids": [], "text": ""})
        );
        for bad in [
            &["read", "-Count", "-Ack", "0123456789abcdef"][..],
            &["read", "-Deliver", "-Wait", "1"],
            &["read", "-Ack", "zz"],
            &["read", "w", "-Id", "0123456789abcdef"],
        ] {
            assert_eq!(r(bad).unwrap_err().check, "usage", "{bad:?}");
        }
    }

    #[test]
    fn concurrent_deliver_and_wait_share_one_receipt() {
        let w = World::human("");
        let (locks, state) = (
            w.ctx.state_root.parent().unwrap().join("locks"),
            w.ctx.state_root.clone(),
        );
        crate::agents::mail::add_mail(&w.ctx, &w.caller(), "pane-1", "root", "%1", "hi", None)
            .unwrap();
        let run = move |args: &'static [&'static str]| {
            let (locks, state) = (locks.clone(), state.clone());
            std::thread::spawn(move || {
                let mut v = World::human("");
                v.ctx.state_root = state;
                v.fakes.real_locks(&locks);
                v.panes(&[]);
                read(&v.ctx, &v.request(args)).unwrap()["batch"].clone()
            })
        };
        let (one, two) = (run(&["read", "-Deliver"]), run(&["read", "-Wait", "1"]));
        let (one, two) = (one.join().unwrap(), two.join().unwrap());
        assert!(one.is_string() && one == two, "{one} {two}");
    }

    type Handler = fn(&Ctx, &Request) -> Res<Map<String, Value>>;

    const PROBE: &str = "#{pane_id}|#{session_id}";
    const NONE: &str = "2 target no live agent has that name";

    fn target_refusal(verb: &str) -> &'static str {
        if verb == "focus" || verb == "stop" || verb == "read" {
            "2 target no unique committed member has that identity"
        } else {
            NONE
        }
    }
    /// The entries of a served batch's `text` (#103 4b-i): each from its `[mux-agent from ` header, without its LF.
    fn entries(reply: &Map<String, Value>) -> Vec<String> {
        let text = reply["text"].as_str().unwrap_or_default();
        text.split("[mux-agent from ")
            .skip(1)
            .map(|e| format!("[mux-agent from {}", e.trim_end_matches('\n')))
            .collect()
    }

    fn run(w: &World, handler: Handler, args: &[&str]) -> String {
        let result = handler(&w.ctx, &w.request(args));
        w.fakes.hc_done().unwrap();
        match result {
            Ok(reply) => format!("ok {}", Value::Object(reply)),
            Err(r) => format!("{} {} {}", r.code, r.check, r.message),
        }
    }

    // Each verb that names a target, with the arguments around the name.
    fn verbs(name: &str) -> Vec<(Handler, Vec<String>)> {
        let args = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        vec![
            (focus as Handler, args(&["focus", name])),
            (read, args(&["read", name])),
            (send, args(&["send", name, "-Body", "x"])),
            (stop, args(&["stop", name])),
        ]
    }

    fn run_owned(w: &World, handler: Handler, args: &[String]) -> String {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        run(w, handler, &args)
    }

    fn mux_verbs(w: &World) -> Vec<String> {
        w.fakes.mux_calls().iter().map(|c| c[1].clone()).collect()
    }

    #[test]
    fn target_malformed_name() {
        for name in ["Worker", "1x", "worker!", "a_b", &"a".repeat(25)] {
            let w = World::human("");
            w.agent_record(ID, json!({"name": "worker"}));
            w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
            for (handler, args) in verbs(name) {
                w.fakes.clear_log();
                if args[0] == "focus" {
                    cf::online(&w);
                }
                assert_eq!(
                    run_owned(&w, handler, &args),
                    target_refusal(&args[0]),
                    "{args:?}"
                );
                // Refused before any pane is listed.
                assert!(!mux_verbs(&w).iter().any(|v| v == "list-panes"), "{args:?}");
            }
        }
    }

    #[test]
    fn target_unknown_name() {
        let w = World::human("");
        w.agent_record(ID, json!({}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        for name in ["nobody", "worke", "workers"] {
            for (handler, args) in verbs(name) {
                w.fakes.clear_log();
                if args[0] == "focus" {
                    cf::online(&w);
                }
                // send also asks hc for a running member with no view by that name (#104 phase 1c).
                if args[0] == "send" {
                    cf::online(&w);
                    cf::listing(&w, &crate::agents::membership::team(&w.caller()), &[]);
                }
                assert_eq!(
                    run_owned(&w, handler, &args),
                    target_refusal(&args[0]),
                    "{args:?}"
                );
                assert_eq!(
                    mux_verbs(&w),
                    if args[0] == "focus" || args[0] == "stop" || args[0] == "read" {
                        vec!["display-message"]
                    } else if args[0] == "send" {
                        // The pane snapshot misses, then the hc member lookup takes its own (#104 phase 1c).
                        vec![
                            "display-message",
                            "display-message",
                            "list-panes",
                            "list-panes",
                        ]
                    } else {
                        vec!["display-message", "list-panes"]
                    },
                    "{args:?}"
                );
            }
        }
    }

    #[test]
    fn target_uncommitted() {
        // Tagged and live but without commit.json: not an agent yet.
        let w = World::human("");
        w.uncommitted(ID, json!({}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        for (handler, args) in verbs("worker") {
            w.fakes.clear_log();
            if args[0] == "focus" {
                cf::online(&w);
            }
            if args[0] == "send" {
                cf::online(&w);
                cf::listing(&w, &crate::agents::membership::team(&w.caller()), &[]);
            }
            assert_eq!(
                run_owned(&w, handler, &args),
                target_refusal(&args[0]),
                "{args:?}"
            );
            if args[0] != "send" {
                assert_eq!(mux_verbs(&w), ["display-message"], "{args:?}");
            }
        }
        // Committed without a record: stop has no hc identity and closes nothing.
        let w = World::human("");
        let folder = w.folder(ID);
        std::fs::create_dir_all(&folder).unwrap();
        write(&folder.join("commit.json"), json!({}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        w.fakes.clear_log();
        assert_eq!(run(&w, stop, &["stop", "worker"]), target_refusal("stop"));
        assert!(pane_kills(&w).is_empty());
    }

    #[test]
    fn send_parent_from_human_refuses_parent() {
        let w = World::human("");
        w.fakes.clear_log();
        assert_eq!(
            run(&w, send, &["send", "parent", "-Body", "x"]),
            "2 parent only an agent has a parent"
        );
        // One pane list names the sender; no recipient probe, nothing written.
        assert_eq!(mux_verbs(&w), ["display-message", "list-panes"]);
        assert!(!w.ctx.state_root.exists());
    }

    #[test]
    fn tagged_sender_without_record_refuses_parent() {
        let refused = "2 parent this agent pane has no record";
        for tag in ["bbbbbbbbbbbb", "tag-7"] {
            let w = World::human(tag);
            w.agent_record(ID, json!({}));
            w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
            w.recipients_live();
            for args in [
                &["send", "parent", "-Body", "x"][..],
                &["send", "%1", "-Body", "x"],
                &["send", "worker", "-Body", "x"],
                &["read"],
            ] {
                if args[0] == "read" {
                    poll(&w, &[]);
                }
                assert_eq!(
                    run(&w, args_handler(args[0]), args),
                    refused,
                    "{tag} {args:?}"
                );
            }
            assert!(!w.state().join("mail").join("pane-1.jsonl").exists());
        }
    }

    fn args_handler(verb: &str) -> Handler {
        match verb {
            "send" => send,
            "read" => read,
            _ => stop,
        }
    }

    // An agent pane %1, running agent ID by its record and launch, whose parent is %4; the recipient probe answers
    // every pane live in session $3.
    fn agent_world() -> World {
        let w = World::agent(ID);
        w.agent_record(ID, json!({"pane": "%1", "parentPane": "%4"}));
        w.panes(&["%1|@1|100|aaaaaaaaaaaa|worker"]);
        w.recipients_live();
        w
    }

    #[test]
    fn send_agent_body_refused_use_bodyfile() {
        let w = agent_world();
        assert_eq!(
            run(&w, send, &["send", "parent", "-Body", "x"]),
            "2 usage agents pass free text only in files: use -BodyFile"
        );
        let usage = "2 usage usage: mux-agent send <name|parent|leader|children|team|%pane> (-Body <text> | -BodyFile <path>) [-Re <id>] [-Id <id>] [-Text]";
        w.file("b.md", b"ok");
        w.fakes.clear_log();
        assert_eq!(run(&w, send, &["send", "parent"]), usage);
        assert_eq!(
            run(
                &w,
                send,
                &["send", "parent", "-Body", "x", "-BodyFile", "b.md"]
            ),
            usage
        );
        assert!(w.fakes.mux_calls().is_empty());
        let reply = run(&w, send, &["send", "parent", "-BodyFile", "b.md"]);
        assert!(
            reply.starts_with(r#"ok {"to":"parent","pane":"%4","id":""#),
            "{reply}"
        );
        let messages = read_mail_of(&w, "pane-4");
        assert_eq!(messages, ["ok"]);
        // A human may pass -Body.
        let w = World::human("");
        w.recipients_live();
        assert!(run(&w, send, &["send", "%1", "-Body", "hi"]).starts_with("ok "));
    }

    // The bodies of a mailbox's unread messages.
    fn read_mail_of(w: &World, key: &str) -> Vec<String> {
        read_mail(&w.ctx, &w.caller(), key)
            .unwrap()
            .iter()
            .map(|m| {
                crate::agents::text(m.get("text"))
                    .split_once('\n')
                    .unwrap()
                    .1
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn send_64kb_limit() {
        let w = World::human("");
        w.recipients_live();
        let big = "the message is larger than 64 KB";
        let send_body = |body: &str| {
            let args = ["send", "%1", "-Body", body];
            run(&w, send, &args)
        };
        // Counted in UTF-8 bytes after cleaning: CR LF becomes LF and controls go.
        assert!(send_body(&"a".repeat(65536)).starts_with("ok "));
        assert_eq!(send_body(&"a".repeat(65537)), format!("2 usage {big}"));
        assert!(send_body(&format!("{}\r\n\u{7}", "a".repeat(65535))).starts_with("ok "));
        assert!(send_body(&"\u{e9}".repeat(32768)).starts_with("ok "));
        assert_eq!(
            send_body(&format!("{}a", "\u{e9}".repeat(32768))),
            format!("2 usage {big}")
        );
        // A file over 64 KB is refused by the reader, before cleaning.
        let w = agent_world();
        w.file("ok.md", "a".repeat(65536).as_bytes());
        w.file("big.md", format!("{}\r\n", "a".repeat(65535)).as_bytes());
        assert!(run(&w, send, &["send", "parent", "-BodyFile", "ok.md"]).starts_with("ok "));
        assert_eq!(
            run(&w, send, &["send", "parent", "-BodyFile", "big.md"]),
            "2 usage a task or brief file is larger than 65536 bytes"
        );
    }

    #[test]
    fn send_target_other_session_refuses() {
        let refused = "2 target the recipient is not a live pane of this session";
        for (code, out) in [
            (0, "%5|$4\n"),
            (1, "%5|$3\n"),
            (0, ""),
            (0, "|\n"),
            (0, "%5\n"),
            (0, "%6|$3\n"),
            (0, "%5|$3\nmore\n"),
            (0, "%5|$03|\n"),
            (0, "%5|3\n"),
        ] {
            let w = World::human("");
            w.fakes.mux_first(PROBE, code, out, "");
            assert_eq!(
                run(&w, send, &["send", "%5", "-Body", "x"]),
                refused,
                "{code} {out:?}"
            );
            assert!(!w.ctx.state_root.exists(), "{out:?}");
        }
        // The probe goes to the named pane and never reads a tag; a running agent's pane is its id's mailbox, any
        // other pane (whatever tag it carries) is pane-<n>.
        let w = World::human("");
        w.agent_record("bbbbbbbbbbbb", json!({"pane": "%5"}));
        w.panes(&["%5|@5|300|cccccccccccc|other", "%6|@5|301|bbbbbbbbbbbb|w"]);
        w.recipients_live();
        w.fakes.clear_log();
        assert!(
            run(&w, send, &["send", "%5", "-Body", "x"])
                .starts_with(r#"ok {"to":"%5","pane":"%5""#)
        );
        assert_eq!(
            w.fakes.mux_calls()[2],
            ["-N", "display-message", "-p", "-t", "%5", PROBE]
        );
        assert_eq!(read_mail_of(&w, "bbbbbbbbbbbb"), ["x"]);
        run(&w, send, &["send", "%6", "-Body", "y"]);
        assert_eq!(read_mail_of(&w, "pane-6"), ["y"]);
        assert!(read_mail_of(&w, "cccccccccccc").is_empty());
    }

    #[test]
    fn send_sender_lock_then_mail_lock() {
        let w = agent_world();
        w.file("b.md", b"ok");
        let caller = w.caller();
        let sent = w.folder(ID).join("last-sent.json");
        let mailbox = w.state().join("mail").join("pane-4.jsonl");
        let seen = sent.clone();
        w.fakes.lock_probe(move |_| {
            let mailed = std::fs::metadata(&mailbox).is_ok_and(|m| m.len() > 0);
            format!("sent={} mailed={mailed}", seen.exists())
        });
        w.fakes.clear_log();
        assert!(run(&w, send, &["send", "parent", "-BodyFile", "b.md"]).starts_with("ok "));
        let agent = lock_key("agent", &caller, ID);
        let mail = lock_key("mail", &caller, "pane-4");
        // One hold of the sender's lock around the append and the last-sent write; the append inside the
        // recipient's mail lock.
        assert_eq!(
            w.fakes.locks(),
            [
                format!("+{agent} sent=false mailed=false"),
                format!("+{mail} sent=false mailed=false"),
                format!("-{mail} sent=false mailed=true"),
                format!("-{agent} sent=true mailed=true")
            ]
        );
        // A human sender takes only the recipient's mail lock.
        let w = World::human("");
        w.recipients_live();
        let mail = lock_key("mail", &w.caller(), "pane-1");
        w.fakes.clear_log();
        run(&w, send, &["send", "%1", "-Body", "x"]);
        assert_eq!(w.fakes.locks(), [format!("+{mail}"), format!("-{mail}")]);
    }

    #[test]
    fn last_sent_written_best_effort() {
        let w = agent_world();
        w.clock("2026-10-29T12:00:00.0000000Z");
        let sent = w.folder(ID).join("last-sent.json");
        w.file("b.md", "\r\ndone:\u{9b}\tall green\u{7}\nmore".as_bytes());
        w.file("blank.md", b" \n");
        run(&w, send, &["send", "parent", "-BodyFile", "b.md"]);
        assert_eq!(
            std::fs::read_to_string(&sent).unwrap(),
            r#"{"line":"done: all green","at":"2026-10-29T12:00:00.0000000Z"}"#
        );
        w.clock("2026-10-29T12:00:05.0000000Z");
        run(&w, send, &["send", "parent", "-BodyFile", "blank.md"]);
        assert_eq!(
            std::fs::read_to_string(&sent).unwrap(),
            r#"{"line":"","at":"2026-10-29T12:00:05.0000000Z"}"#
        );
        // A failed write still delivers the message.
        std::fs::remove_file(&sent).unwrap();
        std::fs::create_dir(&sent).unwrap();
        w.file("c.md", b"still");
        assert!(run(&w, send, &["send", "parent", "-BodyFile", "c.md"]).starts_with("ok "));
        assert_eq!(read_mail_of(&w, "pane-4").last().unwrap(), "still");
        // A human writes none.
        let w = World::human("");
        w.recipients_live();
        run(&w, send, &["send", "%1", "-Body", "x"]);
        let agents = w.state().join("agents");
        assert_eq!(std::fs::read_dir(agents).unwrap().count(), 0);
    }

    #[test]
    fn read_wait_bounds_0_600() {
        let usage = "2 usage usage: mux-agent read [-Wait <seconds> | -Deliver | -Id <id> | -Count | -Cancel] [-Ack <batch>] | read <name> [-Lines <n>] [-Text]";
        let w = World::human("");
        w.fakes.clear_log();
        for wait in ["601", "-1", "x", "", "1.5", "99999999999", "0x10"] {
            assert_eq!(run(&w, read, &["read", "-Wait", wait]), usage, "{wait:?}");
        }
        assert_eq!(run(&w, read, &["read", "worker", "-Wait", "1"]), usage);
        assert!(w.fakes.mux_calls().is_empty());
        // .NET's int.TryParse: a sign and white space around are fine.
        for wait in ["0", "600", "+5", " 7 ", "-0"] {
            w.recipients_live();
            run(&w, send, &["send", "%1", "-Body", "m"]);
            poll(&w, &[]);
            let reply = run(&w, read, &["read", "-Wait", wait]);
            assert!(reply.contains("\\nm\\n\""), "{wait:?} {reply}");
        }
        assert!(w.fakes.slept().is_empty());
    }

    // A live worker (%9) and a committed agent `gone` (%8) whose pane is gone, its notice not yet posted.
    fn gone_world() -> World {
        let w = World::human("");
        w.agent_record(ID, json!({}));
        w.agent_record("bbbbbbbbbbbb", json!({"name": "gone", "pane": "%8"}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        w.fakes
            .mux_rule("-t %8 #{pane_id}|#{@agent-id}", 0, "|\n", "");
        w
    }

    // #104 phase 1c: closing a view never stops an agent, so a running member with no view is still reachable by
    // name; its mailbox is its id and no pane is probed for it.
    #[test]
    fn send_reaches_a_running_member_with_no_view() {
        let w = World::human("");
        let caller = w.caller();
        let r = cf::member(&w, ID, json!({"pane":"%5","window":"@1"}));
        w.pane_rows(&[]);
        cf::online(&w);
        cf::listing(&w, &crate::agents::membership::team(&caller), &[]);
        cf::get(&w, "taaaaaaaaaaaa", cf::wire(&r));
        let out = run(&w, send, &["send", "worker", "-Body", "hi"]);
        assert!(out.starts_with("ok "), "{out}");
        assert_eq!(read_mail_of(&w, ID), ["hi"]);
        w.fakes.hc_done().unwrap();
    }

    // A resumed member keeps the mailbox it inherited: its record's `mailbox` names it, and send delivers there.
    #[test]
    fn send_delivers_to_the_mailbox_a_member_inherited() {
        let w = World::human("");
        let caller = w.caller();
        let r = cf::member(
            &w,
            ID,
            json!({"pane":"%5","window":"@1","mailbox":"bbbbbbbbbbbb"}),
        );
        w.pane_rows(&[]);
        cf::online(&w);
        cf::listing(&w, &crate::agents::membership::team(&caller), &[]);
        cf::get(&w, "taaaaaaaaaaaa", cf::wire(&r));
        let out = run(&w, send, &["send", "worker", "-Body", "hi"]);
        assert!(out.starts_with("ok "), "{out}");
        assert_eq!(read_mail_of(&w, "bbbbbbbbbbbb"), ["hi"]);
        assert!(read_mail_of(&w, ID).is_empty());
        w.fakes.hc_done().unwrap();
    }

    // A member whose record names an inherited mailbox reads that mailbox, not one named by its own id.
    #[test]
    fn mailbox_read_uses_the_inherited_mailbox() {
        let w = World::human("");
        let r = cf::member(&w, ID, json!({"mailbox":"bbbbbbbbbbbb"}));
        w.panes(&["%1|@1|300|aaaaaaaaaaaa|worker"]);
        let mut r = r;
        r["pane"] = json!("%1");
        r["startCommand"] = json!(launch_command("%1"));
        write(&w.folder(ID).join("record.json"), r.clone());
        let c = w.caller();
        add_mail(&w.ctx, &c, "bbbbbbbbbbbb", "root", "%2", "inherited", None).unwrap();
        add_mail(&w.ctx, &c, ID, "root", "%2", "own", None).unwrap();
        poll(&w, &[cf::wire(&r)]);
        fence(&w, &r);
        let reply = read(&w.ctx, &w.request(&["read"])).unwrap();
        let texts: Vec<String> = entries(&reply)
            .iter()
            .map(|e| e.rsplit('\n').next().unwrap().to_string())
            .collect();
        assert_eq!(texts, ["inherited"]);
        w.fakes.hc_done().unwrap();
    }

    // The read fence: holding its mailbox lock, a member reads only while it is still current. A member a committed
    // resume superseded reads nothing, and its mail stays for the successor.
    #[test]
    fn a_superseded_member_reads_nothing() {
        let w = World::human("");
        let x = cf::member(
            &w,
            ID,
            json!({"pane":"%1","startCommand":launch_command("%1")}),
        );
        let y = cf::member(
            &w,
            "bbbbbbbbbbbb",
            json!({"pane":"%6","resumedFrom":ID,"mailbox":ID}),
        );
        w.panes(&["%1|@1|300|aaaaaaaaaaaa|worker"]);
        add_mail(&w.ctx, &w.caller(), ID, "root", "%2", "hi", None).unwrap();
        poll(&w, &[cf::wire(&x), cf::wire(&y)]);
        let reply = read(&w.ctx, &w.request(&["read"])).unwrap();
        assert!(reply["ids"].as_array().unwrap().is_empty());
        w.fakes.hc_done().unwrap();
        assert_eq!(read_mail_of(&w, ID), ["hi"]);
    }

    // A member whose terminal ended after its snapshot and before it took its mailbox lock reads nothing: hc is asked
    // again under the lock.
    #[test]
    fn a_member_that_ended_before_the_lock_reads_nothing() {
        let w = World::human("");
        let x = cf::member(
            &w,
            ID,
            json!({"pane":"%1","startCommand":launch_command("%1")}),
        );
        w.panes(&["%1|@1|300|aaaaaaaaaaaa|worker"]);
        add_mail(&w.ctx, &w.caller(), ID, "root", "%2", "hi", None).unwrap();
        poll(&w, &[cf::wire(&x)]);
        let mut ended = cf::wire(&x);
        ended["state"] = json!("finished");
        ended["reason"] = json!("exited");
        cf::online(&w);
        cf::get(&w, "taaaaaaaaaaaa", ended);
        let reply = read(&w.ctx, &w.request(&["read"])).unwrap();
        assert!(reply["ids"].as_array().unwrap().is_empty());
        w.fakes.hc_done().unwrap();
        assert_eq!(read_mail_of(&w, ID), ["hi"]);
    }

    // A member a committed resume superseded never makes its successor's name ambiguous, even before its end
    // notice is posted.
    #[test]
    fn send_by_name_reaches_the_successor_not_the_superseded_member() {
        let w = World::human("");
        let caller = w.caller();
        let x = cf::member(&w, ID, json!({"pane":"%5","window":"@1"}));
        let y = cf::member(
            &w,
            "bbbbbbbbbbbb",
            json!({"pane":"%6","window":"@1","resumedFrom":ID,"mailbox":ID}),
        );
        let mut ended = cf::wire(&x);
        ended["state"] = json!("finished");
        ended["reason"] = json!("exited");
        w.pane_rows(&[]);
        cf::online(&w);
        cf::listing(&w, &crate::agents::membership::team(&caller), &[]);
        cf::get(&w, "taaaaaaaaaaaa", ended);
        cf::get(&w, "tbbbbbbbbbbbb", cf::wire(&y));
        let out = run(&w, send, &["send", "worker", "-Body", "hi"]);
        assert!(out.starts_with("ok "), "{out}");
        assert_eq!(read_mail_of(&w, ID), ["hi"]);
        w.fakes.hc_done().unwrap();
    }

    // A superseded member whose pane survives is never what its name means: the pane fast path applies the current
    // member rule (committed, not superseded, hc running), so send reaches the running successor, and with the
    // successor ended it refuses and delivers nothing.
    #[test]
    fn send_by_name_skips_a_superseded_member_that_still_has_a_pane() {
        for successor in ["running", "ended"] {
            let w = World::human("");
            let caller = w.caller();
            let x = cf::member(
                &w,
                ID,
                json!({"pane":"%5","window":"@1","startCommand":launch_command("%5")}),
            );
            let y = cf::member(
                &w,
                "bbbbbbbbbbbb",
                json!({"pane":"%6","window":"@1","resumedFrom":ID,"mailbox":ID}),
            );
            w.panes(&["%5|@1|300|aaaaaaaaaaaa|worker"]);
            w.recipients_live();
            let mut ended = cf::wire(&x);
            ended["state"] = json!("finished");
            ended["reason"] = json!("exited");
            let mut ty = cf::wire(&y);
            if successor == "ended" {
                ty["state"] = json!("finished");
                ty["reason"] = json!("exited");
            }
            cf::online(&w);
            cf::listing(&w, &crate::agents::membership::team(&caller), &[]);
            cf::get(&w, "taaaaaaaaaaaa", ended);
            cf::get(&w, "tbbbbbbbbbbbb", ty);
            let got = send(&w.ctx, &w.request(&["send", "worker", "-Body", "hi"]));
            match successor {
                "running" => assert_eq!(got.unwrap()["recipients"][0]["id"], "bbbbbbbbbbbb"),
                _ => {
                    let e = got.unwrap_err();
                    assert_eq!((e.code, e.check), (2, "target"), "{}", e.message);
                    assert!(read_mail_of(&w, ID).is_empty());
                }
            }
            w.fakes.hc_done().unwrap();
        }
    }

    // #104 phase 1c review: the fallback delivers only to the one committed, unnoticed member of that name that hc
    // shows running; an ended, uncommitted or noticed one, or a name two members share (even when hc cannot read
    // one of them), refuses and leaves every mailbox empty.
    #[test]
    fn send_to_a_viewless_member_refuses_unless_it_is_the_one_running() {
        for case in ["ended", "uncommitted", "noticed", "twin", "twin-unknown"] {
            let w = World::human("");
            let caller = w.caller();
            let r = cf::member(&w, ID, json!({"pane":"%5","window":"@1"}));
            let mut t = cf::wire(&r);
            match case {
                "ended" => {
                    t["state"] = json!("finished");
                    t["reason"] = json!("exited");
                }
                "uncommitted" => std::fs::remove_file(w.folder(ID).join("commit.json")).unwrap(),
                "noticed" => write(&w.folder(ID).join("notice.json"), json!({"kind":"stopped"})),
                _ => {}
            }
            w.pane_rows(&[]);
            cf::online(&w);
            cf::listing(&w, &crate::agents::membership::team(&caller), &[]);
            cf::get(&w, "taaaaaaaaaaaa", t);
            if case.starts_with("twin") {
                let b = cf::member(&w, "bbbbbbbbbbbb", json!({"pane":"%6","window":"@1"}));
                let mut tb = cf::wire(&b);
                if case == "twin-unknown" {
                    tb["labels"]["mux.gen"] = json!("1");
                }
                cf::get(&w, "tbbbbbbbbbbbb", tb);
            }
            let out = run(&w, send, &["send", "worker", "-Body", "hi"]);
            assert_eq!(out, NONE, "{case}");
            assert!(read_mail_of(&w, ID).is_empty(), "{case}");
            assert!(read_mail_of(&w, "bbbbbbbbbbbb").is_empty(), "{case}");
        }
    }

    // When hc cannot be read, send says so; it never claims the name has no live agent.
    #[test]
    fn send_reports_an_unreadable_hc_not_a_missing_agent() {
        let w = World::human("");
        cf::member(&w, ID, json!({"pane":"%5","window":"@1"}));
        w.pane_rows(&[]);
        let out = run(&w, send, &["send", "worker", "-Body", "hi"]);
        assert_ne!(out, NONE);
        assert!(!out.starts_with("ok "), "{out}");
        assert!(read_mail_of(&w, ID).is_empty());
    }

    #[test]
    fn named_read_does_not_reap_other_members() {
        let w = gone_world();
        cf::member(&w, ID, json!({"startCommand":launch_command("%9")}));
        cf::member(&w, "bbbbbbbbbbbb", json!({"name":"gone","pane":"%8"}));
        w.fakes.mux_rule("show-buffer", 0, "one\ntwo\n", "");
        w.fakes.clear_log();
        assert_eq!(
            run(&w, read, &["read", "worker", "-Lines", "1"]),
            r#"ok {"name":"worker","pane":"%9","text":"two"}"#
        );
        assert_eq!(
            mux_verbs(&w),
            [
                "display-message",
                "list-panes",
                "if-shell",
                "show-buffer",
                "delete-buffer"
            ]
        );
        assert!(!w.folder("bbbbbbbbbbbb").join("notice.json").exists());
        assert!(read_mail_of(&w, "pane-1").is_empty());
        assert_eq!(
            run(&w, read, &["read", "gone"]),
            "2 target the agent has no live view; run focus gone"
        );
        w.fakes.hc_done().unwrap();
    }

    #[test]
    fn read_lines_1_2000() {
        let usage = "2 usage usage: mux-agent read [-Wait <seconds> | -Deliver | -Id <id> | -Count | -Cancel] [-Ack <batch>] | read <name> [-Lines <n>] [-Text]";
        let w = World::human("");
        cf::member(&w, ID, json!({"startCommand":launch_command("%9")}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        w.fakes.clear_log();
        for lines in ["0", "2001", "x", "-1"] {
            assert_eq!(
                run(&w, read, &["read", "worker", "-Lines", lines]),
                usage,
                "{lines}"
            );
        }
        assert_eq!(run(&w, read, &["read", "-Lines", "5"]), usage);
        assert!(w.fakes.mux_calls().is_empty());
        w.fakes
            .mux_first("show-buffer", 0, "one\r\ntwo\n\nthree\r\n\n", "");
        assert_eq!(
            run(&w, read, &["read", "worker", "-Lines", "2"]),
            r#"ok {"name":"worker","pane":"%9","text":"\nthree"}"#
        );
        let live = registry::Launch {
            command: launch_command("%9"),
            pid: None,
        }
        .live();
        for (args, s) in [
            (&["read", "worker"][..], "-60"),
            (&["read", "worker", "-Lines", "2000"], "-2000"),
            (&["read", "worker", "-Lines", "1"], "-1"),
        ] {
            w.fakes.clear_log();
            assert!(run(&w, read, args).starts_with("ok "));
            // Captured by the server only while %9 is still its live launch, into a private buffer that is read and
            // then deleted.
            let calls = w.fakes.mux_calls();
            let n = calls.len();
            let capture = &calls[n - 3];
            assert_eq!(
                capture[..6],
                ["-N", "if-shell", "-F", "-t", "%9", live.as_str()]
            );
            let command: Vec<&str> = capture[6].split(' ').collect();
            assert_eq!(command[..4], ["capture-pane", "-J", "-t", "%9"]);
            assert_eq!((command[4], command[6], command[7]), ("-b", "-S", s));
            let buffer = command[5];
            assert!(
                buffer
                    .strip_prefix("mux-agents-")
                    .is_some_and(|h| h.len() == 32),
                "{buffer}"
            );
            assert_eq!(calls[n - 2], ["-N", "show-buffer", "-b", buffer]);
            assert_eq!(calls[n - 1], ["-N", "delete-buffer", "-b", buffer]);
        }
        // No buffer (the pane was not its launch when the server checked): nothing read, the buffer still deleted.
        w.fakes.mux_first("show-buffer", 1, "", "no buffer");
        w.fakes.clear_log();
        assert_eq!(
            run(&w, read, &["read", "worker"]),
            "2 target the agent's pane is no longer its launch"
        );
        assert_eq!(mux_verbs(&w).last().unwrap(), "delete-buffer");
    }

    #[test]
    fn read_wait_polls_500ms_until_deadline() {
        let w = World::human("");
        w.state();
        for _ in 0..7 {
            poll(&w, &[]);
        }
        assert_eq!(
            run(&w, read, &["read", "-Wait", "3"]),
            r#"ok {"batch":null,"ids":[],"text":"","timeout":true}"#
        );
        assert_eq!(w.fakes.slept(), [500; 6]);
        // Mail that lands while waiting ends the wait at the next poll.
        let w = World::human("");
        let mailbox = w.state().join("mail").join("pane-1.jsonl");
        let polls = std::cell::Cell::new(0);
        w.fakes.lock_probe(move |event| {
            if event.starts_with('-') {
                polls.set(polls.get() + 1);
                if polls.get() == 2 {
                    std::fs::write(&mailbox, "{\"id\":\"1\",\"from\":\"w\",\"pane\":\"%9\",\"at\":\"x\",\"body\":\"late\"}\n").unwrap();
                }
            }
            String::new()
        });
        for _ in 0..3 {
            poll(&w, &[]);
        }
        let reply = run(&w, read, &["read", "-Wait", "600"]);
        assert!(reply.contains(r#"\nlate\n""#), "{reply}");
        assert_eq!(w.fakes.slept(), [500, 500]);
    }

    // Every kill of a pane sent to the multiplexer.
    fn pane_kills(w: &World) -> Vec<Vec<String>> {
        w.fakes
            .mux_calls()
            .into_iter()
            .filter(|c| c[1] == "kill-pane" || c.last().is_some_and(|a| a.starts_with("kill-pane")))
            .collect()
    }

    // ---- waiting (S2.4)

    const SINCE: &str = "2026-10-29T12:00:00.0000000Z";

    // An agent caller (pid 4242, started 2000) whose own pane %1 is listed live, at SINCE.
    fn waiting_world() -> World {
        let w = agent_world();
        w.panes(&["%1|@1|100|aaaaaaaaaaaa|worker"]);
        w.clock(SINCE);
        w
    }

    #[test]
    fn report_done_is_recorded_and_keeps_the_lease() {
        let w = waiting_world();
        w.fakes.process(crate::testing::row(77, 1, 770));
        // A valid lease: the current owner, a future expiry.
        write(
            &w.folder(ID).join("waiting-77.json"),
            json!({"pid": 77, "start": 770, "since": SINCE,
                "owner": {"id": ID, "gen": "0"}, "until": "2999-01-01T00:00:00.0000000Z"}),
        );
        let r = crate::agents::view::report(&w.ctx, &w.request(&["report", "done"]));
        assert!(r.is_ok(), "done is a report word: {r:?}");
        let r = r.unwrap();
        assert_eq!(r["state"], "done");
        assert_eq!(
            read_json(&w.folder(ID).join("activity.json"))
                .unwrap()
                .unwrap()["state"],
            "done"
        );
        assert!(
            w.folder(ID).join("waiting-77.json").exists(),
            "done clears no lease"
        );
    }

    // The waiting files in the caller's folder, by name.
    fn waiting_names(w: &World) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(w.folder(ID))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("waiting-"))
            .collect();
        names.sort();
        names
    }

    const LATER: &str = "2999-01-01T00:00:00.0000000Z";

    // A lease of `owner` by `pid` (recorded start pid * 10) until `until`.
    fn lease(w: &World, pid: u32, owner: &str, until: &str) {
        write(
            &w.folder(ID).join(format!("waiting-{pid}.json")),
            json!({"pid": pid, "start": pid * 10, "since": SINCE, "owner": {"id": owner, "gen": "0"}, "until": until}),
        );
    }

    #[test]
    fn leases_name_their_owner_and_only_stale_ones_are_pruned() {
        let w = waiting_world();
        for pid in [78, 79, 80] {
            w.fakes
                .process(crate::testing::row(pid, 1, i64::from(pid) * 10));
        }
        let me = crate::agents::receipt::Owner {
            id: ID.into(),
            generation: "0".into(),
        };
        let mine = Waiting::start(&w.ctx, &w.caller(), ID, &me, 600).unwrap();
        let file = read_json(&mine.file).unwrap().unwrap();
        assert_eq!(
            (&file["owner"], file["until"].is_string()),
            (&json!({"id": ID, "gen": "0"}), true)
        );
        lease(&w, 77, ID, LATER); // dead pid
        lease(&w, 78, "bbbbbbbbbbbb", LATER); // another owner
        lease(&w, 79, ID, LATER); // an overlapping wait of this owner
        lease(&w, 80, ID, "1970-01-01T00:00:00.0000000Z"); // past until
        prune_leases(&w.ctx, &w.folder(ID));
        assert_eq!(waiting_names(&w), ["waiting-4242.json", "waiting-79.json"]);
        assert!(on_call(&w.ctx, &w.folder(ID)));
        drop(mine);
        assert_eq!(
            waiting_names(&w),
            ["waiting-79.json"],
            "each wait removes only its own"
        );
    }

    // PM call 13, R4: a held identity owns its pending generation; its lease stays valid when that generation is
    // promoted; an unreadable record names no owner and frees no lease.
    #[test]
    fn a_held_identity_owns_its_pending_generations_leases() {
        let w = waiting_world();
        let f = w.folder(ID);
        std::fs::create_dir_all(&f).unwrap();
        let held = json!({"lifecycle": 1, "id": ID, "generation": "0", "hcAddress": "t0", "hcName": "m0123abcd",
            "launcherArgv": ["x"], "sessionId": "s0",
            "pending": {"gen": 1, "kind": "retry", "hcName": "m4567cdef", "sessionId": "s1"}});
        crate::state::fs::write_json(&f.join("record.json"), &held).unwrap();
        let owner = owner_of_folder(&f).unwrap().unwrap();
        assert_eq!(
            (owner.id.as_str(), owner.generation.as_str()),
            (ID, "1"),
            "the subject is pending 1"
        );
        w.fakes.process(crate::testing::row(79, 1, 790));
        write(
            &f.join("waiting-79.json"),
            json!({"pid": 79, "start": 790, "since": SINCE, "owner": {"id": ID, "gen": "1"}, "until": LATER}),
        );
        assert!(
            on_call(&w.ctx, &f),
            "the held identity is on call through its own lease"
        );
        let mut promoted = held.clone();
        promoted.as_object_mut().unwrap().remove("pending");
        (
            promoted["generation"],
            promoted["hcName"],
            promoted["sessionId"],
        ) = (json!("1"), json!("m4567cdef"), json!("s1"));
        crate::state::fs::write_json(&f.join("record.json"), &promoted).unwrap();
        prune_leases(&w.ctx, &f);
        assert_eq!(
            waiting_names(&w),
            ["waiting-79.json"],
            "promotion keeps the lease of generation 1"
        );
        std::fs::write(f.join("record.json"), br#"{"lifec"#).unwrap();
        assert_eq!(owner_of_folder(&f).unwrap_err().check, "evidence");
        assert!(!on_call(&w.ctx, &f), "an unknown owner is not on call");
        prune_leases(&w.ctx, &f);
        assert_eq!(
            waiting_names(&w),
            ["waiting-79.json"],
            "an unreadable record frees no lease"
        );
    }

    // PM call 8, P2 (finding 5to8-2): a mailbox write stamps the generation the snapshot selected: a held identity
    // its pending generation, else the record generation, else "0". No file is read.
    #[test]
    fn a_mailbox_write_stamps_the_snapshot_generation() {
        let held = crate::agents::core_identity::Identity {
            id: ID.into(),
            name: "worker".into(),
            address: String::new(),
            generation: "1".into(),
            hc_name: "m4567cdef".into(),
            team: "k:$3".into(),
            session_id: crate::agents::fixture::SESSION_ID.into(),
            launcher: Vec::new(),
        };
        let r = json!({"generation": "0"});
        let got = |i: Option<&crate::agents::core_identity::Identity>, r: Option<&Value>| {
            snapshot_owner(ID, i, r).generation
        };
        assert_eq!(
            got(Some(&held), Some(&r)),
            "1",
            "the held identity, not the record"
        );
        assert_eq!(got(None, Some(&r)), "0");
        assert_eq!(
            got(None, Some(&json!({"generation": "4"}))),
            "4",
            "a record without an hc identity"
        );
        assert_eq!(got(None, None), "0", "a person");
    }

    #[test]
    fn send_and_status_keep_a_live_lease_of_the_current_owner_and_prune_a_reused_pid() {
        let w = waiting_world();
        // A pane-only 0.2 record (no hc fields, so no generation): `owner_of_folder` reads it as generation "0".
        w.agent_record(ID, json!({"pane": "%1", "parentPane": "%4"}));
        w.fakes.process(crate::testing::row(79, 1, 790));
        lease(&w, 79, ID, LATER); // live, this owner, future until
        w.fakes.process(crate::testing::row(81, 1, 999));
        lease(&w, 81, ID, LATER); // pid 81 reused: recorded start 810, now 999
        w.file("b.md", b"done");
        assert!(run(&w, send, &["send", "parent", "-BodyFile", "b.md"]).starts_with("ok "));
        assert_eq!(
            waiting_names(&w),
            ["waiting-79.json"],
            "send keeps the live lease, prunes the reused pid"
        );
        assert!(on_call(&w.ctx, &w.folder(ID)));
        let status = crate::agents::view::status;
        assert!(run(&w, status, &["status", "-BodyFile", "b.md"]).starts_with("ok "));
        assert_eq!(
            waiting_names(&w),
            ["waiting-79.json"],
            "status keeps it too"
        );
    }

    /// Member ID listening on %1 (hc, generation "0") and `n` passes of snapshot plus fence answers.
    fn listening(n: usize) -> (World, Value) {
        let w = waiting_world();
        let r = cf::member(
            &w,
            ID,
            json!({"pane":"%1","window":"@1","parentPane":"%4","startCommand":launch_command("%1")}),
        );
        passes(&w, &r, n);
        (w, r)
    }
    fn passes(w: &World, r: &Value, n: usize) {
        for _ in 0..n {
            poll(w, &[cf::wire(r)]);
            cf::online(w);
            cf::get(w, r["hcAddress"].as_str().unwrap(), cf::wire(r));
        }
    }
    /// A read and the sleeps it took.
    fn timed(w: &World, args: &[&str]) -> (Value, usize) {
        let before = w.fakes.slept().len();
        let out = run(w, read, args);
        let v: Value =
            serde_json::from_str(out.strip_prefix("ok ").unwrap_or_else(|| panic!("{out}")))
                .unwrap();
        (v, w.fakes.slept().len() - before)
    }

    #[test]
    fn a_wait_times_out_and_cancel_holds_until_new_mail() {
        let (w, r) = listening(3); // -Wait 1: passes at 0, 0.5 and 1.0 s
        let (t, slept) = timed(&w, &["read", "-Wait", "1"]);
        assert_eq!(t["timeout"], true, "SKILL.md re-arms on this");
        assert_eq!((&t["batch"], slept), (&Value::Null, 2));
        // A running wait sees the cancel of its owner at its next poll: one pass, then the cancel check.
        passes(&w, &r, 1);
        let file = w.folder(ID).join("oncall-cancel.json");
        let (f, base) = (file.clone(), w.fakes.slept().len());
        w.fakes.sleep_probe(move |n| {
            if n == base + 1 {
                write(&f, json!({"owner": {"id": ID, "gen": "0"}, "end": 0}));
            }
        });
        let (c, slept) = timed(&w, &["read", "-Wait", "600"]);
        assert_eq!(
            (&c["cancelled"], &c["batch"], slept),
            (&json!(true), &Value::Null, 1)
        );
        assert!(waiting_names(&w).is_empty(), "its lease removed");
        std::fs::remove_file(&file).unwrap();
        poll(&w, &[cf::wire(&r)]); // -Cancel: the reader snapshot only
        assert_eq!(timed(&w, &["read", "-Cancel"]).0["cancelled"], true);
        assert_eq!(
            read_json(&file).unwrap().unwrap()["owner"],
            json!({"id": ID, "gen": "0"})
        );
        poll(&w, &[cf::wire(&r)]);
        let (again, slept) = timed(&w, &["read", "-Wait", "600"]);
        assert_eq!(
            (&again["cancelled"], slept),
            (&json!(true), 0),
            "a later wait returns at once"
        );
        crate::agents::mail::add_mail(&w.ctx, &w.caller(), ID, "root", "%2", "new", None).unwrap();
        passes(&w, &r, 1);
        let (got, slept) = timed(&w, &["read", "-Wait", "600"]);
        assert_eq!(
            (got["ids"].as_array().map(Vec::len), slept),
            (Some(1), 0),
            "mail past end re-arms"
        );
    }

    fn other_wait(w: &World) {
        write(
            &w.folder(ID).join("waiting-77.json"),
            json!({"pid": 77, "start": 770, "since": SINCE}),
        );
    }

    #[test]
    fn read_wait_marks_the_agent_waiting_until_it_returns() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let mine = format!(
            r#"{{"pid":4242,"start":2000,"since":"{SINCE}","owner":{{"id":"{ID}","gen":"0"}},"until":"2026-10-29T12:00:01.0000000Z"}}"#
        );
        // A timeout: written under the agent lock before the first poll, there at every poll, gone after.
        let w = waiting_world();
        let r = cf::member(
            &w,
            ID,
            json!({"pane":"%1","window":"@1","parentPane":"%4","startCommand":launch_command("%1")}),
        );
        other_wait(&w);
        let file = w.folder(ID).join("waiting-4242.json");
        let seen = Rc::new(RefCell::new(Vec::new()));
        let (probe, during) = (file.clone(), seen.clone());
        w.fakes.sleep_probe(move |_| {
            during
                .borrow_mut()
                .push(std::fs::read_to_string(&probe).ok())
        });
        let agent = lock_key("agent", &w.caller(), ID);
        for _ in 0..3 {
            poll(&w, &[cf::wire(&r)]);
            fence(&w, &r);
        }
        w.fakes.clear_log();
        assert_eq!(
            run(&w, read, &["read", "-Wait", "1"]),
            r#"ok {"batch":null,"ids":[],"text":"","timeout":true}"#
        );
        assert_eq!(*seen.borrow(), vec![Some(mine.clone()); 2]);
        let locks = w.fakes.locks();
        assert_eq!(locks[..2], [format!("+{agent}"), format!("-{agent}")]);
        assert_eq!(
            locks[locks.len() - 2..],
            [format!("+{agent}"), format!("-{agent}")]
        );
        // An overlapping wait keeps its own file.
        assert_eq!(waiting_names(&w), ["waiting-77.json"]);

        // Mail ends the wait and the file goes.
        let w = waiting_world();
        let r = cf::member(
            &w,
            ID,
            json!({"pane":"%1","window":"@1","parentPane":"%4","startCommand":launch_command("%1")}),
        );
        let mailbox = w.state().join("mail").join(format!("{ID}.jsonl"));
        let file = w.folder(ID).join("waiting-4242.json");
        let probe = file.clone();
        w.fakes.sleep_probe(move |_| {
            assert!(probe.is_file());
            std::fs::write(
                &mailbox,
                "{\"id\":\"1\",\"from\":\"p\",\"pane\":\"%4\",\"at\":\"x\",\"body\":\"go\"}\n",
            )
            .unwrap();
        });
        for _ in 0..2 {
            poll(&w, &[cf::wire(&r)]);
            fence(&w, &r);
        }
        let reply = run(&w, read, &["read", "-Wait", "600"]);
        assert!(reply.contains(r#"\ngo\n""#), "{reply}");
        assert_eq!(waiting_names(&w), Vec::<String>::new());

        // A failed mail read or a failed notice pass (the reap's pane list) removes it too.
        let w = waiting_world();
        let r = cf::member(
            &w,
            ID,
            json!({"pane":"%1","window":"@1","parentPane":"%4","startCommand":launch_command("%1")}),
        );
        poll(&w, &[cf::wire(&r)]);
        w.fakes.lock_timeout("mail|");
        assert!(run(&w, read, &["read", "-Wait", "5"]).starts_with("2 lock"));
        assert_eq!(waiting_names(&w), Vec::<String>::new());
        let w = waiting_world();
        let r = cf::member(
            &w,
            ID,
            json!({"pane":"%1","window":"@1","parentPane":"%4","startCommand":launch_command("%1")}),
        );
        poll(&w, &[cf::wire(&r)]);
        w.fakes.mux_first("list-panes", 1, "", "down");
        assert!(run(&w, read, &["read", "-Wait", "5"]).starts_with("1 "));
        assert_eq!(waiting_names(&w), Vec::<String>::new());

        // -Wait 0 never waits, so it marks nothing.
        let w = waiting_world();
        let r = cf::member(
            &w,
            ID,
            json!({"pane":"%1","window":"@1","parentPane":"%4","startCommand":launch_command("%1")}),
        );
        let agent = lock_key("agent", &w.caller(), ID);
        w.fakes.clear_log();
        poll(&w, &[cf::wire(&r)]);
        fence(&w, &r);
        run(&w, read, &["read"]);
        assert!(!w.fakes.locks().iter().any(|l| l.contains(&agent)));

        // A human caller writes nothing.
        let w = World::human("");
        w.state();
        w.fakes.sleep_probe(|_| {});
        for _ in 0..3 {
            poll(&w, &[]);
        }
        run(&w, read, &["read", "-Wait", "1"]);
        let agents = w.state().join("agents");
        assert_eq!(std::fs::read_dir(agents).unwrap().count(), 0);
    }

    #[test]
    fn send_names_the_sender_and_a_claude_recipients_session() {
        // #92: the reply names who sent it and, for a Claude Code recipient, its session, so the sender's hooks can
        // wake it (herdr delivers a message as the recipient's next input); the mailbox stays the message itself.
        const CHILD: &str = "11111111-2222-4333-8444-555555555555";
        const ROOT: &str = "66666666-7777-4888-8999-aaaaaaaaaaaa";
        let mut w = rotated("", "%1");
        let tmux = w.ctx.env.get("TMUX").unwrap().to_string();
        w.ctx.env =
            crate::ctx::Env::from_pairs([("TMUX", tmux.clone()), ("TMUX_PANE", "%1".into())]);
        w.panes(&[
            "%1|@1|100||",
            "%9|@5|300|aaaaaaaaaaaa|a",
            "%8|@5|301|bbbbbbbbbbbb|b",
        ]);
        w.agent_record(ID, json!({"name": "a", "pane": "%9", "parentPane": "%1", "client": "claude",
            "sessionId": CHILD, "parentAgent": {"client": "claude", "pid": 1, "start": 1, "sessionId": ROOT}}));
        w.agent_record(
            "bbbbbbbbbbbb",
            json!({"name": "b", "pane": "%8", "parentPane": "%1", "client": "copilot",
            "sessionId": CHILD}),
        );
        let field = |reply: &str, key: &str| -> Value {
            let json: Value = serde_json::from_str(reply.strip_prefix("ok ").unwrap()).unwrap();
            json[key].clone()
        };
        // The root to a Claude agent: its session; the sender is the root, as its mail names it.
        let sent = run(&w, send, &["send", "a", "-Body", "hi"]);
        assert_eq!(
            (field(&sent, "from"), field(&sent, "toSessionId")),
            (json!("root"), json!(CHILD)),
            "{sent}"
        );
        // To a Copilot agent: no Claude session to wake.
        let sent = run(&w, send, &["send", "b", "-Body", "hi"]);
        assert_eq!(field(&sent, "toSessionId"), Value::Null, "{sent}");
        // From agent a to its parent, the root Claude session it was spawned from.
        w.ctx.env =
            crate::ctx::Env::from_pairs([("TMUX", tmux.clone()), ("TMUX_PANE", "%9".into())]);
        let sent = run(&w, send, &["send", "parent", "-Body", "done"]);
        assert_eq!(
            (field(&sent, "from"), field(&sent, "toSessionId")),
            (json!("a"), json!(ROOT)),
            "{sent}"
        );
        // From agent c, spawned by agent a: its parent is a, so a's own session, not the root's a recorded.
        w.panes(&[
            "%1|@1|100||",
            "%9|@5|300|aaaaaaaaaaaa|a",
            "%8|@5|301|bbbbbbbbbbbb|b",
            "%7|@5|302|cccccccccccc|c",
        ]);
        w.agent_record(
            "cccccccccccc",
            json!({"name": "c", "pane": "%7", "parentPane": "%9", "client": "copilot",
            "parentAgent": {"client": "claude", "pid": 2, "start": 2, "sessionId": ROOT}}),
        );
        w.ctx.env =
            crate::ctx::Env::from_pairs([("TMUX", tmux.clone()), ("TMUX_PANE", "%7".into())]);
        let sent = run(&w, send, &["send", "parent", "-Body", "done"]);
        assert_eq!(
            (field(&sent, "from"), field(&sent, "toSessionId")),
            (json!("c"), json!(CHILD)),
            "{sent}"
        );
        // A root that is a Copilot session: nothing to wake, whatever id its record holds.
        w.agent_record(
            ID,
            json!({"name": "a", "pane": "%9", "parentPane": "%1", "client": "claude",
            "sessionId": CHILD, "parentAgent": {"client": "copilot", "pid": 1, "start": 1, "sessionId": ROOT}}),
        );
        w.ctx.env =
            crate::ctx::Env::from_pairs([("TMUX", tmux.clone()), ("TMUX_PANE", "%9".into())]);
        let sent = run(&w, send, &["send", "parent", "-Body", "done"]);
        assert_eq!(field(&sent, "toSessionId"), Value::Null, "{sent}");
        // A session id that is not one is never answered.
        w.agent_record(
            ID,
            json!({"name": "a", "pane": "%9", "parentPane": "%1", "client": "claude",
            "sessionId": "x; rm -rf /", "parentAgent": {"client": "claude", "sessionId": "nope"}}),
        );
        let sent = run(&w, send, &["send", "parent", "-Body", "done"]);
        assert_eq!(field(&sent, "toSessionId"), Value::Null, "{sent}");
        w.ctx.env = crate::ctx::Env::from_pairs([("TMUX", tmux), ("TMUX_PANE", "%1".into())]);
        let sent = run(&w, send, &["send", "a", "-Body", "hi"]);
        assert_eq!(field(&sent, "toSessionId"), Value::Null, "{sent}");
    }

    #[test]
    fn send_and_status_prune_stale_leases() {
        let w = waiting_world();
        for pid in [4242, 77] {
            write(
                &w.folder(ID).join(format!("waiting-{pid}.json")),
                json!({"pid": pid, "start": 1, "since": SINCE}),
            );
        }
        w.file("b.md", b"done");
        assert!(run(&w, send, &["send", "parent", "-BodyFile", "b.md"]).starts_with("ok "));
        assert_eq!(waiting_names(&w), Vec::<String>::new());
        other_wait(&w);
        let status = crate::agents::view::status;
        assert!(run(&w, status, &["status", "-BodyFile", "b.md"]).starts_with("ok "));
        assert_eq!(waiting_names(&w), Vec::<String>::new());
    }

    // ---- #48: agents found by their pane and exact launch, never by their tags

    // rmux rotated the window: the root %1 carries A's tag, A's pane %9 carries B's, B's pane %8 none. The caller is
    // `pane` (tagged `tag`); every pane answers live in session $3.
    fn rotated(tag: &str, pane: &str) -> World {
        let mut w = World::human(tag);
        let tmux = w.ctx.env.get("TMUX").unwrap().to_string();
        w.ctx.env = crate::ctx::Env::from_pairs([("TMUX", tmux), ("TMUX_PANE", pane.to_string())]);
        w.agent_record(ID, json!({"name": "a", "pane": "%9", "parentPane": "%1"}));
        w.agent_record(
            "bbbbbbbbbbbb",
            json!({"name": "b", "pane": "%8", "parentPane": "%1"}),
        );
        w.panes(&[
            "%1|@1|100|aaaaaaaaaaaa|a",
            "%9|@5|300|bbbbbbbbbbbb|b",
            "%8|@5|301||",
            "%2|@1|102||",
        ]);
        w.recipients_live();
        w
    }

    #[test]
    fn rotated_tags_never_redirect_a_verb() {
        // From a clean pane, every verb reaches A at %9 and A's mailbox.
        let mut w = rotated("", "%2");
        w.fakes.mux_rule("#{window_active}", 0, "%9|1|1|1\n", "");
        w.fakes.mux_rule("show-buffer", 0, "a's screen\n", "");
        let a = cf::member(
            &w,
            ID,
            json!({"name":"a","startCommand":launch_command("%9")}),
        );
        let b = cf::member(
            &w,
            "bbbbbbbbbbbb",
            json!({"name":"b","pane":"%8","startCommand":launch_command("%8")}),
        );
        cf::online(&w);
        cf::listing(&w, a["team"].as_str().unwrap(), &[]);
        cf::get(&w, "taaaaaaaaaaaa", cf::wire(&a));
        cf::get(&w, "tbbbbbbbbbbbb", cf::wire(&b));
        let list = crate::agents::view::list(&w.ctx, &w.request(&["list"])).unwrap();
        w.fakes.hc_done().unwrap();
        let rows: Vec<String> = list["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                format!(
                    "{}@{}",
                    r["name"].as_str().unwrap(),
                    r["pane"].as_str().unwrap()
                )
            })
            .collect();
        assert_eq!(rows, ["a@%9", "b@%8"]);
        cf::member(
            &w,
            ID,
            json!({"name":"a","startCommand":launch_command("%9")}),
        );
        assert_eq!(
            run(&w, read, &["read", "a"]),
            r#"ok {"name":"a","pane":"%9","text":"a's screen"}"#
        );
        cf::online(&w);
        w.fakes
            .mux_first("show-options -qv -t $3 @mux-agents-sidebar", 0, "off\n", "");
        assert_eq!(
            run(&w, focus, &["focus", "a"]),
            r#"ok {"name":"a","pane":"%9","window":"@5","sidebar":null}"#
        );
        // Send by name applies the current-member rule: one hc look confirms a is the member its name means.
        cf::online(&w);
        cf::listing(&w, a["team"].as_str().unwrap(), &[]);
        cf::get(&w, "taaaaaaaaaaaa", cf::wire(&a));
        cf::get(&w, "tbbbbbbbbbbbb", cf::wire(&b));
        // A person's send to a member goes through the team (#103 4a): the reply names A by id, not by pane.
        let sent = run(&w, send, &["send", "a", "-Body", "hi"]);
        assert!(sent.starts_with(r#"ok {"to":"a","id":""#), "{sent}");
        assert!(
            sent.contains(r#""recipients":[{"name":"a","id":"aaaaaaaaaaaa"}]"#),
            "{sent}"
        );
        assert_eq!(read_mail_of(&w, ID), ["hi"]);
        assert!(read_mail_of(&w, "bbbbbbbbbbbb").is_empty());
        let r = cf::member(
            &w,
            ID,
            json!({"name":"a","startCommand":launch_command("%9")}),
        );
        cf::online(&w);
        cf::answer(&w, &["kill", "taaaaaaaaaaaa", "--json"], "{}");
        let mut t = cf::wire(&r);
        t["state"] = json!("finished");
        t["reason"] = json!("killed");
        cf::get(&w, "taaaaaaaaaaaa", t);
        assert_eq!(
            run(&w, stop, &["stop", "a"]),
            r#"ok {"name":"a","hcAddress":"taaaaaaaaaaaa","life":"ended"}"#
        );
        assert!(pane_kills(&w).is_empty());
        w.fakes.hc_done().unwrap();

        // #48 test 2: then B, tagless, is still addressable, and its own pane is B.
        w.panes(&["%1|@1|100|aaaaaaaaaaaa|a", "%8|@5|301||", "%2|@1|102||"]);
        let mut killed = cf::wire(&r);
        killed["state"] = json!("finished");
        killed["reason"] = json!("killed");
        cf::online(&w);
        cf::listing(&w, a["team"].as_str().unwrap(), &[]);
        cf::get(&w, "taaaaaaaaaaaa", killed);
        cf::get(&w, "tbbbbbbbbbbbb", cf::wire(&b));
        let sent = run(&w, send, &["send", "b", "-Body", "to b"]);
        assert!(sent.starts_with(r#"ok {"to":"b","id":""#), "{sent}");
        assert!(
            sent.contains(r#""recipients":[{"name":"b","id":"bbbbbbbbbbbb"}]"#),
            "{sent}"
        );
        assert_eq!(read_mail_of(&w, "bbbbbbbbbbbb"), ["to b"]);
        let tmux = w.ctx.env.get("TMUX").unwrap().to_string();
        w.ctx.env = crate::ctx::Env::from_pairs([("TMUX", tmux), ("TMUX_PANE", "%8".into())]);
        let caller = w.caller();
        let s = registry::Snapshot::take(&w.ctx, &caller).unwrap();
        assert!(matches!(s.own(&caller), registry::Own::Agent(ref p) if p.id == "bbbbbbbbbbbb"));
        // B's parent %1 carries A's tag: its mailbox is still pane-1. B is a member, so its send goes through the
        // team (#103 4a): one hc look proves it, and `parent` falls back to its parentPane.
        let mut killed = cf::wire(&r);
        killed["state"] = json!("finished");
        killed["reason"] = json!("killed");
        cf::online(&w);
        cf::listing(&w, a["team"].as_str().unwrap(), &[]);
        cf::get(&w, "taaaaaaaaaaaa", killed);
        cf::get(&w, "tbbbbbbbbbbbb", cf::wire(&b));
        let sent = run(&w, send, &["send", "parent", "-Body", "done"]);
        assert!(sent.starts_with(r#"ok {"to":"parent","id":""#), "{sent}");
        assert!(
            sent.contains(r#""from":"b","recipients":[{"name":"%1","id":null}]"#),
            "{sent}"
        );
        assert_eq!(read_mail_of(&w, "pane-1").last().unwrap(), "done");
    }

    #[test]
    fn a_rotated_tag_on_the_root_is_no_identity() {
        // The root %1 carries A's tag: it is no agent, so it neither sends as A nor spawns, and list reads pane-1.
        let w = rotated(ID, "%1");
        assert_eq!(
            run(&w, send, &["send", "a", "-Body", "x"]),
            "2 parent this agent pane has no record"
        );
        assert_eq!(
            run(
                &w,
                crate::agents::spawn::spawn,
                &["spawn", "-Client", "claude", "-Task", "hi"]
            ),
            "2 tagged an agent pane cannot spawn agents"
        );
        let caller = w.caller();
        add_mail(&w.ctx, &caller, "pane-1", "a", "%9", "for root", None).unwrap();
        for _ in 0..2 {
            add_mail(&w.ctx, &caller, ID, "a", "%9", "for a", None).unwrap();
        }
        let a = cf::member(
            &w,
            ID,
            json!({"name":"a","startCommand":launch_command("%9")}),
        );
        let b = cf::member(
            &w,
            "bbbbbbbbbbbb",
            json!({"name":"b","pane":"%8","startCommand":launch_command("%8")}),
        );
        cf::online(&w);
        cf::listing(&w, a["team"].as_str().unwrap(), &[]);
        cf::get(&w, "taaaaaaaaaaaa", cf::wire(&a));
        cf::get(&w, "tbbbbbbbbbbbb", cf::wire(&b));
        let list = crate::agents::view::list(&w.ctx, &w.request(&["list"])).unwrap();
        w.fakes.hc_done().unwrap();
        assert_eq!(list["agents"][0]["name"], "a");
        assert_eq!(list["agents"][0]["unread"], 1);
        assert!(read_mail_of(&w, ID).len() == 2);
    }

    #[test]
    fn read_and_focus_never_act_on_a_replaced_pane() {
        let w = World::human("");
        cf::member(&w, ID, json!({"startCommand":launch_command("%9")}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        let live = registry::Launch {
            command: launch_command("%9"),
            pid: None,
        }
        .live();
        // When the server checks, %9 runs `sh` (replaced), and it is restored right after: the condition never holds.
        let acted = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = acted.clone();
        let sh = registry::Launch {
            command: "sh".into(),
            pid: None,
        }
        .live();
        w.fakes.mux_script("if-shell", move |argv| {
            assert_eq!(argv[5], live);
            if argv[5] == sh {
                seen.set(true);
            }
            Ok(Output::default())
        });
        let captured = acted.clone();
        w.fakes.mux_script("show-buffer", move |_| {
            Ok(Output {
                code: if captured.get() { 0 } else { 1 },
                stdout: if captured.get() {
                    "text\n".into()
                } else {
                    String::new()
                },
                stderr: "no buffer".into(),
            })
        });
        // Read back after the select: restored, so live and its launch again, but never selected.
        w.fakes.mux_rule("#{window_active}", 0, "%9|0|0|1\n", "");
        w.fakes.clear_log();
        assert_eq!(
            run(&w, read, &["read", "worker"]),
            "2 target the agent's pane is no longer its launch"
        );
        assert_eq!(mux_verbs(&w).last().unwrap(), "delete-buffer");
        cf::online(&w);
        assert_eq!(
            run(&w, focus, &["focus", "worker"]),
            "2 target the agent's pane is no longer its launch"
        );
        assert!(!acted.get());
    }

    // A call that fails as a call: no server, a timeout.
    fn lost_server() -> Refusal {
        Refusal {
            code: 1,
            check: "endpoint",
            message: "rmux endpoint did not answer".into(),
            reason: None,
            leftover: None,
            details: Default::default(),
        }
    }

    #[test]
    fn read_deletes_its_buffer_whatever_call_fails() {
        // D9: the conditional capture or show-buffer fails as a call: `2 target`, and delete-buffer still runs.
        for failing in ["if-shell", "show-buffer"] {
            let w = World::human("");
            cf::member(&w, ID, json!({"startCommand":launch_command("%9")}));
            w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
            w.fakes.mux_rule("show-buffer", 0, "text\n", "");
            w.fakes.mux_script(failing, |_| Err(lost_server()));
            w.fakes.clear_log();
            assert_eq!(
                run(&w, read, &["read", "worker"]),
                "2 target the agent's pane is no longer its launch",
                "{failing}"
            );
            let calls = w.fakes.mux_calls();
            let last = calls.last().unwrap();
            assert_eq!(last[1], "delete-buffer", "{failing} {calls:?}");
            assert!(last[3].starts_with("mux-agents-"), "{failing}");
        }
    }

    // The read fence's own look under the mailbox lock: hc shows the reading member running.
    fn fence(w: &World, r: &Value) {
        cf::online(w);
        cf::get(w, r["hcAddress"].as_str().unwrap(), cf::wire(r));
    }
    fn poll(w: &World, terminals: &[Value]) {
        cf::online(w);
        cf::listing(w, &super::super::membership::team(&w.caller()), &[]);
        for t in terminals {
            cf::get(w, t["address"].as_str().unwrap(), t.clone());
        }
    }
    #[test]
    fn mailbox_read_reconciles_one_hc_snapshot_without_pane_authority() {
        for mode in ["running", "exited", "killed", "lost", "unknown"] {
            let w = World::human("");
            let r = cf::member(&w, ID, json!({}));
            write(&w.folder(ID).join("launcher.json"), json!({"exitCode":99}));
            let mut t = cf::wire(&r);
            match mode {
                "exited" | "killed" => {
                    t["state"] = json!("finished");
                    t["reason"] = json!(mode);
                    t["exit_code"] = json!(7);
                }
                "lost" => t["argv"] = json!(["/bin/sh"]),
                "unknown" => t["labels"]["mux.gen"] = json!("1"),
                _ => {}
            }
            w.panes(&[]);
            let notice = w.folder(ID).join("notice.json");
            let mut acked: Option<String> = None;
            for round in 0..2 {
                poll(&w, std::slice::from_ref(&t));
                w.fakes.clear_log();
                // Nothing is consumed without an ack (#103 4b-i): round 1 acks the batch of round 0.
                let ack = acked.take();
                let args: Vec<&str> = match &ack {
                    Some(b) => vec!["read", "-Deliver", "-Ack", b],
                    None => vec!["read"],
                };
                let result = read(&w.ctx, &w.request(&args)).unwrap();
                acked = result["batch"].as_str().map(String::from);
                let messages = entries(&result);
                let ended = ["exited", "killed"].contains(&mode);
                assert_eq!(
                    messages.len(),
                    usize::from(ended && round == 0),
                    "{mode}/{round}"
                );
                if let Some(message) = messages.first() {
                    assert_eq!(
                        message.as_str(),
                        if mode == "exited" {
                            "[mux-agent from worker (%9) #exit-aaaaaaaaaaaa]\nworker exited with code 7."
                        } else {
                            "[mux-agent from worker (%9) #exit-aaaaaaaaaaaa]\nworker was stopped."
                        }
                    );
                    let n = read_json(&notice).unwrap().unwrap();
                    assert_eq!(
                        n["kind"],
                        if mode == "exited" {
                            "exited"
                        } else {
                            "stopped"
                        }
                    );
                    assert_eq!(
                        n["exitCode"],
                        if mode == "exited" {
                            json!(7)
                        } else {
                            Value::Null
                        }
                    );
                }
                assert_eq!(notice.exists(), ended, "{mode}");
                assert_eq!(mux_verbs(&w), ["display-message", "list-panes"], "{mode}");
                assert_eq!(
                    w.fakes
                        .hc_calls()
                        .iter()
                        .map(|(_, a, _)| a[0].clone())
                        .collect::<Vec<_>>(),
                    ["--version", "ls", "get"]
                );
                w.fakes.hc_done().unwrap();
            }
        }
    }
    #[test]
    fn mailbox_wait_refreshes_once_per_poll_and_reads_terminal_notice() {
        let w = World::human("");
        let r = cf::member(&w, ID, json!({}));
        w.panes(&[]);
        let live = cf::wire(&r);
        let mut ended = live.clone();
        ended["state"] = json!("finished");
        ended["reason"] = json!("exited");
        ended["exit_code"] = json!(7);
        poll(&w, &[live]);
        poll(&w, &[ended]);
        w.fakes.clear_log();
        let result = read(&w.ctx, &w.request(&["read", "-Wait", "3"])).unwrap();
        assert_eq!(result["ids"].as_array().unwrap().len(), 1);
        assert_eq!(
            entries(&result)[0],
            "[mux-agent from worker (%9) #exit-aaaaaaaaaaaa]\nworker exited with code 7."
        );
        assert_eq!(w.fakes.slept(), [500]);
        assert_eq!(
            w.fakes
                .mux_calls()
                .iter()
                .filter(|a| a[1] == "list-panes")
                .count(),
            2
        );
        assert_eq!(w.fakes.hc_calls().len(), 6);
        w.fakes.hc_done().unwrap();
    }
    #[test]
    fn mailbox_wait_drops_marker_on_reconciliation_errors() {
        for mode in ["corrupt-record", "notice-lock"] {
            let w = waiting_world();
            let r = cf::member(
                &w,
                ID,
                json!({"pane":"%1","window":"@1","parentPane":"%4","startCommand":launch_command("%1")}),
            );
            let folder = w.folder(ID);
            let waiting = folder.join("waiting-4242.json");
            if mode == "corrupt-record" {
                poll(&w, &[cf::wire(&r)]);
                fence(&w, &r);
                w.fakes.sleep_probe(move |_| {
                    assert!(waiting.is_file());
                    std::fs::write(folder.join("record.json"), "{").unwrap();
                });
            } else {
                let child = cf::member(&w, "bbbbbbbbbbbb", json!({"name":"child","pane":"%9"}));
                let mut ended = cf::wire(&child);
                ended["state"] = json!("finished");
                ended["reason"] = json!("exited");
                poll(&w, &[cf::wire(&r), ended]);
                w.fakes.lock_timeout("notice|");
            }
            let e = read(&w.ctx, &w.request(&["read", "-Wait", "1"])).unwrap_err();
            assert_eq!(
                (e.code, e.check),
                if mode == "corrupt-record" {
                    (1, "evidence")
                } else {
                    (2, "lock")
                }
            );
            assert!(waiting_names(&w).is_empty());
            w.fakes.hc_done().unwrap();
        }
    }
    #[test]
    fn mailbox_root_reads_existing_mail_during_core_failure() {
        for mode in ["missing", "ls-error", "get-error"] {
            let w = World::human("");
            let r = cf::member(&w, ID, json!({}));
            w.panes(&[]);
            let c = w.caller();
            add_mail(&w.ctx, &c, "pane-1", "parent", "%2", "keep", None).unwrap();
            if mode != "missing" {
                cf::online(&w);
                if mode == "ls-error" {
                    w.fakes.hc_expect(
                        &[
                            "ls",
                            "--all",
                            "--label",
                            "mux.managed=1",
                            "--label",
                            &format!("mux.team={}", super::super::membership::team(&c)),
                            "--json",
                        ],
                        Err(Refusal::internal("ls failed")),
                    );
                } else {
                    cf::listing(&w, &super::super::membership::team(&c), &[]);
                    w.fakes.hc_expect(
                        &["get", r["hcAddress"].as_str().unwrap(), "--json"],
                        Err(Refusal::internal("get failed")),
                    );
                }
            }
            let reply = read(&w.ctx, &w.request(&["read"])).unwrap();
            assert_eq!(reply["ids"].as_array().unwrap().len(), 1);
            assert!(entries(&reply)[0].ends_with("\nkeep"));
            assert!(!w.folder(ID).join("notice.json").exists());
            w.fakes.hc_done().unwrap();
        }
    }

    #[test]
    fn send_parent_reaches_a_parent_named_by_id() {
        let w = World::agent(ID);
        w.agent_record(
            ID,
            json!({"pane": "%1", "parentPane": "%4", "depth": 2,
            "parentAgent": {"client": "claude", "id": "bbbbbbbbbbbb"}}),
        );
        w.panes(&["%1|@1|100|aaaaaaaaaaaa|worker", "%4|@1|101||"]);
        w.recipients_live();
        w.file("b.md", b"done");
        assert!(run(&w, send, &["send", "parent", "-BodyFile", "b.md"]).starts_with("ok "));
        assert_eq!(read_mail_of(&w, "pane-4"), ["done"]);
    }

    // #103 review: a member's message names the member it came from, so a team bar counts it by identity, not by pane.
    #[test]
    fn a_members_message_names_its_member_id() {
        let w = agent_world();
        w.file("b.md", b"ok");
        assert!(run(&w, send, &["send", "parent", "-BodyFile", "b.md"]).starts_with("ok "));
        let mailbox = std::fs::read_to_string(w.state().join("mail").join("pane-4.jsonl")).unwrap();
        let line: Value = serde_json::from_str(mailbox.lines().last().unwrap()).unwrap();
        assert_eq!(
            (&line["from"], &line["fromId"]),
            (&json!("worker"), &json!(ID))
        );
    }

    // a3c call 42: outside a team a pane caller is the parent only by its own, non-empty pane; an empty pane matches
    // nothing, not even an empty `parentPane`. A person needs no parent.
    #[test]
    fn an_empty_pane_is_nobodys_parent() {
        let w = crate::agents::fixture::World::agent("");
        let mut caller = w.caller();
        assert!(caller.member.is_none(), "a pane caller");
        let check = |c: &crate::caller::Caller, pane: &str| {
            parent_or_human(c, &json!({"parentPane": pane})).map_err(|e| (e.code, e.check))
        };
        assert_eq!(check(&caller, "%1"), Ok(()), "its own pane");
        assert_eq!(check(&caller, "%8"), Err((2, "parent")), "another pane");
        caller.pane = String::new();
        assert_eq!(check(&caller, ""), Err((2, "parent")), "an empty pane");
        caller.kind = crate::caller::Kind::Human;
        assert_eq!(check(&caller, "%8"), Ok(()), "a person");
    }
}
