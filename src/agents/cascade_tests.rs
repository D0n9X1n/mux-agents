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

type Bad = (&'static str, fn(&World));

/// Replaces `at` with a link to an empty folder (Unix symlink; Windows directory symlink or junction). A fn, not a
/// capturing closure, so the `Bad` table can name it (checklist 16).
fn link(w: &World, at: &std::path::Path) {
    let target = w._dir.path().join("elsewhere");
    std::fs::create_dir_all(&target).unwrap();
    let _ = std::fs::remove_dir_all(at);
    crate::testing::link_dir(&target, at);
}

// C4.2, C4.4, C7: published once (an identical retry is the same journal; another body refuses `internal`), read
// back whole, rewritten prepared, removed through a tombstone; an empty folder waits for nothing.
#[test]
fn a_journal_is_published_once_marked_prepared_and_removed() {
    let (w, s) = lf::world();
    let tx = cx::tx(&w, &s);
    assert!(
        journals(&tx).unwrap().is_empty() && !waiting(&w.ctx, &s),
        "no folder"
    );
    let j = cx::seed(&w, A, &[C, B, A], false);
    assert!(waiting(&w.ctx, &s));
    assert_eq!(journals(&tx).unwrap(), std::slice::from_ref(&j));
    publish(&tx, &j).unwrap();
    assert_eq!(
        publish(&tx, &cx::body(A, &[B, A], false))
            .unwrap_err()
            .check,
        "internal",
        "create-once"
    );
    let p = mark_prepared(&tx, &j).unwrap();
    assert!(p.prepared && journals(&tx).unwrap()[0].prepared);
    assert!(p.lists(B) && !p.lists(L));
    remove(&tx, A).unwrap();
    assert!(journals(&tx).unwrap().is_empty() && !cx::journal_of(&w, A).exists());
    assert!(
        !cx::journal_of(&w, A).with_extension("removed").exists(),
        "the tombstone is gone"
    );
    assert!(!waiting(&w.ctx, &s), "an empty folder: nothing waits");
    remove(&tx, A).unwrap();
}

// C-R5, call 15: anything on the walk or in the folder that is not a whole journal refuses `evidence`, and
// `waiting` answers true so the next `finish` meets it. Links run on every OS through `testing::link_dir`; a link
// also refuses publish, mark_prepared and remove (call 30, checklist 22) and nothing is written through it.
#[test]
fn journal_reads_refuse_what_is_not_a_journal() {
    let cases: [Bad; 10] = [
        ("torn", |w| {
            std::fs::write(cx::journal_of(w, A), b"{\"root").unwrap()
        }),
        ("folder at the journal", |w| {
            std::fs::create_dir(cx::journal_of(w, A)).unwrap()
        }),
        ("not an id", |w| {
            std::fs::write(cx::journal_of(w, "x"), b"{}").unwrap()
        }),
        ("folder with a temp name", |w| {
            let tmp = format!("{A}.json.{}.tmp", "0".repeat(32));
            std::fs::create_dir(cx::journal_of(w, A).with_file_name(tmp)).unwrap();
        }),
        ("folder with a tombstone name", |w| {
            std::fs::create_dir(cx::journal_of(w, A).with_extension("removed")).unwrap();
        }),
        ("torn tombstone", |w| {
            std::fs::write(cx::journal_of(w, A).with_extension("removed"), b"{").unwrap();
        }),
        ("linked cascade", |w| {
            link(w, cx::journal_of(w, A).parent().unwrap())
        }),
        ("linked teams", |w| link(w, &w.state().join("teams"))),
        ("linked server key", |w| link(w, &w.state())),
        ("file at cascade", |w| {
            let dir = cx::journal_of(w, A).parent().unwrap().to_path_buf();
            std::fs::remove_dir_all(&dir).unwrap();
            std::fs::write(dir, b"x").unwrap();
        }),
    ];
    for (case, plant) in cases {
        let (w, s) = lf::world();
        std::fs::create_dir_all(cx::journal_of(&w, A).parent().unwrap()).unwrap();
        plant(&w);
        let tx = cx::tx(&w, &s);
        assert_eq!(journals(&tx).unwrap_err().check, "evidence", "{case}");
        assert!(waiting(&w.ctx, &s), "{case}: waits, so finish refuses");
        if case.starts_with("linked") {
            let j = cx::body(A, &[A], false);
            let got = publish(&tx, &j).unwrap_err();
            assert_eq!(got.check, "evidence", "{case}: publish");
            let got = mark_prepared(&tx, &j).unwrap_err();
            assert_eq!(got.check, "evidence", "{case}: mark_prepared");
            let got = remove(&tx, A).unwrap_err();
            assert_eq!(got.check, "evidence", "{case}: remove");
            let target = w._dir.path().join("elsewhere");
            assert_eq!(
                std::fs::read_dir(&target).unwrap().count(),
                0,
                "{case}: never written through a link"
            );
        }
    }
    let (w, s) = lf::world();
    let j = cx::seed(&w, A, &[A], false);
    let tmp = format!("{A}.json.{}.tmp", "0".repeat(32)); // `fs::write_json_durable`'s temp name
    std::fs::write(cx::journal_of(&w, A).with_file_name(tmp), b"{").unwrap();
    assert_eq!(
        journals(&cx::tx(&w, &s)).unwrap(),
        [j],
        "a regular temp file is skipped"
    );
}

// Call 16: one row per structural invariant; each refuses `evidence`. Checklist 21: a body whose root differs from
// its file name, and an entry id that is not a plain agent id, refuse too.
#[test]
fn journal_structure_is_checked() {
    let e = |id: &str, parents: &[&str]| json!({"id": id, "name": "n", "parents": parents, "gens": [1]});
    let cases: [(&str, &str, Value); 10] = [
        ("empty", A, json!([])),
        ("no root", A, json!([e(C, &[B]), e(B, &[])])),
        (
            "duplicate id",
            A,
            json!([e(B, &[A]), e(B, &[A]), e(A, &[])]),
        ),
        ("parent outside", A, json!([e(B, &[L]), e(A, &[])])),
        ("root not last", A, json!([e(A, &[]), e(B, &[A])])),
        ("root with a parent", A, json!([e(B, &[A]), e(A, &[L])])),
        ("orphan entry", A, json!([e(C, &[]), e(B, &[A]), e(A, &[])])),
        ("cycle", A, json!([e(C, &[B]), e(B, &[C]), e(A, &[])])),
        ("another root", B, json!([e(A, &[])])),
        ("entry id not an id", A, json!([e("../x", &[A]), e(A, &[])])),
    ];
    for (case, root, entries) in cases {
        let (w, s) = lf::world();
        std::fs::create_dir_all(cx::journal_of(&w, A).parent().unwrap()).unwrap();
        let body = json!({"root": root, "entries": entries, "prepared": false});
        crate::state::fs::write_json(&cx::journal_of(&w, A), &body).unwrap();
        assert_eq!(
            journals(&cx::tx(&w, &s)).unwrap_err().check,
            "evidence",
            "{case}"
        );
    }
    let (w, s) = lf::world();
    let ok = cx::seed(&w, A, &[C, B, A], true);
    assert_eq!(
        journals(&cx::tx(&w, &s)).unwrap(),
        [ok],
        "a well-formed journal parses"
    );
}

// Call 14, call 30, checklist 20: every flush point of every journal write, one row each. Armed, the step fails
// twice (a retry repeats the flush, also when the folder or the file already exists, or the file is already
// renamed); cleared, the retry completes.
#[test]
fn every_journal_flush_is_repeated_until_it_lands() {
    let rows = [
        ("publish", "key"),
        ("publish", "teams"),
        ("publish", "content"),
        ("publish", "cascade"),
        ("prepared", "key"),
        ("prepared", "teams"),
        ("prepared", "content"),
        ("prepared", "cascade"),
        ("remove", "key"),
        ("remove", "teams"),
        ("remove", "cascade"),
    ];
    for (step, at) in rows {
        let (w, s) = lf::world();
        let tx = cx::tx(&w, &s);
        let j = cx::body(A, &[A], false);
        if step != "publish" {
            publish(&tx, &j).unwrap();
        }
        let file = cx::journal_of(&w, A);
        let dir = file.parent().unwrap().to_path_buf();
        let run = || match step {
            "publish" => publish(&tx, &j),
            "prepared" => mark_prepared(&tx, &j).map(|_| ()),
            _ => remove(&tx, A),
        };
        match at {
            "key" => crate::state::fs::fail_sync_dir(Some(&w.state())),
            "teams" => crate::state::fs::fail_sync_dir(Some(&w.state().join("teams"))),
            "cascade" => crate::state::fs::fail_sync_dir(Some(&dir)),
            _ => crate::state::fs::fail_sync_file(Some(&file)),
        }
        for attempt in [1, 2] {
            let e = run().unwrap_err();
            assert_eq!(
                (e.check, e.message.as_str()),
                ("state", NOT_WRITTEN),
                "{step} {at} #{attempt}"
            );
        }
        if step == "remove" {
            assert!(
                waiting(&w.ctx, &s),
                "remove: the journal or its tombstone keeps it waiting until a flush lands"
            );
        }
        crate::state::fs::fail_sync_dir(None);
        crate::state::fs::fail_sync_file(None);
        run().unwrap();
        let want = match step {
            "remove" => Vec::new(),
            "prepared" => vec![Journal {
                prepared: true,
                ..j.clone()
            }],
            _ => vec![j.clone()],
        };
        assert_eq!(journals(&tx).unwrap(), want, "{step} {at}");
        assert_eq!(
            waiting(&w.ctx, &s),
            step != "remove",
            "{step} {at}: a removal leaves no tombstone"
        );
    }
}

// Calls 14, 30, checklist 20: the folders each write flushes, in order, on every call (also a repeated one): a
// mutant that drops any one flush (clear's final one included) changes its log.
#[test]
fn every_journal_write_flushes_its_folders_in_order() {
    let (w, s) = lf::world();
    let tx = cx::tx(&w, &s);
    let (key, teams) = (w.state(), w.state().join("teams"));
    let dir = cx::journal_of(&w, A).parent().unwrap().to_path_buf();
    let j = cx::body(A, &[A], false);
    for attempt in [1, 2] {
        let (got, log) = crate::state::fs::record_syncs(|| publish(&tx, &j));
        got.unwrap();
        assert_eq!(
            log,
            [key.clone(), teams.clone(), dir.clone()],
            "publish #{attempt}"
        );
    }
    for attempt in [1, 2] {
        let (got, log) = crate::state::fs::record_syncs(|| mark_prepared(&tx, &j));
        got.unwrap();
        assert_eq!(
            log,
            [key.clone(), teams.clone(), dir.clone()],
            "prepared #{attempt}"
        );
    }
    for attempt in [1, 2] {
        let (got, log) = crate::state::fs::record_syncs(|| remove(&tx, A));
        got.unwrap();
        assert_eq!(
            log,
            [key.clone(), teams.clone(), dir.clone(), dir.clone()],
            "remove #{attempt}"
        );
    }
}

// Call 29 (re-audit R3): a removal cut short between its durable rename and the tombstone's deletion leaves
// `<root>.removed`; `journals` reports it as an unfinished removal (never skipped), so its root's sub-tree stays
// listed (closed) and `waiting` stays true; the next `remove` completes the flushes and deletes it.
#[test]
fn a_tombstone_is_an_unfinished_removal_until_it_is_cleared() {
    let (w, s) = lf::world();
    let tx = cx::tx(&w, &s);
    let j = cx::seed(&w, A, &[C, B, A], true);
    cx::at("cascade-tombstoned");
    assert_eq!(remove(&tx, A).unwrap_err().check, "halted");
    let tomb = cx::journal_of(&w, A).with_extension("removed");
    assert!(
        tomb.exists() && !cx::journal_of(&w, A).exists(),
        "renamed, not deleted"
    );
    let got = journals(&tx).unwrap();
    let want = Journal {
        removing: true,
        ..j
    };
    assert_eq!(got, [want], "reported as an unfinished removal");
    assert!(got[0].lists(B), "its sub-tree stays listed");
    assert!(waiting(&w.ctx, &s));
    remove(&tx, A).unwrap();
    assert!(!tomb.exists() && journals(&tx).unwrap().is_empty() && !waiting(&w.ctx, &s));
}
