use super::*;
use crate::agents::{
    core_fixture as cf,
    fixture::{SERVER, World},
    gens,
};
use crate::spawn::Out;
use std::{cell::Cell, path::PathBuf, rc::Rc};

fn out(code: i32, s: &str) -> Res<crate::mux::Output> {
    Ok(crate::mux::Output {
        code,
        stdout: s.into(),
        stderr: String::new(),
    })
}
fn fails(w: &World, contains: &str) {
    w.fakes.mux_script(contains, |_| out(1, ""));
}
pub(crate) fn base() -> World {
    let mut w = World::human("");
    w.ctx.cwd = w._dir.path().join("project.v1");
    std::fs::create_dir_all(&w.ctx.cwd).unwrap();
    cf::online(&w);
    w.fakes.mux_rule("new-session", 0, "$7 @3\n", "");
    // The view session holds a live view (the World's shell, a running process that is no side bar), so a later
    // `up` reuses it.
    w.fakes.mux_rule(
        "list-panes -s -t $7 -F #{pane_dead}|#{pane_pid}",
        0,
        &format!("0|{}\n", crate::agents::fixture::SHELL),
        "",
    );
    w.fakes.mux_rule("@mux-agents-sidebar", 0, "off\n", "");
    w.fakes
        .mux_rule("new-window", 0, "%9|@5|exact,#} attach t1\n", "");
    w.launches_hold();
    w.fakes
        .mux_rule("-t %9 #{pane_id}|#{@agent-id}", 0, "|\n", "");
    w.fakes
        .mux_script("up #{pid}", |_| out(0, &format!("up {SERVER}\n")));
    w
}
pub(crate) fn ready() -> World {
    let w = base();
    w.fakes.hc_answer(Ok(Out {
        stdout: r#"{"address":"t1"}"#.into(),
        ..Out::default()
    }));
    w
}
fn run(w: &World, args: &[&str]) -> Res<Map<String, Value>> {
    up(&w.ctx, &w.request(args))
}
fn claude(w: &World) -> Res<Map<String, Value>> {
    run(w, &["up", "-Client", "claude"])
}
fn verb(c: &[String]) -> &str {
    c.iter()
        .find(|t| !t.starts_with('-'))
        .map_or("", String::as_str)
}
fn of(w: &World, v: &str) -> Vec<Vec<String>> {
    w.fakes
        .mux_calls()
        .into_iter()
        .filter(|c| verb(c) == v)
        .collect()
}
pub(crate) fn team_of(w: &World) -> String {
    identity::team(
        &w.ctx,
        &identity::folder(&w.ctx).unwrap(),
        Client::Claude,
        "",
    )
    .unwrap()
}
/// The published release markers under `up/` (reentry-6a: `up/` also holds the team index `<T>.json`).
fn releases(w: &World) -> usize {
    std::fs::read_dir(w.ctx.state_root.join("up")).map_or(0, |d| {
        d.flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "release"))
            .count()
    })
}
/// reentry-6a call 22: the session `new-session` made is published in the index, its release kept there.
fn kept_in_index(w: &World) -> bool {
    create::index_of(w)
        .is_some_and(|ix| ix.view.is_some() && ix.pending.is_none() && ix.releases.len() == 1)
        && !create::released(w)
}
fn ran(w: &World) -> bool {
    w.fakes.hc_calls().iter().any(|(_, a, _)| a[0] == "run")
}
fn killed(w: &World) -> bool {
    w.fakes
        .mux_calls()
        .iter()
        .any(|c| ["kill-session", "kill-window"].contains(&verb(c)))
}
fn agents(w: &World) -> PathBuf {
    w.ctx.state_root.join(w.caller().server_key).join("agents")
}
fn leader_folder(w: &World) -> PathBuf {
    std::fs::read_dir(agents(w))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
}
pub(crate) fn team_file(w: &World) -> PathBuf {
    crate::agents::team::path(&w.ctx.state_root.join(w.caller().server_key), "$7")
}

#[test]
fn up_creates_the_team_in_a_new_session() {
    let w = ready();
    let key = w.caller().server_key;
    let (prompt, options) = (["\"q\""], ["--model", "a b"]);
    let reply = run(
        &w,
        &[&["up", "-Client", "claude", "--"][..], &prompt, &options].concat(),
    )
    .unwrap();
    let (t, name, id) = (
        team_of(&w),
        reply["name"].as_str().unwrap().to_string(),
        reply["leader"]["id"].as_str().unwrap().to_string(),
    );
    // reentry-6a F8: A3b-2's `view`, then Task 9's `replayed` and `reentered`; Task 11 (finding 21): `views`,
    // `duplicates`, `members`.
    let want = json!({"view": "attached", "team": t, "session": "$7", "name": name, "created": true, "replayed": false, "reentered": false, "leader": {"id": id, "name": "leader", "hcAddress": "t1"}, "views": [{"id": id, "name": "leader", "pane": "%9", "window": "@5"}], "duplicates": 0, "members": [{"id": id, "name": "leader", "life": "running"}], "sidebar": null});
    assert_eq!(Value::Object(reply.clone()).to_string(), want.to_string());
    assert!(
        name.starts_with(&format!("project_v1-{}-", &t[..8]))
            && name.len() == "project_v1-".len() + 8 + 1 + 32
    );
    let ns = &of(&w, "new-session")[0];
    assert!(ns.contains(&"-d".into()) && ns.contains(&"-P".into()) && !ns.contains(&"-A".into()));
    assert_eq!(
        ns[ns.len() - 3..ns.len() - 1].join(" "),
        format!("{} hold", w.ctx.exe.display())
    );
    assert!(
        Path::new(ns.last().unwrap()).starts_with(w.ctx.state_root.join("up")) && releases(&w) == 1
    );
    let calls = w.fakes.mux_calls();
    let at = |v: &str| calls.iter().position(|c| verb(c) == v).unwrap();
    assert!(at("new-session") < at("set-option") && at("set-option") < at("new-window"));
    assert!(
        of(&w, "set-option")[0]
            .join(" ")
            .ends_with(&format!("-t ={name}:@3 remain-on-exit off"))
    );
    for c in &calls[at("new-session")..] {
        for target in c.windows(2).filter(|p| p[0] == "-t").map(|p| &p[1]) {
            assert!(
                target.starts_with(['=', '$', '@']) || target == "%9",
                "{c:?}"
            );
        }
    }
    assert!(!killed(&w));
    let hc_run = w
        .fakes
        .hc_calls()
        .into_iter()
        .find(|(_, a, _)| a[0] == "run")
        .unwrap()
        .1;
    assert!(
        hc_run.contains(&format!("mux.up={t}")) && hc_run.contains(&format!("mux.team={key}:$7"))
    );
    let r = crate::state::fs::read_json(&agents(&w).join(&id).join("record.json"))
        .unwrap()
        .unwrap();
    assert_eq!(
        (&r["depth"], &r["parentPane"], &r["upTeam"], &r["session"]),
        (&json!(0), &Value::Null, &json!(t), &json!("$7"))
    );
    let spec = crate::state::fs::read_json(&gens::path(&agents(&w).join(&id), 0, "spec"))
        .unwrap()
        .unwrap();
    assert_eq!(
        spec["flags"],
        json!(options),
        "only the options are client flags"
    );
    assert_eq!(spec["args"].as_array().unwrap().last().unwrap(), "\"q\"");
    assert_eq!(
        (&r["options"], &r["prompt"]),
        (&json!(options), &json!(prompt))
    );
    let team = crate::agents::team::read(&team_file(&w)).unwrap().unwrap();
    assert_eq!(team.flags, options, "the ceiling is fixed from the options");
    assert_eq!(
        (team.team.as_str(), team.name.as_str(), team.leader.as_str()),
        (t.as_str(), name.as_str(), id.as_str())
    );
    assert_eq!(of(&w, "select-window")[0].last().unwrap(), "@5");
    assert_eq!(
        of(&w, "switch-client")[0].last().unwrap(),
        &format!("={name}")
    );
}

#[test]
fn the_record_is_flushed_before_hc_run_and_the_team_record_follows_the_commit() {
    let w = ready();
    let (fakes, dir, seen) = (w.fakes.clone(), agents(&w), Rc::new(Cell::new(0)));
    let (s, team) = (seen.clone(), team_file(&w));
    w.fakes.hc_probe(move |argv| {
        if argv.first().map(String::as_str) != Some("run") {
            return;
        }
        let folder = std::fs::read_dir(&dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let r = crate::state::fs::read_json(&folder.join("record.json"))
            .unwrap()
            .unwrap();
        assert!(r.get("hcAddress").is_none() && r["upTeam"].is_string());
        assert_eq!(r["pending"]["gen"], 0, "the pending write precedes hc run");
        assert!(
            fakes
                .mux_calls()
                .iter()
                .any(|c| c.iter().any(|t| t == "remain-on-exit"))
        );
        s.set(s.get() + 1);
    });
    let (s, dir) = (seen.clone(), agents(&w));
    w.fakes.mux_script("new-window", move |_| {
        let folder = std::fs::read_dir(&dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(gens::committed(&folder, 0).unwrap() && !team.exists());
        s.set(s.get() + 1);
        out(0, "%9|@5|exact,#} attach t1\n")
    });
    claude(&w).unwrap();
    assert!(
        seen.get() == 2
            && team_file(&w).exists()
            && gens::committed(&leader_folder(&w), 0).unwrap()
    );
}

#[test]
fn post_commit_failures_are_reported_and_the_leader_stays() {
    let w = ready();
    // reentry-6a Task 10: a torn team record seeded before `up` is `index` before anything is made (call 21); the
    // post-commit case tears it after the leader committed (the view's `new-window` runs after promotion).
    let torn = team_file(&w);
    w.fakes.mux_script("new-window", move |_| {
        std::fs::create_dir_all(torn.parent().unwrap()).unwrap();
        std::fs::write(&torn, "{}").unwrap();
        out(
            0,
            "%9|@5|exact,#} attach t1
",
        )
    });
    fails(&w, "@mux-agents-sidebar");
    fails(&w, "select-window");
    let reply = claude(&w).unwrap();
    assert!(
        ["sidebarError", "layoutError", "teamRecordError"]
            .iter()
            .all(|k| reply[*k].is_string()),
        "{reply:?}"
    );
    assert!(
        of(&w, "switch-client").is_empty()
            && !killed(&w)
            && !w.fakes.hc_calls().iter().any(|(_, a, _)| a[0] == "kill")
    );
    let switch = ready();
    fails(&switch, "switch-client");
    assert!(claude(&switch).unwrap()["layoutError"].is_string() && !killed(&switch));
    let release = ready(); // the release write fails after the commit: `up` is then a file
    let up_dir = release.ctx.state_root.join("up");
    release.fakes.mux_script("new-window", move |_| {
        std::fs::remove_dir_all(&up_dir).unwrap();
        std::fs::write(&up_dir, "").unwrap();
        out(0, "%9|@5|exact,#} attach t1\n")
    });
    let reply = claude(&release).unwrap();
    // reentry-6a ambiguity 7: the release is published last, after the bar, the window and the client.
    assert!(
        reply["releaseError"].is_string()
            && reply.get("layoutError").is_none()
            && reply.get("leftoverSession").is_none()
            && !of(&release, "switch-client").is_empty(),
        "{reply:?}"
    );
}

// reentry-6a call 22: no failure names a leftover session any more. A session `new-session` made is published in
// the index with its release kept there (a second `up` reuses it); an unproven one keeps its release in the index.
#[test]
fn cleanup_kills_nothing_and_keeps_a_made_session_in_the_index() {
    let unproven = |w: &World| {
        create::index_of(w)
            .is_some_and(|ix| ix.view.is_none() && ix.pending.is_none() && ix.releases.len() == 1)
    };
    let taken = ready();
    fails(&taken, "new-session");
    let e = claude(&taken).unwrap_err();
    assert_eq!(
        (
            e.leftover,
            e.message.as_str(),
            releases(&taken),
            ran(&taken)
        ),
        (None, publish::UNPROVEN, 0, false)
    );
    assert!(unproven(&taken), "taken: its release kept in the index");
    let malformed = ready();
    malformed
        .fakes
        .mux_script("new-session", |_| out(0, "garbage\n"));
    let e = claude(&malformed).unwrap_err();
    assert!(
        e.leftover.is_none()
            && e.message == publish::UNPROVEN
            && releases(&malformed) == 0
            && unproven(&malformed)
            && of(&malformed, "set-option").is_empty()
    );
    let remain = ready();
    fails(&remain, "remain-on-exit");
    assert!(
        claude(&remain).unwrap_err().leftover.is_none() && kept_in_index(&remain) && !ran(&remain)
    );
    let failed = base();
    failed
        .fakes
        .hc_answer(Err(Refusal::internal("hc run failed")));
    assert_eq!(
        (claude(&failed).unwrap_err().leftover, releases(&failed)),
        (None, 0)
    );
    assert!(
        kept_in_index(&failed),
        "failed: published, its release kept"
    );
    let unwritable = base();
    unwritable
        .fakes
        .hc_answer(Err(Refusal::internal("hc run failed")));
    let up_dir = unwritable.ctx.state_root.join("up");
    unwritable.fakes.mux_script("remain-on-exit", move |_| {
        std::fs::remove_dir_all(&up_dir).unwrap();
        std::fs::write(&up_dir, "").unwrap();
        out(0, "")
    });
    let e = claude(&unwritable).unwrap_err();
    assert!(e.leftover.is_none() && e.check == "hc", "{e:?}");
    assert!(
        [&taken, &malformed, &remain, &failed, &unwritable]
            .iter()
            .all(|w| !killed(w))
    );
}

#[test]
fn only_a_person_inside_a_multiplexer_runs_up() {
    assert_eq!(claude(&World::agent("")).unwrap_err().check, "tagged");
    let mut member = World::human("");
    member.ctx.env = crate::ctx::Env::from_pairs([("HC_TERMINAL_ID", "t1")]);
    assert_eq!(claude(&member).unwrap_err().check, "tagged");
    let mut outside = World::human("");
    outside.ctx.env = crate::ctx::Env::from_pairs([("HOME", "/h")]);
    let e = claude(&outside).unwrap_err();
    assert_eq!(
        (e.code, e.check, e.message.as_str()),
        (1, "endpoint", OUTSIDE)
    );
    assert!(outside.fakes.mux_calls().is_empty());
}

#[test]
fn a_restarted_server_stops_up_before_the_next_command() {
    // reentry-6a Task 9: check 1 is `answered` after new-session (call 34: `index`, nothing published, `pending`
    // kept); 2 precedes remain-on-exit and 3 the launch (refusal; the session published, its release kept in the
    // index); 4-5 follow the commit.
    for check in 1..=5 {
        for start_changed in [false, true] {
            let w = ready();
            let (fakes, n) = (w.fakes.clone(), Rc::new(Cell::new(0)));
            w.fakes.mux_script("up #{pid}", move |_| {
                n.set(n.get() + 1);
                let hit = n.get() == check;
                if hit && start_changed {
                    fakes.opens(SERVER, &[Some(2)]);
                }
                out(
                    0,
                    &format!(
                        "up {}\n",
                        if hit && !start_changed {
                            SERVER + 1
                        } else {
                            SERVER
                        }
                    ),
                )
            });
            let got = claude(&w);
            assert!(
                w.fakes
                    .mux_calls()
                    .last()
                    .unwrap()
                    .iter()
                    .any(|t| t == "up #{pid}"),
                "{check} {start_changed}"
            );
            match got {
                Err(e) if check == 1 => assert!(
                    e.check == "index"
                        && !ran(&w)
                        && e.leftover.is_none()
                        && releases(&w) == 0
                        && create::index_of(&w)
                            .is_some_and(|ix| ix.pending.is_some() && ix.view.is_none()),
                    "{check} {start_changed} {e:?}"
                ),
                Err(e) => assert!(
                    check <= 3
                        && e.message == server::RESTARTED
                        && !ran(&w)
                        && e.leftover.is_none()
                        && kept_in_index(&w),
                    "{check} {start_changed} {e:?}"
                ),
                Ok(reply) => assert!(
                    check > 3 && reply["layoutError"] == server::RESTARTED,
                    "{check}"
                ),
            }
        }
    }
}

#[test]
fn the_leader_is_rediscovered_after_each_write_and_a_failed_flush_launches_nothing() {
    let w = ready();
    // reentry-6a Task 9: the index writes through `write_json_durable_with`, outside this counter; the first counted
    // write is still the leader's record. The session is published in the index, its release kept there.
    crate::state::fs::fail_durable_flush(Some(0));
    let got = claude(&w);
    crate::state::fs::fail_durable_flush(None);
    assert!(
        got.is_err() && !ran(&w) && releases(&w) == 0 && kept_in_index(&w),
        "{got:?} ran {} releases {} index {:?}",
        ran(&w),
        releases(&w),
        create::index_of(&w)
    );
    assert!(
        std::fs::read_dir(agents(&w)).is_ok_and(|mut d| d.next().is_some()),
        "the record's write was reached"
    );
    let first = ready();
    let shots: Rc<std::cell::RefCell<Vec<Vec<u8>>>> = Rc::default();
    let shot = |dir: PathBuf, shots: Rc<std::cell::RefCell<Vec<Vec<u8>>>>| {
        move || {
            let folder = std::fs::read_dir(&dir)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            shots
                .borrow_mut()
                .push(std::fs::read(folder.join("record.json")).unwrap());
        }
    };
    let (s1, s2, s3) = (
        shot(agents(&first), shots.clone()),
        shot(agents(&first), shots.clone()),
        shot(agents(&first), shots.clone()),
    );
    first.fakes.hc_probe(move |argv| {
        if argv.first().map(String::as_str) == Some("run") {
            s1()
        }
    });
    first.fakes.mux_script("new-window", move |_| {
        s2();
        out(0, "%9|@5|exact,#} attach t1\n")
    });
    let _reset = crate::agents::core_launch::hooks::Reset;
    crate::agents::core_launch::hooks::after(Some(Box::new(move |step| {
        if step == "promote" {
            s3()
        }
    })));
    let got = claude(&first);
    crate::agents::core_launch::hooks::after(None);
    got.unwrap();
    let launcher = crate::state::fs::read_json(&leader_folder(&first).join("record.json"))
        .unwrap()
        .unwrap()["launcherArgv"]
        .clone();
    for (n, bytes) in shots.borrow().iter().enumerate() {
        std::fs::write(leader_folder(&first).join("record.json"), bytes).unwrap();
        let _ = std::fs::remove_file(leader_folder(&first).join("commit.json"));
        let _ = std::fs::remove_file(team_file(&first));
        // The fake hc runs no launcher: the `run` claim a real launcher publishes after `hc run` is seeded.
        let _ = gens::publish_claim_once(&leader_folder(&first), 0, &json!("run"));
        let mut again = base();
        (again.ctx.state_root, again.ctx.cwd) =
            (first.ctx.state_root.clone(), first.ctx.cwd.clone());
        let id = leader_folder(&first)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        if n == 0 {
            // E6 Settle (settle-1b G3 row 6): the pending generation's terminal is found, then promoted.
            cf::online(&again);
            crate::agents::reconcile_tests::found(
                &again,
                &id,
                0,
                &create::running_wire(&again, &leader_folder(&first), &id, 0),
            );
        }
        if n > 0 {
            cf::get(
                &again,
                "t1",
                json!({"address": "t1", "state": "live", "argv": launcher, "labels": {}}),
            );
        }
        // 6b ambiguity 8: the rediscovered leader's team is reconciled, never made twice. reentry-6a E6 step 4 (E5):
        // the replay writes the missing team record from the same leader, so `created` is true.
        let reply = claude(&again).unwrap();
        assert!(
            reply["created"] == json!(true)
                && reply["leader"]["id"] == json!(id)
                && team_file(&first).is_file()
                && !ran(&again),
            "write {}: {reply:?}",
            n + 1
        );
    }
    assert_eq!(shots.borrow().len(), 3);
}

#[test]
fn up_holds_admission_then_spawn_once_and_two_servers_launch_one_leader() {
    let first = ready();
    claude(&first).unwrap();
    let (locks, t) = (first.fakes.locks(), team_of(&first));
    assert_eq!(
        (locks.first(), locks.last()),
        (Some(&format!("+up|{t}")), Some(&format!("-up|{t}")))
    );
    assert_eq!(locks.iter().filter(|l| l.starts_with("+spawn|")).count(), 1);
    let launcher = crate::state::fs::read_json(&leader_folder(&first).join("record.json"))
        .unwrap()
        .unwrap()["launcherArgv"]
        .clone();
    let mut second = base();
    (second.ctx.state_root, second.ctx.cwd) = (first.ctx.state_root.clone(), first.ctx.cwd.clone());
    second.fakes.opens(SERVER, &[Some(5); 16]); // another server start: another server key, the same root
    assert_ne!(second.caller().server_key, first.caller().server_key);
    cf::get(
        &second,
        "t1",
        json!({"address": "t1", "state": "live", "argv": launcher, "labels": {}}),
    );
    // 6b ambiguity 8: the second server's `up` reconciles the team at its anchor; it makes no second leader.
    assert_eq!(claude(&second).unwrap()["created"], json!(false));
    assert!(!ran(&second) && second.fakes.locks().first() == Some(&format!("+up|{t}")));
}

#[test]
fn a_custom_state_root_holds_the_release_and_the_spec() {
    let mut w = ready();
    w.ctx.state_root = w._dir.path().join("custom root");
    let reply = claude(&w).unwrap();
    assert!(Path::new(of(&w, "new-session")[0].last().unwrap()).starts_with(&w.ctx.state_root));
    let spec = gens::path(
        &agents(&w).join(reply["leader"]["id"].as_str().unwrap()),
        0,
        "spec",
    );
    assert!(
        Path::new(
            crate::state::fs::read_json(&spec).unwrap().unwrap()["stateDirectory"]
                .as_str()
                .unwrap()
        )
        .starts_with(&w.ctx.state_root)
    );
}

#[test]
fn a_relative_state_root_is_refused_before_anything_is_made() {
    // Windows accepts a relative MUX_AGENTS_STATE; `hold` and the launcher take only an absolute root, and every
    // later verb or bar would resolve the same text against its own folder. `up` refuses it, making nothing.
    let mut w = ready();
    let rel = PathBuf::from(format!("mux-up-relative-{}", crate::util::hex32().unwrap()));
    w.ctx.state_root = rel.clone();
    let got = claude(&w);
    let made = rel.exists();
    let _ = std::fs::remove_dir_all(&rel);
    let e = got.unwrap_err();
    assert_eq!(
        (e.code, e.check, e.message.as_str()),
        (1, "state", "the state folder path is not absolute")
    );
    assert!(!made && w.fakes.mux_calls().is_empty() && !ran(&w));
}

#[test]
fn every_folder_above_the_leader_record_is_flushed_before_hc_run() {
    // The server folder, the state root and each folder above it, up to the filesystem root: a failed flush of any of
    // them refuses `up` before `hc run`, so a leader that may have run is always found.
    let depth = agents(&ready()).parent().unwrap().ancestors().count();
    assert!(depth >= 3);
    for n in 0..depth {
        let w = ready();
        let dir = agents(&w)
            .parent()
            .unwrap()
            .ancestors()
            .nth(n)
            .unwrap()
            .to_path_buf();
        // reentry-6a Task 9: the index and the anchor binding flush some of these folders first. Their flushes up to
        // publication are counted in a twin world and let through, so the fault lands on the leader's own flush.
        let twin = ready();
        halt_after(Some("published"));
        let (halted, syncs) = crate::state::fs::record_syncs(|| claude(&twin));
        halt_after(None);
        assert_eq!(halted.unwrap_err().check, "halted");
        let twin_dir = agents(&twin)
            .parent()
            .unwrap()
            .ancestors()
            .nth(n)
            .unwrap()
            .to_path_buf();
        let before = syncs.iter().filter(|d| **d == twin_dir).count();
        crate::state::fs::fail_sync_dir_after(Some(&dir), u32::try_from(before).unwrap());
        let got = claude(&w);
        crate::state::fs::fail_sync_dir(None);
        let e = got.err();
        assert!(
            e.as_ref().is_some_and(|e| e.check == "internal")
                && !ran(&w)
                && releases(&w) == 0
                && kept_in_index(&w),
            "{} {e:?}",
            dir.display()
        );
    }
}

// S6: up publishes its release marker whole, never as an empty file (reentry-6a F8: `release_all` writes
// `{"released": true}`; `up/` also holds the team index).
#[test]
fn up_publishes_a_whole_release_marker() {
    let w = ready();
    claude(&w).unwrap();
    let bodies: Vec<Vec<u8>> = std::fs::read_dir(w.ctx.state_root.join("up"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "release"))
        .map(|p| std::fs::read(p).unwrap())
        .collect();
    assert_eq!(bodies, [br#"{"released":true}"#.to_vec()]);
}

// E1: an unknown client flag refuses usage before anything is made, connected or launched.
#[test]
fn an_unknown_client_flag_refuses_before_anything_is_made() {
    let w = ready();
    let (mux, hc) = (w.fakes.mux_calls().len(), w.fakes.hc_calls().len());
    assert!(
        !w.ctx.state_root.exists(),
        "premise: nothing has made the state root yet"
    );
    let before = crate::testing::tree(&w.ctx.state_root);
    let e = run(
        &w,
        &[
            "up",
            "-Client",
            "claude",
            "--",
            "--frobnicate",
            "--model",
            "x",
        ],
    )
    .unwrap_err();
    assert_eq!((e.code, e.check), (2, "usage"));
    assert_eq!(
        w.fakes.mux_calls().len(),
        mux,
        "no session, no server check"
    );
    assert_eq!(
        w.fakes.hc_calls().len(),
        hc,
        "nothing connected, nothing launched"
    );
    assert_eq!(
        crate::testing::tree(&w.ctx.state_root),
        before,
        "nothing made"
    );
}

// E1/E2: the ceiling is fixed at team creation from the options alone; bypass options give a Bypass team.
#[test]
fn up_fixes_the_team_ceiling_from_its_options() {
    use crate::agents::team::{Policy, permission_flags};
    let w = ready();
    let mut options = permission_flags(Client::Claude, Policy::Bypass);
    options.extend(["--model".to_string(), "x".to_string()]);
    let mut argv: Vec<&str> = vec!["up", "-Client", "claude", "--", "fix"];
    argv.extend(options.iter().map(String::as_str));
    run(&w, &argv).unwrap();
    let team = crate::agents::team::read(&team_file(&w)).unwrap().unwrap();
    assert_eq!(
        (team.flags.clone(), team.policy),
        (options, Policy::Bypass),
        "the stored ceiling"
    );
}

// E1: a session selector given to `up` is never stored and never reaches the client: the recipe alone chooses the
// session, then the prompt follows.
#[test]
fn up_drops_a_session_selector_and_runs_the_recipes_session() {
    let w = ready();
    let argv = [
        "up",
        "-Client",
        "claude",
        "--",
        "fix",
        "--resume",
        "SEL9",
        "--model",
        "x",
        "--session-id=SEL8",
    ];
    let id = run(&w, &argv).unwrap()["leader"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let read = |f: &Path| crate::state::fs::read_json(f).unwrap().unwrap();
    let folder = agents(&w).join(&id);
    let (r, spec) = (
        read(&folder.join("record.json")),
        read(&gens::path(&folder, 0, "spec")),
    );
    assert_eq!(
        (&r["options"], &r["prompt"]),
        (&json!(["--model", "x"]), &json!(["fix"]))
    );
    assert_eq!(spec["flags"], json!(["--model", "x"]));
    assert_eq!(
        spec["args"],
        json!(["--session-id", r["sessionId"], "fix"]),
        "the recipe's session, then the prompt"
    );
    let hc_run = w
        .fakes
        .hc_calls()
        .into_iter()
        .find(|(_, a, _)| a[0] == "run")
        .unwrap()
        .1;
    for seen in [r.to_string(), spec.to_string(), hc_run.join(" ")] {
        assert!(!seen.contains("SEL9") && !seen.contains("SEL8"), "{seen}");
    }
}

// PM calls 1, 10 (finding 5to8-4): the leader view fails after the commit: up succeeds with viewError, writes the
// team record, runs no layout, and keeps the committed leader.
#[test]
fn a_leader_view_failure_is_reported_and_the_leader_stays() {
    let w = ready();
    fails(&w, "@agent-context");
    let reply = claude(&w).unwrap();
    assert!(reply["viewError"].is_string(), "{reply:?}");
    assert!(
        reply.get("layoutError").is_none() && of(&w, "select-window").is_empty(),
        "no layout without a view"
    );
    assert!(team_file(&w).exists(), "the team record is written");
    assert!(
        gens::committed(&leader_folder(&w), 0).unwrap(),
        "the leader is committed"
    );
    assert!(
        !killed(&w) && !w.fakes.hc_calls().iter().any(|(_, a, _)| a[0] == "kill"),
        "nothing rolled back"
    );
    assert!(
        create::released(&w),
        "released once the reply reports the failure"
    );
}

// reentry-6a E4.5 (integration CI, live up_whose_view_panics_keeps_the_leader_under_hostile_settings): the
// launched leader is Running, so a failed leader view opens no status window; that window runs the session's
// default-command, which may start a client this team did not.
#[test]
fn a_running_leader_whose_view_failed_opens_no_status_window() {
    let w = ready();
    fails(&w, "@agent-context");
    let reply = claude(&w).unwrap();
    assert!(reply["viewError"].is_string(), "{reply:?}");
    let status: Vec<_> = of(&w, "new-window")
        .into_iter()
        .filter(|c| c.iter().any(|a| a == "status"))
        .collect();
    assert!(status.is_empty(), "no status window: {status:?}");
}

// cascade-3d C8: `up` finishes the caller's session's journals under `up|<team>` before anything else; an
// unreadable journal refuses `evidence` before any hc run.
#[test]
fn up_finishes_a_kept_cascade_first() {
    let w = ready();
    let scope = crate::agents::team_state::Scope::of(&w.caller());
    let dir = crate::agents::cascade::dir_at(&w.ctx, &scope);
    // Integration (CI): made private as the product makes it, so `up` reaches the journal; a folder left at the
    // umask (755) or with an inherited shared ACL is refused `state` first, as it must be.
    w.state();
    crate::agents::spawn::private_folder(&w.ctx, &dir).unwrap();
    std::fs::write(dir.join("aaaaaaaaaaa1.json"), b"{").unwrap();
    assert_eq!(claude(&w).unwrap_err().check, "evidence");
    assert!(
        w.fakes.hc_calls().iter().all(|c| c.1[0] != "run"),
        "nothing launched"
    );
    // A fresh world: the first `up` consumed `ready()`'s version answer (fix-cycle finding 11).
    let w = ready();
    let scope = crate::agents::team_state::Scope::of(&w.caller());
    let dir = crate::agents::cascade::dir_at(&w.ctx, &scope);
    // Integration (CI): made private as the product makes it, so `up` reaches the journal; a folder left at the
    // umask (755) or with an inherited shared ACL is refused `state` first, as it must be.
    w.state();
    crate::agents::spawn::private_folder(&w.ctx, &dir).unwrap();
    let a = crate::agents::cascade_fixture::A;
    crate::agents::cascade_fixture::member(&w, a, None, 0, json!({}));
    crate::agents::cascade_fixture::done(&w, a);
    crate::agents::cascade_fixture::seed(&w, a, &[a], true);
    let _ = claude(&w);
    assert!(!dir.join("aaaaaaaaaaa1.json").exists(), "finished");
}

/// A server no process row answers: proven gone.
pub(crate) fn dead_server() -> index::Server {
    index::Server {
        socket: "/tmp/dead".into(),
        pid: 7777,
        start: 1,
        key: index::key_of("/tmp/dead", 7777, 1),
    }
}
/// The index `up` leaves for team `t` anchored at `key`/`session`, its view on `view`.
pub(crate) fn seed_index_at(w: &World, t: &str, key: &str, session: &str, view: index::Server) {
    let folder = identity::folder(&w.ctx).unwrap();
    let ix = index::Index {
        team: t.into(),
        inputs: index::Inputs {
            client: "claude".into(),
            options: vec![],
            prompt: vec![],
            name: String::new(),
            folder: folder.to_str().unwrap().into(),
        },
        anchor: Some(crate::caller::Anchor {
            key: key.into(),
            session: session.into(),
        }),
        view: Some(index::View {
            server: view,
            session: session.into(),
            name: "p-11111111-v".into(),
        }),
        pending: None,
        aliases: vec![],
        releases: vec![],
    };
    index::write(&w.ctx, &ix).unwrap();
}
/// The team record of `t` at `key`/`session`, leader `leader`.
pub(crate) fn team_record_at(w: &World, t: &str, key: &str, session: &str, leader: &str) {
    let made = crate::agents::team::Team {
        team: t.into(),
        session: session.into(),
        name: "p-11111111-v".into(),
        directory: "/p".into(),
        client: Client::Claude,
        flags: vec![],
        policy: crate::agents::team::Policy::Default,
        leader: leader.into(),
        created_at: "2026-11-16T00:00:00.0000000Z".into(),
    };
    let at = crate::agents::team::path(&w.ctx.state_root.join(key), session);
    let _ = std::fs::remove_file(&at); // restore-6c: `member_at` may have written its own record first
    assert!(crate::agents::team::create(&at, &made).unwrap());
}
/// A leader record of this folder's team at the World key in `session`: void (an abandoned claim, no active
/// generation) or active (generation 0 committed).
fn leader_at(w: &World, id: &str, session: &str, void: bool) -> PathBuf {
    let f = agents(w).join(id);
    std::fs::create_dir_all(&f).unwrap();
    let mut r = json!({"lifecycle": 1, "id": id, "name": "leader", "upTeam": team_of(w), "depth": 0,
        "session": session});
    if void {
        r["pending"] = crate::agents::life_fixture::pending(0);
    } else {
        r["generation"] = json!("0");
    }
    crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
    let (claim, commit) = if void {
        (Some("abandoned"), None)
    } else {
        (Some("run"), crate::agents::life_fixture::committed())
    };
    crate::agents::life_fixture::seed_gen(&f, 0, claim, commit);
    f
}

// Ambiguity 9, call 7: the leader is found at the anchor in its session; a void one is skipped and its folder kept;
// one in another session (a retired predecessor) is invisible; two non-void: `index`.
#[test]
fn the_leader_is_found_at_the_anchor_and_a_void_one_is_skipped() {
    let w = ready();
    let (t, key) = (team_of(&w), w.caller().server_key);
    seed_index_at(&w, &t, &key, "$7", dead_server());
    let ix = index::read(&w.ctx, &t).unwrap().unwrap();
    assert_eq!(
        leader_of(&w.ctx, &ix).unwrap(),
        (replay::Leader::Absent, None)
    );
    let void = leader_at(&w, "aaaaaaaaaaa1", "$7", true);
    assert_eq!(
        leader_of(&w.ctx, &ix).unwrap().0,
        replay::Leader::Absent,
        "void: no leader"
    );
    assert!(void.is_dir(), "its folder stays");
    leader_at(&w, "aaaaaaaaaaa4", "$9", false);
    assert_eq!(
        leader_of(&w.ctx, &ix).unwrap().0,
        replay::Leader::Absent,
        "call 7: another session is not this team"
    );
    leader_at(&w, "aaaaaaaaaaa2", "$7", false);
    assert_eq!(
        leader_of(&w.ctx, &ix).unwrap(),
        (replay::Leader::Active, Some("aaaaaaaaaaa2".into()))
    );
    // Call 36 (finding A17): a copied record naming another id under this folder is not this folder's leader.
    let stray = leader_at(&w, "aaaaaaaaaaa5", "$7", false);
    let mut r: Value =
        serde_json::from_slice(&std::fs::read(stray.join("record.json")).unwrap()).unwrap();
    r["id"] = json!("bbbbbbbbbbb2");
    crate::state::fs::write_json(&stray.join("record.json"), &r).unwrap();
    assert_eq!(
        leader_of(&w.ctx, &ix).unwrap(),
        (replay::Leader::Active, Some("aaaaaaaaaaa2".into())),
        "a record of another id"
    );
    leader_at(&w, "aaaaaaaaaaa3", "$7", false);
    assert_eq!(
        leader_of(&w.ctx, &ix).unwrap_err().message,
        "two leaders claim this team"
    );
}

// F13 (checklist 25): the leader's record is revalidated at the reread: its id, `upTeam` and the anchor's session.
#[test]
fn the_leader_record_is_revalidated_at_the_reread() {
    let w = ready();
    let (t, key) = (team_of(&w), w.caller().server_key);
    seed_index_at(&w, &t, &key, "$7", dead_server());
    let ix = index::read(&w.ctx, &t).unwrap().unwrap();
    let f = leader_at(&w, "aaaaaaaaaaa2", "$7", false);
    assert_eq!(
        leaders::leader_record(&w.ctx, &ix, "aaaaaaaaaaa2").unwrap()["id"],
        json!("aaaaaaaaaaa2")
    );
    for (field, value) in [
        ("id", json!("bbbbbbbbbbb2")),
        ("upTeam", json!("ffffffffffffffff")),
        ("session", json!("$9")),
    ] {
        let mut r = crate::agents::life_fixture::record(&f);
        let kept = r[field].clone();
        r[field] = value;
        crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
        assert_eq!(
            leaders::leader_record(&w.ctx, &ix, "aaaaaaaaaaa2")
                .unwrap_err()
                .check,
            "team",
            "{field}"
        );
        r[field] = kept;
        crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
    }
}

// Call 21: `{}` or another team's valid record at the anchor's team path is no team record of this team: `index`.
#[test]
fn a_team_record_must_be_this_teams() {
    let w = base(); // nothing launches: no launch answer queued
    let (t, key) = (team_of(&w), w.caller().server_key);
    seed_index_at(&w, &t, &key, "$7", dead_server());
    let at = crate::agents::team::path(&w.ctx.state_root.join(&key), "$7");
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::fs::write(&at, b"{}").unwrap();
    assert_eq!(claude(&w).unwrap_err().check, "index", "{{}}");
    std::fs::remove_file(&at).unwrap();
    team_record_at(&w, "ffffffffffffffff", &key, "$7", "cccccccccccc");
    cf::online(&w); // the second up's own hc::connect
    let e = claude(&w).unwrap_err();
    assert_eq!(e.check, "index", "another team's record: {}", e.message);
    assert!(of(&w, "new-session").is_empty() && !ran(&w), "nothing made");
    w.fakes.hc_done().unwrap();
}

thread_local! {
    static OPENED: Cell<bool> = const { Cell::new(false) };
}
fn mark_opened(_: &crate::agents::team_state::Tx<'_>) -> Res<()> {
    OPENED.with(|o| o.set(true));
    Ok(())
}
fn refuse_open(_: &crate::agents::team_state::Tx<'_>) -> Res<()> {
    refuse(1, "stopping", "a cascade is unresolved")
}

// Call 8, F11 (checklist 23): re-entry reconciles (opening the anchor's team Tx) before any session is made; a
// reconcile that refuses opens no session. No launch answer is queued; every hc answer is consumed.
#[test]
fn reconcile_runs_before_any_session_is_made() {
    for refused in [true, false] {
        let w = base();
        let (t, key) = (team_of(&w), w.caller().server_key);
        let f = crate::agents::reconcile_tests::member_at(
            &w,
            &key,
            "cccccccccccc",
            json!({"name": "leader", "depth": 0, "upTeam": t, "session": "$A"}),
        );
        crate::agents::reconcile_tests::ok(&f, 1); // member_at seeds generation 1 (finding A14)
        seed_index_at(&w, &t, &key, "$A", dead_server());
        team_record_at(&w, &t, &key, "$A", "cccccccccccc");
        OPENED.with(|o| o.set(false));
        crate::agents::team_state::test_hook(Some(if refused { refuse_open } else { mark_opened }));
        w.fakes.mux_script("new-session", |_| {
            assert!(
                OPENED.with(|o| o.get()),
                "reconcile opened the team before the session was made"
            );
            out(0, "$8 @4\n")
        });
        if !refused {
            for _ in 0..2 {
                cf::online(&w); // reconcile's hc::Lazy, then re-entry's (Task 11)
                crate::agents::reconcile_tests::found(
                    &w,
                    "cccccccccccc",
                    1,
                    &create::running_wire(&w, &f, "cccccccccccc", 1),
                );
            }
        }
        let got = claude(&w);
        crate::agents::team_state::test_hook(None);
        if refused {
            assert_eq!(got.unwrap_err().check, "stopping");
            assert!(
                of(&w, "new-session").is_empty(),
                "a refused team opens no session"
            );
        } else {
            let got = got.unwrap();
            assert_eq!(of(&w, "new-session").len(), 1, "{got:?}");
            assert!(got["restored"].is_array(), "the reconcile result: {got:?}");
            assert_eq!(got["leader"]["id"], json!("cccccccccccc"));
        }
        assert!(!ran(&w), "nothing launched");
        w.fakes.hc_done().unwrap();
    }
}

#[cfg(test)]
pub(crate) mod create;
