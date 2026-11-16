//! reentry-6a E0 (Task 2): state at the anchor, the multiplexer the caller's own.
use super::super::fixture::World;
use super::super::team_state::Scope;
use crate::agents::scope;
use crate::caller::Anchor;
use serde_json::json;

const K1: &str = "a1a1a1a1a1a1a1a1";

/// The World's caller (presented on its own key Kw, session `$3`), anchored at K1/`$A`.
fn anchored(w: &World) -> crate::caller::Caller {
    let mut c = w.caller();
    c.anchor = Some(Anchor {
        key: K1.into(),
        session: "$A".into(),
    });
    c
}
fn at() -> Scope {
    Scope {
        key: K1.into(),
        session: "$A".into(),
    }
}
/// A committed lifecycle member `id` at K1 in `session`: a valid hc identity and generation 1 launched, claimed
/// `run` and committed.
fn record_at(w: &World, id: &str, session: &str) {
    use crate::agents::life_fixture as lf;
    let f = w.ctx.state_root.join(K1).join("agents").join(id);
    std::fs::create_dir_all(&f).unwrap();
    crate::state::fs::write_json(
        &f.join("record.json"),
        &json!({"lifecycle": 1, "id": id, "name": format!("m{}", &id[11..]),
        "client": "claude", "session": session, "sessionId": lf::SID, "team": format!("{K1}:{session}"),
        "directory": w.ctx.cwd, "context": "brief", "reason": "human", "options": [], "prompt": ["fix"], "depth": 1,
        "generation": "1", "hcAddress": format!("t{}", &id[11..]), "hcName": lf::hc_name(1), "launcherArgv": lf::ARGV}),
    )
    .unwrap();
    lf::seed_gen(&f, 1, Some("run"), lf::committed());
}
/// The team record `up` writes at K1/`session`.
fn team_record(w: &World, session: &str) {
    use crate::agents::team;
    let t = team::Team {
        team: "1111111111111111".into(),
        session: session.into(),
        name: "p-11111111-v".into(),
        directory: "/p".into(),
        client: crate::parse::Client::Claude,
        flags: vec![],
        policy: team::Policy::Default,
        leader: "cccccccccccc".into(),
        created_at: "2026-11-16T00:00:00.0000000Z".into(),
    };
    assert!(team::create(&team::path(&w.ctx.state_root.join(K1), session), &t).unwrap());
}

// Call 10: every state consumer reads the anchor pair through production code; the multiplexer is the caller's.
// Re6a call 43: `lock_key` keeps its signature and locks at the anchor; the bar lock is the shown pair's.
#[test]
fn state_consumers_read_the_anchor_pair() {
    use crate::agents as a;
    let w = World::human("");
    let own = w.caller();
    let c = anchored(&w);
    let shown = Scope {
        key: own.server_key.clone(),
        session: "$3".into(),
    };
    assert_eq!(
        scope::state(&own),
        shown,
        "no anchor: the caller's own pair"
    );
    assert_eq!(
        (scope::state(&c), scope::shown(&c)),
        (at(), shown),
        "state at K1/$A, shown on Kw/$3"
    );
    assert_eq!(
        Scope::of(&c).team_lock(),
        format!("spawn|{K1}|$A"),
        "the team lock"
    );
    assert_eq!(
        a::lock_key("member", &c, "x"),
        format!("member|{K1}|x"),
        "call 43: a state lock lands on the anchor"
    );
    assert_eq!(
        scope::shown_lock("sidebar", &c),
        format!("sidebar|{}|$3", own.server_key),
        "call 43: the bar lock is the shown session's"
    );
    assert_eq!(
        a::server_dir(&w.ctx, &c),
        w.ctx.state_root.join(K1),
        "mod.rs: server_dir"
    );
    assert_eq!(
        a::membership::team(&c),
        format!("{K1}:$A"),
        "membership::team"
    );
    assert_eq!(a::notebook::anchor(&c), at(), "n7a call 1");
    team_record(&w, "$A");
    assert!(
        matches!(a::tree::scope(&w.ctx, &c).unwrap(), a::tree::Scope::Team(_)),
        "tree.rs:30 discovery"
    );
    record_at(&w, "aaaaaaaaaaa1", "$A");
    record_at(&w, "aaaaaaaaaaa2", "$3");
    let snap = a::membership::Snapshot::take(&w.ctx, &c).unwrap();
    let ids: Vec<&String> = snap.records.keys().collect();
    assert!(
        ids.contains(&&"aaaaaaaaaaa1".to_string()) && !ids.contains(&&"aaaaaaaaaaa2".to_string()),
        "membership.rs:90: {ids:?}"
    );
}

// Call 10: registry ownership and the board's team record read the anchor pair. The board marks a team
// (`"team": true`) only when it found the record.
#[test]
fn registry_and_board_read_the_anchor_pair() {
    use crate::agents as a;
    let w = World::human("");
    let c = anchored(&w);
    let pre = w
        .ctx
        .state_root
        .join(K1)
        .join("agents")
        .join("999999999999");
    std::fs::create_dir_all(&pre).unwrap();
    crate::state::fs::write_json(
        &pre.join("record.json"),
        &json!({"id": "999999999999", "name": "old", "session": "$A"}),
    )
    .unwrap();
    let snap = a::registry::Snapshot::take(&w.ctx, &c).unwrap();
    assert!(
        snap.agents.iter().any(|x| x.id == "999999999999"),
        "registry.rs:387: owned by the anchor session"
    );
    team_record(&w, "$A");
    let reply = a::board::board(&w.ctx, &c, true, true).unwrap();
    assert_eq!(
        reply.get("team"),
        Some(&json!(true)),
        "board.rs:446: the K1/$A team record"
    );
}

// Call 13: `anchor_caller` sets only `anchor`; the person's socket, session, pane, window and key are kept.
#[test]
fn anchor_caller_sets_only_the_anchor() {
    let w = World::human("");
    let p = w.caller();
    let a = crate::agents::reconcile::anchor_caller(&p, &at());
    assert_eq!(
        a.anchor,
        Some(Anchor {
            key: K1.into(),
            session: "$A".into()
        })
    );
    let kept = |c: &crate::caller::Caller| {
        (
            c.socket.clone(),
            c.session.clone(),
            c.pane.clone(),
            c.window.clone(),
            c.server_key.clone(),
        )
    };
    assert_eq!(kept(&a), kept(&p), "presentation untouched");
    assert_eq!(scope::state(&a), at(), "state at the anchor");
}
