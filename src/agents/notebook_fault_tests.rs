//! Tasks 2-3 (#103 phase 7a): a failed append names its id; flush faults before and after the publish; `-Id`.
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
