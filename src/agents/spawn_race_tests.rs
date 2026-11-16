//! adopt-3e A2 (Task 7): adoption and E's real resume launch race on the real team lock.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

static HOLDING: AtomicBool = AtomicBool::new(false);
static GO: AtomicBool = AtomicBool::new(false);
/// A `team_state` test hook: held at the decision, inside the team lock, until GO (bounded: 60 s).
fn hold(_: &crate::agents::team_state::Tx<'_>) -> Res<()> {
    HOLDING.store(true, SeqCst);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !GO.load(SeqCst) {
        assert!(std::time::Instant::now() < end, "GO never came");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Ok(())
}
fn wait_holding() {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !HOLDING.load(SeqCst) {
        assert!(std::time::Instant::now() < end, "nothing reached the hold");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
/// E's resume, as `resume::admit` hands it to the launch (A3b-2's `Resumed`).
fn resume_e(w: &World) -> Res<Value> {
    use crate::agents::adopt_fixture as ax;
    let mut from = ax::record(w, ax::E);
    from["directory"] = json!(utf8(&w.ctx.cwd).unwrap());
    let resumed = Resumed {
        from: ax::E.into(),
        record: from,
        flags: vec![],
    };
    let mut req = request(w);
    req.task = "";
    req.resume = Some(&resumed);
    launch_hc(&w.ctx, &w.caller(), req, slot)
}
/// The member whose `resumedFrom` is E, if any.
fn successor_of(w: &World) -> Option<String> {
    let agents = super::super::super::server_dir(&w.ctx, &w.caller()).join("agents");
    (std::fs::read_dir(agents).ok()?.flatten())
        .filter_map(|e| read_json(&e.path().join("record.json")).ok().flatten())
        .find(|r| r["resumedFrom"] == crate::agents::adopt_fixture::E)
        .and_then(|r| r["id"].as_str().map(String::from))
}

// adopt-3e A2 (finding 2-12): adoption and E's real resume race on the real team lock, in both orders, each held at
// its decision until the other is blocked. The adopter records, under the lock, whether E′ was on disk when it
// decided; the outcome must be the one that order allows, never a mix.
#[test]
fn adoption_and_resume_race_to_one_outcome() {
    use crate::agents::{adopt, adopt_fixture as ax, cascade_fixture as cx};
    for resume_first in [true, false] {
        HOLDING.store(false, SeqCst);
        GO.store(false, SeqCst);
        let w = World::human("");
        ready(&w);
        ax::team(&w);
        let locks = w._dir.path().join("locks");
        w.fakes.real_locks(&locks);
        let (root, l2) = (w.ctx.state_root.clone(), locks.clone());
        let adopter = move || {
            let (t, ts) = ax::twin(&root, &l2);
            if !resume_first {
                team_state::test_hook(Some(hold));
            }
            let got = team_state::open(&t.ctx, &ts, |tx| {
                let seen = successor_of(&t).is_some();
                Ok((seen, adopt::adopt_from(tx, ax::E)?))
            });
            team_state::test_hook(None);
            got
        };
        let (seen, got) = if resume_first {
            team_state::test_hook(Some(hold));
            let waiter = std::thread::spawn(move || {
                wait_holding();
                let h = std::thread::spawn(adopter);
                std::thread::sleep(std::time::Duration::from_millis(200)); // the adopter waits on the real lock
                GO.store(true, SeqCst);
                h
            });
            resume_e(&w).unwrap();
            team_state::test_hook(None);
            ax::bounded(ax::bounded(waiter)).unwrap()
        } else {
            let h = std::thread::spawn(adopter);
            wait_holding();
            let go = std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(200)); // the resume waits on the real lock
                GO.store(true, SeqCst);
            });
            resume_e(&w).unwrap();
            ax::bounded(go);
            ax::bounded(h).unwrap()
        };
        let e2 = successor_of(&w).expect("the resume committed E′");
        let s = team_state::Scope::of(&w.caller());
        let t = crate::agents::cascade::Tree::read(&cx::tx(&w, &s)).unwrap();
        let order = "the order forced is the order observed under the lock";
        assert_eq!(seen, resume_first, "{order}");
        if seen {
            assert_eq!(got, adopt::Adopted::Nothing, "resume first: E superseded");
            assert_eq!(
                (t.parents(cx::B).unwrap(), ax::seq(&w, &s)),
                (vec![e2.clone()], 0),
                "B under E′"
            );
        } else {
            assert_eq!(got, adopt::Adopted::Moved(cx::A.into()), "adoption first");
            assert_eq!(
                (ax::parent_of(&w, cx::B), ax::seq(&w, &s)),
                (Some(cx::A.into()), 1)
            );
            assert_eq!(t.below(&e2).unwrap(), vec![e2.clone()], "E′ childless");
        }
    }
}
