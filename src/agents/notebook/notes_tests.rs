//! Task 6 (phase 7a): `notes` lists the durable notebook newest first (N3; ambiguity 8).
use std::path::PathBuf;

use serde_json::{Value, json};

use super::tests::{add, notes, raw, tx, world};
use super::verb_tests::{argv, refuse_replay, run, team};
use super::*;
use crate::agents::tree::tests::{member_world, term};

fn listed(r: &Value) -> Vec<String> {
    r["notes"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .map(|n| n["title"].as_str().unwrap().to_string())
        .collect()
}
fn newest(from: usize, to: usize) -> Vec<String> {
    (from..to).rev().map(|n| format!("t{n}")).collect()
}

// N3: the certain prefix only, newest first, 20 by default; `-Lines`, `-Since`, `notes <id>`; ambiguity 8.
#[test]
fn notes_lists_the_certain_prefix_newest_first() {
    let (w, s) = team();
    let mut ids = Vec::new();
    for n in 0..25 {
        let Wrote::Added(e) = add(&w, &s, &format!("t{n}")).unwrap() else {
            panic!("added")
        };
        ids.push(e["id"].as_str().unwrap().to_string());
    }
    raw(
        &w,
        &s,
        b"{\"id\":\"ffffffffffffffff\",\"title\":\"past\"}\n",
    );
    let (code, r) = run(&w, &["notes"]);
    assert_eq!(
        (code, listed(&r), &r["older"]),
        (0, newest(5, 25), &json!(5)),
        "durable only: {r}"
    );
    let first =
        json!({"id": ids[24], "author": "person", "at": r["notes"][0]["at"], "title": "t24"});
    assert_eq!(r["notes"][0], first, "a row has no body");
    let (_, r) = run(&w, &["notes", "-Lines", "3"]);
    assert_eq!((listed(&r), &r["older"]), (newest(22, 25), &json!(22)));
    let (code, r) = run(&w, &["notes", "-Lines", "1000"]);
    assert_eq!(
        (code, listed(&r).len(), &r["older"]),
        (0, 25, &json!(0)),
        "-Lines 1000 is accepted"
    );
    let (_, r) = run(&w, &["notes", "-Since", ids[20].as_str()]);
    assert_eq!((listed(&r), &r["older"]), (newest(21, 25), &json!(0)));
    let (_, r) = run(&w, &["notes", "-Since", ids[20].as_str(), "-Lines", "2"]);
    assert_eq!((listed(&r), &r["older"]), (newest(23, 25), &json!(2)));
    let (code, r) = run(&w, &["notes", ids[3].as_str()]);
    assert_eq!(
        (code, &r["note"]["title"], &r["note"]["body"]),
        (0, &json!("t3"), &json!("body"))
    );
    for tokens in [
        vec!["notes", "ffffffffffffffff"],
        vec!["notes", "-Since", "ffffffffffffffff"],
    ] {
        let (code, r) = run(&w, &tokens);
        assert_eq!(
            (code, &r["check"], &r["message"]),
            (2, &json!("target"), &json!("no note with that id"))
        );
    }
    let bad: Vec<Vec<&str>> = vec![
        vec!["notes", ids[3].as_str(), "-Lines", "2"],
        vec!["notes", ids[3].as_str(), "-Since", ids[1].as_str()],
        vec!["notes", "-Lines", "0"],
        vec!["notes", "-Lines", "1001"],
        vec!["notes", "XYZ"],
    ];
    for tokens in bad {
        assert_eq!(run(&w, &tokens).1["check"], "usage", "{tokens:?}");
    }
    let (outside, _) = world();
    assert_eq!(run(&outside, &["notes"]).1["check"], "team");
}

// N3: listing moves no delivery cursor (7b's `cursors/`), and on a certain marker flushes and writes nothing.
#[test]
fn listing_writes_nothing() {
    let (w, s) = team();
    add(&w, &s, "a").unwrap();
    let at = notes(&w, &s);
    std::fs::create_dir(at.join("cursors")).unwrap();
    std::fs::write(
        at.join("cursors").join("person.json"),
        r#"{"delivered":[]}"#,
    )
    .unwrap();
    let before = snapshot(&at);
    let (codes, syncs) = crate::state::fs::record_syncs(|| {
        [vec!["notes"], vec!["notes", "-Lines", "1"]].map(|t| run(&w, &t).0)
    });
    assert_eq!(
        (codes, syncs),
        ([0, 0], Vec::<PathBuf>::new()),
        "a certain marker is not rewritten"
    );
    assert_eq!(snapshot(&at), before);
}

// N3, ambiguity 7: a member whose proof fails under the team lock reads nothing, by listing or by id.
#[test]
fn notes_refuses_an_unproven_reader_under_the_lock() {
    for tokens in [vec!["notes"], vec!["notes", "0123456789abcdef"]] {
        let mut m = member_world();
        m.hc();
        let t = term(&m.r, Some(150));
        m.rounds(1, &t);
        let mut later = t.clone();
        later["argv"] = json!(["/bin/sh", "-l"]);
        m.rounds(1, &later);
        let (def, request) = crate::cli::parse(&argv(&tokens)).unwrap();
        assert_eq!(
            (def.handler)(&m.w.ctx, &request).unwrap_err().check,
            "tagged",
            "{tokens:?}"
        );
        m.w.fakes.hc_done().unwrap();
    }
}

// PM call 6: replay runs before `notes` reads the notebook: a refusing hook answers, and nothing of it was read.
#[test]
fn replay_runs_before_notes_lists() {
    let (w, s) = team();
    add(&w, &s, "a").unwrap();
    crate::agents::team_state::test_hook(Some(refuse_replay as crate::agents::team_state::Hook));
    let ((code, r), reads) = crate::state::fs::record_reads(|| run(&w, &["notes"]));
    crate::agents::team_state::test_hook(None);
    assert_eq!((code, &r["check"]), (2, &json!("replay")), "{r}");
    let at = notes(&w, &s);
    assert!(
        !reads.iter().any(|p| p.starts_with(&at)),
        "read before replay: {reads:?}"
    );
}

// N3 (6-9 F4-F6): rows carry each saved entry's id, author name, `at` and title, a member's note included;
// `notes <id>` answers the whole stored entry; `-Text` prints the rows.
#[test]
fn notes_rows_match_the_saved_entries() {
    let (w, s) = team();
    let member = Author {
        name: "worker".into(),
        id: Some("aaaaaaaaaaaa".into()),
        key: "aaaaaaaaaaaa".into(),
    };
    w.clock("2026-11-16T10:00:00.0000000Z");
    let Wrote::Added(first) = append(&tx(&w, &s), &member, "from a member", "m", None).unwrap()
    else {
        panic!("added")
    };
    w.clock("2026-11-16T11:00:00.0000000Z");
    let Wrote::Added(second) = add(&w, &s, "from a person").unwrap() else {
        panic!("added")
    };
    let (code, r) = run(&w, &["notes"]);
    assert_eq!(code, 0, "{r}");
    let want = json!([
        {"id": second["id"], "author": "person", "at": "2026-11-16T11:00:00.0000000Z", "title": "from a person"},
        {"id": first["id"], "author": "worker", "at": "2026-11-16T10:00:00.0000000Z", "title": "from a member"},
    ]);
    assert_eq!(r["notes"], want, "the rows");
    let stored = entries(&tx(&w, &s)).unwrap();
    let (_, one) = run(&w, &["notes", first["id"].as_str().unwrap()]);
    assert_eq!(one["note"], stored[0], "the whole stored entry");
    let (code, text) = crate::cli::run(&argv(&["notes", "-Text"]), || Ok(&w.ctx));
    assert_eq!(code, 0, "{text}");
    let row = format!(
        "{}  person  2026-11-16T11:00:00.0000000Z  from a person",
        second["id"].as_str().unwrap()
    );
    assert!(text.lines().any(|l| l == row), "the -Text row: {text}");
}

type Files = Vec<(PathBuf, Vec<u8>)>;
// Every file under `dir` with its bytes, in name order.
fn snapshot(dir: &Path) -> Files {
    let mut out = Files::new();
    let mut all: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    all.sort();
    for p in all {
        if p.is_dir() {
            out.extend(snapshot(&p));
        } else {
            out.push((p.clone(), std::fs::read(&p).unwrap()));
        }
    }
    out
}
