//! restore-6c Task 10 (#103): a stopped team is retired and `up` starts a new one through 6a's E6 (P6; calls 12-14,
//! 20-22, 25, 26, 33; D19, D20). After a retirement the World caller's session is retired, so `w.caller()` (which
//! admits) answers `moved`: these fixtures take the old scope from `resolve_person`, before retiring.
use super::index;
use super::reenter_tests as rx;
use super::retire;
use crate::agents::core_fixture as cf;
use crate::agents::core_launch::forward_tests as ft;
use crate::agents::fixture::World;
use crate::agents::reconcile_restore_tests::L;
use crate::agents::reconcile_tests as rt;
use crate::agents::recover_tests::cli;
use crate::agents::team_state::Scope;
use crate::agents::up::tests as ut;
use serde_json::{Value, json};
use std::path::PathBuf;

/// The person, unadmitted (`up`'s own proof): valid before and after its session is retired.
fn me(w: &World) -> crate::caller::Caller {
    crate::caller::resolve_person(&w.ctx).unwrap()
}
/// The stopped team's scope: the World person's key and session (`up` never moves the caller).
fn old(w: &World) -> Scope {
    let c = me(w);
    Scope {
        key: c.server_key,
        session: c.session,
    }
}
fn up(w: &World) -> crate::error::Res<serde_json::Map<String, Value>> {
    super::up(&w.ctx, &w.request(&["up", "-Client", "claude"]))
}
/// A stopped team: 6a's index and team record at the World's scope, leader L generation 1 ready, `ok` and ended,
/// desired `stopped` (the real `Tx::desired`). The state root is made private first (697e372).
fn stopped_team(w: &World) -> PathBuf {
    w.state();
    let t = ut::team_of(w);
    let lead = rt::member(w, L, json!({"depth": 0, "upTeam": t, "name": "leader"}));
    rt::ok(&lead, 1);
    let end = json!({"address": format!("a{L}"), "hcName": crate::agents::life_fixture::hc_name(1),
        "labels": {}, "state": "finished", "reason": "exited", "exitCode": 0, "at": "t"});
    crate::state::evidence::publish(&crate::agents::gens::path(&lead, 1, "ended"), &end).unwrap();
    let o = old(w);
    rt::seed_index(w, &t, &o.key, &o.session);
    crate::agents::team_state::open(&w.ctx, &o, |tx| tx.desired(L, false)).unwrap();
    lead
}
/// An outstanding release of the stopped team's index (call 14): `<root>/up/q.release`.
fn with_release(w: &World) -> PathBuf {
    let at = w.ctx.state_root.join("up").join("q.release");
    let mut ix = index::read(&w.ctx, &ut::team_of(w)).unwrap().unwrap();
    ix.releases = vec![at.to_str().unwrap().into()];
    index::write(&w.ctx, &ix).unwrap();
    at
}
fn retired_dir(w: &World) -> PathBuf {
    w.ctx.state_root.join("up").join("retired")
}
fn retired(w: &World) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(retired_dir(w))
        .map(|d| {
            d.map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}
/// Call 21's archive name for scope `s` at the World's fixed clock.
fn archive_name(w: &World, s: &Scope) -> String {
    retire::archive_of(&ut::team_of(w), &crate::util::utc_o(w.ctx.clock.now()), s)
}
/// The non-void leaders of this folder's team outside session `old`: the successors (final F2).
fn successors(w: &World, key: &str, old: &str) -> Vec<(String, PathBuf)> {
    let t = ut::team_of(w);
    crate::agents::gens::members(&w.ctx.state_root.join(key).join("agents"))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(id, f, m)| match m {
            crate::agents::gens::Member::Life(g)
                if g.record()["upTeam"] == t.as_str()
                    && g.record()["depth"] == 0
                    && g.record()["session"] != old =>
            {
                let s = g.status().unwrap();
                (!g.never_ran_recorded(&s).unwrap()).then_some((id, f))
            }
            _ => None,
        })
        .collect()
}
/// 6a's `seed_launcher` on the successor only (final F2): the retired leader's folder is never written.
fn successor_launcher(w: &World, key: &str, old: &str, step: &str) {
    let claim = match step {
        "run" | "address" => "abandoned",
        "commit" | "promote" => "run",
        _ => return,
    };
    let t = ut::team_of(w);
    for (_, f, m) in
        crate::agents::gens::members(&w.ctx.state_root.join(key).join("agents")).unwrap()
    {
        if let crate::agents::gens::Member::Life(g) = m
            && g.record()["upTeam"] == t.as_str()
            && g.record()["session"] != old
        {
            let _ = crate::state::evidence::publish(
                &crate::agents::gens::claim_path(&f, 0),
                &json!(claim),
            );
        }
    }
}
/// 6a's `script_second_up` for the successor (final F2): its wire is built from the successor's own record, never
/// through `w.caller()` (the World caller's session is retired, D20).
fn script_second_up(w: &World, key: &str, step: &str, leader: Option<&(String, PathBuf)>) {
    cf::online(w);
    let ran = ["commit", "promote", "team", "presented"].contains(&step);
    match leader {
        Some((id, f)) if ran => {
            let wire = successor_wire(key, f, id);
            if step == "commit" {
                cf::online(w); // settle: one Running terminal, promoted
                rt::found(w, id, 0, &wire);
            }
            for _ in 0..2 {
                cf::online(w); // reconcile, then re-entry
                rt::found(w, id, 0, &wire);
            }
        }
        _ => w.fakes.hc_answer(rx::address("t1")),
    }
}
/// hc's Running terminal for the successor's generation 0: 6a's `running_wire`, its `mux.team` the successor's
/// scope `<key>:<record session>` (a pending record has no `team` yet).
fn successor_wire(key: &str, folder: &std::path::Path, id: &str) -> Value {
    let launch = crate::agents::gens::read(folder, 0, "launch")
        .known()
        .unwrap()
        .expect("launch.json");
    let rec = crate::agents::life_fixture::record(folder);
    json!({"address": rec.get("hcAddress").filter(|a| a.is_string()).cloned().unwrap_or(json!("t1")),
        "name": launch["hcName"], "state": "live", "reason": null, "argv": launch["launcherArgv"],
        "exit_code": null, "labels": {"mux.managed": "1", "mux.team": format!("{key}:{}", rec["session"].as_str().unwrap()), "mux.id": id,
        "mux.gen": "0", "mux.name": "worker"}})
}
fn sessions_made(w: &World) -> usize {
    w.fakes
        .mux_calls()
        .iter()
        .filter(|c| c.iter().any(|a| a == "new-session"))
        .count()
}

/// P6 then E6 in one `up` (call 20; final F15 row 10.10: the stopped arm itself, uninterrupted): the old index is
/// renamed under its bound name, the fresh index anchors the new session, the outstanding release is published by
/// E5 and cleared (call 14), the reply names the retired team, and the old leader is untouched.
#[test]
fn a_stopped_team_is_retired_and_up_starts_a_new_one() {
    let w = ut::base();
    let lead = stopped_team(&w);
    let release = with_release(&w);
    let o = old(&w);
    let before = crate::agents::cascade_fixture::snapshot(&lead);
    w.fakes.hc_answer(ft::address("t2"));
    let reply = up(&w).unwrap();
    assert_eq!(
        (&reply["created"], &reply["reentered"], &reply["retired"]),
        (
            &json!(true),
            &json!(false),
            &json!(format!("{}:{}", o.key, o.session))
        ),
        "{reply:?}"
    );
    assert_eq!(retired(&w), vec![archive_name(&w, &o)], "the bound name");
    let now = index::read(&w.ctx, &ut::team_of(&w)).unwrap().unwrap();
    assert_eq!(
        now.anchor.unwrap().session,
        "$7",
        "the index at T anchors the successor"
    );
    assert!(
        release.is_file(),
        "the outstanding release is published by E5"
    );
    assert!(now.releases.is_empty(), "then cleared");
    assert_eq!(successors(&w, &o.key, &o.session).len(), 1, "one successor");
    assert_eq!(
        crate::agents::cascade_fixture::snapshot(&lead),
        before,
        "the old leader untouched"
    );
    w.fakes.hc_done().unwrap();
}

/// P6.1, call 7: an unresolved leader stop refuses 1 `stopping`, retires nothing and opens no session.
#[test]
fn retirement_waits_for_an_unresolved_stop() {
    let w = ut::base();
    w.state();
    let lead = rt::member(
        &w,
        L,
        json!({"depth": 0, "upTeam": ut::team_of(&w), "name": "leader"}),
    );
    rt::ok(&lead, 1);
    let o = old(&w);
    rt::seed_index(&w, &ut::team_of(&w), &o.key, &o.session);
    let none = || {
        cf::answer(
            &w,
            &[
                "ls",
                "--all",
                "--label",
                &format!("mux.id={L}"),
                "--label",
                "mux.gen=1",
                "--json",
            ],
            r#"{"terminals":[]}"#,
        )
    };
    none(); // `stop leader`: its terminate takes `base()`'s connection; hc knows no terminal: unresolved
    let (code, stop) = cli(&w, &["stop", "leader"]);
    assert_eq!((code, &stop["stopped"]), (0, &json!(false)), "{stop}");
    cf::online(&w); // `up`'s eager connection
    // Checklist 68, the observed order (`hc_calls`): each finish takes its own connection, then lists generation 1
    cf::online(&w); // `up`'s own C8 finish before `drive` (the journal still waits)
    none();
    cf::online(&w); // the pass's cascade finish
    none();
    cf::online(&w); // `retire`'s own finish, in its own Tx
    none();
    let e = up(&w).unwrap_err();
    assert_eq!((e.code, e.check), (1, "stopping"), "{}", e.message);
    assert!(
        !retire::marker_path(&w.ctx, &o).exists() && retired(&w).is_empty(),
        "nothing retired"
    );
    assert_eq!(sessions_made(&w), 0, "nothing created");
    w.fakes.hc_done().unwrap();
}

/// Calls 22, 25 (final F2, F3; checklist 51, 52): killed after each P6 step and each step of 6a's E6 matrix for the
/// successor, the next `up` (6a's replay, successor-only fixtures) ends with one marker, the old index renamed once
/// and anchored at the old scope, exactly one non-void successor in one new session, the outstanding release
/// published, and the old leader byte-identical.
#[test]
fn retirement_survives_a_crash_at_each_step() {
    let steps = [
        "retired",
        "renamed",
        "reindexed",
        "session",
        "published",
        "allocated",
        "pending",
        "spec",
        "launch",
        "run",
        "address",
        "commit",
        "promote",
        "team",
        "presented",
    ];
    let at = |s: &str| steps.iter().position(|x| *x == s);
    for step in steps {
        let (w, made) = rx::crash_world();
        let lead = stopped_team(&w);
        let release = with_release(&w);
        let o = old(&w);
        let before = crate::agents::cascade_fixture::snapshot(&lead);
        if at(step) >= at("run") {
            w.fakes.hc_answer(rx::address("t1"));
        }
        rx::crash_after(step);
        let (code, first) = cli(&w, &["up", "-Client", "claude"]);
        rx::no_crash();
        assert_ne!(code, 0, "{step}: stopped {first}");
        let w = rx::reopen(&w, &made);
        successor_launcher(&w, &o.key, &o.session, step);
        let ran = successors(&w, &o.key, &o.session).pop();
        script_second_up(&w, &o.key, step, ran.as_ref());
        let (code, second) = cli(&w, &["up", "-Client", "claude"]);
        assert_eq!(code, 0, "{step}: {second}");
        assert!(
            retire::marker_path(&w.ctx, &o).is_file(),
            "{step}: the marker"
        );
        assert_eq!(retired(&w).len(), 1, "{step}: the index renamed once");
        let renamed = index::read_retired(&w.ctx, &retired_dir(&w).join(&retired(&w)[0])).unwrap();
        assert_eq!(
            renamed.anchor.map(|a| (a.key, a.session)),
            Some((o.key.clone(), o.session.clone())),
            "{step}: the retired index is the old team's"
        );
        assert_eq!(
            successors(&w, &o.key, &o.session).len(),
            1,
            "{step}: one successor: {second}"
        );
        assert_eq!(made.borrow().len(), 1, "{step}: one new session");
        assert!(release.is_file(), "{step}: the release published");
        assert_eq!(
            crate::agents::cascade_fixture::snapshot(&lead),
            before,
            "{step}: the old leader untouched"
        );
        w.fakes.hc_done().unwrap();
    }
}

/// Call 21: a marker that is a folder, torn, malformed or foreign (another anchor, team or archive) is 1
/// `retiring`; nothing is created or renamed.
#[test]
fn retirement_refuses_a_marker_not_bound_to_this_team() {
    for case in ["dir", "torn", "empty", "anchor", "team", "archive"] {
        let w = ut::base();
        stopped_team(&w);
        let o = old(&w);
        let marker = retire::marker_path(&w.ctx, &o);
        let mut body = json!({"schema": 1, "team": ut::team_of(&w), "anchor": {"key": o.key, "session": o.session},
            "at": crate::util::utc_o(w.ctx.clock.now()), "archive": archive_name(&w, &o)});
        match case {
            "dir" => std::fs::create_dir(&marker).unwrap(),
            "torn" => std::fs::write(&marker, "{torn").unwrap(),
            "empty" => body = json!({}),
            "anchor" => body["anchor"]["session"] = json!("$99"),
            "team" => body["team"] = json!("0123456789abcdef"),
            _ => body["archive"] = json!("x.json"),
        }
        if !["dir", "torn"].contains(&case) {
            crate::state::evidence::publish(&marker, &body).unwrap();
        }
        let e = up(&w).unwrap_err();
        assert_eq!((e.code, e.check), (1, "retiring"), "{case}: {}", e.message);
        assert_eq!(sessions_made(&w), 0, "{case}: nothing created");
        assert!(
            index::path(&w.ctx, &ut::team_of(&w)).is_file(),
            "{case}: nothing renamed"
        );
    }
}

/// Call 21: step 3 renames the index (the source is gone at `renamed`, the archive has its bytes) under the bound
/// name; the next `up` writes E6.1's fresh index, carrying the archived release (call 25), which anchors the
/// successor.
#[test]
fn retirement_renames_the_index_under_its_bound_name() {
    let w = ut::base();
    stopped_team(&w);
    let release = with_release(&w);
    let o = old(&w);
    let live = index::path(&w.ctx, &ut::team_of(&w));
    let bytes = std::fs::read(&live).unwrap();
    super::halt_after(Some("renamed"));
    let e = up(&w).unwrap_err();
    super::halt_after(None);
    assert_eq!(e.check, "halted", "{}", e.message);
    assert!(!live.exists(), "renamed, not copied: the source is gone");
    assert_eq!(retired(&w), vec![archive_name(&w, &o)], "the bound name");
    assert_eq!(
        std::fs::read(retired_dir(&w).join(&retired(&w)[0])).unwrap(),
        bytes,
        "the same bytes"
    );
    cf::online(&w); // the second `up`'s eager connection
    w.fakes.hc_answer(ft::address("t2"));
    let reply = up(&w).unwrap();
    assert_eq!(reply["created"], true, "{reply:?}");
    let now = index::read(&w.ctx, &ut::team_of(&w)).unwrap().unwrap();
    assert_ne!(
        now.anchor.unwrap().session,
        o.session,
        "the index at T anchors the successor (6a E6)"
    );
    assert!(
        release.is_file(),
        "the archived release is carried and published"
    );
    w.fakes.hc_done().unwrap();
}

/// Call 21: two retirements of T at the same instant keep both archives (the anchor hash differs); an occupied
/// destination refuses `retiring` and overwrites nothing.
#[test]
fn no_retirement_overwrites_another() {
    let w = ut::base();
    stopped_team(&w);
    let (t, o) = (ut::team_of(&w), old(&w));
    retire::retire(&w.ctx, &me(&w), &o, &t).unwrap();
    let second = Scope {
        key: o.key.clone(),
        session: "$5".into(),
    };
    // The second predecessor's live index, written directly: after the first retirement the World caller's
    // session is retired, so fixtures that admit it (`member_at`, `seed_index_at`) answer `moved` (D20).
    let mut ix = index::read_retired(&w.ctx, &retired_dir(&w).join(&retired(&w)[0])).unwrap();
    ix.anchor = Some(crate::caller::Anchor {
        key: o.key.clone(),
        session: "$5".into(),
    });
    ix.view = None;
    index::write(&w.ctx, &ix).unwrap();
    retire::retire(&w.ctx, &me(&w), &second, &t).unwrap();
    let mut anchors: Vec<String> = retired(&w)
        .iter()
        .map(|n| {
            index::read_retired(&w.ctx, &retired_dir(&w).join(n))
                .unwrap()
                .anchor
                .unwrap()
                .session
        })
        .collect();
    anchors.sort();
    assert_eq!(anchors, ["$3", "$5"], "both predecessors stay discoverable");

    let w = ut::base();
    stopped_team(&w);
    let o = old(&w);
    let taken = index::retired_dir_made(&w.ctx)
        .unwrap()
        .join(archive_name(&w, &o));
    std::fs::write(&taken, "occupied").unwrap();
    let e = up(&w).unwrap_err();
    assert_eq!((e.code, e.check), (1, "retiring"), "{}", e.message);
    assert_eq!(
        std::fs::read(&taken).unwrap(),
        b"occupied",
        "not overwritten"
    );
    assert!(
        index::path(&w.ctx, &ut::team_of(&w)).is_file(),
        "the live index stays"
    );
}

/// Call 21 (finding 4): a rename whose folder flush failed is visible; the next `up` re-runs the flushes before it
/// accepts the archive (fault armed: refused; cleared: the successor is created).
#[test]
fn replay_flushes_a_visible_archive_before_accepting_it() {
    let w = ut::base();
    stopped_team(&w);
    crate::state::fs::fail_sync_dir(Some(&retired_dir(&w)));
    assert!(up(&w).is_err(), "the flush after the rename failed");
    assert_eq!(retired(&w).len(), 1, "the rename is visible");
    cf::online(&w);
    let e = up(&w).unwrap_err();
    crate::state::fs::fail_sync_dir(None);
    assert_eq!(
        e.check, "retiring",
        "replay re-runs the flush with the fault armed: {}",
        e.message
    );
    cf::online(&w);
    w.fakes.hc_answer(ft::address("t2"));
    assert_eq!(up(&w).unwrap()["created"], true);
    w.fakes.hc_done().unwrap();
}

/// Calls 26, 33 (D20; checklist 50, 72): a retired team is never reopened. A person verb in its session answers 1
/// `moved` (the retired archive's anchor claims it) after an uninterrupted retirement and after a crash at
/// `renamed` and `reindexed`; nothing is launched.
#[test]
fn a_person_verb_in_a_retired_session_answers_moved() {
    for (halt, verb) in [
        (None, &["restore"][..]),
        (None, &["resume", rt::X][..]),
        (None, &["resume", rt::X, "-Fresh"][..]),
        (Some("renamed"), &["restore"][..]),
        (Some("reindexed"), &["restore"][..]),
    ] {
        let w = ut::base();
        stopped_team(&w);
        let x = rt::member(&w, rt::X, json!({"parentAgent": {"id": L}, "depth": 1}));
        crate::agents::recover_tests::failed_ended(&x, 1);
        match halt {
            None => {
                w.fakes.hc_answer(ft::address("t2"));
                assert_eq!(
                    up(&w).unwrap()["created"],
                    true,
                    "{verb:?}: retired and replaced"
                );
            }
            Some(h) => {
                super::halt_after(Some(h));
                assert_eq!(up(&w).unwrap_err().check, "halted", "{h}");
                super::halt_after(None);
            }
        }
        let (code, reply) = cli(&w, verb);
        assert_eq!(
            (code, &reply["check"], &reply["message"]),
            (1, &json!("moved"), &json!(crate::agents::scope::MOVED)),
            "{halt:?} {verb:?}: {reply}"
        );
        assert!(
            !crate::agents::gens::path(&x, 2, "spec").exists(),
            "{verb:?}: nothing launched"
        );
    }
}

/// Call 12, checklist 54, 70: the index readers' shared negative table. A torn body, a present `null`, a wrong-type
/// field and another T refuse `index` through both `index::read` (the live index) and `read_retired` (an archive);
/// `read_retired` also refuses a folder, a file outside `up/retired` and (Unix) a link.
#[test]
fn both_index_readers_refuse_the_same_bad_index() {
    let w = ut::base();
    stopped_team(&w);
    let (t, o) = (ut::team_of(&w), old(&w));
    retire::retire(&w.ctx, &me(&w), &o, &t).unwrap();
    let dir = retired_dir(&w);
    let good = dir.join(&retired(&w)[0]);
    assert_eq!(
        index::read_retired(&w.ctx, &good).unwrap().team,
        t,
        "the archive reads"
    );
    let body: Value = serde_json::from_slice(&std::fs::read(&good).unwrap()).unwrap();
    let z = "0".repeat(16);
    let bad: [(&str, Option<Value>); 4] = [
        ("torn", None),
        (
            "null",
            Some({
                let mut b = body.clone();
                b["view"] = Value::Null;
                b
            }),
        ),
        (
            "type",
            Some({
                let mut b = body.clone();
                b["releases"] = json!("x");
                b
            }),
        ),
        (
            "team",
            Some({
                let mut b = body.clone();
                b["team"] = json!("0123456789abcdef");
                b
            }),
        ),
    ];
    for (i, (case, b)) in bad.iter().enumerate() {
        let text = b.as_ref().map_or("{torn".to_string(), Value::to_string);
        let archived = dir.join(format!("{t}.{i}.{z}.json"));
        std::fs::write(&archived, &text).unwrap();
        std::fs::write(index::path(&w.ctx, &t), &text).unwrap();
        let read = index::read(&w.ctx, &t).map(|_| ()).map_err(|e| e.check);
        let retired = index::read_retired(&w.ctx, &archived)
            .map(|_| ())
            .map_err(|e| e.check);
        assert_eq!((read, retired), (Err("index"), Err("index")), "{case}");
        std::fs::remove_file(&archived).unwrap();
    }
    std::fs::remove_file(index::path(&w.ctx, &t)).unwrap();
    let folder = dir.join(format!("{t}.7.{z}.json"));
    std::fs::create_dir(&folder).unwrap();
    let outside = w.ctx.state_root.join("up").join(format!("{t}.8.{z}.json"));
    std::fs::copy(&good, &outside).unwrap();
    for p in [&folder, &outside] {
        assert_eq!(
            index::read_retired(&w.ctx, p)
                .map(|_| ())
                .map_err(|e| e.check),
            Err("index"),
            "{p:?}"
        );
    }
    #[cfg(unix)]
    {
        let link = dir.join(format!("{t}.9.{z}.json"));
        std::os::unix::fs::symlink(&good, &link).unwrap();
        assert_eq!(
            index::read_retired(&w.ctx, &link)
                .map(|_| ())
                .map_err(|e| e.check),
            Err("index"),
            "a link"
        );
    }
}

/// Checklist 56 (final F7): a missing `retired` folder never bypasses the ancestors' check: `up` a file in a
/// folder's place refuses `index` from every retired reader, never "no archive".
#[test]
fn a_missing_retired_folder_still_checks_its_ancestors() {
    let w = ut::base();
    w.state();
    let up_dir = w.ctx.state_root.join("up");
    std::fs::write(&up_dir, "not a folder").unwrap();
    let got =
        |r: crate::error::Res<Vec<(String, index::Index)>>| r.map(|v| v.len()).map_err(|e| e.check);
    assert_eq!(
        got(index::every_retired(&w.ctx, None)),
        Err("index"),
        "every_retired"
    );
    assert_eq!(
        retire::carried(&w.ctx, "0123456789abcdef", vec![]).map_err(|e| e.check),
        Err("index"),
        "carried"
    );
}

/// Call 25 (10-13 F1): a marker and a live index that agree with each other but name another team F at this
/// anchor are refused before any archive or index evidence is accepted: `bound` checks the T `up` derived.
#[test]
fn a_foreign_but_self_consistent_retirement_is_refused() {
    let w = ut::base();
    stopped_team(&w);
    let o = old(&w);
    let foreign = "0123456789abcdef";
    let at = crate::util::utc_o(w.ctx.clock.now());
    let archive = retire::archive_of(foreign, &at, &o);
    let mut ix = index::read(&w.ctx, &ut::team_of(&w)).unwrap().unwrap();
    ix.team = foreign.into();
    index::write(&w.ctx, &ix).unwrap();
    crate::state::evidence::publish(
        &retire::marker_path(&w.ctx, &o),
        &json!({"schema": 1, "team": foreign, "anchor": {"key": o.key, "session": o.session}, "at": at,
            "archive": archive}),
    )
    .unwrap();
    let e = retire::retired_state(&w.ctx, &o, &ut::team_of(&w)).unwrap_err();
    assert_eq!((e.code, e.check), (1, "retiring"), "{}", e.message);
    assert!(!retired_dir(&w).join(&archive).exists(), "nothing renamed");
}

/// 10-13 F7, checklist 55: a folder or a link (a junction on Windows) in the archive's place is never an archive,
/// and an archive anchored at another scope than its marker is not this team's; both `retired_state` and `up`
/// refuse `retiring`.
#[test]
fn an_archive_not_bound_to_its_marker_is_refused() {
    for case in ["dir", "link", "anchor"] {
        let w = ut::base();
        stopped_team(&w);
        let o = old(&w);
        retire::retire(&w.ctx, &me(&w), &o, &ut::team_of(&w)).unwrap();
        let name = retired(&w).pop().unwrap();
        let at = retired_dir(&w).join(&name);
        let aside = retired_dir(&w).join("aside");
        std::fs::create_dir(&aside).unwrap();
        match case {
            "dir" => {
                std::fs::rename(&at, aside.join(&name)).unwrap();
                std::fs::create_dir(&at).unwrap();
            }
            "link" => {
                std::fs::rename(&at, aside.join(&name)).unwrap();
                crate::testing::link_dir(&aside, &at);
            }
            _ => {
                let mut v: Value = serde_json::from_slice(&std::fs::read(&at).unwrap()).unwrap();
                v["anchor"]["session"] = json!("$99");
                v["view"]["session"] = json!("$99");
                std::fs::write(&at, v.to_string()).unwrap();
            }
        }
        let e = retire::retired_state(&w.ctx, &o, &ut::team_of(&w)).unwrap_err();
        assert_eq!((e.code, e.check), (1, "retiring"), "{case}: {}", e.message);
        let e = up(&w).unwrap_err();
        assert_eq!(
            (e.code, e.check),
            (1, "retiring"),
            "{case}: up: {}",
            e.message
        );
        assert_eq!(sessions_made(&w), 0, "{case}: nothing created");
    }
}

/// `gens::terminate` of member `id`'s Running generation n: its own connection, `find`, the reread, the kill.
fn terminated(w: &World, id: &str, n: u64) {
    let (wire, a) = (
        rt::wire_of(w, id, n, &crate::agents::life_fixture::ARGV),
        format!("a{id}"),
    );
    cf::online(w);
    cf::answer(
        w,
        &[
            "ls",
            "--all",
            "--label",
            &format!("mux.id={id}"),
            "--label",
            &format!("mux.gen={n}"),
            "--json",
        ],
        &json!({"terminals": [{"address": a}]}).to_string(),
    );
    for _ in 0..2 {
        cf::answer(w, &["get", &a, "--json"], &wire.to_string());
    }
    cf::answer(w, &["kill", &a, "--json"], r#"{"ok":true}"#);
}

/// Spec "interrupted team stop": `stop leader` killed after the journal's publication, before the leader's desired
/// write. One `up` finishes the stop (deepest first) in the pass, reads L stopped, retires and creates exactly one
/// successor; old members stay stopped.
#[test]
fn an_interrupted_team_stop_is_finished_by_one_up() {
    let w = ut::base();
    w.state();
    let lead = rt::member(
        &w,
        L,
        json!({"depth": 0, "upTeam": ut::team_of(&w), "name": "leader"}),
    );
    rt::ok(&lead, 1);
    let x = rt::member(&w, rt::X, json!({"parentAgent": {"id": L}, "depth": 1}));
    rt::ok(&x, 1);
    let o = old(&w);
    rt::seed_index(&w, &ut::team_of(&w), &o.key, &o.session);
    crate::agents::release::halt_after(Some("cascade-journal"));
    let (code, _) = cli(&w, &["stop", "leader"]);
    crate::agents::release::halt_after(None);
    assert_ne!(code, 0, "halted after the journal");
    use crate::agents::cascade::{Desired, desired_of};
    assert_ne!(desired_of(&lead).unwrap(), Desired::Stopped);
    if !w.fakes.hc_calls().is_empty() {
        cf::online(&w); // the halted stop took `base()`'s connection: `up`'s eager connection needs its own
    }
    terminated(&w, rt::X, 1); // the pass: cascade finish, deepest first
    terminated(&w, L, 1);
    w.fakes.hc_answer(ft::address("t2"));
    let reply = up(&w).unwrap();
    assert_eq!(
        (&reply["created"], reply.get("retired").is_some()),
        (&json!(true), true),
        "{reply:?}"
    );
    assert_eq!(
        successors(&w, &o.key, &o.session).len(),
        1,
        "exactly one successor"
    );
    for (who, f) in [("leader", &lead), ("X", &x)] {
        assert_eq!(desired_of(f).unwrap(), Desired::Stopped, "{who}");
    }
    w.fakes.hc_done().unwrap();
}
