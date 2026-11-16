//! prelife-1c G10: a member folder whose `record.json` lacks `lifecycle: 1` was made by 0.2.x or a v0.3 dev build.
//! It keeps today's snapshot, `list` and validated `stop`; no lifecycle path reads past its record or writes it.
use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{Map, Value};

use crate::agents::core_identity::Identity;
use crate::agents::gens;
use crate::agents::membership::{Local, Stored};
use crate::caller::Caller;
use crate::error::{Refusal, Res};
use crate::state::evidence::Evidence;

/// Ambiguity 2: the folder notice kinds that prove a pre-lifecycle member Ended (0.2.x writes `exited` only from a
/// recorded launcher exit code). `lost` proves nothing.
pub(crate) const ENDED_KINDS: [&str; 2] = ["stopped", "exited"];
pub(crate) const NOT_RUNNING: &str = "is not running where hc can show it: its terminal cannot be read or matched, so nothing is killed or noticed; retry once hc answers, or close its shell with hc or its pane";
pub(crate) const PRE_LIFECYCLE: &str =
    "predates lifecycle records (pre-lifecycle) and is never resumed";
pub(crate) const LIFECYCLE_ONLY: &str =
    "this member predates lifecycle records; lifecycle state is never read or written for it";

fn refusal(check: &'static str, message: String) -> Refusal {
    Refusal {
        code: 2,
        check,
        message,
        reason: None,
        leftover: None,
        details: Default::default(),
    }
}

/// G10c, p1c-calls 4: hc cannot show the legacy member's terminal (Unknown); nothing is killed or noticed.
pub(crate) fn not_running(name: &str) -> Refusal {
    refusal("state", format!("{name} {NOT_RUNNING}"))
}

/// G10d: a pre-lifecycle member never resumes.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn pre_lifecycle(name: &str) -> Refusal {
    refusal("state", format!("{name} {PRE_LIFECYCLE}"))
}

/// G10a: the folder notice, read through the walk from the state root, is whole and its kind is in `ENDED_KINDS`.
/// Absent, torn, a folder, a link anywhere on the walk, another kind: false (it holds its slot).
pub(crate) fn ended_by_notice(folder: &Path) -> bool {
    match gens::walk(folder, &["notice.json"]) {
        Evidence::Present(n) => n
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|k| ENDED_KINDS.contains(&k)),
        _ => false,
    }
}

/// Ambiguity 11: the backstop of a lifecycle-only writer, run on the record it already read.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn lifecycle_only(record: &Value) -> Res<()> {
    if gens::lifecycle(record) {
        return Ok(());
    }
    Err(refusal("prelife", LIFECYCLE_ONLY.into()))
}

/// What a pre-lifecycle folder is to membership (`Local` boxed: clippy `large_enum_variant`).
pub(crate) enum Slot {
    Local(Box<Local>),
    Held,
    Free,
}

/// G10a (ambiguities 2, 5; calls 14, 15, 21, 22): a readable identity is today's `Local`; anything else holds one
/// slot unless its folder notice proves an end or recorded evidence proves it never ran. The caller has skipped
/// another session's record already. Every read is walk-checked and regular-file-only.
pub(crate) fn slot(folder: &Path, record: &Value, id: &str, caller: &Caller) -> Slot {
    if let Some(l) = record
        .is_object()
        .then(|| local(folder, record, id, caller))
        .flatten()
    {
        return Slot::Local(Box::new(l));
    }
    if ended_by_notice(folder) || never_ran(folder, record) {
        return Slot::Free;
    }
    Slot::Held
}

/// Main's reading of a pre-lifecycle record (moved from membership): its identity, the folder commit and notice.
/// None when the record names no usable identity (G10a: held or free, never `invalid local hc identity`).
fn local(folder: &Path, record: &Value, id: &str, caller: &Caller) -> Option<Local> {
    let team = crate::agents::membership::team(caller);
    let hc = ["hcAddress", "generation", "team", "launcherArgv"]
        .iter()
        .any(|k| record.get(k).is_some());
    let identity = if hc {
        Identity::read(record, id, &team)?
    } else {
        // A resume's successor whose hc run reply was lost after its launcher claimed the run: a member whose life
        // comes from hc. Without a claim (or with an abandoned one) it never ran and is no member. Call 14: the
        // claim is read through the walk; an Unreadable one (FIFO, link, torn) counts as claimed.
        let claimed = match gens::claim(folder, 0) {
            Evidence::Present(c) => c.as_str() != Some("abandoned"),
            Evidence::Absent => false,
            Evidence::Unreadable => true,
        };
        Identity::pending(record, id, &team).filter(|_| claimed)?
    };
    Some(Local {
        identity,
        stored: Stored {
            record: record.clone(),
            committed: committed(folder),
            noticed: folder.join("notice.json").exists(),
            notice: None,
            active_done: None,
        },
        held: false,
        finished: None,
    })
}

/// Call 22: main's `folder_committed` through the walk: a whole regular `commit.json` that is no launcher's give-up.
fn committed(folder: &Path) -> bool {
    matches!(gens::walk(folder, &["commit.json"]),
        Evidence::Present(c) if c.get("aborted") != Some(&Value::Bool(true)))
}

/// Calls 15, 21: recorded never-ran evidence. The claim of the generation the record names (absent: 0; present but
/// not a canonical string: nothing is proven) is `"abandoned"`; or no `commit.json` and an `abort.json` in a
/// writer's exact shape (`writer_abort`). Every read is walk-checked: a FIFO, link, folder or torn file proves nothing.
pub(crate) fn never_ran(folder: &Path, record: &Value) -> bool {
    let n = match record.get("generation") {
        None => 0,
        Some(g) => match g.as_str().and_then(gens::plain_gen) {
            Some(n) => n,
            None => return false,
        },
    };
    if matches!(gens::claim(folder, n), Evidence::Present(c) if c == "abandoned") {
        return true;
    }
    if !matches!(gens::walk(folder, &["commit.json"]), Evidence::Absent) {
        return false;
    }
    match gens::walk(folder, &["abort.json"]) {
        Evidence::Present(Value::Object(a)) => writer_abort(&a, folder, record),
        _ => false,
    }
}

/// Call 21: the two abort shapes a writer makes, with their field types. v0.2.x: `{abortedAt: string, pane: string
/// or null}`. v0.3-dev's rollback: `{hcAddress, identity, kill, pane, viewCleanup}` naming this record's
/// `hcAddress` and this folder's id, `kill` `"acknowledged"` (a pending or uncertain kill may have left it running).
fn writer_abort(a: &Map<String, Value>, folder: &Path, record: &Value) -> bool {
    let keys: BTreeSet<&str> = a.keys().map(String::as_str).collect();
    let text = |k: &str| a.get(k).and_then(Value::as_str);
    let pane = a.get("pane").is_some_and(|p| p.is_null() || p.is_string());
    if keys == BTreeSet::from(["abortedAt", "pane"]) {
        return pane && text("abortedAt").is_some();
    }
    let id = folder.file_name().and_then(|n| n.to_str());
    let identity = a
        .get("identity")
        .and_then(|i| i.get("id"))
        .and_then(Value::as_str);
    keys == BTreeSet::from(["hcAddress", "identity", "kill", "pane", "viewCleanup"])
        && pane
        && text("kill") == Some("acknowledged")
        && a.get("viewCleanup")
            .is_some_and(|v| v.is_string() || v.is_object())
        && text("hcAddress")
            .is_some_and(|h| record.get("hcAddress").and_then(Value::as_str) == Some(h))
        && identity.is_some_and(|i| Some(i) == id)
}

#[cfg(test)]
#[path = "prelife_rule_tests.rs"]
mod tests;
