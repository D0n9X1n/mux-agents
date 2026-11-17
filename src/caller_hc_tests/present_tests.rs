//! reentry-6a Task 5 (E2; calls 18, 46): an `up` team member is presented only by its team's proven index view; a
//! plain-spawned lifecycle member (no index anchored at its folder pair) keeps its own presentation.
use super::*;
use crate::agents::up::index::{self, Index, Inputs, Server, View, key_of};

/// The member's team index, anchored at its folder pair (key from the folder path, session `$3`), with `view`;
/// returns the folder key.
fn member_index(w: &World, folder: &std::path::Path, view: Option<View>) -> String {
    let k1 = folder
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let ix = Index {
        team: "1111111111111111".into(),
        view,
        inputs: Inputs {
            client: "claude".into(),
            options: vec![],
            prompt: vec![],
            name: String::new(),
            folder: "/p".into(),
        },
        anchor: Some(crate::caller::Anchor {
            key: k1.clone(),
            session: "$3".into(),
        }),
        pending: None,
        aliases: vec![],
        releases: vec![],
    };
    index::write(&w.ctx, &ix).unwrap();
    k1
}
/// A live server K2 (pid 4243, start 9) and its view, session `$5`.
fn k2_view(w: &World) -> (Server, View) {
    w.fakes.process(crate::testing::row(4243, 1, 9));
    let socket = "/tmp/rmux-501/new";
    let k2 = Server {
        socket: socket.into(),
        pid: 4243,
        start: 9,
        key: key_of(socket, 4243, 9),
    };
    let view = View {
        server: k2.clone(),
        session: "$5".into(),
        name: "p-11111111-v".into(),
    };
    (k2, view)
}
fn focus(w: &World) -> (u8, Value) {
    let (code, out) = crate::cli::run(&["focus".to_string(), "worker".to_string()], || Ok(&w.ctx));
    (code, serde_json::from_str(&out).unwrap_or(Value::Null))
}

// Call 46 (a): a plain-spawned lifecycle member (no index anchored at its folder pair) keeps its own presentation,
// so view verbs are not refused `views`.
#[test]
fn a_plain_spawned_member_keeps_its_own_presentation() {
    let (w, r, _) = lifecycle_worker(ok(), ok());
    proof(&w, &r);
    let c = caller::resolve(&w.ctx).unwrap();
    assert!(
        c.presentation.is_some(),
        "a plain-spawned member is presented"
    );
    assert!(c.anchor.is_none(), "no team: its own pair");
    proof(&w, &r);
    let (_, out) = focus(&w);
    assert_ne!(
        out["check"],
        json!("views"),
        "focus is not refused views: {out}"
    );
}

// Call 46 (b): a team member whose view is not proven live has no presentation (no server.json fallback), view
// verbs refuse `views`; an unreadable index presents nothing and the member still resolves at its folder key.
#[test]
fn a_team_member_without_a_proven_view_is_not_presented() {
    let (w, r, folder) = lifecycle_worker(ok(), ok());
    let (_, view) = k2_view(&w);
    let k1 = member_index(&w, &folder, Some(view));
    w.fakes.forget(4243);
    proof(&w, &r);
    let gone = caller::resolve(&w.ctx).unwrap();
    assert!(
        gone.presentation.is_none(),
        "an unproven view presents nothing, no server.json fallback"
    );
    proof(&w, &r);
    let (code, out) = focus(&w);
    assert_eq!((code, &out["check"]), (1, &json!("views")), "{out}");
    std::fs::write(index::path(&w.ctx, "1111111111111111"), b"{").unwrap();
    proof(&w, &r);
    let torn = caller::resolve(&w.ctx).unwrap();
    assert!(
        torn.presentation.is_none(),
        "an unreadable index presents nothing"
    );
    assert_eq!(
        crate::agents::scope::state(&torn).key,
        k1,
        "and the member still resolves (send, read need no view)"
    );
}

// Call 46 (c), E2, call 18: a team member whose team view moved to live K2 is presented on K2, not by its own
// server.json; its state stays at the folder key.
#[test]
fn a_team_member_is_presented_by_its_teams_proven_view() {
    let (w, r, folder) = lifecycle_worker(ok(), ok());
    let (k2, view) = k2_view(&w);
    let k1 = member_index(&w, &folder, Some(view));
    proof(&w, &r);
    let c = caller::resolve(&w.ctx).unwrap();
    assert_eq!(
        (c.server_key.as_str(), c.session.as_str(), c.socket.as_str()),
        (k2.key.as_str(), "$5", k2.socket.as_str()),
        "presented on the team view"
    );
    assert!(c.presentation.is_some(), "presented");
    assert_eq!(
        crate::agents::scope::state(&c).key,
        k1,
        "state at the folder key"
    );
    assert_eq!(
        *w.ctx.member_mux.borrow(),
        crate::mux::MemberMux::Live(
            crate::mux::Endpoint {
                socket: k2.socket.clone(),
                server_pid: 4243,
            },
            9
        ),
        "the multiplexer is the team view's server"
    );
}
