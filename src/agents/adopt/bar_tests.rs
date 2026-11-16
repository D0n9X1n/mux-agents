//! adopt-3e Task 9: lock-free reads are stable (A6); the bar adopts with one try (A8, call 4); a blocked source
//! says why (call 9); an Ended leader stays the root (A9); no deadlock across the real operations (call 15).
use super::from_tests::{adopt_now, at};
use super::tests::{adopt_dir, world};
use super::*;
use crate::agents::adopt_fixture as ax;
use crate::agents::cascade_fixture as cx;
use crate::agents::fixture::World;

// A6: a reader paused across the journal's publication and removal returns the whole old or new tree; a lingering
// journal is MOVING after four reads; a journal that vanishes between reads is a retry, never `evidence` (call 14).
#[test]
fn a_stable_read_returns_the_whole_old_or_new_tree() {
    let (w, s) = world();
    ax::team(&w);
    let reads = std::cell::Cell::new(0);
    let got = stable(&w.ctx, &s, || {
        reads.set(reads.get() + 1);
        if reads.get() == 1 {
            let _ = adopt_from(&cx::tx(&w, &s), ax::E)?; // the adoption commits while this read is in flight
            return Ok(Some(ax::E.to_string())); // what this read saw before the commit
        }
        Ok(ax::parent_of(&w, cx::B))
    })
    .unwrap();
    assert_eq!(
        (got.as_deref(), reads.get()),
        (Some(cx::A), 2),
        "the first read discarded"
    );
    let (w, s) = world();
    ax::team(&w);
    cx::at("adopt-journal");
    assert_eq!(
        adopt_from(&cx::tx(&w, &s), ax::E).unwrap_err().check,
        "halted"
    );
    crate::agents::release::halt_after(None);
    let reads = std::cell::Cell::new(0);
    let got = stable(&w.ctx, &s, || {
        reads.set(reads.get() + 1);
        Ok(())
    })
    .unwrap_err();
    assert_eq!(
        (got.check, got.message.as_str(), reads.get()),
        ("topology", MOVING, 4),
        "lingering"
    );
    let reads = std::cell::Cell::new(0);
    let got = stable(&w.ctx, &s, || {
        reads.set(reads.get() + 1);
        if reads.get() == 1 {
            replay(&cx::tx(&w, &s))?; // the journal vanishes between this read's marks
        }
        Ok(ax::parent_of(&w, cx::B))
    });
    assert_eq!(
        (got.map_err(|e| e.check), reads.get()),
        (Ok(Some(cx::A.into())), 2),
        "vanished: a retry"
    );
}

// Call 14: a stable read never opens a journal: a torn one lingering is MOVING, never `evidence`.
#[test]
fn a_stable_read_never_opens_a_journal() {
    let (w, s) = world();
    std::fs::write(adopt_dir(&w, &s).join("1.json"), "{").unwrap();
    let (got, reads) = crate::state::fs::record_reads(|| stable(&w.ctx, &s, || Ok(())));
    assert_eq!(got.unwrap_err().message, MOVING);
    assert!(
        !reads.iter().any(|p| p.starts_with(dir_at(&w.ctx, &s))),
        "{reads:?}"
    );
}

/// Holds the real team lock on another thread until `go` is sent.
fn hold_team(
    w: &World,
    locks: &Path,
) -> (
    std::sync::mpsc::Sender<()>,
    std::thread::JoinHandle<Res<()>>,
) {
    let (root, l2) = (w.ctx.state_root.clone(), locks.to_path_buf());
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let h = std::thread::spawn(move || {
        let (t, ts) = ax::twin(&root, &l2);
        crate::agents::lock(&t.ctx, &ts.team_lock(), || {
            held_tx.send(()).unwrap();
            // Bounded: a test that never sends `go` frees the lock after 60 s.
            let _ = go_rx.recv_timeout(std::time::Duration::from_secs(60));
            Ok(())
        })
    });
    held_rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the holder took the team lock");
    (go_tx, h)
}

// A8, call 4, call 15 (finding 2-9): the bar's tick tries the real team lock once and never waits; busy changes
// nothing; free adopts; then idle.
#[test]
fn tick_adopts_with_one_try_and_a_busy_lock_changes_nothing() {
    let (w, s) = world();
    ax::team(&w);
    let locks = w._dir.path().join("locks");
    w.fakes.real_locks(&locks);
    let (go, holder) = hold_team(&w, &locks);
    let started = std::time::Instant::now();
    assert_eq!(
        tick(&w.ctx, &w.caller()).unwrap(),
        None,
        "busy: no adoption"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "busy: never waited"
    );
    assert_eq!(at(&w, cx::B), (Some(ax::E.into()), 3));
    go.send(()).unwrap();
    ax::bounded(holder).unwrap();
    let swept = tick(&w.ctx, &w.caller())
        .unwrap()
        .expect("free: the try held");
    assert!(swept.moved, "free: adopted");
    assert_eq!(at(&w, cx::B), (Some(cx::A.into()), 2));
    assert!(candidates(&w.ctx, &s).unwrap().is_empty(), "nothing left");
    assert_eq!(
        tick(&w.ctx, &w.caller()).unwrap(),
        None,
        "idle: no lock taken"
    );
}

// A9, call 8: an Ended leader stays the root: never a candidate, its children stay under it.
#[test]
fn an_ended_leader_stays_the_root() {
    let (w, s) = world();
    cx::member(&w, cx::L, None, 0, json!({"upTeam": "t"}));
    cx::member(&w, cx::A, Some(cx::L), 1, json!({}));
    ax::ended(&w, cx::L);
    assert!(candidates(&w.ctx, &s).unwrap().is_empty());
    assert_eq!(tick(&w.ctx, &w.caller()).unwrap(), None);
    assert_eq!(adopt_now(&w, cx::L), Adopted::Nothing);
    assert_eq!(
        (at(&w, cx::A), ax::seq(&w, &s)),
        ((Some(cx::L.into()), 1), 0)
    );
}

// prelife-1c call 2: `adopt_locked(tx, caller)` adopts every candidate for a lock holder; another team's caller is
// refused and nothing moves.
#[test]
fn adopt_locked_sweeps_for_a_lock_holder() {
    let (w, s) = world();
    ax::team(&w);
    let mut other = w.caller();
    other.session = "$9".into();
    let got = crate::agents::team_state::open(&w.ctx, &s, |tx| adopt_locked(tx, &other));
    assert_eq!(got.unwrap_err().check, "internal");
    assert_eq!(
        at(&w, cx::B),
        (Some(ax::E.into()), 3),
        "refused: nothing moved"
    );
    let c = w.caller();
    assert!(crate::agents::team_state::open(&w.ctx, &s, |tx| adopt_locked(tx, &c)).unwrap());
    assert_eq!(at(&w, cx::B), (Some(cx::A.into()), 2));
}

// Call 9 (finding 1-8): the sweep names each blocked source with its reason.
#[test]
fn sweep_reports_each_blocked_source() {
    let (w, s) = world();
    ax::team(&w);
    ax::successor(&w, ax::A2, cx::A, cx::L, 1, ax::Kind::Committed);
    ax::successor(&w, "aaaaaaaaaaa3", cx::A, cx::L, 1, ax::Kind::Committed);
    let got = crate::agents::team_state::open(&w.ctx, &s, sweep).unwrap();
    assert_eq!(
        got,
        Swept {
            moved: false,
            blocked: vec![(ax::E.to_string(), SEVERAL)]
        }
    );
}

// Call 4: one try per tick; a kept cascade journal and an adoption candidate both finish under it.
#[test]
fn one_try_finishes_a_cascade_and_adopts() {
    let (w, s) = world();
    ax::team(&w);
    cx::member(&w, cx::D, Some(cx::L), 1, json!({}));
    cx::done(&w, cx::D);
    cx::seed(&w, cx::D, &[cx::D], true);
    assert!(tick(&w.ctx, &w.caller()).unwrap().expect("held").moved);
    assert!(!cx::journal_of(&w, cx::D).exists(), "cascade finished");
    assert_eq!(at(&w, cx::B), (Some(cx::A.into()), 2), "adopted");
    let take = format!("+{}", s.team_lock());
    let takes = w.fakes.locks().iter().filter(|l| **l == take).count();
    assert_eq!(takes, 1, "one team-lock take");
}

// Call 11 (finding 2-6): a superseded parent's successor is the candidate when it ends; E itself is not.
#[test]
fn a_superseded_parents_successor_is_the_candidate() {
    let (w, s) = world();
    ax::team_live(&w);
    ax::ended(&w, ax::E);
    ax::successor(&w, ax::E2, ax::E, cx::A, 2, ax::Kind::Committed);
    ax::ended(&w, ax::E2);
    assert_eq!(candidates(&w.ctx, &s).unwrap(), vec![ax::E2.to_string()]);
    assert!(tick(&w.ctx, &w.caller()).unwrap().expect("held").moved);
    assert_eq!(
        at(&w, cx::B),
        (Some(cx::A.into()), 2),
        "B, still naming E, adopted from E′"
    );
}

/// A job's own refusals are expected (its hc is unscripted after its locks); a `lock` refusal is a deadlock.
fn no_lock_refusal<T>(r: Res<T>) -> Res<()> {
    match r {
        Err(e) if e.check == "lock" => Err(e),
        _ => Ok(()),
    }
}
/// What a job reports: its own check, then the lock events its thread recorded.
type Job = fn(&World, &Scope) -> Res<()>;

/// The team record `up` writes, led by L (spawn's admission reads it; checklist 15).
fn team_record(w: &World) {
    use crate::agents::team as t;
    use crate::parse::Client;
    let made = t::Team {
        team: "t".into(),
        session: w.caller().session,
        name: "s".into(),
        directory: "/work".into(),
        client: Client::Claude,
        policy: t::policy(Client::Claude, &[]),
        flags: Vec::new(),
        leader: cx::L.into(),
        created_at: "x".into(),
    };
    assert!(t::create(&t::path(&w.state(), &w.caller().session), &made).unwrap());
}

// A8, AD-R7, call 15 (finding 2-14, checklist 15): the real operations on real locks: an adoption, two bars' boards,
// a spawn through `cli::run` (team record present: it reaches the spawn lock, then its hc after it), and a stop that
// ends an Ended C and posts its notice (member, notice and mailbox locks). Every thread finishes; none meets the lock
// timeout; the adoption, the spawn and the stop each took the team lock, and the stop completed with C's notice.
#[test]
fn adoption_spawn_stop_and_two_bars_never_deadlock() {
    let (w, s) = world();
    ax::team(&w);
    team_record(&w);
    cx::member(&w, cx::C, Some(cx::A), 2, json!({}));
    ax::ended(&w, cx::C); // Ended: the stop posts its notice with no hc call
    std::fs::write(w.ctx.state_root.with_file_name("task.txt"), "t").unwrap();
    let locks = w._dir.path().join("locks");
    w.fakes.real_locks(&locks);
    let bar: Job = |t, _| {
        (0..10).try_for_each(|_| {
            no_lock_refusal(crate::agents::board::board(&t.ctx, &t.caller(), true, true))
        })
    };
    let jobs: [Job; 5] = [
        |t, s| no_lock_refusal(adopt(&t.ctx, s, ax::E)),
        bar,
        bar,
        |t, _| {
            crate::agents::core_fixture::online(t);
            let task = t.ctx.state_root.with_file_name("task.txt");
            let task = task.display().to_string();
            let args = [
                "spawn",
                "-Client",
                "claude",
                "-TaskFile",
                &task,
                "-Name",
                "w-x",
            ]
            .map(String::from);
            let (_, out) = crate::cli::run(&args, || Ok(&t.ctx));
            match out.contains(r#""check":"lock""#) {
                true => crate::error::refuse(2, "lock", &out),
                false => Ok(()),
            }
        },
        |t, s| {
            let stop = |tx: &Tx<'_>| crate::agents::cascade::stop(tx, &t.caller(), cx::C, &[]);
            let r = crate::agents::team_state::open(&t.ctx, s, stop)?;
            assert!(r.stopped, "the stop completed");
            Ok(())
        },
    ];
    let handles: Vec<_> = (jobs.into_iter())
        .map(|job| {
            let (root, l) = (w.ctx.state_root.clone(), locks.clone());
            std::thread::spawn(move || {
                let (t, s) = ax::twin(&root, &l);
                let got = job(&t, &s);
                (got, t.fakes.locks())
            })
        })
        .collect();
    let took = format!("+{}", s.team_lock());
    for (i, h) in handles.into_iter().enumerate() {
        let (got, seen) = ax::bounded(h);
        got.unwrap_or_else(|e| panic!("job {i}: {e:?}"));
        if [0, 3, 4].contains(&i) {
            assert!(
                seen.contains(&took),
                "job {i} reached the team lock: {seen:?}"
            );
        }
    }
    assert_eq!(at(&w, cx::B), (Some(cx::A.into()), 2));
    let marker = crate::agents::gens::path(&w.folder(cx::C), 1, "notice");
    assert!(marker.exists(), "C's notice marker");
    assert_eq!(cx::delivered(&w, cx::C), 1, "C's notice delivered once");
}
