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
fn base() -> World {
    let mut w = World::human("");
    w.ctx.cwd = w._dir.path().join("project.v1");
    std::fs::create_dir_all(&w.ctx.cwd).unwrap();
    cf::online(&w);
    w.fakes.mux_rule("new-session", 0, "$7 @3\n", "");
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
fn ready() -> World {
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
fn team_of(w: &World) -> String {
    identity::team(
        &w.ctx,
        &identity::folder(&w.ctx).unwrap(),
        Client::Claude,
        "",
    )
    .unwrap()
}
fn releases(w: &World) -> usize {
    std::fs::read_dir(w.ctx.state_root.join("up")).map_or(0, |d| d.count())
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
fn team_file(w: &World) -> PathBuf {
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
    let want = json!({"team": t, "session": "$7", "name": name, "leader": {"id": id, "name": "leader", "hcAddress": "t1"}, "created": true, "sidebar": null});
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
    crate::agents::state_dir(&w.ctx, &w.caller()).unwrap(); // private, as `up` makes it
    std::fs::create_dir_all(team_file(&w).parent().unwrap()).unwrap();
    std::fs::write(team_file(&w), "{}").unwrap();
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
    assert!(
        reply["layoutError"].is_string()
            && reply.get("leftoverSession").is_none()
            && of(&release, "select-window").is_empty()
    );
}

#[test]
fn cleanup_kills_nothing_and_names_a_leftover_session_until_step_2() {
    let taken = ready();
    fails(&taken, "new-session");
    assert_eq!(
        (
            claude(&taken).unwrap_err().leftover,
            releases(&taken),
            ran(&taken)
        ),
        (None, 0, false)
    );
    let malformed = ready();
    malformed
        .fakes
        .mux_script("new-session", |_| out(0, "garbage\n"));
    let e = claude(&malformed).unwrap_err();
    assert!(
        e.leftover.unwrap().starts_with("project_v1-")
            && releases(&malformed) == 1
            && of(&malformed, "set-option").is_empty()
    );
    let remain = ready();
    fails(&remain, "remain-on-exit");
    assert!(
        claude(&remain).unwrap_err().leftover.is_some() && releases(&remain) == 1 && !ran(&remain)
    );
    let failed = base();
    failed
        .fakes
        .hc_answer(Err(Refusal::internal("hc run failed")));
    assert_eq!(
        (claude(&failed).unwrap_err().leftover, releases(&failed)),
        (None, 1)
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
    assert!(claude(&unwritable).unwrap_err().leftover.is_some());
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
    // Check 1 precedes step 2 (refusal names the session); 2 precedes the launch (refusal, released); 3-4 follow the
    // commit.
    for check in 1..=4 {
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
                Err(e) => assert!(
                    check <= 2
                        && e.message == server::RESTARTED
                        && !ran(&w)
                        && e.leftover.is_some() == (check == 1)
                        && releases(&w) == 1
                ),
                Ok(reply) => assert!(
                    check > 2 && reply["layoutError"] == server::RESTARTED,
                    "{check}"
                ),
            }
        }
    }
}

#[test]
fn the_leader_is_rediscovered_after_each_write_and_a_failed_flush_launches_nothing() {
    let w = ready();
    crate::state::fs::fail_durable_flush(Some(0));
    let got = claude(&w);
    crate::state::fs::fail_durable_flush(None);
    assert!(got.is_err() && !ran(&w) && releases(&w) == 1);
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
        let mut again = base();
        (again.ctx.state_root, again.ctx.cwd) =
            (first.ctx.state_root.clone(), first.ctx.cwd.clone());
        if n > 0 {
            cf::get(
                &again,
                "t1",
                json!({"address": "t1", "state": "live", "argv": launcher, "labels": {}}),
            );
        }
        let e = claude(&again).unwrap_err();
        assert!(
            e.check == "leader"
                && e.message
                    .starts_with("the team already has a leader: leader in $7")
                && !ran(&again),
            "write {}",
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
    assert_eq!(claude(&second).unwrap_err().check, "leader");
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
    // them refuses `up` before `hc run`, so a leader that may have run is always found (#103).
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
        crate::state::fs::fail_sync_dir(Some(&dir));
        let got = claude(&w);
        crate::state::fs::fail_sync_dir(None);
        let e = got.err();
        assert!(
            e.as_ref().is_some_and(|e| e.check == "internal") && !ran(&w) && releases(&w) == 1,
            "{} {e:?}",
            dir.display()
        );
    }
}

// S6: up publishes its release marker whole, as {}, never as an empty file.
#[test]
fn up_publishes_a_whole_release_marker() {
    let w = ready();
    claude(&w).unwrap();
    let bodies: Vec<Vec<u8>> = std::fs::read_dir(w.ctx.state_root.join("up"))
        .unwrap()
        .map(|e| std::fs::read(e.unwrap().path()).unwrap())
        .collect();
    assert_eq!(bodies, [b"{}".to_vec()]);
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
}

// cascade-3d C8: `up` finishes the caller's session's journals under `up|<team>` before anything else; an
// unreadable journal refuses `evidence` before any hc run.
#[test]
fn up_finishes_a_kept_cascade_first() {
    let w = ready();
    let scope = crate::agents::team_state::Scope::of(&w.caller());
    let dir = crate::agents::cascade::dir_at(&w.ctx, &scope);
    // Integration (#150 CI): made private as the product makes it, so `up` reaches the journal; a folder left at the
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
    // Integration (#150 CI): made private as the product makes it, so `up` reaches the journal; a folder left at the
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
