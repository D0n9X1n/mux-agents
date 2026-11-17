//! adopt-3e Task 9: the board and the bar over adoption (A6, calls 4, 9, 13, 20).
use super::*;
use crate::agents::fixture::World;
use crate::agents::team_state::Scope;
use crate::agents::{adopt, adopt_fixture as ax, cascade_fixture as cx, life_fixture as lf};
use serde_json::json;

// adopt-3e A6, call 15: a lingering journal while the real team lock is busy: the bar's board errs MOVING (the bar
// keeps its frame); `list` replays under a Tx once the lock is free; a free bar ticks it.
#[test]
fn a_lingering_journal_keeps_the_bar_frame_and_list_replays() {
    let w = World::human("");
    w.state();
    let c = w.caller();
    let s = Scope::of(&c);
    let locks = w._dir.path().join("locks");
    w.fakes.real_locks(&locks);
    ax::put_journal(&w, &s, &ax::to_a(1)); // B has no record here: replay skips the change
    let (root, l2) = (w.ctx.state_root.clone(), locks.clone());
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let (t, ts) = ax::twin(&root, &l2);
        crate::agents::lock(&t.ctx, &ts.team_lock(), || {
            held_tx.send(()).unwrap();
            let _ = go_rx.recv_timeout(std::time::Duration::from_secs(60));
            Ok(())
        })
    });
    held_rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the holder took the team lock");
    let got = board(&w.ctx, &c, true, true).unwrap_err();
    assert_eq!(
        (got.check, got.message.as_str()),
        ("topology", adopt::MOVING),
        "bar keeps its frame"
    );
    go_tx.send(()).unwrap();
    ax::bounded(holder).unwrap();
    board(&w.ctx, &c, true, false).unwrap();
    assert_eq!(
        (ax::seq(&w, &s), adopt::listing(&w.ctx, &s).unwrap()),
        (1, Default::default()),
        "list replayed"
    );
    ax::put_journal(&w, &s, &ax::to_a(2));
    board(&w.ctx, &c, true, true).unwrap();
    assert_eq!(ax::seq(&w, &s), 2, "bar replayed");
}

// Call 4: a watch board takes the team lock at most once per refresh.
#[test]
fn a_watch_board_takes_the_team_lock_once() {
    let w = World::human("");
    w.state();
    let c = w.caller();
    let s = Scope::of(&c);
    ax::put_journal(&w, &s, &ax::to_a(1));
    board(&w.ctx, &c, true, true).unwrap();
    let take = format!("+{}", s.team_lock());
    assert_eq!(w.fakes.locks().iter().filter(|l| **l == take).count(), 1);
    assert_eq!(ax::seq(&w, &s), 1, "the one try replayed");
}

// adopt-3e A6, call 13 (finding 2-8): the board's snapshot and its `Tree::all` are read inside one `stable`
// bracket. An adoption committed right after the bracket closes leaves the pair the board uses consistent.
#[test]
fn the_board_reads_its_tree_inside_the_bracket() {
    fn adopt_e(ctx: &Ctx, s: &Scope) {
        let tx = crate::agents::team_state::Tx {
            ctx,
            scope: s.clone(),
        };
        let got = adopt::adopt_from(&tx, ax::E).unwrap();
        assert_eq!(got, adopt::Adopted::Moved(cx::A.into()));
    }
    let (w, s) = lf::world();
    ax::team(&w);
    after_bracket(Some(adopt_e));
    let got = steady(&w.ctx, &s, true, || Ok(ax::parent_of(&w, cx::B)));
    after_bracket(None);
    let (seen, shown) = got.unwrap();
    assert_eq!(
        shown.placed(cx::B),
        Some(vec![seen.unwrap()]),
        "snapshot and tree from one bracket"
    );
    assert_eq!(
        ax::parent_of(&w, cx::B).as_deref(),
        Some(cx::A),
        "the adoption committed after it"
    );
}

// Call 9 (finding 1-8), call 20: a blocked source's row says why, found by `id` on the bar's rows and by the
// recorded `hcAddress` on `list`'s (which carry no `id`); other rows are untouched.
#[test]
fn a_blocked_source_row_says_why() {
    let blocked = [(ax::E.to_string(), Some(cx::addr(ax::E)), adopt::SEVERAL)];
    let mut bar = Map::new();
    bar.insert("agents".into(), json!([{"id": ax::E}, {"id": cx::B}]));
    note_blocked(&mut bar, &blocked);
    assert_eq!(bar["agents"][0]["adoption"], adopt::SEVERAL);
    assert!(bar["agents"][1].get("adoption").is_none());
    let mut list = Map::new();
    list.insert(
        "agents".into(),
        json!([{"name": "w-b", "hcAddress": cx::addr(cx::B)}, {"name": "w-e", "hcAddress": cx::addr(ax::E)}]),
    );
    note_blocked(&mut list, &blocked);
    assert!(list["agents"][0].get("adoption").is_none());
    assert_eq!(list["agents"][1]["adoption"], adopt::SEVERAL);
}

// Ambiguity 12, call 2: `list` sweeps under a Tx when a candidate exists, before its reap; then no candidate is left.
#[test]
fn list_sweeps_a_candidate_before_its_reap() {
    let (w, s) = lf::world();
    ax::team(&w);
    board(&w.ctx, &w.caller(), true, false).unwrap();
    assert_eq!(ax::parent_of(&w, cx::B).as_deref(), Some(cx::A), "adopted");
    assert_eq!((ax::depth_of(&w, cx::B), ax::seq(&w, &s)), (2, 1));
    assert!(adopt::candidates(&w.ctx, &s).unwrap().is_empty());
}
