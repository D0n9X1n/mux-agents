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
#[cfg_attr(not(test), allow(dead_code))]
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
mod tests {
    use super::*;
    use crate::agents::fixture::{ID, World, write};
    use serde_json::json;

    /// G10a, ambiguity 2: only a whole `notice.json` whose kind is `stopped` or `exited` proves an end.
    #[test]
    fn only_a_stopped_or_exited_notice_proves_an_end() {
        for (case, ended) in [
            ("stopped", true),
            ("exited", true),
            ("lost", false),
            ("other kind", false),
            ("no kind", false),
            ("not an object", false),
            ("torn", false),
            ("folder", false),
            ("none", false),
            ("linked member", false),
        ] {
            let w = World::human("");
            let f = w.agent_record(ID, json!({}));
            let notice = f.join("notice.json");
            match case {
                "stopped" | "exited" | "lost" => write(&notice, json!({"kind": case, "at": "x"})),
                "other kind" => write(&notice, json!({"kind": "paused"})),
                "no kind" => write(&notice, json!({"at": "x"})),
                "not an object" => write(&notice, json!("stopped")),
                "torn" => std::fs::write(&notice, b"{\"kind\": \"stop").unwrap(),
                "folder" => std::fs::create_dir(&notice).unwrap(),
                "linked member" => {
                    let real = w.state().join("elsewhere");
                    std::fs::create_dir_all(&real).unwrap();
                    write(&real.join("notice.json"), json!({"kind": "stopped"}));
                    std::fs::remove_dir_all(&f).unwrap();
                    crate::testing::link_dir(&real, &f);
                }
                _ => {}
            }
            assert_eq!(ended_by_notice(&f), ended, "{case}");
        }
    }

    /// Ambiguity 11: only `lifecycle: 1` passes; every other record is refused `prelife`.
    #[test]
    fn only_a_lifecycle_record_passes_the_lifecycle_only_guard() {
        assert_eq!(lifecycle_only(&json!({"lifecycle": 1})), Ok(()));
        for r in [
            json!({}),
            json!({"lifecycle": 0}),
            json!({"lifecycle": "1"}),
            json!({"lifecycle": 2}),
            json!("record"),
            json!(null),
        ] {
            let e = lifecycle_only(&r).unwrap_err();
            assert_eq!(
                (e.code, e.check, e.message.as_str()),
                (2, "prelife", LIFECYCLE_ONLY),
                "{r}"
            );
        }
    }

    /// G10c, G10d: refused 2 `state`; the stable phrases are what callers and SKILL readers match.
    #[test]
    fn the_refusals_name_the_member_and_the_rule() {
        let e = not_running("w-c");
        assert_eq!((e.code, e.check), (2, "state"));
        assert_eq!(e.message, format!("w-c {NOT_RUNNING}"));
        assert!(e.message.contains("not running"));
        let e = pre_lifecycle("w-c");
        assert_eq!((e.code, e.check), (2, "state"));
        assert_eq!(e.message, format!("w-c {PRE_LIFECYCLE}"));
        assert!(e.message.contains("pre-lifecycle"));
    }

    /// Calls 15, 21: never-ran evidence is exactly a writer's abort shape with valid field types (the dev shape for
    /// this record's identity and address, its kill acknowledged), with no commit; or the abandoned claim of the
    /// generation the record names (absent: 0). Anything else proves nothing.
    #[test]
    fn only_a_writers_abort_or_the_named_abandoned_claim_proves_never_ran() {
        // (case, abort for the record, commit kept, generation, claim of gens/0, proves)
        type Abort = fn(&Value) -> Option<Value>;
        type Case = (
            &'static str,
            Abort,
            bool,
            Option<Value>,
            Option<&'static str>,
            bool,
        );
        let cases: Vec<Case> = vec![
            (
                "v0.2 abort, no pane",
                |_| Some(json!({"abortedAt": "x", "pane": null})),
                false,
                None,
                None,
                true,
            ),
            (
                "v0.2 abort, a pane",
                |_| Some(json!({"abortedAt": "x", "pane": "%9"})),
                false,
                None,
                None,
                true,
            ),
            (
                "v0.2 abort, commit kept",
                |_| Some(json!({"abortedAt": "x", "pane": null})),
                true,
                None,
                None,
                false,
            ),
            (
                "v0.2 abort, number time",
                |_| Some(json!({"abortedAt": 3, "pane": null})),
                false,
                None,
                None,
                false,
            ),
            (
                "v0.2 abort, number pane",
                |_| Some(json!({"abortedAt": "x", "pane": 9})),
                false,
                None,
                None,
                false,
            ),
            (
                "v0.2 abort, extra field",
                |_| Some(json!({"abortedAt": "x", "pane": null, "runUncertain": false})),
                false,
                None,
                None,
                false,
            ),
            (
                "null abort",
                |_| Some(Value::Null),
                false,
                None,
                None,
                false,
            ),
            ("empty abort", |_| Some(json!({})), false, None, None, false),
            ("no abort", |_| None, false, None, None, false),
            (
                "dev abort",
                |r| {
                    Some(
                        json!({"hcAddress": r["hcAddress"], "identity": r, "pane": "%9", "kill": "acknowledged", "viewCleanup": "unproven"}),
                    )
                },
                false,
                None,
                None,
                true,
            ),
            (
                "dev abort, cleanup object",
                |r| {
                    Some(
                        json!({"hcAddress": r["hcAddress"], "identity": r, "pane": null, "kill": "acknowledged", "viewCleanup": {"gone": true}}),
                    )
                },
                false,
                None,
                None,
                true,
            ),
            (
                "dev abort, kill pending",
                |r| {
                    Some(
                        json!({"hcAddress": r["hcAddress"], "identity": r, "pane": "%9", "kill": "pending", "viewCleanup": "pending"}),
                    )
                },
                false,
                None,
                None,
                false,
            ),
            (
                "dev abort, kill uncertain",
                |r| {
                    Some(
                        json!({"hcAddress": r["hcAddress"], "identity": r, "pane": "%9", "kill": {"uncertain": true, "message": "x"}, "viewCleanup": "unproven"}),
                    )
                },
                false,
                None,
                None,
                false,
            ),
            (
                "dev abort, other address",
                |r| {
                    Some(
                        json!({"hcAddress": "tother", "identity": r, "pane": "%9", "kill": "acknowledged", "viewCleanup": "unproven"}),
                    )
                },
                false,
                None,
                None,
                false,
            ),
            (
                "dev abort, other identity",
                |r| {
                    let mut i = r.clone();
                    i["id"] = json!("e0000000000e");
                    Some(
                        json!({"hcAddress": r["hcAddress"], "identity": i, "pane": "%9", "kill": "acknowledged", "viewCleanup": "unproven"}),
                    )
                },
                false,
                None,
                None,
                false,
            ),
            (
                "dev abort, identity not an object",
                |r| {
                    Some(
                        json!({"hcAddress": r["hcAddress"], "identity": "x", "pane": "%9", "kill": "acknowledged", "viewCleanup": "unproven"}),
                    )
                },
                false,
                None,
                None,
                false,
            ),
            (
                "dev abort, number cleanup",
                |r| {
                    Some(
                        json!({"hcAddress": r["hcAddress"], "identity": r, "pane": "%9", "kill": "acknowledged", "viewCleanup": 1}),
                    )
                },
                false,
                None,
                None,
                false,
            ),
            (
                "abandoned gens/0, no generation",
                |_| None,
                true,
                None,
                Some("abandoned"),
                true,
            ),
            (
                "abandoned gens/0, generation 0",
                |_| None,
                true,
                Some(json!("0")),
                Some("abandoned"),
                true,
            ),
            (
                "abandoned gens/0, generation 1",
                |_| None,
                true,
                Some(json!("1")),
                Some("abandoned"),
                false,
            ),
            (
                "abandoned gens/0, generation 01",
                |_| None,
                false,
                Some(json!("01")),
                Some("abandoned"),
                false,
            ),
            (
                "abandoned gens/0, number generation",
                |_| None,
                false,
                Some(json!(0)),
                Some("abandoned"),
                false,
            ),
            (
                "v0.2 abort, invalid generation",
                |_| Some(json!({"abortedAt": "x", "pane": null})),
                false,
                Some(json!("x")),
                None,
                false,
            ),
            ("run gens/0", |_| None, true, None, Some("run"), false),
        ];
        for (case, abort, commit, generation, claim, proves) in cases {
            let w = World::human("");
            let r = crate::agents::core_fixture::member(&w, ID, json!({}));
            let f = w.folder(ID);
            let mut record = r.clone();
            match generation {
                Some(g) => record["generation"] = g,
                None => drop(record.as_object_mut().unwrap().remove("generation")),
            }
            write(&f.join("record.json"), record.clone());
            if !commit {
                std::fs::remove_file(f.join("commit.json")).unwrap();
            }
            if let Some(a) = abort(&r) {
                write(&f.join("abort.json"), a);
            }
            if let Some(c) = claim {
                std::fs::create_dir_all(f.join("gens")).unwrap();
                write(&f.join("gens").join("0.claim"), json!(c));
            }
            assert_eq!(never_ran(&f, &record), proves, "{case}");
        }
    }
}
