//! The team notebook (#103 phase 7a, N1-N3): `<root>/<key>/teams/<sha16(session)>/notes/notebook.jsonl`, append-only
//! lines `{id, author, at, title, body}`, and `notebook.state.json` = `{durable, uncertain}`. Readers see only whole
//! lines ending at or before a certain `durable`. Every function taking a `Tx` runs under the team lock.
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::team_state::{Scope, Tx};
use crate::caller::Caller;
use crate::error::{Refusal, Res};
use crate::state::evidence;
use crate::state::fs::{sync_dir, sync_file, write_json_durable};

pub const LOG: &str = "notebook.jsonl";
pub const STATE: &str = "notebook.state.json";

/// `<root>/<key>/teams/<sha16(session)>/notes` (ambiguity 1).
pub fn dir(root: &Path, scope: &Scope) -> PathBuf {
    let team = &crate::util::sha256_hex(&scope.session)[..16];
    root.join(&scope.key).join("teams").join(team).join("notes")
}

/// The scope the verbs use; reentry-6a (E0) routes it to the team's anchor.
pub fn anchor(caller: &Caller) -> Scope {
    Scope::of(caller)
}

/// N1: an entry's author; `key` is its logical member key (a person's is `person`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    pub name: String,
    pub id: Option<String>,
    pub key: String,
}

/// What an append did: a new entry, or (Task 2) the entry `-Id` names, already in the certain prefix.
#[derive(Debug, Clone, PartialEq)]
pub enum Wrote {
    Added(Value),
    Found(Value),
}

fn refusal(code: u8, check: &'static str, message: &str) -> Refusal {
    Refusal {
        code,
        check,
        message: message.into(),
        reason: None,
        leftover: None,
        details: Default::default(),
    }
}

/// Ambiguity 6: a flush that did not complete; nothing was answered.
fn not_on_disk() -> Refusal {
    refusal(1, "notes", "the notebook is not yet on disk; retry")
}

/// R2, R5: from the state root, `<key>`, `teams`, `<sha16>`, `notes`, each a real folder (`symlink_metadata`, never
/// following a link or junction). `<key>` must exist. A level missing below it is `None` for a reader; a writer makes
/// it. A writer flushes the folder holding each level below `<key>` on every call (R4). Anything else: `evidence`.
fn folders(root: &Path, scope: &Scope, make: bool) -> Res<Option<PathBuf>> {
    let notes = dir(root, scope);
    let names = notes
        .strip_prefix(root)
        .map_err(|_| evidence::unreadable())?;
    let mut at = root.to_path_buf();
    for (n, name) in names.iter().enumerate() {
        let parent = at.clone();
        at.push(name);
        match std::fs::symlink_metadata(&at) {
            Ok(m) if m.is_dir() => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !make => return Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && n > 0 => {
                match crate::state::fs::create_dir(&at) {
                    Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => {
                        return Err(evidence::unreadable());
                    }
                    _ => {}
                }
                if !std::fs::symlink_metadata(&at).is_ok_and(|m| m.is_dir()) {
                    return Err(evidence::unreadable());
                }
            }
            _ => return Err(evidence::unreadable()),
        }
        if make && n > 0 {
            sync_dir(&parent).map_err(|_| not_on_disk())?;
        }
    }
    Ok(Some(notes))
}

/// Whether `path` is a regular file (`false`: nothing there); anything else at that name is `evidence` (R5), so a
/// folder or link at a leaf is never read or renamed over.
fn leaf(path: &Path) -> Res<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        _ => Err(evidence::unreadable()),
    }
}

/// R1: the log's bytes, read once (none yet: empty).
fn read_log(notes: &Path) -> Res<Vec<u8>> {
    let log = notes.join(LOG);
    if !leaf(&log)? {
        return Ok(Vec::new());
    }
    std::fs::read(&log).map_err(|_| evidence::unreadable())
}

/// What `notebook.state.json` says about the log read (ambiguity 4). Task 3 adds `Uncertain`.
enum Marker {
    Absent,
    Certain(u64),
    Bad,
}

fn marker(notes: &Path, log: &[u8]) -> Res<Marker> {
    let path = notes.join(STATE);
    if !leaf(&path)? {
        return Ok(Marker::Absent);
    }
    let Ok(Some(v)) = crate::state::fs::read_json(&path) else {
        return Ok(Marker::Bad);
    };
    let durable = v.get("durable").and_then(Value::as_u64);
    let uncertain = v.get("uncertain").and_then(Value::as_bool);
    let (Some(durable), Some(uncertain)) = (durable, uncertain) else {
        return Ok(Marker::Bad);
    };
    let line_end = durable == 0 || log.get(durable as usize - 1) == Some(&b'\n');
    if durable > log.len() as u64 || !line_end || uncertain {
        return Ok(Marker::Bad);
    }
    Ok(Marker::Certain(durable))
}

/// R3: the certain `durable`; absent is 0. Task 3 completes an uncertain marker, Task 4 rebuilds a bad one.
fn settle(notes: &Path, log: &[u8]) -> Res<u64> {
    match marker(notes, log)? {
        Marker::Absent => Ok(0),
        Marker::Certain(durable) => Ok(durable),
        Marker::Bad => Err(evidence::unreadable()),
    }
}

/// N2 (4): the marker for `durable`. Task 3 publishes it uncertain, then completes it.
fn publish(notes: &Path, durable: u64) -> Res<()> {
    let path = notes.join(STATE);
    write_json_durable(&path, &json!({"durable": durable, "uncertain": false}))
}

/// N2 (1)-(3): cuts the bytes past `durable` (an unanswered tail) and flushes, writes `line` there, then flushes the
/// file and the notes folder. Answers the new end.
fn write_line(notes: &Path, durable: u64, len: u64, line: &[u8]) -> Res<u64> {
    let path = notes.join(LOG);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&path)?;
    if len > durable {
        file.set_len(durable)?;
        sync_file(&path)?;
    }
    file.seek(SeekFrom::Start(durable))?;
    file.write_all(line)?;
    drop(file);
    sync_file(&path)?;
    sync_dir(notes)?;
    Ok(durable + line.len() as u64)
}

/// The entries of the first `durable` bytes, in order; a line that does not parse is skipped (as `parse_mail`).
fn parse(log: &[u8], durable: u64) -> Vec<Value> {
    String::from_utf8_lossy(&log[..durable as usize])
        .split('\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(Value::is_object)
        .collect()
}

/// A 16-hex id no entry of `known` has (ambiguity 2), drawn from `draw` until it is new.
fn fresh_id(known: &[Value], mut draw: impl FnMut() -> Res<String>) -> Res<String> {
    loop {
        let id = draw()?;
        if !known.iter().any(|e| super::text(e.get("id")) == id) {
            return Ok(id);
        }
    }
}

/// N2: one append under the team lock: settle the certain `durable`, write the line past it, publish the new end.
pub fn append(
    tx: &Tx<'_>,
    author: &Author,
    title: &str,
    body: &str,
    id: Option<&str>,
) -> Res<Wrote> {
    let notes = folders(&tx.ctx.state_root, &tx.scope, true)?.ok_or_else(evidence::unreadable)?;
    let log = read_log(&notes)?;
    let durable = settle(&notes, &log)?;
    let known = parse(&log, durable);
    let id = match id {
        Some(id) => id.to_string(),
        None => fresh_id(&known, super::team_mail::new_id)?,
    };
    let entry = json!({"id": id, "author": {"name": author.name, "id": author.id, "key": author.key},
        "at": crate::util::utc_o(tx.ctx.clock.now()), "title": title, "body": body});
    let line = format!("{entry}\n");
    let end = write_line(&notes, durable, log.len() as u64, line.as_bytes())?;
    publish(&notes, end)?;
    Ok(Wrote::Added(entry))
}

/// N2: the whole entries of the certain prefix, oldest first (7b's reader too). No notebook yet: none.
pub fn entries(tx: &Tx<'_>) -> Res<Vec<Value>> {
    let Some(notes) = folders(&tx.ctx.state_root, &tx.scope, false)? else {
        return Ok(Vec::new());
    };
    let log = read_log(&notes)?;
    let durable = settle(&notes, &log)?;
    Ok(parse(&log, durable))
}

#[cfg(test)]
#[path = "notebook_tests.rs"]
mod tests;
