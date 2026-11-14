use super::*;
use crate::{
    agents::{
        core_fixture,
        fixture::{SESSION_ID, World},
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
        split: None,
        resume: None,
    }
}
fn slot(_: &crate::hc::Core<'_>) -> Res<CoreSlot> {
    Ok(CoreSlot {
        name: "worker".into(),
        color: "blue",
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
struct ProbeReset;
impl Drop for ProbeReset {
    fn drop(&mut self) {
        commit_probe(None);
    }
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
        let argv = crate::launcher_scope::argv(&w.ctx, &folder.join("spec.json")).unwrap();
        let id = r["id"].as_str().unwrap();
        let team = format!("{}:{}", caller.server_key, caller.session);
        assert_eq!(r["generation"], "0");
        assert_eq!(r["launcherArgv"], json!(argv));
        assert!(super::super::core_identity::Identity::read(&r, id, &team).is_some());
        assert_eq!(
            super::super::membership::locals(&w.ctx, &caller).unwrap()[0]
                .stored
                .record,
            r
        );
        assert_eq!(
            read_json(&folder.join("spec.json")).unwrap().unwrap()["generation"],
            "0"
        );
        let mut expected = vec![
            "run".into(),
            "--bg".into(),
            "--keep".into(),
            "--outlive-parent".into(),
            "--no-notify".into(),
            "--name".into(),
            format!("m{}", &id[..8]),
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
        assert!(super::super::committed(&folder));
        w.fakes.hc_done().unwrap();
    }
}
#[test]
fn record_then_commit_order() {
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
        assert_eq!(r["hcAddress"], "t1");
        assert!(r.get("pane").is_none());
        assert!(!folder.join("commit.json").exists());
        seen.borrow_mut().push("attach");
        Ok(crate::mux::Output {
            stdout: "%9|@5|exact,#} attach t1\n".into(),
            ..Default::default()
        })
    });
    let seen = log.clone();
    let _reset = ProbeReset;
    commit_probe(Some(Box::new(move |folder| {
        let r = read_json(&folder.join("record.json")).unwrap().unwrap();
        assert_eq!(r["pane"], "%9");
        assert_eq!(r["window"], "@5");
        assert_eq!(r["startCommand"], "exact,#} attach t1");
        assert!(!folder.join("commit.json").exists());
        seen.borrow_mut().push("commit");
    })));
    launch_hc(&w.ctx, &caller, request(&w), slot).unwrap();
    assert_eq!(*log.borrow(), ["attach", "commit"]);
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
        let success = ["title", "title-panic", "title-option"].contains(&stage);
        let known = !["spec", "run-timeout", "run-malformed"].contains(&stage);
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
        if known && !success {
            core_fixture::answer(&w, &["kill", "t1", "--json"], "{}");
        }
        w.fakes
            .mux_rule("new-window", 0, "%9|@5|exact,#} attach t1\n", "");
        w.launches_hold();
        w.fakes
            .mux_rule("-t %9 #{pane_id}|#{@agent-id}", 0, "|\n", "");
        let state = super::super::server_dir(&w.ctx, &caller);
        let state2 = state.clone();
        if stage == "spec" {
            w.fakes.clock_probe(move |_| {
                let folder = std::fs::read_dir(state2.join("agents"))
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                let p = folder.join("spec.json");
                if p.is_file() {
                    std::fs::remove_file(&p).unwrap();
                    std::fs::create_dir(p).unwrap();
                }
            });
        }
        w.fakes.hc_probe(move |a| {
            if stage == "record" && a[0] == "run" {
                let i = a.iter().position(|x| x == "launcher").unwrap();
                std::fs::create_dir(Path::new(&a[i + 1]).parent().unwrap().join("record.json"))
                    .unwrap();
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
            "view-record" => w.fakes.mux_script("@agent-context", move |_| {
                let folder = std::fs::read_dir(state.join("agents"))
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                std::fs::remove_file(folder.join("record.json"))?;
                std::fs::create_dir(folder.join("record.json"))?;
                Ok(crate::mux::Output::default())
            }),
            _ => {}
        }
        let _reset = ProbeReset;
        if stage == "commit-fs" {
            commit_probe(Some(Box::new(|p| {
                std::fs::create_dir(p.join("commit.json")).unwrap()
            })));
        }
        if stage == "launcher-timeout" {
            commit_probe(Some(Box::new(|p| {
                write_json(&p.join("commit.json"), &json!({"aborted":true})).unwrap()
            })));
        }
        let got = launch_hc(&w.ctx, &caller, req, slot);
        let folder = folder(&w);
        let calls = w.fakes.hc_calls();
        assert_eq!(
            calls.iter().filter(|(_, a, _)| a[0] == "run").count(),
            usize::from(stage != "spec"),
            "{stage}"
        );
        assert_eq!(got.is_ok(), success, "{stage}: {got:?}");
        let kills = calls
            .iter()
            .filter(|(_, a, _)| a[0] == "kill")
            .map(|(_, a, _)| a.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            kills,
            if known && !success {
                vec![vec!["kill", "t1", "--json"]]
            } else {
                vec![]
            },
            "{stage}"
        );
        let proven = [
            "tag",
            "proof",
            "view-record",
            "commit-fs",
            "launcher-timeout",
            "panic",
        ]
        .contains(&stage);
        let mux = w.fakes.mux_calls();
        let clean = mux
            .iter()
            .filter(|a| a.last().is_some_and(|s| s == "kill-pane -t %9"))
            .collect::<Vec<_>>();
        assert_eq!(clean.len(), usize::from(proven), "{stage}");
        if proven {
            assert_eq!(
                clean[0][5],
                registry::Launch {
                    command: "exact,#} attach t1".into(),
                    pid: None
                }
                .launched()
            );
        }
        if !success {
            assert!(
                folder.join("abort.json").exists(),
                "abort evidence missing at {stage}"
            );
            let e = read_json(&folder.join("abort.json")).unwrap().unwrap();
            if known {
                assert_eq!(e["hcAddress"], "t1");
                assert_eq!(e["kill"], "acknowledged");
                if proven {
                    assert_eq!(e["viewCleanup"]["gone"], true);
                } else {
                    assert_eq!(e["viewCleanup"], "unproven");
                }
            } else {
                assert!(e["hcAddress"].is_null());
                assert_eq!(e["runUncertain"], stage != "spec");
            }
            assert!(!super::super::committed(&folder));
        }
        assert!(folder.join("spec.json").exists());
        assert_eq!(
            std::fs::read_dir(source.parent().unwrap()).unwrap().count(),
            2,
            "copy retained: {stage}"
        );
        w.fakes.hc_done().unwrap();
    }
}
