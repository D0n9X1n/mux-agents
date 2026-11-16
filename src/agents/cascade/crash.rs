//! cascade-3d C-R6, process level: an ignored child runs a stop (or a reopen) and `crash_point` kills it (exit 86)
//! after one durable step; the parent finishes on the same files. Every child is bounded (`crash::LIMIT`).
use super::*;
use crate::agents::cascade_fixture::{self as cx, A, B, C, L, S};
use crate::agents::core_fixture as cf;
use crate::agents::core_launch::crash::{killed_at, plain, printed};
use crate::agents::core_launch::forward_tests::{free, launch_of, no_view};
use crate::agents::fixture::World;
use crate::agents::gens;
use crate::agents::life_fixture as lf;
use crate::agents::settle::Mode;
use crate::agents::team_state::Scope;
use serde_json::json;

/// (step, C ended and noticed before the stop). A step name fires at its first occurrence, so B's `kill`, `ended`
/// and `delivered` are reached by the child whose C is already done (as Task 6's in-process rows). b-call 54: the
/// crash at `kill` leaves `kill.json` only; call 29: `cascade-tombstoned` sits between the rename and the delete.
const STEPS: [(&str, bool); 17] = [
    ("cascade-journal", false),
    ("cascade-desired-ccccccccccc1", false),
    ("cascade-desired-bbbbbbbbbbb1", false),
    ("cascade-desired-aaaaaaaaaaa1", false),
    ("cascade-prepared", false),
    ("kill", false),
    ("ended", false),
    ("delivered", false),
    ("cascade-noticed-ccccccccccc1", false),
    ("kill", true),
    ("ended", true),
    ("delivered", true),
    ("cascade-noticed-bbbbbbbbbbb1", true),
    ("cascade-noticed-aaaaaaaaaaa1", false),
    ("cascade-tombstoned", false),
    ("cascade-removed", false),
    ("cascade-removed", true),
];

/// A world on the child's state root (same fake process table, so the same server key).
fn world_at(root: &std::path::Path) -> (World, Scope) {
    let mut w = World::human("");
    w.ctx.state_root = root.to_path_buf();
    let s = Scope::of(&w.caller());
    (w, s)
}
fn child_world() -> (World, Scope) {
    let mut w = World::human("");
    w.ctx.state_root = plain(&w.ctx.state_root);
    w.state();
    let s = Scope::of(&w.caller());
    println!("CRASH-ROOT {}", w.ctx.state_root.display());
    println!("CRASH-DIR {}", w._dir.path().display());
    (w, s)
}

/// L -> A -> B -> C (C done first when `c_done`), each running entry scripted to end; stop A.
fn stop_chain(c_done: bool) {
    let (w, s) = child_world();
    cx::chain(&w);
    if c_done {
        cx::done(&w, C);
    }
    for id in [C, B, A] {
        if !(c_done && id == C) {
            cx::terminates(&w, id);
        }
    }
    let _ = stop(&cx::tx(&w, &s), &w.caller(), A, &[]);
    panic!("no crash point was reached");
}

#[test]
#[ignore = "a child of a_stop_killed_after_each_durable_step_is_finished_by_the_next"]
fn child_cascade() {
    stop_chain(false);
}

#[test]
#[ignore = "a child of a_stop_killed_after_each_durable_step_is_finished_by_the_next"]
fn child_cascade_c_done() {
    stop_chain(true);
}

// Spec "Crash after each prepare write" and "Execute crash boundaries", real processes: B stays closed after the
// kill; the next pass finishes: every entry stopped, one end, one delivery, one marker, no second kill (a kill made
// here only for an entry with no `ended.json`), the journal removed only after every marker.
#[test]
fn a_stop_killed_after_each_durable_step_is_finished_by_the_next() {
    for (step, c_done) in STEPS {
        let child = if c_done {
            "agents::cascade::crash::child_cascade_c_done"
        } else {
            "agents::cascade::crash::child_cascade"
        };
        let kv = killed_at(child, step);
        let row = format!("{step} (C done: {c_done})");
        let (w, s) = world_at(&printed(&kv, "ROOT"));
        let tx = cx::tx(&w, &s);
        let js = journals(&tx).unwrap();
        let t = Tree::read(&tx).unwrap();
        let closed = t.closed_by(B, &|x| js.iter().any(|j| j.lists(x)));
        assert!(closed.unwrap().is_some(), "{row}: B closed");
        let mut fresh = Vec::new();
        for id in [C, B, A] {
            if gens::read(&w.folder(id), 1, "ended")
                .known()
                .unwrap()
                .is_none()
            {
                cx::terminates(&w, id);
                fresh.push(id);
            }
        }
        let c = w.caller();
        let kept = finish(&tx, &c).unwrap();
        assert!(stop(&tx, &c, A, &kept).unwrap().stopped, "{row}");
        for id in [C, B, A] {
            assert!(cx::stopped(&w, id), "{row}: {id}");
            let marker = gens::read(&w.folder(id), 1, "notice").known().unwrap();
            assert!(marker.is_some(), "{row}: {id} marker");
            let once = usize::from(!(c_done && id == C));
            assert_eq!(cx::delivered(&w, id), once, "{row}: {id} one delivery");
            let kills = usize::from(fresh.contains(&id));
            assert_eq!(cx::kills(&w, id), kills, "{row}: {id} no second kill");
        }
        assert!(
            !cx::journal_of(&w, A).exists() && !waiting(&w.ctx, &s),
            "{row}"
        );
        w.fakes.hc_done().unwrap();
        std::fs::remove_dir_all(printed(&kv, "DIR")).unwrap();
    }
}

/// L -> A -> B, A and B stopped; A's generation 1 ended and noticed.
fn stopped_pair(w: &World, s: &Scope) {
    cx::member(w, L, None, 0, json!({}));
    cx::member(w, A, Some(L), 1, json!({}));
    cx::member(w, B, Some(A), 2, json!({}));
    cx::done(w, A);
    let tx = cx::tx(w, s);
    tx.desired(A, false).unwrap();
    tx.desired(B, false).unwrap();
}

#[test]
#[ignore = "a child of a_reopen_killed_at_each_boundary_replays_only_its_own_pending"]
fn child_reopen_same() {
    let (w, s) = child_world();
    stopped_pair(&w, &s);
    cf::online(&w);
    let mut l = launch_of(&w, false, lf::SID);
    l.id = A.into();
    let _ = crate::agents::team_state::open(&w.ctx, &s, |tx| {
        reopen_same(tx, A, || {
            let core = crate::hc::connect(tx.ctx)?;
            crate::agents::core_launch::forward(tx, &core, l, free, no_view)
        })
    });
    panic!("no crash point was reached");
}

#[test]
#[ignore = "a child of a_reopen_killed_at_each_boundary_replays_only_its_own_pending"]
fn child_successor() {
    let (w, s) = child_world();
    stopped_pair(&w, &s);
    cf::online(&w);
    let mut l = launch_of(&w, true, lf::SID);
    l.id = S.into();
    let create = l.create.as_mut().unwrap();
    create["resumedFrom"] = json!(A);
    create["parentAgent"] = json!({"id": L});
    create["depth"] = json!(1);
    let _ = crate::agents::team_state::open(&w.ctx, &s, |tx| {
        let core = crate::hc::connect(tx.ctx)?;
        crate::agents::core_launch::forward(tx, &core, l, free, no_view)
    });
    panic!("no crash point was reached");
}

// Spec "Reopen crash boundaries": each time settle-1b G3 replays only the intended `pending` (A n+1 or S), at most
// once; B stays stopped and is never launched; a reopened member reads `running`.
#[test]
fn a_reopen_killed_at_each_boundary_replays_only_its_own_pending() {
    let cases = [
        ("child_reopen_same", "reopened", A, None),
        ("child_reopen_same", "pending", A, Some(2)),
        ("child_successor", "pending", S, Some(0)),
        ("child_successor", "desired", S, Some(0)),
    ];
    for (child, step, id, abandoned) in cases {
        let kv = killed_at(&format!("agents::cascade::crash::{child}"), step);
        let (w, s) = world_at(&printed(&kv, "ROOT"));
        let tx = cx::tx(&w, &s);
        let row = format!("{child} {step}");
        assert!(
            !cx::stopped(&w, id),
            "{row}: running is durable (absent reads running)"
        );
        for _ in 0..2 {
            tx.settle(id, Mode::Normal).unwrap();
            assert!(lf::record(&w.folder(id)).get("pending").is_none(), "{row}");
        }
        if let Some(n) = abandoned {
            let claim = gens::claim(&w.folder(id), n).known().unwrap();
            assert_eq!(claim, Some(json!("abandoned")), "{row}: abandoned once");
        } else {
            assert_eq!(
                gens::named(&w.folder(id)).unwrap(),
                [1],
                "{row}: no generation 2"
            );
        }
        assert!(cx::stopped(&w, B), "{row}: B stays stopped");
        if id == S {
            assert!(cx::stopped(&w, A), "{row}: X stays stopped");
        }
        assert!(lf::calls(&w).is_empty(), "{row}: nothing launched");
        std::fs::remove_dir_all(printed(&kv, "DIR")).unwrap();
    }
}

/// L -> A -> B; B's committed pending generation 2 is Running at `t2-<B>` beside its generation 1.
fn pending_b(w: &World) -> std::path::PathBuf {
    cx::member(w, L, None, 0, json!({}));
    cx::member(w, A, Some(L), 1, json!({}));
    let mut p = lf::pending(2);
    p["hcAddress"] = json!(cx::addr_at(B, 2));
    let b = cx::member(w, B, Some(A), 2, json!({"pending": p}));
    lf::seed_gen(&b, 2, Some("run"), lf::committed());
    b
}

/// `stop w-a` through the public dispatcher (`cli::parse`, then the verb's handler), as a person runs it.
fn stop_a(w: &World) -> crate::error::Res<serde_json::Map<String, serde_json::Value>> {
    let args = ["stop", cx::name(A).as_str()].map(str::to_string);
    let (def, request) = crate::cli::parse(&args).unwrap();
    (def.handler)(&w.ctx, &request)
}

#[test]
#[ignore = "a child of a_stop_killed_at_terminating_through_the_dispatcher_is_finished_once"]
fn child_dispatch_terminating() {
    let (w, _) = child_world();
    pending_b(&w);
    cx::found_at(&w, B, 2, &cx::wire_at(&w, B, 2, "live", None, &lf::ARGV));
    let _ = stop_a(&w);
    panic!("no crash point was reached");
}

// Checklist 13: a real process running the public `stop` is killed at `terminating` of B's Running pending
// generation 2 (`terminating.json` written, nothing killed). The next public `stop` finishes: B 2, then B 1, then
// A, ancestors kept until then, one notice for B (its highest generation) and one for A, pending dropped once, the
// journal removed last.
#[test]
fn a_stop_killed_at_terminating_through_the_dispatcher_is_finished_once() {
    let kv = killed_at(
        "agents::cascade::crash::child_dispatch_terminating",
        "terminating",
    );
    let (w, s) = world_at(&printed(&kv, "ROOT"));
    let b = w.folder(B);
    assert!(gens::read(&b, 2, "terminating").known().unwrap().is_some());
    assert!(
        gens::read(&b, 2, "ended").known().unwrap().is_none(),
        "nothing ended"
    );
    let kept = cx::journal_of(&w, A).exists() && cx::stopped(&w, A) && cx::stopped(&w, B);
    assert!(kept, "ancestors kept and closed");
    for (id, n) in [(B, 2), (B, 1), (A, 1)] {
        cx::terminates_at(&w, id, n);
    }
    let reply = stop_a(&w).unwrap();
    let got = (&reply["stopped"], &reply["unresolved"]);
    assert_eq!(got, (&json!(true), &json!([])), "{reply:?}");
    let kills: Vec<String> = (lf::calls(&w).into_iter())
        .filter(|c| c[0] == "kill")
        .map(|c| c[1].clone())
        .collect();
    let order = [cx::addr_at(B, 2), cx::addr(B), cx::addr(A)];
    assert_eq!(kills, order, "newest first, then A; no second kill");
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
    assert!(
        !cx::journal_of(&w, A).exists() && !waiting(&w.ctx, &s),
        "removed last"
    );
    w.fakes.hc_done().unwrap();
    std::fs::remove_dir_all(printed(&kv, "DIR")).unwrap();
}
