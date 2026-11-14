//! Team messaging (#103 phase 4a): who a target names, one durable intent per send (also its quota charge), and
//! durable delivery of one message id to each mailbox. Intents: `teams/<sha16(session)>/sends/<sender>/<id>.json`,
//! read and written under the team lock.
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

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

/// Makes `dir` and flushes, on every call, each folder from `dir` up to `base` into its parent, so a retry finishes
/// an entry flush an earlier call left undone, for folders that already existed too (F3).
fn durable_dir(dir: &Path, base: &Path) -> Res<()> {
    std::fs::create_dir_all(dir)?;
    let mut at = dir;
    while at != base {
        let Some(parent) = at.parent() else { break };
        crate::state::fs::sync_dir(parent)?;
        at = parent;
    }
    Ok(())
}

/// The server folder above `teams/<sha16>/sends`.
fn base_of(root: &Path) -> &Path {
    root.ancestors().nth(3).unwrap_or(root)
}

/// Writes intent `value` (its `id`) for `sender` durably: its folders flushed up to the server folder, then the file
/// flushed, renamed, and its folder flushed.
pub fn write_intent(root: &Path, sender: &str, value: &Value) -> Res<()> {
    let dir = root.join(sender);
    durable_dir(&dir, base_of(root))?;
    crate::state::fs::write_json_durable(
        &dir.join(format!("{}.json", super::text(value.get("id")))),
        value,
    )
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

#[cfg(test)]
#[path = "team_mail_tests.rs"]
mod tests;
