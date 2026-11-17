//! restore-6b Task 5: row 0, a member released for restore is relaunched once, within the cap (L1; ambiguity 6).
use super::core_fixture as cf;
use super::core_launch::forward_tests as ft;
use super::fixture::World;
use super::life_fixture as lf;
use super::reconcile_restore_tests::{L, other, sid_of};
use super::reconcile_run_tests::{ids, pass};
use super::reconcile_tests::{X, found, member, ok, released_for_restore, wire_of};
use serde_json::json;

/// `Snapshot::for_spawn`: the team listing, then one `get` per address (`core_facts::take`), in sorted order.
pub(super) fn snapshot(w: &World, live: &[&str], gone: &[&str]) {
    cf::listing(w, &lf::team(w), live);
    let mut all: Vec<&str> = live.iter().chain(gone).copied().collect();
    all.sort();
    for a in all {
        if gone.contains(&a) {
            lf::gone(w, a);
        } else {
            let id = &a[1..];
            cf::answer(
                w,
                &["get", a, "--json"],
                &wire_of(w, id, 1, &lf::ARGV).to_string(),
            );
        }
    }
}
/// Six running members `1..6` (ids `11111111111n`), each on its own conversation (`other`, r6b call 11: a filler
/// never reserves the conversation being restored) and adjudicated `ok`, each observed Running by its own `find`.
pub(super) fn six_running(w: &World) -> Vec<String> {
    (1..=6)
        .map(|n| {
            let id = format!("11111111111{n}");
            ok(&other(w, &id), 1);
            id
        })
        .collect()
}

#[test]
fn a_released_member_is_relaunched_once() {
    let w = World::human("");
    w.state();
    // the record names another session than the generation 1 launch: row 0 resumes the launch one (mutation 5.5)
    let f = member(&w, X, json!({"sessionId": sid_of("999999999999")}));
    ok(&f, 1);
    cf::online(&w);
    released_for_restore(&w, X);
    cf::online(&w);
    snapshot(&w, &[], &[&format!("a{X}")]);
    w.fakes.hc_answer(ft::address("t2"));
    let reply = pass(&w);
    assert_eq!(ids(&reply, "restored"), [X], "{reply:?}");
    assert_eq!(reply["restored"][0]["sessionId"], json!(lf::SID));
    assert_eq!(lf::record(&f)["generation"], "2");
    let spec = super::gens::read(&f, 2, "spec").known().unwrap().unwrap();
    assert_eq!(spec["args"], json!(["--resume", lf::SID]));
    // the next pass: gen 2 is Running (ready and adjudicated ok, so `observe` reads no spec deadline); nothing
    // launches again.
    ok(&f, 2);
    cf::online(&w);
    let launch = super::gens::read(&f, 2, "launch").known().unwrap().unwrap();
    let mut t = wire_of(&w, X, 2, &[]);
    (t["name"], t["argv"], t["address"]) = (
        launch["hcName"].clone(),
        launch["launcherArgv"].clone(),
        json!("t2"),
    );
    found(&w, X, 2, &t);
    assert!(ids(&pass(&w), "restored").is_empty());
    assert_eq!(lf::count(&w, "run"), 1);
    lf::no_effect(&w);
    w.fakes.hc_done().unwrap();
}

/// L1: X released for restore holds no slot; six running members fill the team: held `limit`. One ends: launched once.
#[test]
fn a_full_team_holds_a_released_member_until_a_slot_frees() {
    let w = World::human("");
    w.state();
    let fillers = six_running(&w);
    let f = member(&w, X, json!({}));
    ok(&f, 1);
    cf::online(&w);
    released_for_restore(&w, X);
    let addrs: Vec<String> = fillers.iter().map(|id| format!("a{id}")).collect();
    let live: Vec<&str> = addrs.iter().map(String::as_str).collect();
    let ax = format!("a{X}");
    cf::online(&w);
    for id in &fillers {
        found(&w, id, 1, &wire_of(&w, id, 1, &lf::ARGV));
    }
    snapshot(&w, &live, &[&ax]);
    let reply = pass(&w);
    assert_eq!(
        reply["held"],
        json!([{"id": X, "name": "mbbb", "reason": "limit"}]),
        "{reply:?}"
    );
    assert_eq!(lf::count(&w, "run"), 0);
    // filler 1 ends (its end recorded, as reap or a stop leaves it): the next pass launches X once.
    let end = super::gens::path(&w.folder(&fillers[0]), 1, "ended");
    crate::state::evidence::publish(&end, &json!({"at": "t"})).unwrap();
    cf::online(&w);
    for id in &fillers[1..] {
        found(&w, id, 1, &wire_of(&w, id, 1, &lf::ARGV));
    }
    snapshot(&w, &live[1..], &[live[0], &ax]);
    w.fakes.hc_answer(ft::address("t2"));
    assert_eq!(ids(&pass(&w), "restored"), [X]);
    assert_eq!(lf::count(&w, "run"), 1);
    w.fakes.hc_done().unwrap();
}

/// The leader takes no slot (L1, L2): a released leader in a full team is restored with no team listing.
#[test]
fn a_released_leader_takes_no_slot() {
    let w = World::human("");
    w.state();
    let fillers = six_running(&w);
    let f = member(&w, L, json!({"depth": 0, "upTeam": "T"}));
    ok(&f, 1);
    cf::online(&w);
    released_for_restore(&w, L);
    cf::online(&w);
    for id in &fillers {
        found(&w, id, 1, &wire_of(&w, id, 1, &lf::ARGV));
    }
    w.fakes.hc_answer(ft::address("t2"));
    assert_eq!(ids(&pass(&w), "restored"), [L]);
    let listings = lf::calls(&w)
        .iter()
        // the team listing (`ls .. mux.managed=1`); the `hc run` argv carries that label too
        .filter(|c| c[0] == "ls" && c.iter().any(|a| a == "mux.managed=1"))
        .count();
    assert_eq!(listings, 0, "no cap snapshot for the leader");
    w.fakes.hc_done().unwrap();
}

/// L1 pin (r6b call 12): a member with an active generation and a pending n+1 is one reservation (4 others + X = 5).
#[test]
fn a_pending_next_generation_counts_once() {
    let w = World::human("");
    w.state();
    let others: Vec<String> = (1..=4)
        .map(|n| {
            let id = format!("11111111111{n}");
            member(&w, &id, json!({}));
            id
        })
        .collect();
    member(
        &w,
        X,
        json!({"pending": {"gen": 2, "kind": "restore", "hcName": lf::hc_name(2),
        "sessionId": lf::SID, "launcherArgv": lf::ARGV}}),
    );
    let addrs: Vec<String> = others
        .iter()
        .chain([&X.to_string()])
        .map(|id| format!("a{id}"))
        .collect();
    let live: Vec<&str> = addrs.iter().map(String::as_str).collect();
    cf::online(&w);
    snapshot(&w, &live, &[]);
    let core = crate::hc::connect(&w.ctx).unwrap();
    let snap = super::membership::Snapshot::for_spawn(&w.ctx, &w.caller(), &core).unwrap();
    assert_eq!(super::tree::limit(&snap).map_err(|e| e.check), Ok(()));
    w.fakes.hc_done().unwrap();
}
