//! The team notebook (#103 phase 7a, N1-N3): `<root>/<key>/teams/<sha16(session)>/notes/notebook.jsonl`, append-only
//! lines `{id, author, at, title, body}`, and `notebook.state.json` = `{durable, uncertain}`. Readers see only whole
//! lines ending at or before a certain `durable`. Every function taking a `Tx` runs under the team lock.
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::team_state::{Scope, Tx};
use super::tree::Standing;
use crate::caller::Caller;
use crate::cli::Request;
use crate::ctx::Ctx;
use crate::error::{Refusal, Res};
use crate::state::evidence;
use crate::state::fs::{read_text, sync_dir, sync_file, write_json_durable};

pub const LOG: &str = "notebook.jsonl";
pub const STATE: &str = "notebook.state.json";
pub const TITLE_MAX: usize = 80;
pub const BODY_LIMIT: usize = 4096;
/// PM call 7: the `-BodyFile` read bound, never the refusal a body within `BODY_LIMIT` sees (CRLF doubles it, a
/// BOM adds 3).
pub const RAW_MAX: u64 = 4 * BODY_LIMIT as u64 + 3;

/// `<root>/<key>/teams/<sha16(session)>/notes` (ambiguity 1).
pub fn dir(root: &Path, scope: &Scope) -> PathBuf {
    let team = &crate::util::sha256_hex(&scope.session)[..16];
    root.join(&scope.key).join("teams").join(team).join("notes")
}

/// The scope the verbs use; reentry-6a (E0) routes it to the team's anchor.
pub fn anchor(caller: &Caller) -> Scope {
    super::scope::state(caller)
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

/// Ambiguity 6: the append drew `id`, then failed; a retry with `-Id <id>` saves it once.
fn unsaved(id: &str) -> Refusal {
    refusal(
        1,
        "notes",
        &format!("the note could not be saved; retry with -Id {id}"),
    )
    .with("id", id.to_string())
}

/// Ambiguity 5: the entry `-Id` names is this author's same note.
fn same(entry: &Value, author: &Author, title: &str, body: &str) -> bool {
    super::text(entry.get("author").and_then(|a| a.get("key"))) == author.key
        && super::text(entry.get("title")) == title
        && super::text(entry.get("body")) == body
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
    crate::state::fs::read_bytes(&log).map_err(|_| evidence::unreadable())
}

/// What `notebook.state.json` says about the log read (ambiguity 4). `Uncertain`: renamed, its folder flush not yet known.
enum Marker {
    Absent,
    Certain(u64),
    Uncertain(u64),
    Bad,
}

fn marker(notes: &Path, log: &[u8]) -> Res<Marker> {
    let path = notes.join(STATE);
    if !leaf(&path)? {
        // Call 3: no marker beside a log with content is a deleted marker: unreadable, rebuilt, never durable 0.
        return Ok(if log.is_empty() {
            Marker::Absent
        } else {
            Marker::Bad
        });
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
    if durable > log.len() as u64 || !line_end {
        return Ok(Marker::Bad);
    }
    Ok(if uncertain {
        Marker::Uncertain(durable)
    } else {
        Marker::Certain(durable)
    })
}

/// R3: the certain `durable`; absent is 0. an uncertain marker is completed first; a bad one is rebuilt.
fn settle(notes: &Path, log: &[u8]) -> Res<u64> {
    match marker(notes, log)? {
        Marker::Absent => Ok(0),
        Marker::Certain(durable) => Ok(durable),
        Marker::Uncertain(durable) => complete(notes, durable),
        Marker::Bad => rebuild(notes, log),
    }
}

/// N2 (4): the marker for `durable`. Published uncertain, then completed.
fn publish(notes: &Path, durable: u64) -> Res<()> {
    let path = notes.join(STATE);
    write_json_durable(&path, &json!({"durable": durable, "uncertain": true}))?;
    complete(notes, durable).map(|_| ())
}

/// v5 certainty first: flush the notes folder (the uncertain rename is then on disk), then rewrite the same
/// `durable` certain, on every call (R4). Any failure: 1 `notes`, and the caller answers nothing.
fn complete(notes: &Path, durable: u64) -> Res<u64> {
    sync_dir(notes).map_err(|_| not_on_disk())?;
    let certain = json!({"durable": durable, "uncertain": false});
    write_json_durable(&notes.join(STATE), &certain).map_err(|_| not_on_disk())?;
    Ok(durable)
}

/// N2: an unreadable marker, rebuilt under the team lock: flush the log, then the folder, then publish the end of
/// the last whole line (uncertain, then completed). A flush that fails refuses 1 `notes` and publishes nothing.
fn rebuild(notes: &Path, log: &[u8]) -> Res<u64> {
    let whole = log
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |i| i as u64 + 1);
    if leaf(&notes.join(LOG))? {
        sync_file(&notes.join(LOG)).map_err(|_| not_on_disk())?;
    }
    sync_dir(notes).map_err(|_| not_on_disk())?;
    publish(notes, whole).map_err(|_| not_on_disk())?;
    Ok(whole)
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
        Some(id) => match known.iter().find(|e| super::text(e.get("id")) == id) {
            Some(e) if same(e, author, title, body) => return Ok(Wrote::Found(e.clone())),
            Some(_) => return Err(refusal(2, "id", "that id belongs to a different note")),
            None => id.to_string(),
        },
        None => fresh_id(&known, super::team_mail::new_id)?,
    };
    let entry = json!({"id": id, "author": {"name": author.name, "id": author.id, "key": author.key},
        "at": crate::util::utc_o(tx.ctx.clock.now()), "title": title, "body": body});
    let line = format!("{entry}\n");
    let end =
        write_line(&notes, durable, log.len() as u64, line.as_bytes()).map_err(|_| unsaved(&id))?;
    publish(&notes, end).map_err(|_| unsaved(&id))?;
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

/// N1, ambiguity 2: a person is `person`; a member its record name, its id and its logical mailbox key.
pub fn author_of(standing: &Standing) -> Author {
    match standing {
        Standing::Person => Author {
            name: "person".into(),
            id: None,
            key: "person".into(),
        },
        Standing::Member(m) => Author {
            name: super::text(m.record.get("name")),
            id: Some(m.id.clone()),
            key: super::mail::member_mailbox(&m.id, &m.record),
        },
    }
}

/// N3, ambiguity 7: under the team lock (`with_standing`, phase 3 D1), the proven caller as author and the anchor
/// scope's `Tx` (replay hooks first). Outside a team: 2 `team`.
fn with_author<T>(ctx: &Ctx, f: impl FnOnce(&Tx<'_>, &Author, &Caller) -> Res<T>) -> Res<T> {
    let caller = crate::caller::resolve_tree(ctx)?;
    super::tree::with_standing(ctx, &caller, |standing| {
        let Some(standing) = standing else {
            return Err(refusal(2, "team", "the notebook belongs to a team"));
        };
        let author = author_of(standing);
        super::team_state::held(ctx, &anchor(&caller), |tx| f(tx, &author, &caller))
    })
}

/// `-Id`/`-Since`/`notes <id>`: 8-32 lowercase hex digits.
fn hex_id(v: &str) -> bool {
    (8..=32).contains(&v.len()) && v.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `note -Title <t> (-Body <text> | -BodyFile <p>) [-Id <id>]` (N3; ambiguity 3: every limit is 2 `usage`).
pub fn note(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let usage = || super::usage("note");
    let title = super::mail::message_text(request.value("Title").unwrap_or_default());
    let (body, file, id) = (
        request.value("Body"),
        request.value("BodyFile"),
        request.value("Id"),
    );
    if title.is_empty()
        || title.contains('\n')
        || title.chars().count() > TITLE_MAX
        || body.is_some() == file.is_some()
        || id.is_some_and(|i| !hex_id(i))
    {
        return Err(usage());
    }
    let body = match (body, file) {
        (Some(b), _) => b.to_string(),
        // PM call 7: `RAW_MAX` bounds only the raw read (BOM and CRLF included); the limit applies below.
        (None, f) => read_text(&ctx.cwd.join(f.unwrap_or_default()), RAW_MAX)?,
    };
    let body = super::mail::message_text(&body);
    if body.len() > BODY_LIMIT {
        return Err(usage());
    }
    // 7b V2, V3: the notices follow the publish under the same team lock; a duplicate posts too (each is idempotent
    // by `note-<id>`), so a retry finishes notices an earlier call missed. A failed notice never undoes the note.
    let (entry, duplicate, notified, errors) = with_author(ctx, |tx, author, caller| {
        let (entry, duplicate) = match append(tx, author, &title, &body, id)? {
            Wrote::Added(e) => (e, false),
            Wrote::Found(e) => (e, true),
        };
        let (notified, errors) = news::notify(ctx, caller, author, &entry);
        Ok((entry, duplicate, notified, errors))
    })?;
    let mut reply = Map::new();
    reply.insert("id".into(), entry["id"].clone());
    reply.insert("duplicate".into(), duplicate.into());
    reply.insert("notified".into(), notified.into());
    reply.insert("notifyErrors".into(), errors.into());
    Ok(reply)
}

pub const LIST_DEFAULT: usize = 20;
pub const LIST_MAX: usize = 1000;

/// Ambiguity 8: a listing row; the body only through `notes <id>`.
fn row(e: &Value) -> Value {
    let author = e.get("author").and_then(|a| a.get("name"));
    json!({"id": e.get("id"), "author": author, "at": e.get("at"), "title": e.get("title")})
}

/// `notes [-Since <id>] [-Lines <n>]` newest first, or `notes <id>` in full (N3). Reads the certain prefix under
/// the team lock (completing an uncertain marker first); moves no cursor.
pub fn notes(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let usage = || super::usage("notes");
    let one = request.positional.first().map(String::as_str);
    let since = request.value("Since");
    let lines = match request.value("Lines") {
        None => LIST_DEFAULT,
        Some(v) => v
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=LIST_MAX).contains(n))
            .ok_or_else(usage)?,
    };
    if (one.is_some() && (since.is_some() || request.value("Lines").is_some()))
        || one.into_iter().chain(since).any(|i| !hex_id(i))
    {
        return Err(usage());
    }
    let all = with_author(ctx, |tx, _, _| entries(tx))?;
    let at = |id: &str| all.iter().position(|e| super::text(e.get("id")) == id);
    let unknown = || refusal(2, "target", "no note with that id");
    let mut reply = Map::new();
    if let Some(id) = one {
        reply.insert("note".into(), all[at(id).ok_or_else(unknown)?].clone());
        return Ok(reply);
    }
    let after = match since {
        Some(id) => at(id).ok_or_else(unknown)? + 1,
        None => 0,
    };
    let eligible = &all[after..];
    let shown: Vec<Value> = eligible.iter().rev().take(lines).map(row).collect();
    reply.insert("older".into(), (eligible.len() - shown.len()).into());
    reply.insert("notes".into(), shown.into());
    Ok(reply)
}

// notebook-7b V1: the launch index.
pub mod news;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod fault_tests;

#[cfg(test)]
mod rebuild_tests;

#[cfg(test)]
mod verb_tests;

#[cfg(test)]
mod notes_tests;
