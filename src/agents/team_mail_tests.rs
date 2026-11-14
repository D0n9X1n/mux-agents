use super::*;
use crate::agents::fixture::World;
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
