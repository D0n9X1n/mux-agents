use crate::{
    agents::{
        core_fixture as cf,
        fixture::{ID, World},
    },
    caller,
    ctx::Env,
};
use serde_json::{Value, json};
fn worker() -> (World, caller::Caller, Value, std::path::PathBuf) {
    let mut w = World::agent("");
    let c = w.caller();
    let r = cf::member(
        &w,
        ID,
        json!({"startCommand":crate::agents::fixture::launch_command("%9")}),
    );
    let folder = w.folder(ID);
    cf::proven(&w, ID, 31337, 150);
    crate::state::fs::write_json(
        &w.state().join("server.json"),
        &json!({"socket":c.socket,"serverPid":c.server_pid,"serverStart":c.server_start}),
    )
    .unwrap();
    w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker", "%1|@1|100||"]);
    w.recipients_live();
    w.fakes.forget(150);
    w.fakes.process(crate::testing::row(150, 31337, 1500));
    w.fakes.process(crate::testing::row(31337, 1, 1000));
    w.ctx.env = Env::from_pairs([("HC_TERMINAL_ID", r["hcAddress"].as_str().unwrap())]);
    w.fakes.clear_log();
    (w, c, r, folder)
}
fn wire(r: &Value) -> Value {
    let mut t = cf::wire(r);
    t["pid"] = 31337.into();
    t["foreground"] = json!({"argv": ["claude"], "pid": 150});
    t
}
fn proof(w: &World, r: &Value) {
    cf::online(w);
    cf::get(w, r["hcAddress"].as_str().unwrap(), wire(r));
}
#[test]
fn hc_worker_routes_without_mux_ancestry() {
    let (w, c, r, _) = worker();
    proof(&w, &r);
    let got = caller::resolve(&w.ctx).expect("managed hc worker must resolve without TMUX");
    assert_eq!(
        (
            got.kind,
            got.pane.as_str(),
            got.window.as_str(),
            got.agent_tag.as_str()
        ),
        (caller::Kind::Agent, "%9", "@5", ID)
    );
    assert_eq!(
        (
            got.socket,
            got.server_pid,
            got.server_start,
            got.server_key,
            got.session
        ),
        (
            c.socket,
            c.server_pid,
            c.server_start,
            c.server_key,
            c.session
        )
    );
    assert_eq!(
        got.chain.iter().map(|r| r.pid).collect::<Vec<_>>(),
        [4242, 170, 150, 31337]
    );
    assert!(got.agent.is_none());
    assert!(w.fakes.mux_calls().is_empty());
    w.fakes.hc_done().unwrap();
}
#[test]
fn hc_worker_rejects_unproven_identity_before_mux() {
    for case in [
        "managed",
        "team",
        "id",
        "gen",
        "address",
        "argv",
        "finished",
        "missing-record",
        "uncommitted",
        "aborted",
        "record-team",
        "record-session",
        "record-generation",
        "record-id",
        "record-address",
        "record-client",
        "record-pane",
        "record-window",
        "unsafe-team",
        "unsafe-id",
        "server-missing",
        "server-start",
        "server-key",
        "server-socket",
        "get-error",
        "get-json",
        "version",
    ] {
        let (w, _, r, folder) = worker();
        let mut t = wire(&r);
        let mut record = r.clone();
        match case {
            "managed" => t["labels"]["mux.managed"] = "0".into(),
            "team" => t["labels"]["mux.team"] = "0000000000000000:$9".into(),
            "id" => t["labels"]["mux.id"] = "bbbbbbbbbbbb".into(),
            "gen" => t["labels"]["mux.gen"] = "1".into(),
            "address" => t["address"] = "t-other".into(),
            "argv" => t["argv"] = json!(["/bin/sh", "-l"]),
            "finished" => t["state"] = "finished".into(),
            "uncommitted" => std::fs::remove_file(folder.join("commit.json")).unwrap(),
            "aborted" => {
                crate::state::fs::write_json(&folder.join("commit.json"), &json!({"aborted":true}))
                    .unwrap()
            }
            "record-team" => record["team"] = "0000000000000000:$3".into(),
            "record-session" => record["session"] = "$4".into(),
            "record-generation" => record["generation"] = "1".into(),
            "record-id" => record["id"] = "bbbbbbbbbbbb".into(),
            "record-address" => record["hcAddress"] = "t-other".into(),
            "record-client" => record["client"] = "unknown".into(),
            "record-pane" => record["pane"] = "%bad".into(),
            "record-window" => record["window"] = "@bad".into(),
            "unsafe-team" => t["labels"]["mux.team"] = "../../outside:$3".into(),
            "unsafe-id" => t["labels"]["mux.id"] = "../outside".into(),
            "server-missing" => std::fs::remove_file(
                folder
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("server.json"),
            )
            .unwrap(),
            "server-start" => w.fakes.opens(900, &[Some(2)]),
            "server-key" | "server-socket" => {
                let p = folder
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("server.json");
                let mut v = crate::state::fs::read_json(&p).unwrap().unwrap();
                if case == "server-key" {
                    v["serverStart"] = 2.into();
                    w.fakes.opens(900, &[Some(2)]);
                } else {
                    v["socket"] = "relative".into();
                }
                crate::state::fs::write_json(&p, &v).unwrap();
            }
            "missing-record" | "get-error" | "get-json" | "version" => (),
            _ => unreachable!(),
        }
        crate::state::fs::write_json(&folder.join("record.json"), &record).unwrap();
        if case == "missing-record" {
            std::fs::remove_file(folder.join("record.json")).unwrap();
        }
        let a = r["hcAddress"].as_str().unwrap();
        if case == "version" {
            w.fakes.hc_install("hc");
            cf::answer(&w, &["--version"], "hc 0.1.20");
        } else {
            cf::online(&w);
            match case {
                "get-error" => w.fakes.hc_expect(
                    &["get", a, "--json"],
                    Err(crate::error::Refusal::internal("offline")),
                ),
                "get-json" => cf::answer(&w, &["get", a, "--json"], "{}"),
                _ => cf::get(&w, a, t),
            }
        }
        let e = caller::resolve(&w.ctx).unwrap_err();
        assert_eq!((e.code, e.check), (2, "herdr"), "{case}: {e:?}");
        assert!(w.fakes.mux_calls().is_empty(), "{case}");
        w.fakes.hc_done().unwrap();
        for f in ["status.json", "activity.json", "last-sent.json"] {
            assert!(!folder.join(f).exists(), "{case}: {f}");
        }
    }
}
#[test]
fn hc_worker_commands_keep_mail_status_activity_and_spawn_fence() {
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
        (
            vec!["send", "parent", "-BodyFile", "body.txt"],
            "from",
            json!("worker"),
        ),
        (vec!["read"], "messages", json!([])),
        (
            vec!["spawn", "-Client", "claude", "-TaskFile", "body.txt"],
            "check",
            json!("tagged"),
        ),
    ] {
        let (w, c, r, folder) = worker();
        w.file("body.txt", b"hello");
        proof(&w, &r);
        proof(&w, &r);
        // Every command but spawn re-checks the proven member in hc before acting as it (#104 phase 1c).
        if args[0] != "spawn" {
            cf::online(&w);
            cf::listing(
                &w,
                &crate::agents::membership::team(&c),
                &[r["hcAddress"].as_str().unwrap()],
            );
            cf::get(&w, r["hcAddress"].as_str().unwrap(), wire(&r));
        }
        // read's fence looks at the member again under its mailbox lock.
        if args[0] == "read" {
            cf::online(&w);
            cf::get(&w, r["hcAddress"].as_str().unwrap(), wire(&r));
        }
        let (code, out) = crate::cli::run(
            &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            || Ok(&w.ctx),
        );
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(code, if args[0] == "spawn" { 2 } else { 0 }, "{out}");
        assert_eq!(v[field], want, "{out}");
        if args[0] == "status" {
            assert_eq!(
                crate::state::fs::read_json(&folder.join("status.json"))
                    .unwrap()
                    .unwrap()["text"],
                "hello"
            );
        }
        if args[0] == "report" {
            assert_eq!(
                crate::state::fs::read_json(&folder.join("activity.json"))
                    .unwrap()
                    .unwrap()["kind"],
                "question"
            );
        }
        w.fakes.hc_done().unwrap();
    }
}
#[test]
fn hc_worker_bypass_and_sandbox_boundaries() {
    for (key, value) in [
        ("TMUX", "present"),
        ("RMUX", "present"),
        ("HC_TERMINAL_ID", ""),
    ] {
        let f = crate::testing::Fakes::default();
        let mut c = f.ctx();
        c.env = Env::from_pairs([("HC_TERMINAL_ID", "t1"), (key, value)]);
        if key == "HC_TERMINAL_ID" {
            c.env = Env::from_pairs([(key, value)]);
        }
        assert!(!caller::hc::applies(&c));
        assert!(f.hc_calls().is_empty());
        f.hc_done().unwrap();
    }
    let (mut w, _, r, _) = worker();
    proof(&w, &r);
    w.ctx.env = Env::from_pairs([
        ("HC_TERMINAL_ID", r["hcAddress"].as_str().unwrap()),
        ("SANDBOX_RUNTIME", "1"),
    ]);
    let (code, out) = crate::cli::run(&["report".into(), "idle".into()], || Ok(&w.ctx));
    assert_eq!(code, 2);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["check"],
        "sandbox"
    );
    assert!(w.fakes.mux_calls().is_empty());
    w.fakes.hc_done().unwrap();
    let (w, _, _, _) = worker();
    let e = caller::resolve(&w.ctx).unwrap_err();
    assert_eq!((e.code, e.check), (2, "herdr"));
    w.fakes.hc_done().unwrap();
}
#[test]
fn hc_worker_revalidates_before_writing() {
    let (w, _, r, folder) = worker();
    w.file("body.txt", b"hello");
    proof(&w, &r);
    cf::online(&w);
    let mut changed = wire(&r);
    changed["labels"]["mux.gen"] = "changed".into();
    cf::get(&w, r["hcAddress"].as_str().unwrap(), changed);
    let (code, out) = crate::cli::run(
        &["status".into(), "-BodyFile".into(), "body.txt".into()],
        || Ok(&w.ctx),
    );
    assert_eq!(code, 2, "{out}");
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["check"],
        "herdr"
    );
    assert!(!folder.join("status.json").exists());
    assert!(w.fakes.mux_calls().is_empty());
    w.fakes.hc_done().unwrap();
}
/// `p` with every link resolved: its nearest existing ancestor's canonical path, then the rest as named. None when
/// the rest climbs (`..`), so where it lands is not known.
fn real(p: &std::path::Path) -> Option<std::path::PathBuf> {
    let parts: Vec<_> = p.components().collect();
    (0..=parts.len()).rev().find_map(|n| {
        let at: std::path::PathBuf = parts[..n].iter().collect();
        let real = std::fs::canonicalize(&at).ok()?;
        Some(
            parts[n..]
                .iter()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
                .then(|| parts[n..].iter().fold(real, |acc, c| acc.join(c))),
        )
    })?
}
/// Makes `at` a link to `target`, a folder or a file.
fn link(target: &std::path::Path, at: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    return std::os::unix::fs::symlink(target, at);
    #[cfg(windows)]
    return if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, at)
    } else {
        std::os::windows::fs::symlink_file(target, at)
    };
}
// Whatever its hc terminal's labels name, and wherever a link in the state root points, the worker proof opens no
// file outside the canonical state root (#104 phase 1c).
#[test]
fn hc_worker_proof_reads_nothing_outside_the_state_root() {
    // The seam sees the proof's reads: a proven worker opens its commit, its record and its server's file, then the
    // member proof (D1, #103) its launcher.json and spec.json; the record is read once.
    let (w, _, r, _) = worker();
    proof(&w, &r);
    let (got, reads) = crate::state::fs::record_reads(|| caller::resolve(&w.ctx));
    got.unwrap();
    let root = std::fs::canonicalize(&w.ctx.state_root).unwrap();
    let names: Vec<_> = reads
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        [
            "commit.json",
            "record.json",
            "server.json",
            "launcher.json",
            "spec.json"
        ]
    );
    for p in &reads {
        assert!(real(p).is_some_and(|p| p.starts_with(&root)), "{p:?}");
    }
    w.fakes.hc_done().unwrap();
    for case in [
        "traversal-team",
        "traversal-id",
        "absolute-team",
        "absolute-id",
        "linked-team",
        "linked-member",
        "linked-commit",
        "linked-record",
        "linked-server",
        "linked-launcher",
        "linked-spec",
    ] {
        let (w, _, r, folder) = worker();
        let outside = w._dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let team = folder.parent().unwrap().parent().unwrap().to_path_buf();
        let mut t = wire(&r);
        // Moves `at` outside the state root and leaves a link to it in its place: the proof would succeed through
        // the link, reading the moved files.
        let moved = |at: &std::path::Path| {
            let to = outside.join(at.file_name().unwrap());
            std::fs::rename(at, &to).unwrap();
            link(&to, at)
        };
        let made = match case {
            "traversal-team" => {
                t["labels"]["mux.team"] = "../../outside:$3".into();
                Ok(())
            }
            "traversal-id" => {
                t["labels"]["mux.id"] = "../outside".into();
                Ok(())
            }
            "absolute-team" => {
                t["labels"]["mux.team"] = format!("{}:$3", outside.display()).into();
                Ok(())
            }
            "absolute-id" => {
                t["labels"]["mux.id"] = outside.display().to_string().into();
                Ok(())
            }
            "linked-team" => moved(&team),
            "linked-member" => moved(&folder),
            "linked-commit" => moved(&folder.join("commit.json")),
            "linked-record" => moved(&folder.join("record.json")),
            "linked-server" => moved(&team.join("server.json")),
            "linked-launcher" => moved(&folder.join("launcher.json")),
            "linked-spec" => moved(&folder.join("spec.json")),
            _ => unreachable!(),
        };
        if let Err(e) = made {
            // Windows makes a link only with the privilege or developer mode.
            eprintln!("{case}: skipped, no link: {e}");
            continue;
        }
        cf::online(&w);
        cf::get(&w, r["hcAddress"].as_str().unwrap(), t);
        let (got, reads) = crate::state::fs::record_reads(|| caller::resolve(&w.ctx));
        let root = std::fs::canonicalize(&w.ctx.state_root).unwrap();
        for p in &reads {
            assert!(
                real(p).is_some_and(|p| p.starts_with(&root)),
                "{case}: read {p:?}, outside {root:?}"
            );
        }
        let e = got.unwrap_err();
        // The member proof's files: the terminal identity holds, so a link there leaves a candidate, refused 2 `tagged`.
        let check = if case == "linked-launcher" || case == "linked-spec" {
            "tagged"
        } else {
            "herdr"
        };
        assert_eq!((e.code, e.check), (2, check), "{case}: {e:?}");
        assert!(w.fakes.mux_calls().is_empty(), "{case}");
        w.fakes.hc_done().unwrap();
    }
}
/// A terminal identity whose proof fails is a candidate: never `member`, and `caller::resolve` refuses it.
#[test]
fn a_failed_proof_is_a_candidate() {
    for case in [
        "unrelated",
        "missing-pid",
        "reused-pid",
        "broken-chain",
        "no-foreground",
        "nested",
    ] {
        let (w, _, r, _) = worker();
        let mut t = wire(&r);
        match case {
            "unrelated" => {
                t["pid"] = 31338.into();
                w.fakes.process(crate::testing::row(31338, 1, 900));
            }
            "missing-pid" => {
                t.as_object_mut().unwrap().remove("pid");
            }
            "reused-pid" => {
                w.fakes.forget(31337);
                w.fakes.process(crate::testing::row(31337, 1, 1600));
            }
            "broken-chain" => w.fakes.forget(170),
            "no-foreground" => {
                t.as_object_mut().unwrap().remove("foreground");
            }
            _ => nest(&w),
        }
        for _ in 0..2 {
            cf::online(&w);
            cf::get(&w, r["hcAddress"].as_str().unwrap(), t.clone());
        }
        let got = caller::resolve_tree(&w.ctx).expect(case);
        assert_eq!(
            (got.member, got.candidate.as_deref()),
            (None, Some(ID)),
            "{case}"
        );
        assert_eq!(
            caller::resolve(&w.ctx).unwrap_err().check,
            "tagged",
            "{case}"
        );
        w.fakes.hc_done().unwrap();
    }
}
/// A second client (160) between the shell and C: the walk to L succeeds, the proof does not.
fn nest(w: &World) {
    w.fakes.forget(170);
    w.fakes.process(crate::testing::row(170, 160, 1700));
    w.fakes.process(crate::testing::row(160, 150, 1600));
    w.fakes.client(160, crate::parse::Client::Claude);
}
/// Through the CLI: a nested client, and ancestry that becomes unreadable between dispatch and handler, neither writes
/// as the member nor reads its mailbox; a foreign herdr terminal stays 2 `herdr`.
#[test]
fn a_candidate_never_acts_as_a_member() {
    for (case, verb) in [
        ("nested", "report"),
        ("nested", "read"),
        ("race", "report"),
        ("race", "read"),
        ("foreign", "read"),
    ] {
        let (w, c, r, folder) = worker();
        crate::agents::mail::add_mail(&w.ctx, &c, ID, "root", "%1", "hello", None).unwrap();
        let (a, mut t) = (r["hcAddress"].as_str().unwrap().to_string(), wire(&r));
        match case {
            "nested" => nest(&w),
            "foreign" => t["labels"]["mux.managed"] = "0".into(),
            _ => {
                cf::online(&w);
                cf::get(&w, &a, t.clone());
                t["pid"] = 31338.into();
                w.fakes.process(crate::testing::row(31338, 1, 900));
            }
        }
        cf::online(&w);
        cf::get(&w, &a, t);
        let args: Vec<String> = if verb == "report" {
            vec!["report".into(), "blocked".into()]
        } else {
            vec!["read".into()]
        };
        let (code, out) = crate::cli::run(&args, || Ok(&w.ctx));
        let check = if case == "foreign" { "herdr" } else { "tagged" };
        assert_eq!(
            (code, out.contains(&format!(r#""check":"{check}""#))),
            (2, true),
            "{case} {verb}: {out}"
        );
        assert!(
            !folder.join("activity.json").exists(),
            "{case} {verb}: no member write"
        );
        assert_eq!(
            crate::agents::mail::read_mail(&w.ctx, &c, ID)
                .unwrap()
                .len(),
            1,
            "{case} {verb}: mailbox unread"
        );
        w.fakes.hc_done().unwrap();
    }
}
