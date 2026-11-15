//! Mailboxes: `mail/<key>.jsonl` with a byte offset in `<key>.offset`, one JSON message per LF-ended line
//! (MuxAgents.ps1:1020-1148, 1206-1214).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde_json::{Map, Value, json};

use super::receipt::{self, Owner, Receipt};
use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::error::{Refusal, Res, refuse};

/// An agent's mailbox is its id; any other pane's is `pane-<n>`.
pub fn mailbox_key(pane: &str, tag: &str) -> Res<String> {
    if super::agent_id(tag) {
        return Ok(tag.to_string());
    }
    if !super::pane_id(pane) {
        return refuse(2, "target", "not a pane");
    }
    Ok(format!("pane-{}", &pane[1..]))
}

/// A member's mailbox: the one its record's `mailbox` names (a resumed member keeps the mailbox it inherited),
/// otherwise its id.
pub fn member_mailbox(id: &str, record: &Value) -> String {
    match record.get("mailbox").and_then(Value::as_str) {
        Some(key) if super::agent_id(key) => key.to_string(),
        _ => id.to_string(),
    }
}

/// LF line ends; ESC and the other C0 controls except LF and TAB, and DEL, removed.
pub fn message_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .filter(|&c| c == '\n' || c == '\t' || !(c < ' ' || c == '\u{7f}'))
        .collect()
}

/// One side-bar line: C1 removed too, then the first non-blank line, tabs as spaces, trimmed, at most 80 UTF-16
/// units without splitting a surrogate pair; empty when every line is blank.
pub fn doing_line(text: &str) -> String {
    let clean: String = message_text(text)
        .chars()
        .filter(|c| !('\u{80}'..='\u{9f}').contains(c))
        .collect();
    for line in clean.split('\n') {
        let line = line.replace('\t', " ");
        let line = line.trim();
        if !line.is_empty() {
            return first_units(line, 80);
        }
    }
    String::new()
}

// The longest prefix of at most `max` UTF-16 units that ends on a whole char, as .NET's Substring(0, 80 or 79).
fn first_units(text: &str, max: usize) -> String {
    let mut units = 0;
    let mut out = String::new();
    for c in text.chars() {
        units += c.len_utf16();
        if units > max {
            break;
        }
        out.push(c);
    }
    out
}

/// Appends one message to `key`'s mailbox under its lock and returns its id. A last line without LF, left by an
/// interrupted append, gets its LF first, so it never joins the new message. With `message_id`, a mailbox with a
/// parsable line holding that id gets no second copy, only the LF it may lack.
pub fn add_mail(
    ctx: &Ctx,
    caller: &Caller,
    key: &str,
    from: &str,
    from_pane: &str,
    body: &str,
    message_id: Option<&str>,
) -> Res<String> {
    let given = message_id.filter(|id| !id.is_empty());
    let id = match given {
        Some(id) => id.to_string(),
        None => crate::util::guid()?.replace('-', "")[..8].to_string(),
    };
    let message = json!({"id": id, "from": from, "pane": from_pane, "at": crate::util::utc_o(ctx.clock.now()), "body": body});
    append_message(ctx, caller, key, &message, given, false)?;
    Ok(id)
}

/// `add_mail` from a member: the line also names the member it came from (`fromId`), so a team bar counts it by
/// identity, whichever pane the member's view is on now (#103 review). `read` shows the same fields as before.
pub fn add_member_mail(
    ctx: &Ctx,
    caller: &Caller,
    key: &str,
    from: &str,
    from_id: &str,
    from_pane: &str,
    body: &str,
) -> Res<String> {
    let id = crate::util::guid()?.replace('-', "")[..8].to_string();
    let message = json!({"id": id, "from": from, "fromId": from_id, "pane": from_pane,
        "at": crate::util::utc_o(ctx.clock.now()), "body": body});
    append_message(ctx, caller, key, &message, None, false)?;
    Ok(id)
}

/// Appends message `id` with `fields` after its body, unless the mailbox already holds a parsable line with that
/// id: then nothing is added (only the LF it may lack) and that stored message is the answer.
#[allow(clippy::too_many_arguments)]
pub fn add_once(
    ctx: &Ctx,
    caller: &Caller,
    key: &str,
    from: &str,
    from_pane: &str,
    body: &str,
    id: &str,
    fields: &Map<String, Value>,
) -> Res<Option<Value>> {
    let mut message = json!({"id": id, "from": from, "pane": from_pane, "at": crate::util::utc_o(ctx.clock.now()), "body": body});
    if let Some(object) = message.as_object_mut() {
        object.extend(fields.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    append_message(ctx, caller, key, &message, Some(id), false)
}

/// The append of `add_mail` and `add_once`, under the mailbox lock: seals a torn last line, then adds `message`
/// unless `dedup` names an id already delivered, whose stored message is answered.
fn append_message(
    ctx: &Ctx,
    caller: &Caller,
    key: &str,
    message: &Value,
    dedup: Option<&str>,
    durable: bool,
) -> Res<Option<Value>> {
    let server = super::state_dir(ctx, caller)?;
    let mailbox = server.join("mail").join(format!("{key}.jsonl"));
    super::lock(ctx, &super::lock_key("mail", caller, key), || {
        // Only a dedup needs the whole mailbox; otherwise its last byte tells whether it is torn.
        let existing = match dedup {
            Some(_) => read_or_empty(&mailbox)?,
            None => last_byte(&mailbox)?.into_iter().collect(),
        };
        let torn = existing.last().is_some_and(|&b| b != b'\n');
        let delivered = dedup.and_then(|id| {
            String::from_utf8_lossy(&existing)
                .split('\n')
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .find(|stored| super::text(stored.get("id")) == id)
        });
        let mut out = String::new();
        if torn {
            out.push('\n');
        }
        if delivered.is_none() {
            out.push_str(&message.to_string());
            out.push('\n');
        }
        if !out.is_empty() {
            append(&mailbox, out.as_bytes())?;
        }
        // On every call, a duplicate hit included, so a retry finishes the flush of an earlier call (#103 M5).
        if durable {
            crate::state::fs::sync_file(&mailbox)?;
            if let Some(folder) = mailbox.parent() {
                crate::state::fs::sync_dir(folder)?;
                // A `mail` folder `state_dir` just made is an entry of the server folder (QA fix 4).
                crate::state::fs::durable_dir(folder, &server)?;
            }
        }
        Ok(delivered)
    })
}

/// `message` (its own `id`) in the mailbox of `key` once, its file and folder flushed before this returns, on every
/// call: a failed flush is an error and a retry repeats it (#103 M5).
pub fn deliver_once(ctx: &Ctx, caller: &Caller, key: &str, message: &Value) -> Res<()> {
    let id = super::text(message.get("id"));
    append_message(ctx, caller, key, message, Some(&id), true).map(|_| ())
}

/// The stored message `id` of the mailbox of `key`, read under its lock; none when absent.
pub fn find_message(ctx: &Ctx, caller: &Caller, key: &str, id: &str) -> Res<Option<Value>> {
    let mailbox = super::server_dir(ctx, caller)
        .join("mail")
        .join(format!("{key}.jsonl"));
    super::lock(ctx, &super::lock_key("mail", caller, key), || {
        Ok(
            parse_mail(&String::from_utf8_lossy(&read_or_empty(&mailbox)?))
                .into_iter()
                .find(|m| super::text(m.get("id")) == id),
        )
    })
}

/// The first line of a message: sender, pane, id, then ` re #<id>`, ` hop <n>` (n > 0) and ` to <cast>` when set.
pub fn header(message: &Value) -> String {
    let mut line = format!(
        "[mux-agent from {} ({}) #{}",
        super::text(message.get("from")),
        super::text(message.get("pane")),
        super::text(message.get("id"))
    );
    if let Some(re) = message.get("re").and_then(Value::as_str) {
        line.push_str(&format!(" re #{re}"));
    }
    if let Some(hop) = message
        .get("hop")
        .and_then(Value::as_u64)
        .filter(|&h| h > 0)
    {
        line.push_str(&format!(" hop {hop}"));
    }
    if let Some(cast) = message.get("cast").and_then(Value::as_str) {
        line.push_str(&format!(" to {cast}"));
    }
    line.push(']');
    line
}

/// R6: rendered UTF-8 bytes per serving, Copilot included.
pub const BUDGET: usize = 4096;

/// One rendering: its text, the ids it covers in order, those served as references.
#[derive(Debug, Default, PartialEq)]
pub struct Batch {
    pub text: String,
    pub ids: Vec<String>,
    pub refs: Vec<String>,
}

fn entry(m: &Value) -> String {
    format!("{}\n{}\n", header(m), super::text(m.get("body")))
}

fn reference(m: &Value) -> String {
    let id = super::text(m.get("id"));
    format!(
        "message {id} from {}, {} bytes: run mux-agent read -Id {id}\n",
        super::text(m.get("from")),
        entry(m).len()
    )
}

fn held_line(held: &[String]) -> String {
    format!("{} held: run mux-agent read -Id {}\n", held.len(), held[0])
}

fn more_line(n: usize) -> String {
    format!("+{n} more: run mux-agent read\n")
}

/// R6 over `found` (oldest first): whole entries while they fit; one larger than the budget alone is a reference;
/// the held trailer is reserved first; what does not fit waits behind `+N more`.
pub fn batch(found: &[Value], held: &[String]) -> Batch {
    let reserve = if held.is_empty() {
        0
    } else {
        held_line(held).len()
    };
    let mut b = Batch::default();
    for (n, m) in found.iter().enumerate() {
        let after = found.len() - n - 1;
        let tail = if after > 0 { more_line(after).len() } else { 0 };
        let whole = entry(m);
        // Alone, with the trailers it needs, does the whole entry fit? Else it is a reference.
        let piece = if whole.len() + reserve + tail <= BUDGET {
            whole
        } else {
            reference(m)
        };
        if b.text.len() + piece.len() + reserve + tail > BUDGET {
            b.text.push_str(&more_line(found.len() - n));
            break;
        }
        if !piece.starts_with('[') {
            b.refs.push(super::text(m.get("id")));
        }
        b.ids.push(super::text(m.get("id")));
        b.text.push_str(&piece);
    }
    if !held.is_empty() {
        b.text.push_str(&held_line(held));
    }
    b
}

fn read_or_empty(path: &Path) -> Res<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

fn last_byte(path: &Path) -> Res<Option<u8>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if file.metadata()?.len() == 0 {
        return Ok(None);
    }
    file.seek(SeekFrom::End(-1))?;
    let mut byte = [0u8];
    file.read_exact(&mut byte)?;
    Ok(Some(byte[0]))
}

fn append(path: &Path, bytes: &[u8]) -> Res<()> {
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(bytes)?;
    Ok(())
}

/// The complete lines after the read offset, and the offset after them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unread {
    pub text: String,
    pub end: u64,
}

// An offset is a few digits; anything longer is not one.
const CURSOR_MAX: u64 = 64;

/// The mailbox's complete lines after its read offset, and the offset after them (the old one when none). Reads
/// only: the caller holds the mailbox lock. `tolerant` (list) counts from the start when the offset is not a
/// number, lies outside the mailbox or falls inside a line, so a damaged offset never fails list or hides mail.
pub fn unread_text(mailbox: &Path, cursor: &Path, tolerant: bool) -> Res<Unread> {
    let raw = match File::open(cursor) {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take(CURSOR_MAX + 1).read_to_end(&mut bytes)?;
            Some(String::from_utf8_lossy(&bytes).into_owned())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let raw = raw
        .as_deref()
        .map(|r| r.trim_start_matches('\u{feff}').trim());
    let mut offset = match raw {
        None => 0,
        Some(raw) if tolerant => digits(raw).unwrap_or(0),
        Some("") => 0,
        Some(raw) => digits(raw.strip_prefix('+').unwrap_or(raw))
            .ok_or_else(|| Refusal::internal("mail offset is not a number"))?,
    };
    let mut file = match File::open(mailbox) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(Unread {
                text: String::new(),
                end: offset,
            });
        }
        Err(e) => return Err(e.into()),
    };
    let len = file.metadata()?.len();
    if tolerant && (offset > len || (offset > 0 && byte_at(&mut file, offset - 1)? != b'\n')) {
        offset = 0;
    }
    if offset >= len {
        return Ok(Unread {
            text: String::new(),
            end: offset,
        });
    }
    // Only the bytes past the offset are read: the mailbox only grows.
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let complete = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    Ok(Unread {
        text: String::from_utf8_lossy(&bytes[..complete]).into_owned(),
        end: offset + complete as u64,
    })
}

fn digits(text: &str) -> Option<u64> {
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse::<u64>().ok())
        .flatten()
        .filter(|&n| n <= i64::MAX as u64)
}

fn byte_at(file: &mut File, at: u64) -> Res<u8> {
    file.seek(SeekFrom::Start(at))?;
    let mut byte = [0u8];
    file.read_exact(&mut byte)?;
    Ok(byte[0])
}

/// The parsed messages of mailbox text; a line that does not parse (a sealed torn one) is skipped, so one bad
/// line never blocks the mailbox.
pub fn parse_mail(text: &str) -> Vec<Value> {
    text.split('\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Every unread message of `key`'s mailbox in arrival order, as `{id, from, pane, at, text}`; the offset moves
/// past them under the mailbox lock.
pub fn read_mail(ctx: &Ctx, caller: &Caller, key: &str) -> Res<Vec<Map<String, Value>>> {
    read_mail_if(ctx, caller, key, || true)
}

/// `read_mail` behind a fence: holding the mailbox lock, `current` decides whether the reader may still read this
/// mailbox; when it says no, nothing is read and the offset stays (one owner reads a mailbox).
pub fn read_mail_if(
    ctx: &Ctx,
    caller: &Caller,
    key: &str,
    current: impl FnOnce() -> bool,
) -> Res<Vec<Map<String, Value>>> {
    let mail = super::state_dir(ctx, caller)?.join("mail");
    let mailbox = mail.join(format!("{key}.jsonl"));
    let cursor = mail.join(format!("{key}.offset"));
    let found = super::lock(ctx, &super::lock_key("mail", caller, key), || {
        if !current() {
            return Ok(Vec::new());
        }
        let unread = unread_text(&mailbox, &cursor, false)?;
        if unread.text.is_empty() {
            return Ok(Vec::new());
        }
        std::fs::write(&cursor, unread.end.to_string())?;
        Ok(parse_mail(&unread.text))
    })?;
    Ok(found
        .iter()
        .map(|message| {
            let field = |name: &str| message.get(name).cloned().unwrap_or(Value::Null);
            let text = format!("{}\n{}", header(message), super::text(message.get("body")));
            let mut out = Map::new();
            for name in ["id", "from", "pane", "at"] {
                out.insert(name.into(), field(name));
            }
            // A team message (#103 4a) stores all three; a 0.2 message stores none and reads as before.
            for name in ["re", "hop", "cast"] {
                if message.get(name).is_some() {
                    out.insert(name.into(), field(name));
                }
            }
            out.insert("text".into(), text.into());
            out
        })
        .collect())
}

/// Unread messages in `key`'s mailbox per `<from>|<pane>`, read under the mailbox lock; never moves the offset
/// or creates a folder.
pub fn unread_counts(ctx: &Ctx, caller: &Caller, key: &str) -> Res<HashMap<String, usize>> {
    let mail = super::server_dir(ctx, caller).join("mail");
    let unread = super::lock(ctx, &super::lock_key("mail", caller, key), || {
        unread_text(
            &mail.join(format!("{key}.jsonl")),
            &mail.join(format!("{key}.offset")),
            true,
        )
    })?;
    let mut counts = HashMap::new();
    for message in parse_mail(&unread.text) {
        let from = format!(
            "{}|{}",
            super::text(message.get("from")),
            super::text(message.get("pane"))
        );
        // A line naming its member (`fromId`) also counts as `#<id>`, and as `!<from>|<pane>` so a team row adds the
        // lines without one by name and pane and never counts a line twice (#103 review).
        if let Some(id) = message
            .get("fromId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            *counts.entry(format!("#{id}")).or_insert(0) += 1;
            *counts.entry(format!("!{from}")).or_insert(0) += 1;
        }
        *counts.entry(from).or_insert(0) += 1;
    }
    Ok(counts)
}

/// The complete lines after `offset`, each message with the offset past its line, and the snapshot end. Reads only
/// the bytes past `offset` (the mailbox only grows); the caller holds the mailbox lock.
fn unread_after(mailbox: &Path, offset: u64) -> Res<(Vec<(Value, u64)>, u64)> {
    let mut file = match File::open(mailbox) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), offset)),
        Err(e) => return Err(e.into()),
    };
    if file.metadata()?.len() <= offset {
        return Ok((Vec::new(), offset));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let (mut found, mut at) = (Vec::new(), offset);
    for line in bytes
        .split_inclusive(|&b| b == b'\n')
        .filter(|l| l.ends_with(b"\n"))
    {
        at += line.len() as u64;
        if let Ok(v) = serde_json::from_slice::<Value>(&line[..line.len() - 1]) {
            found.push((v, at));
        }
    }
    Ok((found, at))
}

fn mail_dir(ctx: &Ctx, caller: &Caller) -> Res<std::path::PathBuf> {
    Ok(super::state_dir(ctx, caller)?.join("mail"))
}

#[derive(Debug, PartialEq)]
pub enum Acked {
    Done,
    Duplicate,
}

/// R3: under the mailbox lock and the fence, the pending receipt of `owner` unchanged, else (discarding the one of
/// another owner) a new batch over one snapshot after `offset`, committed as `pending`. None: fenced out, or nothing
/// new (a held-only trailer is served as text with no receipt by the verb). Consumes nothing.
pub fn serve(
    ctx: &Ctx,
    caller: &Caller,
    key: &str,
    owner: &Owner,
    current: &dyn Fn() -> bool,
) -> Res<Option<Receipt>> {
    let mail = mail_dir(ctx, caller)?;
    super::lock(ctx, &super::lock_key("mail", caller, key), || {
        if !current() {
            return Ok(None);
        }
        let mut c = receipt::load(&mail, key)?;
        receipt::gate(&mail, &c)?;
        if let Some(r) = c.pending.as_ref().filter(|r| r.owner == *owner) {
            return Ok(Some(r.clone()));
        }
        let (found, _) = unread_after(&mail.join(format!("{key}.jsonl")), c.offset)?;
        let messages: Vec<Value> = found.iter().map(|(m, _)| m.clone()).collect();
        let b = batch(&messages, &c.held);
        if b.ids.is_empty() {
            if c.pending.take().is_some() {
                receipt::commit(&mail, key, &c)?;
            }
            return Ok(None);
        }
        let end = found[b.ids.len() - 1].1;
        let r = Receipt {
            batch: super::team_mail::new_id()?,
            owner: owner.clone(),
            ids: b.ids,
            refs: b.refs,
            end,
            text: b.text,
        };
        c.pending = Some(r.clone());
        c.uncertain = None;
        receipt::commit(&mail, key, &c)
            .or_else(|_| refuse(1, "mail", "the receipt could not be saved; retry"))?;
        Ok(Some(r))
    })
}

/// R4: `pending` or `idPending` of `owner` named `batch` commits (offset = max, held updated, history, `uncertain`);
/// one in the history of `owner` is a duplicate; anything else is 2 `ack` `stale receipt`. R5 gate first.
pub fn ack(
    ctx: &Ctx,
    caller: &Caller,
    key: &str,
    owner: &Owner,
    batch_id: &str,
    current: &dyn Fn() -> bool,
) -> Res<Acked> {
    let mail = mail_dir(ctx, caller)?;
    super::lock(ctx, &super::lock_key("mail", caller, key), || {
        if !current() {
            return refuse(2, "ack", "stale receipt");
        }
        let mut c = receipt::load(&mail, key)?;
        receipt::gate(&mail, &c)?;
        let mine = |r: &Option<Receipt>| {
            r.as_ref()
                .is_some_and(|r| r.batch == batch_id && r.owner == *owner)
        };
        if mine(&c.pending) {
            let r = c.pending.take().expect("matched");
            c.offset = c.offset.max(r.end);
            let new: Vec<String> = r
                .refs
                .iter()
                .filter(|id| !c.held.contains(id))
                .cloned()
                .collect();
            c.held.extend(new);
        } else if mine(&c.id_pending) {
            let r = c.id_pending.take().expect("matched");
            c.held.retain(|id| !r.ids.contains(id));
        } else if c.was_acked(batch_id, owner) {
            return Ok(Acked::Duplicate);
        } else {
            return refuse(2, "ack", "stale receipt");
        }
        c.remember(batch_id, owner);
        c.uncertain = Some((batch_id.into(), owner.clone()));
        receipt::commit(&mail, key, &c)
            .or_else(|_| refuse(1, "mail", "the acknowledgement could not be saved; retry"))?;
        Ok(Acked::Done)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::fixture::World;
    use crate::agents::text;
    use crate::testing::TempDir;

    fn refusal<T: std::fmt::Debug>(result: Res<T>) -> String {
        let r = result.unwrap_err();
        format!("{} {} {}", r.code, r.check, r.message)
    }

    // A stored line as the fake clock writes it.
    fn line(id: &str, from: &str, pane: &str, body: &str) -> String {
        format!(
            r#"{{"id":"{id}","from":"{from}","pane":"{pane}","at":"1970-01-01T00:00:00.0000000Z","body":"{body}"}}"#
        )
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    /// A message whose rendered entry is exactly `size` UTF-8 bytes, its body ending in `tail`.
    fn sized(id: &str, size: usize, tail: &str) -> Value {
        let head = header(&json!({"id": id, "from": "a", "pane": "%5"})).len() + 2; // header LF ... body LF
        json!({"id": id, "from": "a", "pane": "%5",
            "body": format!("{}{tail}", "x".repeat(size - head - tail.len()))})
    }

    #[test]
    fn the_budget_counts_utf8_bytes_and_trailers() {
        for tail in ["\u{e9}", "\u{20ac}", "\u{1f600}"] {
            for (size, whole) in [(BUDGET - 1, true), (BUDGET, true), (BUDGET + 1, false)] {
                let b = batch(&[sized("1111111111111111", size, tail)], &[]);
                assert_eq!(b.refs.is_empty(), whole, "{size} {tail}");
                assert!(b.text.len() <= BUDGET, "{size} {tail}");
            }
        }
        // Oversized: a reference; a later fitting message still follows.
        let b = batch(
            &[
                sized("1111111111111111", 5000, "x"),
                sized("2222222222222222", 100, "x"),
            ],
            &[],
        );
        assert_eq!(
            (b.ids.len(), b.refs.clone()),
            (2, vec!["1111111111111111".to_string()])
        );
        assert!(
            b.text.starts_with("message 1111111111111111 from a, "),
            "{}",
            b.text
        );
        assert!(
            b.text
                .contains("bytes: run mux-agent read -Id 1111111111111111\n")
        );
        // Overflow: what does not fit waits behind one trailer.
        let three: Vec<Value> = ["1111111111111111", "2222222222222222", "3333333333333333"]
            .iter()
            .map(|i| sized(i, 1500, "x"))
            .collect();
        let b = batch(&three, &[]);
        assert_eq!(
            (
                b.ids.len(),
                b.text.ends_with("+1 more: run mux-agent read\n")
            ),
            (2, true)
        );
        // Ten held ids: one trailer names the oldest; new mail keeps the budget less that trailer.
        let held: Vec<String> = (0..10).map(|n| format!("{n:016x}")).collect();
        let trailer = "10 held: run mux-agent read -Id 0000000000000000\n";
        let fits = sized("4444444444444444", BUDGET - trailer.len(), "\u{20ac}");
        let b = batch(std::slice::from_ref(&fits), &held);
        assert!(
            b.refs.is_empty() && b.text.ends_with(trailer) && b.text.len() == BUDGET,
            "{}",
            b.text.len()
        );
        let over = sized("4444444444444444", BUDGET - trailer.len() + 1, "\u{20ac}");
        assert_eq!(batch(&[over], &held).refs, ["4444444444444444"]);
        assert_eq!(
            batch(&[], &held).text,
            trailer,
            "held only: the trailer, no ids"
        );
    }

    use crate::agents::receipt::{self, Owner, Receipt};

    fn me(id: &str) -> Owner {
        Owner {
            id: id.into(),
            generation: "0".into(),
        }
    }

    fn put(w: &World, id: &str, body: &str) {
        deliver_once(
            &w.ctx,
            &w.caller(),
            "pane-1",
            &json!({"id": id, "from": "a", "pane": "%5", "at": "x", "body": body}),
        )
        .unwrap();
    }

    fn sv(w: &World, o: &str) -> Res<Option<Receipt>> {
        serve(&w.ctx, &w.caller(), "pane-1", &me(o), &|| true)
    }

    fn ak(w: &World, o: &str, b: &str) -> Res<Acked> {
        ack(&w.ctx, &w.caller(), "pane-1", &me(o), b, &|| true)
    }

    const H1: &str = "1111111111111111";
    const H2: &str = "2222222222222222";

    #[test]
    fn a_receipt_is_re_served_until_acked() {
        let w = World::human("");
        put(&w, H1, "one");
        let r = sv(&w, "a").unwrap().unwrap();
        assert_eq!(r.ids, [H1]);
        put(&w, H2, "two");
        assert_eq!(
            sv(&w, "a").unwrap().unwrap(),
            r,
            "death before acceptance: the same receipt, unchanged"
        );
        assert!(matches!(ak(&w, "a", &r.batch).unwrap(), Acked::Done));
        let r2 = sv(&w, "a").unwrap().unwrap();
        assert_eq!(r2.ids, [H2]);
        assert_ne!(r2.batch, r.batch);
        assert!(
            serve(&w.ctx, &w.caller(), "pane-1", &me("a"), &|| false)
                .unwrap()
                .is_none(),
            "a fenced reader serves nothing"
        );
    }

    #[test]
    fn acks_are_done_duplicate_or_stale() {
        let w = World::human("");
        put(&w, H1, "one");
        let r = sv(&w, "a").unwrap().unwrap();
        assert!(matches!(ak(&w, "a", &r.batch).unwrap(), Acked::Done));
        assert!(matches!(ak(&w, "a", &r.batch).unwrap(), Acked::Duplicate));
        let e = ak(&w, "a", "ffffffffffffffff").unwrap_err();
        assert_eq!(
            (e.code, e.check, e.message.as_str()),
            (2, "ack", "stale receipt")
        );
        let mail = w.state().join("mail");
        let mut c = receipt::load(&mail, "pane-1").unwrap();
        let offset = c.offset;
        c.pending = Some(Receipt {
            batch: "old".into(),
            owner: me("a"),
            end: 1,
            ..Receipt::default()
        });
        receipt::commit(&mail, "pane-1", &c).unwrap();
        assert!(matches!(ak(&w, "a", "old").unwrap(), Acked::Done));
        assert_eq!(
            receipt::load(&mail, "pane-1").unwrap().offset,
            offset,
            "the offset never moves backward"
        );
    }

    #[test]
    fn a_new_owner_gets_a_rebuilt_batch() {
        let w = World::human("");
        put(&w, H1, "one");
        let r = sv(&w, "a").unwrap().unwrap();
        let r2 = sv(&w, "b").unwrap().unwrap(); // a resumed successor: new id, gen "0"
        assert_eq!(
            (r2.ids.clone(), r2.batch != r.batch),
            (vec![H1.to_string()], true)
        );
        assert_eq!(
            ak(&w, "a", &r.batch).unwrap_err().check,
            "ack",
            "the predecessor ack is refused"
        );
        assert!(matches!(ak(&w, "b", &r2.batch).unwrap(), Acked::Done));
    }

    #[test]
    fn a_failed_file_flush_keeps_the_old_cursor() {
        let w = World::human("");
        put(&w, H1, "one");
        let r = sv(&w, "a").unwrap().unwrap();
        let mail = w.state().join("mail");
        crate::state::fs::fail_sync_file(Some(&receipt::path(&mail, "pane-1")));
        assert_eq!(ak(&w, "a", &r.batch).unwrap_err().check, "mail");
        crate::state::fs::fail_sync_file(None);
        assert_eq!(
            receipt::load(&mail, "pane-1")
                .unwrap()
                .pending
                .unwrap()
                .batch,
            r.batch,
            "the receipt stands"
        );
        assert!(
            matches!(ak(&w, "a", &r.batch).unwrap(), Acked::Done),
            "a retry commits"
        );
    }

    #[test]
    fn a_failed_folder_flush_blocks_new_receipts_until_flushed() {
        let w = World::human("");
        put(&w, H1, "one");
        let r = sv(&w, "a").unwrap().unwrap();
        let mail = w.state().join("mail");
        put(&w, H2, "two"); // seeded before the fault (F14): the mailbox folder flush of deliver_once must succeed
        crate::state::fs::fail_sync_dir(Some(&mail));
        assert_eq!(
            ak(&w, "a", &r.batch).unwrap_err().check,
            "mail",
            "renamed, folder not flushed"
        );
        let e = sv(&w, "a").unwrap_err();
        assert_eq!((e.code, e.message.as_str()), (1, receipt::NOT_ON_DISK));
        assert!(
            receipt::load(&mail, "pane-1").unwrap().pending.is_none(),
            "no receipt issued"
        );
        crate::state::fs::fail_sync_dir(None);
        assert!(
            matches!(ak(&w, "a", &r.batch).unwrap(), Acked::Duplicate),
            "the retry is retired"
        );
    }

    #[test]
    fn the_17th_ack_ages_out_the_oldest() {
        let w = World::human("");
        let mut first = String::new();
        for n in 0..17u64 {
            put(&w, &format!("{n:016x}"), "m");
            let r = sv(&w, "a").unwrap().unwrap();
            if n == 0 {
                first = r.batch.clone();
            }
            ak(&w, "a", &r.batch).unwrap();
        }
        assert_eq!(ak(&w, "a", &first).unwrap_err().message, "stale receipt");
    }

    fn msg(id: &str, re: Option<&str>, hop: u64, cast: Option<&str>) -> Value {
        json!({"id": id, "from": "a", "pane": "%5", "at": "1970-01-01T00:00:00.0000000Z", "body": "hi",
            "fromBox": "aaaaaaaaaaaa", "re": re, "hop": hop, "cast": cast})
    }

    #[test]
    fn deliver_once_flushes_on_every_call_and_keeps_one_copy() {
        let w = World::human("");
        let caller = w.caller();
        let mail = w.state().join("mail");
        std::fs::create_dir_all(&mail).unwrap();
        let file = mail.join("bbbbbbbbbbbb.jsonl");
        let m = msg("0123456789abcdef", None, 0, Some("team"));
        crate::state::fs::fail_sync_dir(Some(&mail));
        assert!(
            deliver_once(&w.ctx, &caller, "bbbbbbbbbbbb", &m).is_err(),
            "a failed folder flush refuses"
        );
        assert!(
            deliver_once(&w.ctx, &caller, "bbbbbbbbbbbb", &m).is_err(),
            "a duplicate hit flushes again"
        );
        crate::state::fs::fail_sync_dir(None);
        crate::state::fs::fail_sync_file(Some(&file));
        assert!(
            deliver_once(&w.ctx, &caller, "bbbbbbbbbbbb", &m).is_err(),
            "a failed file flush refuses"
        );
        crate::state::fs::fail_sync_file(None);
        deliver_once(&w.ctx, &caller, "bbbbbbbbbbbb", &m).unwrap();
        assert_eq!(
            read(&file).lines().count(),
            1,
            "one copy, whatever the retries"
        );
        let found = find_message(&w.ctx, &caller, "bbbbbbbbbbbb", "0123456789abcdef")
            .unwrap()
            .unwrap();
        assert_eq!(found["cast"], "team");
        assert!(
            find_message(&w.ctx, &caller, "bbbbbbbbbbbb", "ffffffffffffffff")
                .unwrap()
                .is_none()
        );
        assert!(
            find_message(&w.ctx, &caller, "cccccccccccc", "0123456789abcdef")
                .unwrap()
                .is_none()
        );
    }

    /// QA fix 4: a `mail` folder `state_dir` just made is an entry of the server folder, flushed before success,
    /// on every call (a duplicate hit included).
    #[test]
    fn deliver_once_flushes_the_entry_of_a_new_mail_folder() {
        let w = World::human("");
        let caller = w.caller();
        let server = crate::agents::server_dir(&w.ctx, &caller);
        let mail = server.join("mail");
        assert!(!mail.exists(), "no mail folder yet: deliver_once makes it");
        let m = msg("0123456789abcdef", None, 0, None);
        crate::state::fs::fail_sync_dir(Some(&server));
        let r = deliver_once(&w.ctx, &caller, "bbbbbbbbbbbb", &m);
        assert!(
            r.is_err(),
            "the server folder holds the new mail entry: {r:?}"
        );
        assert!(
            deliver_once(&w.ctx, &caller, "bbbbbbbbbbbb", &m).is_err(),
            "a duplicate hit flushes it again"
        );
        crate::state::fs::fail_sync_dir(None);
        deliver_once(&w.ctx, &caller, "bbbbbbbbbbbb", &m).unwrap();
        assert_eq!(read(&mail.join("bbbbbbbbbbbb.jsonl")).lines().count(), 1);
    }

    #[test]
    fn stored_and_read_fields_and_header() {
        let w = World::human("");
        let caller = w.caller();
        deliver_once(
            &w.ctx,
            &caller,
            "pane-1",
            &msg("1111111111111111", None, 0, None),
        )
        .unwrap();
        deliver_once(
            &w.ctx,
            &caller,
            "pane-1",
            &msg("2222222222222222", Some("1111111111111111"), 2, None),
        )
        .unwrap();
        deliver_once(
            &w.ctx,
            &caller,
            "pane-1",
            &msg("3333333333333333", None, 0, Some("children")),
        )
        .unwrap();
        let read = read_mail(&w.ctx, &caller, "pane-1").unwrap();
        let heads: Vec<String> = read
            .iter()
            .map(|m| text(m.get("text")).split("\n").next().unwrap().into())
            .collect();
        assert_eq!(
            heads,
            [
                "[mux-agent from a (%5) #1111111111111111]",
                "[mux-agent from a (%5) #2222222222222222 re #1111111111111111 hop 2]",
                "[mux-agent from a (%5) #3333333333333333 to children]",
            ]
        );
        let keys: Vec<&str> = read[1].keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["id", "from", "pane", "at", "re", "hop", "cast", "text"]
        );
        assert_eq!(
            (&read[1]["re"], &read[1]["hop"], &read[0]["cast"]),
            (&json!("1111111111111111"), &json!(2), &Value::Null)
        );
    }

    #[test]
    fn mailbox_key_id_or_pane_n() {
        assert_eq!(mailbox_key("%9", "0123456789ab").unwrap(), "0123456789ab");
        assert_eq!(mailbox_key("%12", "").unwrap(), "pane-12");
        // A tag that is not a well-formed id falls back to the pane.
        for tag in [
            "0123456789AB",
            "0123456789a",
            "0123456789abc",
            "tag-7",
            "../x",
        ] {
            assert_eq!(mailbox_key("%3", tag).unwrap(), "pane-3", "{tag}");
        }
        for pane in ["", "%", "3", "%3a", "%-3", "% 3", "%3\n", "$3", "@3"] {
            assert_eq!(
                refusal(mailbox_key(pane, "")),
                "2 target not a pane",
                "{pane:?}"
            );
        }
    }

    #[test]
    fn add_mail_seals_torn_line() {
        let w = World::human("");
        let caller = w.caller();
        let mailbox = w.state().join("mail").join("pane-1.jsonl");
        let torn = r#"{"id":"exit-aaaaaaaaaaaa""#;
        std::fs::write(&mailbox, torn).unwrap();
        let id = add_mail(&w.ctx, &caller, "pane-1", "worker", "%9", "hi", None).unwrap();
        assert!(
            id.len() == 8 && id.bytes().all(|b| b.is_ascii_hexdigit()),
            "{id}"
        );
        let one = line(&id, "worker", "%9", "hi");
        assert_eq!(read(&mailbox), format!("{torn}\n{one}\n"));
        // A complete last line gets no extra LF.
        let id2 = add_mail(&w.ctx, &caller, "pane-1", "root", "%1", "two", None).unwrap();
        let two = line(&id2, "root", "%1", "two");
        assert_eq!(read(&mailbox), format!("{torn}\n{one}\n{two}\n"));
        // The sealed line does not parse, so read skips it.
        let read_back: Vec<String> = read_mail(&w.ctx, &caller, "pane-1")
            .unwrap()
            .iter()
            .map(|m| text(m.get("id")))
            .collect();
        assert_eq!(read_back, [id, id2]);
        // A new mailbox is made with the one line.
        let id3 = add_mail(&w.ctx, &caller, "bbbbbbbbbbbb", "root", "%1", "x", None).unwrap();
        let fresh = w.state().join("mail").join("bbbbbbbbbbbb.jsonl");
        assert_eq!(read(&fresh), format!("{}\n", line(&id3, "root", "%1", "x")));
    }

    #[test]
    fn add_mail_message_id_once() {
        let w = World::human("");
        let caller = w.caller();
        let mailbox = w.state().join("mail").join("pane-1.jsonl");
        let add = |body: &str, id: Option<&str>| {
            add_mail(&w.ctx, &caller, "pane-1", "worker", "%9", body, id).unwrap()
        };
        let id = "exit-aaaaaaaaaaaa";
        assert_eq!(add("first", Some(id)), id);
        assert_eq!(add("second", Some(id)), id);
        assert_eq!(
            read(&mailbox),
            format!("{}\n", line(id, "worker", "%9", "first"))
        );

        // A complete line holding the id that lost only its LF gets the LF, nothing more.
        let done = line(
            "exit-bbbbbbbbbbbb",
            "done",
            "%8",
            "done exited with code 7.",
        );
        std::fs::write(&mailbox, &done).unwrap();
        add("again", Some("exit-bbbbbbbbbbbb"));
        assert_eq!(read(&mailbox), format!("{done}\n"));

        // A torn line holding the id does not count: it is sealed and the message added.
        let torn = r#"{"id":"exit-cccccccccccc""#;
        std::fs::write(&mailbox, torn).unwrap();
        add("real", Some("exit-cccccccccccc"));
        let real = line("exit-cccccccccccc", "worker", "%9", "real");
        assert_eq!(read(&mailbox), format!("{torn}\n{real}\n"));

        // Without an id (or with an empty one) the same body is added each time.
        std::fs::write(&mailbox, "").unwrap();
        let a = add("same", None);
        let b = add("same", Some(""));
        assert_ne!(a, b);
        assert_eq!(read(&mailbox).lines().count(), 2);
    }

    #[test]
    fn add_once_keeps_fields_and_answers_the_delivered_one() {
        let w = World::human("");
        let caller = w.caller();
        let mailbox = w.state().join("mail").join("pane-1.jsonl");
        let id = "exit-aaaaaaaaaaaa";
        let mut stopped = Map::new();
        stopped.insert("kind".into(), "stopped".into());
        stopped.insert("exitCode".into(), Value::Null);
        let first = add_once(
            &w.ctx,
            &caller,
            "pane-1",
            "worker",
            "%9",
            "worker was stopped.",
            id,
            &stopped,
        );
        assert_eq!(first.unwrap(), None);
        // The fields follow the body, so the stored line says what the notice was.
        let stored = r#"{"id":"exit-aaaaaaaaaaaa","from":"worker","pane":"%9","at":"1970-01-01T00:00:00.0000000Z","body":"worker was stopped.","kind":"stopped","exitCode":null}"#;
        assert_eq!(read(&mailbox), format!("{stored}\n"));
        // A second post of that id adds nothing and answers the message already there.
        let mut exited = Map::new();
        exited.insert("kind".into(), "exited".into());
        exited.insert("exitCode".into(), 0.into());
        let again = add_once(
            &w.ctx,
            &caller,
            "pane-1",
            "worker",
            "%9",
            "worker exited with code 0.",
            id,
            &exited,
        )
        .unwrap()
        .unwrap();
        assert_eq!(again, serde_json::from_str::<Value>(stored).unwrap());
        assert_eq!(read(&mailbox), format!("{stored}\n"));
        // read hands out the same five fields as before.
        let messages = read_mail(&w.ctx, &caller, "pane-1").unwrap();
        let keys: Vec<&str> = messages[0].keys().map(String::as_str).collect();
        assert_eq!(keys, ["id", "from", "pane", "at", "text"]);
    }

    #[test]
    fn unread_tolerant_offset_rules() {
        let dir = TempDir::new();
        let mailbox = dir.path().join("t.jsonl");
        let cursor = dir.path().join("t.offset");
        std::fs::write(&mailbox, "{\"id\":\"1\"}\n{\"id\":\"2\"}\n").unwrap();
        let ids = |unread: &Unread| {
            parse_mail(&unread.text)
                .iter()
                .map(|m| text(m.get("id")))
                .collect::<Vec<_>>()
                .join(",")
        };
        // Not a number, out of range or inside a line counts from the start; the cursor is never written.
        for (value, want) in [
            ("garbage", "1,2"),
            ("-5", "1,2"),
            ("999", "1,2"),
            ("1", "1,2"),
            ("", "1,2"),
            ("+11", "1,2"),
            ("0", "1,2"),
            ("11", "2"),
            (" 11 \n", "2"),
            ("22", ""),
        ] {
            std::fs::write(&cursor, value).unwrap();
            let unread = unread_text(&mailbox, &cursor, true).unwrap();
            assert_eq!(ids(&unread), want, "{value:?}");
            assert_eq!(read(&cursor), value);
        }

        // Strict: the offset as is; the torn last line is held back; past the end is nothing at that offset.
        std::fs::write(&mailbox, "{\"id\":\"1\"}\n{\"id\":\"2\"}\n{\"id\":\"3\"").unwrap();
        std::fs::write(&cursor, "11").unwrap();
        let unread = unread_text(&mailbox, &cursor, false).unwrap();
        assert_eq!(
            unread,
            Unread {
                text: "{\"id\":\"2\"}\n".into(),
                end: 22
            }
        );
        assert_eq!(read(&cursor), "11");
        std::fs::write(&cursor, "999").unwrap();
        assert_eq!(
            unread_text(&mailbox, &cursor, false).unwrap(),
            Unread {
                text: String::new(),
                end: 999
            }
        );
        std::fs::write(&cursor, "garbage").unwrap();
        assert_eq!(
            refusal(unread_text(&mailbox, &cursor, false)),
            "1 internal unexpected error (mail offset is not a number)"
        );
        // A missing cursor is 0; a missing mailbox is nothing at the cursor's offset.
        let none = dir.path().join("none.offset");
        assert_eq!(unread_text(&mailbox, &none, false).unwrap().end, 22);
        std::fs::write(&cursor, "11").unwrap();
        assert_eq!(
            unread_text(&dir.path().join("none.jsonl"), &cursor, true).unwrap(),
            Unread {
                text: String::new(),
                end: 11
            }
        );
        // Only whole JSON lines parse.
        let parsed = parse_mail("{\"id\":\"a\"}\n{bad\n\n{\"id\":\"b\"}\r\n");
        assert_eq!(
            parsed.iter().map(|m| text(m.get("id"))).collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    #[test]
    fn read_mail_moves_offset_under_lock() {
        let w = World::human("");
        let caller = w.caller();
        let mail = w.state().join("mail");
        let ids: Vec<String> = ["one", "two\nlines"]
            .iter()
            .map(|body| add_mail(&w.ctx, &caller, "pane-1", "worker", "%9", body, None).unwrap())
            .collect();
        let cursor = mail.join("pane-1.offset");
        let seen = cursor.clone();
        w.fakes
            .lock_probe(move |_| std::fs::read_to_string(&seen).unwrap_or_else(|_| "none".into()));
        w.fakes.clear_log();
        let messages = read_mail(&w.ctx, &caller, "pane-1").unwrap();
        let end = std::fs::metadata(mail.join("pane-1.jsonl")).unwrap().len();
        let key = format!("mail|{}|pane-1", caller.server_key);
        assert_eq!(
            w.fakes.locks(),
            [format!("+{key} none"), format!("-{key} {end}")]
        );
        assert_eq!(read(&cursor), end.to_string());
        assert_eq!(
            messages[0].keys().collect::<Vec<_>>(),
            ["id", "from", "pane", "at", "text"]
        );
        let texts: Vec<String> = messages.iter().map(|m| text(m.get("text"))).collect();
        assert_eq!(
            texts,
            [
                format!("[mux-agent from worker (%9) #{}]\none", ids[0]),
                format!("[mux-agent from worker (%9) #{}]\ntwo\nlines", ids[1])
            ]
        );
        // Read once: the next read is empty and leaves the cursor; an empty mailbox writes no cursor.
        assert!(read_mail(&w.ctx, &caller, "pane-1").unwrap().is_empty());
        assert_eq!(read(&cursor), end.to_string());
        assert!(read_mail(&w.ctx, &caller, "pane-2").unwrap().is_empty());
        assert!(!mail.join("pane-2.offset").exists());
    }

    #[test]
    fn read_mail_reads_inside_the_lock() {
        // A message that lands just before the lock is taken is read with the others, and the cursor moves past it.
        let w = World::human("");
        let caller = w.caller();
        let mail = w.state().join("mail");
        for body in ["one", "two"] {
            add_mail(&w.ctx, &caller, "pane-1", "worker", "%9", body, None).unwrap();
        }
        let mailbox = mail.join("pane-1.jsonl");
        let key = format!("+mail|{}|pane-1", caller.server_key);
        let (target, done) = (mailbox.clone(), std::cell::Cell::new(false));
        w.fakes.lock_probe(move |event| {
            if event == key && !done.replace(true) {
                let line = r#"{"id":"m3","from":"worker","pane":"%9","at":"x","body":"three"}"#;
                append(&target, format!("{line}\n").as_bytes()).unwrap();
            }
            String::new()
        });
        let messages = read_mail(&w.ctx, &caller, "pane-1").unwrap();
        let bodies: Vec<String> = messages
            .iter()
            .map(|m| text(m.get("text")).rsplit('\n').next().unwrap().to_string())
            .collect();
        assert_eq!(bodies, ["one", "two", "three"]);
        let end = std::fs::metadata(&mailbox).unwrap().len();
        assert_eq!(read(&mail.join("pane-1.offset")), end.to_string());
    }

    #[test]
    fn unread_counts_by_from_and_pane_without_state() {
        let w = World::human("");
        let caller = w.caller();
        assert!(unread_counts(&w.ctx, &caller, "pane-1").unwrap().is_empty());
        assert!(!w.ctx.state_root.exists());
        for (from, pane) in [("worker", "%9"), ("worker", "%9"), ("idle", "%8")] {
            add_mail(&w.ctx, &caller, "pane-1", from, pane, "x", None).unwrap();
        }
        let mail = w.state().join("mail");
        std::fs::write(mail.join("pane-1.offset"), "garbage").unwrap();
        let counts = unread_counts(&w.ctx, &caller, "pane-1").unwrap();
        assert_eq!(
            (counts.get("worker|%9"), counts.get("idle|%8"), counts.len()),
            (Some(&2), Some(&1), 2)
        );
        assert_eq!(read(&mail.join("pane-1.offset")), "garbage");
    }

    #[test]
    fn message_text_strips_c0_keeps_lf_tab() {
        let c0: String = (0u8..=0x1f).chain([0x7f]).map(char::from).collect();
        assert_eq!(message_text(&format!("a{c0}b")), "a\t\nb");
        assert_eq!(message_text("a\r\nb\rc\u{1b}[31md\u{7}"), "a\nbc[31md");
        // C1 stays in a message; only the doing line drops it.
        assert_eq!(message_text("a\u{9b}b\u{85}"), "a\u{9b}b\u{85}");
        assert_eq!(message_text(""), "");
    }

    #[test]
    fn doing_line_80_chars_no_split_surrogate() {
        let emoji = "\u{1F600}";
        assert_eq!(doing_line(&"x".repeat(100)), "x".repeat(80));
        let y = |n| "y".repeat(n);
        assert_eq!(doing_line(&format!("{}{emoji}z", y(79))), y(79));
        assert_eq!(
            doing_line(&format!("{}{emoji}z", y(78))),
            format!("{}{emoji}", y(78))
        );
        // UTF-16 units, not bytes.
        assert_eq!(doing_line(&"\u{e9}".repeat(100)), "\u{e9}".repeat(80));
        // The first non-blank line with C0, DEL and C1 removed, tabs as spaces, trimmed.
        assert_eq!(
            doing_line("\u{9b}\r\n \t\n\ta\tb\u{1b}[31m\u{9b}c\u{7f}\u{85}\u{7}  \r\nsecond"),
            "a b[31mc"
        );
        let c1: String = (0x80u8..=0x9f).map(char::from).collect();
        assert_eq!(doing_line(&format!("a{c1}b")), "ab");
        assert_eq!(doing_line(" \t\r\n\u{9b}\n"), "");
    }
}
