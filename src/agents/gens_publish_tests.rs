//! Call 19: `publish_once` and `publish_claim_once`. One row per condition, one row per flush; each condition or
//! flush removed alone fails one row. Links through `testing::link_dir`, so every row runs on every OS.
use super::*;
use crate::agents::fixture::{ID, World};
use serde_json::json;

fn world() -> World {
    let w = World::human("");
    w.state(); // <key>, agents and mail, as every caller finds them
    w
}

/// Clears every fault and hook when a row ends.
struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        crate::state::fs::fail_sync_dir(None);
        crate::state::fs::fail_sync_file(None);
        crate::state::fs::fail_create_dir(None);
        publish_hooks::before_link(None);
    }
}

/// R-W3: what a call answered, by check and stable phrase, never by io text.
fn outcome(r: &Res<Published>) -> &'static str {
    match r {
        Ok(Published::Published) => "published",
        Ok(Published::Exists) => "exists",
        Err(e) if e.check == "evidence" => "evidence",
        Err(e) if (e.check, e.message.as_str()) == ("state", NOT_PUBLISHED) => "not published",
        Err(e) if (e.check, e.message.as_str()) == ("state", NOT_FLUSHED) => "not flushed",
        Err(_) => "other",
    }
}

fn target(m: &Path, claim: bool) -> std::path::PathBuf {
    if claim {
        claim_path(m, 1)
    } else {
        path(m, 1, "commit")
    }
}

fn publish(m: &Path, claim: bool) -> Res<Published> {
    if claim {
        publish_claim_once(m, 1, &json!("new"))
    } else {
        publish_once(m, 1, "commit", &json!("new"))
    }
}

/// Makes the state a row names. `elsewhere` is an empty folder a link may point at.
fn plant(row: &str, m: &Path, t: &Path, elsewhere: &Path) {
    let agents = m.parent().unwrap();
    let key = agents.parent().unwrap();
    let gens = m.join("gens");
    match row {
        "nothing yet" => {}
        "member only" => std::fs::create_dir(m).unwrap(),
        "present" => {
            crate::state::evidence::publish(t, &json!("earlier")).unwrap();
        }
        "present, link fails" => {
            crate::state::evidence::publish(t, &json!("earlier")).unwrap();
            crate::state::fs::fail_sync_file(Some(t));
        }
        "torn" => {
            std::fs::create_dir_all(t.parent().unwrap()).unwrap();
            std::fs::write(t, b"{\"comm").unwrap();
        }
        "folder at the file" => std::fs::create_dir_all(t).unwrap(),
        "link at gens/1" => {
            std::fs::create_dir_all(&gens).unwrap();
            crate::testing::link_dir(elsewhere, &gens.join("1"));
        }
        "link at gens" => {
            std::fs::create_dir(m).unwrap();
            crate::testing::link_dir(elsewhere, &gens);
        }
        "link at member" => crate::testing::link_dir(elsewhere, m),
        "link at agents" => {
            std::fs::remove_dir(agents).unwrap();
            crate::testing::link_dir(elsewhere, agents);
        }
        // The linked <key> holds a real `agents`, so only the check of <key> itself can refuse.
        "link at key" => {
            std::fs::create_dir(elsewhere.join("agents")).unwrap();
            std::fs::remove_dir_all(key).unwrap();
            crate::testing::link_dir(elsewhere, key);
        }
        "file at gens/1" => {
            std::fs::create_dir_all(&gens).unwrap();
            std::fs::write(gens.join("1"), b"").unwrap();
        }
        "no agents" => std::fs::remove_dir(agents).unwrap(),
        "racer" => publish_hooks::before_link(Some(Box::new(|t: &Path| {
            crate::state::evidence::publish(t, &json!("earlier")).unwrap();
        }))),
        _ => unreachable!("{row}"),
    }
}

/// (row, answer, also run as a claim). Links come before `file at gens/1` (mutation P1 names the first).
const ROWS: &[(&str, &str, bool)] = &[
    ("nothing yet", "published", true),
    ("member only", "published", false),
    ("present", "exists", true),
    ("present, link fails", "exists", true),
    ("torn", "evidence", true),
    ("folder at the file", "evidence", false),
    ("link at gens/1", "evidence", false),
    ("link at gens", "evidence", true),
    ("link at member", "evidence", true),
    ("link at agents", "evidence", true),
    ("link at key", "evidence", true),
    ("file at gens/1", "evidence", false),
    ("no agents", "evidence", true),
    ("racer", "exists", true),
];

// R-W1 conditions: the walk from the state root (a link or non-folder at every level refuses), the levels it may
// make (<id>, gens, gens/<n>; never agents), the read first (Present answers Exists before any write; Unreadable
// refuses), and a racer between the read and the link (Exists, its body stands).
#[test]
fn every_condition_alone_fails_one_row() {
    for (row, want, both) in ROWS {
        for claim in [false, true] {
            if claim && !both {
                continue;
            }
            let w = world();
            let _reset = Reset;
            let m = w.folder(ID);
            let t = target(&m, claim);
            let elsewhere = w.ctx.cwd.join("elsewhere");
            std::fs::create_dir(&elsewhere).unwrap();
            plant(row, &m, &t, &elsewhere);
            let before = std::fs::read(&t).ok();
            let beyond = crate::testing::tree(&elsewhere);
            let got = publish(&m, claim);
            assert_eq!(outcome(&got), *want, "{row} (claim {claim}): {got:?}");
            let read = crate::state::evidence::read(&t);
            match *want {
                "published" => assert_eq!(read, Evidence::Present(json!("new")), "{row}"),
                "exists" => assert_eq!(read, Evidence::Present(json!("earlier")), "{row}"),
                _ => assert_eq!(
                    std::fs::read(&t).ok(),
                    before,
                    "{row} (claim {claim}): untouched"
                ),
            }
            assert_eq!(
                crate::testing::tree(&elsewhere),
                beyond,
                "{row}: nothing through a link"
            );
            if *row == "no agents" {
                assert!(!m.parent().unwrap().exists(), "agents is never made here");
            }
        }
    }
}

// Call 27: a level whose creation fails refuses `evidence`, even when the folder then reads as a real directory
// (the fault makes it, then answers an error).
#[test]
fn a_folder_that_could_not_be_made_refuses() {
    for claim in [false, true] {
        for level in ["member", "gens", "gens/n"] {
            if claim && level == "gens/n" {
                continue;
            }
            let w = world();
            let _reset = Reset;
            let m = w.folder(ID);
            let at = match level {
                "member" => m.clone(),
                "gens" => m.join("gens"),
                _ => dir(&m, 1),
            };
            crate::state::fs::fail_create_dir(Some(&at));
            let got = publish(&m, claim);
            let case = format!("claim {claim}, {level}");
            assert_eq!(outcome(&got), "evidence", "{case}: {got:?}");
            assert!(at.is_dir(), "{case}: the fault made the folder");
            assert_eq!(
                crate::state::evidence::read(&target(&m, claim)),
                Evidence::Absent,
                "{case}"
            );
        }
    }
}

// R-W1 flushes, on every call: the content before the link (a failure leaves no name), then the file's folder up
// to agents (a failure leaves the name visible). Every folder exists first, so no flush depends on having made it;
// `existed` rows prove the flushes run when the file was already there (finding 1to2-C1).
#[test]
fn every_flush_alone_fails_one_row() {
    for claim in [false, true] {
        for existed in [false, true] {
            for at in ["content", "gens/n", "gens", "member", "agents"] {
                if (claim && at == "gens/n") || (existed && at == "content") {
                    continue;
                }
                let w = world();
                let _reset = Reset;
                let m = w.folder(ID);
                let t = target(&m, claim);
                std::fs::create_dir_all(t.parent().unwrap()).unwrap();
                if existed {
                    crate::state::evidence::publish(&t, &json!("earlier")).unwrap();
                }
                match at {
                    "content" => crate::state::fs::fail_sync_file(Some(&t)),
                    "gens/n" => crate::state::fs::fail_sync_dir(Some(&dir(&m, 1))),
                    "gens" => crate::state::fs::fail_sync_dir(Some(&m.join("gens"))),
                    "member" => crate::state::fs::fail_sync_dir(Some(&m)),
                    _ => crate::state::fs::fail_sync_dir(Some(m.parent().unwrap())),
                }
                let got = publish(&m, claim);
                let want = if at == "content" {
                    "not published"
                } else {
                    "not flushed"
                };
                let case = format!("claim {claim}, existed {existed}, {at}");
                assert_eq!(outcome(&got), want, "{case}: {got:?}");
                let left = match (existed, at) {
                    (true, _) => Evidence::Present(json!("earlier")),
                    (false, "content") => Evidence::Absent,
                    _ => Evidence::Present(json!("new")),
                };
                assert_eq!(crate::state::evidence::read(&t), left, "{case}: the name");
            }
        }
    }
}

// R-W1: the folders are flushed innermost first, gens/<n>, gens, the member, agents, and nothing else; on every
// call, the file existing or not.
#[test]
fn the_folders_are_flushed_innermost_first() {
    for claim in [false, true] {
        for existed in [false, true] {
            let w = world();
            let m = w.folder(ID);
            let t = target(&m, claim);
            if existed {
                crate::state::evidence::publish(&t, &json!("earlier")).unwrap();
            }
            let (got, flushed) = crate::state::fs::record_syncs(|| publish(&m, claim));
            let case = format!("claim {claim}, existed {existed}");
            assert!(got.is_ok(), "{case}: {got:?}");
            let mut want = vec![m.join("gens"), m.clone(), m.parent().unwrap().to_path_buf()];
            if !claim {
                want.insert(0, dir(&m, 1));
            }
            assert_eq!(flushed, want, "{case}");
        }
    }
}

// R-W1: the target is read once, before anything is written; an absent target opens nothing.
#[test]
fn the_target_is_read_once() {
    for claim in [false, true] {
        for existed in [false, true] {
            let w = world();
            let m = w.folder(ID);
            let t = target(&m, claim);
            if existed {
                crate::state::evidence::publish(&t, &json!("earlier")).unwrap();
            }
            let (got, reads) = crate::state::fs::record_reads(|| publish(&m, claim));
            let case = format!("claim {claim}, existed {existed}");
            assert!(got.is_ok(), "{case}: {got:?}");
            let want = if existed { vec![t.clone()] } else { vec![] };
            assert_eq!(reads, want, "{case}");
        }
    }
}

// Ambiguity 2, every OS: a reader holding the file (on Windows sharing read and delete only, as `read_json` opens
// it) never fails a publication that finds the file: an existing name is never reopened for writing.
#[test]
fn a_reader_holding_the_file_never_fails_a_publication_that_finds_it() {
    let w = world();
    let m = w.folder(ID);
    let t = path(&m, 1, "commit");
    crate::state::evidence::publish(&t, &json!("earlier")).unwrap();
    let mut open = std::fs::OpenOptions::new();
    open.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        open.share_mode(0x1 | 0x4); // FILE_SHARE_READ | FILE_SHARE_DELETE
    }
    let _held = open.open(&t).unwrap();
    let got = publish_once(&m, 1, "commit", &json!("new"));
    assert_eq!(outcome(&got), "exists", "{got:?}");
}
