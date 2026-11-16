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

// A1 (C0): A resumed as A′, then E ends: A′ adopts B, live or pending; authority's walk over `Tree::all`
// (cascade Task 13's `Tree::ancestors`; finding 2-11) reaches A′.
#[test]
fn a_resumed_ancestor_adopts_as_its_successor() {
    use crate::agents::life_fixture as lf;
    for pending in [false, true] {
        let (w, s) = world();
        ax::team(&w);
        if pending {
            ax::set(&w, cx::B, "pending", lf::pending(2));
        }
        ax::successor(&w, ax::A2, cx::A, cx::L, 1, ax::Kind::Committed);
        assert_eq!(adopt_now(&w, ax::E), moved(ax::A2), "pending {pending}");
        assert_eq!(at(&w, cx::B), (Some(ax::A2.into()), 2), "pending {pending}");
        let t = crate::agents::cascade::Tree::all(&cx::tx(&w, &s)).unwrap();
        let up = t.ancestors(cx::B, cx::L).unwrap();
        assert_eq!(
            up,
            vec![ax::A2.to_string(), cx::L.to_string()],
            "pending {pending}"
        );
    }
}

// C0 (finding 2-13): `stop A′` reaches the adopted B, live and pending, through the real cascade: B's generations
// end newest first, then A′; E is Ended (no hc call); every scripted hc answer is used.
#[test]
fn the_successors_stop_reaches_the_adopted_child() {
    use crate::agents::{gens, life_fixture as lf};
    for pending in [false, true] {
        let (w, s) = world();
        ax::team(&w);
        ax::successor(&w, ax::A2, cx::A, cx::L, 1, ax::Kind::Committed);
        if pending {
            let mut p = lf::pending(2);
            p["hcAddress"] = json!(cx::addr_at(cx::B, 2));
            ax::set(&w, cx::B, "pending", p);
            lf::seed_gen(&w.folder(cx::B), 2, Some("run"), lf::committed());
        }
        assert_eq!(adopt_now(&w, ax::E), moved(ax::A2), "pending {pending}");
        if pending {
            cx::terminates_at(&w, cx::B, 2);
        }
        cx::terminates_at(&w, cx::B, 1);
        cx::terminates_at(&w, ax::A2, 1);
        let r = crate::agents::cascade::stop(&cx::tx(&w, &s), &w.caller(), ax::A2, &[]).unwrap();
        assert!(r.stopped, "pending {pending}");
        let ended = |id: &str, n: u64| {
            gens::read(&w.folder(id), n, "ended")
                .known()
                .unwrap()
                .is_some()
        };
        assert!(ended(cx::B, 1), "pending {pending}: B ended");
        assert_eq!(
            ended(cx::B, 2),
            pending,
            "pending {pending}: B's pending launch ended"
        );
        assert!(ended(ax::A2, 1), "pending {pending}: A′ ended");
        w.fakes.hc_done().unwrap();
    }
}
