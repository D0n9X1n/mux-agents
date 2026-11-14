use super::spawn::Created;
use crate::{
    ctx::Ctx,
    error::{Refusal, Res},
    hc::{self, Core, Run},
    state::fs::write_json,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
/// Prepared input for the launch transaction the spawn adapter calls in b23-B.
pub struct Prepared {
    pub folder: PathBuf,
    pub record: Value,
    pub argv: Vec<String>,
}
fn field<'a>(v: &'a Value, k: &str) -> Res<&'a str> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| Refusal::internal("invalid prepared identity"))
}
/// The launch transaction the spawn adapter calls in b23-B.
pub fn execute(
    ctx: &Ctx,
    prepare: impl FnOnce() -> Res<Prepared>,
    attach: impl FnOnce(&Core<'_>, &Prepared, &mut Option<Created>) -> Res<()>,
    commit: impl FnOnce(&Path) -> Res<()>,
    clean: impl FnOnce(&Created) -> Res<bool>,
) -> Res<Value> {
    let core = hc::connect(ctx)?;
    execute_connected(&core, prepare, attach, commit, clean)
}
pub fn execute_connected(
    core: &Core<'_>,
    prepare: impl FnOnce() -> Res<Prepared>,
    attach: impl FnOnce(&Core<'_>, &Prepared, &mut Option<Created>) -> Res<()>,
    commit: impl FnOnce(&Path) -> Res<()>,
    clean: impl FnOnce(&Created) -> Res<bool>,
) -> Res<Value> {
    let mut p = prepare()?;
    let mut candidate = p.record.clone();
    candidate["hcAddress"] = json!("pending");
    candidate["launcherArgv"] = json!(p.argv);
    let id = field(&p.record, "id")?;
    let team = field(&p.record, "team")?;
    let spec = crate::state::fs::read_json(&p.folder.join("spec.json"))?;
    if super::core_identity::Identity::read(&candidate, id, team).is_none()
        || spec
            .as_ref()
            .and_then(|s| s.get("generation"))
            .and_then(Value::as_str)
            != Some("0")
    {
        return Err(Refusal::internal("invalid prepared identity or spec"));
    }
    let id = field(&p.record, "id")?;
    if !super::agent_id(id) || p.argv.is_empty() || field(&p.record, "generation")? != "0" {
        return Err(Refusal::internal("invalid prepared generation"));
    }
    let name = format!("m{}", &id[..8]);
    let mut labels = vec![
        ("mux.managed", "1"),
        ("mux.team", field(&p.record, "team")?),
        ("mux.id", id),
        ("mux.gen", "0"),
        ("mux.name", field(&p.record, "name")?),
    ];
    // `up`'s leader carries its team (#103).
    if let Some(up) = p.record.get("upTeam").and_then(Value::as_str) {
        labels.push(("mux.up", up));
    }
    let argv: Vec<&str> = p.argv.iter().map(String::as_str).collect();
    let run = attempt(|| {
        core.run(&Run {
            bg: true,
            keep: true,
            outlive_parent: true,
            no_notify: true,
            name: Some(&name),
            labels: &labels,
            cwd: Some(field(&p.record, "directory")?),
            argv: &argv,
        })
    });
    let address = match run {
        Ok(a) => a,
        Err(mut e) => {
            if write_json(
                &p.folder.join("abort.json"),
                &json!({"runUncertain":true,"hcAddress":null,"message":e.message}),
            )
            .is_err()
            {
                e.message.push_str("; abort evidence could not be written");
            }
            return Err(e);
        }
    };
    p.record["hcAddress"] = address.clone().into();
    p.record["launcherArgv"] = json!(p.argv);
    let mut pane = None;
    let operation = || -> Res<Value> {
        crate::state::fs::write_json_durable(&p.folder.join("record.json"), &p.record)?;
        attach(core, &p, &mut pane)?;
        let v = pane
            .as_ref()
            .filter(|v| !v.command.is_empty())
            .ok_or_else(|| Refusal::internal("unproven new view"))?;
        p.record["pane"] = v.pane.clone().into();
        p.record["window"] = v.window.clone().into();
        p.record["startCommand"] = v.command.clone().into();
        crate::state::fs::write_json_durable(&p.folder.join("record.json"), &p.record)?;
        commit(&p.folder)?;
        Ok(p.record.clone())
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation))
        .unwrap_or_else(|_| Err(Refusal::internal("launch panicked")));
    match result {
        Ok(record) => Ok(record),
        Err(mut e) => {
            if !rollback(core, &p, &address, pane.as_ref(), clean) {
                e.message.push_str("; abort evidence could not be written");
            }
            Err(e)
        }
    }
}
fn attempt<T>(f: impl FnOnce() -> Res<T>) -> Res<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(Refusal::internal("launch boundary panicked")))
}
fn rollback(
    core: &Core<'_>,
    p: &Prepared,
    address: &str,
    pane: Option<&Created>,
    clean: impl FnOnce(&Created) -> Res<bool>,
) -> bool {
    let mut evidence = json!({"hcAddress":address,"identity":p.record,"pane":pane.map(|v|v.pane.as_str()),"kill":"pending","viewCleanup":"pending"});
    let path = p.folder.join("abort.json");
    let first = write_json(&path, &evidence).is_ok();
    evidence["kill"] = match attempt(|| core.kill(address)) {
        Ok(()) => json!("acknowledged"),
        Err(e) => json!({"uncertain":true,"message":e.message}),
    };
    evidence["viewCleanup"] = match pane.filter(|v| !v.command.is_empty()) {
        None => json!("unproven"),
        Some(v) => match attempt(|| clean(v)) {
            Ok(gone) => json!({"gone":gone}),
            Err(e) => json!({"failed":true,"message":e.message}),
        },
    };
    write_json(&path, &evidence).is_ok() && first
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{agents::fixture::World, spawn::Out, testing::Fakes};
    use serde_json::json;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    fn answer(f: &Fakes, text: &str) {
        f.hc_answer(Ok(Out {
            stdout: text.into(),
            ..Out::default()
        }));
    }
    fn ready(w: &World) {
        w.fakes.hc_install("hc");
        answer(&w.fakes, "hc 0.1.21");
    }
    fn prepared(w: &World) -> Prepared {
        let folder = w.ctx.state_root.join("key/agents/aaaaaaaaaaaa");
        std::fs::create_dir_all(&folder).unwrap();
        let spec = folder.join("spec.json");
        write_json(&spec, &json!({"generation":"0"})).unwrap();
        let mut record = super::super::core_identity::tests::record();
        record.as_object_mut().unwrap().remove("hcAddress");
        record["directory"] = w.ctx.cwd.to_str().unwrap().into();
        Prepared {
            folder,
            argv: crate::launcher_scope::argv(&w.ctx, &spec).unwrap(),
            record,
        }
    }
    fn created() -> Created {
        Created {
            pane: "%9".into(),
            window: "@5".into(),
            command: "hc attach t1".into(),
        }
    }
    #[test]
    fn connected_transaction_does_not_reconnect() {
        let w = World::human("");
        ready(&w);
        let core = hc::connect(&w.ctx).unwrap();
        answer(&w.fakes, r#"{"address":"t1"}"#);
        execute_connected(
            &core,
            || Ok(prepared(&w)),
            |_, _, view| {
                *view = Some(created());
                Ok(())
            },
            |p| write_json(&p.join("commit.json"), &json!({})),
            |_| panic!("clean"),
        )
        .unwrap();
        assert_eq!(
            w.fakes
                .hc_calls()
                .iter()
                .map(|(_, a, _)| a[0].clone())
                .collect::<Vec<_>>(),
            ["--version", "run"]
        );
        w.fakes.hc_done().unwrap();
    }
    #[test]
    fn core_connect_precedes_all_preparation() {
        for version in [None, Some("hc 0.1.20"), Some("garbled")] {
            let w = World::human("");
            if let Some(v) = version {
                w.fakes.hc_install("hc");
                answer(&w.fakes, v);
            }
            let called = Cell::new(false);
            let result = execute(
                &w.ctx,
                || {
                    called.set(true);
                    Ok(prepared(&w))
                },
                |_, _, _| panic!("attach"),
                |_| panic!("commit"),
                |_| panic!("clean"),
            );
            assert!(result.is_err());
            assert!(!called.get());
            assert!(!w.ctx.state_root.exists());
        }
    }
    #[test]
    fn identity_precedes_view_and_commit() {
        let w = World::human("");
        ready(&w);
        answer(&w.fakes, r#"{"address":"t1"}"#);
        let events = RefCell::new(Vec::new());
        let r = execute(
            &w.ctx,
            || {
                events.borrow_mut().push("prepare");
                Ok(prepared(&w))
            },
            |core, p, pane| {
                assert_eq!(core.binary(), w.fakes.hc_install("hc"));
                let r = crate::state::fs::read_json(&p.folder.join("record.json"))?.unwrap();
                assert_eq!(r["hcAddress"], "t1");
                assert_eq!(r["generation"], "0");
                assert!(!p.folder.join("commit.json").exists());
                assert_eq!(r["launcherArgv"], json!(p.argv));
                events.borrow_mut().push("attach");
                *pane = Some(created());
                Ok(())
            },
            |folder| {
                let r = crate::state::fs::read_json(&folder.join("record.json"))?.unwrap();
                assert_eq!(r["pane"], "%9");
                events.borrow_mut().push("commit");
                write_json(&folder.join("commit.json"), &json!({}))
            },
            |_| panic!("clean"),
        )
        .unwrap();
        assert_eq!(*events.borrow(), ["prepare", "attach", "commit"]);
        assert_eq!(r["hcAddress"], "t1");
        let calls = w.fakes.hc_calls();
        assert_eq!(calls.len(), 2);
        let a = &calls[1].1;
        assert_eq!(
            &a[..6],
            [
                "run",
                "--bg",
                "--keep",
                "--outlive-parent",
                "--no-notify",
                "--name"
            ]
        );
        assert_eq!(a[6], "maaaaaaaa");
        for label in [
            "mux.managed=1",
            "mux.team=k:$3",
            "mux.id=aaaaaaaaaaaa",
            "mux.gen=0",
            "mux.name=worker",
        ] {
            assert!(a.iter().any(|s| s == label));
        }
        assert_eq!(a.iter().filter(|s| s.as_str() == "run").count(), 1);
    }

    #[test]
    fn post_run_fault_matrix_preserves_evidence_and_never_skips_cleanup() {
        for stage in [
            "identity",
            "attach",
            "reply",
            "tags",
            "proof",
            "record",
            "commit",
            "timeout",
            "panic",
            "abort-write",
            "kill",
            "cleanup",
            "cleanup-panic",
            "title",
        ] {
            let w = World::human("");
            ready(&w);
            answer(&w.fakes, r#"{"address":"t1"}"#);
            if stage == "kill" {
                w.fakes.hc_answer(Err(Refusal::internal("kill uncertain")));
            } else {
                answer(&w.fakes, "{}");
            }
            let folder = w.ctx.state_root.join("key/agents/aaaaaaaaaaaa");
            let cleaned = Rc::new(Cell::new(0));
            let seen = cleaned.clone();
            let result = execute(
                &w.ctx,
                || {
                    let p = prepared(&w);
                    if stage == "identity" {
                        std::fs::create_dir(p.folder.join("record.json"))?;
                    }
                    Ok(p)
                },
                |_, p, pane| {
                    if ["attach", "reply"].contains(&stage) {
                        return Err(Refusal::internal(stage));
                    }
                    *pane = Some(created());
                    if stage == "record" {
                        std::fs::remove_file(p.folder.join("record.json"))?;
                        std::fs::create_dir(p.folder.join("record.json"))?;
                    }
                    if stage == "abort-write" {
                        std::fs::create_dir(p.folder.join("abort.json"))?;
                    }
                    if stage == "panic" {
                        panic!("after attach");
                    }
                    if [
                        "tags",
                        "proof",
                        "abort-write",
                        "kill",
                        "cleanup",
                        "cleanup-panic",
                    ]
                    .contains(&stage)
                    {
                        return Err(Refusal::internal(stage));
                    }
                    Ok(())
                },
                |p| {
                    if stage == "timeout" {
                        write_json(&p.join("commit.json"), &json!({"aborted":true}))?;
                    }
                    if ["commit", "timeout"].contains(&stage) {
                        return Err(Refusal::internal(stage));
                    }
                    write_json(&p.join("commit.json"), &json!({}))
                },
                move |_| {
                    seen.set(seen.get() + 1);
                    if stage == "cleanup-panic" {
                        panic!("cleanup");
                    }
                    if stage == "cleanup" {
                        return Err(Refusal::internal("probe failed"));
                    }
                    Ok(true)
                },
            );
            if stage == "title" {
                assert!(result.is_ok());
                assert_eq!(cleaned.get(), 0);
                continue;
            }
            assert!(result.is_err(), "{stage}");
            assert!(folder.join("spec.json").exists());
            let calls = w.fakes.hc_calls();
            assert_eq!(calls.iter().filter(|(_, a, _)| a[0] == "run").count(), 1);
            assert_eq!(
                calls
                    .iter()
                    .filter(|(_, a, _)| a[0] == "kill")
                    .map(|(_, a, _)| a[1].as_str())
                    .collect::<Vec<_>>(),
                ["t1"]
            );
            assert_eq!(
                cleaned.get(),
                usize::from(!["identity", "attach", "reply"].contains(&stage)),
                "{stage}"
            );
            if stage != "abort-write" {
                let e = crate::state::fs::read_json(&folder.join("abort.json"))
                    .unwrap()
                    .unwrap();
                assert_eq!(e["hcAddress"], "t1");
                if stage == "kill" {
                    assert_eq!(e["kill"]["uncertain"], true);
                }
                if stage == "cleanup-panic" {
                    assert_eq!(e["viewCleanup"]["failed"], true);
                }
            }
            assert!(!crate::agents::committed(&folder), "{stage}");
        }
    }

    // Each record.json write is flushed to disk; a failed flush at either one rolls the launch back: no commit, so
    // the launcher never starts the client, and the run is killed.
    #[test]
    fn a_failed_record_flush_never_commits() {
        for nth in [0, 1] {
            let w = World::human("");
            ready(&w);
            answer(&w.fakes, r#"{"address":"t1"}"#);
            answer(&w.fakes, "{}");
            crate::state::fs::fail_durable_flush(Some(nth));
            let committed = Cell::new(false);
            let got = execute(
                &w.ctx,
                || Ok(prepared(&w)),
                |_, _, pane| {
                    *pane = Some(created());
                    Ok(())
                },
                |_| {
                    committed.set(true);
                    Ok(())
                },
                |_| Ok(true),
            );
            crate::state::fs::fail_durable_flush(None);
            assert!(got.is_err(), "{nth}");
            assert!(!committed.get(), "{nth}");
            assert_eq!(
                w.fakes.hc_calls().last().unwrap().1,
                ["kill", "t1", "--json"],
                "{nth}"
            );
            w.fakes.hc_done().unwrap();
        }
    }

    #[test]
    fn rollback_writes_pending_before_cleanup_and_remembers_first_write_failure() {
        for block_first_write in [false, true] {
            let w = World::human("");
            ready(&w);
            answer(&w.fakes, r#"{"address":"t1"}"#);
            answer(&w.fakes, "{}");
            let path = w.ctx.state_root.join("key/agents/aaaaaaaaaaaa/abort.json");
            let saw_pending = Cell::new(false);
            let cleaned = Cell::new(false);
            let got = execute(
                &w.ctx,
                || Ok(prepared(&w)),
                |_, _, pane| {
                    *pane = Some(created());
                    if block_first_write {
                        std::fs::create_dir(&path)?;
                    }
                    panic!("after attach");
                },
                |_| panic!("commit"),
                |_| {
                    if block_first_write {
                        // Let the final write succeed without hiding the first write's failure.
                        std::fs::remove_dir(&path)?;
                    } else {
                        // Observe pending evidence before the final write can replace it.
                        if let Some(e) = crate::state::fs::read_json(&path)? {
                            saw_pending.set(
                                e["hcAddress"] == "t1"
                                    && e["kill"] == "pending"
                                    && e["viewCleanup"] == "pending",
                            );
                        }
                    }
                    cleaned.set(true);
                    Ok(true)
                },
            )
            .unwrap_err();
            assert!(cleaned.get());
            if !block_first_write {
                assert!(saw_pending.get(), "pending evidence must precede cleanup");
            }
            assert_eq!(
                got.message
                    .contains("; abort evidence could not be written"),
                block_first_write,
                "first evidence write failure must remain reported"
            );
            let evidence = crate::state::fs::read_json(&path).unwrap().unwrap();
            assert_eq!(evidence["kill"], "acknowledged");
            assert_eq!(evidence["viewCleanup"]["gone"], true);
        }
    }

    #[test]
    fn empty_view_command_never_commits_or_authorizes_cleanup() {
        for attach_fails in [false, true] {
            let w = World::human("");
            ready(&w);
            answer(&w.fakes, r#"{"address":"t1"}"#);
            answer(&w.fakes, "{}");
            let committed = Cell::new(false);
            let cleaned = Cell::new(false);
            let got = execute(
                &w.ctx,
                || Ok(prepared(&w)),
                |_, _, pane| {
                    *pane = Some(Created {
                        command: String::new(),
                        ..created()
                    });
                    if attach_fails {
                        return Err(Refusal::internal("proof failed"));
                    }
                    Ok(())
                },
                |_| {
                    committed.set(true);
                    Ok(())
                },
                |_| {
                    cleaned.set(true);
                    Ok(true)
                },
            );
            assert!(got.is_err(), "empty view command must refuse");
            assert!(!committed.get(), "unproven view must not commit");
            assert!(
                !cleaned.get(),
                "empty view command must not authorize cleanup"
            );
            let evidence = crate::state::fs::read_json(
                &w.ctx.state_root.join("key/agents/aaaaaaaaaaaa/abort.json"),
            )
            .unwrap()
            .unwrap();
            assert_eq!(evidence["viewCleanup"], "unproven");
            assert_eq!(
                w.fakes.hc_calls().last().unwrap().1,
                ["kill", "t1", "--json"]
            );
        }
    }

    #[test]
    fn uncertain_run_reports_failed_abort_write_without_retry() {
        let w = World::human("");
        ready(&w);
        w.fakes.hc_answer(Err(Refusal::internal("timeout")));
        let got = execute(
            &w.ctx,
            || {
                let p = prepared(&w);
                std::fs::create_dir(p.folder.join("abort.json"))?;
                Ok(p)
            },
            |_, _, _| panic!("attach"),
            |_| panic!("commit"),
            |_| panic!("clean"),
        )
        .unwrap_err();
        assert!(
            got.message
                .contains("; abort evidence could not be written")
        );
        let calls = w.fakes.hc_calls();
        assert_eq!(calls.iter().filter(|(_, a, _)| a[0] == "run").count(), 1);
        assert!(!calls.iter().any(|(_, a, _)| a[0] == "kill"));
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn uncertain_run_is_never_retried_or_guessed_killed() {
        struct PanicRun(Fakes);
        impl crate::hc::Hc for PanicRun {
            fn find(&self, n: &str, p: &str) -> Option<PathBuf> {
                crate::hc::Hc::find(&self.0, n, p)
            }
            fn captured(&self, p: &Path, a: &[&str], t: std::time::Duration) -> Res<Out> {
                let out = crate::hc::Hc::captured(&self.0, p, a, t);
                if a[0] == "run" {
                    panic!("run uncertain");
                }
                out
            }
        }
        for mode in ["nonzero", "malformed", "timeout", "panic"] {
            let mut w = World::human("");
            ready(&w);
            match mode {
                "nonzero" => w.fakes.hc_answer(Ok(Out {
                    code: 1,
                    stdout: r#"{"address":"do-not-kill"}"#.into(),
                    stderr: String::new(),
                })),
                "timeout" => w.fakes.hc_answer(Err(Refusal::internal("timeout"))),
                _ => answer(&w.fakes, "not-json"),
            }
            if mode == "panic" {
                w.ctx.hc = Box::new(PanicRun(w.fakes.clone()));
            }
            let got = execute(
                &w.ctx,
                || Ok(prepared(&w)),
                |_, _, _| panic!("attach"),
                |_| panic!("commit"),
                |_| panic!("clean"),
            );
            assert!(got.is_err());
            let calls = w.fakes.hc_calls();
            assert_eq!(calls.iter().filter(|(_, a, _)| a[0] == "run").count(), 1);
            assert!(!calls.iter().any(|(_, a, _)| a[0] == "kill"));
            let e = crate::state::fs::read_json(
                &w.ctx.state_root.join("key/agents/aaaaaaaaaaaa/abort.json"),
            )
            .unwrap()
            .unwrap();
            assert_eq!(e["runUncertain"], true);
            assert!(e["hcAddress"].is_null());
        }
    }
    #[test]
    fn failed_abort_write_is_reported_without_skipping_kill_or_cleanup() {
        let w = World::human("");
        ready(&w);
        answer(&w.fakes, r#"{"address":"t1"}"#);
        answer(&w.fakes, "{}");
        let cleaned = Cell::new(false);
        let got = execute(
            &w.ctx,
            || Ok(prepared(&w)),
            |_, p, pane| {
                *pane = Some(created());
                std::fs::create_dir(p.folder.join("abort.json"))?;
                Err(Refusal::internal("proof failed"))
            },
            |_| panic!("commit"),
            |_| {
                cleaned.set(true);
                Ok(true)
            },
        )
        .unwrap_err();
        assert!(got.message.contains("abort evidence could not be written"));
        assert!(got.message.contains("proof failed"));
        assert!(cleaned.get());
        assert_eq!(
            w.fakes.hc_calls().last().unwrap().1,
            ["kill", "t1", "--json"]
        );
    }
    #[test]
    fn prepare_failure_has_no_run_and_kill_error_still_cleans() {
        let w = World::human("");
        ready(&w);
        let got = execute(
            &w.ctx,
            || Err(Refusal::internal("prepare failed")),
            |_, _, _| panic!("attach"),
            |_| panic!("commit"),
            |_| panic!("clean"),
        );
        assert!(got.is_err());
        assert_eq!(w.fakes.hc_calls().len(), 1);
        let w = World::human("");
        ready(&w);
        let ctx = &w.ctx;
        let core = hc::connect(ctx).unwrap();
        let p = prepared(&w);
        let cleaned = Cell::new(false);
        let value: Res<()> = attempt(|| panic!("boundary panicked"));
        assert!(value.is_err());
        w.fakes.hc_answer(Err(Refusal::internal("kill failed")));
        assert!(rollback(&core, &p, "t1", Some(&created()), |_| {
            cleaned.set(true);
            Ok(true)
        }));
        assert!(cleaned.get());
    }

    #[test]
    fn invalid_preparation_never_runs() {
        for key in ["generation", "team", "sessionId", "spec"] {
            let w = World::human("");
            ready(&w);
            let got = execute(
                &w.ctx,
                || {
                    let mut p = prepared(&w);
                    if key == "spec" {
                        write_json(&p.folder.join("spec.json"), &json!({"generation":"1"}))?;
                    } else {
                        p.record[key] = json!("bad");
                    }
                    Ok(p)
                },
                |_, _, _| panic!("attach"),
                |_| panic!("commit"),
                |_| panic!("clean"),
            );
            assert!(got.is_err());
            assert_eq!(w.fakes.hc_calls().len(), 1, "{key}");
        }
    }
    #[test]
    fn kill_provider_panic_still_cleans_the_proven_view() {
        struct PanicKill(Fakes);
        impl crate::hc::Hc for PanicKill {
            fn find(&self, n: &str, p: &str) -> Option<PathBuf> {
                crate::hc::Hc::find(&self.0, n, p)
            }
            fn captured(&self, p: &Path, a: &[&str], t: std::time::Duration) -> Res<Out> {
                let out = crate::hc::Hc::captured(&self.0, p, a, t);
                if a[0] == "kill" {
                    panic!("kill uncertain");
                }
                out
            }
        }
        let mut w = World::human("");
        ready(&w);
        answer(&w.fakes, r#"{"address":"t1"}"#);
        answer(&w.fakes, "{}");
        w.ctx.hc = Box::new(PanicKill(w.fakes.clone()));
        let cleaned = Cell::new(false);
        let e = execute(
            &w.ctx,
            || Ok(prepared(&w)),
            |_, _, pane| {
                *pane = Some(created());
                Err(Refusal::internal("proof"))
            },
            |_| panic!("commit"),
            |_| {
                cleaned.set(true);
                Ok(true)
            },
        )
        .unwrap_err();
        assert_eq!(e.message, "unexpected error (proof)");
        assert!(cleaned.get());
        let v = crate::state::fs::read_json(
            &w.ctx.state_root.join("key/agents/aaaaaaaaaaaa/abort.json"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(v["kill"]["uncertain"], true);
        assert_eq!(v["viewCleanup"]["gone"], true);
    }
}
