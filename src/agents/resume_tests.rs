use super::*;
use crate::agents::{
    core_fixture as cf,
    core_identity::Identity,
    fixture::{ID, SESSION_ID, World, write},
    gens, lock_key,
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

/// A committed member whose launch spec records its client's flags. prelife-1c G10d: a lifecycle generation-0
/// subject (`prelife_fixture::to_lifecycle`), since a pre-lifecycle member never resumes.
/// A successor (`resumedFrom`) stays as the resume that made it wrote it, so tests that shape its claim, commit or
/// abort by hand keep their meaning.
fn member(w: &World, id: &str, fields: Value) -> Value {
    let successor = fields.get("resumedFrom").is_some();
    let r = pre_member(w, id, fields);
    if successor {
        return r;
    }
    crate::agents::prelife_fixture::to_lifecycle(w, id, false)
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
    for (r, life) in &members {
        // prelife-1c: the record, not hc, decides a lifecycle member Ended (state-0 S6).
        let ended = matches!(*life, "exited" | "stopped");
        if ended && r.get("lifecycle") == Some(&json!(1)) {
            let path = gens::path(&w.folder(r["id"].as_str().unwrap()), 0, "ended");
            if !path.exists() {
                crate::state::evidence::publish(
                    &path,
                    &json!({"state": "finished", "reason": "exited"}),
                )
                .unwrap();
            }
        }
    }
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

/// A lifecycle successor's natural end, as group B's reaper records it: generation 0's `ended.json`.
fn ended_life(w: &World, id: &str) {
    crate::state::evidence::publish(
        &gens::path(&w.folder(id), 0, "ended"),
        &json!({"state": "finished", "reason": "exited", "exitCode": 0}),
    )
    .unwrap();
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
            let before = folders(&w);
            w.panes(&[]);
            facts(&w, &[(&x, how)]);
            // prelife-1c: after `facts`, which records X's end (gens/0/ended.json) as fixture setup.
            let kept = files(&w.folder(ID));
            launches(&w, "t1");
            let mut reply = resume_of(&w, ID).unwrap();
            // The bar's fields are spawn's, tested by `resume_reply_carries_the_bar_as_spawn_does`.
            assert!(reply.remove("sidebar").is_some(), "{client} {how}");
            reply.remove("sidebarError");
            let y = new_folder(&w, &before);
            let id = y.file_name().unwrap().to_str().unwrap().to_string();
            assert_eq!(
                Value::Object(reply),
                json!({"from": ID, "agent": {"id": id, "name": "worker", "hcAddress": "t1"}, "view": "attached"}),
                "{client} {how}"
            );
            let spec = read_json(&gens::path(&y, 0, "spec")).unwrap().unwrap();
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
            assert!(gens::committed(&y, 0).unwrap());
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
                    name: "s".into(),
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
            // The session check reads the one other record that carries X's session (Y, a lifecycle member with no
            // recorded end) from its generation evidence alone; hc never ends it (R3), so it asks hc nothing more.
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
        w.fakes
            .hc_done()
            .unwrap_or_else(|e| panic!("{case}: {}", e.message));
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
    std::fs::remove_file(gens::path(&folder, 0, "commit")).unwrap();
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
    let folder = new_folder(&w, &before);
    assert_eq!(
        gens::decide(
            &read_json(&gens::path(&folder, 0, "commit"))
                .unwrap()
                .unwrap()
        ),
        gens::Decision::Aborted
    );
    w.fakes.hc_done().unwrap();
    facts(&w, &[(&x, "exited")]);
    launches(&w, "t1");
    assert_eq!(resume_of(&w, ID).unwrap()["from"], ID);
    assert_eq!(runs(&w), 1);
    w.fakes.hc_done().unwrap();
}

// A successor that claimed its run, failed before its commit and ended is the member its name means. Built by hand
// in the pre-lifecycle layout (no `lifecycle: 1`), G10d refuses it (prelife-1c Task 4, call 20): by id, the early
// guard refuses from its record before any hc call; by name, admission refuses the subject it selected (Y, not the
// lifecycle X the name also names). Nothing launches and no folder is made.
#[test]
fn an_aborted_pre_lifecycle_successor_is_refused_by_name_or_id() {
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
        if target == "worker" {
            facts(&w, &[(&x, "exited"), (&y, "stopped")]);
        }
        let before = (folders(&w), files(&folder));
        let e = resume_of(&w, target).unwrap_err();
        assert_eq!(
            (e.code, e.check, e.message.as_str()),
            (
                2,
                "state",
                "worker predates lifecycle records (pre-lifecycle) and is never resumed"
            ),
            "{target}"
        );
        assert_eq!((folders(&w), files(&folder)), before, "{target}");
        assert_eq!(runs(&w), 0, "{target}");
        if target == Y {
            assert!(w.fakes.hc_calls().is_empty(), "{target}: refused before hc");
        }
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
        let spec = read_json(&gens::path(&new_folder(&w, &before), 0, "spec"))
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
    // Y is a lifecycle member: it is ended by its recorded end (R3), as the reaper records a natural exit.
    ended_life(&w, &y_id);
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
            } else {
                // #137: a conversation just stopped may still be ending; the message says to retry.
                assert_eq!(
                    e.message,
                    "that conversation is live in another member or terminal, or cannot be proven ended; if you \
                     just stopped it, retry in a few seconds",
                    "{case}"
                );
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
fn a_successor_that_lost_its_run_reply_is_void() {
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
    // G1, PM call 3: the run error fenced generation 0; the successor is void, never a member.
    assert_eq!(
        gens::decide(
            &read_json(&gens::path(&folder, 0, "commit"))
                .unwrap()
                .unwrap()
        ),
        gens::Decision::Aborted
    );
    let pre_run = record_of(&w, &y);
    assert_eq!(pre_run["resumedFrom"], ID);
    assert!(pre_run.get("pending").is_none() && pre_run.get("generation").is_none());
    w.clock("2026-11-13T10:05:00Z");
    facts(&w, &[(&x, "exited")]);
    launches(&w, "t2");
    let before = folders(&w);
    assert_eq!(resume_of(&w, ID).unwrap()["from"], ID, "X is not taken");
    let z = new_folder(&w, &before);
    assert_eq!(
        record_of(&w, z.file_name().unwrap().to_str().unwrap())["resumedFrom"],
        ID
    );
    w.fakes.hc_done().unwrap();
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
    let (reader_released, reader_unmail) = (released.clone(), unmail.clone());
    let reader = std::thread::spawn(move || {
        let mut r = World::human("");
        r.ctx.state_root = state_root;
        r.fakes.real_locks(&root);
        let caller = r.caller();
        let identity = Identity::read(&x_record, ID, &membership::team(&caller)).unwrap();
        r.fakes.lock_probe(move |event| {
            if event == reader_unmail {
                reader_released.store(true, SeqCst);
            }
            String::new()
        });
        // X's own read, before Y exists: hc still shows X running. prelife-1c: X is a lifecycle member, so the read
        // pauses after its fence answered (under the mailbox lock), before the resume publishes X's recorded end.
        cf::online(&r);
        cf::get(
            &r,
            x_record["hcAddress"].as_str().unwrap(),
            wire(&x_record, "running"),
        );
        let first = read_mail_if(&r.ctx, &caller, ID, || {
            let now = current(&r.ctx, &caller, &identity);
            held_tx.send(()).unwrap();
            wait(&go_rx);
            now
        })
        .unwrap();
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
    w.panes(&[]);
    facts(&w, &[(&x, "exited"), (&c, "stopped")]);
    // prelife-1c: after `facts`, which records C's end (gens/0/ended.json) as fixture setup.
    let kept = files(&w.folder(C));
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
    // Y is a lifecycle member: it is ended by its recorded end (R3), as the reaper records a natural exit.
    ended_life(&w, &y_id);
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
// evidence; a later resume launches. prelife-1c: X is a lifecycle member whose admission reads its recorded end
// through a flushing read (`agents` included), so the fault is armed at the mailbox barrier, after admission and
// before Y's create.
#[test]
fn a_failed_agents_folder_flush_launches_nothing() {
    let w = World::human("");
    let x = member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
    w.panes(&[]);
    facts(&w, &[(&x, "exited")]);
    let before = folders(&w);
    let agents = w.state().join("agents");
    let barrier = format!("+{}", lock_key("mail", &w.caller(), ID));
    let armed = agents.clone();
    w.fakes.lock_probe(move |event| {
        if event == barrier {
            crate::state::fs::fail_sync_dir(Some(&armed));
        }
        String::new()
    });
    let got = resume_of(&w, ID);
    crate::state::fs::fail_sync_dir(None);
    w.fakes.lock_probe(|_| String::new());
    assert!(got.is_err());
    assert_eq!(runs(&w), 0);
    let folder = new_folder(&w, &before);
    assert_eq!(
        gens::decide(
            &read_json(&gens::path(&folder, 0, "commit"))
                .unwrap()
                .unwrap()
        ),
        gens::Decision::Aborted
    );
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
    // Record writes in order: 0 the create, 1 the address patch, 2 the promotion. A creating launch also writes
    // `desired.json` durably right after the create (cascade-3d C1), so record write k is durable write `at`.
    for (write, at) in [(0, 0), (1, 2), (2, 3)] {
        let w = World::human("");
        let x = member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
        w.panes(&[]);
        facts(&w, &[(&x, "exited")]);
        if write > 0 {
            launches(&w, "t1");
        }
        let before = folders(&w);
        crate::state::fs::fail_durable_flush(Some(at));
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
        ] {
            assert_eq!(record[k], v, "write {write} {k}");
        }
        assert_eq!(
            record.get("sessionId").is_some(),
            write == 2,
            "write {write}: the session arrives at promotion"
        );
        assert_eq!(
            record.get("hcAddress").is_some(),
            write == 2,
            "write {write}: the address is in pending until promotion"
        );
        assert!(record.get("pending").is_none(), "write {write}");
        assert!(record.get("pane").is_none(), "write {write}");
        let left: Vec<_> = std::fs::read_dir(&folder)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(left.is_empty(), "write {write}: {left:?}");
        assert_eq!(
            gens::decide(
                &read_json(&gens::path(&folder, 0, "commit"))
                    .unwrap()
                    .unwrap()
            ),
            if write == 2 {
                gens::Decision::Run
            } else {
                gens::Decision::Aborted
            },
            "write {write}"
        );
        assert_eq!(runs(&w), usize::from(write > 0), "write {write}");
        w.fakes.hc_done().unwrap();
    }
}

// v11: send, read, spawn and stop stay bounded during a resume. Real threads and the real locks: a resume of X is
// paused while it holds the spawn lock, or (spawn lock held) X's mailbox barrier. Meanwhile another thread acts on
// another member B (a running member of another conversation): `send` to B, `read` as B, `stop` B, or `spawn` a new
// member. Each completes, or refuses `lock` within LOCK_WAIT; none waits without bound. read and stop take no lock
// the resume holds, so they finish while it is still paused; spawn and send (the team lock, #103 4a) wait for the
// spawn lock and finish within LOCK_WAIT (plus a margin) after the resume gives it back. The lock wait is the real
// lock's, on the real clock (the seam's), and no case comes near it.
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
            // B is a bystander, not a resume subject: it stays pre-lifecycle, so `stop B` is the legacy stop that
            // takes no lock the resume holds (a lifecycle stop's cascade waits on the team lock).
            let b = pre_member(
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
                    // prelife-1c: a lifecycle member's terminal carries its generation's `launch.json` hcName.
                    t["name"] = json!(format!("m{}", &r["id"].as_str().unwrap()[..8]));
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
                        // A send takes the team lock (#103 4a), so it runs after the resume: Y is committed at t1
                        // and hc cannot read it here.
                        v.panes(&[]);
                        cf::online(&v);
                        cf::listing(&v, &team, &[]);
                        v.fakes
                            .hc_expect(&["get", "t1", "--json"], Err(Refusal::internal("busy")));
                        cf::get(&v, &addr(&xr), terminal(&xr, "exited"));
                        cf::get(&v, &addr(&br), terminal(&br, "running"));
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
                        // prelife-1c P-R3: the validating get (Running) is the last hc call before the kill.
                        cf::get(&v, &addr(&br), terminal(&br, "running"));
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
                // spawn and send need the spawn lock the resume holds: let the resume go once it is waiting.
                let waits = verb == "spawn" || verb == "send";
                if waits {
                    let go = go_tx.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(300));
                        go.send(()).unwrap();
                    });
                }
                let got = match verb {
                    "send" => crate::agents::message::send(&v.ctx, &request),
                    "read" => crate::agents::message::read(&v.ctx, &request),
                    "stop" => crate::agents::message::stop_hc(&v.ctx, &request),
                    _ => crate::agents::spawn::spawn(&v.ctx, &request),
                };
                let done = Instant::now();
                if !waits {
                    go_tx.send(()).unwrap();
                }
                let fixture = v.fakes.hc_done();
                (got, started, done, fixture, waits)
            });
            let resumed = resume_of(&w, ID);
            let (got, started, done, fixture, waits) = worker.join().unwrap();
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
            if waits {
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
                "read" => assert_eq!(got.unwrap()["ids"], json!([]), "{case}"),
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

// E2: a lifecycle member resumes with exactly its stored options: never the spec flags, the prompt or a session
// choice. The successor is still launched pre-lifecycle, so it keeps neither mark, options nor prompt (A3 Task 10).
#[test]
fn a_lifecycle_member_resumes_with_its_stored_options_only() {
    let w = World::human("");
    let x = member(
        &w,
        ID,
        json!({"client": "claude", "pane": "%5", "window": "@1", "depth": 1,
        "lifecycle": 1, "options": ["--model", "x"], "prompt": ["fix"]}),
    );
    let f = w.folder(ID);
    let publish = |file: &str, v: Value| {
        crate::state::evidence::publish(&crate::agents::gens::path(&f, 0, file), &v).unwrap();
    };
    publish("commit", json!({"committed": true}));
    publish("ended", json!({"state": "finished", "reason": "exited"}));
    let name = format!("m{}", &ID[..8]);
    crate::state::evidence::publish(
        &crate::agents::gens::path(&f, 0, "launch"),
        &json!({"hcName": name}),
    )
    .unwrap();
    write(
        &f.join("view.json"),
        json!({"pane": x["pane"], "window": x["window"], "startCommand": x["startCommand"], "gen": 0, "hcAddress": x["hcAddress"], "hcName": name}),
    );
    let before = folders(&w);
    w.panes(&[]);
    facts(&w, &[(&x, "exited")]);
    launches(&w, "t1");
    resume_of(&w, ID).unwrap();
    let y = new_folder(&w, &before);
    let spec = read_json(&gens::path(&y, 0, "spec")).unwrap().unwrap();
    assert_eq!(
        (&spec["flags"], &spec["args"]),
        (&json!(["--model", "x"]), &json!(["--resume", SESSION_ID]))
    );
    let r = record_of(&w, y.file_name().unwrap().to_str().unwrap());
    assert_eq!(
        (&r["lifecycle"], &r["options"]),
        (&json!(1), &json!(["--model", "x"]))
    );
    assert!(r.get("prompt").is_none(), "a resume never carries the task");
    w.fakes.hc_done().unwrap();
}

// E2: stored options that carry a session choice or a prompt, or are not text, launch nothing and make nothing.
#[test]
fn a_lifecycle_member_whose_options_are_not_pure_is_refused() {
    for (options, check) in [
        (json!(["--resume", "s"]), "usage"),
        (json!(["fix"]), "usage"),
        (json!(["--model", 1]), "internal"),
        (json!(null), "internal"),
    ] {
        let w = World::human("");
        let x = member(
            &w,
            ID,
            json!({"client": "claude", "pane": "%5", "window": "@1", "depth": 1,
            "lifecycle": 1, "options": options.clone()}),
        );
        let f = w.folder(ID);
        let publish = |file: &str, v: Value| {
            crate::state::evidence::publish(&crate::agents::gens::path(&f, 0, file), &v).unwrap();
        };
        publish("commit", json!({"committed": true}));
        publish("ended", json!({"state": "finished", "reason": "exited"}));
        let name = format!("m{}", &ID[..8]);
        crate::state::evidence::publish(
            &crate::agents::gens::path(&f, 0, "launch"),
            &json!({"hcName": name}),
        )
        .unwrap();
        write(
            &f.join("view.json"),
            json!({"pane": x["pane"], "window": x["window"], "startCommand": x["startCommand"], "gen": 0, "hcAddress": x["hcAddress"], "hcName": name}),
        );
        w.panes(&[]);
        facts(&w, &[(&x, "exited")]);
        launches(&w, "t1");
        let before = crate::testing::tree(&w.state());
        let e = resume_of(&w, ID).unwrap_err();
        assert_eq!(e.check, check, "{options}");
        assert_eq!(runs(&w), 0, "{options}: nothing launched");
        assert_eq!(
            crate::testing::tree(&w.state()),
            before,
            "{options}: nothing made"
        );
    }
}

// E2: replayed options are held to the team ceiling; outside a team there is none, and replay still refuses a
// prompt or a session choice.
#[test]
fn the_replay_ceiling_is_the_teams_else_bypass() {
    use crate::agents::{
        options::replay,
        team::{Policy, Team, permission_flags},
    };
    use crate::parse::Client;
    assert_eq!(ceiling(None), Policy::Bypass);
    let team = Team {
        team: "t".into(),
        session: "$3".into(),
        name: "s".into(),
        directory: "/work".into(),
        client: Client::Claude,
        flags: Vec::new(),
        policy: Policy::Default,
        leader: "111111111111".into(),
        created_at: "x".into(),
    };
    assert_eq!(ceiling(Some(&team)), Policy::Default);
    let above = permission_flags(Client::Claude, Policy::Bypass);
    assert_eq!(
        replay(Client::Claude, &above, ceiling(Some(&team)))
            .unwrap_err()
            .check,
        "ceiling"
    );
    let bypass = Team {
        policy: Policy::Bypass,
        ..team
    };
    assert_eq!(ceiling(Some(&bypass)), Policy::Bypass);
    assert_eq!(
        replay(Client::Claude, &above, ceiling(Some(&bypass))),
        Ok(above.clone())
    );
}

// The one choice of flags: the options of a lifecycle record, never its spec; the spec of any other member.
#[test]
fn a_lifecycle_records_flags_are_its_options_never_its_spec() {
    let w = World::human("");
    let mut r = member(&w, ID, json!({"lifecycle": 1, "options": ["--model", "x"]}));
    let key = w.caller().server_key;
    let flags = |r: &Value| member_flags(&w.ctx.state_root, &key, ID, r).unwrap();
    assert_eq!(flags(&r), ["--model", "x"]);
    r.as_object_mut().unwrap().remove("lifecycle");
    assert_eq!(flags(&r), client_flags(client_named("claude").unwrap()));
}

// R1, R2 (af434f CRITICAL 4; audit C3): a lifecycle successor is judged by its own generations, never gens/0.claim
// or abort.json; an unreadable marker or record takes the member (never reused on a guess).
#[test]
fn a_lifecycle_successor_is_judged_by_its_own_generations() {
    use crate::{agents::gens, state::evidence::publish};
    let w = World::human("");
    let caller = w.caller();
    let y = w.uncommitted(
        Y,
        json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd", "resumedFrom": ID}),
    );
    std::fs::create_dir_all(y.join("gens")).unwrap();
    std::fs::write(gens::claim_path(&y, 0), "\"abandoned\"").unwrap();
    write(&y.join("abort.json"), json!({"runUncertain": false}));
    publish(&gens::path(&y, 1, "commit"), &json!({"committed": true})).unwrap();
    let got = || taken(&w.ctx, &caller, ID).unwrap();
    assert_eq!(
        got(),
        Some(Y.to_string()),
        "generation 1 ran; gens/0.claim and abort.json say nothing"
    );
    write(
        &y.join("record.json"),
        json!({"lifecycle": 1, "resumedFrom": ID, "session": "$3", "pending": {"gen": 2}}),
    );
    assert_eq!(got(), Some(Y.to_string()), "pending 2 undecided");
    publish(&gens::path(&y, 2, "commit"), &json!({"aborted": true})).unwrap();
    assert_eq!(
        got(),
        Some(Y.to_string()),
        "generation 1 ran, though the record no longer names it (G5)"
    );
    std::fs::remove_dir_all(gens::dir(&y, 1)).unwrap();
    assert_eq!(got(), None, "a void first launch takes nothing");
    std::fs::write(gens::claim_path(&y, 2), b"\"aban").unwrap();
    assert_eq!(got(), Some(Y.to_string()), "a torn claim takes it");
    for (n, file, v) in [
        (3, "ended", json!({"state": "finished"})),
        (4, "released", json!({})),
    ] {
        let r = json!({"lifecycle": 1, "resumedFrom": ID, "session": "$3", "pending": {"gen": n}});
        write(&y.join("record.json"), r);
        publish(&gens::path(&y, n, file), &v).unwrap();
        assert_eq!(
            got(),
            Some(Y.to_string()),
            "pending {n} only {file}: it ran, never void"
        );
    }
    write(
        &y.join("record.json"),
        json!({"lifecycle": 1, "generation": "01", "resumedFrom": ID}),
    );
    assert_eq!(
        got(),
        Some(Y.to_string()),
        "an unreadable successor takes it"
    );
}

// R3 (audits C1, C2, C9, C11; re-audit a0586ee): a lifecycle member is ended only when every generation it names is
// done by recorded evidence; generation 0's end and a stale folder notice never end generation 1; a pending
// generation that only ended or was released ran and is done; hc's finished terminal never ends it; Unreadable
// refuses. Each case in a fresh world, hc's finished terminal queued after the connection, so a fallback to hc
// would answer Ended.
#[test]
fn a_lifecycle_member_ends_only_by_its_recorded_completion() {
    use crate::{agents::gens, state::evidence::publish};
    let end = json!({"state": "finished"});
    for (case, want) in [
        ("hc exited, nothing recorded", Ok(false)),
        ("generation 0 ended, 1 not", Ok(false)),
        ("ended recorded", Ok(true)),
        ("completing", Ok(false)),
        ("torn ended", Err("evidence")),
        ("never ran", Ok(true)),
        ("pending undecided", Ok(false)),
        ("pending aborted", Ok(true)),
        ("pending ended", Ok(true)),
        ("pending released", Ok(true)),
        ("01", Err("internal")),
    ] {
        let w = World::human("");
        cf::online(&w);
        let core = crate::hc::connect(&w.ctx).unwrap();
        let mut r = cf::member(
            &w,
            ID,
            json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd"}),
        );
        let f = w.folder(ID);
        std::fs::create_dir_all(gens::dir(&f, 1)).unwrap();
        // Every case: generation 0's end and a folder Stopped notice, which say nothing of generation 1 (S7).
        publish(&gens::path(&f, 0, "ended"), &end).unwrap();
        write(&f.join("notice.json"), json!({"kind": "stopped"}));
        // Every arm is a unit block: `publish` returns `Published`.
        match case {
            "ended recorded" => {
                publish(&gens::path(&f, 1, "ended"), &end).unwrap();
            }
            "completing" => {
                r["completing"] = json!({"gen": 1});
                publish(&gens::path(&f, 1, "ended"), &end).unwrap();
            }
            "torn ended" => std::fs::write(gens::path(&f, 1, "ended"), b"{\"st").unwrap(),
            "never ran" | "pending undecided" | "pending aborted" | "pending ended"
            | "pending released" => {
                r.as_object_mut().unwrap().remove("generation");
                if case != "never ran" {
                    r["pending"] = json!({"gen": 1});
                }
                let marker = match case {
                    "pending aborted" => Some(("commit", json!({"aborted": true}))),
                    "pending ended" => Some(("ended", end.clone())),
                    "pending released" => Some(("released", json!({}))),
                    _ => None,
                };
                if let Some((file, v)) = marker {
                    publish(&gens::path(&f, 1, file), &v).unwrap();
                }
            }
            "01" => r["generation"] = json!("01"),
            _ => {}
        }
        write(&f.join("record.json"), r.clone());
        let mut t = cf::wire(&r);
        (t["state"], t["reason"], t["exit_code"]) = (json!("finished"), json!("exited"), json!(0));
        cf::get(&w, r["hcAddress"].as_str().unwrap(), t);
        let member = gens::member_of(&f);
        assert_eq!(
            ended(&core, &f, ID, &r, &member).map_err(|e| e.check),
            want,
            "{case}"
        );
    }
}

// R2, R5: a member folder that cannot be read holds the session; it is never read as ended.
#[test]
fn an_unreadable_member_holds_the_session() {
    let w = World::human("");
    let y = w.uncommitted(
        Y,
        json!({"lifecycle": 1, "generation": "01", "sessionId": SESSION_ID}),
    );
    write(&y.join("abort.json"), json!({"runUncertain": false}));
    cf::online(&w);
    let core = crate::hc::connect(&w.ctx).unwrap();
    assert!(
        session_held(&w.ctx, &core, ID, SESSION_ID).is_some(),
        "unclear evidence is never free"
    );
}

// R3 (re-audit a0586ee CRITICAL; final re-audit): every conversation a lifecycle record names, the active `sessionId`
// and `pending.sessionId`, is held while either named generation is unfinished; both are free once both are done.
#[test]
fn a_pending_conversation_is_held_until_its_generation_is_done() {
    use crate::{agents::gens, state::evidence::publish};
    let other = "11111111-2222-4333-8444-555555555555";
    let end = json!({"state": "finished"});
    for first in [1u64, 2] {
        let w = World::human("");
        let y = w.uncommitted(
            Y,
            json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd", "sessionId": other,
            "pending": {"gen": 2, "hcName": "m4567cdef", "sessionId": SESSION_ID}}),
        );
        publish(&gens::path(&y, 1, "commit"), &json!({"committed": true})).unwrap();
        publish(&gens::path(&y, 2, "commit"), &json!({"committed": true})).unwrap();
        cf::online(&w);
        let core = crate::hc::connect(&w.ctx).unwrap();
        // [the active generation's conversation, the pending generation's]
        let held = || [other, SESSION_ID].map(|s| session_held(&w.ctx, &core, ID, s).is_some());
        assert_eq!(
            held(),
            [true, true],
            "first {first}: neither generation done: both held"
        );
        publish(&gens::path(&y, first, "ended"), &end).unwrap();
        assert_eq!(
            held(),
            [true, true],
            "only generation {first} done: both still held"
        );
        publish(&gens::path(&y, 3 - first, "ended"), &end).unwrap();
        assert_eq!(
            held(),
            [false, false],
            "first {first}: both generations done: both free"
        );
    }
}

// settle-1b G5, b-calls 10: a successor whose committed generation was settled (core death: Lost, Normal promotes
// then releases; Lost or Ended in Fence leaves a record naming no generation) ran, so its predecessor's task is
// never eligible again: `taken` names it and `ran_nothing` (up discover, tree scope) says it ran. A torn unnamed
// generation holds it (`taken`) and refuses (`ran_nothing`, `evidence`).
#[test]
fn a_fenced_successor_keeps_its_predecessor_taken() {
    use crate::agents::settle::{Mode, Outcome};
    use crate::agents::team_state::Tx;
    use crate::agents::{core_fixture as cf, gens, life_fixture as lf};
    for case in ["normal lost", "fence lost", "fence ended", "torn unnamed"] {
        let (w, s) = lf::world();
        let m = if case == "torn unnamed" {
            let m = lf::life(&w, json!({"resumedFrom": Y}));
            lf::seed_gen(&m, 0, Some("abandoned"), None);
            std::fs::create_dir_all(gens::dir(&m, 1)).unwrap();
            std::fs::write(gens::path(&m, 1, "commit"), b"{").unwrap();
            m
        } else {
            let mut p = lf::pending(0);
            p["kind"] = json!("resume");
            let m = lf::life(&w, json!({"resumedFrom": Y, "pending": p}));
            lf::seed_gen(&m, 0, Some("run"), lf::committed());
            cf::online(&w);
            if case == "fence ended" {
                lf::found(&w, 0, &lf::ended(&w, 0));
            } else {
                lf::found(&w, 0, &lf::lost(&w, 0, lf::LOST_ARGVS[0]));
                lf::get(&w, "t0", &lf::lost(&w, 0, lf::LOST_ARGVS[0]));
                lf::relabel(&w, "t0");
            }
            let mode = if case == "normal lost" {
                Mode::Normal
            } else {
                Mode::Fence
            };
            let got = Tx {
                ctx: &w.ctx,
                scope: s.clone(),
            }
            .settle(ID, mode)
            .unwrap();
            assert!(
                matches!(got, Outcome::Promoted | Outcome::Done),
                "{case}: {got:?}"
            );
            m
        };
        assert_eq!(
            taken(&w.ctx, &w.caller(), Y).unwrap(),
            Some(ID.to_string()),
            "{case}: the task is free again"
        );
        let ran =
            crate::agents::up::discover::ran_nothing(&m, &gens::member_of(&m)).map_err(|e| e.check);
        let want = if case == "torn unnamed" {
            Err("evidence")
        } else {
            Ok(false)
        };
        assert_eq!(ran, want, "{case}: ran nothing");
    }
}

// Checklist 16, PM call 1: a successor whose view fails after the commit stays; resume's reply carries viewError.
#[test]
fn a_resume_whose_view_fails_after_the_commit_keeps_the_successor() {
    let w = World::human("");
    let x = member(&w, ID, json!({"pane": "%5", "window": "@1", "depth": 1}));
    let before = folders(&w);
    w.panes(&[]);
    facts(&w, &[(&x, "exited")]);
    launches(&w, "t1");
    w.fakes.mux_first("new-window", 1, "", "no room");
    let reply = resume_of(&w, ID).unwrap();
    assert_eq!(reply["view"], json!("none"), "{reply:?}");
    assert!(reply["viewError"].is_string(), "{reply:?}");
    let y = new_folder(&w, &before);
    assert!(gens::committed(&y, 0).unwrap(), "the successor stays");
    assert!(!y.join("view.json").exists());
    assert!(
        !w.fakes.hc_calls().iter().any(|(_, a, _)| a[0] == "kill"),
        "nothing is rolled back"
    );
    w.fakes.hc_done().unwrap();
}

// cascade-3d C3: a member a kept journal lists is not resumed: 2 `closed`, nothing launched.
#[test]
fn a_resume_under_a_kept_journal_is_refused_closed() {
    let w = World::human("");
    let x = member(&w, ID, json!({}));
    facts(&w, &[(&x, "stopped")]);
    crate::agents::cascade_fixture::seed(&w, ID, &[ID], true);
    let before = folders(&w);
    assert_eq!(resume_of(&w, ID).unwrap_err().check, "closed");
    assert_eq!(folders(&w), before, "nothing launched");
}

// adopt-3e A6, call 13: resume replays before its snapshot: two journals refuse `topology`; the snapshot's scripted
// hc answers are never asked for; nothing launched.
#[test]
fn resume_admission_replays_first() {
    use crate::agents::adopt_fixture as ax;
    let w = World::human("");
    let x = member(&w, ID, json!({}));
    facts(&w, &[(&x, "stopped")]);
    let s = crate::agents::team_state::Scope::of(&w.caller());
    for n in [1, 3] {
        ax::put_journal(&w, &s, &ax::to_a(n));
    }
    let before = folders(&w);
    let got = resume_of(&w, ID).unwrap_err();
    assert_eq!(
        (got.check, got.message.as_str()),
        ("topology", crate::agents::adopt::OUT_OF_ORDER)
    );
    assert_eq!(folders(&w), before, "nothing launched");
    assert!(
        w.fakes.hc_done().is_err(),
        "the snapshot's answers were never asked for"
    );
}

/// prelife-1c: a pre-lifecycle member as main writes one (main's `member`): its record and its folder `spec.json`.
fn pre_member(w: &World, id: &str, fields: Value) -> Value {
    let r = cf::member(w, id, fields);
    let client = client_named(r["client"].as_str().unwrap()).unwrap();
    write(
        &w.folder(id).join("spec.json"),
        json!({"schema": 1, "id": id, "client": r["client"], "flags": client_flags(client),
            "args": ["--session-id", r["sessionId"]], "generation": "0"}),
    );
    r
}

/// G10d, call 16, through the real dispatcher with no hc installed: `resume` of a pre-lifecycle member, by name
/// or id, Ended or not, read as an identity or not, is refused 2 `state` from its record before any hc call.
/// Another session's folder, and a name a lifecycle member also has, are not refused here.
#[test]
fn a_pre_lifecycle_member_is_never_resumed_even_without_hc() {
    use crate::agents::prelife_fixture as pf;
    let c = "c0000000000c";
    for case in [
        "hc member",
        "stopped notice",
        "pane era",
        "partial",
        "other session",
        "lifecycle twin",
    ] {
        for by in ["name", "id"] {
            let w = World::human(""); // no hc installed: `hc::connect` would refuse
            w.panes(&[]);
            match case {
                "pane era" => drop(w.agent_record(c, json!({"name": "legacy"}))),
                "other session" => {
                    drop(w.agent_record(c, json!({"name": "legacy", "session": "$4"})))
                }
                _ => {
                    let r = pre_member(&w, c, json!({"name": "legacy"}));
                    if case == "stopped notice" {
                        write(
                            &w.folder(c).join("notice.json"),
                            json!({"kind": "stopped", "exitCode": null, "at": "x"}),
                        );
                    }
                    if case == "partial" {
                        let mut p = r.clone();
                        p.as_object_mut().unwrap().remove("launcherArgv");
                        write(&w.folder(c).join("record.json"), p);
                    }
                    if case == "lifecycle twin" {
                        cf::member(&w, "e0000000000e", json!({"name": "legacy"}));
                        pf::to_lifecycle(&w, "e0000000000e", true);
                    }
                }
            }
            let refused = !(case == "other session" || (case == "lifecycle twin" && by == "name"));
            let before = (pf::bytes(&w.folder(c)), folders(&w));
            let target = if by == "id" { c } else { "legacy" };
            let (code, out) =
                crate::cli::run(&["resume".to_string(), target.to_string()], || Ok(&w.ctx));
            let pre =
                code == 2 && out.contains(r#""check":"state""#) && out.contains("pre-lifecycle");
            assert_eq!(pre, refused, "{case} by {by}: {out}");
            if refused {
                assert!(
                    w.fakes.hc_calls().is_empty(),
                    "{case} by {by}: refused before any hc call"
                );
            }
            assert_eq!(runs(&w), 0, "{case} by {by}: nothing launched");
            assert_eq!(
                (pf::bytes(&w.folder(c)), folders(&w)),
                before,
                "{case} by {by}"
            );
        }
    }
}

/// Call 20: admission checks the subject it selected. An older lifecycle `legacy` (Ended) passes the offline guard,
/// but the name selects the newer pre-lifecycle `legacy` (Ended too): refused `state`, no `hc run`.
#[test]
fn admission_refuses_a_selected_pre_lifecycle_subject() {
    use crate::agents::prelife_fixture as pf;
    let w = World::human("");
    w.panes(&[]);
    let old = pre_member(
        &w,
        "b0000000000b",
        json!({"name": "legacy", "createdAt": "2026-01-01T00:00:00.0000000Z"}),
    );
    pf::to_lifecycle(&w, "b0000000000b", true);
    let new = pre_member(
        &w,
        "c0000000000c",
        json!({"name": "legacy", "createdAt": "2026-06-01T00:00:00.0000000Z"}),
    );
    let before = folders(&w);
    snapshot(&w, &[(&old, "exited"), (&new, "exited")]);
    let e = resume_of(&w, "legacy").unwrap_err();
    assert_eq!((e.code, e.check), (2, "state"), "{}", e.message);
    assert!(e.message.contains("pre-lifecycle"), "{}", e.message);
    assert_eq!(runs(&w), 0);
    assert_eq!(folders(&w), before);
    w.fakes.hc_done().unwrap();
}

/// Call 20: an exact id decides before any name. A lifecycle member whose name is the pre-lifecycle folder's id
/// (and whose own id sorts first) does not let `resume <that id>` through.
#[test]
fn an_exact_id_decides_before_a_same_named_member() {
    use crate::agents::prelife_fixture as pf;
    let c = "c0000000000c";
    let w = World::human(""); // no hc installed
    w.panes(&[]);
    cf::member(&w, "b0000000000b", json!({"name": c}));
    pf::to_lifecycle(&w, "b0000000000b", true);
    pre_member(&w, c, json!({"name": "legacy"}));
    let (code, out) = crate::cli::run(&["resume".to_string(), c.to_string()], || Ok(&w.ctx));
    assert!(
        code == 2 && out.contains(r#""check":"state""#) && out.contains("pre-lifecycle"),
        "{out}"
    );
    assert!(w.fakes.hc_calls().is_empty(), "refused before any hc call");
}
