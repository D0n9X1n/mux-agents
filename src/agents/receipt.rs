//! Mailbox receipts (#103 4b-i): `mail/<key>.cursor.json` = `{offset, held, pending?, idPending?, acked, uncertain?}`,
//! read and committed whole under the mailbox lock. Shared with the notebook (phase 7b).
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::error::{Refusal, Res, refuse};

pub const HISTORY: usize = 16;
/// Held ids at most: an ack that would hold more is refused until `-Id` drains some. With the history bounded too,
/// a cursor stays far below the 1 MB read cap. shortcut: a count, not bytes; ids are 8 or 16 characters, and
/// `commit` refuses any cursor over the cap, keeping the old one.
pub const HELD_MAX: usize = 1024;
pub const NOT_ON_DISK: &str = "the last acknowledgement is not yet on disk; retry";

/// A reader: its member id (a person: its pane mailbox) and generation. A resumed successor is another owner.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Owner {
    pub id: String,
    pub generation: String,
}

/// One served batch: the ids it covers, those served as references (`refs`, held at the ack), its end offset, its text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Receipt {
    pub batch: String,
    pub owner: Owner,
    pub ids: Vec<String>,
    pub refs: Vec<String>,
    pub end: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cursor {
    pub offset: u64,
    pub held: Vec<String>,
    pub pending: Option<Receipt>,
    pub id_pending: Option<Receipt>,
    pub acked: Vec<(String, Owner)>,
    pub uncertain: Option<(String, Owner)>,
}

pub fn path(mail: &Path, key: &str) -> PathBuf {
    mail.join(format!("{key}.cursor.json"))
}

pub fn legacy_path(mail: &Path, key: &str) -> PathBuf {
    mail.join(format!("{key}.offset"))
}

fn owner_json(o: &Owner) -> Value {
    json!({"id": o.id, "gen": o.generation})
}

fn owner_of(v: &Value) -> Owner {
    Owner {
        id: super::text(v.get("id")),
        generation: super::text(v.get("gen")),
    }
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

impl Receipt {
    fn to_json(&self) -> Value {
        json!({"batch": self.batch, "owner": owner_json(&self.owner), "ids": self.ids, "refs": self.refs,
            "end": self.end, "text": self.text})
    }

    fn from_json(v: &Value) -> Receipt {
        Receipt {
            batch: super::text(v.get("batch")),
            owner: owner_of(&v["owner"]),
            ids: strings(v.get("ids")),
            refs: strings(v.get("refs")),
            end: v["end"].as_u64().unwrap_or(0),
            text: super::text(v.get("text")),
        }
    }
}

fn pair_json(p: &(String, Owner)) -> Value {
    json!({"batch": p.0, "owner": owner_json(&p.1)})
}

fn pair_of(v: &Value) -> (String, Owner) {
    (super::text(v.get("batch")), owner_of(&v["owner"]))
}

impl Cursor {
    pub fn to_json(&self) -> Value {
        let mut v = json!({"offset": self.offset, "held": self.held,
            "acked": self.acked.iter().map(pair_json).collect::<Vec<_>>()});
        if let Some(r) = &self.pending {
            v["pending"] = r.to_json();
        }
        if let Some(r) = &self.id_pending {
            v["idPending"] = r.to_json();
        }
        if let Some(p) = &self.uncertain {
            v["uncertain"] = pair_json(p);
        }
        v
    }

    pub fn from_json(v: &Value) -> Cursor {
        Cursor {
            offset: v["offset"].as_u64().unwrap_or(0),
            held: strings(v.get("held")),
            pending: v.get("pending").map(Receipt::from_json),
            id_pending: v.get("idPending").map(Receipt::from_json),
            acked: v["acked"]
                .as_array()
                .map(|a| a.iter().map(pair_of).collect())
                .unwrap_or_default(),
            uncertain: v.get("uncertain").map(pair_of),
        }
    }

    /// Appends `{batch, owner}` to the history, keeping the last 16 of that owner (R4). The history of any other
    /// owner goes unless it owns a receipt still pending: a predecessor can no longer ack (#103 4b-i).
    pub fn remember(&mut self, batch: &str, owner: &Owner) {
        self.acked.push((batch.into(), owner.clone()));
        let (pending, id_pending) = (&self.pending, &self.id_pending);
        let owns = |r: &Option<Receipt>, o: &Owner| r.as_ref().is_some_and(|r| r.owner == *o);
        self.acked
            .retain(|(_, o)| o == owner || owns(pending, o) || owns(id_pending, o));
        let mine = self.acked.iter().filter(|(_, o)| o == owner).count();
        if mine > HISTORY {
            let first = self
                .acked
                .iter()
                .position(|(_, o)| o == owner)
                .expect("one of this owner");
            self.acked.remove(first);
        }
    }

    pub fn was_acked(&self, batch: &str, owner: &Owner) -> bool {
        self.acked.iter().any(|(b, o)| b == batch && o == owner)
    }
}

/// The cursor of `key`; with none, a legacy `<key>.offset` read once (migrated at the next commit). Caller holds the
/// lock.
pub fn load(mail: &Path, key: &str) -> Res<Cursor> {
    if let Some(v) = crate::state::fs::read_json(&path(mail, key))? {
        return Ok(Cursor::from_json(&v));
    }
    let raw = match std::fs::read_to_string(legacy_path(mail, key)) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Cursor::default()),
        Err(e) => return Err(e.into()),
    };
    let raw = raw.trim_start_matches('\u{feff}').trim();
    let digits = raw.strip_prefix('+').unwrap_or(raw);
    let offset = if raw.is_empty() {
        0
    } else {
        digits
            .parse::<u64>()
            .ok()
            .filter(|_| digits.bytes().all(|b| b.is_ascii_digit()))
            .ok_or_else(|| Refusal::internal("mail offset is not a number"))?
    };
    Ok(Cursor {
        offset,
        ..Cursor::default()
    })
}

/// Commits `c` whole with `write_json_durable`; then the legacy offset goes. A failed file flush leaves the old cursor;
/// a failed folder flush errors after the rename (the new cursor is there, its folder maybe not: R5). A cursor `load`
/// could not read back (over 1 MB) is refused and the old one stays.
pub fn commit(mail: &Path, key: &str, c: &Cursor) -> Res<()> {
    let v = c.to_json();
    if v.to_string().len() as u64 > crate::state::fs::STATE_MAX {
        return Err(Refusal::internal(
            "the mail cursor would be larger than 1 MB",
        ));
    }
    #[cfg(feature = "test-panic")]
    {
        // Live tests only: a one-shot fault named by a file in the mail folder, through the real durable write:
        // fault-file fails the temp-file flush (before the rename), fault-folder the folder sync (after it).
        let file_fault = std::fs::remove_file(mail.join("fault-file")).is_ok();
        let dir_fault = std::fs::remove_file(mail.join("fault-folder")).is_ok();
        if file_fault || dir_fault {
            let mut sync = |at: &Path, is_dir: bool| -> std::io::Result<()> {
                if (is_dir && dir_fault) || (!is_dir && file_fault) {
                    return Err(std::io::Error::other("injected flush failure"));
                }
                if is_dir {
                    crate::state::fs::sync_dir(at)
                } else {
                    crate::state::fs::sync_file(at)
                }
            };
            crate::state::fs::write_json_durable_with(&path(mail, key), &v, &mut sync)?;
            let _ = std::fs::remove_file(legacy_path(mail, key));
            return Ok(());
        }
    }
    crate::state::fs::write_json_durable(&path(mail, key), &v)?;
    let _ = std::fs::remove_file(legacy_path(mail, key));
    Ok(())
}

/// Before an existing receipt is served again, its cursor file and folder are flushed (F5); a failure refuses 1 `mail`.
pub fn settle(mail: &Path, key: &str) -> Res<()> {
    let ok = crate::state::fs::sync_file(&path(mail, key)).is_ok()
        && crate::state::fs::sync_dir(mail).is_ok();
    if ok {
        Ok(())
    } else {
        refuse(1, "mail", NOT_ON_DISK)
    }
}

/// R5: with an uncertain ack, the folder of the cursor is flushed first; a failure refuses 1 `mail` and issues nothing.
pub fn gate(mail: &Path, c: &Cursor) -> Res<()> {
    if c.uncertain.is_some() && crate::state::fs::sync_dir(mail).is_err() {
        return refuse(1, "mail", NOT_ON_DISK);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    #[cfg(feature = "test-panic")]
    #[test]
    fn a_fault_file_fails_one_commit() {
        let d = TempDir::new();
        let mail = d.path();
        std::fs::write(mail.join("fault-folder"), "").unwrap();
        let r = commit(mail, "k", &Cursor::default());
        assert!(r.is_err(), "the folder flush failed");
        assert_eq!(r.unwrap_err().check, "internal");
        assert!(
            path(mail, "k").exists() && !mail.join("fault-folder").exists(),
            "renamed; one shot"
        );
        std::fs::write(mail.join("fault-file"), "").unwrap();
        let c = Cursor {
            offset: 9,
            ..Cursor::default()
        };
        assert!(commit(mail, "k", &c).is_err());
        assert_eq!(load(mail, "k").unwrap().offset, 0, "the old cursor stands");
        commit(mail, "k", &c).unwrap();
    }

    fn o(id: &str) -> Owner {
        Owner {
            id: id.into(),
            generation: "0".into(),
        }
    }

    #[test]
    fn a_legacy_offset_migrates_once_and_the_cursor_round_trips() {
        let d = TempDir::new();
        let mail = d.path();
        std::fs::write(legacy_path(mail, "k"), "42").unwrap();
        let mut c = load(mail, "k").unwrap();
        assert_eq!(c.offset, 42);
        c.held.push("1111111111111111".into());
        c.pending = Some(Receipt {
            batch: "b1".into(),
            owner: o("a"),
            ids: vec!["x".into()],
            refs: vec![],
            end: 50,
            text: "t".into(),
        });
        c.uncertain = Some(("b0".into(), o("a")));
        commit(mail, "k", &c).unwrap();
        assert!(!legacy_path(mail, "k").exists(), "migrated once");
        assert_eq!(load(mail, "k").unwrap(), c);
        std::fs::write(legacy_path(mail, "k"), "7").unwrap();
        assert_eq!(
            load(mail, "k").unwrap().offset,
            42,
            "the cursor wins over a stray offset"
        );
        std::fs::write(legacy_path(mail, "j"), "x1").unwrap();
        assert_eq!(load(mail, "j").unwrap_err().check, "internal");
        assert_eq!(load(mail, "none").unwrap(), Cursor::default());
    }

    #[test]
    fn the_history_keeps_the_last_16_per_owner() {
        let mut c = Cursor::default();
        for n in 0..17 {
            c.remember(&format!("b{n}"), &o("a"));
        }
        assert!(
            !c.was_acked("b0", &o("a"))
                && c.was_acked("b1", &o("a"))
                && c.was_acked("b16", &o("a"))
        );
        c.remember("z", &o("b"));
        assert!(
            c.was_acked("z", &o("b")) && !c.was_acked("z", &o("a")),
            "per owner"
        );
        assert_eq!(c.acked.len(), 1, "a's history went when b acked");
    }

    #[test]
    fn a_predecessors_history_is_pruned_and_the_current_owner_keeps_16() {
        let mut c = Cursor::default();
        for n in 0..16 {
            c.remember(&format!("a{n}"), &o("a"));
        }
        for n in 0..17 {
            c.remember(&format!("b{n}"), &o("b"));
        }
        assert!(
            !c.acked.iter().any(|(_, w)| *w == o("a")),
            "the predecessor's history goes"
        );
        assert_eq!(c.acked.len(), HISTORY);
        assert!(
            !c.was_acked("b0", &o("b"))
                && c.was_acked("b1", &o("b"))
                && c.was_acked("b16", &o("b"))
        );
        // The owner of a receipt still pending keeps its history: it may still ack, and retry an ack.
        let mut c = Cursor::default();
        c.remember("a0", &o("a"));
        c.pending = Some(Receipt {
            owner: o("a"),
            ..Receipt::default()
        });
        c.remember("b0", &o("b"));
        assert!(c.was_acked("a0", &o("a")) && c.was_acked("b0", &o("b")));
    }

    #[test]
    fn a_cursor_too_large_to_load_is_never_committed() {
        let d = TempDir::new();
        let mail = d.path();
        let small = Cursor {
            offset: 5,
            ..Cursor::default()
        };
        commit(mail, "k", &small).unwrap();
        let mut big = small.clone();
        big.held = (0..60_000).map(|n| format!("{n:016x}")).collect();
        assert!(commit(mail, "k", &big).is_err(), "over the read cap");
        assert_eq!(
            load(mail, "k").unwrap(),
            small,
            "the old cursor stays, loadable"
        );
    }

    #[test]
    fn the_gate_flushes_the_folder_of_an_uncertain_ack() {
        let d = TempDir::new();
        let mail = d.path();
        let mut c = Cursor::default();
        crate::state::fs::fail_sync_dir(Some(mail));
        assert!(gate(mail, &c).is_ok(), "nothing uncertain: no flush");
        c.uncertain = Some(("b".into(), o("a")));
        let e = gate(mail, &c).unwrap_err();
        assert_eq!(
            (e.code, e.check, e.message.as_str()),
            (1, "mail", NOT_ON_DISK)
        );
        crate::state::fs::fail_sync_dir(None);
        assert!(gate(mail, &c).is_ok());
    }
}
