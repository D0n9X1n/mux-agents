use super::*;
use crate::agents::fixture::World;
use crate::agents::tree::tests::{LEADER, child, leader_record};
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
