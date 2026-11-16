use super::*;
use crate::agents::cascade_fixture::{self as cx, A, B, C, L, S};
use crate::agents::fixture::{ID, World};
use crate::agents::gens;
use crate::agents::life_fixture as lf;
use crate::agents::team_state::{Scope, open};
use serde_json::json;

type Case = (&'static str, fn(&std::path::Path));

// C1: absent reads running; each written word reads back; anything else refuses. Unreadable never means running.
#[test]
fn desired_of_is_three_valued() {
    let w = World::human("");
    let s = Scope::of(&w.caller());
    let m = w.folder(ID);
    std::fs::create_dir_all(&m).unwrap();
    assert_eq!(desired_of(&m).unwrap(), Desired::Running, "absent");
    open(&w.ctx, &s, |tx| tx.desired(ID, false)).unwrap();
    assert_eq!(desired_of(&m).unwrap(), Desired::Stopped, "stopped");
    open(&w.ctx, &s, |tx| tx.desired(ID, true)).unwrap();
    assert_eq!(desired_of(&m).unwrap(), Desired::Running, "running");
    let cases: [Case; 3] = [
        ("torn", |m| {
            std::fs::write(m.join("desired.json"), b"{\"desi").unwrap()
        }),
        ("folder", |m| {
            std::fs::create_dir(m.join("desired.json")).unwrap()
        }),
        ("unknown word", |m| {
            std::fs::write(m.join("desired.json"), br#"{"desired":"paused"}"#).unwrap()
        }),
    ];
    for (case, plant) in cases {
        let _ = std::fs::remove_file(m.join("desired.json"));
        plant(&m);
        let want = if case == "unknown word" {
            "internal"
        } else {
            "evidence"
        };
        assert_eq!(desired_of(&m).unwrap_err().check, want, "{case}");
        let _ = std::fs::remove_dir(m.join("desired.json"));
    }
    // R5 (call 19): a linked member folder refuses through the walk; the link's target is never read.
    let target = w._dir.path().join("elsewhere");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("desired.json"), br#"{"desired":"stopped"}"#).unwrap();
    let linked = w.folder("bbbbbbbbbbbb");
    crate::testing::link_dir(&target, &linked);
    assert_eq!(
        desired_of(&linked).unwrap_err().check,
        "evidence",
        "a linked member folder"
    );
}

const T: &str = "777777777777";
const V: &str = "666666666666";
/// (case, expected check, expected message or "" for any, the plant).
type Plant = (&'static str, &'static str, &'static str, fn(&World));

fn none(_: &str) -> bool {
    false
}

// C2: a member is closed when it or an ancestor is desired stopped, or a journal lists one of them (nearest
// first); a listed descendant closes nothing above it. `below` is X's sub-tree, deepest first.
#[test]
fn a_stopped_member_closes_its_sub_tree() {
    let (w, s) = lf::world();
    cx::chain(&w);
    open(&w.ctx, &s, |tx| tx.desired(A, false)).unwrap();
    let t = Tree::read(&cx::tx(&w, &s)).unwrap();
    for (id, want) in [(L, None), (A, Some(A)), (B, Some(A)), (C, Some(A))] {
        assert_eq!(t.closed_by(id, &none).unwrap().as_deref(), want, "{id}");
    }
    assert_eq!(
        t.closed_by(L, &|x| x == B).unwrap(),
        None,
        "a listed descendant closes nothing above it"
    );
    assert_eq!(
        t.closed_by(C, &|x| x == B).unwrap().as_deref(),
        Some(B),
        "nearest first"
    );
    assert_eq!(t.chain(C).unwrap(), [C, B, A, L]);
    assert_eq!(t.below(A).unwrap(), [C, B, A]);
    assert_eq!(t.below(L).unwrap(), [C, B, A, L]);
}

// C0 (call 17): a parent naming a superseded member resolves forward to every non-void successor, also once the
// predecessor's record is gone; a void successor is not followed; no record is rewritten. Call 18: `chain` is
// breadth-first, so `closed_by` names the nearest stopped ancestor (T, at distance 1), not L (distance 2).
#[test]
fn a_superseded_parent_resolves_forward() {
    let (w, s) = lf::world();
    cx::member(&w, L, None, 0, json!({}));
    cx::member(&w, A, Some(L), 1, json!({}));
    cx::member(&w, B, Some(A), 2, json!({}));
    let tx = cx::tx(&w, &s);
    tx.desired(A, false).unwrap();
    cx::void(&w, V, A, L);
    let t = Tree::read(&tx).unwrap();
    assert_eq!(
        t.parents(B).unwrap(),
        [A],
        "a void successor is not followed"
    );
    assert_eq!(t.closed_by(B, &none).unwrap().as_deref(), Some(A));
    cx::member(&w, S, Some(L), 1, json!({"resumedFrom": A}));
    cx::member(&w, T, Some(L), 1, json!({"resumedFrom": A}));
    let before = std::fs::read(w.folder(B).join("record.json")).unwrap();
    let t = Tree::read(&tx).unwrap();
    assert_eq!(t.parents(B).unwrap(), [S, T], "every non-void successor");
    assert_eq!(t.chain(B).unwrap(), [B, S, T, L], "breadth-first");
    assert_eq!(
        t.closed_by(B, &none).unwrap(),
        None,
        "A stays stopped; B's parents are S and T"
    );
    tx.desired(T, false).unwrap();
    tx.desired(L, false).unwrap();
    let t = Tree::read(&tx).unwrap();
    assert_eq!(
        t.closed_by(B, &none).unwrap().as_deref(),
        Some(T),
        "the nearest, not L"
    );
    assert_eq!(t.below(A).unwrap(), [A]);
    assert_eq!(t.below(S).unwrap(), [B, S]);
    std::fs::remove_dir_all(w.folder(A)).unwrap();
    let t = Tree::read(&tx).unwrap();
    assert_eq!(
        t.parents(B).unwrap(),
        [S, T],
        "the predecessor's record is gone: still forward"
    );
    assert_eq!(
        std::fs::read(w.folder(B).join("record.json")).unwrap(),
        before,
        "no record rewritten"
    );
}

// prelife-1c call 1 (G10): a pre-lifecycle member is not in the tree; a chain ends at it; its folder is never
// read for desired state or written.
#[test]
fn a_pre_lifecycle_member_is_not_in_the_tree() {
    let (w, s) = lf::world();
    std::fs::create_dir_all(w.folder(A)).unwrap();
    let pre = json!({"id": A, "name": "w-a", "session": w.caller().session, "depth": 1,
        "parentAgent": {"id": L}});
    crate::state::fs::write_json(&w.folder(A).join("record.json"), &pre).unwrap();
    std::fs::write(w.folder(A).join("desired.json"), b"{").unwrap(); // torn: would refuse if read
    cx::member(&w, B, Some(A), 2, json!({}));
    let t = Tree::read(&cx::tx(&w, &s)).unwrap();
    assert!(!t.has(A));
    assert_eq!(
        t.chain(B).unwrap(),
        [B],
        "the chain ends at a pre-lifecycle parent"
    );
}

// Ambiguity 3, calls 6, 17, 18, R2, R5: Tree::read validates every member. A parent loop refuses `authority`, a
// resumedFrom loop `internal`, each naming every member on the loop (sorted); unreadable successor evidence, an
// unreadable record or an in-session unreadable desired state refuses `evidence` (never void, never running). A
// parent with no record and no successor ends the chain; another session's member is skipped unread. Checklist 23:
// `never_ran_recorded`'s own read (an unnamed generation's torn commit beside a void pending) refuses `evidence`.
#[test]
fn broken_chains_and_unreadable_members_refuse() {
    let cases: [Plant; 9] = [
        ("missing parent", "", "", |w| {
            cx::member(w, B, Some(A), 2, json!({}));
        }),
        (
            "parent loop",
            "authority",
            "the parent chain loops: w-b, w-c",
            |w| {
                cx::member(w, B, Some(C), 2, json!({}));
                cx::member(w, C, Some(B), 3, json!({}));
            },
        ),
        (
            "resumedFrom loop",
            "internal",
            "a resumedFrom chain loops: w-5, w-a",
            |w| {
                cx::member(w, A, Some(L), 1, json!({"resumedFrom": S}));
                cx::member(w, S, Some(L), 1, json!({"resumedFrom": A}));
            },
        ),
        ("unreadable successor evidence", "evidence", "", |w| {
            cx::member(w, A, Some(L), 1, json!({}));
            let s = cx::member(w, S, Some(L), 1, json!({"resumedFrom": A}));
            std::fs::remove_file(gens::path(&s, 1, "commit")).unwrap();
            std::fs::write(gens::path(&s, 1, "commit"), b"{\"comm").unwrap();
        }),
        ("void pending, torn unnamed commit", "evidence", "", |w| {
            cx::member(w, A, Some(L), 1, json!({}));
            cx::void(w, V, A, L);
            let v = w.folder(V);
            std::fs::create_dir_all(gens::dir(&v, 1)).unwrap();
            std::fs::write(gens::path(&v, 1, "commit"), b"{\"comm").unwrap();
        }),
        ("torn record", "evidence", "", |w| {
            std::fs::create_dir_all(w.folder(A)).unwrap();
            std::fs::write(w.folder(A).join("record.json"), b"{\"li").unwrap();
        }),
        ("torn desired", "evidence", "", |w| {
            cx::member(w, A, Some(L), 1, json!({}));
            std::fs::write(w.folder(A).join("desired.json"), b"{").unwrap();
        }),
        ("other session, torn desired", "", "", |w| {
            cx::member(w, A, Some(L), 1, json!({"session": "other"}));
            std::fs::write(w.folder(A).join("desired.json"), b"{").unwrap();
        }),
        ("a void successor of a gone member", "", "", |w| {
            cx::void(w, V, A, L);
            cx::member(w, B, Some(A), 2, json!({}));
        }),
    ];
    for (case, check, message, plant) in cases {
        let (w, s) = lf::world();
        cx::member(&w, L, None, 0, json!({}));
        plant(&w);
        let got = Tree::read(&cx::tx(&w, &s)).and_then(|t| t.below(L));
        match check {
            "" => assert!(got.is_ok(), "{case}: {got:?}"),
            want => {
                let e = got.unwrap_err();
                assert_eq!(e.check, want, "{case}");
                if !message.is_empty() {
                    assert_eq!(e.message, message, "{case}");
                }
            }
        }
    }
}
