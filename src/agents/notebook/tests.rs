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
pub(crate) fn person() -> Author {
    Author {
        name: "person".into(),
        id: None,
        key: "person".into(),
    }
}
pub(crate) fn add(w: &World, s: &Scope, title: &str) -> Res<Wrote> {
    append(&tx(w, s), &person(), title, "body", None)
}
pub(crate) fn titles(w: &World, s: &Scope) -> Vec<String> {
    entries(&tx(w, s))
        .unwrap()
        .iter()
        .map(|e| e["title"].as_str().unwrap().to_string())
        .collect()
}
pub(crate) fn notes(w: &World, s: &Scope) -> PathBuf {
    dir(&w.ctx.state_root, s)
}
pub(crate) fn state(w: &World, s: &Scope) -> Value {
    crate::state::fs::read_json(&notes(w, s).join(STATE))
        .unwrap()
        .unwrap()
}
// The bytes a process killed after its write and before its publish leaves.
pub(crate) fn raw(w: &World, s: &Scope, bytes: &[u8]) {
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

// Ambiguity 2 (F13): a drawn id the certain prefix already holds is drawn again.
#[test]
fn a_drawn_id_already_in_the_prefix_is_drawn_again() {
    let known = vec![json!({"id": "aaaaaaaaaaaaaaaa"})];
    let mut draws = vec!["bbbbbbbbbbbbbbbb", "aaaaaaaaaaaaaaaa"];
    let got = fresh_id(&known, || Ok(draws.pop().unwrap().to_string())).unwrap();
    assert_eq!((got.as_str(), draws.len()), ("bbbbbbbbbbbbbbbb", 0));
}

// R4 (F9): the line is flushed before its folder: a failing file flush leaves the notes folder unflushed.
#[test]
fn the_line_is_flushed_before_its_folder() {
    let (w, s) = world();
    add(&w, &s, "kept").unwrap();
    let at = notes(&w, &s);
    crate::state::fs::fail_sync_file(Some(&at.join(LOG)));
    let (e, syncs) = crate::state::fs::record_syncs(|| add(&w, &s, "next").unwrap_err());
    crate::state::fs::fail_sync_file(None);
    assert_eq!(e.code, 1, "{e:?}");
    assert!(
        !syncs.contains(&at),
        "the folder flushed before the file: {syncs:?}"
    );
}

// R4 (F3): a failed flush of any folder above `notes` (`<sha16>`, `teams`, `<key>`) refuses and writes nothing.
#[test]
fn a_failed_parent_flush_refuses_and_writes_nothing() {
    for level in 1..4 {
        let (w, s) = world();
        add(&w, &s, "kept").unwrap();
        let at = notes(&w, &s);
        let parent = at.ancestors().nth(level).unwrap().to_path_buf();
        let log = std::fs::read(at.join(LOG)).unwrap();
        crate::state::fs::fail_sync_dir(Some(&parent));
        let e = add(&w, &s, "next").unwrap_err();
        crate::state::fs::fail_sync_dir(None);
        let got = (e.code, e.check, e.message.as_str());
        assert_eq!(
            got,
            (1, "notes", "the notebook is not yet on disk; retry"),
            "{parent:?}"
        );
        assert_eq!(
            std::fs::read(at.join(LOG)).unwrap(),
            log,
            "{parent:?}: nothing written"
        );
    }
}

// PM call 14 (F5): a real junction at each level and at both leaves refuses `evidence` (Windows).
#[cfg(windows)]
#[test]
fn a_junction_at_any_level_or_leaf_refuses_evidence() {
    for case in 0..6 {
        let (w, s) = world();
        add(&w, &s, "kept").unwrap();
        let at = notes(&w, &s);
        let target = match case {
            4 => at.join(LOG),
            5 => at.join(STATE),
            n => at.ancestors().nth(3 - n).unwrap().to_path_buf(),
        };
        std::fs::rename(&target, w.ctx.state_root.join("aside")).unwrap();
        let elsewhere = w.ctx.state_root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        crate::testing::junction(&elsewhere, &target);
        assert_eq!(
            entries(&tx(&w, &s)).unwrap_err().check,
            "evidence",
            "case {case}: reader"
        );
        assert_eq!(
            add(&w, &s, "t").unwrap_err().check,
            "evidence",
            "case {case}: writer"
        );
        assert_eq!(
            std::fs::read_dir(&elsewhere).unwrap().count(),
            0,
            "case {case}: nothing made through it"
        );
    }
}

// PM call 14 (F6): a FIFO at either leaf refuses `evidence` without opening it. A helper opens the FIFO both ways,
// non-blocking, until the calls return: a write-open succeeds only while a reader is blocked on it (`freed`), and a
// read-open releases a writer blocked in its open, so a weakened check fails here instead of hanging.
#[cfg(unix)]
#[test]
fn a_fifo_at_a_leaf_refuses_without_blocking() {
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    for leaf in [LOG, STATE] {
        let (w, s) = world();
        add(&w, &s, "kept").unwrap();
        let at = notes(&w, &s).join(leaf);
        std::fs::remove_file(&at).unwrap();
        let c = std::ffi::CString::new(at.to_str().unwrap()).unwrap();
        // SAFETY: `c` is a NUL-terminated path that outlives the call.
        assert_eq!(
            unsafe { libc::mkfifo(c.as_ptr(), 0o600) },
            0,
            "{leaf}: mkfifo"
        );
        let (done, freed) = (AtomicBool::new(false), AtomicBool::new(false));
        let (reader, writer) = std::thread::scope(|scope| {
            scope.spawn(|| {
                while !done.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    let open = |write: bool| {
                        let mut o = std::fs::OpenOptions::new();
                        o.read(!write)
                            .write(write)
                            .custom_flags(libc::O_NONBLOCK)
                            .open(&at)
                    };
                    if open(true).is_ok() {
                        freed.store(true, Ordering::SeqCst);
                    }
                    let _ = open(false);
                }
            });
            let reader = entries(&tx(&w, &s)).map(|_| ()).map_err(|e| e.check);
            let writer = add(&w, &s, "t").map(|_| ()).map_err(|e| e.check);
            done.store(true, Ordering::SeqCst);
            (reader, writer)
        });
        assert!(
            !freed.load(Ordering::SeqCst),
            "{leaf}: an open blocked on the FIFO"
        );
        assert_eq!(
            (reader, writer),
            (Err("evidence"), Err("evidence")),
            "{leaf}"
        );
    }
}
