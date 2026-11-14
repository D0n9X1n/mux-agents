use super::*;
use crate::agents::{
    core_fixture as cf,
    fixture::{SERVER, World},
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
    let flags = ["--model", "a b", "-Text", "\"q\""];
    let reply = run(
        &w,
        &[&["up", "-Client", "claude", "--"][..], &flags].concat(),
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
    assert_eq!(
        crate::state::fs::read_json(&agents(&w).join(&id).join("spec.json"))
            .unwrap()
            .unwrap()["flags"],
        json!(flags)
    );
    let team = crate::agents::team::read(&team_file(&w)).unwrap().unwrap();
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
        assert!(
            fakes
                .mux_calls()
                .iter()
                .any(|c| c.iter().any(|t| t == "remain-on-exit"))
        );
        s.set(s.get() + 1);
    });
    let s = seen.clone();
    crate::agents::spawn::commit_probe(Some(Box::new(move |folder| {
        assert!(!folder.join("commit.json").exists() && !team.exists());
        s.set(s.get() + 1);
    })));
    let reply = claude(&w);
    crate::agents::spawn::commit_probe(None);
    reply.unwrap();
    assert!(
        seen.get() == 2 && team_file(&w).exists() && leader_folder(&w).join("commit.json").exists()
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
    crate::agents::spawn::commit_probe(Some(Box::new(move |_| {
        std::fs::remove_dir_all(&up_dir).unwrap();
        std::fs::write(&up_dir, "").unwrap();
    })));
    let got = claude(&release);
    crate::agents::spawn::commit_probe(None);
    let reply = got.unwrap();
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
    crate::agents::spawn::commit_probe(Some(Box::new(move |_| s3())));
    let got = claude(&first);
    crate::agents::spawn::commit_probe(None);
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
    let spec = agents(&w)
        .join(reply["leader"]["id"].as_str().unwrap())
        .join("spec.json");
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
