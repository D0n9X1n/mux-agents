//! adopt-3e AD-R6, process level: an ignored child adopts and `crash_point` kills it (exit 86) after one durable
//! step; the parent opens a Tx on the same files and must converge. Every child is bounded (`crash::LIMIT`).
use super::*;
use crate::agents::adopt_fixture as ax;
use crate::agents::cascade_fixture as cx;
use crate::agents::core_launch::crash::{killed_at, plain, printed};
use crate::agents::fixture::World;

const STEPS: [&str; 5] = [
    "adopt-journal",
    "adopt-change-bbbbbbbbbbb1",
    "adopt-change-ccccccccccc1",
    "adopt-seq",
    "adopt-removed",
];

/// A world on the child's state root (same fake process table, so the same server key).
fn world_at(root: &Path) -> (World, Scope) {
    let mut w = World::human("");
    w.ctx.state_root = root.to_path_buf();
    let s = Scope::of(&w.caller());
    (w, s)
}

#[test]
#[ignore = "a child of an_adoption_killed_after_each_step_converges"]
fn child_adopt() {
    let mut w = World::human("");
    w.ctx.state_root = plain(&w.ctx.state_root);
    w.state();
    let s = Scope::of(&w.caller());
    println!("CRASH-ROOT {}", w.ctx.state_root.display());
    println!("CRASH-DIR {}", w._dir.path().display());
    ax::four_deep(&w);
    let c = w.caller();
    let _ = crate::agents::team_state::open(&w.ctx, &s, |tx| adopt_locked(tx, &c));
    panic!("no crash point was reached");
}

// A4: "a crash after the journal, after each change, after the seq write, and before removal: replay converges".
#[test]
fn an_adoption_killed_after_each_step_converges() {
    for step in STEPS {
        let kv = killed_at("agents::adopt::crash::child_adopt", step);
        let (w, s) = world_at(&printed(&kv, "ROOT"));
        crate::agents::team_state::open(&w.ctx, &s, |_| Ok(())).unwrap();
        let b = (ax::parent_of(&w, cx::B), ax::depth_of(&w, cx::B));
        let c = (ax::parent_of(&w, cx::C), ax::depth_of(&w, cx::C));
        let want = ((Some(cx::L.to_string()), 1), (Some(cx::B.to_string()), 2));
        assert_eq!((b, c), want, "{step}");
        let after = (ax::seq(&w, &s), listing(&w.ctx, &s).unwrap());
        assert_eq!(after, (1, Listing::default()), "{step}");
        let caller = w.caller();
        let again = crate::agents::team_state::open(&w.ctx, &s, |tx| adopt_locked(tx, &caller));
        assert!(!again.unwrap(), "{step}: nothing left");
        assert_eq!(
            ax::parent_of(&w, ax::E).as_deref(),
            Some(cx::L),
            "{step}: E untouched"
        );
        std::fs::remove_dir_all(printed(&kv, "DIR")).unwrap();
    }
}
