//! reentry-6a Task 9: a team is created index first (E6.1-4; calls 1, 20, 22, 23, 33, 36, 39).
use super::*;
use crate::agents::reconcile_tests::{found, wire_of};

pub(super) fn index_of(w: &World) -> Option<index::Index> {
    index::read(&w.ctx, &team_of(w)).unwrap()
}

/// Whether some release file under `up/` reads `Present`.
pub(super) fn released(w: &World) -> bool {
    std::fs::read_dir(w.ctx.state_root.join("up"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "release"))
        .any(|e| {
            matches!(
                crate::state::evidence::read(&e.path()),
                crate::state::evidence::Evidence::Present(_)
            )
        })
}

// E6.1-4: the index (inputs, no anchor) exists before any session; publication sets `view` and `anchor` in one
// write; the leader launches at the anchor; the team record is created there; the release is published last.
#[test]
fn up_writes_the_index_before_any_session() {
    let w = ready();
    let at = index::path(&w.ctx, &team_of(&w));
    w.fakes.mux_script("new-session", move |_| {
        let v: Value =
            serde_json::from_slice(&std::fs::read(&at).expect("E6.1 before new-session")).unwrap();
        assert!(
            v.get("anchor").is_none() && v["pending"]["name"].is_string(),
            "{v}"
        );
        assert_eq!(v["inputs"]["options"], json!(["--model", "a b"]));
        out(0, "$7 @3\n")
    });
    let reply = run(&w, &["up", "-Client", "claude", "--", "--model", "a b"]).unwrap();
    let ix = index_of(&w).unwrap();
    let key = w.caller().server_key;
    assert_eq!(
        ix.anchor,
        Some(crate::caller::Anchor {
            key: key.clone(),
            session: "$7".into()
        })
    );
    assert_eq!(ix.view.as_ref().map(|v| v.session.as_str()), Some("$7"));
    assert_eq!(ix.pending, None);
    assert_eq!(
        (&reply["created"], &reply["replayed"], &reply["reentered"]),
        (&json!(true), &json!(false), &json!(false))
    );
    assert_eq!(
        crate::agents::team::read(&team_file(&w))
            .unwrap()
            .unwrap()
            .leader,
        reply["leader"]["id"].as_str().unwrap()
    );
    let rel = std::fs::read_dir(w.ctx.state_root.join("up"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "release"))
        .expect("a release");
    assert!(rel.is_file(), "published at the end");
    assert!(ix.releases.is_empty(), "9.15: the release left the index");
    assert!(!killed(&w));
}

// Ambiguity 18, call 20: each durable step halts with only its files. E6.1 writes the index with `pending`.
#[test]
fn each_step_of_up_halts_with_only_its_files() {
    for step in ["index", "session", "published", "team", "presented"] {
        let w = ready();
        halt_after(Some(step));
        let e = claude(&w).unwrap_err();
        halt_after(None);
        assert_eq!(
            (e.check, e.message.clone()),
            ("halted", format!("halted after {step}"))
        );
        let ix = index_of(&w).expect("the index exists after every step");
        let sessions = of(&w, "new-session").len();
        // Call 39 (F1): from publication on, the anchor's own server is bound (before any write names the anchor).
        let bound = w
            .ctx
            .state_root
            .join(w.caller().server_key)
            .join("server.json")
            .is_file();
        assert_eq!(bound, step != "index", "{step}: call 39");
        match step {
            "index" => assert!(
                ix.pending.is_some() && ix.anchor.is_none() && sessions == 0,
                "{step}: call 20"
            ),
            "session" => assert!(
                ix.pending.is_some() && ix.view.is_none() && sessions == 1,
                "{step}"
            ),
            "published" => assert!(
                ix.anchor.is_some() && ix.pending.is_none() && !ran(&w) && !team_file(&w).is_file(),
                "{step}"
            ),
            "team" => assert!(
                ran(&w) && team_file(&w).is_file() && !released(&w),
                "{step}"
            ),
            _ => {
                assert!(
                    !of(&w, "switch-client").is_empty() && !released(&w),
                    "{step}: views, bar, switch before the release"
                );
                // N18 (unit part): the outstanding release is durable in the index, its marker absent.
                assert_eq!(
                    ix.releases.len(),
                    1,
                    "{step}: the release kept in the index"
                );
                assert!(!Path::new(&ix.releases[0]).exists(), "{step}");
            }
        }
        assert!(!killed(&w), "{step}");
    }
}

// Call 1: a lifecycle leader of T and no index (a development build): `index`, naming `stop` and `up`.
#[test]
fn a_team_without_an_index_names_stop_and_up() {
    let w = ready();
    let f = agents(&w).join("cccccccccccc");
    std::fs::create_dir_all(&f).unwrap();
    crate::state::fs::write_json(
        &f.join("record.json"),
        &json!({"lifecycle": 1, "id": "cccccccccccc", "name": "leader",
        "upTeam": team_of(&w), "depth": 0, "session": "$1", "generation": "0"}),
    )
    .unwrap();
    crate::agents::life_fixture::seed_gen(
        &f,
        0,
        Some("run"),
        crate::agents::life_fixture::committed(),
    );
    let e = claude(&w).unwrap_err();
    assert_eq!(e.check, "index");
    assert!(
        e.message.contains("mux-agent stop") && e.message.contains("up again"),
        "{}",
        e.message
    );
    assert!(!ran(&w) && of(&w, "new-session").is_empty(), "nothing made");
    assert!(index_of(&w).is_none(), "no index written");
}

// Call 22, Task 9 alone: a second `up` of a created team reconciles at the anchor and creates nothing.
#[test]
fn a_second_up_reconciles_and_creates_nothing() {
    let w = ready();
    let first = claude(&w).unwrap();
    let id = first["leader"]["id"].as_str().unwrap().to_string();
    let mut ix = index_of(&w).unwrap();
    let left = w
        .ctx
        .state_root
        .join("up")
        .join("left.release")
        .to_str()
        .unwrap()
        .to_string();
    ix.releases.push(left.clone());
    index::write(&w.ctx, &ix).unwrap();
    // F10 (checklist #22): the listing names the first session, so `ensure` reuses it.
    let name = first["name"].as_str().unwrap().to_string();
    w.fakes.mux_rule(
        "list-sessions",
        0,
        &format!(
            "$7 {name}
"
        ),
        "",
    );
    cf::online(&w); // up's own hc::connect (finding A13)
    cf::online(&w); // reconcile's hc::Lazy
    found(&w, &id, 0, &running_wire(&w, &agents(&w).join(&id), &id, 0));
    let again = claude(&w).unwrap();
    assert_eq!(
        (&again["created"], &again["reentered"]),
        (&json!(false), &json!(true)),
        "{again:?}"
    );
    assert_eq!(
        (
            of(&w, "new-session").len(),
            w.fakes
                .hc_calls()
                .iter()
                .filter(|c| c.1[0] == "run")
                .count()
        ),
        (1, 1)
    );
    w.fakes.hc_done().unwrap();
    assert!(
        index_of(&w).unwrap().releases.is_empty() && Path::new(&left).is_file(),
        "call 25: a crashed up's release finished"
    );
}

/// A team index for this folder's T, anchored at the caller's key and `$7`.
fn anchored(w: &World, key: &str) -> index::Index {
    index::Index {
        team: team_of(w),
        anchor: Some(crate::caller::Anchor {
            key: key.into(),
            session: "$7".into(),
        }),
        inputs: index::Inputs {
            client: "claude".into(),
            options: vec![],
            prompt: vec![],
            name: String::new(),
            folder: "/p".into(),
        },
        view: None,
        pending: None,
        aliases: vec![],
        releases: vec![],
    }
}

// Calls 33, 36, 39 (findings A1, A7, F1-F3): the anchor's binding is written on its own server, in a private key
// folder, and never invented elsewhere; a present binding must hash to the key; a file in the team record's folder
// place is `index`, not "no team".
#[test]
fn the_anchor_server_is_bound_and_a_wrong_type_team_record_is_index() {
    let w = ready();
    let me = w.caller();
    let key_dir = w.ctx.state_root.join(&me.server_key);
    super::super::bind_anchor_server(&w.ctx, &me, &me.server_key).unwrap();
    assert!(key_dir.join("server.json").is_file(), "bound");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key_dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "call 39: a private key folder");
    }
    let ix = anchored(&w, &me.server_key);
    let teams = crate::agents::team::path(&key_dir, "$7")
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::write(&teams, b"x").unwrap();
    assert_eq!(
        super::super::team_exists(&w.ctx, &ix).unwrap_err().check,
        "index",
        "a wrong-type team folder"
    );
    assert_eq!(
        super::super::bind_anchor_server(&w.ctx, &me, "a1a1a1a1a1a1a1a1")
            .unwrap_err()
            .check,
        "index",
        "absent elsewhere"
    );
    assert!(
        !w.ctx
            .state_root
            .join("a1a1a1a1a1a1a1a1")
            .join("server.json")
            .exists(),
        "never invented"
    );
    // Call 39: a malformed or foreign binding at the anchor key is `index`, on its own server too.
    for body in [
        json!({}),
        json!({"socket": me.socket, "serverPid": me.server_pid + 1, "serverStart": me.server_start}),
    ] {
        crate::state::fs::write_json(&key_dir.join("server.json"), &body).unwrap();
        assert_eq!(
            super::super::bind_anchor_server(&w.ctx, &me, &me.server_key)
                .unwrap_err()
                .check,
            "index",
            "{body}"
        );
    }
}

/// hc's Running terminal for `id`'s generation `n` in `folder`: its `launch.json` `hcName` and `launcherArgv`
/// (a replayed leader's hc name is random), the record's `hcAddress` (else `t1`, the address `forward` was answered).
pub(crate) fn running_wire(w: &World, folder: &Path, id: &str, n: u64) -> Value {
    let launch = gens::read(folder, n, "launch")
        .known()
        .unwrap()
        .expect("launch.json");
    let argv: Vec<&str> = launch["launcherArgv"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let mut t = wire_of(w, id, n, &argv);
    t["name"] = launch["hcName"].clone();
    let rec = crate::agents::life_fixture::record(folder);
    t["address"] = rec
        .get("hcAddress")
        .filter(|a| a.is_string())
        .cloned()
        .unwrap_or(json!("t1"));
    t
}
