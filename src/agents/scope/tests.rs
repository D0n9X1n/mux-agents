//! reentry-6a E0 (Task 2): state at the anchor, the multiplexer the caller's own.
use super::super::fixture::World;
use super::super::team_state::Scope;
use crate::agents::scope;
use crate::agents::up::index::{self, Index, Inputs, Pending, Server, View};
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
/// Integration CI: `key`'s state folder and its agents and mail folders made private, as the product makes
/// them, the state root first; a folder left at the umask (755) or with an inherited shared ACL is refused `state`
/// before anything under test runs.
fn private_key(w: &World, key: &str) {
    w.state();
    let k = w.ctx.state_root.join(key);
    for f in [k.clone(), k.join("agents"), k.join("mail")] {
        crate::agents::spawn::private_folder(&w.ctx, &f).unwrap();
    }
}
/// A committed lifecycle member `id` at K1 in `session`: a valid hc identity and generation 1 launched, claimed
/// `run` and committed.
fn record_at(w: &World, id: &str, session: &str) {
    use crate::agents::life_fixture as lf;
    let f = w.ctx.state_root.join(K1).join("agents").join(id);
    private_key(w, K1);
    crate::agents::spawn::private_folder(&w.ctx, &f).unwrap();
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
    private_key(w, K1);
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
    private_key(&w, K1);
    crate::agents::spawn::private_folder(&w.ctx, &pre).unwrap();
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

// ---- reentry-6a E2 (Task 3): person admission ----

/// An index of team `t` anchored at K1/$A with the given view, pending and aliases.
fn ix(t: &str, view: Option<View>, pending: Option<Pending>, aliases: Vec<View>) -> Index {
    Index {
        team: t.into(),
        inputs: Inputs {
            client: "claude".into(),
            options: vec![],
            prompt: vec![],
            name: String::new(),
            folder: "/p".into(),
        },
        anchor: Some(Anchor {
            key: K1.into(),
            session: "$A".into(),
        }),
        view,
        pending,
        aliases,
        releases: vec![],
    }
}
/// The World's own server tuple, taken before any index exists (once one claims the session, resolving admits).
fn here(w: &World) -> Server {
    index::server_of(&w.caller())
}
fn on(s: Server, session: &str) -> View {
    View {
        server: s,
        session: session.into(),
        name: "p-01234567-v".into(),
    }
}
fn pending_on(w: &World, s: Server, name: &str) -> Pending {
    Pending {
        server: s,
        name: name.into(),
        release: w
            .ctx
            .state_root
            .join("up")
            .join("r.release")
            .to_str()
            .unwrap()
            .into(),
    }
}
const T1: &str = "1111111111111111";
const T2: &str = "2222222222222222";

/// `mux-agent <args>` through the real dispatcher: (exit code, reply).
fn run(w: &World, args: &[&str]) -> (u8, serde_json::Value) {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let (code, out) = crate::cli::run(&args, || Ok(&w.ctx));
    (code, serde_json::from_str(&out).unwrap())
}

// E2: a person in the proven view resolves to the anchor; its multiplexer stays its own.
#[test]
fn a_person_in_the_proven_view_reaches_the_anchor() {
    let w = World::human("");
    let me = here(&w);
    index::write(&w.ctx, &ix(T1, Some(on(me, "$3")), None, vec![])).unwrap();
    let c = crate::caller::resolve(&w.ctx).unwrap();
    assert_eq!(
        c.anchor.clone(),
        Some(Anchor {
            key: K1.into(),
            session: "$A".into()
        })
    );
    assert_eq!(
        (scope::state(&c), c.session.as_str()),
        (at(), "$3"),
        "state at the anchor, multiplexer its own"
    );
    assert!(
        !w.fakes
            .mux_calls()
            .iter()
            .any(|a| a.join(" ").contains("#{session_name}")),
        "no pending: no name read"
    );
}

// Row 3.1: the caller proof sees the server; the index proof, one open later, sees it restarted: the view is not
// proven, so the session is unclaimed, never anchored on a dead server.
#[test]
fn a_view_on_a_server_restarted_since_the_caller_proof_claims_nothing() {
    let w = World::human("");
    let me = here(&w);
    index::write(&w.ctx, &ix(T1, Some(on(me.clone(), "$3")), None, vec![])).unwrap();
    w.fakes.opens(me.pid, &[Some(me.start), Some(me.start + 1)]);
    assert_eq!(crate::caller::resolve(&w.ctx).unwrap().anchor, None);
}

// E2: in a session named `pending.name` on its proven server, or at an alias: 1 `moved`, through the dispatcher.
#[test]
fn a_pending_session_or_an_alias_is_moved() {
    let w = World::human("");
    let me = here(&w);
    w.fakes.mux_rule("#{session_name}", 0, "p-01234567-y\n", "");
    let p = pending_on(&w, me.clone(), "p-01234567-y");
    index::write(&w.ctx, &ix(T1, None, Some(p), vec![])).unwrap();
    let e = crate::caller::resolve(&w.ctx).unwrap_err();
    assert_eq!(
        (e.code, e.check, e.message.as_str()),
        (1, "moved", scope::MOVED)
    );
    let (code, reply) = run(&w, &["list"]);
    assert_eq!(
        (code, reply["check"].as_str()),
        (1, Some("moved")),
        "{reply}"
    );
    std::fs::remove_file(index::path(&w.ctx, T1)).unwrap();
    index::write(
        &w.ctx,
        &ix(T2, Some(on(me.clone(), "$9")), None, vec![on(me, "$3")]),
    )
    .unwrap();
    assert_eq!(
        crate::caller::resolve(&w.ctx).unwrap_err().check,
        "moved",
        "an alias"
    );
}

// E2: two claims, or an unreadable index: 1 `index`; a torn index of another team names its full path (call 2).
#[test]
fn conflicting_or_unreadable_indexes_refuse_index() {
    let w = World::human("");
    let me = here(&w);
    index::write(&w.ctx, &ix(T1, Some(on(me.clone(), "$3")), None, vec![])).unwrap();
    index::write(&w.ctx, &ix(T2, Some(on(me.clone(), "$3")), None, vec![])).unwrap();
    assert_eq!(
        crate::caller::resolve(&w.ctx).unwrap_err().check,
        "index",
        "two anchors"
    );
    let moved = ix(
        T2,
        Some(on(me.clone(), "$9")),
        None,
        vec![on(me.clone(), "$3")],
    );
    index::write(&w.ctx, &moved).unwrap();
    assert_eq!(
        crate::caller::resolve(&w.ctx).unwrap_err().check,
        "index",
        "an anchor and a moved claim"
    );
    index::write(&w.ctx, &ix(T2, Some(on(me, "$9")), None, vec![])).unwrap();
    assert_eq!(
        crate::caller::resolve(&w.ctx)
            .unwrap()
            .anchor
            .map(|a| a.key),
        Some(K1.into()),
        "one claim"
    );
    let torn = index::path(&w.ctx, T2);
    std::fs::write(&torn, b"{").unwrap();
    let e = crate::caller::resolve(&w.ctx).unwrap_err();
    assert_eq!(
        (e.check, e.message),
        (
            "index",
            format!(
                "the team index cannot be read; repair or remove {}",
                torn.display()
            )
        ),
        "a torn index of another team names its path"
    );
}

// E2, call 6 (row 3.3): a same-server pending of another name, and a view of another session, claim nothing.
#[test]
fn an_unclaimed_person_keeps_todays_scope() {
    let w = World::human("");
    let me = here(&w);
    w.fakes.mux_first("#{session_name}", 0, "mine\n", "");
    let p = pending_on(&w, me.clone(), "p-01234567-other");
    index::write(&w.ctx, &ix(T1, Some(on(me, "$9")), Some(p), vec![])).unwrap();
    let c = crate::caller::resolve(&w.ctx).unwrap();
    assert_eq!(
        (c.anchor.clone(), scope::state(&c).key),
        (None, c.server_key.clone())
    );
}

// E2: `up` needs only person proof, never the index; agents and hc members fail it.
#[test]
fn up_needs_only_person_proof() {
    let w = World::human("");
    let me = here(&w);
    w.fakes.mux_rule("#{session_name}", 0, "p-01234567-y\n", "");
    let p = pending_on(&w, me, "p-01234567-y");
    index::write(&w.ctx, &ix(T1, None, Some(p), vec![])).unwrap();
    let up = crate::agents::up::person;
    assert_eq!(up(&w.ctx).unwrap().anchor, None, "up: person proof only");
    std::fs::write(index::path(&w.ctx, T1), b"{").unwrap();
    assert!(up(&w.ctx).is_ok(), "a torn index never blocks up");
    let a = World::agent("");
    assert_eq!(up(&a.ctx).unwrap_err().check, "tagged", "an agent pane");
    let mut m = World::human("");
    m.ctx.env = crate::ctx::Env::from_pairs([("HC_TERMINAL_ID", "t1".to_string())]);
    assert_eq!(up(&m.ctx).unwrap_err().check, "tagged", "an hc member");
}

// Call 10 through the dispatcher: a person in the view lists the K1/$A team and takes its team lock; the real
// `route_team` keeps its send journal at K1 under `$A` (call 32).
#[test]
fn a_person_in_the_view_reaches_the_teams_records_and_lock_through_the_dispatcher() {
    let w = World::human("");
    let me = here(&w);
    team_record(&w, "$A");
    record_at(&w, "aaaaaaaaaaa1", "$A");
    index::write(&w.ctx, &ix(T1, Some(on(me, "$3")), None, vec![])).unwrap();
    let (code, listed) = run(&w, &["list", "-All"]);
    assert_eq!(code, 0, "{listed}");
    assert!(
        listed.to_string().contains("\"m1\""),
        "the K1/$A member is listed: {listed}"
    );
    // Checklist R6: hc runs `m1` (generation 1, live), so the send is delivered through the real `route_team`.
    use crate::agents::{core_fixture as cf, life_fixture as lf};
    cf::online(&w);
    cf::listing(&w, &format!("{K1}:$A"), &[]);
    let wire = json!({"address": "t1", "name": lf::hc_name(1), "state": "live", "reason": null,
        "argv": lf::ARGV, "exit_code": null, "labels": {"mux.managed": "1", "mux.team": format!("{K1}:$A"),
        "mux.id": "aaaaaaaaaaa1", "mux.gen": "1", "mux.name": "m1"}});
    cf::get(&w, "t1", wire);
    let (sent_code, sent) = run(&w, &["send", "m1", "-Body", "x"]);
    assert_eq!(sent_code, 0, "the send is delivered: {sent}");
    w.fakes.hc_done().unwrap();
    assert!(
        w.fakes
            .locks()
            .iter()
            .any(|k| k.contains(&format!("spawn|{K1}|$A"))),
        "the anchor's team lock: {:?}",
        w.fakes.locks()
    );
    let k1 = w.ctx.state_root.join(K1);
    assert!(
        crate::agents::team_mail::sends_root(&k1, "$A").exists(),
        "team_mail.rs:485: the journal's session is $A ({sent_code}: {sent})"
    );
    assert!(
        !crate::agents::team_mail::sends_root(&k1, "$3").exists(),
        "none under the shown session"
    );
}

/// The World caller re-pointed at pane `pane` of its own server (Kw).
fn from_pane(w: &mut World, pane: &str) {
    let tmux = w.ctx.env.get("TMUX").unwrap().to_string();
    w.ctx.env = crate::ctx::Env::from_pairs([("TMUX", tmux), ("TMUX_PANE", pane.to_string())]);
}

// Call 15: K1's `%1` and Kw's `%1` have two mailboxes; `send %1` from Kw's `%2` reaches Kw's, and `read` by Kw's
// `%1` reads only its own, both under the anchor's mail folder.
#[test]
fn the_same_pane_on_two_servers_has_two_mailboxes_through_the_dispatcher() {
    let mut w = World::human("");
    let me = here(&w);
    w.panes(&["%1|@1|100||", "%2|@1|101||"]);
    // `send %1` proves the recipient a live pane of this session (finding F10).
    w.fakes
        .mux_rule("#{pane_id}|#{session_id}", 0, "%1|$3\n", "");
    team_record(&w, "$A");
    index::write(&w.ctx, &ix(T1, Some(on(me.clone(), "$3")), None, vec![])).unwrap();
    let mail = w.ctx.state_root.join(K1).join("mail");
    crate::agents::mail::add_mail(
        &w.ctx,
        &anchored(&w),
        "pane-1",
        "worker",
        "%9",
        "for K1 pane 1",
        None,
    )
    .unwrap();
    from_pane(&mut w, "%2");
    let (code, r) = run(&w, &["send", "%1", "-Body", "for Kw pane 1"]);
    assert_eq!(code, 0, "send: {r}");
    assert!(
        mail.join(format!("pane-{}-1.jsonl", me.key)).is_file(),
        "the producer routes off the anchor"
    );
    from_pane(&mut w, "%1");
    let (code, got) = run(&w, &["read"]);
    assert_eq!(code, 0, "{got}");
    let text = got.to_string();
    assert!(
        text.contains("for Kw") && !text.contains("for K1"),
        "the reader reads its own server's box: {text}"
    );
}

// The notice producers: a member spawned by a person off the anchor records `parentServerKey`; its notice parent
// box, and adopt-3e's `notice_route` fallback, name that server; on the anchor no key is recorded.
#[test]
fn a_childs_notice_reaches_its_off_anchor_parent_box() {
    let w = World::human("");
    let me = here(&w);
    let rec = json!({"parentPane": "%1", "parentServerKey": me.key});
    assert_eq!(
        crate::agents::mail::parent_box(&json!({"parentPane": "%1"})).unwrap(),
        "pane-1",
        "on the anchor"
    );
    assert_eq!(
        crate::agents::mail::parent_box(&rec).unwrap(),
        format!("pane-{}-1", me.key)
    );
    // Row 4.6, R11: adopt-3e's no-parent fallback (`notice_route`'s and the pre-lifecycle registry path's).
    assert_eq!(
        crate::agents::adopt::pane_key(&rec).unwrap(),
        Some(format!("pane-{}-1", me.key)),
        "adopt-3e fallback"
    );
    // R11: a recorded qualified route reads back on retry (never refused as unreadable evidence).
    let folder = w
        .ctx
        .state_root
        .join(K1)
        .join("agents")
        .join("aaaaaaaaaaa1");
    std::fs::create_dir_all(&folder).unwrap();
    let qualified = format!("pane-{}-1", me.key);
    crate::agents::gens::publish_once(&folder, 1, "route", &json!({ "mailbox": qualified }))
        .unwrap();
    let routed =
        crate::agents::adopt::notice_route(&w.ctx, &anchored(&w), &folder, "aaaaaaaaaaa1", &rec, 1);
    assert_eq!(
        routed.map_err(|e| e.check),
        Ok(Some(qualified)),
        "a recorded qualified route"
    );
    // Call 32 (finding F11): the board's parent mailbox, its `parent_key` fallback and its notice box, honour it.
    assert_eq!(
        crate::agents::board::parent_mailbox(&rec),
        Some(format!("pane-{}-1", me.key)),
        "board parent box"
    );
}

// Finding F13: `elsewhere` is None without an anchor and on the anchor's own server, so a person there keeps `pane-<n>`.
#[test]
fn elsewhere_is_none_without_an_anchor_or_on_it() {
    let w = World::human("");
    let mut c = w.caller();
    assert_eq!(crate::agents::mail::elsewhere(&c), None, "no anchor");
    c.anchor = Some(Anchor {
        key: c.server_key.clone(),
        session: "$A".into(),
    });
    assert_eq!(
        crate::agents::mail::elsewhere(&c),
        None,
        "anchored on its own server"
    );
    assert_eq!(
        crate::agents::mail::person_box(&c).unwrap(),
        format!("pane-{}", &c.pane[1..]),
        "the anchor's own box"
    );
}

// Checklist R9: a lifecycle member at K1 forces `route_team`; a person on Kw's `%2` sends to Kw's `%1`: the intent
// names the qualified recipient box (and the sender's own qualified box), and delivery lands there.
#[test]
fn a_team_send_to_a_pane_off_the_anchor_names_its_qualified_box() {
    use crate::agents::{core_fixture as cf, life_fixture as lf, team_mail};
    let mut w = World::human("");
    let me = here(&w);
    w.panes(&["%1|@1|100||", "%2|@1|101||"]);
    w.fakes
        .mux_rule("#{pane_id}|#{session_id}", 0, "%1|$3\n", "");
    team_record(&w, "$A");
    record_at(&w, "aaaaaaaaaaa1", "$A");
    index::write(&w.ctx, &ix(T1, Some(on(me.clone(), "$3")), None, vec![])).unwrap();
    from_pane(&mut w, "%2");
    let want = format!("pane-{}-1", me.key);
    let sender = format!("pane-{}-2", me.key);
    let sends = team_mail::sends_root(&w.ctx.state_root.join(K1), "$A").join(&sender);
    for crash in [true, false] {
        cf::online(&w);
        cf::listing(&w, &format!("{K1}:$A"), &[]);
        let wire = json!({"address": "t1", "name": lf::hc_name(1), "state": "live", "reason": null,
            "argv": lf::ARGV, "exit_code": null, "labels": {"mux.managed": "1", "mux.team": format!("{K1}:$A"),
            "mux.id": "aaaaaaaaaaa1", "mux.gen": "1", "mux.name": "m1"}});
        cf::get(&w, "t1", wire);
        team_mail::crash_after_intent(crash);
        let (code, r) = run(&w, &["send", "%1", "-Body", "for Kw pane 1"]);
        team_mail::crash_after_intent(false);
        if crash {
            assert_ne!(code, 0, "the send stops after its intent: {r}");
            let intent = std::fs::read_dir(&sends)
                .unwrap_or_else(|e| {
                    panic!("the sender's own qualified intent folder {sends:?}: {e}")
                })
                .map(|e| e.unwrap().path())
                .find(|p| p.extension().is_some_and(|x| x == "json"))
                .expect("an intent");
            let intent: serde_json::Value =
                serde_json::from_slice(&std::fs::read(intent).unwrap()).unwrap();
            assert_eq!(
                intent["recipients"][0]["box"],
                json!(want),
                "the intent's mailbox field"
            );
        } else {
            assert_eq!(code, 0, "send: {r}");
            let file = w
                .ctx
                .state_root
                .join(K1)
                .join("mail")
                .join(format!("{want}.jsonl"));
            assert!(file.is_file(), "delivered to the qualified box: {r}");
        }
    }
    w.fakes.hc_done().unwrap();
}

/// The team record `up` writes at `key`/`session`, so a person there has standing (tree::with_standing).
fn team_record_at(w: &World, key: &str, session: &str) {
    use crate::agents::team;
    let t = team::Team {
        team: T1.into(),
        session: session.into(),
        name: "p-01234567-v".into(),
        directory: "/p".into(),
        client: crate::parse::Client::Claude,
        flags: vec![],
        policy: team::Policy::Default,
        leader: "cccccccccccc".into(),
        created_at: "2026-11-16T00:00:00.0000000Z".into(),
    };
    private_key(w, key);
    assert!(team::create(&team::path(&w.ctx.state_root.join(key), session), &t).unwrap());
}

// n7a call 1: a note written in the view on Kw; the server restarts (a persistent new start, so a new key K2) and
// the index view moves to K2 (anchor still Kw/$3); from K2 `note` and `notes` reach the same notebook, newest
// first, at the anchor; nothing is written under K2. A second team T2 whose view is on another live server with the
// same session id `$3` claims nothing from K2 (row 6.3).
#[test]
fn note_and_notes_reach_the_anchor_notebook_after_a_server_restart() {
    use crate::agents::notebook;
    let w = World::human("");
    let first = here(&w);
    let k1 = first.key.clone();
    team_record_at(&w, &k1, "$3");
    let mut index = ix(T1, Some(on(first.clone(), "$3")), None, vec![]);
    index.anchor = Some(Anchor {
        key: k1.clone(),
        session: "$3".into(),
    });
    index::write(&w.ctx, &index).unwrap();
    let (code, r) = run(&w, &["note", "-Title", "before", "-Body", "b1"]);
    assert_eq!(code, 0, "{r}");
    w.fakes.process(crate::testing::row(
        crate::agents::fixture::SERVER,
        1,
        first.start + 1,
    ));
    w.fakes.prefer_last(); // the restarted server: same pid and socket, a new start, persistently
    let k2 = Server {
        start: first.start + 1,
        key: index::key_of(&first.socket, first.pid, first.start + 1),
        ..first.clone()
    };
    index.view = Some(on(k2.clone(), "$3"));
    index.aliases = vec![on(first, "$3")];
    index::write(&w.ctx, &index).unwrap();
    w.fakes.process(crate::testing::row(4244, 1, 3));
    let other = Server {
        socket: "/tmp/other".into(),
        pid: 4244,
        start: 3,
        key: index::key_of("/tmp/other", 4244, 3),
    };
    let mut t2 = ix(T2, Some(on(other, "$3")), None, vec![]);
    t2.anchor = Some(Anchor {
        key: "b2b2b2b2b2b2b2b2".into(),
        session: "$3".into(),
    });
    index::write(&w.ctx, &t2).unwrap();
    assert_eq!(
        w.caller().server_key,
        k2.key,
        "a restarted server has a new key"
    );
    let (code, r) = run(&w, &["note", "-Title", "after", "-Body", "b2"]);
    assert_eq!(code, 0, "{r}");
    let (code, listed) = run(&w, &["notes"]);
    assert_eq!(code, 0, "{listed}");
    let titles: Vec<&str> = listed["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["after", "before"], "one notebook, newest first");
    let at = Scope {
        key: k1.clone(),
        session: "$3".into(),
    };
    assert!(
        notebook::dir(&w.ctx.state_root, &at)
            .join(notebook::LOG)
            .is_file(),
        "at the anchor"
    );
    assert!(
        !w.ctx.state_root.join(&k2.key).join("teams").exists(),
        "nothing under the restarted server's key"
    );
}
