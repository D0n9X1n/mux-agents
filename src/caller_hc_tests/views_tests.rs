//! a3c call 47 (TL #149, HIGH): a view verb's prerequisite holds against the handler's own proof, not only dispatch's.
//! Dispatch proves the member with its server live; the server goes before the handler proves it again. The verb
//! refuses `views` and launches nothing (no `hc run`).
use super::k2_tests::kill_server;
use super::*;

/// The member's server pid, from the server.json its proof binds.
fn server_pid(folder: &std::path::Path) -> u32 {
    let at = folder
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("server.json");
    let s = crate::state::fs::read_json(&at).unwrap().unwrap();
    s["serverPid"].as_u64().unwrap() as u32
}

// The dispatch-to-handler schedule: dispatch's proof (the first `--version`) sees the live server; the server is gone
// at the handler's proof (the second `--version`). `watch` is not here: it refuses a non-terminal stdout before its
// proof, so a unit test cannot reach its gate.
#[test]
fn a_server_gone_between_dispatch_and_handler_refuses_views_and_launches_nothing() {
    for args in [
        vec!["resume", "worker"],
        vec!["spawn", "-Client", "claude", "-Task", "hi"],
        vec!["focus", "worker"],
        vec!["sidebar"],
        vec!["placement", "h"],
    ] {
        let (w, r, folder) = lifecycle_worker(ok(), ok());
        let pid = server_pid(&folder);
        proof(&w, &r);
        proof(&w, &r);
        let (fakes, versions) = (w.fakes.clone(), std::rc::Rc::new(std::cell::Cell::new(0)));
        w.fakes.hc_probe(move |argv| {
            if argv.first().map(String::as_str) == Some("--version") {
                versions.set(versions.get() + 1);
                if versions.get() == 2 {
                    fakes.forget(pid);
                }
            }
        });
        let (code, out) = crate::cli::run(
            &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            || Ok(&w.ctx),
        );
        let v: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
        assert_eq!((code, &v["check"]), (1, &json!("views")), "{args:?}: {out}");
        let ran = w
            .fakes
            .hc_calls()
            .iter()
            .any(|c| c.1.first().map(String::as_str) == Some("run"));
        assert!(!ran, "{args:?}: nothing launched");
    }
}

// The gate's first-use leg: a member presented at the handler's proof whose server is gone before its first
// multiplexer use is refused `views` by the gate (not swallowed into a snapshot's view error).
#[test]
fn the_view_gate_refuses_a_server_gone_after_the_handlers_proof() {
    let (w, r, folder) = lifecycle_worker(ok(), ok());
    proof(&w, &r);
    let c = caller::resolve(&w.ctx).unwrap();
    assert!(c.presentation.is_some(), "presented at the handler's proof");
    *w.ctx.mux.borrow_mut() = None;
    kill_server(&w, &folder);
    let e = crate::cli::member_views(&w.ctx, &c).unwrap_err();
    assert_eq!((e.code, e.check), (1, "views"), "{e:?}");
    assert!(w.ctx.mux.borrow().is_none(), "nothing built");
}
