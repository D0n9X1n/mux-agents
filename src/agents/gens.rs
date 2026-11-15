//! The one reader of generation evidence (#103 state-0 S5-S7): `gens/<n>.claim` and `gens/<n>/<file>.json`.
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{Refusal, Res};
use crate::state::evidence::{self, Evidence};

/// Every create-once file of a generation (S5); the claim lives beside the folder.
pub const FILES: [&str; 15] = [
    "spec",
    "launch",
    "commit",
    "launcher",
    "exit",
    "client",
    "report",
    "ready",
    "failure",
    "outcome",
    "ended",
    "terminating",
    "release",
    "released",
    "notice",
];

/// `gens/<n>.claim`.
pub fn claim_path(member: &Path, n: u64) -> PathBuf {
    member.join("gens").join(format!("{n}.claim"))
}

/// `gens/<n>/`.
pub fn dir(member: &Path, n: u64) -> PathBuf {
    member.join("gens").join(n.to_string())
}

/// `gens/<n>/<file>.json`; `file` is one of `FILES`.
pub fn path(member: &Path, n: u64, file: &str) -> PathBuf {
    debug_assert!(FILES.contains(&file), "{file}");
    dir(member, n).join(format!("{file}.json"))
}

pub fn read(member: &Path, n: u64, file: &str) -> Evidence {
    evidence::read(&path(member, n, file))
}

pub fn claim(member: &Path, n: u64) -> Evidence {
    evidence::read(&claim_path(member, n))
}

/// The notice delivery id of generation `n` of member `id`.
pub fn delivery_id(id: &str, n: u64) -> String {
    format!("exit-{id}-{n}")
}

/// A record made by `tx.create` (`lifecycle: 1`); any other is pre-lifecycle (prelife-1c).
pub fn lifecycle(record: &Value) -> bool {
    record.get("lifecycle").and_then(Value::as_u64) == Some(1)
}

/// The active generation a lifecycle record names.
pub fn active(record: &Value) -> Option<u64> {
    record.get("generation")?.as_str()?.parse().ok()
}

/// The generation `record.pending` names.
pub fn pending(record: &Value) -> Option<u64> {
    record.get("pending")?.get("gen")?.as_u64()
}

/// launch-1 G1.1: 1 + the highest generation any file under `gens/` or `pending` names; 0 for none.
pub fn next(member: &Path, record: Option<&Value>) -> Res<u64> {
    let mut high: Option<u64> = record.and_then(pending);
    if let Some(a) = record.and_then(active) {
        high = high.max(Some(a));
    }
    match std::fs::read_dir(member.join("gens")) {
        Ok(entries) => {
            for e in entries {
                let name = e?.file_name();
                let name = name.to_string_lossy();
                let stem = name.strip_suffix(".claim").unwrap_or(&name);
                if let Ok(n) = stem.parse::<u64>() {
                    high = high.max(Some(n));
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(high.map_or(0, |h| h + 1))
}

/// S6, the one completion predicate: markers first (claim `abandoned`, commit `aborted`, `ended`, `released`),
/// then a reread of `record.json`; never done while `record.completing.gen` is `n` (release-1d R4). `Unreadable`
/// anywhere is the `evidence` refusal, never "not done".
pub fn done(member: &Path, n: u64) -> Res<bool> {
    let abandoned = claim(member, n).known()?.is_some_and(|c| c == "abandoned");
    let aborted = read(member, n, "commit")
        .known()?
        .is_some_and(|c| c.get("aborted") == Some(&Value::Bool(true)));
    let ended = read(member, n, "ended").known()?.is_some();
    let released = read(member, n, "released").known()?.is_some();
    if !(abandoned || aborted || ended || released) {
        return Ok(false);
    }
    let record = evidence::read(&member.join("record.json"))
        .known()?
        .ok_or_else(|| Refusal::internal("a generation has no member record"))?;
    let completing = record
        .get("completing")
        .and_then(|c| c.get("gen"))
        .and_then(Value::as_u64);
    Ok(completing != Some(n))
}

/// Whether generation `n` committed (commit `{committed: true}`); `Unreadable` refuses.
pub fn committed(member: &Path, n: u64) -> Res<bool> {
    Ok(read(member, n, "commit")
        .known()?
        .is_some_and(|c| c.get("committed") == Some(&Value::Bool(true))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::evidence::{Published, publish};
    use serde_json::json;

    fn member() -> (crate::testing::TempDir, PathBuf) {
        let d = crate::testing::TempDir::new();
        let m = d.path().join("agents/aaaaaaaaaaaa");
        std::fs::create_dir_all(&m).unwrap();
        let r = json!({"lifecycle": 1, "generation": "1"});
        crate::state::fs::write_json(&m.join("record.json"), &r).unwrap();
        (d, m)
    }

    #[test]
    fn generation_zero_reads_only_its_own_folder() {
        let (_d, m) = member();
        // A folder-level (pre-lifecycle) notice is not generation 0s.
        crate::state::fs::write_json(&m.join("notice.json"), &json!({})).unwrap();
        assert_eq!(read(&m, 0, "notice"), Evidence::Absent);
        assert_eq!(
            publish(&path(&m, 0, "notice"), &json!({})).unwrap(),
            Published::Published
        );
        assert!(matches!(read(&m, 0, "notice"), Evidence::Present(_)));
        assert_eq!(read(&m, 1, "notice"), Evidence::Absent);
        assert_eq!(delivery_id("aaaaaaaaaaaa", 1), "exit-aaaaaaaaaaaa-1");
        assert_ne!(
            delivery_id("aaaaaaaaaaaa", 0),
            delivery_id("aaaaaaaaaaaa", 1)
        );
    }

    #[test]
    fn next_counts_every_file_and_pending() {
        let (_d, m) = member();
        assert_eq!(next(&m, None).unwrap(), 0);
        publish(&claim_path(&m, 0), &json!("run")).unwrap();
        assert_eq!(next(&m, None).unwrap(), 1);
        publish(&path(&m, 3, "spec"), &json!({})).unwrap();
        assert_eq!(next(&m, None).unwrap(), 4);
        assert_eq!(next(&m, Some(&json!({"pending": {"gen": 6}}))).unwrap(), 7);
    }

    #[test]
    fn done_reads_markers_then_the_record_and_honours_completing() {
        let (_d, m) = member();
        assert!(!done(&m, 1).unwrap());
        publish(&path(&m, 1, "ended"), &json!({"state": "finished"})).unwrap();
        let (got, reads) = crate::state::fs::record_reads(|| done(&m, 1));
        assert!(got.unwrap());
        assert_eq!(
            reads.last().unwrap(),
            &m.join("record.json"),
            "the record is reread last"
        );
        let r = json!({"lifecycle": 1, "completing": {"gen": 1}});
        crate::state::fs::write_json(&m.join("record.json"), &r).unwrap();
        assert!(!done(&m, 1).unwrap(), "completing holds the reservation");
        for (file, value) in [
            ("commit", json!({"aborted": true})),
            ("released", json!({})),
        ] {
            let (_d, m) = member();
            publish(&path(&m, 2, file), &value).unwrap();
            assert!(done(&m, 2).unwrap(), "{file}");
        }
        let (_d, m) = member();
        publish(&claim_path(&m, 2), &json!("abandoned")).unwrap();
        assert!(done(&m, 2).unwrap());
    }

    #[test]
    fn an_unreadable_marker_stops_the_caller() {
        let (_d, m) = member();
        std::fs::create_dir_all(dir(&m, 1)).unwrap();
        std::fs::write(path(&m, 1, "ended"), br#"{"st"#).unwrap();
        assert_eq!(done(&m, 1).map_err(|e| e.check), Err("evidence"));
        std::fs::write(claim_path(&m, 1), br#""aban"#).unwrap();
        assert_eq!(done(&m, 1).map_err(|e| e.check), Err("evidence"));
    }

    // Ended is recorded: once ended.json is published, hc forgetting the terminal changes nothing.
    #[test]
    fn a_recorded_end_stays_done_after_hc_forgets() {
        let (_d, m) = member();
        let facts = json!({"address": "t1", "hcName": "m0123abcd", "labels": {"mux.gen": "1"},
            "state": "finished", "reason": "exited", "exitCode": 0, "at": "2026-11-14T00:00:00Z"});
        assert_eq!(
            publish(&path(&m, 1, "ended"), &facts).unwrap(),
            Published::Published
        );
        assert!(done(&m, 1).unwrap());
        assert_eq!(
            publish(&path(&m, 1, "ended"), &json!({})).unwrap(),
            Published::Exists
        );
        assert_eq!(read(&m, 1, "ended"), Evidence::Present(facts));
    }

    // S6: completing holds every kind of done, not only ended.
    #[test]
    fn completing_holds_every_kind_of_done() {
        for kind in ["abandoned", "aborted", "ended", "released"] {
            let (_d, m) = member();
            let at = match kind {
                "abandoned" => claim_path(&m, 1),
                "aborted" => path(&m, 1, "commit"),
                _ => path(&m, 1, kind),
            };
            let body = match kind {
                "abandoned" => json!("abandoned"),
                "aborted" => json!({"aborted": true}),
                _ => json!({}),
            };
            assert_eq!(publish(&at, &body).unwrap(), Published::Published);
            assert_eq!(done(&m, 1).ok(), Some(true), "{kind}");
            let r = json!({"lifecycle": 1, "completing": {"gen": 1}});
            crate::state::fs::write_json(&m.join("record.json"), &r).unwrap();
            assert_eq!(done(&m, 1).ok(), Some(false), "{kind}: completing holds it");
        }
    }

    // S6: an unreadable record.json stops done; it never means done.
    #[test]
    fn an_unreadable_record_stops_done() {
        let (_d, m) = member();
        publish(&path(&m, 1, "ended"), &json!({})).unwrap();
        std::fs::write(m.join("record.json"), b"{").unwrap();
        assert_eq!(done(&m, 1).map_err(|e| e.check), Err("evidence"));
    }
}
