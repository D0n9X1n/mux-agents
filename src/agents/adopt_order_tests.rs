//! adopt-3e Tasks 7-8: resume and adoption are ordered by the team lock (A2); C0 through adoption (A1).
use super::from_tests::{adopt_now, at, moved};
use super::tests::world;
use super::*;
use crate::agents::adopt_fixture as ax;
use crate::agents::cascade_fixture as cx;
use crate::agents::fixture::World;

fn tree(w: &World) -> crate::agents::cascade::Tree {
    crate::agents::cascade::Tree::read(&cx::tx(w, &Scope::of(&w.caller()))).unwrap()
}

// A2: adoption first moves B to A; E's successor E′ then has no children.
#[test]
fn adoption_then_resume_leaves_the_successor_childless() {
    let (w, _) = world();
    ax::team(&w);
    assert_eq!(adopt_now(&w, ax::E), moved(cx::A));
    ax::successor(&w, ax::E2, ax::E, cx::A, 2, ax::Kind::Committed);
    let below = tree(&w).below(ax::E2).unwrap();
    assert_eq!(below, vec![ax::E2.to_string()], "E′ childless");
    assert_eq!(at(&w, cx::B), (Some(cx::A.into()), 2));
}

// A2: resume first (pending or committed): E is superseded, never a source; B is under E′ (C0).
#[test]
fn resume_first_makes_e_no_source() {
    for kind in [ax::Kind::Pending, ax::Kind::Committed] {
        let (w, s) = world();
        ax::team(&w);
        ax::successor(&w, ax::E2, ax::E, cx::A, 2, kind);
        assert_eq!(adopt_now(&w, ax::E), Adopted::Nothing, "{kind:?}");
        let parents = tree(&w).parents(cx::B).unwrap();
        assert_eq!(parents, vec![ax::E2.to_string()], "{kind:?}");
        assert_eq!(ax::seq(&w, &s), 0, "{kind:?}");
    }
}

// A2, settle-1b G5: a successor proven never to have run stops blocking: E is adoptable again.
#[test]
fn a_void_successor_makes_e_adoptable_again() {
    for kind in [ax::Kind::Abandoned, ax::Kind::Aborted] {
        let (w, _) = world();
        ax::team(&w);
        ax::successor(&w, ax::E2, ax::E, cx::A, 2, kind);
        assert_eq!(adopt_now(&w, ax::E), moved(cx::A), "{kind:?}");
        assert_eq!(at(&w, cx::B), (Some(cx::A.into()), 2), "{kind:?}");
    }
}

// A2: a successor that ran stays the successor once released: E stays superseded, B under E′, no adoption.
#[test]
fn a_committed_released_successor_keeps_e_superseded() {
    let (w, s) = world();
    ax::team(&w);
    ax::successor(&w, ax::E2, ax::E, cx::A, 2, ax::Kind::Released);
    assert_eq!(
        (adopt_now(&w, ax::E), adopt_now(&w, ax::E2)),
        (Adopted::Nothing, Adopted::Nothing),
        "E′ Lost survives"
    );
    assert_eq!(tree(&w).parents(cx::B).unwrap(), vec![ax::E2.to_string()]);
    assert_eq!(ax::seq(&w, &s), 0);
}
