//! adopt-3e Task 3: replay, the first `team_state` hook (A4 replay, A6; calls 6, 7, 26).
use super::tests::{adopt_dir, put, up, world};
use super::*;
use crate::agents::adopt_fixture as ax;
use crate::agents::cascade_fixture as cx;
use crate::agents::fixture::World;
use crate::agents::team_state::Scope;
use crate::state::fs::{fail_sync_dir, fail_sync_file};

fn opened(w: &World, s: &Scope) -> Res<()> {
    crate::agents::team_state::open(&w.ctx, s, |_| Ok(()))
}

// Call 7: a journal at or below the committed seq is stale: removed (through its tombstone) without its body read,
// torn or whole; newer records untouched.
#[test]
fn a_stale_journal_is_removed_unparsed() {
    for torn in [false, true] {
        let (w, s) = world();
        ax::team(&w);
        commit(&cx::tx(&w, &s), 1).unwrap();
        ax::put_journal(&w, &s, &ax::to_a(1));
        if torn {
            std::fs::write(dir_at(&w.ctx, &s).join("1.json"), "{\"se").unwrap();
        }
        opened(&w, &s).unwrap();
        assert_eq!(
            (ax::parent_of(&w, cx::B).as_deref(), ax::depth_of(&w, cx::B)),
            (Some(ax::E), 3),
            "torn {torn}"
        );
        assert_eq!(
            listing(&w.ctx, &s).unwrap(),
            Listing::default(),
            "torn {torn}: removed"
        );
        assert_eq!(ax::seq(&w, &s), 1);
    }
}

// A4 replay: seq = committed + 1 is finished from step 3; a second open changes nothing.
#[test]
fn the_next_journal_is_finished_from_step_three() {
    let (w, s) = world();
    ax::team(&w);
    ax::put_journal(&w, &s, &ax::to_a(1));
    opened(&w, &s).unwrap();
    assert_eq!(
        (ax::parent_of(&w, cx::B).as_deref(), ax::depth_of(&w, cx::B)),
        (Some(cx::A), 2)
    );
    assert_eq!(
        (ax::seq(&w, &s), listing(&w.ctx, &s).unwrap()),
        (1, Listing::default())
    );
    let before = ax::record(&w, cx::B);
    opened(&w, &s).unwrap();
    assert_eq!(ax::record(&w, cx::B), before);
}

// A4 replay, ambiguity 10: two journals above the seq, a gap, or a tombstone above the seq: 1 `topology`, nothing
// removed or written, no body run.
#[test]
fn two_journals_a_gap_or_a_live_tombstone_refuse() {
    let cases: [(&str, u64, &[&str]); 4] = [
        ("two", 0, &["1.json", "2.json"]),
        ("gap", 0, &["2.json"]),
        ("stale and gap", 2, &["1.json", "4.json"]),
        ("live tombstone", 0, &["1.removed"]),
    ];
    for (case, at, names) in cases {
        let (w, s) = world();
        ax::team(&w);
        if at > 0 {
            commit(&cx::tx(&w, &s), at).unwrap();
        }
        for name in names {
            let seq: u64 = name.split('.').next().unwrap().parse().unwrap();
            put(&w, &s, name, ax::to_a(seq).body());
        }
        let before = listing(&w.ctx, &s).unwrap();
        let ran = std::cell::Cell::new(false);
        let got = crate::agents::team_state::open(&w.ctx, &s, |_| {
            ran.set(true);
            Ok(())
        })
        .unwrap_err();
        assert_eq!(
            (got.check, got.message.as_str()),
            ("topology", OUT_OF_ORDER),
            "{case}"
        );
        assert!(!ran.get(), "{case}: no body");
        assert_eq!(
            listing(&w.ctx, &s).unwrap(),
            before,
            "{case}: nothing removed"
        );
        assert_eq!(
            (ax::parent_of(&w, cx::B).as_deref(), ax::seq(&w, &s)),
            (Some(ax::E), at),
            "{case}"
        );
    }
}

// Call 6: a tombstone a cut-short removal left at or below the seq is swept.
#[test]
fn a_leftover_tombstone_is_swept() {
    let (w, s) = world();
    ax::team(&w);
    commit(&cx::tx(&w, &s), 1).unwrap();
    std::fs::write(adopt_dir(&w, &s).join("1.removed"), "").unwrap();
    opened(&w, &s).unwrap();
    assert_eq!(listing(&w.ctx, &s).unwrap(), Listing::default());
}

// Calls 6, 14, 26 (checklist 3): replay finishes every unfinished publication or seq flush, the current journal's
// own content flush, and a removal's final flush, before it classifies, applies or removes anything; a flush that
// still fails stops replay and the journal (or tombstone) stays.
#[test]
fn replay_finishes_every_unfinished_flush_first() {
    for at in [
        "key",
        "teams",
        "seq content",
        "adopt",
        "journal content",
        "removal",
    ] {
        let (w, s) = world();
        ax::team(&w);
        let tx = cx::tx(&w, &s);
        publish(&tx, &ax::to_a(1)).unwrap();
        let dir = dir_at(&w.ctx, &s);
        if at != "journal content" {
            for c in &ax::to_a(1).changes {
                apply(&tx, c).unwrap();
            }
            commit(&tx, 1).unwrap(); // the seq is visible: journal 1 is now stale
        }
        if at == "removal" {
            // The removal's final flush (after the delete) failed: the tombstone is kept for replay.
            crate::state::fs::fail_sync_dir_after(Some(&dir), 1);
            assert_eq!(remove(&tx, 1).unwrap_err().message, NOT_WRITTEN);
        }
        match at {
            "key" => fail_sync_dir(Some(&up(&dir, 2))),
            "teams" => fail_sync_dir(Some(&up(&dir, 1))),
            "adopt" | "removal" => fail_sync_dir(Some(&dir)),
            "journal content" => fail_sync_file(Some(&dir.join("1.json"))),
            _ => fail_sync_file(Some(&seq_path(&w.ctx, &s))),
        }
        let kept_now = || {
            let l = listing(&w.ctx, &s).unwrap();
            if at == "removal" {
                l.tombstones
            } else {
                l.journals
            }
        };
        for attempt in [1, 2] {
            let e = replay(&tx).unwrap_err();
            assert_eq!(
                (e.check, e.message.as_str()),
                ("state", NOT_WRITTEN),
                "{at} #{attempt}"
            );
            assert_eq!(kept_now(), vec![1], "{at} #{attempt}: kept");
            assert_eq!(
                ax::depth_of(&w, cx::B),
                if at == "journal content" { 3 } else { 2 },
                "{at}: not applied"
            );
        }
        fail_sync_dir(None);
        fail_sync_file(None);
        replay(&tx).unwrap();
        assert_eq!(
            (listing(&w.ctx, &s).unwrap(), ax::seq(&w, &s)),
            (Listing::default(), 1),
            "{at}"
        );
        assert_eq!(ax::depth_of(&w, cx::B), 2, "{at}");
    }
}

// A6, ambiguity 15: adoption replays before group B's hooks, so they and every body see a finished topology.
#[test]
fn replay_is_the_first_hook() {
    use crate::agents::team_state::{HOOKS, Hook};
    assert!(std::ptr::fn_addr_eq(HOOKS[0], replay as Hook));
}
