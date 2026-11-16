use super::*;
use crate::agents::cascade_fixture::{self as cx, A, B, C, D, L, S};
use crate::agents::core_fixture as cf;
use crate::agents::fixture::{ID, World};
use crate::agents::gens;
use crate::agents::life_fixture as lf;
use crate::agents::settle::Mode;
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

/// L -> A -> {B -> C, D}. B: active 1, pending 2 known only from the record (no gens/2). C: active 2 known only
/// from the record, generation 1 ended (done), generation 2's ended.json torn (unreadable completion). D: active 4
/// known only from the record; generation 1 an orphan known only from its files.
fn standing_tree(w: &World) {
    cx::member(w, L, None, 0, json!({}));
    cx::member(w, A, Some(L), 1, json!({}));
    cx::member(w, B, Some(A), 2, json!({"pending": lf::pending(2)}));
    let c = cx::member(
        w,
        C,
        Some(B),
        3,
        json!({"generation": "2", "hcAddress": cx::addr_at(C, 2)}),
    );
    crate::state::evidence::publish(&gens::path(&c, 1, "ended"), &json!({"at": "x"})).unwrap();
    std::fs::create_dir_all(gens::dir(&c, 2)).unwrap();
    std::fs::write(gens::path(&c, 2, "ended"), b"{\"at").unwrap();
    cx::member(
        w,
        D,
        Some(A),
        2,
        json!({"generation": "4", "hcAddress": cx::addr_at(D, 4)}),
    );
}

// C4.1-4.4, call 19: entries are every member whose chain reaches A, deepest first, each with every generation
// not done as it stands: a pending one known only from the record (B 2), an active one known only from the record
// (C 2, D 4), an orphan known only from files (D 1); a generation whose completion is unreadable stays listed
// (C 2); a done one does not (C 1). Parents only inside the set; each desired stopped, then prepared; no hc call
// (C4 never waits for pending launches). A second prepare of A refuses (C4: finish first).
#[test]
fn prepare_lists_the_sub_tree_as_it_stands_and_closes_it_before_any_kill() {
    let (w, s) = lf::world();
    standing_tree(&w);
    let tx = cx::tx(&w, &s);
    let j = prepare(&tx, A).unwrap();
    let rows: Vec<(&str, Vec<u64>, Vec<String>)> = (j.entries.iter())
        .map(|e| (e.id.as_str(), e.gens.clone(), e.parents.clone()))
        .collect();
    let want = [
        (C, vec![2], vec![B.to_string()]),
        (B, vec![1, 2], vec![A.to_string()]),
        (D, vec![1, 4], vec![A.to_string()]),
        (A, vec![1], vec![]),
    ];
    assert_eq!(rows, want);
    assert!(j.prepared && journals(&tx).unwrap()[0].prepared, "C4.4");
    for id in [A, B, C, D] {
        assert!(cx::stopped(&w, id), "{id}: C4.3");
    }
    assert!(!cx::stopped(&w, L), "the parent stays");
    assert!(lf::calls(&w).is_empty(), "C4 calls no hc and kills nothing");
    assert_eq!(
        prepare(&tx, A).unwrap_err().check,
        "internal",
        "one journal per root"
    );
    assert_eq!(
        prepare(&tx, S).unwrap_err().check,
        "internal",
        "not a lifecycle member of this session"
    );
}

// Call 19, C-R2, checklist 19: prepare decides from one snapshot. Every record.json is read once (in Tree::read),
// also for a successor with a recorded end (T) and a running successor (S); each commit.json at most once (the
// successors' marks read for their void evidence are the ones `open_gens` uses).
#[test]
fn prepare_reads_each_record_once() {
    let (w, s) = lf::world();
    standing_tree(&w);
    cx::member(&w, S, Some(A), 2, json!({"resumedFrom": D}));
    let t = cx::member(&w, T, Some(A), 2, json!({"resumedFrom": D}));
    crate::state::evidence::publish(&gens::path(&t, 1, "ended"), &json!({"at": "x"})).unwrap();
    let (got, reads) = crate::state::fs::record_reads(|| prepare(&cx::tx(&w, &s), A));
    for id in [L, A, B, C, D, S, T] {
        let n = (reads.iter())
            .filter(|p| **p == w.folder(id).join("record.json"))
            .count();
        assert_eq!(n, 1, "{id}");
    }
    for p in reads.iter().filter(|p| p.ends_with("commit.json")) {
        let n = reads.iter().filter(|q| *q == p).count();
        assert_eq!(n, 1, "{}: one read", p.display());
    }
    assert!(reads.contains(&gens::path(&w.folder(S), 1, "commit")));
    let j = got.unwrap();
    assert!(j.lists(S) && j.lists(T), "both successors are entries");
}

// Checklist 24 (S6): a generation whose ended.json exists while record.completing names it is not done: listed.
#[test]
fn a_completing_generation_stays_listed() {
    let (w, s) = lf::world();
    cx::member(&w, L, None, 0, json!({}));
    let a = cx::member(&w, A, Some(L), 1, json!({"completing": {"gen": 1}}));
    crate::state::evidence::publish(&gens::path(&a, 1, "ended"), &json!({"at": "x"})).unwrap();
    let j = prepare(&cx::tx(&w, &s), A).unwrap();
    assert_eq!(j.entries.len(), 1);
    assert_eq!(j.entries[0].gens, [1], "completing holds it");
}

// C4 "A failed write kills nothing" (C-R4): a failed journal flush stops before any desired write; a failed
// desired write stops before the next one and before `prepared`. The unprepared journal is finished by `desire`.
#[test]
fn a_failed_journal_write_kills_nothing() {
    let (w, s) = lf::world();
    cx::chain(&w);
    let tx = cx::tx(&w, &s);
    crate::state::fs::fail_sync_dir(Some(&dir_at(&w.ctx, &s)));
    let got = prepare(&tx, A).unwrap_err();
    crate::state::fs::fail_sync_dir(None);
    assert_eq!(
        (got.check, got.message.as_str()),
        ("state", NOT_WRITTEN),
        "journal flush"
    );
    assert!(
        ![A, B, C].iter().any(|id| cx::stopped(&w, id)),
        "no desired write after a failed publication"
    );
    remove(&tx, A).unwrap();
    crate::state::fs::fail_sync_dir(Some(&w.folder(B)));
    let got = prepare(&tx, A).unwrap_err();
    crate::state::fs::fail_sync_dir(None);
    assert_eq!(
        (got.check, got.message.as_str()),
        ("state", crate::agents::team_state::NOT_WRITTEN),
        "desired"
    );
    assert!(
        cx::stopped(&w, C) && !cx::stopped(&w, A),
        "C written, A not reached"
    );
    let kept = journals(&tx).unwrap();
    assert!(
        !kept[0].prepared,
        "not prepared after a failed desired write"
    );
    assert!(desire(&tx, &kept[0]).unwrap().prepared);
    assert!([A, B, C].iter().all(|id| cx::stopped(&w, id)));
    assert!(lf::calls(&w).is_empty(), "nothing killed");
}

// C5, C-R3: `desire` finishes C4.3-4 from the journal alone: a member added under B after C4.2 is not added.
#[test]
fn desire_never_adds_members() {
    let (w, s) = lf::world();
    cx::chain(&w);
    let j = cx::seed(&w, A, &[C, B, A], false);
    cx::member(&w, S, Some(B), 3, json!({}));
    let p = desire(&cx::tx(&w, &s), &j).unwrap();
    assert!(p.prepared && p.entries == j.entries);
    assert!(!cx::stopped(&w, S), "fixed at C4.2");
}

// p1c call 19, checklist 26 (Task 4 part): replaying an unprepared journal reads each entry's record once and
// skips a pre-lifecycle entry before any write: its folder stays byte-identical; the others are stopped and the
// journal is prepared. (Task 5 reports the skipped entry unresolved `prelife`.)
#[test]
fn desire_skips_a_pre_lifecycle_entry_before_any_write() {
    let (w, s) = lf::world();
    cx::member(&w, L, None, 0, json!({}));
    cx::member(&w, A, Some(L), 1, json!({}));
    std::fs::create_dir_all(w.folder(B)).unwrap();
    let pre = json!({"id": B, "name": "w-b", "session": w.caller().session, "depth": 2,
        "parentAgent": {"id": A}});
    crate::state::fs::write_json(&w.folder(B).join("record.json"), &pre).unwrap();
    let before = cx::snapshot(&w.folder(B));
    let j = cx::seed(&w, A, &[B, A], false);
    let (got, reads) = crate::state::fs::record_reads(|| desire(&cx::tx(&w, &s), &j));
    assert!(got.unwrap().prepared, "the journal is prepared");
    for id in [A, B] {
        let n = (reads.iter())
            .filter(|p| **p == w.folder(id).join("record.json"))
            .count();
        assert_eq!(n, 1, "{id}: one record read at replay");
    }
    assert_eq!(cx::snapshot(&w.folder(B)), before, "B byte-identical");
    assert!(cx::stopped(&w, A));
    // An unreadable record is never taken for a lifecycle one: desire refuses before any write.
    std::fs::write(w.folder(B).join("record.json"), b"{\"id").unwrap();
    let before = cx::snapshot(&w.folder(B));
    let got = desire(&cx::tx(&w, &s), &j).unwrap_err();
    assert_eq!(got.check, "evidence", "torn record");
    assert_eq!(cx::snapshot(&w.folder(B)), before, "nothing written to B");
}

// Call 19: `desired_of` walks from the state root, so a linked ancestor of the member folder (`agents`) refuses
// `evidence` and the link's target is never read.
#[test]
fn desired_of_refuses_through_a_linked_ancestor() {
    let (w, _) = lf::world();
    let target = w._dir.path().join("elsewhere");
    std::fs::create_dir_all(target.join(A)).unwrap();
    std::fs::write(
        target.join(A).join("desired.json"),
        br#"{"desired":"running"}"#,
    )
    .unwrap();
    let (agents, member) = (w.state().join("agents"), w.folder(A));
    std::fs::remove_dir_all(&agents).unwrap();
    crate::testing::link_dir(&target, &agents);
    assert_eq!(desired_of(&member).unwrap_err().check, "evidence");
}

// Call 19 (deviation guard): the snapshot's own void rule (`void_of`, from the marks it keeps) answers exactly as
// group B's `never_ran_recorded` for each kind of successor.
#[test]
fn the_snapshot_void_rule_is_never_ran_recorded() {
    type Seed = (&'static str, fn(&World));
    let cases: [Seed; 5] = [
        ("void pending", |w| cx::void(w, V, A, L)),
        ("running", |w| {
            cx::member(w, V, Some(L), 1, json!({"resumedFrom": A}));
        }),
        ("recorded end", |w| {
            let v = cx::member(w, V, Some(L), 1, json!({"resumedFrom": A}));
            crate::state::evidence::publish(&gens::path(&v, 1, "ended"), &json!({"at": "x"}))
                .unwrap();
        }),
        ("held pending", |w| {
            let mut p = lf::pending(0);
            p["hcAddress"] = json!("t0");
            let v = w.folder(V);
            std::fs::create_dir_all(&v).unwrap();
            let r = json!({"lifecycle": 1, "id": V, "name": "w-6", "session": w.caller().session,
                "sessionId": lf::SID, "team": lf::team(w), "depth": 1, "parentAgent": {"id": L},
                "resumedFrom": A, "pending": p});
            crate::state::fs::write_json(&v.join("record.json"), &r).unwrap();
        }),
        ("void pending still completing", |w| {
            cx::void(w, V, A, L);
            let p = w.folder(V).join("record.json");
            let mut r: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
            r["completing"] = json!({"gen": 0});
            crate::state::fs::write_json(&p, &r).unwrap();
        }),
    ];
    for (case, seed) in cases {
        let (w, s) = lf::world();
        cx::member(&w, L, None, 0, json!({}));
        cx::member(&w, A, Some(L), 1, json!({}));
        seed(&w);
        let t = Tree::read(&cx::tx(&w, &s)).unwrap();
        let gens::Member::Life(g) = gens::member_of(&w.folder(V)) else {
            panic!("{case}: not a lifecycle member");
        };
        let want = g.never_ran_recorded(&g.status().unwrap()).unwrap();
        assert_eq!(t.nodes[V].void, want, "{case}");
    }
}

fn caller(w: &World) -> crate::caller::Caller {
    w.caller()
}
fn kill_order(w: &World) -> Vec<String> {
    (lf::calls(w).into_iter())
        .filter(|c| c[0] == "kill")
        .map(|c| c[1].clone())
        .collect()
}

// C6, C7 ("stop A over A -> B -> C kills C, B, A"): deepest first; each complete entry gets its Stopped notice
// (message, then marker); the journal is removed only after the last marker; L is untouched.
#[test]
fn execute_ends_c_then_b_then_a_and_removes_the_journal_last() {
    let (w, s) = lf::world();
    cx::chain(&w);
    for id in [C, B, A] {
        cx::terminates(&w, id);
    }
    let tx = cx::tx(&w, &s);
    let j = prepare(&tx, A).unwrap();
    let unprepared = Journal {
        prepared: false,
        ..j.clone()
    };
    let e = execute(&tx, &caller(&w), &unprepared).unwrap_err();
    assert_eq!(e.check, "internal", "C6: prepared only");
    let r = execute(&tx, &caller(&w), &j).unwrap();
    assert!(r.stopped && r.unresolved.is_empty(), "{r:?}");
    assert_eq!(r.ids, [C, B, A]);
    assert_eq!(
        kill_order(&w),
        [cx::addr(C), cx::addr(B), cx::addr(A)],
        "bottom-up"
    );
    for id in [C, B, A] {
        let f = w.folder(id);
        assert!(
            gens::read(&f, 1, "ended").known().unwrap().is_some(),
            "{id} ended"
        );
        assert!(
            gens::read(&f, 1, "notice").known().unwrap().is_some(),
            "{id} marker"
        );
        assert_eq!(cx::delivered(&w, id), 1, "{id}: one delivery");
    }
    assert!(!cx::journal_of(&w, A).exists() && !cx::stopped(&w, L));
    w.fakes.hc_done().unwrap();
}

// C6 ("a failed kill keeps ancestors and journal"): C's kill fails: B and A are held (never asked of hc), the
// sibling D under A still ends, the journal stays; the reply lists only C. The kept report answers a second stop
// of A without a second prepare; the next finish completes it.
#[test]
fn a_failed_kill_keeps_its_ancestors_and_the_journal() {
    let (w, s) = lf::world();
    cx::chain(&w);
    cx::member(&w, D, Some(A), 2, json!({}));
    cx::kill_fails(&w, C);
    cx::terminates(&w, D);
    let tx = cx::tx(&w, &s);
    let j = prepare(&tx, A).unwrap();
    let r = execute(&tx, &caller(&w), &j).unwrap();
    assert!(!r.stopped);
    let names: Vec<(&str, &str)> = (r.unresolved.iter())
        .map(|u| (u.name.as_str(), u.check))
        .collect();
    assert_eq!(names, [("w-c", "hc")]);
    assert_eq!(
        kill_order(&w),
        [cx::addr(C), cx::addr(D)],
        "B and A never killed"
    );
    assert!(cx::journal_of(&w, A).exists());
    w.fakes.hc_done().unwrap();
    cx::kill_fails(&w, C);
    let kept = finish(&tx, &caller(&w)).unwrap();
    assert_eq!(
        stop(&tx, &caller(&w), A, &kept).unwrap(),
        kept[0],
        "the kept report; no second journal"
    );
    for id in [C, B, A] {
        cx::terminates(&w, id);
    }
    let done = finish(&tx, &caller(&w)).unwrap();
    assert!(done[0].stopped && !cx::journal_of(&w, A).exists());
    assert_eq!(cx::kills(&w, D), 1, "D was ended once");
    w.fakes.hc_done().unwrap();
}

// prelife-1c call 1 (G10): a pre-lifecycle member under A is no entry and gets no desired.json; its folder stays
// byte-identical; A's cascade still completes.
#[test]
fn a_pre_lifecycle_member_under_a_is_left_byte_identical() {
    let (w, s) = lf::world();
    cx::member(&w, L, None, 0, json!({}));
    cx::member(&w, A, Some(L), 1, json!({}));
    std::fs::create_dir_all(w.folder(B)).unwrap();
    let pre = json!({"id": B, "name": "w-b", "session": w.caller().session, "depth": 2,
        "parentAgent": {"id": A}});
    crate::state::fs::write_json(&w.folder(B).join("record.json"), &pre).unwrap();
    let before = cx::snapshot(&w.folder(B));
    cx::terminates(&w, A);
    let tx = cx::tx(&w, &s);
    let j = prepare(&tx, A).unwrap();
    let ids: Vec<&str> = j.entries.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, [A], "no entry for B");
    assert!(
        execute(&tx, &caller(&w), &j).unwrap().stopped,
        "A's cascade completes"
    );
    assert_eq!(
        cx::snapshot(&w.folder(B)),
        before,
        "B's folder byte-identical"
    );
    w.fakes.hc_done().unwrap();
}

// prelife-1c Task 5 (G10, spec "mixed tree", p1c-calls 1 and 9): an unmarked child B naming A as parent, holding a
// desired.json and gens/1 evidence, is never read past its record by Tree::read, prepare, execute or finish; it is
// no entry, nothing under it changes and no hc call names it.
#[test]
fn a_pre_lifecycle_child_is_never_listed_closed_or_ended() {
    use crate::agents::prelife_fixture as pf;
    let (w, s) = lf::world();
    cx::member(&w, L, None, 0, json!({}));
    cx::member(&w, A, Some(L), 1, json!({}));
    std::fs::create_dir_all(w.folder(B)).unwrap();
    let pre = json!({"id": B, "name": "w-b", "session": w.caller().session, "depth": 2,
        "parentAgent": {"id": A}});
    crate::state::fs::write_json(&w.folder(B).join("record.json"), &pre).unwrap();
    crate::state::fs::write_json(
        &w.folder(B).join("desired.json"),
        &json!({"desired": "running"}),
    )
    .unwrap();
    pf::dev_evidence(&w, B);
    let before = pf::bytes(&w.folder(B));
    let tx = cx::tx(&w, &s);
    cx::terminates(&w, A);
    let (got, reads) = crate::state::fs::record_reads(|| -> crate::error::Res<_> {
        let in_tree = Tree::read(&tx)?.has(B);
        let j = prepare(&tx, A)?;
        let r = execute(&tx, &caller(&w), &j)?;
        Ok((in_tree, j, r, finish(&tx, &caller(&w))?))
    });
    let past = pf::reads_past_record(&reads, &w.folder(B));
    assert!(past.is_empty(), "nothing of B past record.json: {past:?}");
    let (in_tree, j, r, done) = got.unwrap();
    assert!(!in_tree, "never read into the tree");
    let ids: Vec<&str> = j.entries.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, [A], "A only");
    assert!(r.stopped && r.unresolved.is_empty(), "{r:?}");
    assert!(done.iter().all(|r| r.stopped), "{done:?}");
    assert_eq!(
        pf::bytes(&w.folder(B)),
        before,
        "no desired.json write, no gens write"
    );
    assert!(
        !lf::calls(&w).iter().flatten().any(|a| a.contains(B)),
        "no hc call about B"
    );
    w.fakes.hc_done().unwrap();
}

// Checklist 26 (p1c call 19): an unprepared journal [B(pre), A] (B became pre-lifecycle between passes): the
// replay writes nothing to B, reports it unresolved `prelife` and holds its ancestor A (no hc call); the journal stays.
// prelife-1c Task 5 (checklist 3): B holds gens/1 evidence and a desired.json; the replay reads neither.
#[test]
fn a_replayed_pre_lifecycle_entry_is_unresolved_and_left_byte_identical() {
    use crate::agents::prelife_fixture as pf;
    let (w, s) = lf::world();
    cx::member(&w, L, None, 0, json!({}));
    cx::member(&w, A, Some(L), 1, json!({}));
    std::fs::create_dir_all(w.folder(B)).unwrap();
    let pre = json!({"id": B, "name": "w-b", "session": w.caller().session, "depth": 2,
        "parentAgent": {"id": A}});
    crate::state::fs::write_json(&w.folder(B).join("record.json"), &pre).unwrap();
    crate::state::fs::write_json(
        &w.folder(B).join("desired.json"),
        &json!({"desired": "running"}),
    )
    .unwrap();
    pf::dev_evidence(&w, B);
    cx::seed(&w, A, &[B, A], false);
    let before = cx::snapshot(&w.folder(B));
    let (r, reads) = crate::state::fs::record_reads(|| finish(&cx::tx(&w, &s), &caller(&w)));
    let past = pf::reads_past_record(&reads, &w.folder(B));
    assert!(past.is_empty(), "nothing of B past record.json: {past:?}");
    let r = r.unwrap();
    assert_eq!(r.len(), 1);
    let names: Vec<(&str, &str)> = (r[0].unresolved.iter())
        .map(|u| (u.name.as_str(), u.check))
        .collect();
    assert_eq!(names, [("w-b", "prelife")], "{r:?}");
    assert!(!r[0].stopped && cx::journal_of(&w, A).exists());
    assert_eq!(cx::snapshot(&w.folder(B)), before, "B byte-identical");
    assert!(cx::stopped(&w, A), "A closed, held");
    assert!(lf::calls(&w).is_empty(), "A held: never asked of hc");
}

// C7, call 14: a removal cut short after its rename leaves a tombstone; `waiting` stays true; the next `finish`
// clears it and flushes its folder.
#[test]
fn finish_clears_a_tombstone_a_cut_short_removal_left() {
    let (w, s) = lf::world();
    cx::seed(&w, A, &[A], true);
    let f = cx::journal_of(&w, A);
    std::fs::rename(&f, f.with_extension("removed")).unwrap();
    assert!(waiting(&w.ctx, &s));
    assert!(finish(&cx::tx(&w, &s), &caller(&w)).unwrap().is_empty());
    assert!(!f.with_extension("removed").exists() && !waiting(&w.ctx, &s));
}

/// (halt step, C ended and noticed before the test, members whose whole end runs before the halt, the member whose
/// kill is cut short at `kill`: matched and reread, `kill.json` published, no kill sent; b-calls 54).
type Row = (String, bool, Vec<&'static str>, Option<&'static str>);

fn rows() -> Vec<Row> {
    let mut v: Vec<Row> = vec![("cascade-journal".into(), false, vec![], None)];
    for id in [C, B, A] {
        v.push((format!("cascade-desired-{id}"), false, vec![], None));
    }
    v.push(("cascade-prepared".into(), false, vec![], None));
    for (first, done) in [(C, false), (B, true)] {
        v.push(("kill".into(), done, vec![], Some(first)));
        for step in ["ended", "delivered"] {
            v.push((step.into(), done, vec![first], None));
        }
        v.push((format!("cascade-noticed-{first}"), done, vec![first], None));
    }
    v.push((format!("cascade-noticed-{A}"), false, vec![C, B, A], None));
    v.push(("cascade-tombstoned".into(), false, vec![C, B, A], None));
    v.push(("cascade-removed".into(), false, vec![C, B, A], None));
    v
}

// C5, C6, C7, spec "Crash after each prepare write" and "Execute crash boundaries": stopped after each durable
// step, B stays closed; the next pass finishes: every entry stopped, one end, one delivery, one marker, no
// second kill (b-calls 54: at `kill` only `kill.json` exists, so the replay kills once), the journal removed only
// after every marker.
#[test]
fn a_crash_after_each_durable_step_is_finished_by_the_next_pass() {
    for (step, c_done, first, cut) in rows() {
        let (w, s) = lf::world();
        cx::chain(&w);
        if c_done {
            cx::done(&w, C);
        }
        first.iter().for_each(|id| cx::terminates(&w, id));
        if let Some(id) = cut {
            cx::rereads(&w, id, &cx::wire(&w, id, "live", None, &lf::ARGV));
        }
        let (tx, c) = (cx::tx(&w, &s), w.caller());
        cx::at(&step);
        let e = stop(&tx, &c, A, &[]).unwrap_err();
        assert_eq!(e.check, "halted", "{step}");
        let gone = ["cascade-tombstoned", "cascade-removed"].contains(&step.as_str());
        assert_eq!(
            cx::journal_of(&w, A).exists(),
            !gone,
            "{step}: removed last"
        );
        let js = journals(&tx).unwrap();
        let t = Tree::read(&tx).unwrap();
        let closed = t.closed_by(B, &|x| js.iter().any(|j| j.lists(x)));
        assert!(closed.unwrap().is_some(), "{step}: B closed");
        w.fakes.hc_done().unwrap();
        for id in [C, B, A] {
            let ended = gens::read(&w.folder(id), 1, "ended").known().unwrap();
            if ended.is_none() {
                cx::terminates(&w, id);
            }
        }
        let kept = finish(&tx, &c).unwrap();
        assert!(stop(&tx, &c, A, &kept).unwrap().stopped, "{step}");
        for id in [C, B, A] {
            assert!(cx::stopped(&w, id), "{step}: {id}");
            let marker = gens::read(&w.folder(id), 1, "notice").known().unwrap();
            assert!(marker.is_some(), "{step}: {id} marker");
            let want = usize::from(!(c_done && id == C));
            let got = (cx::kills(&w, id), cx::delivered(&w, id));
            assert_eq!(got, (want, want), "{step}: {id} once");
        }
        assert!(
            !cx::journal_of(&w, A).exists() && !waiting(&w.ctx, &s),
            "{step}"
        );
        w.fakes.hc_done().unwrap();
    }
}

// Spec "Unresolved pending": L -> A -> B; B's pending generation 2 at G3 row 1 (torn launch.json), then at
// unresolved row 6 (committed; hc finds no terminal and no kill was recorded). stop A: both stopped before any kill;
// A not killed; journal kept; the reply lists B. Once B settles, the next pass ends B's older generation, then A,
// and removes the journal.
#[test]
fn an_unresolved_pending_launch_keeps_its_ancestors_running() {
    for row in ["row 1", "row 6"] {
        let (w, s) = lf::world();
        cx::member(&w, L, None, 0, json!({}));
        cx::member(&w, A, Some(L), 1, json!({}));
        let b = cx::member(&w, B, Some(A), 2, json!({"pending": lf::pending(2)}));
        if row == "row 1" {
            std::fs::create_dir_all(gens::dir(&b, 2)).unwrap();
            std::fs::write(gens::path(&b, 2, "launch"), b"{\"hc").unwrap();
        } else {
            lf::seed_gen(&b, 2, Some("run"), lf::committed());
            cf::online(&w);
            let label = format!("mux.id={B}");
            let argv = [
                "ls",
                "--all",
                "--label",
                label.as_str(),
                "--label",
                "mux.gen=2",
                "--json",
            ];
            cf::answer(&w, &argv, r#"{"terminals":[]}"#);
        }
        let (tx, c) = (cx::tx(&w, &s), w.caller());
        let r = stop(&tx, &c, A, &[]).unwrap();
        let both = cx::stopped(&w, A) && cx::stopped(&w, B);
        assert!(!r.stopped && both, "{row}: both stopped");
        let want = if row == "row 1" { "evidence" } else { "hc" };
        let got: Vec<(&str, &str)> = (r.unresolved.iter())
            .map(|u| (u.name.as_str(), u.check))
            .collect();
        assert_eq!(got, [("w-b", want)], "{row}");
        assert_eq!(
            lf::count(&w, "kill"),
            0,
            "{row}: neither A nor B's generation 1 killed"
        );
        assert!(cx::journal_of(&w, A).exists(), "{row}");
        w.fakes.hc_done().unwrap();
        if row == "row 1" {
            std::fs::remove_file(gens::path(&b, 2, "launch")).unwrap();
            lf::seed_gen(&b, 2, None, None); // G3 row 3 next: abandoned, no hc
        } else {
            let end = json!({"at": "x"});
            crate::state::evidence::publish(&gens::path(&b, 2, "ended"), &end).unwrap();
        }
        cx::terminates(&w, B);
        cx::terminates(&w, A);
        assert!(finish(&tx, &c).unwrap()[0].stopped, "{row}");
        assert_eq!(
            kill_order(&w),
            [cx::addr(B), cx::addr(A)],
            "{row}: B, then A"
        );
        let notices = (cx::delivered_at(&w, B, 2), cx::delivered(&w, A));
        assert_eq!(notices, (1, 1), "{row}");
        w.fakes.hc_done().unwrap();
    }
}

// Spec "Revalidate before kill" (G11.3): the terminal turns Lost or changes argv between the match and the kill:
// no kill, unresolved, the journal kept; the Lost one is released by the next pass, still with no kill.
#[test]
fn a_terminal_changed_before_the_kill_is_never_killed() {
    for case in ["lost", "argv"] {
        let (w, s) = lf::world();
        cx::member(&w, L, None, 0, json!({}));
        cx::member(&w, A, Some(L), 1, json!({}));
        let argv: &[&str] = if case == "lost" {
            lf::LOST_ARGVS[0]
        } else {
            &["/bin/other"]
        };
        cx::rereads(&w, A, &cx::wire(&w, A, "live", None, argv));
        let (tx, c) = (cx::tx(&w, &s), w.caller());
        let r = stop(&tx, &c, A, &[]).unwrap();
        assert!(!r.stopped && r.unresolved[0].check == "hc", "{case}: {r:?}");
        assert_eq!(lf::count(&w, "kill"), 0, "{case}");
        assert!(cx::journal_of(&w, A).exists(), "{case}");
        w.fakes.hc_done().unwrap();
        if case == "lost" {
            cx::releases(&w, A, lf::LOST_ARGVS[0]);
            assert!(finish(&tx, &c).unwrap()[0].stopped);
            assert_eq!(lf::count(&w, "kill"), 0, "released, never killed");
            w.fakes.hc_done().unwrap();
        }
    }
}

// Spec "Released shell": A released (release-1d R1); stop A, then the team (L): both finish; the shell is never
// asked of or touched.
#[test]
fn a_released_shell_is_never_touched() {
    let (w, s) = lf::world();
    cx::member(&w, L, None, 0, json!({}));
    let a = cx::member(&w, A, Some(L), 1, json!({}));
    let rel =
        json!({"address": cx::addr(A), "hcName": lf::hc_name(1), "cause": "recovery", "at": "x"});
    crate::state::evidence::publish(&gens::path(&a, 1, "release"), &rel).unwrap();
    crate::state::evidence::publish(&gens::path(&a, 1, "released"), &json!({"at": "x"})).unwrap();
    let (tx, c) = (cx::tx(&w, &s), w.caller());
    assert!(stop(&tx, &c, A, &[]).unwrap().stopped, "stop A");
    cx::terminates(&w, L);
    assert!(stop(&tx, &c, L, &[]).unwrap().stopped, "stop the team");
    let touched = lf::calls(&w).iter().any(|call| call.contains(&cx::addr(A)));
    assert!(!touched, "the shell untouched");
    assert_eq!(cx::delivered(&w, A), 1);
    w.fakes.hc_done().unwrap();
}

// Spec "Lost stop" (release-1d R2, b-calls 2): A is Lost; stop A releases it (`cause: lost`), sends no kill, send
// or keys; the cascade completes. Both recovery shells (POSIX and Windows argv) on every OS.
#[test]
fn a_lost_member_is_released_never_killed() {
    for argv in lf::LOST_ARGVS {
        let (w, s) = lf::world();
        cx::member(&w, L, None, 0, json!({}));
        let a = cx::member(&w, A, Some(L), 1, json!({}));
        cx::releases(&w, A, argv);
        let (tx, c) = (cx::tx(&w, &s), w.caller());
        assert!(stop(&tx, &c, A, &[]).unwrap().stopped, "{argv:?}");
        let rel = gens::read(&a, 1, "release").known().unwrap().unwrap();
        assert_eq!(rel["cause"], "lost", "{argv:?}");
        assert!(
            gens::read(&a, 1, "released").known().unwrap().is_some(),
            "{argv:?}"
        );
        lf::no_effect(&w);
        assert_eq!(cx::delivered(&w, A), 1, "{argv:?}");
        w.fakes.hc_done().unwrap();
    }
}

// Call 9: a notice's message is flushed before its marker. The mailbox flush fails: the entry is unresolved,
// its end recorded, no marker; once the flush lands the next pass posts it, one delivery.
#[test]
fn a_notice_marker_never_outlives_its_message() {
    let (w, s) = lf::world();
    cx::member(&w, L, None, 0, json!({}));
    let a = cx::member(&w, A, Some(L), 1, json!({}));
    cx::terminates(&w, A);
    let key = crate::agents::mail::mailbox_key(cx::PANE, "").unwrap();
    let mailbox = w.state().join("mail").join(format!("{key}.jsonl"));
    let (tx, c) = (cx::tx(&w, &s), w.caller());
    crate::state::fs::fail_sync_file(Some(&mailbox));
    let r = stop(&tx, &c, A, &[]);
    crate::state::fs::fail_sync_file(None);
    assert!(!r.unwrap().stopped, "the delivery refused");
    assert!(
        gens::read(&a, 1, "ended").known().unwrap().is_some(),
        "the end is recorded"
    );
    let marker = gens::read(&a, 1, "notice").known().unwrap();
    assert!(marker.is_none(), "no marker without a flushed message");
    assert!(finish(&tx, &c).unwrap()[0].stopped);
    assert!(gens::read(&a, 1, "notice").known().unwrap().is_some());
    assert_eq!(cx::delivered(&w, A), 1, "one delivery");
    w.fakes.hc_done().unwrap();
}

// Call 22 (checklist 6): a marker already there still completes its publication's flushes before the entry counts
// as noticed. A's newest generation 2 is void (claim `abandoned`) and already has its marker; `gens/2` cannot be
// flushed: the entry is unresolved and the journal kept; with the fault armed the retry still refuses; cleared,
// the next pass completes once (no delivery: the marker decided).
#[test]
fn a_marker_already_there_is_flushed_before_it_counts() {
    let (w, s) = lf::world();
    cx::member(&w, L, None, 0, json!({}));
    let a = cx::member(&w, A, Some(L), 1, json!({}));
    cx::done(&w, A);
    lf::seed_gen(&a, 2, Some("abandoned"), None);
    let marker = json!({"kind": "stopped", "exitCode": null, "at": "x"});
    crate::state::evidence::publish(&gens::path(&a, 2, "notice"), &marker).unwrap();
    let (tx, c) = (cx::tx(&w, &s), w.caller());
    crate::state::fs::fail_sync_dir(Some(&gens::dir(&a, 2)));
    let r = stop(&tx, &c, A, &[]);
    let retry = finish(&tx, &c);
    crate::state::fs::fail_sync_dir(None);
    let r = r.unwrap();
    assert!(!r.stopped, "the marker's flush failed: {r:?}");
    assert_eq!(r.unresolved[0].check, "state", "{r:?}");
    assert!(
        !retry.unwrap()[0].stopped,
        "still refused while the fault holds"
    );
    assert!(cx::journal_of(&w, A).exists());
    assert!(finish(&tx, &c).unwrap()[0].stopped, "the flush landed");
    assert!(!cx::journal_of(&w, A).exists());
    assert_eq!(
        cx::delivered_at(&w, A, 2),
        0,
        "the marker decided: no delivery"
    );
    assert!(
        lf::calls(&w).is_empty(),
        "no hc: every generation done by its files"
    );
}

// Fix-cycle finding 13: B's committed pending generation 2 is Running; `stop A` is cut short right after
// `terminating.json` (B1's halt `terminating`, before any kill). The replay through the stop path (`finish`, then
// `stop` with the kept report, as `stop_hc` runs them) ends B 2, then B 1, then A; ancestors are kept until then;
// one notice for B (its highest generation); the journal is removed last.
#[test]
fn a_stop_cut_short_after_terminating_is_finished_once() {
    let (w, s) = lf::world();
    cx::member(&w, L, None, 0, json!({}));
    cx::member(&w, A, Some(L), 1, json!({}));
    let mut p = lf::pending(2);
    p["hcAddress"] = json!(cx::addr_at(B, 2));
    let b = cx::member(&w, B, Some(A), 2, json!({"pending": p}));
    lf::seed_gen(&b, 2, Some("run"), lf::committed());
    cx::found_at(&w, B, 2, &cx::wire_at(&w, B, 2, "live", None, &lf::ARGV));
    let (tx, c) = (cx::tx(&w, &s), w.caller());
    cx::at("terminating");
    assert_eq!(stop(&tx, &c, A, &[]).unwrap_err().check, "halted");
    assert!(gens::read(&b, 2, "terminating").known().unwrap().is_some());
    assert_eq!(lf::count(&w, "kill"), 0, "nothing killed before the crash");
    let kept = cx::journal_of(&w, A).exists() && cx::stopped(&w, A);
    assert!(kept, "ancestors kept and closed");
    w.fakes.hc_done().unwrap();
    for (id, n) in [(B, 2), (B, 1), (A, 1)] {
        cx::terminates_at(&w, id, n);
    }
    let kept = finish(&tx, &c).unwrap();
    assert!(stop(&tx, &c, A, &kept).unwrap().stopped);
    let order = [cx::addr_at(B, 2), cx::addr(B), cx::addr(A)];
    assert_eq!(kill_order(&w), order, "newest first, then A");
    let notices = (
        cx::delivered_at(&w, B, 2),
        cx::delivered_at(&w, B, 1),
        cx::delivered(&w, A),
    );
    assert_eq!(notices, (1, 0, 1), "one notice each");
    assert!(
        lf::record(&b).get("pending").is_none(),
        "pending dropped once"
    );
    assert!(!cx::journal_of(&w, A).exists(), "removed last");
    w.fakes.hc_done().unwrap();
}

// C7 (row 6.10): an entry counts complete only once its marker exists. A's generation 1 is ended but the record
// still names it `completing` (a completion not yet finished): `terminate` answers done, `post` publishes nothing
// (R3): the entry is unresolved `notice`, the journal kept; once the completion clears, the next pass notices it.
#[test]
fn an_entry_without_its_marker_keeps_the_journal() {
    let (w, s) = lf::world();
    cx::member(&w, L, None, 0, json!({}));
    let a = cx::member(&w, A, Some(L), 1, json!({"completing": {"gen": 1}}));
    crate::state::evidence::publish(&gens::path(&a, 1, "ended"), &json!({"at": "x"})).unwrap();
    let (tx, c) = (cx::tx(&w, &s), w.caller());
    let r = stop(&tx, &c, A, &[]).unwrap();
    let got: Vec<(&str, &str)> = (r.unresolved.iter())
        .map(|u| (u.check, u.reason.as_str()))
        .collect();
    assert_eq!(got, [("notice", NOT_NOTICED)], "{r:?}");
    assert!(!r.stopped && cx::journal_of(&w, A).exists());
    let mut rec = lf::record(&a);
    rec.as_object_mut().unwrap().remove("completing");
    crate::state::fs::write_json(&a.join("record.json"), &rec).unwrap();
    assert!(finish(&tx, &c).unwrap()[0].stopped);
    assert_eq!(cx::delivered(&w, A), 1);
    assert!(lf::calls(&w).is_empty(), "done by its files");
}

// Spec C2/C3: a spawn under an unprepared journal's member or after a whole-team stop: `closed`; reopening while
// a journal lists a descendant: `closed`; after completion: admitted, descendants still stopped. C0: a stopped
// parent with a live successor admits under the successor.
#[test]
fn admission_and_reopen_follow_the_closure() {
    let (w, s) = lf::world();
    cx::chain(&w);
    let tx = cx::tx(&w, &s);
    cx::seed(&w, A, &[C, B, A], false);
    let got = admit(&tx, B).unwrap_err();
    assert_eq!(got.check, "closed", "under an unprepared journal's member");
    admit(&tx, L).unwrap();
    remove(&tx, A).unwrap();
    for id in [A, B, C] {
        tx.desired(id, false).unwrap();
    }
    cx::seed(&w, B, &[C, B], true);
    let got = reopen_check(&tx, A).unwrap_err();
    let want = (
        "closed",
        "w-c is stopped; its sub-tree takes no new members",
    );
    assert_eq!((got.check, got.message.as_str()), want);
    remove(&tx, B).unwrap();
    reopen_check(&tx, A).unwrap();
    tx.desired(A, true).unwrap();
    let got = admit(&tx, B).unwrap_err();
    assert_eq!(
        got.check, "closed",
        "B stays stopped after its parent reopens"
    );
    assert!(cx::stopped(&w, B) && cx::stopped(&w, C));
    admit(&tx, A).unwrap();
    tx.desired(L, false).unwrap();
    let got = admit(&tx, L).unwrap_err();
    assert_eq!(
        got.check, "closed",
        "a whole-team stop: the root stays closed"
    );
    let got = reopen_check(&tx, A).unwrap_err();
    assert_eq!(got.check, "closed", "A's parent is closed");
    tx.desired(L, true).unwrap();
    tx.desired(A, false).unwrap();
    cx::member(&w, S, Some(L), 1, json!({"resumedFrom": A}));
    admit(&tx, A).unwrap();
}

// Call 29 (checklist 17, reopen/admission half): a `<root>.removed` tombstone is an unfinished removal; it still
// fences its sub-tree for admission and reopen until the removal completes.
#[test]
fn a_tombstone_still_fences_admission_and_reopen() {
    let (w, s) = lf::world();
    cx::chain(&w);
    let tx = cx::tx(&w, &s);
    cx::seed(&w, B, &[C, B], true);
    let f = cx::journal_of(&w, B);
    std::fs::rename(&f, f.with_extension("removed")).unwrap();
    assert_eq!(admit(&tx, C).unwrap_err().check, "closed", "admission");
    assert_eq!(reopen_check(&tx, A).unwrap_err().check, "closed", "reopen");
    remove(&tx, B).unwrap();
    admit(&tx, C).unwrap();
    reopen_check(&tx, A).unwrap();
}

// C3 same id: refused while a journal lists a descendant; otherwise `running` is durable before the launch runs.
#[test]
fn reopen_same_writes_running_before_the_launch() {
    let (w, s) = lf::world();
    cx::chain(&w);
    let tx = cx::tx(&w, &s);
    for id in [A, B, C] {
        tx.desired(id, false).unwrap();
    }
    cx::seed(&w, B, &[C, B], true);
    let got = reopen_same(&tx, A, || -> Res<()> { panic!("launched while closed") });
    assert_eq!(got.unwrap_err().check, "closed");
    assert!(cx::stopped(&w, A), "nothing written");
    remove(&tx, B).unwrap();
    reopen_same(&tx, A, || {
        assert!(!cx::stopped(&w, A), "running before the launch");
        Ok(())
    })
    .unwrap();
}

// Spec "Reopen crash boundaries", same id, in process: killed after `tx.desired`, then after `pending`. settle
// replays only A's intended pending generation, at most once; B and C stay stopped and are never launched.
#[test]
fn a_reopen_cut_short_replays_only_its_own_pending() {
    for cut in ["reopened", "pending"] {
        let (w, s) = lf::world();
        cx::chain(&w);
        cx::done(&w, A);
        let tx = cx::tx(&w, &s);
        for id in [A, B, C] {
            tx.desired(id, false).unwrap();
        }
        if cut == "reopened" {
            cx::at("reopened");
        }
        let got = reopen_same(&tx, A, || {
            let p = lf::pending(2);
            tx.record_write(
                A,
                crate::agents::team_state::Guard::PendingGen { n: None },
                |r| {
                    r.insert("pending".into(), p);
                    Ok(())
                },
            )?;
            super::super::release::halt("pending-test")
        });
        if cut == "pending" {
            assert!(got.is_ok(), "no halt set: the closure ran to its end");
        } else {
            assert_eq!(got.unwrap_err().check, "halted");
        }
        assert!(!cx::stopped(&w, A), "{cut}: running is durable");
        for _ in 0..2 {
            tx.settle(A, Mode::Normal).unwrap();
            assert!(lf::record(&w.folder(A)).get("pending").is_none(), "{cut}");
        }
        let claim = gens::claim(&w.folder(A), 2).known().unwrap();
        let want = if cut == "pending" {
            Some(json!("abandoned"))
        } else {
            None
        };
        assert_eq!(claim, want, "{cut}: abandoned once, or never pending");
        assert!(cx::stopped(&w, B) && cx::stopped(&w, C), "{cut}");
        assert!(lf::calls(&w).is_empty(), "{cut}: nothing launched");
    }
}

// Call 11 as call 35 reads it (C2 root): a stopped root takes no new members. After its whole cascade, with the
// finished journal removed, a spawn under L and a resume of its stopped child A refuse `closed`; resuming L itself
// reopens it: `running` before its launch.
#[test]
fn a_stopped_root_takes_no_new_members_but_reopens_itself() {
    let (w, s) = lf::world();
    cx::member(&w, L, None, 0, json!({}));
    cx::member(&w, A, Some(L), 1, json!({}));
    cx::terminates(&w, A);
    cx::terminates(&w, L);
    let (tx, c) = (cx::tx(&w, &s), w.caller());
    assert!(stop(&tx, &c, L, &[]).unwrap().stopped, "stopped completely");
    assert!(
        journals(&tx).unwrap().is_empty(),
        "the finished journal is removed"
    );
    assert_eq!(admit(&tx, L).unwrap_err().check, "closed", "spawn under L");
    let got = reopen_same(&tx, A, || -> Res<()> {
        panic!("launched under a stopped root")
    });
    assert_eq!(got.unwrap_err().check, "closed", "resume of A");
    assert!(cx::stopped(&w, A), "A stays stopped");
    reopen_same(&tx, L, || {
        assert!(!cx::stopped(&w, L), "running before the launch");
        Ok(())
    })
    .unwrap();
    w.fakes.hc_done().unwrap();
}

/// A prepared journal of A whose only entry is already ended and noticed: the next pass only removes it.
fn complete(w: &World) {
    cx::member(w, L, None, 0, json!({}));
    cx::member(w, A, Some(L), 1, json!({}));
    cx::done(w, A);
    cx::seed(w, A, &[A], true);
}

// C8: one try of the team lock finishes a kept journal; a busy lock is skipped at once (no wait, no error).
#[test]
fn the_bar_tick_tries_the_team_lock_once() {
    let (w, s) = lf::world();
    complete(&w);
    w.fakes.lock_timeout(&s.team_lock());
    assert!(
        crate::agents::adopt::tick(&w.ctx, &w.caller())
            .unwrap()
            .is_none(),
        "busy: skipped"
    );
    assert!(cx::journal_of(&w, A).exists());
    assert!(
        w.fakes.locks().contains(&format!("?{}", s.team_lock())),
        "one try, recorded busy"
    );
    let (w, _) = lf::world();
    complete(&w);
    assert!(
        crate::agents::adopt::tick(&w.ctx, &w.caller())
            .unwrap()
            .is_some()
    );
    assert!(!cx::journal_of(&w, A).exists(), "finished");
    let (w, _) = lf::world();
    assert!(
        crate::agents::adopt::tick(&w.ctx, &w.caller())
            .unwrap()
            .is_none(),
        "no journal folder: no lock taken"
    );
    assert!(w.fakes.locks().is_empty());
}

// C8 "stop or a bar tick finishes it, no deadlock": real locks (Unix flock, Windows named mutex); a tick, then a
// stop of another member, each take the team lock once and return.
#[test]
fn stop_and_tick_never_deadlock() {
    let (w, s) = lf::world();
    w.fakes.real_locks(&w._dir.path().join("locks"));
    complete(&w);
    assert!(
        crate::agents::adopt::tick(&w.ctx, &w.caller())
            .unwrap()
            .is_some()
    );
    cx::member(&w, B, Some(A), 2, json!({}));
    cx::terminates(&w, B);
    let c = w.caller();
    let r = crate::agents::team_state::open(&w.ctx, &s, |tx| {
        let kept = finish(tx, &c)?;
        stop(tx, &c, B, &kept)
    })
    .unwrap();
    assert!(r.stopped);
    w.fakes.hc_done().unwrap();
}

// Call 21 (checklist 1): the bar's view never refuses. A parent loop marks only its members; the rest are placed.
// A torn record marks that member, and no member is placed (the torn one may be a successor), never "absent".
#[test]
fn the_bars_view_marks_a_bad_member_and_never_refuses() {
    let (w, s) = lf::world();
    cx::member(&w, L, None, 0, json!({}));
    cx::member(&w, A, Some(L), 1, json!({}));
    cx::member(&w, B, Some(C), 1, json!({}));
    cx::member(&w, C, Some(B), 1, json!({}));
    let tx = cx::tx(&w, &s);
    let v = Tree::shown(&tx);
    assert_eq!(
        [L, A, B, C].map(|x| v.bad(x)),
        [false, false, true, true],
        "loop: bad"
    );
    assert_eq!(v.placed(A), Some(vec![L.to_string()]), "loop: A placed");
    assert_eq!((v.placed(B), v.placed(C)), (None, None), "loop: raw");
    let (w, s) = lf::world();
    cx::chain(&w);
    std::fs::write(w.folder(A).join("record.json"), b"{").unwrap();
    let tx = cx::tx(&w, &s);
    let v = Tree::shown(&tx);
    assert!(v.bad(A), "torn: marked");
    assert_eq!(
        [L, B, C].map(|x| (v.bad(x), v.placed(x))),
        [(false, None), (false, None), (false, None)],
        "torn: nothing placed, nothing else marked"
    );
}

// Integration (#150 CI, live `list_all_json_shape`): the Stopped notice is posted from the presented record, as
// main's `stop_life` posted it, so its line carries the pane the member's own view.json presents: the `<from>|<pane>`
// the board counts the stopped row's unread under. The raw record holds no pane (views-6f V1).
#[test]
fn a_stopped_member_is_noticed_from_the_pane_its_view_presents() {
    let (w, s) = lf::world();
    cx::member(&w, L, None, 0, json!({}));
    let a = cx::member(&w, A, Some(L), 1, json!({}));
    let view = json!({"gen": 1, "hcAddress": cx::addr(A), "hcName": lf::hc_name(1), "pane": "%7",
        "window": "@1", "startCommand": format!("hc attach {}", cx::addr(A)),
        "serverKey": w.caller().server_key, "session": w.caller().session, "attachProgram": "hc"});
    crate::state::fs::write_json(&a.join("view.json"), &view).unwrap();
    cx::terminates(&w, A);
    let tx = cx::tx(&w, &s);
    let j = prepare(&tx, A).unwrap();
    let r = execute(&tx, &caller(&w), &j).unwrap();
    assert!(r.stopped && r.unresolved.is_empty(), "{r:?}");
    let key = crate::agents::mail::mailbox_key(cx::PANE, "").unwrap();
    let text =
        std::fs::read_to_string(w.state().join("mail").join(format!("{key}.jsonl"))).unwrap();
    let want = gens::delivery_id(A, 1);
    let line: Vec<serde_json::Value> = (text.lines())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|m| m["id"] == want.as_str())
        .collect();
    assert_eq!(line.len(), 1, "{text}");
    assert_eq!(
        (&line[0]["from"], &line[0]["pane"]),
        (&json!(cx::name(A)), &json!("%7")),
        "{text}"
    );
    w.fakes.hc_done().unwrap();
}
