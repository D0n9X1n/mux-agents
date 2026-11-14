use super::*;
use crate::agents::{
    core_fixture as cf,
    core_identity::Identity,
    fixture::{ID, SESSION_ID, World, write},
    lock_key,
    mail::{add_mail, member_mailbox, read_mail, read_mail_if},
    membership,
    message::current,
    spawn::{client_flags, client_named},
    text,
};
use crate::spawn::Out;
use crate::state::fs::read_json;
use serde_json::json;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

const Y: &str = "bbbbbbbbbbbb";
const OTHER_SESSION: &str = "11111111-2222-4333-8444-555555555555";

/// A committed member whose launch spec records its client's flags.
fn member(w: &World, id: &str, fields: Value) -> Value {
    let r = cf::member(w, id, fields);
    let client = client_named(r["client"].as_str().unwrap()).unwrap();
    write(
        &w.folder(id).join("spec.json"),
        json!({"schema": 1, "id": id, "client": r["client"], "flags": client_flags(client),
            "args": ["--session-id", r["sessionId"]], "generation": "0"}),
    );
    r
}

/// The member's terminal as hc shows it: running, lost, unknown, stopped (killed) or exited.
fn wire(r: &Value, life: &str) -> Value {
    let mut t = cf::wire(r);
    match life {
        "running" => {}
        "lost" => t["argv"] = json!(["/bin/sh", "-l"]),
        "unknown" => t["labels"]["mux.gen"] = json!("1"),
        "stopped" => {
            t["state"] = json!("finished");
            t["reason"] = json!("killed");
        }
        _ => {
            t["state"] = json!("finished");
            t["reason"] = json!("exited");
            t["exit_code"] = json!(0);
        }
    }
    t
}

/// hc as resume's snapshot reads it: connected, then each member's terminal in address order.
fn snapshot(w: &World, members: &[(&Value, &str)]) {
    cf::online(w);
    cf::listing(w, &membership::team(&w.caller()), &[]);
    let mut members = members.to_vec();
    members.sort_by_key(|(r, _)| r["hcAddress"].as_str().unwrap().to_string());
    for (r, life) in members {
        cf::get(w, r["hcAddress"].as_str().unwrap(), wire(r, life));
    }
}

/// hc as an admitted resume reads it: the snapshot. The session check asks hc only about a record that carries X's
/// session (none here).
fn facts(w: &World, members: &[(&Value, &str)]) {
    snapshot(w, members);
}

/// The launch after the checks: hc run answers `address`, and the new view attaches as %9.
fn launches(w: &World, address: &str) {
    w.fakes.hc_answer(Ok(Out {
        stdout: format!(r#"{{"address":"{address}"}}"#),
        ..Out::default()
    }));
    for verb in ["new-window", "split-window"] {
        w.fakes.mux_rule(verb, 0, "%9|@5|exact,#} attach t1\n", "");
    }
    w.launches_hold();
    w.fakes
        .mux_rule("-t %9 #{pane_id}|#{@agent-id}", 0, "|\n", "");
}

fn resume_of(w: &World, target: &str) -> Res<Map<String, Value>> {
    resume(&w.ctx, &w.request(&["resume", target]))
}

fn folders(w: &World) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(w.state().join("agents"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn new_folder(w: &World, before: &[String]) -> PathBuf {
    let new: Vec<_> = folders(w)
        .into_iter()
        .filter(|f| !before.contains(f))
        .collect();
    assert_eq!(new.len(), 1, "{new:?}");
    w.folder(&new[0])
}

/// Every file under `folder` with its bytes.
fn files(folder: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut all = BTreeMap::new();
    for entry in std::fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            for (name, bytes) in files(&path) {
                all.insert(format!("{}/{name}", path.display()), bytes);
            }
        } else {
            all.insert(path.display().to_string(), std::fs::read(&path).unwrap());
        }
    }
    all
}

fn runs(w: &World) -> usize {
    w.fakes
        .hc_calls()
        .iter()
        .filter(|(_, a, _)| a[0] == "run")
        .count()
}

fn record_of(w: &World, id: &str) -> Value {
    read_json(&w.folder(id).join("record.json"))
        .unwrap()
        .unwrap()
}

// Each client, ended by exit or by a stop: Y runs `<client> <X's recorded flags> --resume <X's session>` with no
// task, in X's directory, keeps X's name, session, parent and depth, and X's files are never written.
#[test]
fn resume_starts_an_ended_member_again_with_its_conversation() {
    for client in ["claude", "copilot"] {
        for how in ["exited", "stopped"] {
            let w = World::human("");
            let x = member(
                &w,
                ID,
                json!({"client": client, "pane": "%5", "window": "@1", "depth": 1}),
            );
            let kept = files(&w.folder(ID));
            let before = folders(&w);
            w.panes(&[]);
            facts(&w, &[(&x, how)]);
            launches(&w, "t1");
            let mut reply = resume_of(&w, ID).unwrap();
            // The bar's fields are spawn's, tested by `resume_reply_carries_the_bar_as_spawn_does`.
            assert!(reply.remove("sidebar").is_some(), "{client} {how}");
            reply.remove("sidebarError");
            let y = new_folder(&w, &before);
            let id = y.file_name().unwrap().to_str().unwrap().to_string();
            assert_eq!(
                Value::Object(reply),
                json!({"from": ID, "agent": {"id": id, "name": "worker", "hcAddress": "t1"}}),
                "{client} {how}"
            );
            let spec = read_json(&y.join("spec.json")).unwrap().unwrap();
            assert_eq!(
                spec["flags"],
                json!(client_flags(client_named(client).unwrap()))
            );
            assert_eq!(spec["args"], json!(["--resume", SESSION_ID]));
            assert!(spec.get("prompt").is_none());
            let run = w
                .fakes
                .hc_calls()
                .into_iter()
                .map(|(_, a, _)| a)
                .find(|a| a[0] == "run")
                .unwrap();
            let cwd = run.iter().position(|a| a == "--cwd").unwrap();
            assert_eq!(run[cwd + 1], "/work");
            let record = record_of(&w, &id);
            for (k, v) in [
                ("name", json!("worker")),
                ("client", json!(client)),
                ("sessionId", json!(SESSION_ID)),
                ("parentPane", json!("%1")),
                ("parentAgent", Value::Null),
                ("depth", json!(1)),
                ("directory", json!("/work")),
                ("resumedFrom", json!(ID)),
                ("mailbox", json!(ID)),
            ] {
                assert_eq!(record[k], v, "{client} {how} {k}");
            }
            assert!(crate::agents::committed(&y));
            assert_eq!(files(&w.folder(ID)), kept, "{client} {how}");
            w.fakes.hc_done().unwrap();
        }
    }
}

// Every refusal launches nothing and makes no folder.
#[test]
fn resume_refuses_and_launches_nothing() {
    for case in [
        "agent", "running", "lost", "unknown", "missing", "taken", "name", "session", "leader",
        "ceiling", "cap",
    ] {
        let w = if case == "agent" {
            World::agent("")
        } else {
            World::human("")
        };
        let depth = if case == "leader" { 0 } else { 1 };
        let x = member(
            &w,
            ID,
            json!({"pane": "%5", "window": "@1", "depth": depth}),
        );
        let life = if ["running", "lost", "unknown"].contains(&case) {
            case
        } else {
            "exited"
        };
        let mut terminals = vec![(x, life.to_string())];
        let mut add = |id: &str, fields: Value, life: &str| {
            terminals.push((member(&w, id, fields), life.to_string()));
        };
        match case {
            "taken" => add(
                Y,
                json!({"pane": "%6", "resumedFrom": ID, "mailbox": ID}),
                "exited",
            ),
            "name" => add(
                Y,
                json!({"pane": "%6", "sessionId": OTHER_SESSION}),
                "running",
            ),
            "session" => add(Y, json!({"name": "other", "pane": "%6"}), "running"),
            "cap" => {
                for n in 1..=6 {
                    add(
                        &n.to_string().repeat(12),
                        json!({"name": format!("w{n}"), "pane": format!("%{}", 20 + n),
                            "sessionId": OTHER_SESSION}),
                        "running",
                    );
                }
            }
            "ceiling" => {
                let file = super::super::team::path(&w.state(), "$3");
                let team = super::super::team::Team {
                    team: membership::team(&w.caller()),
                    session: "$3".into(),
                    directory: "/work".into(),
                    client: crate::parse::Client::Claude,
                    flags: Vec::new(),
                    policy: super::super::team::Policy::Default,
                    leader: Y.into(),
                    created_at: "x".into(),
                };
                assert!(super::super::team::create(&file, &team).unwrap());
            }
            _ => {}
        }
        w.panes(&[]);
        if case != "agent" {
            let refs: Vec<_> = terminals.iter().map(|(r, l)| (r, l.as_str())).collect();
            snapshot(&w, &refs);
            // The session check asks hc about the one other record that carries X's session: Y, running.
            if case == "session" {
                let (y, life) = &terminals[1];
                cf::get(&w, y["hcAddress"].as_str().unwrap(), wire(y, life));
            }
        }
        let before = folders(&w);
        let target = if case == "missing" { "nobody" } else { ID };
        let e = resume_of(&w, target).unwrap_err();
        let want = match case {
            "agent" => "tagged",
            "running" | "lost" | "unknown" => "life",
            "missing" => "target",
            other => other,
        };
        assert_eq!((e.code, e.check), (2, want), "{case}: {}", e.message);
        assert_eq!(folders(&w), before, "{case}");
        assert_eq!(runs(&w), 0, "{case}");
        w.fakes.hc_done().unwrap();
    }
}

// Two resumes of one member launch one, whether the first successor still runs or has ended.
#[test]
fn two_resumes_of_a_member_launch_one() {
    for second in ["running", "exited"] {
        let w = World::human("");
        let x = member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
        w.panes(&[]);
        facts(&w, &[(&x, "exited")]);
        launches(&w, "t1");
        let first = resume_of(&w, ID).unwrap();
        let y = record_of(&w, first["agent"]["id"].as_str().unwrap());
        snapshot(&w, &[(&x, "exited"), (&y, second)]);
        let before = folders(&w);
        let e = resume_of(&w, ID).unwrap_err();
        assert_eq!((e.code, e.check), (2, "taken"), "{second}");
        assert_eq!(folders(&w), before);
        assert_eq!(runs(&w), 1);
        w.fakes.hc_done().unwrap();
    }
}

// Power loss after the successor's launcher claimed its run but before its commit: the claim alone takes X.
#[test]
fn a_kept_claim_without_its_commit_still_takes_the_member() {
    let w = World::human("");
    let x = member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
    w.panes(&[]);
    facts(&w, &[(&x, "exited")]);
    launches(&w, "t1");
    let id = resume_of(&w, ID).unwrap()["agent"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let folder = w.folder(&id);
    std::fs::remove_file(folder.join("commit.json")).unwrap();
    std::fs::create_dir(folder.join("gens")).unwrap();
    std::fs::write(folder.join("gens/0.claim"), "\"run\"").unwrap();
    snapshot(&w, &[(&x, "exited"), (&record_of(&w, &id), "exited")]);
    let e = resume_of(&w, ID).unwrap_err();
    assert_eq!((e.code, e.check), (2, "taken"));
    assert_eq!(runs(&w), 1);
    w.fakes.hc_done().unwrap();
}

// A failed flush of the successor's record before hc run launches nothing and leaves abort evidence; nothing ran,
// so X is not taken and a later resume launches.
#[test]
fn a_failed_flush_before_the_run_launches_nothing() {
    let w = World::human("");
    let x = member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
    w.panes(&[]);
    facts(&w, &[(&x, "exited")]);
    let before = folders(&w);
    crate::state::fs::fail_durable_flush(Some(0));
    let got = resume_of(&w, ID);
    crate::state::fs::fail_durable_flush(None);
    assert!(got.is_err());
    assert_eq!(runs(&w), 0);
    assert!(new_folder(&w, &before).join("abort.json").exists());
    w.fakes.hc_done().unwrap();
    facts(&w, &[(&x, "exited")]);
    launches(&w, "t1");
    assert_eq!(resume_of(&w, ID).unwrap()["from"], ID);
    assert_eq!(runs(&w), 1);
    w.fakes.hc_done().unwrap();
}

// A successor that claimed its run, failed before its commit and ended is the member its name means: `resume
// worker` and `resume <its id>` both resume it, with no task, keeping the first mailbox.
#[test]
fn an_aborted_successor_is_resumed_by_name_or_id() {
    for target in ["worker", Y] {
        let w = World::human("");
        let x = member(
            &w,
            ID,
            json!({"pane": "%5", "window": "@1", "depth": 1, "createdAt": "2026-11-13T10:00:00Z"}),
        );
        let y = member(
            &w,
            Y,
            json!({"pane": "%6", "window": "@1", "depth": 1, "resumedFrom": ID, "mailbox": ID,
                "createdAt": "2026-11-13T10:01:00Z"}),
        );
        let folder = w.folder(Y);
        std::fs::remove_file(folder.join("commit.json")).unwrap();
        std::fs::create_dir(folder.join("gens")).unwrap();
        std::fs::write(folder.join("gens/0.claim"), "\"run\"").unwrap();
        write(
            &folder.join("abort.json"),
            json!({"hcAddress": "tbbbbbbbbbbbb"}),
        );
        w.panes(&[]);
        facts(&w, &[(&x, "exited"), (&y, "stopped")]);
        launches(&w, "t1");
        let before = folders(&w);
        assert_eq!(resume_of(&w, target).unwrap()["from"], Y, "{target}");
        let z = new_folder(&w, &before);
        let spec = read_json(&z.join("spec.json")).unwrap().unwrap();
        assert_eq!(spec["args"], json!(["--resume", SESSION_ID]));
        assert!(spec.get("prompt").is_none());
        let record = read_json(&z.join("record.json")).unwrap().unwrap();
        assert_eq!(record["resumedFrom"], Y);
        assert_eq!(record["mailbox"], ID);
        w.fakes.hc_done().unwrap();
    }
}

// Two unrelated ended members of one name: each resumes by its id, and the name means the newer.
#[test]
fn of_two_unrelated_ended_members_a_name_resumes_the_newer() {
    const C: &str = "cccccccccccc";
    for (target, from, session) in [
        ("worker", C, OTHER_SESSION),
        (ID, ID, SESSION_ID),
        (C, C, OTHER_SESSION),
    ] {
        let w = World::human("");
        let x = member(
            &w,
            ID,
            json!({"pane": "%5", "depth": 1, "createdAt": "2026-11-13T10:00:00Z"}),
        );
        let c = member(
            &w,
            C,
            json!({"pane": "%7", "depth": 1, "createdAt": "2026-11-13T10:01:00Z",
                "sessionId": OTHER_SESSION}),
        );
        w.panes(&[]);
        facts(&w, &[(&x, "exited"), (&c, "stopped")]);
        launches(&w, "t1");
        let before = folders(&w);
        assert_eq!(resume_of(&w, target).unwrap()["from"], from, "{target}");
        let spec = read_json(&new_folder(&w, &before).join("spec.json"))
            .unwrap()
            .unwrap();
        assert_eq!(spec["args"], json!(["--resume", session]), "{target}");
        w.fakes.hc_done().unwrap();
    }
}

// X -> Y -> Z keeps X's mailbox and its read offset: mail read before is never replayed, mail after is read once.
#[test]
fn a_resume_chain_keeps_the_first_mailbox_and_its_cursor() {
    let w = World::human("");
    let caller = w.caller();
    let x = member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
    add_mail(&w.ctx, &caller, ID, "root", "%1", "one", None).unwrap();
    assert_eq!(read_mail(&w.ctx, &caller, ID).unwrap().len(), 1);
    add_mail(&w.ctx, &caller, ID, "root", "%1", "two", None).unwrap();
    w.panes(&[]);
    facts(&w, &[(&x, "exited")]);
    launches(&w, "t1");
    let y_id = resume_of(&w, ID).unwrap()["agent"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let y = record_of(&w, &y_id);
    facts(&w, &[(&x, "exited"), (&y, "exited")]);
    launches(&w, "t2");
    let reply = resume_of(&w, &y_id).unwrap();
    assert_eq!(reply["from"], y_id.as_str());
    let z_id = reply["agent"]["id"].as_str().unwrap().to_string();
    let z = record_of(&w, &z_id);
    assert_eq!(z["mailbox"], ID);
    let bodies: Vec<String> = read_mail(&w.ctx, &caller, &member_mailbox(&z_id, &z))
        .unwrap()
        .iter()
        .map(|m| text(m.get("text")).rsplit('\n').next().unwrap().to_string())
        .collect();
    assert_eq!(bodies, ["two"]);
    w.fakes.hc_done().unwrap();
}

// One spawn lock for the whole resume; X's mailbox lock is taken and released inside it, before hc run.
#[test]
fn resume_holds_the_spawn_lock_once_and_passes_the_mailbox_barrier_before_the_run() {
    let w = World::human("");
    let caller = w.caller();
    let x = member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
    w.panes(&[]);
    facts(&w, &[(&x, "exited")]);
    launches(&w, "t1");
    let seen = Rc::new(RefCell::new(Vec::new()));
    let (fakes, at_run) = (w.fakes.clone(), seen.clone());
    w.fakes.hc_probe(move |a| {
        if a[0] == "run" {
            *at_run.borrow_mut() = fakes.locks();
        }
    });
    w.fakes.clear_log();
    resume_of(&w, ID).unwrap();
    let spawn = lock_key("spawn", &caller, "$3");
    let mail = lock_key("mail", &caller, ID);
    let count = |events: &[String], event: String| events.iter().filter(|e| **e == event).count();
    let at_run = seen.borrow().clone();
    assert_eq!(
        count(&at_run, format!("+{spawn}")) - count(&at_run, format!("-{spawn}")),
        1
    );
    assert_eq!(
        count(&at_run, format!("+{mail}")) - count(&at_run, format!("-{mail}")),
        0
    );
    let order: Vec<_> = w
        .fakes
        .locks()
        .into_iter()
        .filter(|e| e.ends_with(&spawn) || e.ends_with(&mail))
        .collect();
    assert_eq!(
        order,
        [
            format!("+{spawn}"),
            format!("+{mail}"),
            format!("-{mail}"),
            format!("-{spawn}")
        ]
    );
    w.fakes.hc_done().unwrap();
}

// Resume keeps the bar as spawn does, with spawn's reply fields: `sidebar` (null when the bar is off) and, when the
// best-effort step fails, `sidebarError`; the resume itself still succeeds.
#[test]
fn resume_reply_carries_the_bar_as_spawn_does() {
    for case in ["off", "failed"] {
        let w = World::human("");
        let x = member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
        w.panes(&[]);
        facts(&w, &[(&x, "exited")]);
        launches(&w, "t1");
        match case {
            "off" => w.fakes.mux_first("@mux-agents-sidebar", 0, "off\n", ""),
            _ => w.fakes.mux_first("@mux-agents-sidebar", 1, "", "no server"),
        }
        let reply = resume_of(&w, ID).unwrap();
        assert_eq!(reply.get("sidebar"), Some(&Value::Null), "{case}");
        assert_eq!(
            reply.contains_key("sidebarError"),
            case == "failed",
            "{case}"
        );
        w.fakes.hc_done().unwrap();
    }
}

/// A process the fake table lists, with `argv`. Pid 7000+ is clear of the fixture's own (server 900, panes 100+).
fn process(w: &World, pid: u32, argv: &[&str]) {
    let mut row = crate::testing::row(pid, 1, 1);
    row.argv = Some(argv.iter().map(|a| a.to_string()).collect());
    w.fakes.process(row);
}

// X's conversation live anywhere refuses (amendment 1 on #103), and nothing launches:
// - any process on this machine whose argv carries X's uuid after `--resume`, `-r`, `--resume=`, `--session-id` or
//   `--session-id=`, with no hc terminal and no record (a client someone resumed by hand), and a process table that
//   cannot be read;
// - a member under another server key whose record carries X's session and that hc does not prove ended, or whose
//   terminal hc cannot read.
// A fork of X (`--resume X --fork-session --session-id Z`) occupies only Z, so it does not block X; a fork into X
// (`... --session-id X`) does. A row with no argv refuses (naming its pid) when it is a claude or copilot client,
// known by `client_of` or its image's file name; one that is no client is skipped. The member elsewhere, proven
// ended, does not block.
#[test]
fn resume_refuses_while_the_conversation_is_live_anywhere() {
    let resume_equals = format!("--resume={SESSION_ID}");
    let session_equals = format!("--session-id={SESSION_ID}");
    for (case, refused) in [
        ("process", true),
        ("short", true),
        ("resume-equals", true),
        ("session-id", true),
        ("session-id-equals", true),
        ("fork-into-x", true),
        ("table", true),
        ("elsewhere", true),
        ("unreadable", true),
        ("client-path-no-argv", true),
        ("client-of-no-argv", true),
        ("fork-of-x", false),
        ("no-argv", false),
        ("other-path-no-argv", false),
        ("elsewhere-ended", false),
    ] {
        let w = World::human("");
        let x = member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
        w.panes(&[]);
        match case {
            "process" => process(&w, 7001, &["claude", "--resume", SESSION_ID]),
            "short" => process(&w, 7001, &["copilot", "-r", SESSION_ID]),
            "resume-equals" => process(&w, 7001, &["claude", &resume_equals]),
            "session-id" => process(&w, 7001, &["claude", "--session-id", SESSION_ID]),
            "session-id-equals" => process(&w, 7001, &["claude", &session_equals]),
            "fork-into-x" => process(
                &w,
                7001,
                &[
                    "claude",
                    "--resume",
                    OTHER_SESSION,
                    "--fork-session",
                    "--session-id",
                    SESSION_ID,
                ],
            ),
            "fork-of-x" => process(
                &w,
                7001,
                &[
                    "claude",
                    "--resume",
                    SESSION_ID,
                    "--fork-session",
                    "--session-id",
                    OTHER_SESSION,
                ],
            ),
            "no-argv" => w.fakes.process(crate::testing::row(7001, 1, 1)),
            // A client whose argv cannot be read, known by its image's file name or by `client_of`.
            "client-path-no-argv" | "other-path-no-argv" => {
                let mut row = crate::testing::row(7001, 1, 1);
                let file = if case == "client-path-no-argv" {
                    "claude"
                } else {
                    "sleep"
                };
                row.path = Some(format!("/usr/local/bin/{file}"));
                w.fakes.process(row);
            }
            "client-of-no-argv" => {
                w.fakes.process(crate::testing::row(7001, 1, 1));
                w.fakes.client(7001, crate::parse::Client::Copilot);
            }
            "table" => w.fakes.table_fails(),
            _ => {}
        }
        facts(&w, &[(&x, "exited")]);
        if ["elsewhere", "elsewhere-ended", "unreadable"].contains(&case) {
            let launcher = json!(["/bin/mux-agent", "launcher", "/other/spec.json"]);
            let elsewhere = w.ctx.state_root.join("other-server/agents").join(Y);
            std::fs::create_dir_all(&elsewhere).unwrap();
            write(
                &elsewhere.join("record.json"),
                json!({"schema": 1, "id": Y, "name": "worker", "client": "claude",
                    "sessionId": SESSION_ID, "session": "$1", "team": "other-server:$1",
                    "generation": "0", "hcAddress": "tother", "launcherArgv": launcher}),
            );
            // The session check's one look at hc: that member's own terminal.
            if case == "unreadable" {
                w.fakes
                    .hc_expect(&["get", "tother", "--json"], Err(Refusal::internal("gone")));
            } else {
                let mut t = json!({"address": "tother", "state": "live", "argv": launcher,
                    "labels": {"mux.managed": "1", "mux.team": "other-server:$1", "mux.id": Y,
                        "mux.gen": "0", "mux.name": "worker"}});
                if case == "elsewhere-ended" {
                    t["state"] = json!("finished");
                    t["reason"] = json!("exited");
                }
                cf::get(&w, "tother", t);
            }
        }
        let before = folders(&w);
        if refused {
            let e = resume_of(&w, ID).unwrap_err();
            assert_eq!((e.code, e.check), (2, "session"), "{case}: {}", e.message);
            if case.ends_with("-no-argv") {
                assert!(e.message.contains("7001"), "{case}: {}", e.message);
            }
            assert_eq!(folders(&w), before, "{case}");
            assert_eq!(runs(&w), 0, "{case}");
        } else {
            launches(&w, "t1");
            assert_eq!(resume_of(&w, ID).unwrap()["from"], ID, "{case}");
            assert_eq!(runs(&w), 1, "{case}");
        }
        w.fakes.hc_done().unwrap();
    }
}

// The real lost-reply path: resume X durably writes Y's pre-run record, hc run starts Y's terminal and its launcher
// claims gen 0, then the run's reply is lost. Y keeps only its pre-run record (no hc fields) and abort evidence. It
// is still a member: `resume <X id>` refuses as taken and names Y; `resume worker` and `resume <Y id>` resume Y with
// no task, its life read from hc by its labels; and the new successor supersedes X through Y.
#[test]
fn a_successor_that_lost_its_run_reply_is_a_member() {
    for target in ["x", "worker", "y"] {
        let w = World::human("");
        w.clock("2026-11-13T10:00:00Z");
        let x = member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
        w.panes(&[]);
        facts(&w, &[(&x, "exited")]);
        let before = folders(&w);
        let agents = w.state().join("agents");
        let known = before.clone();
        w.fakes.hc_probe(move |a| {
            if a[0] == "run" {
                for entry in std::fs::read_dir(&agents).unwrap() {
                    let folder = entry.unwrap().path();
                    let name = folder.file_name().unwrap().to_string_lossy().into_owned();
                    if !known.contains(&name) {
                        std::fs::create_dir_all(folder.join("gens")).unwrap();
                        crate::state::fs::claim_json(&folder.join("gens/0.claim"), &json!("run"))
                            .unwrap();
                    }
                }
            }
        });
        w.fakes
            .hc_answer(Err(Refusal::internal("the run reply was lost")));
        assert!(resume_of(&w, ID).is_err());
        w.fakes.hc_done().unwrap();
        w.fakes.hc_probe(|_| {});
        let folder = new_folder(&w, &before);
        let y = folder.file_name().unwrap().to_str().unwrap().to_string();
        let pre_run = record_of(&w, &y);
        assert_eq!(pre_run["resumedFrom"], ID);
        assert!(pre_run.get("hcAddress").is_none());
        assert!(!crate::agents::committed(&folder));
        // The terminal hc started for Y, labelled by its run; its launcher found no commit and ended.
        let t1 = json!({"address": "t1", "state": "finished", "reason": "exited", "exit_code": 1,
            "argv": ["/bin/mux-agent", "launcher", "/spec"],
            "labels": {"mux.managed": "1", "mux.team": membership::team(&w.caller()), "mux.id": y,
                "mux.gen": "0", "mux.name": "worker"}});
        w.clock("2026-11-13T10:05:00Z");
        cf::online(&w);
        cf::listing(&w, &membership::team(&w.caller()), &["t1"]);
        cf::get(&w, "t1", t1.clone());
        cf::get(&w, "taaaaaaaaaaaa", wire(&x, "exited"));
        if target == "x" {
            let e = resume_of(&w, ID).unwrap_err();
            assert_eq!((e.code, e.check), (2, "taken"), "{}", e.message);
            assert!(
                e.message.contains(&format!("mux-agent resume {y}")),
                "{}",
                e.message
            );
            w.fakes.hc_done().unwrap();
            continue;
        }
        launches(&w, "t2");
        let before = folders(&w);
        let named = if target == "worker" { "worker" } else { &y };
        let reply = resume_of(&w, named).unwrap();
        assert_eq!(reply["from"], y.as_str(), "{target}");
        let z = new_folder(&w, &before);
        let spec = read_json(&z.join("spec.json")).unwrap().unwrap();
        assert_eq!(spec["args"], json!(["--resume", SESSION_ID]));
        assert!(spec.get("prompt").is_none());
        let record = read_json(&z.join("record.json")).unwrap().unwrap();
        assert_eq!(
            (&record["resumedFrom"], &record["mailbox"]),
            (&json!(y), &json!(ID))
        );
        let local = membership::locals(&w.ctx, &w.caller()).unwrap();
        let superseded = membership::superseded_locals(&local);
        assert!(
            superseded.contains(ID) && superseded.contains(&y),
            "{superseded:?}"
        );
        w.fakes.hc_done().unwrap();
    }
}

/// The real per-user locks under one root beside the state folder, so threads whose worlds share it contend.
fn real_locks(w: &World) -> PathBuf {
    let root = w.ctx.state_root.parent().unwrap().join("locks");
    w.fakes.real_locks(&root);
    root
}

/// Waits (up to 10 s) for a signal from another thread.
fn wait(rx: &std::sync::mpsc::Receiver<()>) {
    rx.recv_timeout(std::time::Duration::from_secs(10))
        .expect("the other thread never signalled");
}

// Real threads and the real locks: a read as X holds X's mailbox lock, paused, when the resume reaches its barrier.
// The barrier waits: hc run has not happened while the reader holds the lock, and the resume takes the mailbox lock
// only after the reader gave it back. That reader was X's own read and got X's mail; a stale read as X after the
// resume rechecks under the lock, finds X superseded and reads nothing, so the later mail stays for Y.
#[test]
fn a_paused_reader_holds_the_barrier_and_a_stale_reader_then_reads_nothing() {
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    use std::sync::{Arc, mpsc};
    let w = World::human("");
    let root = real_locks(&w);
    let caller = w.caller();
    let x = member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
    add_mail(&w.ctx, &caller, ID, "root", "%1", "one", None).unwrap();
    let mail = format!("+{}", lock_key("mail", &caller, ID));
    let unmail = format!("-{}", lock_key("mail", &caller, ID));
    let (held_tx, held_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    let (resumed_tx, resumed_rx) = mpsc::channel();
    let (released, ran) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let state_root = w.ctx.state_root.clone();
    let x_record = x.clone();
    let (reader_released, reader_mail, reader_unmail) =
        (released.clone(), mail.clone(), unmail.clone());
    let reader = std::thread::spawn(move || {
        let mut r = World::human("");
        r.ctx.state_root = state_root;
        r.fakes.real_locks(&root);
        let caller = r.caller();
        let identity = Identity::read(&x_record, ID, &membership::team(&caller)).unwrap();
        let paused = AtomicBool::new(false);
        r.fakes.lock_probe(move |event| {
            if event == reader_mail && !paused.swap(true, SeqCst) {
                held_tx.send(()).unwrap();
                wait(&go_rx);
            }
            if event == reader_unmail {
                reader_released.store(true, SeqCst);
            }
            String::new()
        });
        // X's own read, before Y exists: hc still shows X running.
        cf::online(&r);
        cf::get(
            &r,
            x_record["hcAddress"].as_str().unwrap(),
            wire(&x_record, "running"),
        );
        let first =
            read_mail_if(&r.ctx, &caller, ID, || current(&r.ctx, &caller, &identity)).unwrap();
        r.fakes.hc_done().unwrap();
        // A stale read as X once Y exists: the fence finds X superseded before it asks hc anything.
        wait(&resumed_rx);
        let stale =
            read_mail_if(&r.ctx, &caller, ID, || current(&r.ctx, &caller, &identity)).unwrap();
        r.fakes.hc_done().unwrap();
        (first.len(), stale.len())
    });
    wait(&held_rx);
    let releaser_ran = ran.clone();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        let early = releaser_ran.load(SeqCst);
        go_tx.send(()).unwrap();
        early
    });
    w.panes(&[]);
    facts(&w, &[(&x, "exited")]);
    launches(&w, "t1");
    let run_seen = ran.clone();
    w.fakes.hc_probe(move |a| {
        if a[0] == "run" {
            run_seen.store(true, SeqCst);
        }
    });
    let after_release = Arc::new(AtomicBool::new(false));
    let (seen, gave_back) = (after_release.clone(), released.clone());
    w.fakes.lock_probe(move |event| {
        if event == mail {
            seen.store(gave_back.load(SeqCst), SeqCst);
        }
        String::new()
    });
    resume_of(&w, ID).unwrap();
    assert!(
        !releaser.join().unwrap(),
        "hc run happened while a reader held X's mailbox lock"
    );
    assert!(
        after_release.load(SeqCst),
        "the barrier took the mailbox lock before the reader gave it back"
    );
    add_mail(&w.ctx, &caller, ID, "root", "%1", "two", None).unwrap();
    resumed_tx.send(()).unwrap();
    let (first, stale) = reader.join().unwrap();
    assert_eq!((first, stale), (1, 0));
    let bodies: Vec<String> = read_mail(&w.ctx, &caller, ID)
        .unwrap()
        .iter()
        .map(|m| text(m.get("text")).rsplit('\n').next().unwrap().to_string())
        .collect();
    assert_eq!(bodies, ["two"]);
    w.fakes.hc_done().unwrap();
}

// Real threads and the real locks: two resumes of one X at once launch exactly one Y. The second waits on the spawn
// lock the first holds, takes it only after the first gave it back, finds Y's durable record and refuses taken,
// naming Y; it runs nothing and makes no folder.
#[test]
fn two_concurrent_resumes_of_a_member_launch_exactly_one() {
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    use std::sync::{Arc, mpsc};
    let w = World::human("");
    let root = real_locks(&w);
    let caller = w.caller();
    let x = member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
    let before = folders(&w);
    let spawn = format!("+{}", lock_key("spawn", &caller, "$3"));
    let unspawn = format!("-{}", lock_key("spawn", &caller, "$3"));
    let (held_tx, held_rx) = mpsc::channel();
    let released = Arc::new(AtomicBool::new(false));
    let state_root = w.ctx.state_root.clone();
    let x_record = x.clone();
    let (second_spawn, second_released) = (spawn.clone(), released.clone());
    let second = std::thread::spawn(move || {
        let mut b = World::human("");
        b.ctx.state_root = state_root;
        b.fakes.real_locks(&root);
        let entered_after_release = Arc::new(AtomicBool::new(false));
        let seen = entered_after_release.clone();
        b.fakes.lock_probe(move |event| {
            if event == second_spawn {
                seen.store(second_released.load(SeqCst), SeqCst);
            }
            String::new()
        });
        b.panes(&[]);
        wait(&held_rx);
        // B's look, under the lock once A committed Y at t1; Y's terminal is not read here (taken needs no hc).
        cf::online(&b);
        cf::listing(&b, &membership::team(&b.caller()), &[]);
        b.fakes
            .hc_expect(&["get", "t1", "--json"], Err(Refusal::internal("busy")));
        cf::get(
            &b,
            x_record["hcAddress"].as_str().unwrap(),
            wire(&x_record, "exited"),
        );
        let got = resume(&b.ctx, &b.request(&["resume", ID]));
        b.fakes.hc_done().unwrap();
        (got, runs(&b), entered_after_release.load(SeqCst))
    });
    w.panes(&[]);
    facts(&w, &[(&x, "exited")]);
    launches(&w, "t1");
    w.fakes.lock_probe(move |event| {
        if event == spawn {
            held_tx.send(()).unwrap();
            // Long enough for B to reach the spawn lock and wait on it.
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        if event == unspawn {
            released.store(true, SeqCst);
        }
        String::new()
    });
    let y = resume_of(&w, ID).unwrap()["agent"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (got, b_runs, after_release) = second.join().unwrap();
    let e = got.unwrap_err();
    assert_eq!((e.code, e.check), (2, "taken"), "{}", e.message);
    assert!(e.message.contains(&y), "{}", e.message);
    assert!(
        after_release,
        "the second resume took the spawn lock while the first held it"
    );
    assert_eq!((runs(&w), b_runs), (1, 0));
    assert_eq!(new_folder(&w, &before), w.folder(&y));
    w.fakes.hc_done().unwrap();
}

// A resume starts X alone: a descendant of X (a member whose parent pane is X's) stays stopped. Exactly one launch
// happens, for X's successor, and the descendant's files are byte-identical.
#[test]
fn descendants_of_a_resumed_member_stay_stopped() {
    const C: &str = "cccccccccccc";
    let w = World::human("");
    let x = member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
    let c = member(
        &w,
        C,
        json!({"name": "child", "pane": "%7", "window": "@1", "depth": 2, "parentPane": "%5",
            "sessionId": OTHER_SESSION}),
    );
    let kept = files(&w.folder(C));
    w.panes(&[]);
    facts(&w, &[(&x, "exited"), (&c, "stopped")]);
    launches(&w, "t1");
    let before = folders(&w);
    assert_eq!(resume_of(&w, ID).unwrap()["from"], ID);
    let y = new_folder(&w, &before);
    assert_eq!(runs(&w), 1);
    assert_eq!(
        record_of(&w, y.file_name().unwrap().to_str().unwrap())["resumedFrom"],
        ID
    );
    assert_eq!(files(&w.folder(C)), kept);
    assert!(!folders(&w).iter().any(|f| {
        std::fs::read(w.folder(f).join("record.json")).is_ok_and(|r| {
            String::from_utf8_lossy(&r).contains(&format!("\"resumedFrom\":\"{C}\""))
        })
    }));
    w.fakes.hc_done().unwrap();
}

// `focus <name>` on a superseded X picks its successor Y: X's own view is still proven, yet the name means Y.
#[test]
fn focus_on_a_superseded_member_picks_its_successor() {
    let w = World::human("");
    let x = member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
    let y = member(
        &w,
        Y,
        json!({"pane": "%9", "window": "@5", "depth": 1, "resumedFrom": ID, "mailbox": ID}),
    );
    let row = |pane: &str, window: &str, r: &Value| {
        let mut row = crate::agents::fixture::row_of(&format!("{pane}|{window}|300||"), &[]);
        row.command = r["startCommand"].as_str().unwrap().into();
        row
    };
    w.pane_rows(&[row("%5", "@1", &x), row("%9", "@5", &y)]);
    w.fakes.mux_rule("#{window_active}", 0, "%9|1|1|1\n", "");
    cf::online(&w);
    let core = crate::hc::connect(&w.ctx).unwrap();
    let pane = crate::agents::view::focus_hc(&w.ctx, &core, &w.caller(), "worker", None).unwrap();
    assert_eq!((pane.pane.as_str(), pane.window.as_str()), ("%9", "@5"));
    assert!(
        !w.fakes
            .mux_calls()
            .iter()
            .any(|a| a.iter().any(|t| t.contains("%5")))
    );
    w.fakes.hc_done().unwrap();
}

// X -> Y -> Z: mail after the chain is read once, by the current member Z. A read as X or Y rechecks under the
// lock, finds it superseded and reads nothing (without asking hc); Z reads the message, and a second read nothing.
#[test]
fn a_resume_chain_reads_new_mail_once_by_the_current_member() {
    let w = World::human("");
    let caller = w.caller();
    let x = member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
    w.panes(&[]);
    facts(&w, &[(&x, "exited")]);
    launches(&w, "t1");
    let y_id = resume_of(&w, ID).unwrap()["agent"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let y = record_of(&w, &y_id);
    facts(&w, &[(&x, "exited"), (&y, "exited")]);
    launches(&w, "t2");
    let z_id = resume_of(&w, &y_id).unwrap()["agent"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let z = record_of(&w, &z_id);
    w.fakes.hc_done().unwrap();
    add_mail(&w.ctx, &caller, ID, "root", "%1", "three", None).unwrap();
    let team = membership::team(&caller);
    let read_as = |r: &Value, id: &str| {
        let identity = Identity::read(r, id, &team).unwrap();
        read_mail_if(&w.ctx, &caller, ID, || current(&w.ctx, &caller, &identity)).unwrap()
    };
    w.fakes.clear_log();
    assert!(read_as(&x, ID).is_empty());
    assert!(read_as(&y, &y_id).is_empty());
    assert!(w.fakes.hc_calls().is_empty(), "{:?}", w.fakes.hc_calls());
    for want in [1, 0] {
        cf::online(&w);
        cf::get(&w, "t2", wire(&z, "running"));
        assert_eq!(read_as(&z, &z_id).len(), want);
    }
    w.fakes.hc_done().unwrap();
}

// A failed flush of the agents folder (after Y's record and before hc run) launches nothing and leaves abort
// evidence; a later resume launches.
#[test]
fn a_failed_agents_folder_flush_launches_nothing() {
    let w = World::human("");
    let x = member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
    w.panes(&[]);
    facts(&w, &[(&x, "exited")]);
    let before = folders(&w);
    let agents = w.state().join("agents");
    crate::state::fs::fail_sync_dir(Some(&agents));
    let got = resume_of(&w, ID);
    crate::state::fs::fail_sync_dir(None);
    assert!(got.is_err());
    assert_eq!(runs(&w), 0);
    let folder = new_folder(&w, &before);
    assert!(folder.join("abort.json").exists());
    assert_eq!(
        record_of(&w, folder.file_name().unwrap().to_str().unwrap())["resumedFrom"],
        ID
    );
    w.fakes.hc_done().unwrap();
    facts(&w, &[(&x, "exited")]);
    launches(&w, "t1");
    assert_eq!(resume_of(&w, ID).unwrap()["from"], ID);
    assert_eq!(runs(&w), 1);
    w.fakes.hc_done().unwrap();
}

// Power loss after each record write of a resume: the folder flush after the rename fails, as a crash right after
// the write would leave it. The record on disk is whole JSON with its link (`resumedFrom`, `mailbox`, `sessionId`)
// and what that write added, no temp file is left, and nothing commits. A write after hc run rolls the run back.
#[test]
fn power_loss_after_each_record_write_leaves_a_complete_record() {
    for write in 0..3 {
        let w = World::human("");
        let x = member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
        w.panes(&[]);
        facts(&w, &[(&x, "exited")]);
        if write > 0 {
            launches(&w, "t1");
            cf::answer(&w, &["kill", "t1", "--json"], "{}");
        }
        let before = folders(&w);
        crate::state::fs::fail_durable_flush(Some(write));
        let got = resume_of(&w, ID);
        crate::state::fs::fail_durable_flush(None);
        assert!(got.is_err(), "write {write}");
        let folder = new_folder(&w, &before);
        let id = folder.file_name().unwrap().to_str().unwrap().to_string();
        let record = record_of(&w, &id);
        for (k, v) in [
            ("id", json!(id)),
            ("resumedFrom", json!(ID)),
            ("mailbox", json!(ID)),
            ("sessionId", json!(SESSION_ID)),
        ] {
            assert_eq!(record[k], v, "write {write} {k}");
        }
        assert_eq!(
            record.get("hcAddress").is_some(),
            write > 0,
            "write {write}"
        );
        assert_eq!(record.get("pane").is_some(), write > 1, "write {write}");
        let left: Vec<_> = std::fs::read_dir(&folder)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(left.is_empty(), "write {write}: {left:?}");
        assert!(!crate::agents::committed(&folder), "write {write}");
        assert!(folder.join("abort.json").exists(), "write {write}");
        assert_eq!(runs(&w), usize::from(write > 0), "write {write}");
        w.fakes.hc_done().unwrap();
    }
}

// v11: send, read, spawn and stop stay bounded during a resume. Real threads and the real locks: a resume of X is
// paused while it holds the spawn lock, or (spawn lock held) X's mailbox barrier. Meanwhile another thread acts on
// another member B (a running member of another conversation): `send` to B, `read` as B, `stop` B, or `spawn` a new
// member. Each completes, or refuses `lock` within LOCK_WAIT; none waits without bound. send, read and stop take no
// lock the resume holds, so they finish while it is still paused; spawn waits for the spawn lock and finishes within
// LOCK_WAIT (plus a margin) after the resume gives it back. The lock wait is the real lock's, on the real clock (the
// seam's), and no case comes near it.
#[test]
fn send_read_spawn_and_stop_stay_bounded_during_a_resume() {
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::{Duration, Instant};
    const B: &str = "cccccccccccc";
    let margin = Duration::from_secs(2);
    for pause in ["spawn", "mail"] {
        for verb in ["send", "read", "spawn", "stop"] {
            let w = World::human("");
            let root = real_locks(&w);
            let caller = w.caller();
            let x = member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
            let b = member(
                &w,
                B,
                json!({"name": "other", "pane": "%6", "window": "@1", "depth": 1,
                    "sessionId": OTHER_SESSION}),
            );
            w.panes(&[]);
            facts(&w, &[(&x, "exited"), (&b, "running")]);
            launches(&w, "t1");
            let key = match pause {
                "spawn" => lock_key("spawn", &caller, "$3"),
                _ => lock_key("mail", &caller, ID),
            };
            let (held_tx, held_rx) = mpsc::channel();
            let (go_tx, go_rx) = mpsc::channel::<()>();
            let released: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
            let (paused, at, hold, give) = (
                AtomicBool::new(false),
                released.clone(),
                format!("+{key}"),
                format!("-{key}"),
            );
            w.fakes.lock_probe(move |event| {
                if event == hold && !paused.swap(true, SeqCst) {
                    held_tx.send(()).unwrap();
                    go_rx
                        .recv_timeout(Duration::from_secs(10))
                        .expect("the verb never let the resume go: it waited without bound");
                }
                if event == give {
                    *at.lock().unwrap() = Some(Instant::now());
                }
                String::new()
            });
            let state_root = w.ctx.state_root.clone();
            let (xr, br) = (x.clone(), b.clone());
            let worker = std::thread::spawn(move || {
                let mut v = World::human("");
                v.ctx.state_root = state_root;
                v.fakes.real_locks(&root);
                let team = membership::team(&v.caller());
                let terminal = |r: &Value, life: &str| {
                    let mut t = wire(r, life);
                    t["labels"]["mux.team"] = json!(team);
                    t
                };
                let addr = |r: &Value| r["hcAddress"].as_str().unwrap().to_string();
                let look = |v: &World| {
                    cf::online(v);
                    cf::listing(v, &team, &[]);
                    cf::get(v, &addr(&xr), terminal(&xr, "exited"));
                    cf::get(v, &addr(&br), terminal(&br, "running"));
                };
                let request: Vec<&str> = match verb {
                    "send" => {
                        v.panes(&[]);
                        look(&v);
                        vec!["send", "other", "-Body", "hi"]
                    }
                    "read" => {
                        let tmux = v.ctx.env.get("TMUX").unwrap().to_string();
                        v.ctx.env = crate::ctx::Env::from_pairs([
                            ("TMUX", tmux),
                            ("TMUX_PANE", "%6".to_string()),
                        ]);
                        let mut row = crate::agents::fixture::row_of("%6|@1|300||", &[]);
                        row.command = br["startCommand"].as_str().unwrap().into();
                        v.pane_rows(&[row]);
                        look(&v);
                        // The read fence's own look under B's mailbox lock.
                        cf::online(&v);
                        cf::get(&v, &addr(&br), terminal(&br, "running"));
                        vec!["read"]
                    }
                    "stop" => {
                        v.panes(&[]);
                        cf::online(&v);
                        cf::answer(&v, &["kill", &addr(&br), "--json"], "{}");
                        cf::get(&v, &addr(&br), terminal(&br, "stopped"));
                        vec!["stop", "other"]
                    }
                    _ => {
                        // After the resume: Y is committed at t1; hc cannot read it here, so it is unknown.
                        v.panes(&[]);
                        cf::online(&v);
                        cf::listing(&v, &team, &[]);
                        v.fakes
                            .hc_expect(&["get", "t1", "--json"], Err(Refusal::internal("busy")));
                        cf::get(&v, &addr(&xr), terminal(&xr, "exited"));
                        cf::get(&v, &addr(&br), terminal(&br, "running"));
                        launches(&v, "t2");
                        vec!["spawn", "-Client", "claude", "-Task", "hi", "-Name", "w9"]
                    }
                };
                wait(&held_rx);
                let started = Instant::now();
                let request = v.request(&request);
                let got = match verb {
                    "send" => crate::agents::message::send(&v.ctx, &request),
                    "read" => crate::agents::message::read(&v.ctx, &request),
                    "stop" => crate::agents::message::stop_hc(&v.ctx, &request),
                    _ => {
                        // spawn needs the spawn lock the resume holds: let the resume go once it is waiting.
                        let go = go_tx.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_millis(300));
                            go.send(()).unwrap();
                        });
                        crate::agents::spawn::spawn(&v.ctx, &request)
                    }
                };
                let done = Instant::now();
                if verb != "spawn" {
                    go_tx.send(()).unwrap();
                }
                let fixture = v.fakes.hc_done();
                (got, started, done, fixture)
            });
            let resumed = resume_of(&w, ID);
            let (got, started, done, fixture) = worker.join().unwrap();
            let case = format!("{verb} during the resume's {pause} lock");
            assert!(resumed.is_ok(), "{case}: the resume {resumed:?}");
            match &got {
                Ok(_) => {}
                Err(e) => assert_eq!((e.code, e.check), (2, "lock"), "{case}: {}", e.message),
            }
            fixture.unwrap_or_else(|e| panic!("{case}: {}", e.message));
            let released = released
                .lock()
                .unwrap()
                .expect("the resume released its lock");
            assert!(
                done <= released + crate::state::lock::LOCK_WAIT + margin,
                "{case}"
            );
            if verb == "spawn" {
                // It waited for the resume's spawn lock, then went on.
                assert!(done >= released && started < released, "{case}");
            } else {
                // It took no lock the resume holds: done while the resume was still paused.
                assert!(done < released, "{case}");
            }
            match verb {
                "send" => assert_eq!(
                    read_mail(&w.ctx, &caller, B)
                        .unwrap()
                        .iter()
                        .map(|m| text(m.get("text")).rsplit('\n').next().unwrap().to_string())
                        .collect::<Vec<_>>(),
                    ["hi"],
                    "{case}"
                ),
                "read" => assert_eq!(got.unwrap()["messages"], json!([]), "{case}"),
                "stop" => assert_eq!(got.unwrap()["life"], "ended", "{case}"),
                _ => assert_eq!(got.unwrap()["name"], "w9", "{case}"),
            }
            w.fakes.hc_done().unwrap();
        }
    }
}

// v11:25: every rewrite of a member's record is durable, focus's included. Resume X to Y, Y's view is gone, then
// `focus worker` reopens it and rewrites Y's record with the new view. A failed flush of Y's folder after that
// rewrite is reported (focus fails, the new view is cleaned up); the record on disk, as a power loss right after the
// write leaves it, is whole JSON that keeps `resumedFrom`, `mailbox` and `sessionId`, and no temp file is left. With
// the flush working, focus reopens the view.
#[test]
fn focus_rewrites_a_resumed_record_durably() {
    let w = World::human("");
    let x = member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
    w.panes(&[]);
    facts(&w, &[(&x, "exited")]);
    launches(&w, "t1");
    let y = resume_of(&w, ID).unwrap()["agent"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    w.fakes.hc_done().unwrap();
    w.fakes.mux_rule("#{window_active}", 0, "%9|1|1|1\n", "");
    cf::online(&w);
    let core = crate::hc::connect(&w.ctx).unwrap();
    let caller = w.caller();
    let folder = w.folder(&y);
    crate::state::fs::fail_sync_dir(Some(&folder));
    let got = crate::agents::view::focus_hc(&w.ctx, &core, &caller, "worker", None);
    crate::state::fs::fail_sync_dir(None);
    assert!(got.is_err(), "the failed flush is reported: {got:?}");
    let record = record_of(&w, &y);
    for (k, v) in [
        ("id", json!(y)),
        ("resumedFrom", json!(ID)),
        ("mailbox", json!(ID)),
        ("sessionId", json!(SESSION_ID)),
        ("pane", json!("%9")),
    ] {
        assert_eq!(record[k], v, "{k}");
    }
    let left: Vec<_> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    let focused = crate::agents::view::focus_hc(&w.ctx, &core, &caller, "worker", None).unwrap();
    assert_eq!(focused.pane, "%9");
    assert_eq!(record_of(&w, &y)["resumedFrom"], ID);
    w.fakes.hc_done().unwrap();
}

// Two unrelated ended `worker`s: resume the newer, and `send worker` reaches its successor Y. The older one, proven
// ended, is no rival for the name (as `member_target` decides). A lost or unknown `worker` beside them still makes
// the name ambiguous: send refuses and delivers nothing.
#[test]
fn send_by_name_reaches_the_successor_of_one_of_two_ended_members() {
    const C: &str = "cccccccccccc";
    const D: &str = "dddddddddddd";
    const THIRD: &str = "22222222-3333-4444-8555-666666666666";
    for rival in ["none", "lost", "unknown"] {
        let w = World::human("");
        let caller = w.caller();
        let x = member(
            &w,
            ID,
            json!({"pane": "%5", "depth": 1, "createdAt": "2026-11-13T10:00:00Z"}),
        );
        let c = member(
            &w,
            C,
            json!({"pane": "%7", "depth": 1, "createdAt": "2026-11-13T10:01:00Z",
                "sessionId": OTHER_SESSION}),
        );
        w.panes(&[]);
        facts(&w, &[(&x, "exited"), (&c, "stopped")]);
        launches(&w, "t1");
        let reply = resume_of(&w, "worker").unwrap();
        assert_eq!(reply["from"], C, "{rival}");
        let y = record_of(&w, reply["agent"]["id"].as_str().unwrap());
        w.fakes.hc_done().unwrap();
        let mut members = vec![
            (x.clone(), "exited"),
            (c.clone(), "stopped"),
            (y, "running"),
        ];
        if rival != "none" {
            let d = member(&w, D, json!({"pane": "%8", "depth": 1, "sessionId": THIRD}));
            members.push((d, rival));
        }
        let refs: Vec<_> = members.iter().map(|(r, l)| (r, *l)).collect();
        snapshot(&w, &refs);
        let got =
            crate::agents::message::send(&w.ctx, &w.request(&["send", "worker", "-Body", "hi"]));
        let delivered: Vec<String> = read_mail(&w.ctx, &caller, C)
            .unwrap()
            .iter()
            .map(|m| text(m.get("text")).rsplit('\n').next().unwrap().to_string())
            .collect();
        if rival == "none" {
            assert!(got.is_ok(), "{rival}: {got:?}");
            assert_eq!(delivered, ["hi"], "{rival}");
        } else {
            let e = got.unwrap_err();
            assert_eq!((e.code, e.check), (2, "target"), "{rival}: {}", e.message);
            assert!(delivered.is_empty(), "{rival}");
        }
        w.fakes.hc_done().unwrap();
    }
}
