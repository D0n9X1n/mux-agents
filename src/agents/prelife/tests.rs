//! prelife-1c Tasks 1-2: the rule (`ended_by_notice`, `never_ran`, `lifecycle_only`) and its refusals.
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
