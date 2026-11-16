use super::*;
use crate::agents::core_fixture as cf;
use crate::agents::fixture::World;
use crate::agents::fixture::{Listed, row_of};
use crate::agents::tree::tests::{LEADER, child, leader_record};
use crate::error::Refusal;
use crate::util::{parse_utc, utc_o};
use serde_json::json;

const T0: &str = "2026-03-01T00:00:00Z";

fn at(secs: u64) -> String {
    utc_o(parse_utc(T0).unwrap() + Duration::from_secs(secs))
}

fn put(dir: &Path, id: &str, at: &str, done: bool) {
    std::fs::create_dir_all(dir).unwrap();
    crate::state::fs::write_json(
        &dir.join(format!("{id}.json")),
        &json!({"id": id, "at": at, "done": done}),
    )
    .unwrap()
}

fn dir_of(w: &World, session: &str, sender: &str) -> PathBuf {
    sends_dir(
        &crate::agents::server_dir(&w.ctx, &w.caller()),
        session,
        sender,
    )
}

#[test]
fn quota_counts_the_last_hour_and_slides() {
    let w = World::human("");
    w.clock(T0);
    let a = dir_of(&w, "$3", "aaaaaaaaaaaa");
    for n in 0..120u64 {
        put(&a, &format!("{n:016x}"), &at(n * 10), true);
    }
    w.fakes.advance(Duration::from_secs(1200));
    let r = charge(&w.ctx, &a, true);
    assert!(r.is_err(), "the 121st refuses");
    let r = r.unwrap_err();
    assert_eq!(
        (r.code, r.check, r.message.as_str()),
        (
            2,
            "quota",
            "120 messages in the last hour; retry at 2026-03-01T01:00:00.0000000Z"
        )
    );
    assert_eq!(r.details["retryAt"], "2026-03-01T01:00:00.0000000Z");
    charge(&w.ctx, &dir_of(&w, "$3", "bbbbbbbbbbbb"), true).unwrap(); // another sender
    charge(&w.ctx, &dir_of(&w, "$4", "aaaaaaaaaaaa"), true).unwrap(); // another team
    charge(&w.ctx, &a, false).unwrap(); // a person: not counted
    w.fakes.advance(Duration::from_secs(2400)); // T0 + 3600: the oldest is out
    charge(&w.ctx, &a, true).unwrap();
}

#[test]
fn pruning_drops_only_done_intents_older_than_a_day() {
    let w = World::human("");
    w.clock(T0);
    let a = dir_of(&w, "$3", "aaaaaaaaaaaa");
    put(&a, "1111111111111111", &at(0), true);
    put(&a, "2222222222222222", &at(0), false);
    std::fs::write(a.join("3333333333333333.json"), "not json").unwrap();
    w.fakes.advance(Duration::from_secs(86_401));
    charge(&w.ctx, &a, true).unwrap();
    assert!(
        !a.join("1111111111111111.json").exists(),
        "done and past a day: pruned"
    );
    assert!(
        a.join("2222222222222222.json").exists(),
        "unfinished: kept, still retryable"
    );
    assert!(
        a.join("3333333333333333.json").exists(),
        "unreadable: kept and counted"
    );
}

#[test]
fn fresh_ids_never_collide_across_senders() {
    let w = World::human("");
    let root = sends_root(&crate::agents::server_dir(&w.ctx, &w.caller()), "$3");
    put(&root.join("bbbbbbbbbbbb"), "1111111111111111", "x", false);
    let mut seq = ["1111111111111111", "2222222222222222"]
        .into_iter()
        .map(String::from);
    assert_eq!(
        fresh_id(&root, || Ok(seq.next().unwrap())).unwrap(),
        "2222222222222222"
    );
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..200 {
        let id = fresh_id(&root, new_id).unwrap();
        assert!(id.len() == 16 && seen.insert(id));
    }
}

#[test]
fn an_intent_is_written_durably_and_found_by_sender() {
    let w = World::human("");
    let root = sends_root(&crate::agents::server_dir(&w.ctx, &w.caller()), "$3");
    let i = json!({"id": "1111111111111111", "sender": "aaaaaaaaaaaa", "bodyHash": "h", "recipients": []});
    crate::state::fs::fail_sync_dir(Some(&root.join("aaaaaaaaaaaa")));
    assert!(
        write_intent(&root, "aaaaaaaaaaaa", &i).is_err(),
        "the folder flush is part of the write"
    );
    crate::state::fs::fail_sync_dir(None);
    write_intent(&root, "aaaaaaaaaaaa", &i).unwrap();
    assert!(
        matches!(find_intent(&root, "aaaaaaaaaaaa", "1111111111111111").unwrap(), Found::Mine(v) if v["bodyHash"] == "h")
    );
    assert!(matches!(
        find_intent(&root, "bbbbbbbbbbbb", "1111111111111111").unwrap(),
        Found::Other
    ));
    assert!(matches!(
        find_intent(&root, "aaaaaaaaaaaa", "2222222222222222").unwrap(),
        Found::None
    ));
    mark_done(&root, "aaaaaaaaaaaa", "1111111111111111");
    assert!(
        matches!(find_intent(&root, "aaaaaaaaaaaa", "1111111111111111").unwrap(), Found::Mine(v) if v["done"] == true)
    );
}

/// Two threads, the real team lock, 70 charged sends each: exactly 120 pass.
#[test]
fn concurrent_senders_never_pass_120() {
    let w = World::human("");
    let (locks, state) = (
        w.ctx.state_root.parent().unwrap().join("locks"),
        w.ctx.state_root.clone(),
    );
    let run = move || {
        let mut v = World::human("");
        v.ctx.state_root = state.clone();
        v.fakes.real_locks(&locks);
        v.clock(T0);
        let c = v.caller();
        let root = sends_root(&crate::agents::server_dir(&v.ctx, &c), "$3");
        let key = crate::agents::lock_key("spawn", &c, "$3");
        (0..70)
            .filter(|_| {
                crate::agents::lock(&v.ctx, &key, || {
                    charge(&v.ctx, &root.join("aaaaaaaaaaaa"), true)?;
                    let id = fresh_id(&root, new_id)?;
                    write_intent(
                        &root,
                        "aaaaaaaaaaaa",
                        &json!({"id": id, "at": utc_o(v.ctx.clock.now()), "done": true}),
                    )
                })
                .is_ok()
            })
            .count()
    };
    let (one, two) = (std::thread::spawn(run.clone()), std::thread::spawn(run));
    assert_eq!(one.join().unwrap() + two.join().unwrap(), 120);
}

#[test]
fn a_failed_entry_flush_is_finished_by_the_retry() {
    let w = World::human("");
    let root = sends_root(&crate::agents::server_dir(&w.ctx, &w.caller()), "$3");
    let teams = root.parent().unwrap().parent().unwrap().to_path_buf();
    let i = json!({"id": "1111111111111111", "sender": "aaaaaaaaaaaa"});
    // The uncertain state, built explicitly (finding 8): every folder exists, made by an earlier call whose entry
    // flushes may not have reached the disk. Task 5 flushes only folders it creates, so it flushes nothing here.
    std::fs::create_dir_all(root.join("aaaaaaaaaaaa")).unwrap();
    crate::state::fs::fail_sync_dir(Some(&teams));
    let r = write_intent(&root, "aaaaaaaaaaaa", &i);
    assert!(
        r.is_err(),
        "folders that exist are flushed into their parents: {r:?}"
    );
    let r = write_intent(&root, "aaaaaaaaaaaa", &i);
    assert!(r.is_err(), "the retry flushes teams again: {r:?}");
    crate::state::fs::fail_sync_dir(None);
    write_intent(&root, "aaaaaaaaaaaa", &i).unwrap();
    assert!(
        root.join("aaaaaaaaaaaa")
            .join("1111111111111111.json")
            .is_file()
    );
}

const A: &str = "0000000000a1";
const B: &str = "0000000000b2";
const C: &str = "0000000000c3";

fn m(id: &str, record: Value) -> Member {
    Member {
        id: id.into(),
        name: crate::agents::text(record.get("name")),
        parents: raw_parents(&record),
        record,
    }
}

/// leader > A > B, leader > C.
fn tree3() -> Vec<Member> {
    vec![
        m(LEADER, leader_record()),
        m(A, json!({"name": "alpha", "parentAgent": {"id": LEADER}})),
        m(
            B,
            json!({"name": "beta", "parentAgent": {"id": A}, "parentPane": "%7"}),
        ),
        m(C, json!({"name": "gamma", "parentAgent": {"id": LEADER}})),
    ]
}

fn ids(p: Res<Pick>) -> Vec<String> {
    match p.unwrap() {
        Pick::Members(v) => v.iter().map(|m| m.id.clone()).collect(),
        Pick::Pane(p) => vec![p],
        Pick::Unknown => vec!["unknown".into()],
    }
}

fn why(p: Res<Pick>) -> String {
    let r = p.unwrap_err();
    format!("{} {} {}", r.code, r.check, r.message)
}

#[test]
fn words_resolve_on_the_roster() {
    let r = tree3();
    assert_eq!(ids(pick(&r, Some(A), &Target::Parent)), [LEADER]);
    assert_eq!(
        ids(pick(&r, Some(B), &Target::Parent)),
        [A],
        "parentAgent.id names a running member"
    );
    assert_eq!(ids(pick(&r, Some(B), &Target::Leader)), [LEADER]);
    assert_eq!(ids(pick(&r, Some(A), &Target::Children)), [B]);
    assert_eq!(
        ids(pick(&r, None, &Target::Children)),
        [A, C],
        "a person reaches the children of the leader"
    );
    assert_eq!(
        ids(pick(&r, Some(A), &Target::Team)),
        [LEADER, B, C],
        "everyone but the sender"
    );
    assert_eq!(ids(pick(&r, None, &Target::Team)), [LEADER, A, B, C]);
    assert_eq!(ids(pick(&r, Some(A), &Target::Name("gamma".into()))), [C]);
    assert_eq!(
        ids(pick(&r, Some(A), &Target::Name("nobody".into()))),
        ["unknown"]
    );
    assert_eq!(ids(pick(&r, Some(A), &Target::Pane("%9".into()))), ["%9"]);
    assert_eq!(
        why(pick(&r, None, &Target::Parent)),
        "2 parent only an agent has a parent"
    );
    assert_eq!(
        (
            target("leader").unwrap(),
            target("%3").unwrap(),
            target("beta").unwrap()
        ),
        (
            Target::Leader,
            Target::Pane("%3".into()),
            Target::Name("beta".into())
        )
    );
    for bad in ["Beta", "a b", "%", "x!", ""] {
        assert_eq!(target(bad).unwrap_err().check, "usage", "{bad}");
    }
}

#[test]
fn parent_falls_back_to_the_parent_pane() {
    let mut r = tree3();
    r.retain(|x| x.id != A);
    assert_eq!(
        ids(pick(&r, Some(B), &Target::Parent)),
        ["%7"],
        "no running parent member: parentPane"
    );
    assert_eq!(ids(pick(&r, Some(C), &Target::Parent)), [LEADER]);
}

#[test]
fn empty_targets_refuse() {
    let lone = vec![m(
        A,
        json!({"name": "alpha", "parentAgent": {"id": "ffffffffffff"}, "parentPane": null}),
    )];
    assert_eq!(
        why(pick(&lone, Some(A), &Target::Leader)),
        "2 target this team has no leader"
    );
    assert_eq!(
        why(pick(&lone, Some(A), &Target::Children)),
        "2 target no other member to send to"
    );
    assert_eq!(
        why(pick(&lone, Some(A), &Target::Team)),
        "2 target no other member to send to"
    );
    assert_eq!(
        why(pick(&lone, Some(A), &Target::Parent)),
        "2 parent only an agent has a parent"
    );
    assert_eq!(
        why(pick(&lone, None, &Target::Children)),
        "2 target this team has no leader"
    );
}

#[test]
fn roster_is_running_committed_unnoticed() {
    use crate::agents::lifecycle::Life;
    let s = crate::agents::tree::tests::snap(
        &[
            (LEADER, leader_record(), Life::Running),
            (A, child(A, LEADER, 1), Life::Running),
            (B, child(B, A, 2), Life::Ended),
            (C, child(C, LEADER, 1), Life::Lost),
        ],
        0,
    );
    assert_eq!(
        roster(&s).into_iter().map(|m| m.id).collect::<Vec<_>>(),
        [LEADER, A]
    );
}

fn rows(r: &[&str]) -> Vec<Listed> {
    r.iter().map(|x| row_of(x, &[])).collect()
}

/// Session $3 with a team record: leader > A (ma1) > B (mb2, copilot); pane rows `rows`.
fn team_with(w: World, rows: Vec<Listed>) -> World {
    crate::agents::tree::tests::team_record(&w, &[]);
    cf::member(
        &w,
        LEADER,
        json!({"name": "leader", "depth": 0, "upTeam": "t", "parentAgent": null, "pane": "%4", "window": "@1"}),
    );
    cf::member(
        &w,
        A,
        json!({"name": "ma1", "depth": 1, "parentAgent": {"id": LEADER}, "pane": "%5", "window": "@1"}),
    );
    cf::member(
        &w,
        B,
        json!({"name": "mb2", "depth": 2, "parentAgent": {"id": A}, "client": "copilot", "pane": "%6", "window": "@1"}),
    );
    w.pane_rows(&rows);
    w
}

fn team_world() -> World {
    team_with(World::human(""), Vec::new())
}

/// One snapshot: hc running every hc member of session $3 (gets in address order, as core_facts asks).
fn online(w: &World) {
    let mut records: Vec<Value> = std::fs::read_dir(w.state().join("agents"))
        .unwrap()
        .filter_map(|e| {
            crate::state::fs::read_json(&e.unwrap().path().join("record.json"))
                .ok()
                .flatten()
        })
        .filter(|r| r.get("hcAddress").is_some() && r["session"] == "$3")
        .collect();
    records.sort_by_key(|r| crate::agents::text(r.get("hcAddress")));
    cf::online(w);
    cf::listing(w, &crate::agents::membership::team(&w.caller()), &[]);
    for r in &records {
        cf::get(w, r["hcAddress"].as_str().unwrap(), cf::wire(r));
    }
}

/// `send` as member `member` (hc-proven) or, with None, as the caller the world resolves.
fn go(w: &World, member: Option<&str>, args: &[&str]) -> Res<Value> {
    let mut c = w.caller();
    if member.is_some() {
        c.member = member.map(String::from);
    }
    let got = crate::agents::message::send_as(&w.ctx, &c, &w.request(args));
    // Every queued answer must be used by a send that succeeds; a refusal stops early by design, and the test's own
    // assertion on it is the reported failure (finding 19), not the leftover fixture.
    if got.is_ok() {
        w.fakes.hc_done().unwrap();
    }
    got.map(Value::Object)
}

fn said(e: Refusal) -> String {
    format!("{} {} {}", e.code, e.check, e.message)
}

fn inbox(w: &World, key: &str) -> Vec<Value> {
    std::fs::read_to_string(w.state().join("mail").join(format!("{key}.jsonl")))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn a_sends(w: &World) -> usize {
    std::fs::read_dir(sends_dir(
        &crate::agents::server_dir(&w.ctx, &w.caller()),
        "$3",
        A,
    ))
    .map_or(0, |d| d.count())
}

#[test]
fn a_broadcast_is_one_id_per_mailbox_none_for_the_sender() {
    let w = team_world();
    online(&w);
    let r = go(&w, Some(A), &["send", "team", "-Body", "hi"]);
    assert!(r.is_ok(), "{r:?}");
    let r = r.unwrap();
    let id = r["id"].as_str().unwrap().to_string();
    assert_eq!(
        r["recipients"],
        json!([{"name": "mb2", "id": B}, {"name": "leader", "id": LEADER}])
    );
    for key in [LEADER, B] {
        let got = inbox(&w, key);
        assert_eq!(got.len(), 1, "{key}");
        assert_eq!(
            (
                &got[0]["id"],
                &got[0]["cast"],
                &got[0]["fromBox"],
                &got[0]["from"]
            ),
            (&json!(id), &json!("team"), &json!(A), &json!("ma1"))
        );
    }
    assert!(inbox(&w, A).is_empty(), "none for the sender");
    assert_eq!(a_sends(&w), 1, "a broadcast is one intent");
    cf::member(
        &w,
        C,
        json!({"name": "mc3", "depth": 1, "parentAgent": {"id": LEADER}, "pane": "%8", "window": "@1"}),
    );
    assert!(inbox(&w, C).is_empty(), "a later member gets nothing");
}

#[test]
fn words_route_from_members_and_persons() {
    for (member, word, want) in [
        (Some(A), "parent", LEADER),
        (Some(B), "parent", A),
        (Some(B), "leader", LEADER),
        (Some(A), "children", B),
        (None, "children", A),
        (None, "mb2", B),
        (Some(B), "ma1", A),
    ] {
        let w = team_world();
        online(&w);
        let r = go(&w, member, &["send", word, "-Body", "x"]).unwrap();
        let got: Vec<&str> = r["recipients"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["id"].as_str().unwrap())
            .collect();
        assert_eq!(got, [want], "{word}");
        assert_eq!(inbox(&w, want).len(), 1, "{word}");
    }
}

#[test]
fn another_team_is_refused() {
    let w = team_world();
    cf::member(
        &w,
        C,
        json!({"name": "other", "session": "$4", "pane": "%8"}),
    );
    online(&w);
    assert_eq!(
        said(go(&w, Some(A), &["send", "other", "-Body", "x"]).unwrap_err()),
        "2 target no agent of this team has that name"
    );
    online(&w);
    let person = said(go(&w, None, &["send", "other", "-Body", "x"]).unwrap_err());
    assert!(person.starts_with("2 target "), "{person}");
    assert!(inbox(&w, C).is_empty());
}

#[test]
fn without_a_team_record_the_session_members_are_the_team() {
    let w = team_world();
    std::fs::remove_file(crate::agents::team::path(
        &crate::agents::server_dir(&w.ctx, &w.caller()),
        "$3",
    ))
    .unwrap();
    online(&w);
    assert_eq!(
        go(&w, Some(A), &["send", "team", "-Body", "x"]).unwrap()["recipients"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn a_person_reaches_a_pane_only_agent() {
    let w = team_with(World::human(""), rows(&["%8|@5|300|0000000000d4|pw"]));
    w.agent_record(
        "0000000000d4",
        json!({"name": "pw", "pane": "%8", "window": "@5"}),
    );
    w.recipients_live();
    online(&w);
    assert_eq!(
        go(&w, None, &["send", "pw", "-Body", "hi"]).unwrap()["pane"],
        "%8",
        "the 0.2 reply"
    );
    assert_eq!(inbox(&w, "0000000000d4").len(), 1);
}

#[test]
fn a_non_member_agent_pane_keeps_0_2() {
    let w = team_with(
        World::agent("0000000000d4"),
        rows(&["%1|@1|100|0000000000d4|pw", "%4|@1|101||"]),
    );
    w.agent_record(
        "0000000000d4",
        json!({"name": "pw", "pane": "%1", "parentPane": "%4"}),
    );
    w.recipients_live();
    w.file("b.md", b"done");
    // A 0.2 caller is decided from its own record: no hc look (QA fix 3).
    assert_eq!(
        go(&w, None, &["send", "parent", "-BodyFile", "b.md"]).unwrap()["pane"],
        "%4",
        "the 0.2 reply"
    );
    assert_eq!(inbox(&w, "pane-4").len(), 1);
    // QA fix 2: the 0.2 path has no intents and no replies, so it refuses -Id and -Re before anything is written.
    for args in [
        &[
            "send",
            "parent",
            "-BodyFile",
            "b.md",
            "-Id",
            "ffffffffffffffff",
        ][..],
        &["send", "%4", "-BodyFile", "b.md", "-Re", "ffffffffffffffff"][..],
    ] {
        let r = go(&w, None, args);
        assert!(r.is_err(), "{args:?} on the 0.2 path: {r:?}");
        assert_eq!(
            said(r.unwrap_err()),
            "2 usage -Id and -Re work only inside a team",
            "{args:?}"
        );
    }
    assert_eq!(inbox(&w, "pane-4").len(), 1, "nothing appended");
}

#[test]
fn a_pane_proven_member_routes_through_the_team() {
    let mut row = row_of("%1|@1|100|0000000000a1|ma1", &[]);
    row.command = format!("hc attach t{A}");
    let w = team_with(World::agent(A), vec![row]);
    cf::member(
        &w,
        A,
        json!({"name": "ma1", "depth": 1, "parentAgent": {"id": LEADER}, "pane": "%1", "window": "@1"}),
    );
    w.file("b.md", b"hi");
    online(&w);
    let r = go(&w, None, &["send", "leader", "-BodyFile", "b.md"]).unwrap();
    assert_eq!(
        (&r["from"], &r["recipients"][0]["id"]),
        (&json!("ma1"), &json!(LEADER))
    );
    assert_eq!(inbox(&w, LEADER)[0]["fromBox"], A);
    // QA fix 1 (round 2): a second record claiming the pane makes its owner ambiguous, whatever claims it first.
    let mut row = row_of("%1|@1|100|0000000000a1|ma1", &[]);
    row.command = format!("hc attach t{A}");
    let w = team_with(World::agent(A), vec![row]);
    cf::member(
        &w,
        A,
        json!({"name": "ma1", "depth": 1, "parentAgent": {"id": LEADER}, "pane": "%1", "window": "@1"}),
    );
    w.uncommitted("0000000000d4", json!({"name": "pw", "pane": "%1"}));
    w.file("b.md", b"hi");
    online(&w);
    let r = go(&w, None, &["send", "leader", "-BodyFile", "b.md"]);
    assert!(r.is_err(), "two records claim the pane: {r:?}");
    assert_eq!(
        said(r.unwrap_err()),
        "2 tagged this caller cannot be proven a member"
    );
    assert!(inbox(&w, LEADER).is_empty());
}

#[test]
fn a_failed_send_is_finished_by_its_id_to_its_original_recipients() {
    let w = team_world();
    online(&w);
    let leader = w.state().join("mail").join(format!("{LEADER}.jsonl"));
    std::fs::create_dir_all(leader.parent().unwrap()).unwrap();
    crate::state::fs::fail_sync_file(Some(&leader));
    let e = go(&w, Some(A), &["send", "team", "-Body", "hi"]).unwrap_err();
    crate::state::fs::fail_sync_file(None);
    assert_eq!(
        (e.code, e.check, e.details["delivered"].clone()),
        (1, "mail", json!(["mb2"])),
        "B is first by id"
    );
    let id = e.details["id"].as_str().unwrap().to_string();
    assert_eq!(
        e.message,
        format!("the message could not be saved; retry with -Id {id}")
    );
    cf::member(
        &w,
        C,
        json!({"name": "mc3", "depth": 1, "parentAgent": {"id": LEADER}, "pane": "%8", "window": "@1"}),
    );
    online(&w);
    assert_eq!(
        go(&w, Some(A), &["send", "team", "-Body", "hi", "-Id", &id]).unwrap()["id"],
        id.as_str()
    );
    assert_eq!(
        (
            inbox(&w, LEADER).len(),
            inbox(&w, B).len(),
            inbox(&w, C).len()
        ),
        (1, 1, 0),
        "one copy each, originals only"
    );
    assert_eq!(a_sends(&w), 1, "no new intent");
}

#[test]
fn an_id_must_be_this_send() {
    let w = team_world();
    online(&w);
    let id = go(&w, Some(A), &["send", "leader", "-Body", "hi"]).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let cases = [
        (
            A,
            "changed",
            id.clone(),
            "2 id that id belongs to a different send",
        ),
        (
            B,
            "hi",
            id.clone(),
            "2 id that id belongs to a different send",
        ),
        (
            A,
            "hi",
            "ffffffffffffffff".to_string(),
            "2 id no send with that id",
        ),
    ];
    for (who, body, idv, want) in cases {
        online(&w);
        assert_eq!(
            said(
                go(
                    &w,
                    Some(who),
                    &["send", "leader", "-Body", body, "-Id", &idv]
                )
                .unwrap_err()
            ),
            want,
            "{who} {body}"
        );
    }
    for bad in ["xyz", "ABCDEF12", "1234567", "g0000000"] {
        assert_eq!(
            go(&w, Some(A), &["send", "leader", "-Body", "x", "-Id", bad])
                .unwrap_err()
                .check,
            "usage",
            "{bad}"
        );
    }
    assert_eq!(inbox(&w, LEADER).len(), 1);
}

#[test]
fn an_uncertain_intent_returns_its_id_and_the_retry_settles_it() {
    let w = team_world();
    online(&w);
    let dir = sends_dir(&crate::agents::server_dir(&w.ctx, &w.caller()), "$3", A);
    std::fs::create_dir_all(&dir).unwrap();
    crate::state::fs::fail_sync_dir(Some(&dir));
    let e = go(&w, Some(A), &["send", "leader", "-Body", "hi"]);
    assert!(e.is_err(), "an unflushed intent is no send yet");
    let e = e.unwrap_err();
    assert_eq!((e.code, e.check), (1, "mail"), "{}", e.message);
    let id = e.details["id"].as_str().unwrap().to_string();
    assert_eq!(
        e.message,
        format!("the message could not be saved; retry with -Id {id}")
    );
    assert!(
        inbox(&w, LEADER).is_empty(),
        "nothing delivered before the intent is durable"
    );
    online(&w);
    assert_eq!(
        go(&w, Some(A), &["send", "leader", "-Body", "hi", "-Id", &id])
            .unwrap_err()
            .check,
        "mail",
        "the retry flushes again"
    );
    crate::state::fs::fail_sync_dir(None);
    online(&w);
    assert_eq!(
        go(&w, Some(A), &["send", "leader", "-Body", "hi", "-Id", &id]).unwrap()["id"],
        id.as_str()
    );
    assert_eq!((inbox(&w, LEADER).len(), a_sends(&w)), (1, 1));
}

/// A message from A in the mailbox of B at `hop`.
fn seed(w: &World, id: &str, hop: u64, cast: Option<&str>) {
    crate::agents::mail::deliver_once(
        &w.ctx,
        &w.caller(),
        B,
        &json!({"id": id, "from": "ma1", "pane": "%5",
        "at": "x", "body": "q", "fromBox": A, "re": null, "hop": hop, "cast": cast}),
    )
    .unwrap();
}

#[test]
fn replies_go_to_the_sender_until_hop_4() {
    for hop in 0..3u64 {
        let w = team_world();
        seed(&w, "1111111111111111", hop, None);
        online(&w);
        let r = go(
            &w,
            Some(B),
            &["send", "parent", "-Body", "a", "-Re", "1111111111111111"],
        )
        .unwrap();
        assert_eq!(
            (&r["hop"], &r["re"]),
            (&json!(hop + 1), &json!("1111111111111111")),
            "hop {hop}"
        );
        assert_eq!(inbox(&w, A)[0]["hop"], hop + 1);
    }
    let w = team_world();
    seed(&w, "1111111111111111", 3, None);
    online(&w);
    assert_eq!(
        said(
            go(
                &w,
                Some(B),
                &["send", "ma1", "-Body", "a", "-Re", "1111111111111111"]
            )
            .unwrap_err()
        ),
        "2 hop this reply would be hop 4; the thread is closed"
    );
    let w = team_world();
    seed(&w, "1111111111111111", 0, Some("team"));
    let not = "2 re a reply goes only to the message's sender";
    for word in ["team", "children", "%5"] {
        online(&w); // routing takes its membership snapshot before it refuses a reply target (cross-family audit)
        assert_eq!(
            said(
                go(
                    &w,
                    Some(B),
                    &["send", word, "-Body", "a", "-Re", "1111111111111111"]
                )
                .unwrap_err()
            ),
            not,
            "{word}"
        );
    }
    online(&w);
    assert_eq!(
        said(
            go(
                &w,
                Some(B),
                &["send", "leader", "-Body", "a", "-Re", "1111111111111111"]
            )
            .unwrap_err()
        ),
        not
    );
    online(&w);
    assert_eq!(
        said(
            go(
                &w,
                Some(B),
                &["send", "ma1", "-Body", "a", "-Re", "2222222222222222"]
            )
            .unwrap_err()
        ),
        "2 re no message with that id in your mailbox"
    );
    assert_eq!(
        go(&w, Some(B), &["send", "ma1", "-Body", "a", "-Re", "zz"])
            .unwrap_err()
            .check,
        "usage"
    );
    assert_eq!(
        go(
            &w,
            Some(B),
            &[
                "send",
                "ma1",
                "-Body",
                "a",
                "-Re",
                "1111111111111111",
                "-Id",
                "1111111111111111"
            ]
        )
        .unwrap_err()
        .check,
        "usage"
    );
}

#[test]
fn a_reply_reaches_a_since_resumed_sender() {
    let w = team_world();
    seed(&w, "1111111111111111", 0, None);
    // A2 resumed A: same name, inherits the mailbox of A; A is superseded.
    cf::member(
        &w,
        "0000000000a9",
        json!({"name": "ma1", "depth": 1, "parentAgent": {"id": LEADER}, "pane": "%9",
        "window": "@1", "resumedFrom": A, "mailbox": A}),
    );
    online(&w);
    let r = go(
        &w,
        Some(B),
        &["send", "ma1", "-Body", "a", "-Re", "1111111111111111"],
    )
    .unwrap();
    assert_eq!(r["recipients"][0]["id"], "0000000000a9");
    assert_eq!(inbox(&w, A).len(), 1, "the inherited mailbox of A");
}

#[test]
fn a_reply_reads_its_message_inside_the_sender_transaction() {
    let w = team_world();
    seed(&w, "1111111111111111", 0, None);
    w.fakes
        .real_locks(&w.ctx.state_root.parent().unwrap().join("locks"));
    online(&w);
    w.fakes.clear_log();
    go(
        &w,
        Some(B),
        &["send", "ma1", "-Body", "a", "-Re", "1111111111111111"],
    )
    .unwrap();
    let c = w.caller();
    let keys = [
        crate::agents::lock_key("spawn", &c, "$3"),
        crate::agents::lock_key("agent", &c, B),
        crate::agents::lock_key("mail", &c, B),
        crate::agents::lock_key("mail", &c, A),
    ];
    let got: Vec<String> = w
        .fakes
        .locks()
        .into_iter()
        .filter(|e| keys.iter().any(|k| e[1..] == *k))
        .collect();
    let (s, g, mb, ma) = (&keys[0], &keys[1], &keys[2], &keys[3]);
    assert_eq!(
        got,
        [
            format!("+{s}"),
            format!("+{g}"),
            format!("+{mb}"),
            format!("-{mb}"),
            format!("+{ma}"),
            format!("-{ma}"),
            format!("-{g}"),
            format!("-{s}")
        ],
        "the -Re lookup holds the agent lock of the sender"
    );
}

/// M3 as written: `parent` is `parentAgent.id` when that member runs, else `parentPane`.
#[test]
fn a_member_answers_a_person_when_parent_falls_back_to_its_pane() {
    let w = team_with(World::human(""), rows(&["%1|@1|100||"]));
    w.recipients_live();
    cf::member(
        &w,
        C,
        json!({"name": "mc3", "depth": 1, "parentAgent": {"id": "ffffffffffff"}, "parentPane": "%1",
        "pane": "%8", "window": "@1"}),
    );
    crate::agents::mail::deliver_once(&w.ctx, &w.caller(), C, &json!({"id": "1111111111111111", "from": "root",
        "pane": "%1", "at": "x", "body": "q", "fromBox": "pane-1", "re": null, "hop": 0, "cast": null})).unwrap();
    online(&w);
    let r = go(
        &w,
        Some(C),
        &["send", "parent", "-Body", "a", "-Re", "1111111111111111"],
    );
    assert!(
        r.is_ok(),
        "reachable: the parent member does not run, parentPane is the person: {r:?}"
    );
    assert_eq!(r.unwrap()["hop"], 1);
    assert_eq!(inbox(&w, "pane-1").len(), 1);
    // Unreachable: B has a running parent member (A); parent names A, never the person.
    crate::agents::mail::deliver_once(&w.ctx, &w.caller(), B, &json!({"id": "2222222222222222", "from": "root",
        "pane": "%1", "at": "x", "body": "q", "fromBox": "pane-1", "re": null, "hop": 0, "cast": null})).unwrap();
    online(&w);
    assert_eq!(
        said(
            go(
                &w,
                Some(B),
                &["send", "parent", "-Body", "a", "-Re", "2222222222222222"]
            )
            .unwrap_err()
        ),
        "2 re a reply goes only to the message's sender"
    );
}

/// 120 intents of `sender` at T0.
fn fill(w: &World, sender: &str) {
    let dir = sends_dir(
        &crate::agents::server_dir(&w.ctx, &w.caller()),
        "$3",
        sender,
    );
    std::fs::create_dir_all(&dir).unwrap();
    for n in 0..120u64 {
        crate::state::fs::write_json(
            &dir.join(format!("{n:016x}.json")),
            &json!({"id": format!("{n:016x}"), "at": "2026-03-01T00:00:00.0000000Z", "done": true}),
        )
        .unwrap();
    }
}

#[test]
fn the_121st_send_refuses_and_persons_are_exempt() {
    let w = team_world();
    w.clock("2026-03-01T00:30:00Z");
    fill(&w, A);
    online(&w);
    let e = go(&w, Some(A), &["send", "leader", "-Body", "x"]);
    assert!(e.is_err(), "the 121st refuses");
    let e = e.unwrap_err();
    assert_eq!(
        (e.code, e.check, e.details["retryAt"].clone()),
        (2, "quota", json!("2026-03-01T01:00:00.0000000Z"))
    );
    assert!(
        inbox(&w, LEADER).is_empty(),
        "a refused send delivers nothing"
    );
    fill(&w, "pane-1");
    online(&w);
    go(&w, None, &["send", "leader", "-Body", "x"]).unwrap();
    assert_eq!(inbox(&w, LEADER).len(), 1, "persons are exempt");
}

fn only_id(w: &World) -> String {
    let d = sends_dir(&crate::agents::server_dir(&w.ctx, &w.caller()), "$3", A);
    let n = std::fs::read_dir(d)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name();
    n.to_string_lossy().trim_end_matches(".json").to_string()
}

#[test]
fn a_crash_boundary_after_the_intent_counts_it_and_the_retry_adds_none() {
    let w = team_world();
    online(&w);
    crash_after_intent(true);
    let e = go(&w, Some(A), &["send", "team", "-Body", "hi"]);
    assert!(e.is_err(), "the crash hook stops the send");
    let e = e.unwrap_err();
    crash_after_intent(false);
    assert_eq!((e.code, e.check), (1, "internal"));
    assert_eq!(
        (a_sends(&w), inbox(&w, LEADER).len(), inbox(&w, B).len()),
        (1, 0, 0),
        "the intent is the charge"
    );
    let id = only_id(&w);
    online(&w);
    go(&w, Some(A), &["send", "team", "-Body", "hi", "-Id", &id]).unwrap();
    assert_eq!(
        (a_sends(&w), inbox(&w, LEADER).len(), inbox(&w, B).len()),
        (1, 1, 1)
    );
}

#[test]
fn a_failed_folder_flush_is_finished_by_a_duplicate_hit() {
    let w = team_world();
    online(&w);
    let mail = w.state().join("mail");
    std::fs::create_dir_all(&mail).unwrap();
    crate::state::fs::fail_sync_dir(Some(&mail));
    let e = go(&w, Some(A), &["send", "leader", "-Body", "hi"]).unwrap_err();
    assert_eq!((e.code, e.check), (1, "mail"));
    let id = e.details["id"].as_str().unwrap().to_string();
    assert_eq!(inbox(&w, LEADER).len(), 1, "appended, folder not flushed");
    online(&w);
    assert_eq!(
        go(&w, Some(A), &["send", "leader", "-Body", "hi", "-Id", &id])
            .unwrap_err()
            .check,
        "mail",
        "the duplicate hit flushes again"
    );
    crate::state::fs::fail_sync_dir(None);
    online(&w);
    go(&w, Some(A), &["send", "leader", "-Body", "hi", "-Id", &id]).unwrap();
    assert_eq!(
        inbox(&w, LEADER).len(),
        1,
        "one copy, flushed: what a crash at this boundary now keeps"
    );
}

#[test]
fn an_unfinished_send_past_a_day_stays_and_its_id_finishes_it() {
    let w = team_world();
    w.clock("2026-03-01T00:00:00Z");
    online(&w);
    let leader = w.state().join("mail").join(format!("{LEADER}.jsonl"));
    std::fs::create_dir_all(leader.parent().unwrap()).unwrap();
    crate::state::fs::fail_sync_file(Some(&leader));
    let id = go(&w, Some(A), &["send", "team", "-Body", "hi"])
        .unwrap_err()
        .details["id"]
        .as_str()
        .unwrap()
        .to_string();
    crate::state::fs::fail_sync_file(None);
    w.fakes.advance(std::time::Duration::from_secs(2 * 86_400));
    online(&w);
    go(&w, Some(A), &["send", "mb2", "-Body", "other"]).unwrap();
    assert_eq!(a_sends(&w), 2, "pruning ran; the unfinished intent stays");
    online(&w);
    go(&w, Some(A), &["send", "team", "-Body", "hi", "-Id", &id]).unwrap();
    assert_eq!(
        (inbox(&w, LEADER).len(), inbox(&w, B).len()),
        (1, 2),
        "no duplicate; the original recipients"
    );
}

#[test]
fn dead_or_foreign_panes_refuse() {
    for out in ["%9|$4\n", ""] {
        let w = team_world();
        w.fakes.mux_first("#{pane_id}|#{session_id}", 0, out, "");
        online(&w);
        assert_eq!(
            said(go(&w, Some(A), &["send", "%9", "-Body", "x"]).unwrap_err()),
            "2 target the recipient is not a live pane of this session",
            "{out}"
        );
    }
}

#[test]
fn locks_are_team_then_agent_then_each_mailbox() {
    let w = team_world();
    w.fakes
        .real_locks(&w.ctx.state_root.parent().unwrap().join("locks"));
    online(&w);
    w.fakes.clear_log();
    go(&w, Some(A), &["send", "team", "-Body", "x"]).unwrap();
    let c = w.caller();
    let keys = [
        crate::agents::lock_key("spawn", &c, "$3"),
        crate::agents::lock_key("agent", &c, A),
        crate::agents::lock_key("mail", &c, B),
        crate::agents::lock_key("mail", &c, LEADER),
    ];
    let got: Vec<String> = w
        .fakes
        .locks()
        .into_iter()
        .filter(|e| keys.iter().any(|k| e[1..] == *k))
        .collect();
    let (s, a, b, l) = (&keys[0], &keys[1], &keys[2], &keys[3]);
    assert_eq!(
        got,
        [
            format!("+{s}"),
            format!("+{a}"),
            format!("+{b}"),
            format!("-{b}"),
            format!("+{l}"),
            format!("-{l}"),
            format!("-{a}"),
            format!("-{s}")
        ],
        "team, agent, each mailbox; never reversed"
    );
}

#[test]
fn the_reply_json_stored_fields_last_sent_and_wait_cleanup() {
    let w = team_world();
    online(&w);
    crate::agents::fixture::write(
        &w.folder(A).join("waiting-77.json"),
        json!({"pid": 77, "start": 1, "since": "x"}),
    );
    let r = go(&w, Some(A), &["send", "leader", "-Body", "hello"]).unwrap();
    assert_eq!(
        r.as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["to", "id", "from", "recipients", "hop", "re", "toSessionId"]
    );
    assert_eq!(
        (&r["to"], &r["hop"], &r["re"]),
        (&json!("leader"), &json!(0), &Value::Null)
    );
    let stored = &inbox(&w, LEADER)[0];
    assert_eq!(
        stored
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        // A member sender's line also names it (`fromId`, c723913): the side bar counts unread mail by identity.
        [
            "id", "from", "fromId", "pane", "at", "body", "fromBox", "re", "hop", "cast"
        ]
    );
    assert_eq!(
        crate::state::fs::read_json(&w.folder(A).join("last-sent.json"))
            .unwrap()
            .unwrap()["line"],
        "hello"
    );
    assert!(
        !w.folder(A).join("waiting-77.json").exists(),
        "a send ends the wait"
    );
    online(&w);
    assert!(
        go(&w, Some(A), &["send", "mb2", "-Body", "x"])
            .unwrap()
            .get("toSessionId")
            .is_none(),
        "copilot: none"
    );
}

/// QA fix 1: a pane an uncommitted agent record claims is no person; in a team it refuses 2 `tagged`.
#[test]
fn an_unproven_pane_in_a_team_is_refused_tagged() {
    let w = team_world();
    w.uncommitted("0000000000d4", json!({"name": "pw", "pane": "%1"}));
    online(&w);
    let r = go(&w, None, &["send", "leader", "-Body", "x"]);
    assert!(r.is_err(), "an unproven pane sends as no one: {r:?}");
    assert_eq!(
        said(r.unwrap_err()),
        "2 tagged this caller cannot be proven a member"
    );
    assert!(inbox(&w, LEADER).is_empty());
}

/// QA fix 1: a pane a committed 0.2 agent record claims keeps that agent's identity and 0.2 send.
#[test]
fn a_committed_0_2_agent_pane_sends_as_itself() {
    let w = team_with(World::human(""), rows(&["%1|@1|100||"]));
    w.agent_record(
        "0000000000d4",
        json!({"name": "pw", "pane": "%1", "window": "@1"}),
    );
    w.recipients_live();
    online(&w);
    let r = go(&w, None, &["send", "leader", "-Body", "x"]);
    assert!(r.is_ok(), "{r:?}");
    let r = r.unwrap();
    assert_eq!(
        (&r["from"], &r["pane"]),
        (&json!("pw"), &json!("%4")),
        "the 0.2 reply, as the agent: {r}"
    );
    assert_eq!(inbox(&w, LEADER)[0]["from"], "pw");
    // QA fix 2 (round 2): a record whose own id is not its folder's proves no one; never the person path.
    // Round 3: its id must be that string, as `registry::own_record` reads it; an array holding it is not.
    for id in [json!("0000000000e5"), json!(["0000000000d4"])] {
        let w = team_with(World::human(""), rows(&["%1|@1|100||"]));
        w.agent_record(
            "0000000000d4",
            json!({"id": id, "name": "pw", "pane": "%1", "window": "@1"}),
        );
        w.recipients_live();
        online(&w);
        let r = go(&w, None, &["send", "leader", "-Body", "x"]);
        assert!(r.is_err(), "{id}: an invalid record sends as no one: {r:?}");
        assert_eq!(
            said(r.unwrap_err()),
            "2 tagged this caller cannot be proven a member",
            "{id}"
        );
        assert!(inbox(&w, LEADER).is_empty(), "{id}: never as root");
    }
}

/// QA fix 3: whether a caller is 0.2 is decided from the caller, so a 0.2 send never needs hc.
#[test]
fn a_non_member_agent_pane_keeps_0_2_while_hc_is_down() {
    let w = team_with(
        World::agent("0000000000d4"),
        rows(&["%1|@1|100|0000000000d4|pw", "%4|@1|101||"]),
    );
    w.agent_record(
        "0000000000d4",
        json!({"name": "pw", "pane": "%1", "parentPane": "%4"}),
    );
    w.recipients_live();
    w.file("b.md", b"done");
    // No `online`: hc is not installed.
    let r = go(&w, None, &["send", "parent", "-BodyFile", "b.md"]);
    assert!(r.is_ok(), "the 0.2 send needs no hc: {r:?}");
    assert_eq!(r.unwrap()["pane"], "%4", "the 0.2 reply");
    assert_eq!(inbox(&w, "pane-4").len(), 1);
}

/// One snapshot as `online`, with member `id`'s terminal unreadable, Lost, Ended or at another generation.
fn online_but(w: &World, id: &str, case: &str) {
    let mut records: Vec<Value> = std::fs::read_dir(w.state().join("agents"))
        .unwrap()
        .filter_map(|e| {
            crate::state::fs::read_json(&e.unwrap().path().join("record.json"))
                .ok()
                .flatten()
        })
        .filter(|r| r.get("hcAddress").is_some() && r["session"] == "$3")
        .collect();
    records.sort_by_key(|r| crate::agents::text(r.get("hcAddress")));
    cf::online(w);
    cf::listing(w, &crate::agents::membership::team(&w.caller()), &[]);
    for r in &records {
        let address = r["hcAddress"].as_str().unwrap();
        let mine = r["id"] == id;
        if mine && case == "unreadable" {
            w.fakes.hc_expect(
                &["get", address, "--json"],
                Err(Refusal::internal("get failed")),
            );
            continue;
        }
        let mut t = cf::wire(r);
        if mine {
            match case {
                "lost" => t["argv"] = json!(["/bin/sh"]),
                "ended" => {
                    t["state"] = json!("finished");
                    t["reason"] = json!("exited");
                    t["exit_code"] = json!(0);
                }
                "generation" => t["labels"]["mux.gen"] = json!("1"),
                _ => {}
            }
        }
        cf::get(w, address, t);
    }
}

/// Tech Lead (HIGH): a pane an hc member record claims never falls back to 0.2; a failed proof refuses 2 `tagged`
/// and writes nothing: no mail, no intent, no sender state.
#[test]
fn a_claimed_member_whose_proof_fails_never_falls_back_to_0_2() {
    for case in ["unreadable", "lost", "ended", "generation"] {
        let mut row = row_of("%1|@1|100|0000000000a1|ma1", &[]);
        row.command = format!("hc attach t{A}");
        let w = team_with(World::agent(A), vec![row, row_of("%4|@1|101||", &[])]);
        cf::member(
            &w,
            A,
            json!({"name": "ma1", "depth": 1, "parentAgent": {"id": LEADER}, "pane": "%1",
                "parentPane": "%4", "window": "@1"}),
        );
        w.recipients_live();
        w.file("b.md", b"hi");
        online_but(&w, A, case);
        let r = go(&w, None, &["send", "parent", "-BodyFile", "b.md"]);
        assert!(
            r.is_err(),
            "{case}: an unproven member sends as no one: {r:?}"
        );
        assert_eq!(
            said(r.unwrap_err()),
            "2 tagged this caller cannot be proven a member",
            "{case}"
        );
        for key in [LEADER, "pane-4", "pane-1"] {
            assert!(inbox(&w, key).is_empty(), "{case}: no mail in {key}");
        }
        assert_eq!(a_sends(&w), 0, "{case}: no intent");
        assert!(
            !w.folder(A).join("last-sent.json").exists(),
            "{case}: the sender's state is unchanged"
        );
    }
}

/// The side bar counts a member's unread mail by identity (c723913): a member's team message names it (`fromId`), as
/// its 0.2 message does; a person's names none.
#[test]
fn a_team_message_names_its_member_id() {
    let w = team_world();
    online(&w);
    go(&w, Some(A), &["send", "leader", "-Body", "x"]).unwrap();
    assert_eq!(inbox(&w, LEADER)[0]["fromId"], A, "a member names itself");
    online(&w);
    go(&w, None, &["send", "leader", "-Body", "y"]).unwrap();
    assert!(
        inbox(&w, LEADER)[1].get("fromId").is_none(),
        "a person names no member"
    );
}

#[test]
fn notices_are_opt_in_and_one_per_recipient() {
    let w = team_world();
    w.fakes.mux_rule("set-option", 0, "", "");
    let r = crate::agents::message::notify_as(
        &w.ctx,
        &w.caller(),
        &w.request(&["notify", "-On"]),
        &mut |_: &str| {},
    )
    .unwrap();
    assert_eq!(r["warning"], crate::agents::message::DRAFT_WARNING);
    assert!(w.fakes.hc_calls().is_empty(), "-On sends nothing");
    assert!(
        w.fakes
            .mux_calls()
            .iter()
            .any(|c| c.iter().any(|a| a == "@mux-agents-notify-send"))
    );
    w.fakes.mux_first("show-options", 0, "on\n", "");
    online(&w);
    cf::online(&w);
    for t in [B, LEADER] {
        cf::answer(
            &w,
            &[
                "send",
                &format!("t{t}"),
                "mail from ma1: run mux-agent read",
                "--json",
            ],
            "{}",
        );
    }
    go(&w, Some(A), &["send", "team", "-Body", "x"]).unwrap();
    w.fakes.mux_first("show-options", 0, "off\n", "");
    online(&w);
    go(&w, Some(A), &["send", "team", "-Body", "y"]).unwrap();
    let mut c = w.caller();
    c.member = Some(A.into());
    assert_eq!(
        crate::agents::message::notify_as(
            &w.ctx,
            &c,
            &w.request(&["notify", "-Off"]),
            &mut |_: &str| {}
        )
        .unwrap_err()
        .check,
        "authority"
    );
}

#[test]
fn the_warning_comes_before_any_notice_even_with_a_concurrent_sender() {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering::SeqCst},
    };
    let w = team_world();
    let locks = w.ctx.state_root.parent().unwrap().join("locks");
    w.fakes.real_locks(&locks);
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let on = Arc::new(AtomicBool::new(false));
    let (l, o) = (log.clone(), on.clone());
    w.fakes.mux_script("set-option", move |_| {
        l.lock().unwrap().push("set".into());
        o.store(true, SeqCst);
        Ok(crate::mux::Output {
            code: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    });
    let state = w.ctx.state_root.clone();
    let mut sender = None;
    let (l, o) = (log.clone(), on.clone());
    let mut warn = |text: &str| {
        l.lock().unwrap().push(format!("warn {text}"));
        let (l, o, state, locks) = (l.clone(), o.clone(), state.clone(), locks.clone());
        sender = Some(std::thread::spawn(move || {
            sender_thread(state, locks, l, o)
        }));
        // The sender now waits on the team lock that notify holds.
        std::thread::sleep(std::time::Duration::from_millis(300));
    };
    crate::agents::message::notify_as(
        &w.ctx,
        &w.caller(),
        &w.request(&["notify", "-On"]),
        &mut warn,
    )
    .unwrap();
    sender.take().unwrap().join().unwrap();
    let mut want = vec![format!("warn {}", crate::agents::message::DRAFT_WARNING)];
    want.extend(["set", "notice", "notice"].map(String::from));
    assert_eq!(
        *log.lock().unwrap(),
        want,
        "the warning is printed before notices can be on"
    );
}

/// A concurrent sender on the same state and real locks: it sends to `team` once notify releases the team lock.
fn sender_thread(
    state: PathBuf,
    locks: PathBuf,
    log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    on: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let mut v = World::human("");
    v.ctx.state_root = state;
    v.fakes.real_locks(&locks);
    v.fakes.mux_script("show-options", move |_| {
        let now = if on.load(std::sync::atomic::Ordering::SeqCst) {
            "on"
        } else {
            "off"
        };
        Ok(crate::mux::Output {
            code: 0,
            stdout: format!("{now}\n"),
            stderr: String::new(),
        })
    });
    let l = log.clone();
    v.fakes.hc_probe(move |argv| {
        if argv.first().map(String::as_str) == Some("send") {
            l.lock().unwrap().push("notice".into());
        }
    });
    online(&v);
    cf::online(&v);
    for t in [B, LEADER] {
        cf::answer(
            &v,
            &[
                "send",
                &format!("t{t}"),
                "mail from ma1: run mux-agent read",
                "--json",
            ],
            "{}",
        );
    }
    go(&v, Some(A), &["send", "team", "-Body", "x"]).unwrap();
}

#[test]
fn a_failed_set_option_refuses_on_and_off() {
    for flag in ["-On", "-Off"] {
        let w = team_world();
        w.fakes.mux_rule("set-option", 1, "", "no server running");
        let r = crate::agents::message::notify_as(
            &w.ctx,
            &w.caller(),
            &w.request(&["notify", flag]),
            &mut |_: &str| {},
        );
        assert!(r.is_err(), "{flag}: {r:?}");
        assert_eq!(r.unwrap_err().check, "notify", "{flag}");
    }
}

// Call 19 (Tech Lead): a notice names each recipient's address from the classified snapshot (R1, R5); a
// record.json rewritten after the snapshot is never reread.
#[test]
fn notices_take_addresses_from_the_snapshot() {
    let w = team_world();
    online(&w);
    let b = w.folder(B).join("record.json");
    w.fakes
        .mux_script(crate::agents::message::NOTIFY_OPTION, move |_| {
            let mut r = crate::state::fs::read_json(&b).unwrap().unwrap();
            r["hcAddress"] = json!("moved");
            crate::state::fs::write_json(&b, &r).unwrap();
            Ok(crate::mux::Output {
                code: 0,
                stdout: "on\n".into(),
                stderr: String::new(),
            })
        });
    cf::online(&w);
    for address in [format!("t{B}"), format!("t{LEADER}")] {
        cf::answer(
            &w,
            &[
                "send",
                &address,
                "mail from ma1: run mux-agent read",
                "--json",
            ],
            "{}",
        );
    }
    let mut c = w.caller();
    c.member = Some(A.into());
    let r =
        crate::agents::message::send_as(&w.ctx, &c, &w.request(&["send", "team", "-Body", "hi"]));
    assert!(r.is_ok(), "{r:?}");
    let sent: Vec<String> = w
        .fakes
        .hc_calls()
        .into_iter()
        .filter(|(_, a, _)| a[0] == "send")
        .map(|(_, a, _)| a[1].clone())
        .collect();
    assert_eq!(
        sent,
        [format!("t{B}"), format!("t{LEADER}")],
        "the snapshot's addresses"
    );
    w.fakes.hc_done().unwrap();
}

// A3a calls 5, 8; PM calls 2, 7: a lifecycle member claims the pane its view presents; its subject commit comes from
// one status snapshot (P4), so a torn commit or record refuses instead of passing the pane to a person.
#[test]
fn a_lifecycle_member_claims_the_pane_its_view_presents() {
    use crate::agents::fixture::{ID, SESSION_ID};
    use crate::agents::gens;
    let w = World::human("");
    let caller = w.caller();
    let f = w.folder(ID);
    std::fs::create_dir_all(&f).unwrap();
    let team = crate::agents::membership::team(&caller);
    let r = json!({"lifecycle": 1, "id": ID, "name": "worker", "session": caller.session, "team": team,
        "generation": "0", "hcAddress": "t1", "hcName": "m0123abcd", "launcherArgv": ["x"],
        "sessionId": SESSION_ID});
    crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
    let launch = json!({"hcName": "m0123abcd", "launcherArgv": ["x"], "sessionId": SESSION_ID});
    crate::state::evidence::publish(&gens::path(&f, 0, "launch"), &launch).unwrap();
    crate::state::evidence::publish(&gens::path(&f, 0, "commit"), &json!({"committed": true}))
        .unwrap();
    let view = json!({"serverKey": caller.server_key, "session": caller.session, "pane": caller.pane,
        "window": "@1", "startCommand": "hc attach t1", "gen": 0, "hcAddress": "t1", "hcName": "m0123abcd"});
    crate::state::fs::write_json(&f.join("view.json"), &view).unwrap();
    let (got, reads) = crate::state::evidence::record_evidence_reads(|| claim(&w.ctx, &caller));
    assert!(
        matches!(got.unwrap(), Claim::Member),
        "the view presents the pane"
    );
    // Checklist item 2 (P4, a3c call 36): one status snapshot, so the subject commit is read once.
    let commit = gens::path(&f, 0, "commit");
    let commit_reads = reads.iter().filter(|p| **p == commit).count();
    assert_eq!(commit_reads, 1, "one status read: {reads:?}");
    // Another session's member with a torn view is skipped before its view is read (PM calls 2, 7).
    let other = w.folder("bbbbbbbbbbbb");
    std::fs::create_dir_all(&other).unwrap();
    let mut theirs = r.clone();
    (theirs["id"], theirs["session"]) = (json!("bbbbbbbbbbbb"), json!("$9"));
    crate::state::fs::write_json(&other.join("record.json"), &theirs).unwrap();
    std::fs::write(other.join("view.json"), br#"{"pa"#).unwrap();
    assert!(
        matches!(claim(&w.ctx, &caller).unwrap(), Claim::Member),
        "another session never blocks"
    );
    // PM calls 2, 7: a readable pre-lifecycle record of another session at this pane is skipped, not counted.
    let third = w.folder("cccccccccccc");
    std::fs::create_dir_all(&third).unwrap();
    let pre =
        json!({"id": "cccccccccccc", "session": "$9", "pane": caller.pane, "hcAddress": "t9"});
    crate::state::fs::write_json(&third.join("record.json"), &pre).unwrap();
    assert!(
        matches!(claim(&w.ctx, &caller).unwrap(), Claim::Member),
        "a readable other session is skipped"
    );
    // A torn record under another server key is never read: claim reads only the caller own key.
    let foreign = w
        .ctx
        .state_root
        .join("ffffffffffffffff")
        .join("agents")
        .join("dddddddddddd");
    std::fs::create_dir_all(&foreign).unwrap();
    std::fs::write(foreign.join("record.json"), br#"{"lifec"#).unwrap();
    assert!(
        matches!(claim(&w.ctx, &caller).unwrap(), Claim::Member),
        "another server key is never read"
    );
    // Torn evidence of a member of this session refuses (R2).
    std::fs::remove_file(gens::path(&f, 0, "commit")).unwrap();
    std::fs::write(gens::path(&f, 0, "commit"), br#"{"comm"#).unwrap();
    assert_eq!(
        claim(&w.ctx, &caller).err().map(|e| e.check),
        Some("evidence"),
        "a torn commit"
    );
    std::fs::write(f.join("record.json"), br#"{"lifec"#).unwrap();
    assert_eq!(
        claim(&w.ctx, &caller).err().map(|e| e.check),
        Some("evidence"),
        "a torn record"
    );
}

// Call 25 (checklist 9): S resumed A (A ended, superseded); B, A's child, reaches S with `send parent`, and S's
// `send children` reaches B. A raw `parentAgent.id` walk falls back to B's parent pane and finds no child of S.
#[test]
fn parent_and_children_follow_a_successor() {
    const S: &str = "0000000000d4";
    for (from, word, want) in [(B, "parent", S), (S, "children", B)] {
        let w = team_world();
        cf::member(
            &w,
            S,
            json!({"name": "ms4", "depth": 1, "parentAgent": {"id": LEADER}, "resumedFrom": A, "pane": "%7",
                "window": "@1"}),
        );
        online(&w);
        let r = go(&w, Some(from), &["send", word, "-Body", "x"]);
        assert!(r.is_ok(), "{word}: {r:?}");
        let got: Vec<String> = r.unwrap()["recipients"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| crate::agents::text(x.get("id")))
            .collect();
        assert_eq!(got, [want], "{word}");
    }
}
