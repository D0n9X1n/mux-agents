//! restore-6c Task 11 (#103): the new team lists and closes its predecessors' kept shells (spec "kept shells across
//! retirement"; ambiguity 14; calls 3, 12; 10-13 F9-F13). The predecessor is session `$5`, retired; the caller stays
//! in `$3`, so `w.caller()` stays admitted (D20 refuses only a retired session).
use super::core_fixture as cf;
use super::fixture::World;
use super::kept;
use super::reconcile_tests as rt;
use super::reconcile_verb_tests::person;
use super::recover_tests::{cli, failed_ended};
use super::team_state::{self, Scope};
use super::up::retire;
use super::up::tests as ut;
use serde_json::{Value, json};

/// The predecessor team's T and its leader's id.
const T: &str = "0123456789abcdef";
const P: &str = "eeeeeeeeeeee";

/// A kept shell `aK` labelled for team label `t`, as hc shows it.
fn wire(t: &str) -> Value {
    json!({"address": "aK", "name": "m", "state": "live", "argv": ["/bin/sh"],
        "labels": {"mux.kept": t, "mux.keptName": "mbbb"}})
}
fn kills(w: &World) -> usize {
    w.fakes
        .hc_calls()
        .iter()
        .filter(|c| c.1.first().map(String::as_str) == Some("kill"))
        .count()
}
/// A predecessor team T in session `$5` under the World's key, its leader stopped, retired (P6). Returns its kept
/// label `<key>:$5`. Retirement asks hc nothing and kills nothing (V6; row 10.16).
fn retired_predecessor(w: &World) -> String {
    let key = w.caller().server_key;
    let pred = Scope {
        key: key.clone(),
        session: "$5".into(),
    };
    let lead = rt::member_at(
        w,
        &key,
        P,
        json!({"depth": 0, "upTeam": T, "session": "$5", "name": "leader"}),
    );
    failed_ended(&lead, 1);
    team_state::open(&w.ctx, &pred, |tx| tx.desired(P, false)).unwrap();
    ut::seed_index_at(w, T, &key, "$5", ut::dead_server());
    let before = w.fakes.hc_calls().len();
    retire::retire(&w.ctx, &w.caller(), &pred, T).unwrap();
    assert_eq!(
        w.fakes.hc_calls().len(),
        before,
        "retirement asks hc nothing, kills nothing (V6)"
    );
    format!("{key}:$5")
}

/// Spec: the predecessor's kept shell survives its team's retirement. The successor lists it by the old T's
/// anchor, read through `index::read_retired` (call 12), and `close_any` closes it; a torn archive refuses.
#[test]
fn a_kept_shell_survives_retirement_and_closes_from_the_new_team() {
    let w = person();
    let pred_t = retired_predecessor(&w);
    let key = w.caller().server_key;
    let new = Scope {
        key: key.clone(),
        session: "$7".into(),
    };
    let teams = retire::kept_teams(&w.ctx, &new, T).unwrap();
    assert_eq!(
        teams,
        vec![format!("{key}:$7"), pred_t.clone()],
        "own T, then the retired index's anchor"
    );
    cf::kept_listing(&w, &teams[0], &[]); // the connection is `person()`'s `--version`
    cf::kept_listing(&w, &pred_t, &["aK"]);
    cf::get(&w, "aK", wire(&pred_t));
    let core = crate::hc::connect(&w.ctx).unwrap();
    let rows = kept::list_all(&core, &teams).unwrap();
    let got: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| (r.address.as_str(), r.team.as_str()))
        .collect();
    assert_eq!(
        got,
        [("aK", pred_t.as_str())],
        "listed by the predecessor's T"
    );
    cf::get(&w, "aK", wire(&pred_t)); // `close_any` reads the label
    cf::get(&w, "aK", wire(&pred_t)); // `close`'s own re-read right before the kill (V5)
    cf::answer(&w, &["kill", "aK", "--json"], r#"{"ok":true}"#);
    kept::close_any(&core, &teams, "aK").unwrap();
    assert_eq!(kills(&w), 1, "closed once");
    w.fakes.hc_done().unwrap();
    let torn = (w.ctx.state_root.join("up").join("retired"))
        .join(format!("{T}.9.{}.json", "0".repeat(16)));
    std::fs::write(&torn, "{\"team\":").unwrap();
    assert_eq!(
        retire::kept_teams(&w.ctx, &new, T)
            .map_err(|e| e.check)
            .err(),
        Some("index"),
        "a torn retired index refuses (call 12), never a guessed T"
    );
}

/// Finding 15, checklist 59: the real `kept -Close` from the live team T in the caller's session closes the
/// predecessor's shell, then lists both teams (scripted before the verb: it lists after closing); a shell of no
/// team of this T is refused `target` and not killed.
#[test]
fn kept_close_reaches_a_predecessors_shell_through_the_dispatcher() {
    let w = person();
    let pred_t = retired_predecessor(&w);
    let me = w.caller();
    let own = format!("{}:{}", me.server_key, me.session);
    ut::seed_index_at(&w, T, &me.server_key, &me.session, ut::dead_server());
    cf::get(&w, "aK", wire(&pred_t));
    cf::get(&w, "aK", wire(&pred_t));
    cf::answer(&w, &["kill", "aK", "--json"], r#"{"ok":true}"#);
    cf::kept_listing(&w, &own, &[]);
    cf::kept_listing(&w, &pred_t, &[]);
    let (code, reply) = cli(&w, &["kept", "-Close", "aK"]);
    assert_eq!(
        (code, &reply["closed"], &reply["kept"]),
        (0, &json!("aK"), &json!([])),
        "{reply}"
    );
    w.fakes.hc_done().unwrap();
    cf::online(&w);
    cf::get(&w, "aK", wire("ffffffffffffffff:$9"));
    let (code, reply) = cli(&w, &["kept", "-Close", "aK"]);
    assert_eq!(
        (code, &reply["check"]),
        (2, &json!("target")),
        "a stranger's shell: {reply}"
    );
    assert_eq!(kills(&w), 1, "the stranger's shell is not killed");
    w.fakes.hc_done().unwrap();
}

/// 10-13 F10, checklist 57: the rows the board's watch lists (`kept::watched`, its producer) reach the bar
/// (`sidebar::kept_targets`) each with its own team.
#[test]
fn the_watch_hands_each_kept_row_its_own_team_to_the_bar() {
    let w = person();
    let pred_t = retired_predecessor(&w);
    let me = w.caller();
    let own = format!("{}:{}", me.server_key, me.session);
    ut::seed_index_at(&w, T, &me.server_key, &me.session, ut::dead_server());
    cf::kept_listing(&w, &own, &[]);
    cf::kept_listing(&w, &pred_t, &["aK"]);
    cf::get(&w, "aK", wire(&pred_t));
    let rows = kept::watched(&w.ctx, &me).unwrap();
    let board = json!({"kept": kept::rows(&rows)});
    let targets = crate::sidebar::kept_targets(board.as_object());
    let got: Vec<(&str, &str)> = targets
        .iter()
        .map(|x| (x.id.as_str(), x.team.as_str()))
        .collect();
    assert_eq!(
        got,
        [("aK", pred_t.as_str())],
        "the predecessor's row reaches the bar with its team"
    );
    w.fakes.hc_done().unwrap();
}

/// 10-13 F13: a retired entry whose name cannot be read refuses, never skipped. Linux only: macOS (APFS) and
/// Windows refuse such a name at creation.
#[cfg(target_os = "linux")]
#[test]
fn kept_teams_refuses_a_name_it_cannot_read() {
    use std::os::unix::ffi::OsStrExt;
    let w = person();
    retired_predecessor(&w);
    let me = w.caller(); // before the bad name: admission reads every archive
    let scope = Scope {
        key: me.server_key,
        session: me.session,
    };
    let dir = w.ctx.state_root.join("up").join("retired");
    std::fs::write(dir.join(std::ffi::OsStr::from_bytes(b"\xff.json")), "{}").unwrap();
    assert!(
        retire::kept_teams(&w.ctx, &scope, T).is_err(),
        "an unreadable name refuses"
    );
}

/// Checklist 57 (10-13 F8), row 11.12: the board's own watch (`board::board(.., watch)`) lists the predecessor's
/// kept shell with its team, beside a lifecycle member of the live team.
#[test]
fn the_board_watch_lists_a_predecessors_kept_shell() {
    let w = person();
    let pred_t = retired_predecessor(&w);
    let me = w.caller();
    let own = format!("{}:{}", me.server_key, me.session);
    ut::seed_index_at(&w, T, &me.server_key, &me.session, ut::dead_server());
    let x = rt::member(
        &w,
        rt::X,
        json!({"depth": 0, "upTeam": T, "name": "leader"}),
    );
    rt::ok(&x, 1);
    // checklist 68, the observed order (`hc_calls`): the board's member listing and the member's recorded address
    // on `person()`'s connection, then `kept::watched`'s own connection and its two kept listings
    cf::answer(
        &w,
        &[
            "ls",
            "--all",
            "--label",
            "mux.managed=1",
            "--label",
            &format!("mux.team={own}"),
            "--json",
        ],
        r#"{"terminals":[]}"#,
    );
    cf::get(
        &w,
        &format!("a{}", rt::X),
        rt::wire_of(&w, rt::X, 1, &super::life_fixture::ARGV),
    );
    cf::online(&w);
    cf::kept_listing(&w, &own, &[]);
    cf::kept_listing(&w, &pred_t, &["aK"]);
    cf::get(&w, "aK", wire(&pred_t));
    let board = super::board::board(&w.ctx, &me, false, true).unwrap();
    assert_eq!(
        board.get("kept"),
        Some(&json!([{"address": "aK", "name": "mbbb", "team": pred_t}])),
        "{board:?}"
    );
    w.fakes.hc_done().unwrap();
}
