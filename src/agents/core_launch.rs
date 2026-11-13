use super::spawn::Created;
use crate::{
    ctx::Ctx,
    error::{Refusal, Res},
    hc::{self, Core, Run},
    state::fs::write_json,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
pub(crate) struct Prepared {
    pub folder: PathBuf,
    pub record: Value,
    pub argv: Vec<String>,
}
fn field<'a>(v: &'a Value, k: &str) -> Res<&'a str> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| Refusal::internal("invalid prepared identity"))
}
pub(crate) fn execute(
    ctx: &Ctx,
    prepare: impl FnOnce() -> Res<Prepared>,
    attach: impl FnOnce(&Core<'_>, &Prepared, &mut Option<Created>) -> Res<()>,
    commit: impl FnOnce(&Path) -> Res<()>,
    clean: impl FnOnce(&Created) -> Res<bool>,
) -> Res<Value> {
    let core = hc::connect(ctx)?;
    let mut p = prepare()?;
    let id = field(&p.record, "id")?;
    if !super::agent_id(id) || p.argv.is_empty() || field(&p.record, "generation")? != "0" {
        return Err(Refusal::internal("invalid prepared generation"));
    }
    let name = format!("m{}", &id[..8]);
    let labels = [
        ("mux.managed", "1"),
        ("mux.team", field(&p.record, "team")?),
        ("mux.id", id),
        ("mux.gen", "0"),
        ("mux.name", field(&p.record, "name")?),
    ];
    let argv: Vec<&str> = p.argv.iter().map(String::as_str).collect();
    let run = core.run(&Run {
        bg: true,
        keep: true,
        outlive_parent: true,
        no_notify: true,
        name: Some(&name),
        labels: &labels,
        cwd: Some(field(&p.record, "directory")?),
        argv: &argv,
    });
    let address = match run {
        Ok(a) => a,
        Err(e) => {
            let _ = write_json(
                &p.folder.join("abort.json"),
                &json!({"runUncertain":true,"hcAddress":null,"message":e.message}),
            );
            return Err(e);
        }
    };
    p.record["hcAddress"] = address.clone().into();
    p.record["launcherArgv"] = json!(p.argv);
    let mut pane = None;
    let operation = || -> Res<Value> {
        write_json(&p.folder.join("record.json"), &p.record)?;
        attach(&core, &p, &mut pane)?;
        let v = pane
            .as_ref()
            .filter(|v| !v.command.is_empty())
            .ok_or_else(|| Refusal::internal("unproven new view"))?;
        p.record["pane"] = v.pane.clone().into();
        p.record["window"] = v.window.clone().into();
        p.record["startCommand"] = v.command.clone().into();
        write_json(&p.folder.join("record.json"), &p.record)?;
        commit(&p.folder)?;
        Ok(p.record.clone())
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation))
        .unwrap_or_else(|_| Err(Refusal::internal("launch panicked")));
    if let Err(error) = result {
        rollback(&core, &p, &address, pane.as_ref(), clean);
        return Err(error);
    }
    result
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
) {
    let mut evidence = json!({"hcAddress":address,"identity":p.record,"pane":pane.map(|v|v.pane.as_str()),"kill":"pending","viewCleanup":"pending"});
    let path = p.folder.join("abort.json");
    let _ = write_json(&path, &evidence);
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
    let _ = write_json(&path, &evidence);
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
}
