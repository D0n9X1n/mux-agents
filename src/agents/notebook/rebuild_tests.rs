//! Task 4 (#103 phase 7a): an unreadable or missing marker is rebuilt from the last whole line; R1 single read.
use serde_json::json;

use super::fault_tests::id_of;
use super::tests::{add, notes, person, raw, state, titles, tx, world};
use super::*;

// N2: torn, not an object, not at a line end, past the log: rebuilt under the team lock from the last whole line;
// no flushed entry is lost and the torn tail stays invisible until the next append cuts it.
#[test]
fn an_unreadable_marker_is_rebuilt_from_the_last_whole_line() {
    let bad = [
        "{",
        "[]",
        r#"{"durable":1,"uncertain":false}"#,
        r#"{"durable":99999,"uncertain":false}"#,
        r#"{"durable":0}"#,
        r#"{"durable":0,"uncertain":"no"}"#,
        r#"{"uncertain":false}"#,
        r#"{"durable":"5","uncertain":false}"#,
    ];
    for bad in bad {
        let (w, s) = world();
        add(&w, &s, "a").unwrap();
        add(&w, &s, "b").unwrap();
        raw(&w, &s, b"{\"id\":\"ff");
        std::fs::write(notes(&w, &s).join(STATE), bad).unwrap();
        let log = std::fs::read_to_string(notes(&w, &s).join(LOG)).unwrap();
        let whole = log.rfind('\n').unwrap() + 1;
        assert_eq!(titles(&w, &s), ["a", "b"], "{bad}: no flushed entry lost");
        assert_eq!(
            state(&w, &s),
            json!({"durable": whole, "uncertain": false}),
            "{bad}"
        );
        add(&w, &s, "c").unwrap();
        assert_eq!(titles(&w, &s), ["a", "b", "c"], "{bad}");
    }
}

// N2: the rebuild flushes the log and the folder before it publishes; a failure refuses and publishes nothing.
#[test]
fn a_rebuild_whose_flush_fails_refuses() {
    for case in ["file", "folder"] {
        let (w, s) = world();
        add(&w, &s, "a").unwrap();
        let at = notes(&w, &s);
        std::fs::write(at.join(STATE), "{").unwrap();
        match case {
            "file" => crate::state::fs::fail_sync_file(Some(&at.join(LOG))),
            _ => crate::state::fs::fail_sync_dir(Some(&at)),
        }
        let r = entries(&tx(&w, &s)).unwrap_err();
        crate::state::fs::fail_sync_file(None);
        crate::state::fs::fail_sync_dir(None);
        let got = (r.code, r.check, r.message.as_str());
        assert_eq!(
            got,
            (1, "notes", "the notebook is not yet on disk; retry"),
            "{case}"
        );
        assert_eq!(
            std::fs::read(at.join(STATE)).unwrap(),
            b"{",
            "{case}: nothing published"
        );
        assert_eq!(
            titles(&w, &s),
            ["a"],
            "{case}: rebuilt once the flush works"
        );
    }
}

// R4: a writer flushes every level below `<key>` on every call, so a retry finishes an earlier failed flush.
#[test]
fn a_writer_flushes_every_level_on_every_call() {
    let (w, s) = world();
    add(&w, &s, "a").unwrap();
    let at = notes(&w, &s);
    let ((), syncs) = crate::state::fs::record_syncs(|| {
        add(&w, &s, "b").unwrap();
    });
    for level in at.ancestors().take(4).collect::<Vec<_>>().into_iter().rev() {
        assert!(
            syncs.iter().any(|p| p == level),
            "{level:?} not flushed: {syncs:?}"
        );
    }
}

// Call 3: a missing marker beside a log with content is unreadable and rebuilt from the last whole line, never
// durable 0, so nothing is cut; only a missing marker beside an empty or absent log is durable 0.
#[test]
fn a_missing_marker_beside_entries_is_rebuilt_and_cuts_nothing() {
    let (w, s) = world();
    add(&w, &s, "a").unwrap();
    add(&w, &s, "b").unwrap();
    std::fs::remove_file(notes(&w, &s).join(STATE)).unwrap();
    add(&w, &s, "c").unwrap();
    assert_eq!(titles(&w, &s), ["a", "b", "c"], "the next note keeps both");
    let (e, es) = world();
    std::fs::create_dir_all(notes(&e, &es)).unwrap();
    std::fs::write(notes(&e, &es).join(LOG), "").unwrap();
    add(&e, &es, "first").unwrap();
    assert_eq!(
        titles(&e, &es),
        ["first"],
        "an empty log with no marker is durable 0"
    );
}

// N2: every failure point of the rebuild's publish: the uncertain temp file (nothing published), the uncertain
// folder flush (renamed uncertain), the completion's own folder flush (checklist A3: renamed uncertain, never
// completed), the certain rewrite's folder flush (renamed certain). Each refuses 1 `notes`; the next reader
// completes it and no entry is lost.
#[test]
fn every_failure_point_of_the_rebuild_refuses_and_recovers() {
    for case in [
        "uncertain-temp",
        "uncertain-folder",
        "completion-folder",
        "certain-folder",
    ] {
        let (w, s) = world();
        add(&w, &s, "a").unwrap();
        let at = notes(&w, &s);
        std::fs::write(at.join(STATE), "{").unwrap();
        let whole = std::fs::metadata(at.join(LOG)).unwrap().len();
        match case {
            "uncertain-temp" => crate::state::fs::fail_sync_file(Some(&at.join(STATE))),
            "uncertain-folder" => crate::state::fs::fail_durable_flush(Some(0)),
            // The rebuild's folder flushes: its own, the uncertain publish's, then the completion's (the third).
            "completion-folder" => crate::state::fs::fail_sync_dir_nth(Some(&at), 2),
            _ => crate::state::fs::fail_durable_flush(Some(1)),
        }
        let r = entries(&tx(&w, &s)).unwrap_err();
        crate::state::fs::fail_sync_file(None);
        crate::state::fs::fail_durable_flush(None);
        crate::state::fs::fail_sync_dir_nth(None, 0);
        let got = (r.code, r.check, r.message.as_str());
        assert_eq!(
            got,
            (1, "notes", "the notebook is not yet on disk; retry"),
            "{case}"
        );
        let want = match case {
            "uncertain-temp" => b"{".to_vec(),
            "uncertain-folder" | "completion-folder" => {
                json!({"durable": whole, "uncertain": true})
                    .to_string()
                    .into_bytes()
            }
            _ => json!({"durable": whole, "uncertain": false})
                .to_string()
                .into_bytes(),
        };
        assert_eq!(
            std::fs::read(at.join(STATE)).unwrap(),
            want,
            "{case}: the marker the failure leaves"
        );
        assert_eq!(titles(&w, &s), ["a"], "{case}");
    }
}

// R1 (PM call 13): each operation reads the log once and the marker once, counted at the filesystem
// (`state::fs::record_reads`); completion and rebuild read nothing again.
#[test]
fn each_operation_reads_the_log_and_the_marker_once() {
    let (w, s) = world();
    add(&w, &s, "a").unwrap();
    let at = notes(&w, &s);
    let once = |what: &str, f: &dyn Fn()| {
        let ((), reads) = crate::state::fs::record_reads(f);
        let count = |name: &str| reads.iter().filter(|p| **p == at.join(name)).count();
        assert_eq!((count(LOG), count(STATE)), (1, 1), "{what}: {reads:?}");
    };
    once("append", &|| {
        add(&w, &s, "b").unwrap();
    });
    once("entries", &|| {
        titles(&w, &s);
    });
    crate::state::fs::fail_durable_flush(Some(0));
    let _ = add(&w, &s, "c").unwrap_err();
    crate::state::fs::fail_durable_flush(None);
    once("completion", &|| {
        titles(&w, &s);
    });
    std::fs::write(notes(&w, &s).join(STATE), "{").unwrap();
    once("rebuild", &|| {
        titles(&w, &s);
    });
}

// N2 (F2): a stray, invalid marker temp beside the marker is never read; the marker in force decides.
// Cases: an append's publish, a certain rewrite, a rebuild.
#[test]
fn a_stray_marker_temp_is_never_read() {
    for case in ["publish", "certain", "rebuild"] {
        let (w, s) = world();
        add(&w, &s, "kept").unwrap();
        let at = notes(&w, &s);
        let kept = std::fs::metadata(at.join(LOG)).unwrap().len();
        let marker = match case {
            "publish" => {
                raw(&w, &s, b"{\"id\":\"ffffffffffffffff\",\"title\":\"new\"}\n");
                json!({"durable": kept, "uncertain": false}).to_string()
            }
            "certain" => json!({"durable": kept, "uncertain": true}).to_string(),
            _ => "{".to_string(),
        };
        std::fs::write(at.join(STATE), &marker).unwrap();
        let stray = json!({"durable": 1, "uncertain": false}).to_string();
        std::fs::write(
            at.join(format!("{STATE}.0123456789abcdef0123456789abcdef.tmp")),
            stray,
        )
        .unwrap();
        assert_eq!(titles(&w, &s), ["kept"], "{case}: the marker in force");
        assert_eq!(
            state(&w, &s),
            json!({"durable": kept, "uncertain": false}),
            "{case}"
        );
        add(&w, &s, "next").unwrap();
        assert_eq!(
            titles(&w, &s),
            ["kept", "next"],
            "{case}: nothing answered is lost"
        );
    }
}

// Checklist A4: a crash after a marker temp's flush and before its rename leaves the real pending marker of that
// checkpoint beside the one in force: an append's uncertain publish, a completion's certain rewrite, a rebuild's
// uncertain publish. The reader answers only the certain prefix of the marker in force and never reads a temp; an
// `-Id` retry adds once, then answers a duplicate.
#[test]
fn a_pending_marker_temp_is_never_adopted() {
    for case in ["append", "certain", "rebuild"] {
        let (w, s) = world();
        add(&w, &s, "kept").unwrap();
        let at = notes(&w, &s);
        let kept = std::fs::metadata(at.join(LOG)).unwrap().len();
        let fixed = "0123456789abcdef".to_string();
        // (the pending marker, the retry's title and id, what the reader lists before the retry)
        let (pending, title, id, before): (Value, &str, String, Vec<&str>) = match case {
            "append" => {
                // The killed append's flushed line, then its uncertain publish's temp.
                let line = json!({"id": fixed, "author": {"name": "person", "id": null, "key": "person"},
                    "at": "1970-01-01T00:00:00.0000000Z", "title": "new", "body": "body"});
                raw(&w, &s, format!("{line}\n").as_bytes());
                let end = std::fs::metadata(at.join(LOG)).unwrap().len();
                (
                    json!({"durable": end, "uncertain": true}),
                    "new",
                    fixed,
                    vec!["kept"],
                )
            }
            "certain" => {
                // The uncertain publish renamed; the completion's certain rewrite died before its rename.
                crate::state::fs::fail_durable_flush(Some(0));
                let e = add(&w, &s, "new").unwrap_err();
                crate::state::fs::fail_durable_flush(None);
                let end = std::fs::metadata(at.join(LOG)).unwrap().len();
                (
                    json!({"durable": end, "uncertain": false}),
                    "new",
                    id_of(&e),
                    vec!["kept", "new"],
                )
            }
            _ => {
                // A torn marker beside a torn tail, then the rebuild's uncertain publish temp.
                raw(&w, &s, b"{\"id\":\"ff");
                std::fs::write(at.join(STATE), "{").unwrap();
                (
                    json!({"durable": kept, "uncertain": true}),
                    "next",
                    fixed,
                    vec!["kept"],
                )
            }
        };
        let temp = at.join(format!("{STATE}.0123456789abcdef0123456789abcdef.tmp"));
        std::fs::write(&temp, pending.to_string()).unwrap();
        if case == "certain" {
            // The marker in force is uncertain: while its folder flush fails, nothing is answered.
            crate::state::fs::fail_sync_dir(Some(&at));
            let r = entries(&tx(&w, &s)).unwrap_err();
            crate::state::fs::fail_sync_dir(None);
            assert_eq!(
                (r.code, r.check),
                (1, "notes"),
                "{case}: the marker in force decides"
            );
        }
        let (listed, reads) = crate::state::fs::record_reads(|| titles(&w, &s));
        assert_eq!(
            listed, before,
            "{case}: the certain prefix of the marker in force"
        );
        assert!(!reads.contains(&temp), "{case}: a temp was read: {reads:?}");
        let retry = || append(&tx(&w, &s), &person(), title, "body", Some(&id)).unwrap();
        let first = retry();
        assert_eq!(
            matches!(first, Wrote::Added(_)),
            case != "certain",
            "{case}: the retry adds once: {first:?}"
        );
        assert!(
            matches!(retry(), Wrote::Found(_)),
            "{case}: then answers a duplicate"
        );
        let mut after = before.clone();
        if case != "certain" {
            after.push(title);
        }
        assert_eq!(titles(&w, &s), after, "{case}: once, nothing lost");
    }
}

// R4 (F10): the rebuild flushes the log before its folder: a failing log flush leaves the folder unflushed.
#[test]
fn the_rebuild_flushes_the_log_before_its_folder() {
    let (w, s) = world();
    add(&w, &s, "a").unwrap();
    let at = notes(&w, &s);
    std::fs::write(at.join(STATE), "{").unwrap();
    crate::state::fs::fail_sync_file(Some(&at.join(LOG)));
    let (r, syncs) = crate::state::fs::record_syncs(|| entries(&tx(&w, &s)).unwrap_err());
    crate::state::fs::fail_sync_file(None);
    assert_eq!(r.check, "notes");
    assert!(
        !syncs.contains(&at),
        "the folder flushed before the log: {syncs:?}"
    );
}
