use std::path::PathBuf;

use serde_json::{Value, json};

use super::*;
use crate::agents::fixture::World;
use crate::agents::team_state::{Scope, Tx};

// A person in session $3; `w.state()` makes `<root>/<key>` (with `agents`, `mail`), as every team has.
pub(crate) fn world() -> (World, Scope) {
    let w = World::human("");
    w.state();
    let s = Scope::of(&w.caller());
    (w, s)
}
// The team lock's Tx; tests call the store as the verbs do, after the lock.
pub(crate) fn tx<'a>(w: &'a World, s: &Scope) -> Tx<'a> {
    Tx {
        ctx: &w.ctx,
        scope: s.clone(),
    }
}
fn person() -> Author {
    Author {
        name: "person".into(),
        id: None,
        key: "person".into(),
    }
}
fn add(w: &World, s: &Scope, title: &str) -> Res<Wrote> {
    append(&tx(w, s), &person(), title, "body", None)
}
fn titles(w: &World, s: &Scope) -> Vec<String> {
    entries(&tx(w, s))
        .unwrap()
        .iter()
        .map(|e| e["title"].as_str().unwrap().to_string())
        .collect()
}
fn notes(w: &World, s: &Scope) -> PathBuf {
    dir(&w.ctx.state_root, s)
}
fn state(w: &World, s: &Scope) -> Value {
    crate::state::fs::read_json(&notes(w, s).join(STATE))
        .unwrap()
        .unwrap()
}
// The bytes a process killed after its write and before its publish leaves.
fn raw(w: &World, s: &Scope, bytes: &[u8]) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(notes(w, s).join(LOG))
        .unwrap();
    f.write_all(bytes).unwrap();
}

#[test]
fn an_append_is_one_whole_line_and_a_certain_marker() {
    let (w, s) = world();
    w.clock("2026-11-16T10:00:00.0000000Z");
    let Wrote::Added(e) = add(&w, &s, "plan").unwrap() else {
        panic!("added")
    };
    let log = std::fs::read_to_string(notes(&w, &s).join(LOG)).unwrap();
    assert_eq!(log, format!("{e}\n"));
    assert_eq!(e["id"].as_str().unwrap().len(), 16);
    let author = json!({"name": "person", "id": null, "key": "person"});
    assert_eq!(
        (&e["author"], &e["title"], &e["body"]),
        (&author, &json!("plan"), &json!("body"))
    );
    assert_eq!(
        e["at"],
        json!("2026-11-16T10:00:00.0000000Z"),
        "the fixture clock"
    );
    assert_eq!(
        state(&w, &s),
        json!({"durable": log.len(), "uncertain": false})
    );
    add(&w, &s, "two").unwrap();
    assert_eq!(titles(&w, &s), ["plan", "two"]);
}

#[test]
fn a_crash_before_the_publish_leaves_the_line_invisible_and_the_next_append_cuts_it() {
    let mut long = json!({"id": "ffffffffffffffff", "title": "x".repeat(400)})
        .to_string()
        .into_bytes();
    long.push(10); // a tail longer than the replacement line: only set_len removes all of it
    for tail in [
        &b"{\"id\":\"ffffffffffffffff\",\"title\":\"lost\"}\n"[..],
        &b"{\"id\":\"ff"[..],
        long.as_slice(),
    ] {
        let (w, s) = world();
        add(&w, &s, "kept").unwrap();
        let before = state(&w, &s);
        raw(&w, &s, tail);
        assert_eq!(
            titles(&w, &s),
            ["kept"],
            "a line past durable is never read"
        );
        assert_eq!(state(&w, &s), before, "a reader changes nothing");
        add(&w, &s, "next").unwrap();
        assert_eq!(titles(&w, &s), ["kept", "next"]);
        let log = std::fs::read_to_string(notes(&w, &s).join(LOG)).unwrap();
        assert!(!log.contains("ffff"), "the unpublished tail is cut: {log}");
        assert_eq!(log.lines().count(), 2);
    }
}

#[test]
fn a_reader_of_no_notebook_sees_none_and_makes_nothing() {
    let (w, s) = world();
    assert_eq!(entries(&tx(&w, &s)).unwrap(), Vec::<Value>::new());
    assert!(!w.ctx.state_root.join(&s.key).join("teams").exists());
}

#[test]
fn a_link_or_a_file_at_any_level_refuses_evidence() {
    // Levels from the state root: 0 `<key>`, 1 `teams`, 2 `<sha16>`, 3 `notes` (call 8).
    for level in 0..4 {
        for kind in ["link", "file"] {
            let (w, s) = world();
            let at: Vec<PathBuf> = notes(&w, &s)
                .ancestors()
                .take(4)
                .map(Path::to_path_buf)
                .collect();
            let target = &at[3 - level];
            if level == 0 {
                std::fs::remove_dir_all(target).unwrap();
            } else {
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            }
            let elsewhere = w.ctx.state_root.join("elsewhere");
            std::fs::create_dir_all(&elsewhere).unwrap();
            match kind {
                "link" => crate::testing::link_dir(&elsewhere, target),
                _ => std::fs::write(target, "x").unwrap(),
            }
            let tag = format!("{kind} at level {level}");
            assert_eq!(
                entries(&tx(&w, &s)).unwrap_err().check,
                "evidence",
                "{tag}: reader"
            );
            assert_eq!(
                add(&w, &s, "t").unwrap_err().check,
                "evidence",
                "{tag}: writer"
            );
            assert_eq!(
                std::fs::read_dir(&elsewhere).unwrap().count(),
                0,
                "{tag}: nothing made through it"
            );
        }
    }
}

// R5 (call 8): a link at a leaf, even to a regular file, is never read, written or replaced.
#[test]
fn a_file_link_at_the_log_or_the_marker_refuses_evidence() {
    for leaf in [LOG, STATE] {
        let (w, s) = world();
        add(&w, &s, "kept").unwrap();
        let at = notes(&w, &s).join(leaf);
        let outside = w.ctx.state_root.join("outside");
        std::fs::rename(&at, &outside).unwrap();
        file_link(&outside, &at);
        let before = std::fs::read(&outside).unwrap();
        assert_eq!(
            entries(&tx(&w, &s)).unwrap_err().check,
            "evidence",
            "{leaf}: reader"
        );
        assert_eq!(
            add(&w, &s, "t").unwrap_err().check,
            "evidence",
            "{leaf}: writer"
        );
        assert_eq!(
            std::fs::read(&outside).unwrap(),
            before,
            "{leaf}: the target is untouched"
        );
        assert!(
            std::fs::symlink_metadata(&at)
                .unwrap()
                .file_type()
                .is_symlink(),
            "{leaf}: never replaced"
        );
    }
}

// A file symlink; Windows `symlink_file` needs the right `link_dir` also tries first (CI's runners have it).
fn file_link(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(target, link).unwrap();
}

#[test]
fn a_folder_at_the_log_or_the_state_refuses_evidence() {
    for leaf in [LOG, STATE] {
        let (w, s) = world();
        add(&w, &s, "kept").unwrap();
        let at = notes(&w, &s).join(leaf);
        std::fs::remove_file(&at).unwrap();
        std::fs::create_dir(&at).unwrap();
        assert_eq!(
            add(&w, &s, "t").unwrap_err().check,
            "evidence",
            "{leaf}: writer"
        );
        assert_eq!(
            entries(&tx(&w, &s)).unwrap_err().check,
            "evidence",
            "{leaf}: reader"
        );
        assert!(at.is_dir(), "{leaf}: never renamed over");
    }
}

// 6a E0: the notebook is the scope's, whichever session's caller reaches it.
#[test]
fn the_notebook_is_the_anchor_scopes_whatever_session_reaches_it() {
    let (w, home) = world();
    add(&w, &home, "kept").unwrap();
    let mut moved = w.caller();
    moved.session = "$9".into();
    assert_eq!(
        titles(&w, &Scope::of(&moved)),
        Vec::<String>::new(),
        "another session is another team"
    );
    assert_eq!(
        titles(&w, &home),
        ["kept"],
        "the anchor scope reaches the same notebook"
    );
    assert_eq!(
        super::anchor(&moved),
        Scope::of(&moved),
        "until 6a routes it"
    );
}
