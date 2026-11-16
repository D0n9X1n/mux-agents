//! Team messaging (#103 phase 4a): who a target names, one durable intent per send (also its quota charge), and
//! durable delivery of one message id to each mailbox. Intents: `teams/<sha16(session)>/sends/<sender>/<id>.json`,
//! read and written under the team lock.
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value, json};

use super::membership::Snapshot;
use crate::caller::{Caller, Kind};
use crate::cli::Request;
use crate::ctx::Ctx;
use crate::error::{Res, refuse};
use crate::util::{parse_utc, utc_o};

pub const QUOTA: usize = 120;
const WINDOW: Duration = Duration::from_secs(3600);
const KEEP: Duration = Duration::from_secs(86_400);
pub const HOP_LIMIT: u64 = 4;

pub fn sends_root(server_dir: &Path, session: &str) -> PathBuf {
    server_dir
        .join("teams")
        .join(&crate::util::sha256_hex(session)[..16])
        .join("sends")
}

pub fn sends_dir(server_dir: &Path, session: &str, sender: &str) -> PathBuf {
    sends_root(server_dir, session).join(sender)
}

/// Each `<id>.json` of `dir` with its content (None: unreadable).
fn intents(dir: &Path) -> Res<Vec<(PathBuf, Option<Value>)>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            out.push((
                path.clone(),
                crate::state::fs::read_json(&path).ok().flatten(),
            ));
        }
    }
    Ok(out)
}

/// Under the team lock: prunes done intents older than a day, then (when `limited`) refuses the 121st send of the
/// last hour with `retryAt` = oldest `at` + 3600 s. An unreadable intent counts and is never pruned (M4).
pub fn charge(ctx: &Ctx, dir: &Path, limited: bool) -> Res<()> {
    let now = ctx.clock.now();
    let mut recent: Vec<SystemTime> = Vec::new();
    for (path, value) in intents(dir)? {
        let at = value
            .as_ref()
            .and_then(|v| v.get("at"))
            .and_then(Value::as_str)
            .and_then(parse_utc);
        let done = value
            .as_ref()
            .and_then(|v| v.get("done"))
            .and_then(Value::as_bool)
            == Some(true);
        match at {
            Some(at) if done && now.duration_since(at).is_ok_and(|age| age > KEEP) => {
                let _ = std::fs::remove_file(&path);
            }
            Some(at) if now.duration_since(at).map_or(true, |age| age < WINDOW) => recent.push(at),
            Some(_) => {}
            None => recent.push(now),
        }
    }
    if limited && recent.len() >= QUOTA {
        let retry = utc_o(*recent.iter().min().expect("120 intents") + WINDOW);
        return Err(refuse::<()>(
            2,
            "quota",
            format!("120 messages in the last hour; retry at {retry}"),
        )
        .unwrap_err()
        .with("retryAt", retry));
    }
    Ok(())
}

/// A new 16-hex id.
pub fn new_id() -> Res<String> {
    Ok(crate::util::guid()?.replace("-", "")[..16].to_string())
}

/// An id no sender of this team (`root` = its sends folder) has used, drawn from `draw`.
pub fn fresh_id(root: &Path, mut draw: impl FnMut() -> Res<String>) -> Res<String> {
    loop {
        let id = draw()?;
        let taken = std::fs::read_dir(root)
            .map(|s| {
                s.filter_map(Result::ok)
                    .any(|s| s.path().join(format!("{id}.json")).exists())
            })
            .unwrap_or(false);
        if !taken {
            return Ok(id);
        }
    }
}

/// The server folder above `teams/<sha16>/sends`.
fn base_of(root: &Path) -> &Path {
    root.ancestors().nth(3).unwrap_or(root)
}

/// Writes intent `value` (its `id`) for `sender` durably: its folders flushed up to the server folder, then the file
/// flushed, renamed, and its folder flushed.
pub fn write_intent(root: &Path, sender: &str, value: &Value) -> Res<()> {
    let dir = root.join(sender);
    crate::state::fs::durable_dir(&dir, base_of(root))?;
    crate::state::fs::write_json_durable(
        &dir.join(format!("{}.json", super::text(value.get("id")))),
        value,
    )
}

/// 1 `mail` naming the send by its id: a retry with `-Id <id>` finishes it (F4, M5).
fn unsaved(id: &str, delivered: &[String]) -> crate::error::Refusal {
    refuse::<()>(
        1,
        "mail",
        format!("the message could not be saved; retry with -Id {id}"),
    )
    .unwrap_err()
    .with("id", id.to_string())
    .with("delivered", json!(delivered))
}

/// Flushes intent `id` again: its file, its folder, and every folder entry up to the server folder.
fn settle(root: &Path, sender: &str, id: &str) -> Res<()> {
    crate::state::fs::sync_file(&root.join(sender).join(format!("{id}.json")))?;
    crate::state::fs::sync_dir(&root.join(sender))?;
    crate::state::fs::durable_dir(&root.join(sender), base_of(root))
}

pub enum Found {
    Mine(Value),
    Other,
    None,
}

/// Intent `id`: one of this sender, one of another sender, or none.
pub fn find_intent(root: &Path, sender: &str, id: &str) -> Res<Found> {
    let file = format!("{id}.json");
    if let Some(v) = crate::state::fs::read_json(&root.join(sender).join(&file))? {
        return Ok(Found::Mine(v));
    }
    let other = std::fs::read_dir(root)
        .map(|s| {
            s.filter_map(Result::ok)
                .any(|s| s.path().join(&file).exists())
        })
        .unwrap_or(false);
    Ok(if other { Found::Other } else { Found::None })
}

/// Marks intent `id` done once every copy is durable; best effort (an unmarked intent stays retryable, never pruned).
pub fn mark_done(root: &Path, sender: &str, id: &str) {
    let path = root.join(sender).join(format!("{id}.json"));
    if let Ok(Some(mut v)) = crate::state::fs::read_json(&path) {
        v["done"] = json!(true);
        let _ = crate::state::fs::write_json_durable(&path, &v);
    }
}

/// A running member of the team.
#[derive(Debug, Clone)]
pub struct Member {
    pub id: String,
    pub name: String,
    pub record: Value,
}

impl Member {
    pub fn mailbox(&self) -> String {
        super::mail::member_mailbox(&self.id, &self.record)
    }
}

/// The team (`membership::team(caller)`, with or without a team record: the members of the session): every committed
/// member with no end notice, not superseded, that hc shows running; in snapshot order.
pub fn roster(s: &super::membership::Snapshot) -> Vec<Member> {
    let superseded = s.superseded();
    s.facts
        .local
        .iter()
        .filter(|o| {
            o.life == super::lifecycle::Life::Running && !superseded.contains(&o.identity.id)
        })
        .filter_map(|o| {
            s.records
                .get(&o.identity.id)
                .filter(|st| st.committed && !st.noticed)
                .map(|st| Member {
                    id: o.identity.id.clone(),
                    name: o.identity.name.clone(),
                    record: st.record.clone(),
                })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Name(String),
    Parent,
    Leader,
    Children,
    Team,
    Pane(String),
}

/// The target word of a send; anything else is 2 `usage`.
pub fn target(word: &str) -> Res<Target> {
    Ok(match word {
        "parent" => Target::Parent,
        "leader" => Target::Leader,
        "children" => Target::Children,
        "team" => Target::Team,
        w if super::pane_id(w) => Target::Pane(w.into()),
        w if super::agent_name(w) => Target::Name(w.into()),
        _ => return Err(super::usage("send")),
    })
}

#[derive(Debug)]
pub enum Pick {
    Members(Vec<Member>),
    Pane(String),
    Unknown,
}

pub const NO_NAME: &str = "no agent of this team has that name";
const NO_LEADER: &str = "this team has no leader";
const NO_OTHER: &str = "no other member to send to";
const NO_PARENT: &str = "only an agent has a parent";

/// Who `t` names for `sender` (a member id; None for a person) on `roster`.
pub fn pick(roster: &[Member], sender: Option<&str>, t: &Target) -> Res<Pick> {
    let leader = roster.iter().find(|m| super::tree::is_leader(&m.record));
    let parent_of = |m: &Member| super::text(m.record.get("parentAgent").and_then(|p| p.get("id")));
    let others = |keep: &dyn Fn(&Member) -> bool| -> Res<Pick> {
        let v: Vec<Member> = roster
            .iter()
            .filter(|m| Some(m.id.as_str()) != sender && keep(m))
            .cloned()
            .collect();
        if v.is_empty() {
            refuse(2, "target", NO_OTHER)
        } else {
            Ok(Pick::Members(v))
        }
    };
    match t {
        Target::Name(n) => Ok(roster
            .iter()
            .find(|m| m.name == *n)
            .map_or(Pick::Unknown, |m| Pick::Members(vec![m.clone()]))),
        Target::Pane(p) => Ok(Pick::Pane(p.clone())),
        Target::Leader => match leader {
            Some(l) => Ok(Pick::Members(vec![l.clone()])),
            None => refuse(2, "target", NO_LEADER),
        },
        Target::Parent => {
            let Some(me) = sender.and_then(|s| roster.iter().find(|m| m.id == s)) else {
                return refuse(2, "parent", NO_PARENT);
            };
            let parent = parent_of(me);
            if let Some(p) = roster.iter().find(|m| !parent.is_empty() && m.id == parent) {
                return Ok(Pick::Members(vec![p.clone()]));
            }
            match me
                .record
                .get("parentPane")
                .and_then(Value::as_str)
                .filter(|p| super::pane_id(p))
            {
                Some(pane) => Ok(Pick::Pane(pane.into())),
                None => refuse(2, "parent", NO_PARENT),
            }
        }
        Target::Children => {
            let parent = match (sender, leader) {
                (Some(s), _) => s.to_string(),
                (None, Some(l)) => l.id.clone(),
                (None, None) => return refuse(2, "target", NO_LEADER),
            };
            others(&|m| parent_of(m) == parent)
        }
        Target::Team => others(&|_| true),
    }
}

/// What claims the caller's pane: no agent record (a person), a member record, one committed 0.2 agent record, or
/// records that prove no one (uncommitted, or more than one).
enum Claim {
    None,
    Member,
    Agent,
    Unproven,
}

/// The agent records of this session whose pane is the caller's. A pane is a person's only when none claims it.
fn claim(ctx: &Ctx, caller: &Caller) -> Res<Claim> {
    use super::gens::Member;
    let agents = super::server_dir(ctx, caller).join("agents");
    let mut found = Vec::new();
    // R5: one pass over `gens::members`, so links reach `member_of`.
    for (id, folder, member) in super::gens::members(&agents)? {
        let (record, life_committed) = match &member {
            Member::Absent => continue,
            // PM calls 2, 7 (A3a call 6): the caller's own server key, the only key read; session unknown.
            Member::Unreadable(e) => return Err(e.clone()),
            Member::Pre(r) => (r.clone(), None),
            Member::Life(g) => {
                // Another session is skipped before its status or view is read, so its torn files block no one.
                if g.record().get("session").and_then(Value::as_str)
                    != Some(caller.session.as_str())
                {
                    continue;
                }
                // P4 (a3c call 36): one status snapshot; its commit decides. A3c Task 1 changes the next call to
                // `presented(g, &s)` (A3b-2 header concern 1).
                let s = g.status()?;
                (
                    super::team_state::presented(&folder, g.record().clone())?,
                    Some(s.committed),
                )
            }
        };
        let here = super::text(record.get("pane")) == caller.pane
            && record
                .get("session")
                .and_then(Value::as_str)
                .is_none_or(|s| s == caller.session);
        if here {
            // As `registry::own_record`: its own id is its folder's, its session the caller's (QA round 2, fix 2).
            let valid = record.get("id").and_then(Value::as_str) == Some(id.as_str())
                && record.get("session").and_then(Value::as_str) == Some(caller.session.as_str());
            let committed = match life_committed {
                Some(c) => c,
                None => super::member_committed(&folder, &member)?,
            };
            found.push((record.get("hcAddress").is_some(), committed, valid));
        }
    }
    // Every record that claims the pane counts, hc or not: two make its owner ambiguous (QA round 2, fix 1).
    Ok(match found.as_slice() {
        [] => Claim::None,
        [(true, _, _)] => Claim::Member,
        [(false, true, true)] => Claim::Agent,
        _ => Claim::Unproven,
    })
}

/// What `send` does: the team path ran, or the 0.2 path runs (with the snapshot already taken, if any).
pub enum Routed {
    Sent(Map<String, Value>),
    Legacy(Option<Box<Snapshot>>),
}

/// The sender: a member (`id`) or a person (`id` None, `key` its pane mailbox).
struct Sender {
    id: Option<String>,
    name: String,
    key: String,
    pane: String,
}

/// `-Re`/`-Id`: 8-32 lowercase hex digits, else 2 `usage`.
fn flag(request: &Request, name: &str) -> Res<Option<String>> {
    match request.value(name) {
        None => Ok(None),
        Some(v)
            if (8..=32).contains(&v.len())
                && v.bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) =>
        {
            Ok(Some(v.into()))
        }
        Some(_) => Err(super::usage("send")),
    }
}

/// A team send: members (hc- or pane-proven) and persons route through the team under the team lock (M1, M2). A
/// non-member agent pane, and a person whose name the team lacks, get `Legacy`. The 0.2 path has no intents and
/// no replies, so `-Id` and `-Re` refuse there before anything is written (QA fix 2).
pub fn route(ctx: &Ctx, caller: &Caller, request: &Request, body: &str) -> Res<Routed> {
    let (re, id) = (flag(request, "Re")?, flag(request, "Id")?);
    if re.is_some() && id.is_some() {
        return Err(super::usage("send"));
    }
    match route_team(ctx, caller, request, body, re, id)? {
        Routed::Legacy(_) if request.value("Re").is_some() || request.value("Id").is_some() => {
            refuse(2, "usage", "-Id and -Re work only inside a team")
        }
        routed => Ok(routed),
    }
}

fn route_team(
    ctx: &Ctx,
    caller: &Caller,
    request: &Request,
    body: &str,
    re: Option<String>,
    id: Option<String>,
) -> Res<Routed> {
    let word = request
        .positional
        .first()
        .map(String::as_str)
        .unwrap_or_default();
    let team = caller.member.is_some()
        || super::membership::locals(ctx, caller)?
            .iter()
            .any(|l| l.stored.record.get("hcAddress").is_some());
    if !team {
        return Ok(Routed::Legacy(None));
    }
    // What the caller is decides 0.2 or team before any hc look (QA fix 3); a pane is a person's only when no agent
    // record claims it (QA fix 1).
    // A pane an hc member record claims is that member's or no one's: its failed proof never falls back to 0.2.
    let claimed_member = match caller.member {
        Some(_) => false,
        None => match claim(ctx, caller)? {
            Claim::Unproven => return Err(super::tree::tagged()),
            Claim::Agent => return Ok(Routed::Legacy(None)),
            Claim::Member => true,
            Claim::None => false,
        },
    };
    let t = target(word)?;
    // Lock order (M2): team, then (in deliver) the agent of the sender, then each mailbox.
    super::lock(
        ctx,
        &super::lock_key("spawn", caller, &caller.session),
        || {
            let s = Snapshot::take(ctx, caller)?;
            if let Some(e) = &s.core_error {
                return Err(e.clone());
            }
            let me = match s.reader(caller) {
                Ok(me) => me,
                // Unreadable, Lost, Ended or at another generation: refused, never sent as 0.2 (Tech Lead, HIGH).
                Err(_) if claimed_member => return Err(super::tree::tagged()),
                Err(_) if caller.member.is_none() && caller.kind == Kind::Agent => {
                    return Ok(Routed::Legacy(Some(Box::new(s))));
                }
                Err(e) => return Err(e),
            };
            let sender = match &me.record {
                Some(r) if r.get("hcAddress").is_some() => Sender {
                    id: Some(me.key.clone()),
                    name: me.name.clone(),
                    key: super::mail::member_mailbox(&me.key, r),
                    pane: caller.pane.clone(),
                },
                Some(_) => return Ok(Routed::Legacy(Some(Box::new(s)))),
                None => Sender {
                    id: None,
                    name: me.name.clone(),
                    key: me.key.clone(),
                    pane: caller.pane.clone(),
                },
            };
            let root = sends_root(&super::server_dir(ctx, caller), &caller.session);
            // A member is keyed by its id (a resumed member has a new id, so its own quota); a person by its pane mailbox.
            let owner = sender.id.clone().unwrap_or_else(|| sender.key.clone());
            let hash = crate::util::sha256_hex(body);
            let tx = || -> Res<Option<Map<String, Value>>> {
                let intent = match &id {
                    Some(id) => match find_intent(&root, &owner, id)? {
                        Found::Mine(i) if super::text(i.get("bodyHash")) == hash => {
                            settle(&root, &owner, id).map_err(|_| unsaved(id, &[]))?;
                            i
                        }
                        Found::Mine(_) | Found::Other => {
                            return refuse(2, "id", "that id belongs to a different send");
                        }
                        Found::None => return refuse(2, "id", "no send with that id"),
                    },
                    None => {
                        let Some((recipients, hop, cast)) =
                            plan(ctx, caller, &s, &sender, &t, re.as_deref())?
                        else {
                            return Ok(None);
                        };
                        charge(ctx, &root.join(&owner), sender.id.is_some())?;
                        let i = json!({"id": fresh_id(&root, new_id)?, "sender": owner,
                            "at": utc_o(ctx.clock.now()), "bodyHash": hash, "cast": cast, "re": re, "hop": hop,
                            "recipients": recipients});
                        if let Err(e) = write_intent(&root, &owner, &i) {
                            // Renamed but maybe not flushed: the intent may outlive a crash, and its id is the way
                            // back.
                            let id = super::text(i.get("id"));
                            return Err(if root.join(&owner).join(format!("{id}.json")).exists() {
                                unsaved(&id, &[])
                            } else {
                                e
                            });
                        }
                        crash_point()?;
                        i
                    }
                };
                deliver(ctx, caller, &s, &sender, &root, &owner, &intent, body, word).map(Some)
            };
            // Team, then the agent lock of a member sender, then each mailbox: the -Re lookup and every copy inside
            // it.
            let sent = match &sender.id {
                Some(m) => super::lock(ctx, &super::lock_key("agent", caller, m), tx)?,
                None => tx()?,
            };
            match sent {
                Some(reply) => Ok(Routed::Sent(reply)),
                None => Ok(Routed::Legacy(Some(Box::new(s)))),
            }
        },
    )
}

type Plan = (Vec<Value>, u64, Option<&'static str>);

const NOT_SENDER: &str = "a reply goes only to the message's sender";

/// Recipients `[{name, id, box, session, pane?}]`, hop and cast; None: a person names what the team lacks (0.2).
fn plan(
    ctx: &Ctx,
    caller: &Caller,
    s: &Snapshot,
    sender: &Sender,
    t: &Target,
    re: Option<&str>,
) -> Res<Option<Plan>> {
    if re.is_some() && matches!(t, Target::Pane(_) | Target::Team | Target::Children) {
        return refuse(2, "re", NOT_SENDER);
    }
    let answered = match re {
        Some(id) => match super::mail::find_message(ctx, caller, &sender.key, id)? {
            Some(m) => Some(m),
            None => return refuse(2, "re", "no message with that id in your mailbox"),
        },
        None => None,
    };
    let picked = match t {
        // As 0.2: the one running member of that name, a lost or unknown rival refusing.
        Target::Name(n) => match s.running_named(n) {
            Some((id, record)) => Pick::Members(vec![Member {
                id,
                name: n.clone(),
                record,
            }]),
            None if sender.id.is_none() && re.is_none() => return Ok(None),
            None => return refuse(2, "target", NO_NAME),
        },
        _ => pick(&roster(s), sender.id.as_deref(), t)?,
    };
    let recipients: Vec<Value> = match picked {
        Pick::Members(v) => v
            .iter()
            .map(|m| {
                json!({"name": m.name, "id": m.id, "box": m.mailbox(),
                    "session": claude_session(&m.record)})
            })
            .collect(),
        Pick::Pane(p) => vec![pane_box(ctx, caller, &p)?],
        Pick::Unknown => return refuse(2, "target", NO_NAME),
    };
    let hop = match &answered {
        None => 0,
        Some(m) => {
            let from = super::text(m.get("fromBox"));
            if recipients.len() != 1
                || from.is_empty()
                || super::text(recipients[0].get("box")) != from
            {
                return refuse(2, "re", NOT_SENDER);
            }
            let hop = m.get("hop").and_then(Value::as_u64).unwrap_or(0) + 1;
            if hop >= HOP_LIMIT {
                return refuse(2, "hop", "this reply would be hop 4; the thread is closed");
            }
            hop
        }
    };
    let cast = match t {
        Target::Team => Some("team"),
        Target::Children => Some("children"),
        _ => None,
    };
    Ok(Some((recipients, hop, cast)))
}

/// A Claude Code session id of a record, else null.
fn claude_session(record: &Value) -> Value {
    record
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|s| crate::checks::session_id(s))
        .filter(|_| record.get("client").and_then(Value::as_str) == Some("claude"))
        .map_or(Value::Null, Value::from)
}

/// A pane target: a live pane of this session (0.2 check); its mailbox is the running agent there, else `pane-<n>`.
fn pane_box(ctx: &Ctx, caller: &Caller, pane: &str) -> Res<Value> {
    super::message::live_pane(ctx, caller, pane)?;
    let snapshot = super::registry::Snapshot::take(ctx, caller)?;
    let key = super::registry::mailbox_of(&snapshot, pane)?;
    let agent = snapshot.at(pane);
    Ok(
        json!({"name": agent.as_ref().map_or(pane.to_string(), |a| a.name.clone()),
        "id": agent.as_ref().map(|a| a.id.clone()), "box": key,
        "session": agent.as_ref().map_or(Value::Null, |a| claude_session(&a.record)), "pane": pane}),
    )
}

#[cfg(test)]
thread_local! {
    static CRASH_AFTER_INTENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test only: the next send stops right after its intent is durable, as a crash at that boundary would.
#[cfg(test)]
pub fn crash_after_intent(on: bool) {
    CRASH_AFTER_INTENT.with(|c| c.set(on));
}

fn crash_point() -> Res<()> {
    #[cfg(test)]
    if CRASH_AFTER_INTENT.with(|c| c.get()) {
        return Err(crate::error::Refusal::internal("crash after intent"));
    }
    Ok(())
}

/// W7: with the option on, one `hc send` per recipient with an hc address; failures are dropped (the mail is durable).
/// Call 19 (R1, R5): the address is the classified snapshot's identity, never a reread of `record.json`.
fn notices(ctx: &Ctx, caller: &Caller, s: &Snapshot, sender: &Sender, recipients: &[Value]) {
    let on = crate::mux::native(
        ctx,
        &[
            "show-options",
            "-v",
            "-t",
            &caller.session,
            super::message::NOTIFY_OPTION,
        ],
        true,
    )
    .is_ok_and(|o| o.code == 0 && o.stdout.trim() == "on");
    if !on {
        return;
    }
    let Ok(core) = crate::hc::connect(ctx) else {
        return;
    };
    for r in recipients {
        let id = super::text(r.get("id"));
        if let Some(address) = s
            .facts
            .local
            .iter()
            .find(|o| o.identity.id == id)
            .map(|o| o.identity.address.as_str())
            .filter(|a| !a.is_empty())
        {
            let _ = core.send(
                address,
                &format!("mail from {}: run mux-agent read", sender.name),
            );
        }
    }
}

/// Delivers intent `intent` (one id) to each recipient durably (M5), then marks it done and writes `last-sent.json`;
/// a failed save refuses 1 `mail` with `id` and `delivered`. The caller holds the agent lock of a member sender.
#[allow(clippy::too_many_arguments)]
fn deliver(
    ctx: &Ctx,
    caller: &Caller,
    s: &Snapshot,
    sender: &Sender,
    root: &Path,
    owner: &str,
    intent: &Value,
    body: &str,
    word: &str,
) -> Res<Map<String, Value>> {
    let id = super::text(intent.get("id"));
    let recipients = intent
        .get("recipients")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let run = || -> Res<()> {
        let mut delivered = Vec::new();
        for r in &recipients {
            let mut message = Map::new();
            message.insert("id".into(), id.clone().into());
            message.insert("from".into(), sender.name.clone().into());
            // A member's line names it, as its 0.2 line does: the side bar counts unread mail by identity (c723913).
            if let Some(member) = &sender.id {
                message.insert("fromId".into(), member.clone().into());
            }
            for (k, v) in [
                ("pane", json!(sender.pane)),
                ("at", intent["at"].clone()),
                ("body", json!(body)),
                ("fromBox", json!(sender.key)),
                ("re", intent["re"].clone()),
                ("hop", intent["hop"].clone()),
                ("cast", intent["cast"].clone()),
            ] {
                message.insert(k.into(), v);
            }
            let message = Value::Object(message);
            if super::mail::deliver_once(ctx, caller, &super::text(r.get("box")), &message).is_err()
            {
                return Err(unsaved(&id, &delivered));
            }
            delivered.push(super::text(r.get("name")));
        }
        mark_done(root, owner, &id);
        notices(ctx, caller, s, sender, &recipients);
        if let Some(member) = &sender.id {
            // Best effort, inside the agent lock: last-sent.json names the last message sent; a send ends a wait.
            let file = super::server_dir(ctx, caller)
                .join("agents")
                .join(member)
                .join("last-sent.json");
            let _ = crate::state::fs::write_json(
                &file,
                &json!({"line": super::mail::doing_line(body), "at": utc_o(ctx.clock.now())}),
            );
            if let Some(folder) = file.parent() {
                super::message::prune_leases(ctx, folder);
            }
        }
        Ok(())
    };
    // The caller holds the agent lock of a member sender (lock order: team, agent, then each mailbox).
    run()?;
    let mut reply = Map::new();
    reply.insert("to".into(), word.into());
    reply.insert("id".into(), id.clone().into());
    reply.insert("from".into(), sender.name.clone().into());
    reply.insert(
        "recipients".into(),
        recipients
            .iter()
            .map(|r| json!({"name": r["name"], "id": r["id"]}))
            .collect(),
    );
    reply.insert("hop".into(), intent["hop"].clone());
    reply.insert("re".into(), intent["re"].clone());
    if let [one] = recipients.as_slice()
        && one["session"].is_string()
    {
        reply.insert("toSessionId".into(), one["session"].clone());
    }
    Ok(reply)
}

#[cfg(test)]
#[path = "team_mail_tests.rs"]
mod tests;
