//! restore-6b Task 7: `up` reconciles an existing lifecycle team at its anchor (ambiguities 7, 8, 16; r6b call 20).
use super::core_fixture as cf;
use super::core_launch::forward_tests as ft;
use super::fixture::World;
use super::life_fixture as lf;
use super::reconcile_restore_tests::{L, released_by_hc};
use super::reconcile_run_tests::{bytes, ids};
use super::reconcile_tests::{X, found, member, member_at, ok, seed_index, wire_of};
use super::reconcile_verb_tests::person;
use serde_json::{Value, json};

/// `mux-agent up -Client claude` through the real dispatcher, from the person world of the `up` tests.
fn up(w: &World) -> (u8, Value) {
    let args: Vec<String> = ["up", "-Client", "claude"].map(String::from).to_vec();
    let (code, out) = crate::cli::run(&args, || Ok(&w.ctx));
    (code, serde_json::from_str(&out).unwrap())
}
/// The team T `up` derives for this folder, client and empty name (`up::identity::team`).
fn team_t(w: &World) -> String {
    let folder = super::up::identity::folder(&w.ctx).unwrap();
    super::up::identity::team(&w.ctx, &folder, crate::parse::Client::Claude, "").unwrap()
}
/// release-1d R1.3 relabel for a shell of team `t` (`lf::relabel` names the caller team).
fn relabel_for(w: &World, address: &str, t: &str) {
    let argv: Vec<String> = lf::relabel_argv(w, address)
        .into_iter()
        .map(|a| {
            if a.starts_with("mux.kept=") {
                format!("mux.kept={t}")
            } else {
                a
            }
        })
        .collect();
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    cf::answer(w, &argv, "{}");
}
/// The leader of team `t` under the caller key, generation 1 ended (it ran; it asks hc nothing).
fn leader_of(w: &World, t: &str) {
    let lead = member(w, L, json!({"depth": 0, "upTeam": t, "name": "leader"}));
    crate::state::evidence::publish(&super::gens::path(&lead, 1, "ended"), &json!({"at": "t"}))
        .unwrap();
}

/// reentry-6a Task 11 (finding F18): re-entry's own pass after reconcile: one `hc::Lazy`, then member `id`'s
/// generation `n` looked up; hc lists no terminal of it, so it is Unknown and gets no view.
fn reentry_unlisted(w: &super::fixture::World, id: &str, n: u64) {
    cf::online(w);
    let (by_id, by_gen) = (format!("mux.id={id}"), format!("mux.gen={n}"));
    cf::answer(
        w,
        &[
            "ls", "--all", "--label", &by_id, "--label", &by_gen, "--json",
        ],
        r#"{"terminals":[]}"#,
    );
}

/// Ambiguity 8: a second `up` of a lifecycle team reconciles it: `created: false`, the Lost ok member restored.
#[test]
fn up_on_an_existing_team_reconciles_instead_of_refusing() {
    let w = person();
    let t = team_t(&w);
    leader_of(&w, &t);
    seed_index(&w, &t, &w.caller().server_key, &w.caller().session); // reentry-6a Task 9: up made it
    let f = member(&w, X, json!({"parentAgent": {"id": L}}));
    ok(&f, 1);
    cf::online(&w); // the pass (hc::Lazy); the base answer serves the connect of up (r6b call 11)
    found(&w, X, 1, &wire_of(&w, X, 1, lf::LOST_ARGVS[0]));
    released_by_hc(&w, X, lf::LOST_ARGVS[0]);
    w.fakes.hc_answer(ft::address("t2"));
    reentry_unlisted(&w, X, 2);
    let (code, reply) = up(&w);
    assert_eq!(code, 0, "{reply}");
    assert_eq!(
        (&reply["created"], &reply["team"], &reply["reentered"]),
        (&json!(false), &json!(t), &json!(true))
    );
    assert_eq!(ids(reply.as_object().unwrap(), "restored"), [X]);
    assert_eq!(lf::count(&w, "run"), 1, "no second leader");
    w.fakes.hc_done().unwrap();
}

/// Ambiguities 7 and 16: after a multiplexer restart the caller has a new server key. `up` finds the team at its
/// anchor (K1, session `$4`: neither the caller key nor its session, checklist 23), reconciles there, writes nothing under the caller key, and leaves a pre-lifecycle
/// folder of the team byte-identical (p1c call 6, coordinator).
#[test]
fn up_from_another_server_reconciles_at_the_anchor() {
    const K1: &str = "a1a1a1a1a1a1a1a1";
    let w = person();
    let (t, t1) = (team_t(&w), format!("{K1}:$4"));
    assert_ne!(
        w.caller().session,
        "$4",
        "mutation 7.2 needs another session"
    );
    let at = |id: &str, more: Value| {
        let mut fields = json!({"session": "$4", "team": t1});
        for (k, v) in more.as_object().unwrap() {
            fields[k] = v.clone();
        }
        member_at(&w, K1, id, fields)
    };
    let lead = at(L, json!({"depth": 0, "upTeam": t, "name": "leader"}));
    seed_index(&w, &t, K1, "$4"); // reentry-6a Task 9: up made it on K1
    crate::state::evidence::publish(&super::gens::path(&lead, 1, "ended"), &json!({"at": "t"}))
        .unwrap();
    let f = at(X, json!({"parentAgent": {"id": L}}));
    ok(&f, 1);
    let pre = w
        .ctx
        .state_root
        .join(K1)
        .join("agents")
        .join("999999999999");
    std::fs::create_dir_all(&pre).unwrap();
    crate::state::fs::write_json(
        &pre.join("record.json"),
        &json!({"id": "999999999999", "name": "old", "session": "$4", "hcAddress": "t9"}),
    )
    .unwrap();
    lf::seed_gen(&pre, 1, Some("run"), lf::committed());
    let before = bytes(&pre);
    let mut lost = wire_of(&w, X, 1, lf::LOST_ARGVS[0]);
    lost["labels"]["mux.team"] = json!(t1);
    cf::online(&w); // the pass; the base answer serves the connect of up
    found(&w, X, 1, &lost);
    cf::answer(&w, &["get", &format!("a{X}"), "--json"], &lost.to_string());
    relabel_for(&w, &format!("a{X}"), &t1);
    w.fakes.hc_answer(ft::address("t2"));
    reentry_unlisted(&w, X, 2);
    let (code, reply) = up(&w);
    assert_eq!((code, &reply["created"]), (0, &json!(false)), "{reply}");
    assert_eq!(ids(reply.as_object().unwrap(), "restored"), [X]);
    assert_eq!(lf::record(&f)["generation"], "2");
    assert!(!w.folder(X).exists(), "nothing under the caller key");
    assert_eq!(
        bytes(&pre),
        before,
        "the pre-lifecycle folder is byte-identical"
    );
    w.fakes.hc_done().unwrap();
}

/// r6b call 20 (checklist 9, 23): two concurrent `up`s of one folder restoring the same member released by the
/// real R1 writer. A holds up|T past its launch until B waits for it (asserted through the gate); B then reconciles
/// a Running generation 2. One replacement launch, one promoted generation.
#[test]
fn two_concurrent_ups_restore_a_member_once() {
    let a = person();
    let t = team_t(&a);
    leader_of(&a, &t);
    seed_index(&a, &t, &a.caller().server_key, &a.caller().session); // reentry-6a Task 9: up made it
    let f = member(&a, X, json!({"parentAgent": {"id": L}}));
    ok(&f, 1);
    let gate = std::sync::Arc::new(crate::testing::Gate::default());
    a.fakes.share_locks(gate.clone());
    cf::online(&a); // the pass (the base answer serves the connect of up)
    found(&a, X, 1, &wire_of(&a, X, 1, lf::LOST_ARGVS[0]));
    released_by_hc(&a, X, lf::LOST_ARGVS[0]);
    a.fakes.hc_answer(ft::address("t2"));
    reentry_unlisted(&a, X, 2);
    let g = gate.clone();
    a.fakes.lock_probe(move |e| {
        if let Some(k) = e.strip_prefix("-").filter(|k| k.starts_with("up|")) {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while g.waiting(k) == 0 {
                assert!(std::time::Instant::now() < end, "contention timeout");
                std::thread::yield_now();
            }
        }
        String::new()
    });
    let (root, cwd) = (a.ctx.state_root.clone(), a.ctx.cwd.clone());
    std::thread::scope(|scope| {
        let other = scope.spawn(|| {
            let mut b = super::up::tests::base(); // its cf::online serves the connect of up
            (b.ctx.state_root, b.ctx.cwd) = (root.clone(), cwd.clone()); // same folder: same team T
            b.fakes.share_locks(gate.clone());
            let launch = super::gens::path(&b.folder(X), 2, "launch");
            let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !launch.is_file() {
                assert!(std::time::Instant::now() < end, "A never launched");
                std::thread::yield_now();
            }
            let l = super::gens::read(&b.folder(X), 2, "launch")
                .known()
                .unwrap()
                .unwrap();
            let mut w2 = wire_of(&b, X, 2, &[]);
            (w2["name"], w2["argv"], w2["address"]) =
                (l["hcName"].clone(), l["launcherArgv"].clone(), json!("t2"));
            cf::online(&b); // the pass of B
            found(&b, X, 2, &w2);
            cf::online(&b); // re-entry's pass of B (Task 11): X Running, its view shown
            found(&b, X, 2, &w2);
            let (code, reply) = up(&b);
            assert_eq!((code, &reply["created"]), (0, &json!(false)), "{reply}");
            assert_eq!(reply["restored"], json!([]), "B launches nothing");
            assert_eq!(lf::count(&b, "run"), 0);
            b.fakes.hc_done().unwrap();
        });
        let (code, reply) = up(&a);
        assert_eq!(
            ids(reply.as_object().unwrap(), "restored"),
            [X],
            "{code} {reply}"
        );
        other.join().unwrap();
    });
    assert_eq!(gate.waited(&format!("up|{t}")), 1, "B waited for up|T");
    assert_eq!(lf::count(&a, "run"), 1, "one replacement launch");
    assert_eq!(lf::record(&f)["generation"], "2", "one promoted generation");
    assert!(
        super::gens::read(&f, 3, "launch")
            .known()
            .unwrap()
            .is_none()
    );
    a.fakes.hc_done().unwrap();
}
