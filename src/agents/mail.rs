//! Mailboxes: `mail/<key>.jsonl` with a byte offset in `<key>.offset`, one JSON message per LF-ended line
//! (MuxAgents.ps1:1020-1148, 1206-1214).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde_json::{Map, Value, json};

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
    let mailbox = super::state_dir(ctx, caller)?
        .join("mail")
        .join(format!("{key}.jsonl"));
    let given = message_id.filter(|id| !id.is_empty());
    let id = match given {
        Some(id) => id.to_string(),
        None => crate::util::guid()?.replace('-', "")[..8].to_string(),
    };
    let message = json!({"id": id, "from": from, "pane": from_pane, "at": crate::util::utc_o(ctx.clock.now()), "body": body});
    super::lock(ctx, &super::lock_key("mail", caller, key), || {
        // Only a dedup needs the whole mailbox; otherwise its last byte tells whether it is torn.
        let existing = match given {
            Some(_) => read_or_empty(&mailbox)?,
            None => last_byte(&mailbox)?.into_iter().collect(),
        };
        let torn = existing.last().is_some_and(|&b| b != b'\n');
        let delivered = given.is_some_and(|id| {
            String::from_utf8_lossy(&existing)
                .split('\n')
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .any(|stored| super::text(stored.get("id")) == id)
        });
        let mut out = String::new();
        if torn {
            out.push('\n');
        }
        if !delivered {
            out.push_str(&message.to_string());
            out.push('\n');
        }
        if !out.is_empty() {
            append(&mailbox, out.as_bytes())?;
        }
        Ok(())
    })?;
    Ok(id)
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
    let mail = super::state_dir(ctx, caller)?.join("mail");
    let mailbox = mail.join(format!("{key}.jsonl"));
    let cursor = mail.join(format!("{key}.offset"));
    let found = super::lock(ctx, &super::lock_key("mail", caller, key), || {
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
            let text = format!(
                "[mux-agent from {} ({}) #{}]\n{}",
                super::text(message.get("from")),
                super::text(message.get("pane")),
                super::text(message.get("id")),
                super::text(message.get("body"))
            );
            let mut out = Map::new();
            for name in ["id", "from", "pane", "at"] {
                out.insert(name.into(), field(name));
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
        *counts.entry(from).or_insert(0) += 1;
    }
    Ok(counts)
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
