//! notebook-7b V2, V3: one notice per other member when a note publishes; a failed notice never undoes it.
use serde_json::{Value, json};

use super::*;
use crate::agents::notebook::Author;
use crate::agents::notebook::verb_tests::run;
use crate::agents::team_mail::tests::{A, B, C, inbox, online, team_world};
use crate::agents::tree::tests::LEADER;

const ID: &str = "0123456789abcdef";
const BODY: &str =
    "new note 0123456789abcdef from ma1: plan — run mux-agent notes 0123456789abcdef";

fn author_a() -> Author {
    Author {
        name: "ma1".into(),
        id: Some(A.into()),
        key: A.into(),
    }
}

fn entry() -> Value {
    json!({"id": ID, "author": {"name": "ma1", "id": A, "key": A}, "title": "plan", "body": "b"})
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[test]
fn one_notice_per_other_member_none_to_the_author_none_twice() {
    let w = team_world();
    online(&w);
    let (notified, errors) = notify(&w.ctx, &w.caller(), &author_a(), &entry());
    assert_eq!(
        (sorted(notified), errors),
        (vec!["leader".to_string(), "mb2".to_string()], vec![])
    );
    for key in [LEADER, B] {
        let got = inbox(&w, key);
        assert_eq!(got.len(), 1, "{key}");
        assert_eq!(
            (&got[0]["id"], &got[0]["body"], &got[0]["from"]),
            (&json!(format!("note-{ID}")), &json!(BODY), &json!("ma1")),
            "{key}"
        );
    }
    assert!(inbox(&w, A).is_empty(), "none for the author");
    // A retried `note` (the same entry) posts no second copy.
    online(&w);
    notify(&w.ctx, &w.caller(), &author_a(), &entry());
    for key in [LEADER, B] {
        assert_eq!(inbox(&w, key).len(), 1, "{key}: no second copy");
    }
    // A member who joins after the note gets no back-notice (the launch index covers it).
    crate::agents::core_fixture::member(
        &w,
        C,
        json!({"name": "mc3", "depth": 1, "parentAgent": {"id": LEADER}, "pane": "%8", "window": "@1"}),
    );
    assert!(inbox(&w, C).is_empty(), "no back-notice");
    w.fakes.hc_done().unwrap();
}

#[test]
fn a_torn_mailbox_line_is_sealed_and_the_notice_appended() {
    let w = team_world();
    let mail = w.state().join("mail");
    std::fs::create_dir_all(&mail).unwrap();
    let torn = r#"{"id":"exit-aaaaaaaaaaaa""#;
    std::fs::write(mail.join(format!("{LEADER}.jsonl")), torn).unwrap();
    online(&w);
    notify(&w.ctx, &w.caller(), &author_a(), &entry());
    let text = std::fs::read_to_string(mail.join(format!("{LEADER}.jsonl"))).unwrap();
    let (first, rest) = text.split_once('\n').unwrap();
    assert_eq!(first, torn);
    let notice: Value = serde_json::from_str(rest.trim_end()).unwrap();
    assert_eq!(notice["body"], BODY);
}

// V3: one member's mailbox unwritable: the note persists, the others are notified, `notifyErrors` names the member,
// and the exit code is unchanged.
#[test]
fn a_failed_notice_never_undoes_the_note_or_skips_the_others() {
    let w = team_world();
    std::fs::create_dir_all(w.state().join("mail").join(format!("{B}.jsonl"))).unwrap();
    online(&w);
    let (code, r) = run(&w, &["note", "-Title", "plan", "-Body", "b"]);
    assert_eq!(code, 0, "{r}");
    let names = |v: &Value| -> Vec<String> {
        let all = v.as_array().unwrap().iter();
        sorted(
            all.map(|x| {
                x.as_str()
                    .unwrap_or_else(|| x["name"].as_str().unwrap())
                    .to_string()
            })
            .collect(),
        )
    };
    assert_eq!(names(&r["notified"]), ["leader", "ma1"], "{r}");
    assert_eq!(names(&r["notifyErrors"]), ["mb2"], "{r}");
    assert!(r["notifyErrors"][0]["check"].is_string(), "{r}");
    let scope = crate::agents::team_state::Scope::of(&w.caller());
    assert_eq!(crate::agents::notebook::tests::titles(&w, &scope), ["plan"]);
    let id = r["id"].as_str().unwrap();
    assert_eq!(inbox(&w, LEADER)[0]["id"], format!("note-{id}"));
}
