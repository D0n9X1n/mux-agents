//! proof-0b K2 (A3c Task 5; W3; ambiguities 9, 11, 12): a lifecycle member is proven without its multiplexer; its
//! view and live server only present it. In their own file: `caller_hc_tests.rs` is past 500 lines (call 38).
use super::*;

/// Kills the multiplexer server of the member in `folder` in the fake process table (its pid from server.json).
fn kill_server(w: &World, folder: &std::path::Path) {
    let at = folder
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("server.json");
    let server = crate::state::fs::read_json(&at).unwrap().unwrap();
    w.fakes.forget(server["serverPid"].as_u64().unwrap() as u32);
}

// K2, W3: a lifecycle member is proven with its server gone or its view missing; it is presented only with both,
// and its proof asks the multiplexer nothing.
#[test]
fn a_lifecycle_member_is_proven_without_its_multiplexer() {
    let (w, r, folder) = lifecycle_worker(ok(), ok());
    proof(&w, &r);
    let got = caller::resolve(&w.ctx).unwrap();
    let up = Some(("%9".to_string(), "@5".to_string()));
    assert_eq!(
        (got.member.as_deref(), got.presentation),
        (Some(ID), up),
        "views up"
    );
    kill_server(&w, &folder);
    proof(&w, &r);
    let got = caller::resolve(&w.ctx).expect("proven without the server");
    let fields = (
        got.member.as_deref(),
        got.presentation,
        got.pane.as_str(),
        got.window.as_str(),
    );
    assert_eq!(fields, (Some(ID), None, "", ""), "server gone"); // PM call 27: pane and window both empty
    assert!(w.fakes.mux_calls().is_empty(), "no multiplexer call");
    let (w, r, folder) = lifecycle_worker(ok(), ok());
    std::fs::remove_file(folder.join("view.json")).unwrap();
    proof(&w, &r);
    let got = caller::resolve(&w.ctx).expect("proven without a view");
    let fields = (
        got.member.as_deref(),
        got.presentation,
        got.pane.as_str(),
        got.window.as_str(),
    );
    assert_eq!(fields, (Some(ID), None, "", ""), "no view"); // PM call 27
}

// K2 is for lifecycle members only: a pre-lifecycle member still needs its live server.
#[test]
fn a_pre_lifecycle_member_still_needs_its_server() {
    let (w, _c, r, folder) = worker();
    kill_server(&w, &folder);
    proof(&w, &r);
    assert_eq!(caller::hc::resolve(&w.ctx).unwrap_err().check, "herdr");
}

// Ambiguity 11: only liveness is dropped; server.json must still hash to the key.
#[test]
fn a_server_json_of_another_key_proves_nothing() {
    let (w, r, folder) = lifecycle_worker(ok(), ok());
    let at = folder
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("server.json");
    let mut s = crate::state::fs::read_json(&at).unwrap().unwrap();
    s["serverStart"] = json!(s["serverStart"].as_i64().unwrap() + 1);
    crate::state::fs::write_json(&at, &s).unwrap();
    kill_server(&w, &folder);
    proof(&w, &r);
    assert_eq!(caller::hc::resolve(&w.ctx).unwrap_err().check, "herdr");
}

// K2 through the real CLI: with no server and no view, status, report and read run. (`focus`'s `views` refusal comes
// with the dispatch call, Task 6; member-to-member mail is the live test's, Task 7.)
#[test]
fn a_member_without_its_multiplexer_runs_status_report_and_read() {
    for (args, field, want) in [
        (
            vec!["status", "-BodyFile", "body.txt"],
            "text",
            json!("hello"),
        ),
        (
            vec!["report", "blocked", "-Kind", "question"],
            "state",
            json!("blocked"),
        ),
        (vec!["read"], "ids", json!([])),
    ] {
        let (w, r, folder) = lifecycle_worker(ok(), ok());
        std::fs::remove_file(folder.join("view.json")).unwrap();
        kill_server(&w, &folder);
        w.file("body.txt", b"hello");
        let (team, address) = (
            r["team"].as_str().unwrap(),
            r["hcAddress"].as_str().unwrap(),
        );
        proof(&w, &r);
        proof(&w, &r);
        // As in hc_worker_commands_keep_mail_status_activity_and_spawn_fence: the verb re-checks the member in hc.
        {
            cf::online(&w);
            cf::listing(&w, team, &[address]);
            cf::get(&w, address, wire(&r));
        }
        if args[0] == "read" {
            cf::online(&w);
            cf::get(&w, address, wire(&r));
        }
        let (code, out) = crate::cli::run(
            &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            || Ok(&w.ctx),
        );
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(code, 0, "{args:?}: {out}");
        assert_eq!(v[field], want, "{args:?}: {out}");
    }
}

// 4to6 #18: a server.json that is not a regular file is never opened: a folder, or (Unix) a FIFO, proves nothing at
// once. The FIFO case frees a reader blocked in open after 5 s, so a regression fails instead of hanging the suite.
#[test]
fn a_server_json_of_the_wrong_type_proves_nothing_at_once() {
    let (w, r, folder) = lifecycle_worker(ok(), ok());
    let at = folder
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("server.json");
    std::fs::remove_file(&at).unwrap();
    std::fs::create_dir(&at).unwrap();
    proof(&w, &r);
    assert_eq!(
        caller::hc::resolve(&w.ctx).unwrap_err().check,
        "herdr",
        "a folder"
    );
    #[cfg(unix)]
    {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        use std::time::{Duration, Instant};
        let (w, r, folder) = lifecycle_worker(ok(), ok());
        let at = folder
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("server.json");
        std::fs::remove_file(&at).unwrap();
        let c = std::ffi::CString::new(at.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path; mkfifo only creates the node.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo");
        let done = Arc::new(AtomicBool::new(false));
        let (d, fifo) = (done.clone(), at.clone());
        let rescue = std::thread::spawn(move || {
            let end = Instant::now() + Duration::from_secs(5);
            while !d.load(Ordering::SeqCst) {
                if Instant::now() >= end {
                    let _ = std::fs::OpenOptions::new().write(true).open(&fifo);
                    return true;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            false
        });
        proof(&w, &r);
        let got = caller::hc::resolve(&w.ctx);
        done.store(true, Ordering::SeqCst);
        assert!(
            !rescue.join().unwrap(),
            "resolve blocked on a FIFO server.json"
        );
        assert_eq!(got.unwrap_err().check, "herdr", "a FIFO");
    }
}

// Ambiguity 12: a terminal labelled with another generation of this member is never this member, nor a candidate.
#[test]
fn a_terminal_of_another_generation_is_never_this_member() {
    let (w, r, _) = lifecycle_worker(ok(), ok());
    cf::online(&w);
    let mut t = wire(&r);
    t["labels"]["mux.gen"] = json!("0");
    cf::get(&w, r["hcAddress"].as_str().unwrap(), t);
    assert_eq!(caller::hc::resolve(&w.ctx).unwrap_err().check, "herdr");
}

// K2: hook reports are never read by member proof.
#[test]
fn member_proof_reads_no_hook_report() {
    let (w, r, folder) = lifecycle_worker(ok(), ok());
    for f in ["status.json", "activity.json"] {
        crate::state::fs::write_json(&folder.join(f), &json!({"state": "idle"})).unwrap();
    }
    kill_server(&w, &folder);
    proof(&w, &r);
    let (got, reads) = crate::state::fs::record_reads(|| caller::resolve(&w.ctx));
    assert_eq!(got.unwrap().member.as_deref(), Some(ID));
    for p in &reads {
        assert!(
            p.file_name()
                .is_none_or(|n| n != "status.json" && n != "activity.json"),
            "{p:?}"
        );
    }
}

// Checklist 14, PM call 31: a held member (active generation 1 committed, pending generation 2 not done) with an
// active view and a pending view is presented by the pending view only; with its server gone, by none.
#[test]
fn a_held_member_is_presented_by_its_pending_view() {
    let (w, mut r, folder) = lifecycle_worker(ok(), ok());
    let active_view = crate::state::fs::read_json(&folder.join("view.json"))
        .unwrap()
        .unwrap();
    assert_eq!(
        active_view["gen"], 1,
        "the fixture's view is the active one"
    );
    r["pending"] = json!({"gen": 2, "kind": "resume", "hcName": "m4567cdef",
        "hcAddress": "t2", "sessionId": r["sessionId"]});
    crate::state::fs::write_json(&folder.join("record.json"), &r).unwrap();
    let pending_view = json!({"pane": "%7", "window": "@7", "startCommand": r["startCommand"], "gen": 2,
        "hcAddress": "t2", "hcName": "m4567cdef"});
    crate::state::fs::write_json(&folder.join("view.json"), &pending_view).unwrap();
    proof(&w, &r);
    let got = caller::resolve(&w.ctx).expect("a held member resolves");
    let fields = (
        got.member.as_deref(),
        got.presentation.clone(),
        got.pane.as_str(),
        got.window.as_str(),
    );
    let pending = Some(("%7".to_string(), "@7".to_string()));
    assert_eq!(
        fields,
        (Some(ID), pending, "%7", "@7"),
        "the pending view presents"
    );
    // The active generation's view no longer presents the held member.
    crate::state::fs::write_json(&folder.join("view.json"), &active_view).unwrap();
    proof(&w, &r);
    let got = caller::resolve(&w.ctx).expect("a held member resolves without its view");
    assert_eq!(
        (got.presentation, got.pane.as_str(), got.window.as_str()),
        (None, "", ""),
        "the active view is not the held member's"
    );
    crate::state::fs::write_json(&folder.join("view.json"), &pending_view).unwrap();
    kill_server(&w, &folder);
    proof(&w, &r);
    let got = caller::resolve(&w.ctx).expect("a held member resolves without its server");
    assert_eq!(got.presentation, None, "server gone");
}
