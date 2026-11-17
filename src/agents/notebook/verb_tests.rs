//! Task 5 (phase 7a): `note`, the proven caller writing the team notebook (N1, N3; ambiguities 2, 3, 7).
use serde_json::{Value, json};

use super::tests::{notes, titles, tx, world};
use super::*;
use crate::agents::fixture::World;
use crate::agents::team_state::{Scope, Tx};
use crate::agents::tree::tests::{
    LEADER, OTHER, child, member_of, member_world, team_record, term,
};

pub(crate) fn argv(tokens: &[&str]) -> Vec<String> {
    tokens.iter().map(|t| t.to_string()).collect()
}
// One verb through the entry, as a person of the team in `w`.
pub(crate) fn run(w: &World, tokens: &[&str]) -> (u8, Value) {
    let (code, out) = crate::cli::run(&argv(tokens), || Ok(&w.ctx));
    (
        code,
        serde_json::from_str(out.trim()).unwrap_or(Value::Null),
    )
}
pub(crate) fn team() -> (World, Scope) {
    let (w, s) = world();
    team_record(&w, &[]);
    (w, s)
}
pub(crate) fn refuse_replay(_: &Tx<'_>) -> Res<()> {
    Err(refusal(2, "replay", "replay refused"))
}
// 7b V2: the team snapshot a member's `note` takes for its notices: hc running every hc member of the caller's
// session, read in address order.
pub(crate) fn notice_round(w: &World, state: &std::path::Path, caller: &crate::caller::Caller) {
    let session = caller.session.clone();
    let mut records: Vec<Value> = std::fs::read_dir(state.join("agents"))
        .unwrap()
        .filter_map(|e| {
            crate::state::fs::read_json(&e.unwrap().path().join("record.json"))
                .ok()
                .flatten()
        })
        .filter(|r| r.get("hcAddress").is_some() && r["session"] == session.as_str())
        .collect();
    records.sort_by_key(|r| crate::agents::text(r.get("hcAddress")));
    crate::agents::core_fixture::online(w);
    crate::agents::core_fixture::listing(w, &crate::agents::membership::team(caller), &[]);
    for r in &records {
        crate::agents::core_fixture::get(
            w,
            r["hcAddress"].as_str().unwrap(),
            crate::agents::core_fixture::wire(r),
        );
    }
}

#[test]
fn a_person_and_a_member_write_as_themselves() {
    let (w, s) = team();
    let (code, r) = run(&w, &["note", "-Title", "plan", "-Body", "hi"]);
    assert_eq!((code, &r["duplicate"]), (0, &json!(false)), "{r}");
    let e = entries(&tx(&w, &s)).unwrap();
    assert_eq!(
        e[0]["author"],
        json!({"name": "person", "id": null, "key": "person"})
    );
    assert_eq!(e[0]["id"], r["id"]);
    // A proven member (phase 3 D1): resolve_tree and standing each take one hc round.
    let mut m = member_world();
    m.hc();
    m.rounds(2, &term(&m.r, Some(150)));
    notice_round(&m.w, &m.state, &m.c);
    let (def, request) =
        crate::cli::parse(&argv(&["note", "-Title", "plan", "-Body", "hi"])).unwrap();
    (def.handler)(&m.w.ctx, &request).unwrap();
    let ms = Scope::of(&m.c);
    let e = entries(&Tx {
        ctx: &m.w.ctx,
        scope: ms,
    })
    .unwrap();
    let id = crate::agents::fixture::ID;
    assert_eq!(e[0]["author"], json!({"name": "maa", "id": id, "key": id}));
    m.w.fakes.hc_done().unwrap();
}

// N1: the key is the inherited mailbox, unchanged for a resumed successor (its record names its predecessor's).
#[test]
fn the_author_key_is_the_inherited_mailbox() {
    let id = crate::agents::fixture::ID;
    let own = author_of(&member_of(id, child(id, LEADER, 1)));
    assert_eq!(
        (own.name.as_str(), own.id.as_deref(), own.key.as_str()),
        ("maa", Some(id), id)
    );
    let mut record = child(OTHER, LEADER, 1);
    record["mailbox"] = json!(id);
    let successor = author_of(&member_of(OTHER, record));
    assert_eq!(
        (successor.id.as_deref(), successor.key.as_str()),
        (Some(OTHER), id)
    );
}

// N3, ambiguity 7: outside a team 2 `team`; a member whose proof fails under the lock 2 `tagged`; a pane agent
// that is no hc member keeps phase 3's 2 `caller`. None of them makes a notes folder.
#[test]
fn outside_a_team_or_unproven_the_notebook_refuses() {
    let (w, _) = world();
    let (code, r) = run(&w, &["note", "-Title", "plan", "-Body", "hi"]);
    assert_eq!(
        (code, &r["check"], &r["message"]),
        (2, &json!("team"), &json!("the notebook belongs to a team"))
    );
    let mut m = member_world();
    m.hc();
    let t = term(&m.r, Some(150));
    m.rounds(1, &t);
    let mut later = t.clone();
    later["argv"] = json!(["/bin/sh", "-l"]);
    m.rounds(1, &later);
    let (def, request) =
        crate::cli::parse(&argv(&["note", "-Title", "plan", "-Body", "hi"])).unwrap();
    assert_eq!(
        (def.handler)(&m.w.ctx, &request).unwrap_err().check,
        "tagged"
    );
    assert!(
        !m.state
            .join("teams")
            .join(&crate::util::sha256_hex(&m.c.session)[..16])
            .exists()
    );
    m.w.fakes.hc_done().unwrap();
    let a = World::agent("");
    a.state();
    team_record(&a, &[]);
    assert_eq!(
        run(&a, &["note", "-Title", "plan", "-Body", "hi"]).1["check"],
        "caller"
    );
}

// Ambiguity 3: every limit is 2 `usage`; at the limit it is accepted.
#[test]
fn a_note_outside_the_limits_is_usage() {
    let (w, _) = team();
    let (t80, t81) = ("x".repeat(80), "x".repeat(81));
    let (b4096, b4097) = ("y".repeat(4096), "y".repeat(4097));
    // The units: 80 characters in 160 bytes; 2049 characters in 4098 bytes. Call 7: CRLF x 4096 is 8192 bytes, and
    // 4096 after `message_text`; inline and in a file with a BOM it is within the limit.
    let (wide80, wide81, wide_body) = ("é".repeat(80), "é".repeat(81), "é".repeat(2049));
    let crlf = "\r\n".repeat(4096);
    let mut bom = vec![0xef, 0xbb, 0xbf];
    bom.extend(crlf.as_bytes());
    std::fs::write(w.ctx.cwd.join("crlf.md"), &bom).unwrap();
    std::fs::write(w.ctx.cwd.join("big.md"), &b4097).unwrap();
    let ok: Vec<Vec<&str>> = vec![
        vec!["note", "-Title", &t80, "-Body", "b"],
        vec!["note", "-Title", &wide80, "-Body", "b"],
        vec!["note", "-Title", "t", "-Body", &b4096],
        vec!["note", "-Title", "t", "-Body", &crlf],
        vec!["note", "-Title", "t", "-BodyFile", "crlf.md"],
    ];
    for tokens in ok {
        assert_eq!(run(&w, &tokens).0, 0, "{:?}", &tokens[..4]);
    }
    let bad: Vec<Vec<&str>> = vec![
        vec!["note", "-Title", &t81, "-Body", "b"],
        vec!["note", "-Title", &wide81, "-Body", "b"],
        vec!["note", "-Title", "t", "-Body", &b4097],
        vec!["note", "-Title", "t", "-Body", &wide_body],
        vec!["note", "-Title", "t", "-BodyFile", "big.md"],
        vec!["note", "-Title", "a\nb", "-Body", "b"],
        vec!["note", "-Title", "\u{1b}", "-Body", "b"],
        vec!["note", "-Title", "t"],
        vec!["note", "-Title", "t", "-Body", "b", "-BodyFile", "big.md"],
        vec!["note", "-Title", "t", "-Body", "b", "-Id", "0123456"],
        vec![
            "note",
            "-Title",
            "t",
            "-Body",
            "b",
            "-Id",
            "0123456789ABCDEF",
        ],
    ];
    for tokens in bad {
        let (code, r) = run(&w, &tokens);
        assert_eq!(
            (code, &r["check"]),
            (2, &json!("usage")),
            "{:?}",
            &tokens[..3]
        );
    }
}

// N2: two notes at once (the real per-user locks, one state root): two whole lines, in the team lock's order.
#[test]
fn two_concurrent_notes_are_two_whole_lines_in_lock_order() {
    let (w, s) = team();
    let (state, locks) = (
        w.ctx.state_root.clone(),
        w.ctx.state_root.parent().unwrap().join("locks"),
    );
    let order = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    std::thread::scope(|scope| {
        for name in ["one", "two"] {
            let (state, locks, order) = (state.clone(), locks.clone(), order.clone());
            scope.spawn(move || {
                let mut r = World::human("");
                r.ctx.state_root = state;
                r.fakes.real_locks(&locks);
                r.fakes.lock_probe(move |event| {
                    if event.starts_with('+') && event.contains("spawn|") {
                        order.lock().unwrap().push(name.to_string());
                    }
                    String::new()
                });
                assert_eq!(
                    run(&r, &["note", "-Title", name, "-Body", "x"]).0,
                    0,
                    "{name}"
                );
            });
        }
    });
    let log = std::fs::read_to_string(notes(&w, &s).join(LOG)).unwrap();
    assert!(
        log.lines()
            .all(|l| serde_json::from_str::<Value>(l).is_ok()),
        "whole lines: {log}"
    );
    assert_eq!(titles(&w, &s), *order.lock().unwrap());
}

// Call 3 through the verb: two notes, the marker deleted, the next `note` keeps both.
#[test]
fn a_note_after_a_deleted_marker_keeps_both() {
    let (w, s) = team();
    for title in ["a", "b"] {
        assert_eq!(
            run(&w, &["note", "-Title", title, "-Body", "x"]).0,
            0,
            "{title}"
        );
    }
    std::fs::remove_file(notes(&w, &s).join(STATE)).unwrap();
    assert_eq!(run(&w, &["note", "-Title", "c", "-Body", "x"]).0, 0);
    assert_eq!(titles(&w, &s), ["a", "b", "c"]);
}

// Call 2: an agent passes its body inline (`-Body`) or in a file (`-BodyFile`, from its working folder); both
// are saved, the file's CRLF made LF.
#[test]
fn an_agent_writes_with_body_and_with_body_file() {
    let mut m = member_world();
    std::fs::write(m.w.ctx.cwd.join("note.md"), "from a file\r\n").unwrap();
    m.hc();
    let t = term(&m.r, Some(150));
    for tokens in [
        ["note", "-Title", "inline", "-Body", "from the line"],
        ["note", "-Title", "file", "-BodyFile", "note.md"],
    ] {
        m.rounds(2, &t);
        notice_round(&m.w, &m.state, &m.c);
        let (def, request) = crate::cli::parse(&argv(&tokens)).unwrap();
        assert!((def.handler)(&m.w.ctx, &request).is_ok(), "{tokens:?}");
    }
    let e = entries(&Tx {
        ctx: &m.w.ctx,
        scope: Scope::of(&m.c),
    })
    .unwrap();
    let got: Vec<(&str, &str)> = e
        .iter()
        .map(|e| (e["title"].as_str().unwrap(), e["body"].as_str().unwrap()))
        .collect();
    assert_eq!(
        got,
        [("inline", "from the line"), ("file", "from a file\n")]
    );
    m.w.fakes.hc_done().unwrap();
}

// N3: `-Id` through the public verb: forwarded to the store, a retry answers `duplicate: true` and writes nothing,
// and a refused note's retry with its id saves it once.
#[test]
fn note_forwards_its_id_and_answers_a_retry_as_a_duplicate() {
    let (w, s) = team();
    let id = "0123456789abcdef";
    let tokens = ["note", "-Title", "plan", "-Body", "b", "-Id", id];
    let (code, r) = run(&w, &tokens);
    assert_eq!(
        (code, &r["id"], &r["duplicate"]),
        (0, &json!(id), &json!(false)),
        "{r}"
    );
    let log = std::fs::read(notes(&w, &s).join(LOG)).unwrap();
    let (code, r) = run(&w, &tokens);
    assert_eq!(
        (code, &r["id"], &r["duplicate"]),
        (0, &json!(id), &json!(true)),
        "{r}"
    );
    assert_eq!(
        std::fs::read(notes(&w, &s).join(LOG)).unwrap(),
        log,
        "a duplicate writes nothing"
    );
    crate::state::fs::fail_sync_file(Some(&notes(&w, &s).join(LOG)));
    let (code, r) = run(&w, &["note", "-Title", "next", "-Body", "b"]);
    crate::state::fs::fail_sync_file(None);
    assert_eq!((code, &r["check"]), (1, &json!("notes")), "{r}");
    let next = r["id"].as_str().unwrap().to_string();
    let retry = ["note", "-Title", "next", "-Body", "b", "-Id", next.as_str()];
    assert_eq!(
        run(&w, &retry).1["duplicate"],
        json!(false),
        "the retry saves it"
    );
    assert_eq!(
        run(&w, &retry).1["duplicate"],
        json!(true),
        "then answers it"
    );
    assert_eq!(titles(&w, &s), ["plan", "next"]);
}

// PM call 6: replay runs first: a refusing hook stops `note` before its session folder or `notes/` exists.
#[test]
fn replay_runs_before_note_makes_its_folder() {
    let (w, s) = team();
    crate::agents::team_state::test_hook(Some(refuse_replay as crate::agents::team_state::Hook));
    let (code, r) = run(&w, &["note", "-Title", "plan", "-Body", "b"]);
    crate::agents::team_state::test_hook(None);
    assert_eq!((code, &r["check"]), (2, &json!("replay")), "{r}");
    assert!(
        !notes(&w, &s).parent().unwrap().exists(),
        "nothing made before replay"
    );
}

// N3 (F7): the caller is proven under the team lock: a launch aborted as that lock is taken refuses `tagged`.
#[test]
fn the_caller_is_proven_under_the_team_lock() {
    let mut m = member_world();
    m.hc();
    m.rounds(2, &term(&m.r, Some(150)));
    let commit = m.folder(crate::agents::fixture::ID).join("commit.json");
    m.w.fakes.lock_probe(move |event| {
        if event.starts_with("+spawn|") {
            crate::agents::fixture::write(&commit, json!({"aborted": true}));
        }
        String::new()
    });
    let (def, request) =
        crate::cli::parse(&argv(&["note", "-Title", "plan", "-Body", "hi"])).unwrap();
    assert_eq!(
        (def.handler)(&m.w.ctx, &request).unwrap_err().check,
        "tagged"
    );
    let team = m
        .state
        .join("teams")
        .join(&crate::util::sha256_hex(&m.c.session)[..16]);
    assert!(!team.join("notes").exists(), "nothing saved");
}

// Ambiguity 3 (F14): the body is `message_text`'s: CRLF made LF; ESC, DEL and the other controls but TAB and LF gone.
#[test]
fn a_note_body_loses_its_controls() {
    let (w, s) = team();
    let (code, r) = run(
        &w,
        &["note", "-Title", "t", "-Body", "a\u{1b}[31mb\u{7f}\tc\r\nd"],
    );
    assert_eq!(code, 0, "{r}");
    assert_eq!(
        entries(&tx(&w, &s)).unwrap()[0]["body"],
        json!("a[31mb\tc\nd")
    );
}

// Ambiguity 7 (F15): a leaderless team (a launched leader, no team record) is a team.
#[test]
fn a_leaderless_team_keeps_a_notebook() {
    let (w, s) = world();
    w.uncommitted(LEADER, json!({"depth": 0, "upTeam": "t"}));
    let (code, r) = run(&w, &["note", "-Title", "plan", "-Body", "b"]);
    assert_eq!(code, 0, "{r}");
    assert_eq!(titles(&w, &s), ["plan"]);
}

// N3 (F12, PM call 12): `-Id` is 8-32 lowercase hex digits; `-BodyFile` is read up to exactly `RAW_MAX` bytes, and
// one more is refused even though it would normalize to one byte.
#[test]
fn ids_and_the_raw_read_are_bounded_exactly() {
    let (w, _) = team();
    let (eight, thirty_two, thirty_three) = ("0".repeat(8), "a".repeat(32), "a".repeat(33));
    for (id, code) in [
        (eight.as_str(), 0),
        (thirty_two.as_str(), 0),
        (thirty_three.as_str(), 2),
        ("0123456g", 2),
    ] {
        let (got, r) = run(&w, &["note", "-Title", id, "-Body", "b", "-Id", id]);
        assert_eq!(got, code, "{id}: {r}");
    }
    let mut raw = b"x".to_vec();
    raw.resize(4 * 4096 + 3, 0x1b); // RAW_MAX as a literal, so a changed constant fails
    std::fs::write(w.ctx.cwd.join("max.md"), &raw).unwrap();
    raw.push(0x1b);
    std::fs::write(w.ctx.cwd.join("over.md"), &raw).unwrap();
    assert_eq!(
        run(&w, &["note", "-Title", "max", "-BodyFile", "max.md"]).0,
        0,
        "exactly RAW_MAX is read"
    );
    let (code, r) = run(&w, &["note", "-Title", "over", "-BodyFile", "over.md"]);
    assert_eq!(
        (code, &r["check"]),
        (2, &json!("usage")),
        "RAW_MAX + 1 is refused: {r}"
    );
}
