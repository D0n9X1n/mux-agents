use super::*;
use crate::agents::{core_launch, team_state};
use crate::{
    agents::{
        core_fixture,
        fixture::{SESSION_ID, World},
        gens,
    },
    spawn::Out,
    state::fs::read_json,
    testing::Fakes,
};
use std::{cell::RefCell, rc::Rc};
fn answer(f: &Fakes, body: &str) {
    f.hc_answer(Ok(Out {
        stdout: body.into(),
        ..Out::default()
    }));
}
fn request(w: &World) -> CoreRequest<'_> {
    CoreRequest {
        client: Client::Claude,
        task: "task",
        brief: None,
        directory: &w.ctx.cwd,
        model: None,
        no_fork: true,
        effort: None,
        split: None,
        resume: None,
        leader: None,
    }
}
fn slot(_: &crate::hc::Core<'_>) -> Res<CoreSlot> {
    Ok(CoreSlot {
        name: "worker".into(),
        color: "blue",
        placed: None,
        debit: false,
    })
}
fn folder(w: &World) -> PathBuf {
    std::fs::read_dir(super::super::server_dir(&w.ctx, &w.caller()).join("agents"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
}
fn ready(w: &World) {
    w.fakes.hc_install("herdr-core");
    core_fixture::answer(w, &["--version"], "hc 0.1.21");
    answer(&w.fakes, r#"{"address":"t1"}"#);
    for verb in ["new-window", "split-window"] {
        w.fakes.mux_rule(verb, 0, "%9|@5|exact,#} attach t1\n", "");
    }
    w.launches_hold();
    w.fakes
        .mux_rule("-t %9 #{pane_id}|#{@agent-id}", 0, "|\n", "");
}
fn forked() -> (World, Caller, PathBuf) {
    let mut w = World::human("");
    let mut caller = w.caller();
    w.ctx.home = w.ctx.cwd.join("home");
    let source = copilot::session_folder(&w.ctx.home, SESSION_ID);
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("workspace.yaml"), format!("id: {SESSION_ID}\n")).unwrap();
    std::fs::write(
        source.join("events.jsonl"),
        format!("{{\"sessionId\":\"{SESSION_ID}\"}}\n"),
    )
    .unwrap();
    w.ctx.env = crate::ctx::Env::from_pairs(
        w.ctx
            .env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .chain([("COPILOT_AGENT_SESSION_ID".into(), SESSION_ID.into())]),
    );
    caller.client = Some(Client::Copilot);
    caller.kind = Kind::Agent;
    (w, caller, source)
}
#[test]
fn hc_unavailable_has_no_side_effects() {
    for version in [None, Some("hc 0.1.20")] {
        let (w, caller, source) = forked();
        let reaped = w.file("notice.json", b"untouched");
        let copied = copilot::session_folder(&w.ctx.home, "11111111-2222-4333-8444-555555555555");
        let mut req = request(&w);
        req.client = Client::Copilot;
        req.no_fork = false;
        req.brief = Some("brief");
        if let Some(v) = version {
            w.fakes.hc_install("hc");
            core_fixture::answer(&w, &["--version"], v);
        }
        w.fakes.clear_log();
        let e = launch_hc(&w.ctx, &caller, req, |core| {
            state_dir(&w.ctx, &caller)?;
            std::fs::write(&reaped, "reaped")?;
            std::fs::create_dir_all(&copied)?;
            slot(core)
        })
        .unwrap_err();
        assert_eq!(e.check, "hc");
        assert!(!w.ctx.state_root.exists());
        assert!(
            !super::super::server_dir(&w.ctx, &caller)
                .join("server.json")
                .exists()
        );
        assert!(!copied.exists());
        assert_eq!(
            std::fs::read_dir(source.parent().unwrap()).unwrap().count(),
            1
        );
        assert_eq!(std::fs::read(reaped).unwrap(), b"untouched");
        assert!(w.fakes.locks().is_empty());
        assert!(w.fakes.mux_calls().is_empty());
        w.fakes.hc_done().unwrap();
    }
}
#[test]
fn hc_launch_transaction() {
    for split in [None, Some("h"), Some("v")] {
        let w = World::human("");
        let caller = w.caller();
        ready(&w);
        let mut req = request(&w);
        req.split = split;
        let r = launch_hc(&w.ctx, &caller, req, |core| {
            assert_eq!(w.fakes.hc_calls().len(), 1);
            assert_eq!(
                w.fakes.locks(),
                [format!("+{}", lock_key("spawn", &caller, &caller.session))]
            );
            slot(core)
        })
        .unwrap();
        let folder = folder(&w);
        let argv = crate::launcher_scope::argv(&w.ctx, &gens::path(&folder, 0, "spec")).unwrap();
        let id = r["id"].as_str().unwrap();
        let team = format!("{}:{}", caller.server_key, caller.session);
        assert_eq!(r["generation"], "0");
        assert_eq!(r["launcherArgv"], json!(argv));
        assert!(super::super::core_identity::Identity::read(&r, id, &team).is_some());
        // views-6f ambiguity 2: the presented record also carries view.json's key and program (VIEW_KEY).
        let mut presented = r.clone();
        let view = read_json(&folder.join("view.json")).unwrap().unwrap();
        for (from, to) in super::super::team_state::VIEW_KEY {
            presented[to] = view[from].clone();
        }
        assert_eq!(
            super::super::membership::locals(&w.ctx, &caller).unwrap()[0]
                .stored
                .record,
            presented
        );
        assert_eq!(
            read_json(&gens::path(&folder, 0, "spec")).unwrap().unwrap()["generation"],
            "0"
        );
        let mut expected = vec![
            "run".into(),
            "--bg".into(),
            "--keep".into(),
            "--outlive-parent".into(),
            "--no-notify".into(),
            "--name".into(),
            r["hcName"].as_str().unwrap().into(),
        ];
        for label in [
            "mux.managed=1".into(),
            format!("mux.team={team}"),
            format!("mux.id={id}"),
            "mux.gen=0".into(),
            "mux.name=worker".into(),
        ] {
            expected.extend(["--label".into(), label]);
        }
        expected.extend([
            "--cwd".into(),
            utf8(&w.ctx.cwd).unwrap().into(),
            "--json".into(),
            "--".into(),
        ]);
        expected.extend(argv);
        assert!(super::super::core_identity::is_hc_name(&expected[6]));
        let hc = w.fakes.hc_calls();
        assert_eq!(hc.len(), 2);
        assert_eq!(hc[1].1, expected);
        let calls = w.fakes.mux_calls();
        let create = calls
            .iter()
            .find(|c| {
                c[1] == if split.is_some() {
                    "split-window"
                } else {
                    "new-window"
                }
            })
            .unwrap();
        assert_eq!(
            &create[create.len() - 3..],
            [
                utf8(&w.fakes.hc_install("herdr-core")).unwrap(),
                "attach",
                "t1"
            ]
        );
        let launch = registry::Launch {
            command: r["startCommand"].as_str().unwrap().into(),
            pid: None,
        };
        assert!(calls.iter().any(|c| c
            == &vec![
                "-N",
                "display-message",
                "-p",
                "-t",
                "%9",
                &format!("#{{pane_id}}|{}", launch.launched())
            ]));
        assert!(gens::committed(&folder, 0).unwrap());
        w.fakes.hc_done().unwrap();
    }
}
#[test]
fn commit_then_view_order() {
    let w = World::human("");
    let caller = w.caller();
    ready(&w);
    let state = super::super::server_dir(&w.ctx, &caller);
    let log = Rc::new(RefCell::new(Vec::new()));
    let seen = log.clone();
    w.fakes.mux_script("new-window", move |_| {
        let folder = std::fs::read_dir(state.join("agents"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let r = read_json(&folder.join("record.json"))?.unwrap();
        assert_eq!(
            (&r["hcAddress"], &r["generation"]),
            (&json!("t1"), &json!("0"))
        );
        assert!(r.get("pending").is_none() && r.get("pane").is_none());
        assert!(
            gens::committed(&folder, 0).unwrap(),
            "the view follows the commit"
        );
        assert!(!folder.join("view.json").exists());
        seen.borrow_mut().push("attach");
        Ok(crate::mux::Output {
            stdout: "%9|@5|exact,#} attach t1\n".into(),
            ..Default::default()
        })
    });
    let r = launch_hc(&w.ctx, &caller, request(&w), slot).unwrap();
    assert_eq!(*log.borrow(), ["attach"]);
    let view = read_json(&of(&w, &r).join("view.json")).unwrap().unwrap();
    assert_eq!((&view["pane"], &view["gen"]), (&json!("%9"), &json!(0)));
    w.fakes.hc_done().unwrap();
}
#[test]
fn hc_launch_fault_matrix() {
    for stage in [
        "record",
        "mux",
        "malformed",
        "empty",
        "tag",
        "proof",
        "view-record",
        "commit-fs",
        "launcher-timeout",
        "timeout",
        "panic",
        "title",
        "title-panic",
        "title-option",
        "run-timeout",
        "run-malformed",
        "spec",
    ] {
        let (w, caller, source) = forked();
        let mut req = request(&w);
        req.client = Client::Copilot;
        req.no_fork = false;
        req.brief = Some("brief");
        core_fixture::online(&w);
        if stage == "run-timeout" {
            w.fakes.hc_answer(Err(Refusal::internal("timeout")));
        } else if stage != "spec" {
            answer(
                &w.fakes,
                if stage == "run-malformed" {
                    "bad"
                } else {
                    r#"{"address":"t1"}"#
                },
            );
        }
        w.fakes
            .mux_rule("new-window", 0, "%9|@5|exact,#} attach t1\n", "");
        w.launches_hold();
        w.fakes
            .mux_rule("-t %9 #{pane_id}|#{@agent-id}", 0, "|\n", "");
        let state = super::super::server_dir(&w.ctx, &caller);
        let first = move || {
            std::fs::read_dir(state.join("agents"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path()
        };
        let _reset = core_launch::hooks::Reset;
        if stage == "spec" {
            let first = first.clone();
            core_launch::hooks::after(Some(Box::new(move |step| {
                if step == "pending" {
                    crate::state::fs::fail_sync_file(Some(&gens::path(&first(), 0, "spec")));
                }
            })));
        }
        if stage == "record" {
            // The second durable write is the address patch: its folder flush fails after `hc run`.
            crate::state::fs::fail_durable_flush(Some(1));
        }
        w.fakes.hc_probe(move |a| {
            if a[0] != "run" {
                return;
            }
            let i = a.iter().position(|x| x == "launcher").unwrap();
            let commit = Path::new(&a[i + 1]).with_file_name("commit.json");
            match stage {
                "commit-fs" => std::fs::create_dir(&commit).unwrap(),
                "launcher-timeout" => write_json(&commit, &json!({"aborted": true})).unwrap(),
                _ => {}
            }
        });
        match stage {
            "mux" => w.fakes.mux_first("new-window", 1, "", "bad"),
            "malformed" => w.fakes.mux_first("new-window", 0, "%9|@5\n", ""),
            "empty" => w.fakes.mux_first("new-window", 0, "%9|@5|\n", ""),
            "tag" => w.fakes.mux_first("@agent-context", 1, "", "bad"),
            "proof" => w
                .fakes
                .mux_first("}|#{==:#{pane_start_command},", 0, "%9|0\n", ""),
            "title" => w.fakes.mux_first("select-pane -T", 1, "", "bad"),
            "title-option" => w.fakes.mux_first("allow-set-title", 1, "", "bad"),
            "title-panic" => w.fakes.mux_script("select-pane -T", |_| panic!("title")),
            "panic" => w.fakes.mux_script("@agent-context", |_| panic!("tag")),
            "timeout" => w
                .fakes
                .mux_script("new-window", |_| Err(Refusal::internal("timeout"))),
            "view-record" => {
                let first = first.clone();
                w.fakes.mux_script("@agent-context", move |_| {
                    std::fs::create_dir(first().join("view.json"))?;
                    Ok(crate::mux::Output::default())
                })
            }
            _ => {}
        }
        let got = launch_hc(&w.ctx, &caller, req, slot);
        crate::state::fs::fail_durable_flush(None);
        crate::state::fs::fail_sync_file(None);
        let f = folder(&w);
        let decided = read_json(&gens::path(&f, 0, "commit"))
            .ok()
            .flatten()
            .map(|c| gens::decide(&c));
        assert!(
            !w.fakes.hc_calls().iter().any(|(_, a, _)| a[0] == "kill"),
            "{stage}: nothing is killed"
        );
        let runs = w
            .fakes
            .hc_calls()
            .iter()
            .filter(|(_, a, _)| a[0] == "run")
            .count();
        assert_eq!(runs, usize::from(stage != "spec"), "{stage}");
        match stage {
            "spec" | "record" => {
                let e = got.unwrap_err();
                let phrase = if stage == "spec" {
                    gens::NOT_PUBLISHED
                } else {
                    team_state::NOT_WRITTEN
                };
                assert_eq!(e.check, "state", "{stage}");
                // abort may add a suffix
                assert!(e.message.starts_with(phrase), "{stage}: {}", e.message);
                assert_eq!(decided, Some(gens::Decision::Aborted), "{stage}: fenced");
            }
            "run-timeout" | "run-malformed" => {
                assert!(got.is_err(), "{stage}");
                assert_eq!(decided, Some(gens::Decision::Aborted), "{stage}: fenced");
            }
            "launcher-timeout" => {
                assert_eq!(got.map_err(|e| e.check).unwrap_err(), "launch", "{stage}");
                assert_eq!(decided, Some(gens::Decision::Aborted), "{stage}");
            }
            "commit-fs" => {
                assert_eq!(got.map_err(|e| e.check).unwrap_err(), "evidence", "{stage}");
                let r = read_json(&f.join("record.json")).unwrap().unwrap();
                assert_eq!(
                    r["pending"]["hcAddress"],
                    json!("t1"),
                    "{stage}: pending kept for settle-1b"
                );
            }
            "title" | "title-panic" | "title-option" => {
                assert!(got.unwrap().get("viewError").is_none(), "{stage}");
                assert_eq!(decided, Some(gens::Decision::Run), "{stage}");
            }
            _ => {
                assert!(
                    got.unwrap()["viewError"].is_string(),
                    "{stage}: the member stays"
                );
                assert_eq!(decided, Some(gens::Decision::Run), "{stage}");
            }
        }
        let mux = w.fakes.mux_calls();
        let clean = mux
            .iter()
            .filter(|a| a.last().is_some_and(|s| s == "kill-pane -t %9"))
            .collect::<Vec<_>>();
        let cleaned = ["tag", "proof", "view-record", "panic"].contains(&stage);
        assert_eq!(
            clean.len(),
            usize::from(cleaned),
            "{stage}: only a proven pane is cleaned"
        );
        if cleaned {
            assert_eq!(
                clean[0][5],
                registry::Launch {
                    command: "exact,#} attach t1".into(),
                    pid: None
                }
                .launched()
            );
        }
        assert_eq!(
            gens::path(&f, 0, "spec").is_file(),
            stage != "spec",
            "{stage}"
        );
        assert_eq!(
            std::fs::read_dir(source.parent().unwrap()).unwrap().count(),
            2,
            "copy retained: {stage}"
        );
        w.fakes.hc_done().unwrap();
    }
}
#[test]
fn a_placed_child_records_its_place_and_a_failed_launch_keeps_its_debit() {
    let w = World::human("");
    let (caller, dir) = (w.caller(), w.ctx.cwd.join("member"));
    std::fs::create_dir(&dir).unwrap();
    let d = utf8(&dir).unwrap().to_string();
    let placed = crate::agents::tree::Placed {
        depth: 2,
        parent: json!({"id": "aaaaaaaaaaaa"}),
        parent_pane: "%4".into(),
        flags: vec!["--effort".into(), "low".into()],
        directory: Some(d.clone()),
        name: "worker".into(),
    };
    ready(&w);
    let slot = |p| {
        Ok(CoreSlot {
            name: "worker".into(),
            color: "blue",
            placed: p,
            debit: false,
        })
    };
    let r = launch_hc(&w.ctx, &caller, request(&w), |_| slot(Some(placed.clone()))).unwrap();
    assert_eq!(
        (
            &r["depth"],
            &r["parentAgent"]["id"],
            &r["parentPane"],
            &r["directory"]
        ),
        (&json!(2), &json!("aaaaaaaaaaaa"), &json!("%4"), &json!(d))
    );
    let spec = read_json(&gens::path(&folder(&w), 0, "spec"))
        .unwrap()
        .unwrap();
    assert_eq!(
        (&spec["flags"], &spec["parentPane"]),
        (&json!(["--effort", "low"]), &json!("%4"))
    );
    assert!(
        w.fakes.hc_calls()[1]
            .1
            .windows(2)
            .any(|a| a[0] == "--cwd" && a[1] == d),
        "the member's directory"
    );
    let file = w.state().join("teams").join("b.json");
    let debit = |_: &crate::hc::Core<'_>| {
        crate::agents::budget::spend(&file, &crate::agents::budget::DEFAULT, 0)?;
        slot(None)
    };
    w.fakes.clear_log();
    core_fixture::online(&w);
    crate::state::fs::fail_durable_flush(Some(0));
    let flushed = launch_hc(&w.ctx, &caller, request(&w), debit);
    crate::state::fs::fail_durable_flush(None);
    assert!(
        flushed.is_err() && w.fakes.hc_calls().len() == 1,
        "a failed flush launches nothing"
    );
    let _ = std::fs::remove_file(&file);
    core_fixture::online(&w);
    w.fakes.hc_answer(Err(Refusal::internal("timeout")));
    assert!(launch_hc(&w.ctx, &caller, request(&w), debit).is_err());
    assert_eq!(
        read_json(&file).unwrap().unwrap()["tokens"],
        19,
        "a failed launch keeps the debit"
    );
}
/// The member folder named by a launch reply.
fn of(w: &World, r: &Value) -> PathBuf {
    w.state().join("agents").join(r["id"].as_str().unwrap())
}

// S2, E1, V1: a spawned member is a lifecycle record with options and prompt; generation 0 files; a view.json
// that names its generation; the launcher accepts it.
#[test]
fn a_spawned_record_is_lifecycle_with_options_and_prompt() {
    let w = World::human("");
    let caller = w.caller();
    ready(&w);
    let r = launch_hc(&w.ctx, &caller, request(&w), slot).unwrap();
    let f = of(&w, &r);
    // Row 8.2 (finding 5to8-12): the reply first, so a refused view.json write fails here, not at the read.
    assert_eq!((&r["pane"], r.get("viewError")), (&json!("%9"), None));
    let on_disk = read_json(&f.join("record.json")).unwrap().unwrap();
    assert_eq!(
        (&on_disk["lifecycle"], &on_disk["generation"]),
        (&json!(1), &json!("0"))
    );
    assert_eq!(on_disk["prompt"], json!(["task"]));
    assert!(on_disk["options"].is_array() && on_disk.get("flags").is_none());
    for k in ["pane", "window", "startCommand", "pending"] {
        assert!(on_disk.get(k).is_none(), "{k} is not in record.json");
    }
    assert!(crate::agents::core_identity::is_hc_name(
        on_disk["hcName"].as_str().unwrap()
    ));
    let view = read_json(&f.join("view.json")).unwrap().unwrap();
    let got = (
        &view["pane"],
        &view["gen"],
        &view["hcAddress"],
        &view["hcName"],
    );
    assert_eq!(
        got,
        (
            &json!("%9"),
            &json!(0),
            &on_disk["hcAddress"],
            &on_disk["hcName"]
        )
    );
    assert!(gens::committed(&f, 0).unwrap());
    assert!(
        !f.join("spec.json").exists() && !f.join("commit.json").exists(),
        "no folder-level launch files"
    );
    assert!(
        crate::launcher::own_spec(&f, 0).is_some(),
        "the launcher accepts it"
    );
}

// E1 (finding af434f, model): the selected model is a stored option, so a resume replays it.
#[test]
fn a_selected_model_is_stored_and_replayed() {
    let w = World::human("");
    let caller = w.caller();
    ready(&w);
    let mut req = request(&w);
    req.model = Some("x");
    let r = launch_hc(&w.ctx, &caller, req, slot).unwrap();
    let on_disk = read_json(&of(&w, &r).join("record.json")).unwrap().unwrap();
    let options: Vec<String> = serde_json::from_value(on_disk["options"].clone()).unwrap();
    assert!(
        options.ends_with(&["--model".to_string(), "x".to_string()]),
        "{options:?}"
    );
    let spec = read_json(&gens::path(&of(&w, &r), 0, "spec"))
        .unwrap()
        .unwrap();
    let args: Vec<String> = serde_json::from_value(spec["args"].clone()).unwrap();
    assert!(
        args.windows(2).any(|p| p == ["--model", "x"]),
        "the first launch argv is main: {args:?}"
    );
    // resume.rs builds Resumed.flags from a lifecycle record options (A2a); replay them.
    ready(&w);
    let resumed = Resumed {
        from: on_disk["id"].as_str().unwrap().into(),
        record: on_disk.clone(),
        flags: options.clone(),
    };
    let mut req = request(&w);
    req.task = "";
    req.resume = Some(&resumed);
    let y = launch_hc(&w.ctx, &caller, req, slot).unwrap();
    let spec = read_json(&gens::path(&of(&w, &y), 0, "spec"))
        .unwrap()
        .unwrap();
    assert_eq!(spec["flags"], json!(options), "the model is replayed");
    assert_eq!(
        spec["args"],
        json!(["--resume", on_disk["sessionId"]]),
        "no task, no second model"
    );
}

// G1.6, F5: a view that cannot be opened after the commit keeps the member and says why.
#[test]
fn a_view_failure_after_the_commit_keeps_the_member() {
    let w = World::human("");
    let caller = w.caller();
    w.fakes.hc_install("herdr-core");
    core_fixture::answer(&w, &["--version"], "hc 0.1.21");
    answer(&w.fakes, "{\"address\":\"t1\"}");
    for verb in ["new-window", "split-window"] {
        w.fakes.mux_rule(verb, 1, "", "no room");
    }
    w.launches_hold();
    let r = launch_hc(&w.ctx, &caller, request(&w), slot).unwrap();
    assert!(r["viewError"].as_str().is_some(), "{r}");
    let f = of(&w, &r);
    assert!(gens::committed(&f, 0).unwrap());
    assert!(
        !w.fakes.hc_calls().iter().any(|(_, a, _)| a[0] == "kill"),
        "nothing is rolled back"
    );
    assert!(!f.join("view.json").exists());
    let reply = view_reply(&r);
    assert_eq!(
        (&reply["view"], &reply["viewError"]),
        (&json!("none"), &r["viewError"])
    );
}

// PM call 6 (finding 5to8-5), D2: a spawned member that exits on its own has its end recorded by group B (`reap_hc`,
// `reap_life`, `gens::record_end`), then its exit notice posted. After the switch no lifecycle member is left without
// recorded completion.
#[test]
fn a_natural_exit_of_a_spawned_member_is_recorded_then_noticed() {
    let w = World::human("");
    let caller = w.caller();
    ready(&w);
    let r = launch_hc(&w.ctx, &caller, request(&w), slot).unwrap();
    let f = of(&w, &r);
    let rec = read_json(&f.join("record.json")).unwrap().unwrap();
    let mut t = core_fixture::wire(&rec);
    (t["name"], t["state"], t["reason"], t["exit_code"]) = (
        rec["hcName"].clone(),
        json!("finished"),
        json!("exited"),
        json!(3),
    );
    core_fixture::online(&w);
    core_fixture::listing(&w, &super::super::membership::team(&caller), &[]);
    core_fixture::get(&w, rec["hcAddress"].as_str().unwrap_or_default(), t);
    w.panes(&[]);
    let s = super::super::membership::Snapshot::take(&w.ctx, &caller).unwrap();
    let _ = super::super::registry::reap_hc(&w.ctx, &caller, &s);
    let end = gens::read(&f, 0, "ended")
        .known()
        .unwrap()
        .expect("the natural exit is recorded (D2)");
    assert_eq!(
        (&end["reason"], &end["exitCode"]),
        (&json!("exited"), &json!(3))
    );
    assert!(
        gens::read(&f, 0, "notice").known().unwrap().is_some(),
        "the exit notice of generation 0 is posted"
    );
}

// G1: a run error leaves an aborted generation 0 whose member holds no slot; nothing is killed.
#[test]
fn a_run_error_leaves_an_aborted_generation_and_no_member() {
    let w = World::human("");
    let caller = w.caller();
    w.fakes.hc_install("herdr-core");
    core_fixture::answer(&w, &["--version"], "hc 0.1.21");
    w.fakes.hc_answer(Err(Refusal::internal("timeout")));
    assert!(launch_hc(&w.ctx, &caller, request(&w), slot).is_err());
    let f = folder(&w);
    let c = read_json(&gens::path(&f, 0, "commit")).unwrap().unwrap();
    assert_eq!(gens::decide(&c), gens::Decision::Aborted);
    let r = read_json(&f.join("record.json")).unwrap().unwrap();
    assert!(r.get("pending").is_none() && r.get("generation").is_none());
    let locals = super::super::membership::locals(&w.ctx, &caller).unwrap();
    assert!(
        locals.iter().all(|l| l.finished == Some(true)),
        "a void generation holds no slot"
    );
    assert!(!w.fakes.hc_calls().iter().any(|(_, a, _)| a[0] == "kill"));
}

// The leader of up: a new member with a new session, kind leader, its team label (spec kind, G1.2).
#[test]
fn a_leader_is_a_new_member_with_a_new_session() {
    let w = World::human("");
    let caller = w.caller();
    ready(&w);
    let leader = crate::agents::leader::Leader {
        team: "0123456789abcdef".into(),
        options: vec!["--model".into(), "a b".into()],
        prompt: vec!["go".into()],
    };
    let mut req = request(&w);
    req.task = "";
    req.leader = Some(&leader);
    let r = launch_hc(&w.ctx, &caller, req, slot).unwrap();
    let f = of(&w, &r);
    let on_disk = read_json(&f.join("record.json")).unwrap().unwrap();
    let got = (&on_disk["lifecycle"], &on_disk["upTeam"], &on_disk["depth"]);
    assert_eq!(got, (&json!(1), &json!("0123456789abcdef"), &json!(0)));
    assert_eq!(
        (&on_disk["options"], &on_disk["prompt"]),
        (&json!(["--model", "a b"]), &json!(["go"]))
    );
    let spec = read_json(&gens::path(&f, 0, "spec")).unwrap().unwrap();
    assert_eq!(
        (&spec["kind"], &spec["sessionId"]),
        (&json!("leader"), &on_disk["sessionId"])
    );
    assert!(crate::checks::session_id(
        on_disk["sessionId"].as_str().unwrap()
    ));
    let calls = w.fakes.hc_calls();
    let run = calls
        .iter()
        .map(|(_, a, _)| a)
        .find(|a| a[0] == "run")
        .unwrap();
    assert!(run.contains(&"mux.up=0123456789abcdef".to_string()));
}

// options-6e: mux-agent resume makes a new lifecycle member that continues the ended conversation.
#[test]
fn a_resume_is_a_new_lifecycle_member_with_the_ended_session() {
    let w = World::human("");
    let caller = w.caller();
    ready(&w);
    let from = json!({"id": "bbbbbbbbbbbb", "name": "worker", "client": "claude", "sessionId": SESSION_ID,
        "session": caller.session, "directory": utf8(&w.ctx.cwd).unwrap(), "lifecycle": 1, "generation": "0",
        "options": ["--model", "x"], "prompt": ["old task"], "hcName": "m0123abcd", "parentPane": null});
    let flags = vec!["--model".to_string(), "x".to_string()];
    let resumed = Resumed {
        from: "bbbbbbbbbbbb".into(),
        record: from,
        flags,
    };
    let mut req = request(&w);
    req.task = "";
    req.resume = Some(&resumed);
    let r = launch_hc(&w.ctx, &caller, req, slot).unwrap();
    let f = of(&w, &r);
    let on_disk = read_json(&f.join("record.json")).unwrap().unwrap();
    let got = (
        &on_disk["lifecycle"],
        &on_disk["sessionId"],
        &on_disk["resumedFrom"],
    );
    assert_eq!(got, (&json!(1), &json!(SESSION_ID), &json!("bbbbbbbbbbbb")));
    assert_eq!(on_disk["options"], json!(["--model", "x"]));
    assert!(
        on_disk.get("prompt").is_none(),
        "a resume never carries the task"
    );
    assert_ne!(
        on_disk["hcName"],
        json!("m0123abcd"),
        "a new generation never reuses a name"
    );
    let spec = read_json(&gens::path(&f, 0, "spec")).unwrap().unwrap();
    assert_eq!(
        (&spec["kind"], &spec["sessionId"]),
        (&json!("resume"), &json!(SESSION_ID))
    );
    assert_eq!(spec["flags"], json!(["--model", "x"]));
}
