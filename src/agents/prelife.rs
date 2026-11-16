//! prelife-1c G10: a member folder whose `record.json` lacks `lifecycle: 1` was made by 0.2.x or a v0.3 dev build.
//! It keeps today's snapshot, `list` and validated `stop`; no lifecycle path reads past its record or writes it.
#![cfg_attr(not(test), allow(dead_code))]
use std::path::Path;

use serde_json::Value;

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
pub(crate) fn pre_lifecycle(name: &str) -> Refusal {
    refusal("state", format!("{name} {PRE_LIFECYCLE}"))
}

/// G10a: the folder notice, read through the walk from the state root, is whole and its kind is in `ENDED_KINDS`.
/// Absent, torn, a folder, a link anywhere on the walk, another kind: false (it holds its slot).
pub(crate) fn ended_by_notice(folder: &Path) -> bool {
    match crate::agents::gens::walk(folder, &["notice.json"]) {
        Evidence::Present(n) => n
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|k| ENDED_KINDS.contains(&k)),
        _ => false,
    }
}

/// Ambiguity 11: the backstop of a lifecycle-only writer, run on the record it already read.
pub(crate) fn lifecycle_only(record: &Value) -> Res<()> {
    if crate::agents::gens::lifecycle(record) {
        return Ok(());
    }
    Err(refusal("prelife", LIFECYCLE_ONLY.into()))
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
}
