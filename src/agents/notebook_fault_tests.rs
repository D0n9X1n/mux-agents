//! Tasks 2-3 (#103 phase 7a): a failed append names its id; flush faults before and after the publish; `-Id`.
use serde_json::json;

use super::tests::{add, notes, person, raw, state, titles, tx, world};
use super::*;

pub(crate) fn id_of(e: &Refusal) -> String {
    e.details["id"].as_str().unwrap().to_string()
}

// N2 (1)-(3): a file or folder flush failing before the state rename: refused with the id, invisible, marker
// unchanged; the `-Id` retry adds it exactly once, and a second retry adds nothing.
#[test]
fn a_flush_failure_before_the_publish_refuses_with_the_id_and_changes_nothing() {
    for case in ["file", "folder"] {
        let (w, s) = world();
        add(&w, &s, "kept").unwrap();
        let before = state(&w, &s);
        let at = notes(&w, &s);
        match case {
            "file" => crate::state::fs::fail_sync_file(Some(&at.join(LOG))),
            _ => crate::state::fs::fail_sync_dir(Some(&at)),
        }
        let e = add(&w, &s, "lost").unwrap_err();
        crate::state::fs::fail_sync_file(None);
        crate::state::fs::fail_sync_dir(None);
        assert_eq!((e.code, e.check), (1, "notes"), "{case}");
        let id = id_of(&e);
        assert_eq!(
            e.message,
            format!("the note could not be saved; retry with -Id {id}")
        );
        assert_eq!(state(&w, &s), before, "{case}: marker unchanged");
        let written = std::fs::metadata(at.join(LOG)).unwrap().len();
        assert!(
            written > before["durable"].as_u64().unwrap(),
            "{case}: written before the failing flush"
        );
        assert_eq!(titles(&w, &s), ["kept"], "{case}: invisible");
        let retry = || append(&tx(&w, &s), &person(), "lost", "body", Some(&id)).unwrap();
        assert!(
            matches!(retry(), Wrote::Added(_)),
            "{case}: the retry saves it"
        );
        assert!(
            matches!(retry(), Wrote::Found(_)),
            "{case}: a second retry finds it"
        );
        assert_eq!(titles(&w, &s), ["kept", "lost"], "{case}: exactly once");
    }
}

// Ambiguity 5: `-Id` answers only the same author's same note; anything else under that id is refused.
#[test]
fn an_id_answers_only_its_own_note() {
    let (w, s) = world();
    let t = tx(&w, &s);
    let id = "0123456789abcdef";
    let Wrote::Added(e) = append(&t, &person(), "plan", "body", Some(id)).unwrap() else {
        panic!("added")
    };
    assert_eq!(
        append(&t, &person(), "plan", "body", Some(id)).unwrap(),
        Wrote::Found(e)
    );
    let member = Author {
        name: "worker".into(),
        id: Some("aaaaaaaaaaaa".into()),
        key: "aaaaaaaaaaaa".into(),
    };
    for (who, title, body) in [
        (&member, "plan", "body"),
        (&person(), "plan2", "body"),
        (&person(), "plan", "x"),
    ] {
        let r = append(&t, who, title, body, Some(id)).unwrap_err();
        assert_eq!(
            (r.code, r.check, r.message.as_str()),
            (2, "id", "that id belongs to a different note"),
            "{title} {body}"
        );
    }
    assert_eq!(titles(&w, &s), ["plan"]);
}

// N2 (1): the cut is flushed before the replacement is written: a flush failing right after the cut leaves the log
// at `durable` with no new byte; the `-Id` retry saves the note once.
#[test]
fn the_cut_is_flushed_before_the_replacement_is_written() {
    let (w, s) = world();
    add(&w, &s, "kept").unwrap();
    raw(&w, &s, b"{\"id\":\"ff");
    let durable = state(&w, &s)["durable"].as_u64().unwrap();
    crate::state::fs::fail_sync_file(Some(&notes(&w, &s).join(LOG)));
    let e = add(&w, &s, "next").unwrap_err();
    crate::state::fs::fail_sync_file(None);
    assert_eq!((e.code, e.check), (1, "notes"));
    assert_eq!(
        std::fs::metadata(notes(&w, &s).join(LOG)).unwrap().len(),
        durable,
        "cut, nothing written"
    );
    let id = id_of(&e);
    assert!(matches!(
        append(&tx(&w, &s), &person(), "next", "body", Some(&id)).unwrap(),
        Wrote::Added(_)
    ));
    assert_eq!(titles(&w, &s), ["kept", "next"]);
}

// Ambiguity 5: retry ownership is the logical key: a resumed successor (another id, the same mailbox key) gets its
// predecessor's note back for that `-Id`.
#[test]
fn a_successor_retries_by_its_logical_key() {
    let (w, s) = world();
    let key = "aaaaaaaaaaaa".to_string();
    let first = Author {
        name: "worker".into(),
        id: Some(key.clone()),
        key: key.clone(),
    };
    let next = Author {
        name: "worker".into(),
        id: Some("bbbbbbbbbbbb".into()),
        key,
    };
    let id = "0123456789abcdef";
    append(&tx(&w, &s), &first, "plan", "body", Some(id)).unwrap();
    let r = append(&tx(&w, &s), &next, "plan", "body", Some(id)).unwrap();
    assert!(matches!(r, Wrote::Found(_)), "{r:?}");
    assert_eq!(titles(&w, &s), ["plan"]);
}

// v5: the uncertain publish renames, then its folder flush fails. While the flush keeps failing, a reader and the
// `-Id` retry refuse and change nothing; once it works, each flushes the notes folder before it answers.
#[test]
fn a_fault_after_the_state_rename_refuses_and_the_next_reader_completes_first() {
    for retry in ["reader", "id"] {
        let (w, s) = world();
        add(&w, &s, "kept").unwrap();
        let at = notes(&w, &s);
        // The append's first write_json_durable is its publish: it renames, then its folder flush fails.
        crate::state::fs::fail_durable_flush(Some(0));
        let e = add(&w, &s, "new").unwrap_err();
        crate::state::fs::fail_durable_flush(None);
        let id = id_of(&e);
        assert_eq!((e.code, e.check), (1, "notes"), "{retry}");
        let end = std::fs::metadata(at.join(LOG)).unwrap().len();
        assert_eq!(
            state(&w, &s),
            json!({"durable": end, "uncertain": true}),
            "{retry}: renamed, not certain"
        );
        crate::state::fs::fail_sync_dir(Some(&at));
        let r = entries(&tx(&w, &s)).unwrap_err();
        assert_eq!(
            (r.code, r.check, r.message.as_str()),
            (1, "notes", "the notebook is not yet on disk; retry")
        );
        let r = append(&tx(&w, &s), &person(), "new", "body", Some(&id)).unwrap_err();
        assert_eq!(
            (r.code, r.check),
            (1, "notes"),
            "{retry}: the retry refuses too"
        );
        crate::state::fs::fail_sync_dir(None);
        assert_eq!(
            state(&w, &s),
            json!({"durable": end, "uncertain": true}),
            "{retry}: nothing changed"
        );
        let (got, syncs) = crate::state::fs::record_syncs(|| {
            if retry == "id" {
                let r = append(&tx(&w, &s), &person(), "new", "body", Some(&id)).unwrap();
                assert!(matches!(r, Wrote::Found(_)), "answered, never added twice");
            }
            titles(&w, &s)
        });
        assert_eq!(got, ["kept", "new"], "{retry}");
        // complete: the folder flush, then the certain rewrite's own folder flush.
        assert_eq!(
            syncs.iter().filter(|p| **p == at).count(),
            2,
            "{retry}: {syncs:?}"
        );
        assert_eq!(
            state(&w, &s),
            json!({"durable": end, "uncertain": false}),
            "{retry}"
        );
    }
}

// v5 rollback: the uncertain publish's flush fails, the process dies and the old marker returns (the rename never
// reached the disk). Nothing answered or listed is lost; the unanswered line is cut by the next append.
#[test]
fn a_rolled_back_marker_loses_only_what_no_one_received() {
    let (w, s) = world();
    add(&w, &s, "a").unwrap();
    let listed = titles(&w, &s);
    let old = std::fs::read(notes(&w, &s).join(STATE)).unwrap();
    crate::state::fs::fail_durable_flush(Some(0));
    let e = add(&w, &s, "b").unwrap_err();
    crate::state::fs::fail_durable_flush(None);
    assert_eq!(e.check, "notes", "b was refused, never answered");
    std::fs::write(notes(&w, &s).join(STATE), &old).unwrap();
    assert_eq!(
        titles(&w, &s),
        listed,
        "the returned marker lists only what was listed"
    );
    add(&w, &s, "c").unwrap();
    assert_eq!(titles(&w, &s), ["a", "c"], "every listed entry survives");
    let log = std::fs::read_to_string(notes(&w, &s).join(LOG)).unwrap();
    assert!(
        !log.contains(r#""title":"b""#),
        "the unanswered line is cut: {log}"
    );
    let r = append(&tx(&w, &s), &person(), "b", "body", Some(&id_of(&e))).unwrap();
    assert!(matches!(r, Wrote::Added(_)), "its -Id retry adds it once");
    assert_eq!(titles(&w, &s), ["a", "c", "b"]);
}

// v5, every failure point of the publish: the uncertain temp file's flush (nothing renamed), the certain rewrite's
// folder flush (renamed certain), and a crash that loses that rename. Each refuses with the id; the retry saves once.
#[test]
fn every_failure_point_of_the_publish_saves_the_note_once() {
    for case in ["uncertain-temp", "certain-folder", "certain-lost"] {
        let (w, s) = world();
        add(&w, &s, "kept").unwrap();
        let before = state(&w, &s);
        match case {
            "uncertain-temp" => crate::state::fs::fail_sync_file(Some(&notes(&w, &s).join(STATE))),
            _ => crate::state::fs::fail_durable_flush(Some(1)),
        }
        let e = add(&w, &s, "new").unwrap_err();
        crate::state::fs::fail_sync_file(None);
        crate::state::fs::fail_durable_flush(None);
        assert_eq!((e.code, e.check), (1, "notes"), "{case}");
        let end = std::fs::metadata(notes(&w, &s).join(LOG)).unwrap().len();
        let want = match case {
            "uncertain-temp" => before.clone(),
            _ => json!({"durable": end, "uncertain": false}),
        };
        assert_eq!(state(&w, &s), want, "{case}: the marker the failure leaves");
        if case == "certain-lost" {
            let lost = json!({"durable": end, "uncertain": true});
            crate::state::fs::write_json(&notes(&w, &s).join(STATE), &lost).unwrap();
        }
        let r = append(&tx(&w, &s), &person(), "new", "body", Some(&id_of(&e))).unwrap();
        assert_eq!(
            matches!(r, Wrote::Found(_)),
            case != "uncertain-temp",
            "{case}: {r:?}"
        );
        assert_eq!(
            titles(&w, &s),
            ["kept", "new"],
            "{case}: once, nothing lost"
        );
    }
}

// v5: an uncertain marker beside a longer log. A completion that fails (its folder flush, or the certain temp
// file's flush) refuses a reader and an `-Id` retry and leaves the log and the marker byte for byte.
#[test]
fn a_failed_completion_changes_neither_the_log_nor_the_marker() {
    for fault in ["folder", "temp"] {
        let (w, s) = world();
        add(&w, &s, "kept").unwrap();
        crate::state::fs::fail_durable_flush(Some(0));
        let e = add(&w, &s, "new").unwrap_err();
        crate::state::fs::fail_durable_flush(None);
        raw(&w, &s, b"{\"id\":\"ff");
        let at = notes(&w, &s);
        let (log, marker) = (
            std::fs::read(at.join(LOG)).unwrap(),
            std::fs::read(at.join(STATE)).unwrap(),
        );
        match fault {
            "folder" => crate::state::fs::fail_sync_dir(Some(&at)),
            _ => crate::state::fs::fail_sync_file(Some(&at.join(STATE))),
        }
        let r = entries(&tx(&w, &s)).unwrap_err();
        let retry = append(&tx(&w, &s), &person(), "new", "body", Some(&id_of(&e))).unwrap_err();
        crate::state::fs::fail_sync_dir(None);
        crate::state::fs::fail_sync_file(None);
        let not_yet = (1, "notes", "the notebook is not yet on disk; retry");
        assert_eq!(
            (r.code, r.check, r.message.as_str()),
            not_yet,
            "{fault}: reader"
        );
        assert_eq!(
            (retry.code, retry.check, retry.message.as_str()),
            not_yet,
            "{fault}: -Id"
        );
        assert_eq!(
            std::fs::read(at.join(LOG)).unwrap(),
            log,
            "{fault}: log byte for byte"
        );
        assert_eq!(
            std::fs::read(at.join(STATE)).unwrap(),
            marker,
            "{fault}: marker byte for byte"
        );
        assert_eq!(
            titles(&w, &s),
            ["kept", "new"],
            "{fault}: completed once the flush works"
        );
    }
}

// F2: the completion's own folder flush failing inside the append (the third `sync_dir(notes)` of that append:
// line, uncertain publish, completion) refuses with the id; the retry completes and answers it once.
#[test]
fn a_failed_completion_inside_the_append_refuses_and_the_retry_answers_once() {
    let (w, s) = world();
    add(&w, &s, "kept").unwrap();
    let at = notes(&w, &s);
    crate::state::fs::fail_sync_dir_nth(Some(&at), 2);
    let e = add(&w, &s, "new").unwrap_err();
    crate::state::fs::fail_sync_dir_nth(None, 0);
    assert_eq!((e.code, e.check), (1, "notes"));
    let end = std::fs::metadata(at.join(LOG)).unwrap().len();
    assert_eq!(
        state(&w, &s),
        json!({"durable": end, "uncertain": true}),
        "renamed, never completed"
    );
    let r = append(&tx(&w, &s), &person(), "new", "body", Some(&id_of(&e))).unwrap();
    assert!(matches!(r, Wrote::Found(_)), "{r:?}");
    assert_eq!(titles(&w, &s), ["kept", "new"]);
}
