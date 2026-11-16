use super::from_tests::{adopt_now, moved};
use super::tests::world;
use super::*;
use crate::agents::adopt_fixture as ax;
use crate::agents::cascade_fixture as cx;
use crate::agents::fixture::World;
use crate::agents::gens;
use crate::agents::life_fixture as lf;
use crate::caller::Caller;

/// Messages with delivery id `exit-<id>-1` in mailbox `key`'s file (no cursor moved).
fn notices_in(w: &World, key: &str, id: &str) -> usize {
    let want = gens::delivery_id(id, 1);
    let text = std::fs::read_to_string(w.state().join("mail").join(format!("{key}.jsonl")))
        .unwrap_or_default();
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|m| m["id"] == want.as_str())
        .count()
}
/// B's generation-1 exit notice, posted as a reap posts it: under B's member and notice locks (AD-R7).
fn notice_b(w: &World, caller: &Caller) -> Res<bool> {
    use crate::agents::registry;
    let r = ax::record(w, cx::B);
    registry::with_member_notice(&w.ctx, caller, cx::B, || {
        registry::add_notice_gen(
            &w.ctx,
            caller,
            cx::B,
            &r,
            1,
            registry::Notice::Exited(json!(0)),
        )
    })
}
fn pane_box() -> String {
    crate::agents::mail::mailbox_key(cx::PANE, "").unwrap()
}
fn route_file(w: &World) -> PathBuf {
    gens::path(&w.folder(cx::B), 1, "route")
}
/// L -> A -> E -> B, E Ended and adopted from (B under A), B's generation 1 Ended.
fn adopted(w: &World) {
    ax::team(w);
    assert_eq!(adopt_now(w, ax::E), moved(cx::A));
    ax::ended(w, cx::B);
}

// A7: the adopted child's route and its exit notice reach A's member mailbox, never E's or the pane's.
#[test]
fn an_adopted_childs_route_and_notice_reach_the_adopter() {
    let (w, s) = world();
    ax::team(&w);
    let before = route(&cx::tx(&w, &s), cx::B).unwrap();
    assert!(
        matches!(before, Route::Member(ref id, _) if id == ax::E),
        "before: E, Ended and viewless"
    );
    assert_eq!(adopt_now(&w, ax::E), moved(cx::A));
    let after = route(&cx::tx(&w, &s), cx::B).unwrap();
    assert!(
        matches!(after, Route::Member(ref id, _) if id == cx::A),
        "after: A"
    );
    ax::ended(&w, cx::B);
    assert!(notice_b(&w, &w.caller()).unwrap());
    assert_eq!(notices_in(&w, cx::A, cx::B), 1, "A's mailbox");
    let elsewhere = notices_in(&w, ax::E, cx::B) + notices_in(&w, &pane_box(), cx::B);
    assert_eq!(elsewhere, 0, "not E's, not the pane's");
    w.fakes.hc_done().unwrap();
}

// A7: an Ended, viewless root still receives its child's notice.
#[test]
fn an_ended_viewless_root_is_still_routed() {
    let (w, s) = world();
    cx::member(&w, cx::L, None, 0, json!({"upTeam": "t"}));
    cx::member(&w, cx::B, Some(cx::L), 1, json!({}));
    ax::ended(&w, cx::L);
    let root = route(&cx::tx(&w, &s), cx::L).unwrap();
    assert!(matches!(root, Route::Legacy), "a root has no parent");
    ax::ended(&w, cx::B);
    assert!(notice_b(&w, &w.caller()).unwrap());
    assert_eq!(notices_in(&w, cx::L, cx::B), 1);
    assert_eq!(notices_in(&w, &pane_box(), cx::B), 0);
    w.fakes.hc_done().unwrap();
}

// Call 12: the destination is recorded in `gens/1/route.json` before the first delivery attempt; a crash after the
// message and before the marker, then an adoption, then the retry: the retry delivers to the recorded mailbox (E's),
// so the notice is in exactly one mailbox.
#[test]
fn a_notice_is_delivered_once_across_an_adoption() {
    let (w, _) = world();
    ax::team(&w);
    ax::ended(&w, cx::B); // generation 1 Ended
    ax::set(&w, cx::B, "pending", lf::pending(2)); // B stays live (pending), so it moves
    cx::at("delivered");
    assert_eq!(notice_b(&w, &w.caller()).unwrap_err().check, "halted");
    crate::agents::release::halt_after(None);
    let recorded = crate::state::fs::read_json(&route_file(&w)).unwrap();
    assert_eq!(recorded.unwrap()["mailbox"], ax::E, "recorded first");
    assert_eq!(adopt_now(&w, ax::E), moved(cx::A));
    assert!(notice_b(&w, &w.caller()).unwrap(), "the retry posts");
    let got = (notices_in(&w, ax::E, cx::B), notices_in(&w, cx::A, cx::B));
    assert_eq!(got, (1, 0), "once, where recorded");
    w.fakes.hc_done().unwrap();
}

// Call 12: the route is computed in the subject's scope: a caller of another session posts B's notice; it reaches
// B's logical parent's member mailbox, not the pane's.
#[test]
fn a_notice_routes_in_its_members_scope() {
    let (w, _) = world();
    adopted(&w);
    let mut other = w.caller();
    other.session = "$9".into();
    other.pane = "%77".into();
    assert!(notice_b(&w, &other).unwrap());
    let got = (
        notices_in(&w, cx::A, cx::B),
        notices_in(&w, &pane_box(), cx::B),
    );
    assert_eq!(got, (1, 0));
    w.fakes.hc_done().unwrap();
}

// Ambiguity 13: a moving tree refuses the post before anything is recorded; once replayed, the next post routes.
#[test]
fn a_moving_tree_records_no_route() {
    let (w, s) = world();
    ax::team(&w);
    cx::at("adopt-journal");
    assert_eq!(
        adopt_from(&cx::tx(&w, &s), ax::E).unwrap_err().check,
        "halted"
    );
    crate::agents::release::halt_after(None);
    ax::ended(&w, cx::B);
    assert_eq!(notice_b(&w, &w.caller()).unwrap_err().message, MOVING);
    assert!(!route_file(&w).exists(), "nothing recorded");
    crate::agents::team_state::open(&w.ctx, &s, |_| Ok(())).unwrap();
    assert!(notice_b(&w, &w.caller()).unwrap());
    assert_eq!(notices_in(&w, cx::A, cx::B), 1);
    w.fakes.hc_done().unwrap();
}

// Checklist 12 (call 19): a recorded route that is torn, of the wrong shape, names no mailbox, or is a folder or a
// link refuses `evidence`: nothing is delivered and no marker is published.
#[test]
fn a_route_that_is_not_a_route_refuses() {
    type Plant = fn(&World, &Path);
    let rows: [(&str, Plant); 7] = [
        ("torn", |_, at| {
            std::fs::write(at, b"{\"mailbox\": \"aaa").unwrap()
        }),
        ("not an object", |_, at| std::fs::write(at, b"[]").unwrap()),
        ("a number", |_, at| {
            std::fs::write(at, b"{\"mailbox\": 5}").unwrap()
        }),
        ("another key", |_, at| {
            std::fs::write(at, b"{\"mailbox\": null, \"more\": 1}").unwrap()
        }),
        ("not a mailbox", |_, at| {
            std::fs::write(at, b"{\"mailbox\": \"../x\"}").unwrap()
        }),
        ("a folder", |_, at| std::fs::create_dir_all(at).unwrap()),
        ("a link", |w, at| {
            let t = w._dir.path().join("elsewhere");
            std::fs::create_dir_all(&t).unwrap();
            crate::testing::link_dir(&t, at);
        }),
    ];
    for (case, plant) in rows {
        let (w, _) = world();
        adopted(&w);
        let at = route_file(&w);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        plant(&w, &at);
        let e = notice_b(&w, &w.caller()).unwrap_err();
        assert_eq!(e.check, "evidence", "{case}: {e:?}");
        assert_eq!(notices_in(&w, cx::A, cx::B), 0, "{case}: nothing delivered");
        let marker = gens::read(&w.folder(cx::B), 1, "notice").known();
        assert!(marker.unwrap().is_none(), "{case}: no marker");
        w.fakes.hc_done().unwrap();
    }
}

// Checklist 12 (call 19, refinding R1): each flush of the route's publication fails; with the fault armed the post
// refuses twice, a post delivers nothing (a route left visible but unflushed is not used); cleared, one delivery.
#[test]
fn every_route_flush_is_repeated_until_it_lands() {
    use crate::state::fs::{fail_sync_dir, fail_sync_file};
    for case in ["content", "gens/1", "gens", "member", "agents"] {
        let (w, _) = world();
        adopted(&w);
        let m = w.folder(cx::B);
        let arm = |on: bool| match case {
            "content" => fail_sync_file(on.then_some(route_file(&w)).as_deref()),
            "gens/1" => fail_sync_dir(on.then_some(gens::dir(&m, 1)).as_deref()),
            "gens" => fail_sync_dir(on.then_some(m.join("gens")).as_deref()),
            "member" => fail_sync_dir(on.then_some(m.clone()).as_deref()),
            _ => fail_sync_dir(on.then_some(m.parent().unwrap().to_path_buf()).as_deref()),
        };
        // `post` reads `ended.json` through the same folders first, so the route's publication is driven directly.
        let decide = || notice_route(&w.ctx, &w.caller(), &m, cx::B, &ax::record(&w, cx::B), 1);
        arm(true);
        let first = decide();
        let again = decide();
        let post = notice_b(&w, &w.caller());
        arm(false);
        assert_eq!(first.map_err(|e| e.check), Err("state"), "{case}: first");
        assert_eq!(
            again.map_err(|e| e.check),
            Err("state"),
            "{case}: armed retry"
        );
        assert!(!matches!(post, Ok(true)), "{case}: armed post: {post:?}");
        assert_eq!(
            notices_in(&w, cx::A, cx::B),
            0,
            "{case}: nothing delivered while armed"
        );
        assert!(notice_b(&w, &w.caller()).unwrap(), "{case}: cleared");
        assert_eq!(notices_in(&w, cx::A, cx::B), 1, "{case}: one delivery");
        w.fakes.hc_done().unwrap();
    }
}

// AD-R7 (refinding R18): a post under the member and notice locks routes without the team lock.
#[test]
fn a_notice_routes_without_the_team_lock() {
    let (w, s) = world();
    adopted(&w);
    let before = w.fakes.locks().len();
    assert!(notice_b(&w, &w.caller()).unwrap());
    let taken: Vec<String> = w.fakes.locks()[before..].to_vec();
    assert!(
        taken
            .iter()
            .any(|l| l == &format!("+{}", s.member_lock(cx::B))),
        "the member lock: {taken:?}"
    );
    let team = s.team_lock();
    assert!(
        !taken.iter().any(|l| l.ends_with(&team)),
        "no team lock: {taken:?}"
    );
    assert_eq!(notices_in(&w, cx::A, cx::B), 1);
    w.fakes.hc_done().unwrap();
}
