use super::*;
use crate::agents::adopt_fixture as ax;
use crate::agents::cascade_fixture as cx;
use crate::agents::fixture::World;
use crate::agents::life_fixture as lf;
use crate::agents::team_state::Scope;
use crate::state::fs::{fail_sync_dir, fail_sync_file};
use serde_json::json;
use std::path::Path;

fn world() -> (World, Scope) {
    lf::world()
}
fn adopt_dir(w: &World, s: &Scope) -> PathBuf {
    let d = dir_at(&w.ctx, s);
    std::fs::create_dir_all(&d).unwrap();
    d
}
fn put(w: &World, s: &Scope, name: &str, body: Value) {
    crate::state::fs::write_json(&adopt_dir(w, s).join(name), &body).unwrap();
}
fn with(f: impl FnOnce(&mut Value)) -> Value {
    let mut b = ax::to_a(1).body();
    f(&mut b);
    b
}
/// Replaces the folder `at` with a link to an empty folder elsewhere.
fn linked(w: &World, at: &Path) {
    let t = w._dir.path().join("elsewhere");
    std::fs::create_dir_all(&t).unwrap();
    let _ = std::fs::remove_dir_all(at);
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    crate::testing::link_dir(&t, at);
}
/// Every listed journal read strictly (what replay reads of the current one).
fn read_all(w: &World, s: &Scope) -> Res<Vec<Journal>> {
    let l = listing(&w.ctx, s)?;
    l.journals
        .iter()
        .map(|n| read_journal(&w.ctx, s, *n))
        .collect()
}
fn up(d: &Path, n: usize) -> PathBuf {
    d.ancestors().nth(n).unwrap().to_path_buf()
}
/// Every file below `d`, sorted.
fn files_under(d: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(d).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(files_under(&p))
        } else {
            out.push(p)
        }
    }
    out.sort();
    out
}

type Bad = (&'static str, fn(&World, &Scope));
const T32: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BAD: [Bad; 15] = [
    ("leading zero", |w, s| {
        put(w, s, "1.json", ax::to_a(1).body()); // a whole journal 1 beside it: only the name can refuse
        put(w, s, "01.json", ax::to_a(1).body())
    }),
    ("not a number", |w, s| {
        put(w, s, "x.json", ax::to_a(1).body())
    }),
    ("temp-named folder", |w, s| {
        std::fs::create_dir(adopt_dir(w, s).join(format!("1.json.{T32}.tmp"))).unwrap()
    }),
    ("tombstone folder", |w, s| {
        std::fs::create_dir(adopt_dir(w, s).join("1.removed")).unwrap()
    }),
    ("seq differs", |w, s| {
        put(w, s, "2.json", ax::to_a(1).body())
    }),
    ("extra key", |w, s| {
        put(w, s, "1.json", with(|b| b["x"] = json!(1)))
    }),
    ("change key extra", |w, s| {
        put(w, s, "1.json", with(|b| b["changes"][0]["x"] = json!(1)))
    }),
    ("change key missing", |w, s| {
        put(
            w,
            s,
            "1.json",
            with(|b| {
                b["changes"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("parentPane");
            }),
        )
    }),
    ("pane not text", |w, s| {
        put(
            w,
            s,
            "1.json",
            with(|b| b["changes"][0]["parentPane"] = json!(5)),
        )
    }),
    ("torn", |w, s| {
        std::fs::write(adopt_dir(w, s).join("1.json"), "{\"seq\":").unwrap()
    }),
    ("folder at the journal", |w, s| {
        std::fs::create_dir_all(adopt_dir(w, s).join("1.json")).unwrap()
    }),
    ("depth not a number", |w, s| {
        put(
            w,
            s,
            "1.json",
            with(|b| b["changes"][0]["depth"] = json!("2")),
        )
    }),
    ("linked adopt", |w, s| linked(w, &dir_at(&w.ctx, s))),
    ("linked teams", |w, s| linked(w, &up(&dir_at(&w.ctx, s), 1))),
    ("linked key", |w, s| linked(w, &up(&dir_at(&w.ctx, s), 2))),
];

// AD-R4, call 15: names and bodies read strictly; a temp file or a tombstone is skipped or listed only as a regular
// file; a name that vanished since the listing is gone, never `evidence` (call 14).
#[test]
fn journal_reads_refuse_what_is_not_a_journal() {
    for (case, plant) in BAD {
        let (w, s) = world();
        plant(&w, &s);
        assert_eq!(read_all(&w, &s).unwrap_err().check, "evidence", "{case}");
    }
    let (w, s) = world();
    std::fs::write(adopt_dir(&w, &s).join(format!("1.json.{T32}.tmp")), "{").unwrap();
    std::fs::write(adopt_dir(&w, &s).join("2.removed"), "").unwrap();
    let l = listing(&w.ctx, &s).unwrap();
    assert_eq!(
        (l.journals, l.tombstones),
        (vec![], vec![2]),
        "temp skipped, tombstone listed"
    );
    assert!(
        !kept(&adopt_dir(&w, &s).join("9.removed")).unwrap(),
        "vanished: gone"
    );
}

// Call 14: a lock-free listing reads names only; a torn journal is listed, its body never opened.
#[test]
fn a_listing_reads_names_only() {
    let (w, s) = world();
    std::fs::write(adopt_dir(&w, &s).join("1.json"), "{").unwrap();
    let (l, reads) = crate::state::fs::record_reads(|| listing(&w.ctx, &s).unwrap());
    assert_eq!(l.journals, vec![1]);
    assert!(
        !reads.iter().any(|p| p.starts_with(dir_at(&w.ctx, &s))),
        "no body read: {reads:?}"
    );
}

// A4.2: create-once; an identical retry is this journal; another body of the seq refuses.
#[test]
fn a_journal_round_trips_and_publishes_once() {
    let (w, s) = world();
    let tx = cx::tx(&w, &s);
    publish(&tx, &ax::to_a(1)).unwrap();
    assert_eq!(read_all(&w, &s).unwrap(), vec![ax::to_a(1)]);
    publish(&tx, &ax::to_a(1)).unwrap();
    let mut other = ax::to_a(1);
    other.changes[0].depth = 5;
    assert_eq!(
        publish(&tx, &other).unwrap_err().message,
        OUT_OF_ORDER,
        "another body"
    );
    remove(&tx, 1).unwrap();
    remove(&tx, 1).unwrap();
    assert_eq!(listing(&w.ctx, &s).unwrap(), Listing::default());
}

// A4: `{seq}` read strictly; absent is 0; `commit` makes `teams` itself (finding 1-14).
#[test]
fn committed_is_three_valued() {
    let (w, s) = world();
    assert_eq!(committed(&w.ctx, &s).unwrap(), 0, "absent");
    commit(&cx::tx(&w, &s), 3).unwrap();
    assert_eq!(committed(&w.ctx, &s).unwrap(), 3);
    for (case, body) in [
        ("extra", r#"{"seq":3,"x":1}"#),
        ("text", r#"{"seq":"3"}"#),
        ("torn", r#"{"se"#),
    ] {
        std::fs::write(seq_path(&w.ctx, &s), body).unwrap();
        assert_eq!(
            committed(&w.ctx, &s).unwrap_err().check,
            "evidence",
            "{case}"
        );
    }
    std::fs::remove_file(seq_path(&w.ctx, &s)).unwrap();
    std::fs::create_dir(seq_path(&w.ctx, &s)).unwrap();
    assert_eq!(
        committed(&w.ctx, &s).unwrap_err().check,
        "evidence",
        "folder at the seq file"
    );
}

// AD-R5, calls 6, 14, 26 (checklist 4): every flush point, one row each. Armed, the step fails twice (a retry
// repeats the flush, also when the folder or file already exists or the journal is already a tombstone); cleared,
// it completes. `republish` is an identical retry over an existing journal: its own content is flushed again.
#[test]
fn every_adoption_flush_is_repeated_until_it_lands() {
    let rows = [
        ("publish", "key"),
        ("publish", "teams"),
        ("publish", "content"),
        ("publish", "adopt"),
        ("republish", "content"),
        ("commit", "key"),
        ("commit", "content"),
        ("commit", "teams"),
        ("remove", "adopt"),
    ];
    for (step, at) in rows {
        let (w, s) = world();
        let tx = cx::tx(&w, &s);
        if step == "remove" || step == "republish" {
            publish(&tx, &ax::to_a(1)).unwrap();
        }
        let dir = dir_at(&w.ctx, &s);
        let file = match step {
            "commit" => seq_path(&w.ctx, &s),
            _ => dir.join("1.json"),
        };
        match at {
            "key" => fail_sync_dir(Some(&up(&dir, 2))),
            "teams" => fail_sync_dir(Some(&up(&dir, 1))),
            "adopt" => fail_sync_dir(Some(&dir)),
            _ => fail_sync_file(Some(&file)),
        }
        let run = || match step {
            "publish" | "republish" => publish(&tx, &ax::to_a(1)),
            "commit" => commit(&tx, 1),
            _ => remove(&tx, 1),
        };
        for attempt in [1, 2] {
            let e = run().unwrap_err();
            assert_eq!(
                (e.check, e.message.as_str()),
                ("state", NOT_WRITTEN),
                "{step} {at} #{attempt}"
            );
            if step == "remove" {
                assert_eq!(
                    listing(&w.ctx, &s).unwrap().tombstones,
                    vec![1],
                    "remove: the tombstone stays"
                );
            }
        }
        fail_sync_dir(None);
        fail_sync_file(None);
        run().unwrap();
        let l = listing(&w.ctx, &s).unwrap();
        let want = if step == "remove" || step == "commit" {
            vec![]
        } else {
            vec![1]
        };
        assert_eq!((l.journals, l.tombstones), (want, vec![]), "{step} {at}");
        assert_eq!(
            committed(&w.ctx, &s).unwrap(),
            u64::from(step == "commit"),
            "{step} {at}"
        );
    }
}

// AD-R5 (checklist 5): `remove` flushes `.adopt` after the rename and again after the delete, on every call; the
// ordered flush log tells the second flush from the first.
#[test]
fn a_removal_flushes_adopt_after_the_rename_and_after_the_delete() {
    let (w, s) = world();
    let tx = cx::tx(&w, &s);
    publish(&tx, &ax::to_a(1)).unwrap();
    let dir = dir_at(&w.ctx, &s);
    for call in ["first", "retry"] {
        let (got, syncs) = crate::state::fs::record_syncs(|| remove(&tx, 1));
        got.unwrap();
        let adopt: Vec<_> = syncs.iter().filter(|p| **p == dir).collect();
        assert_eq!(adopt.len(), 2, "{call}: {syncs:?}");
        assert_eq!(
            syncs.last(),
            Some(&dir),
            "{call}: the last flush is after the delete"
        );
    }
}

// Calls 6, 15 (finding 1-4): every read and write walks `<key>`, `teams` and `.adopt` from the state root; a link at
// any level refuses `evidence` and nothing is written or removed through it.
#[test]
fn every_level_is_walked_from_the_state_root() {
    for (level, n) in [("key", 2), ("teams", 1), ("adopt", 0)] {
        let (w, s) = world();
        publish(&cx::tx(&w, &s), &ax::to_a(1)).unwrap();
        commit(&cx::tx(&w, &s), 1).unwrap();
        let dir = dir_at(&w.ctx, &s);
        linked(&w, &up(&dir, n));
        // A whole journal behind the link: a read or removal that skips the walk would reach it.
        let rel = dir.strip_prefix(up(&dir, n)).unwrap().to_path_buf();
        let planted = w._dir.path().join("elsewhere").join(rel).join("1.json");
        std::fs::create_dir_all(planted.parent().unwrap()).unwrap();
        crate::state::fs::write_json(&planted, &ax::to_a(1).body()).unwrap();
        let tx = cx::tx(&w, &s);
        let got = [
            ("listing", listing(&w.ctx, &s).map(drop)),
            ("publish", publish(&tx, &ax::to_a(2))),
            ("remove", remove(&tx, 1)),
            ("read", read_journal(&w.ctx, &s, 1).map(drop)),
        ];
        for (op, r) in got {
            assert_eq!(r.unwrap_err().check, "evidence", "{level}: {op}");
        }
        if level != "adopt" {
            assert_eq!(
                committed(&w.ctx, &s).unwrap_err().check,
                "evidence",
                "{level}: committed"
            );
            assert_eq!(
                commit(&tx, 2).unwrap_err().check,
                "evidence",
                "{level}: commit"
            );
        }
        assert_eq!(
            files_under(&w._dir.path().join("elsewhere")),
            vec![planted],
            "{level}: only the planted file"
        );
    }
}

// Calls 26, 27 (checklist 6): every journal, tombstone and seq write checks its final name first: a folder or a
// link there, or torn content at `topology.json`, refuses `evidence` before any rename or write.
#[test]
fn a_wrong_leaf_refuses_before_any_write() {
    // A folder at the tombstone: `remove` never renames the journal onto it.
    let (w, s) = world();
    let tx = cx::tx(&w, &s);
    publish(&tx, &ax::to_a(1)).unwrap();
    std::fs::create_dir(dir_at(&w.ctx, &s).join("1.removed")).unwrap();
    assert_eq!(
        remove(&tx, 1).unwrap_err().check,
        "evidence",
        "tombstone folder"
    );
    assert!(
        dir_at(&w.ctx, &s).join("1.json").is_file(),
        "tombstone folder: the journal stays"
    );
    // A folder or a link at the journal: `publish` refuses.
    for case in ["folder", "link"] {
        let (w, s) = world();
        let at = adopt_dir(&w, &s).join("1.json");
        match case {
            "folder" => std::fs::create_dir(&at).unwrap(),
            _ => linked(&w, &at),
        }
        let e = publish(&cx::tx(&w, &s), &ax::to_a(1)).unwrap_err();
        assert_eq!(e.check, "evidence", "journal {case}");
    }
    // Torn content, a folder or a link at `topology.json`: `commit` refuses and leaves it as it was.
    for case in ["torn", "folder", "link"] {
        let (w, s) = world();
        let tx = cx::tx(&w, &s);
        commit(&tx, 1).unwrap();
        let p = seq_path(&w.ctx, &s);
        std::fs::remove_file(&p).unwrap();
        match case {
            "torn" => std::fs::write(&p, r#"{"se"#).unwrap(),
            "folder" => std::fs::create_dir(&p).unwrap(),
            _ => linked(&w, &p),
        }
        assert_eq!(commit(&tx, 2).unwrap_err().check, "evidence", "seq {case}");
        if case == "torn" {
            assert_eq!(
                std::fs::read_to_string(&p).unwrap(),
                r#"{"se"#,
                "torn: untouched"
            );
        }
    }
}
