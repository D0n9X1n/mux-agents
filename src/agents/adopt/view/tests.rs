//! adopt-3e Task 4: one `View`; the source (A2) and its nearest surviving ancestor, or why not (A3; calls 8-10).
use super::*;
use crate::agents::adopt_fixture as ax;
use crate::agents::cascade_fixture as cx;
use crate::agents::fixture::World;
use crate::agents::life_fixture as lf;
use crate::agents::team_state::Scope;
use serde_json::json;
use std::collections::BTreeSet;

fn world() -> World {
    lf::world().0
}
fn tx(w: &World) -> Tx<'_> {
    cx::tx(w, &Scope::of(&w.caller()))
}
fn view(w: &World) -> View {
    View::read(&tx(w)).unwrap()
}
fn stop(w: &World, id: &str) {
    tx(w).desired(id, false).unwrap();
}
fn released_only(w: &World, id: &str) {
    let m = w.folder(id);
    let _ = std::fs::remove_file(gens::path(&m, 1, "ended"));
    let p = gens::path(&m, 1, "released");
    crate::state::evidence::publish(&p, &json!({"cause": "lost"})).unwrap();
}
fn no_end(w: &World) {
    std::fs::remove_file(gens::path(&w.folder(ax::E), 1, "ended")).unwrap();
}
/// Checklist 8: an older generation Ended, the active one released only.
fn older_ended(w: &World) {
    let m = w.folder(ax::E);
    lf::seed_gen(&m, 2, Some("run"), lf::committed());
    let p = gens::path(&m, 2, "released");
    crate::state::evidence::publish(&p, &json!({"cause": "lost"})).unwrap();
    ax::set(w, ax::E, "generation", json!("2"));
}
type Plant = (&'static str, fn(&World), &'static str, bool);

// A2, ambiguities 1-4, 16 (calls 8, 18): each condition removed alone changes one row.
#[test]
fn a_source_meets_every_condition() {
    use ax::{E, E2, Kind};
    let cases: [Plant; 17] = [
        ("ended", |_| {}, E, true),
        ("live", no_end, E, false),
        ("released only", |w| released_only(w, E), E, false),
        ("older ended, active released", older_ended, E, false),
        (
            "successor",
            |w| ax::successor(w, E2, E, cx::A, 2, Kind::Committed),
            E,
            false,
        ),
        (
            "pending successor",
            |w| ax::successor(w, E2, E, cx::A, 2, Kind::Pending),
            E,
            false,
        ),
        (
            "released successor",
            |w| ax::successor(w, E2, E, cx::A, 2, Kind::Released),
            E,
            false,
        ),
        (
            "void successor",
            |w| ax::successor(w, E2, E, cx::A, 2, Kind::Abandoned),
            E,
            true,
        ),
        (
            "aborted successor",
            |w| ax::successor(w, E2, E, cx::A, 2, Kind::Aborted),
            E,
            true,
        ),
        (
            "pending",
            |w| ax::set(w, E, "pending", lf::pending(2)),
            E,
            false,
        ),
        (
            "not done",
            |w| lf::seed_gen(&w.folder(E), 2, Some("run"), lf::committed()),
            E,
            false,
        ),
        (
            "completing",
            |w| ax::set(w, E, "completing", json!({"gen": 1})),
            E,
            false,
        ),
        ("stopped", |w| stop(w, E), E, false),
        ("ancestor stopped", |w| stop(w, cx::L), E, false),
        // Deepest first, the root last (cascade call 16; finding 1-15).
        (
            "journal",
            |w| drop(cx::seed(w, cx::A, &[cx::B, E, cx::A], false)),
            E,
            false,
        ),
        ("root", |w| ax::ended(w, cx::L), cx::L, false),
        (
            "leader without upTeam",
            |w| {
                ax::ended(w, cx::L);
                ax::set(w, cx::L, "upTeam", Value::Null)
            },
            cx::L,
            true,
        ),
    ];
    for (case, plant, id, want) in cases {
        let w = world();
        ax::team(&w);
        plant(&w);
        assert_eq!(view(&w).source(id).unwrap(), want, "{case}");
    }
}

fn ended_a(w: &World) {
    ax::ended(w, cx::A);
}
fn ended_a_and_l(w: &World) {
    ax::ended(w, cx::A);
    ax::ended(w, cx::L);
}
fn resumed_a(w: &World) {
    ax::ended(w, cx::A);
    ax::successor(w, ax::A2, cx::A, cx::L, 1, ax::Kind::Committed);
}
fn two_successors(w: &World) {
    ax::successor(w, ax::A2, cx::A, cx::L, 1, ax::Kind::Committed);
    ax::successor(w, "aaaaaaaaaaa3", cx::A, cx::L, 1, ax::Kind::Committed);
}
/// Finding 1-7: L pre-lifecycle and A Ended: A's chain leaves the tree; Ended A is no root.
fn ended_orphan(w: &World) {
    ax::ended(w, cx::A);
    ax::pre(w, cx::L, None, 0);
}
/// Checklist 2 (call 25): L pre-lifecycle and A live: A's own chain leaves the tree, so A adopts nothing.
fn pre_root(w: &World) {
    ax::pre(w, cx::L, None, 0);
}
fn pre_parent(w: &World) {
    ax::pre(w, cx::A, Some(cx::L), 1);
}
fn rootless_leader(w: &World) {
    ax::ended(w, cx::A);
    ax::ended(w, cx::L);
    ax::set(w, cx::L, "upTeam", Value::Null);
}
type Want = (&'static str, fn(&World), Result<&'static str, &'static str>);

// A3, ambiguities 3-5 (calls 8, 9, 25): the nearest ancestor not proven Ended, whose own chain reaches the
// depth-0 `upTeam` leader; Lost survives; only that leader adopts while Ended; otherwise the reason.
#[test]
fn the_nearest_surviving_ancestor_adopts() {
    let cases: [Want; 10] = [
        ("live parent", |_| {}, Ok(cx::A)),
        ("lost parent", |w| released_only(w, cx::A), Ok(cx::A)),
        ("ended non-root skipped", ended_a, Ok(cx::L)),
        ("ended root adopts", ended_a_and_l, Ok(cx::L)),
        ("resumed parent", resumed_a, Ok(ax::A2)),
        ("two successors", two_successors, Err(SEVERAL)),
        ("ended orphan", ended_orphan, Err(LEAVES)),
        ("pre-lifecycle root", pre_root, Err(LEAVES)),
        ("pre-lifecycle parent", pre_parent, Err(LEAVES)),
        ("leader without upTeam", rootless_leader, Err(LEAVES)),
    ];
    for (case, plant, want) in cases {
        let w = world();
        ax::team(&w);
        plant(&w);
        let got = match view(&w).adopter(ax::E).unwrap() {
            Up::To(id) => Ok(id),
            Up::Blocked(why) => Err(why),
        };
        assert_eq!(
            got.as_ref().map(String::as_str).map_err(|e| *e),
            want,
            "{case}"
        );
    }
}

// Call 10 (finding 1-9, checklist 1): `View::read` reads every file once: records come from `Tree::read`, each
// generation's marks once, also those `Tree::read` read for an (Abandoned) successor's void evidence.
#[test]
fn a_view_reads_each_file_once() {
    let w = world();
    ax::team(&w);
    ax::successor(&w, ax::E2, ax::E, cx::A, 2, ax::Kind::Abandoned);
    let ((_, records), files) = crate::state::evidence::record_evidence_reads(|| {
        crate::state::fs::record_reads(|| View::read(&tx(&w)).unwrap())
    });
    for (kind, reads) in [("record", &records), ("evidence", &files)] {
        let mut seen = BTreeSet::new();
        let twice: Vec<_> = reads
            .iter()
            .filter(|p| !seen.insert(p.to_path_buf()))
            .collect();
        assert!(twice.is_empty(), "{kind} read twice: {twice:?}");
    }
    assert!(
        records.contains(&w.folder(ax::E).join("record.json")),
        "E's record read"
    );
    let e2 = w.folder(ax::E2);
    assert!(
        files.iter().any(|p| p.starts_with(&e2)),
        "E2's evidence read: {files:?}"
    );
}
