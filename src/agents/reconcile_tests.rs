//! restore-6b unit tests (fake hc, proc, clock; real files and locks).
use super::gens::Verdict;
use super::lifecycle::Life;
use super::reconcile::{Row, Seen, classify};

fn seen(life: Option<Life>, verdict: Option<Verdict>) -> Seen {
    Seen {
        life,
        verdict,
        ..Seen::default()
    }
}
fn failed(r: &str) -> Option<Verdict> {
    Some(Verdict::Failed(r.into()))
}

/// L2: every row in spec order. Each case fails when only its rule is removed (mutation rows 1.1-1.10).
#[test]
fn every_row_in_order() {
    let lost_ok = seen(Some(Life::Lost), Some(Verdict::Ok));
    let awaiting = Seen {
        cause: Some("restore".into()),
        released: true,
        ..seen(None, Some(Verdict::Ok))
    };
    let cases: Vec<(&str, Seen, Row)> = vec![
        (
            "stopped beats awaiting",
            Seen {
                stopped: true,
                ..awaiting.clone()
            },
            Row::Stopped,
        ),
        (
            "stopped beats lost ok",
            Seen {
                stopped: true,
                ..lost_ok.clone()
            },
            Row::Stopped,
        ),
        (
            "pending is unknown",
            Seen {
                pending: true,
                ..awaiting.clone()
            },
            Row::Unknown("pending".into()),
        ),
        (
            "half release is unknown",
            Seen {
                cause: Some("restore".into()),
                ..seen(Some(Life::Lost), Some(Verdict::Ok))
            },
            Row::Unknown("release".into()),
        ),
        ("awaiting", awaiting.clone(), Row::Await),
        (
            "awaiting, outcome undecided",
            Seen {
                verdict: None,
                ..awaiting.clone()
            },
            Row::Held("outcome".into()),
        ),
        (
            "awaiting, outcome failed",
            Seen {
                verdict: failed("client exited before readiness"),
                ..awaiting.clone()
            },
            Row::Recover("client exited before readiness".into()),
        ),
        (
            "recovery",
            Seen {
                cause: Some("recovery".into()),
                ..awaiting.clone()
            },
            Row::Recover("recovery".into()),
        ),
        (
            "lost cause is ended",
            Seen {
                cause: Some("lost".into()),
                ..awaiting.clone()
            },
            Row::Ended,
        ),
        (
            "ended marker",
            Seen {
                ended: true,
                ..lost_ok.clone()
            },
            Row::Ended,
        ),
        (
            "hc unclear",
            seen(Some(Life::Unknown), None),
            Row::Unknown("hc".into()),
        ),
        (
            "no terminal read",
            seen(None, None),
            Row::Unknown("hc".into()),
        ),
        (
            "running",
            seen(Some(Life::Running), failed("terminal lost")),
            Row::Running,
        ),
        ("lost ok", lost_ok.clone(), Row::Lost),
        (
            "lost failed",
            seen(Some(Life::Lost), failed("terminal lost")),
            Row::Recover("terminal lost".into()),
        ),
        (
            "lost void",
            seen(
                Some(Life::Lost),
                Some(Verdict::Void("commit aborted".into())),
            ),
            Row::Recover("commit aborted".into()),
        ),
        (
            "lost undecided",
            seen(Some(Life::Lost), None),
            Row::Recover("outcome undecided".into()),
        ),
        ("ended life", seen(Some(Life::Ended), None), Row::Ended),
    ];
    for (name, s, want) in cases {
        assert_eq!(classify(&s), want, "{name}");
    }
}

use super::core_fixture as cf;
use super::fixture::World;
use super::life_fixture as lf;
use super::reconcile::observe;
use super::team_state::{self, Scope};
use crate::hc::Lazy;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub(super) const X: &str = "bbbbbbbbbbbb";

/// A lifecycle member `id` of the caller session under `key`: active generation 1 committed, `fields` over it.
pub(super) fn member_at(w: &World, key: &str, id: &str, fields: Value) -> PathBuf {
    let folder = w.ctx.state_root.join(key).join("agents").join(id);
    std::fs::create_dir_all(&folder).unwrap();
    let dir = w.ctx.cwd.to_str().unwrap().to_string();
    let mut r = json!({"lifecycle": 1, "id": id, "name": format!("m{}", &id[..3]), "client": "claude",
        "session": w.caller().session, "sessionId": lf::SID, "team": format!("{key}:{}", w.caller().session),
        "directory": dir, "options": ["--effort", "high"], "prompt": ["fix"], "depth": 1, "generation": "1",
        "hcAddress": format!("a{id}"), "hcName": lf::hc_name(1), "launcherArgv": lf::ARGV});
    for (k, v) in fields.as_object().unwrap() {
        r[k] = v.clone();
    }
    crate::state::fs::write_json(&folder.join("record.json"), &r).unwrap();
    lf::seed_gen(&folder, 1, Some("run"), lf::committed());
    team_record_at(w, key, r["session"].as_str().unwrap_or_default());
    folder
}
/// The team record `up` writes for `session` under `key` (restore-6c checklist 9: `launch_next` requires it), once.
/// Its flags give a `bypass` ceiling, the one a missing record used to give.
pub(super) fn team_record_at(w: &World, key: &str, session: &str) {
    let flags = vec!["--dangerously-skip-permissions".to_string()];
    let team = super::team::Team {
        team: format!("{key}:{session}"),
        session: session.into(),
        name: "s".into(),
        directory: w.ctx.cwd.to_str().unwrap().into(),
        client: crate::parse::Client::Claude,
        policy: super::team::policy(crate::parse::Client::Claude, &flags),
        flags,
        leader: "cccccccccccc".into(),
        created_at: "x".into(),
    };
    super::team::create(
        &super::team::path(&w.ctx.state_root.join(key), session),
        &team,
    )
    .unwrap();
}
pub(super) fn member(w: &World, id: &str, fields: Value) -> PathBuf {
    member_at(w, &w.caller().server_key, id, fields)
}
/// The terminal hc shows for member `id`, generation n, running `argv`.
pub(super) fn wire_of(w: &World, id: &str, n: u64, argv: &[&str]) -> Value {
    json!({"address": format!("a{id}"), "name": lf::hc_name(n), "state": "live", "reason": null,
        "argv": argv, "exit_code": null, "labels": {"mux.managed": "1", "mux.team": lf::team(w),
        "mux.id": id, "mux.gen": n.to_string(), "mux.name": "worker"}})
}
/// `gens::find`: one `ls` by id and generation, then one `get`.
pub(super) fn found(w: &World, id: &str, n: u64, wire: &Value) {
    let (by_id, by_gen) = (format!("mux.id={id}"), format!("mux.gen={n}"));
    let a = wire["address"].as_str().unwrap();
    cf::answer(
        w,
        &[
            "ls", "--all", "--label", &by_id, "--label", &by_gen, "--json",
        ],
        &json!({"terminals": [{"address": a}]}).to_string(),
    );
    cf::answer(w, &["get", a, "--json"], &wire.to_string());
}
/// Generation n ready and adjudicated `ok`, as outcome-2 leaves it.
pub(super) fn ok(folder: &Path, n: u64) {
    for (file, body) in [
        ("ready", json!({"at": "t"})),
        ("outcome", json!({"outcome": "ok", "at": "t"})),
    ] {
        crate::state::evidence::publish(&super::gens::path(folder, n, file), &body).unwrap();
    }
}
pub(super) fn observed(w: &World, id: &str) -> crate::error::Res<Seen> {
    let s = Scope::of(&w.caller());
    team_state::open(&w.ctx, &s, |tx| {
        let lazy = Lazy::new(tx.ctx);
        let super::gens::Member::Life(g) = super::gens::member_of(&tx.folder(id)) else {
            panic!("not a lifecycle member")
        };
        observe(tx, &lazy, &g).map(|(seen, _)| seen)
    })
}

/// R1 of release-1d as hc sees it for member `id` gen 1: the reread of the shell, then the one relabel.
fn released_by_hc(w: &World, id: &str, argv: &[&str]) {
    let a = format!("a{id}");
    cf::answer(
        w,
        &["get", &a, "--json"],
        &wire_of(w, id, 1, argv).to_string(),
    );
    lf::relabel(w, &a);
}
/// Member `id` released for restore through the real R1 writer (r6b call 10).
pub(super) fn released_for_restore(w: &World, id: &str) {
    released_by_hc(w, id, lf::LOST_ARGVS[0]);
    team_state::open(&w.ctx, &Scope::of(&w.caller()), |tx| {
        let lazy = Lazy::new(tx.ctx);
        super::release::release(
            tx,
            &lazy,
            id,
            1,
            &format!("a{id}"),
            super::release::Cause::Restore,
        )
    })
    .unwrap();
}

#[test]
fn waiting_is_a_restore_release_with_nothing_pending() {
    let cases = [
        ("restore", Ok(true)),
        ("pending", Ok(false)),
        ("recovery", Ok(false)),
        ("half", Ok(false)),
        ("none", Ok(false)),
        ("torn", Err("evidence")),
    ];
    for (case, want) in cases {
        let w = World::human("");
        w.state();
        let f = member(&w, X, json!({}));
        let rel = super::gens::path(&f, 1, "release");
        let body = |cause: &str| json!({"address": "a", "hcName": lf::hc_name(1), "cause": cause, "at": "t"});
        let released = || {
            crate::state::evidence::publish(&super::gens::path(&f, 1, "released"), &json!({}))
                .unwrap();
        };
        match case {
            "restore" => {
                cf::online(&w);
                released_for_restore(&w, X);
            }
            "pending" => {
                // released for restore, and a pending n+1 already exists: not waiting (r6b call 11)
                crate::state::evidence::publish(&rel, &body("restore")).unwrap();
                released();
                let mut r = lf::record(&f);
                r["pending"] = json!({"gen": 2, "kind": "restore", "hcName": lf::hc_name(2), "sessionId": lf::SID,
                    "launcherArgv": lf::ARGV});
                crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
            }
            "recovery" => {
                crate::state::evidence::publish(&rel, &body("recovery")).unwrap();
                released();
            }
            "half" => {
                crate::state::evidence::publish(&rel, &body("restore")).unwrap();
            }
            "torn" => std::fs::create_dir_all(&rel).unwrap(),
            _ => {}
        }
        // r6b call 26: from the record the snapshot holds, never reread.
        let got = super::reconcile::waiting(&f, &lf::record(&f));
        assert_eq!(got.map_err(|e| e.check), want, "{case}");
    }
}

/// Task 8 board fixture: a member released for restore, generation 1 ok.
pub(super) fn member_for_board(w: &World) -> PathBuf {
    let f = member(w, X, json!({}));
    ok(&f, 1);
    cf::online(w);
    released_for_restore(w, X); // the real R1 writer (r6b call 10)
    f
}
