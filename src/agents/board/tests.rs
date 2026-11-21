use super::*;

// views-6f ambiguity 14: only a watch of a team with a lifecycle member lists kept shells; `list` and a
// pre-lifecycle team make no new hc call.
#[test]
fn only_a_lifecycle_watch_lists_kept_shells() {
    let life = serde_json::json!({"lifecycle": 1});
    let pre = serde_json::json!({"name": "old"});
    assert!(lists_kept(true, [&pre, &life]));
    assert!(!lists_kept(false, [&life]), "list (no watch)");
    assert!(!lists_kept(true, [&pre]), "a pre-lifecycle team");
    assert!(!lists_kept(true, std::iter::empty::<&Value>()), "no member");
}

// The field: rows `[{address, name, team}]`; a listing failure is `kept: []` plus `keptError` (display only).
#[test]
fn the_kept_field_is_display_only() {
    use crate::agents::kept::Shell;
    let mut reply = Map::new();
    add_kept(&mut reply, None);
    assert!(reply.is_empty(), "nothing to list: no field");
    add_kept(
        &mut reply,
        Some(Ok(vec![Shell {
            address: "k1".into(),
            name: "a".into(),
            team: "t".into(),
        }])),
    );
    assert_eq!(
        Value::Object(reply.clone()),
        serde_json::json!({"kept": [{"address": "k1", "name": "a", "team": "t"}]})
    );
    let mut failed = Map::new();
    add_kept(
        &mut failed,
        Some(Err(crate::error::Refusal::internal("boom"))),
    );
    assert_eq!(
        (&failed["kept"], failed["keptError"].is_string()),
        (&serde_json::json!([]), true)
    );
}

// Audit 6-10 F10, row 6.3: a real lifecycle watch whose kept listing fails still answers, with `keptError`.
// The snapshot's hc script follows the merged `Snapshot::take`; a deviation changes the script, never the asserts.
#[test]
fn a_lifecycle_watch_survives_a_failing_kept_listing() {
    use crate::agents::{core_fixture as cf, life_fixture as lf};
    let w = crate::agents::fixture::World::human("");
    let caller = w.caller();
    let f = lf::life(
        &w,
        serde_json::json!({"generation": "0", "hcAddress": "t0", "hcName": lf::hc_name(0),
        "launcherArgv": lf::ARGV}),
    );
    lf::seed_gen(&f, 0, None, lf::committed());
    cf::online(&w);
    cf::listing(&w, &lf::team(&w), &["t0"]);
    lf::get(&w, "t0", &lf::running(&w, 0));
    cf::answer(&w, &["--version"], "hc 0.1.21");
    let ls = [
        "ls".to_string(),
        "--label".into(),
        format!("mux.kept={}", lf::team(&w)),
        "--json".into(),
    ];
    let ls: Vec<&str> = ls.iter().map(String::as_str).collect();
    w.fakes.hc_expect(
        &ls,
        Ok(crate::spawn::Out {
            code: 1,
            stdout: String::new(),
            stderr: "boom".into(),
        }),
    );
    let reply = board(&w.ctx, &caller, true, true).unwrap();
    assert_eq!(reply["kept"], serde_json::json!([]), "{reply:?}");
    assert!(reply["keptError"].is_string(), "{reply:?}");
    w.fakes.hc_done().unwrap();
}

#[test]
fn a_reported_done_is_done() {
    let now = crate::util::parse_utc("2026-03-01T00:01:00Z").unwrap();
    let r = json!({"state": "done", "kind": null, "since": "2026-03-01T00:00:00.0000000Z"});
    assert_eq!(activity(Some(&r), None, None, now), ("done", Some(60)));
    // Seen (the bar's Space, or the person looked at it): the spell reads idle, so the row leaves the Inbox; a seen
    // mark older than the spell does not count.
    let seen = json!({"since": "2026-03-01T00:00:00.0000000Z"});
    assert_eq!(
        activity(Some(&r), Some(&seen), None, now),
        ("idle", Some(60))
    );
    let older = json!({"since": "2026-02-28T23:59:00.0000000Z"});
    assert_eq!(
        activity(Some(&r), Some(&older), None, now),
        ("done", Some(60))
    );
}

#[test]
fn on_call_ranks_below_needs_you_and_above_working() {
    for (word, on, want) in [
        ("working", true, "on call"),
        ("done", true, "on call"),
        ("idle", true, "on call"),
        ("blocked", true, "blocked"),
        ("working", false, "working"),
    ] {
        assert_eq!(listening(word, on), want, "{word} {on}");
    }
}

#[test]
fn the_board_ranks_on_call_below_needs_you_above_done_and_working() {
    let w = World::human("");
    w.clock(NOW);
    let caller = w.caller();
    let ids = [
        "0000000000a1",
        "0000000000b2",
        "0000000000c3",
        "0000000000d4",
    ];
    let names = ["wk", "dn", "oc", "nd"];
    let mut folders = Vec::new();
    let mut panes = Vec::new();
    for (i, (id, name)) in ids.iter().zip(names).enumerate() {
        folders.push(member(
            &w,
            id,
            json!({"name": name, "pane": format!("%{}", 10 + i)}),
        ));
        panes.push(format!("%{}|@5|{}|{id}|{name}", 10 + i, 300 + i));
    }
    let panes: Vec<&str> = panes.iter().map(String::as_str).collect();
    w.panes(&panes);
    let report = |i: usize, state: &str| {
        write(
            &folders[i].join("activity.json"),
            json!({"state": state, "kind": null, "since": "2026-10-29T11:59:00.0000000Z"}),
        )
    };
    report(0, "working");
    report(1, "idle");
    report(2, "idle");
    report(3, "blocked");
    w.fakes.process(crate::testing::row(700, 302, 7000));
    write(
        &folders[2].join("waiting-700.json"),
        json!({"pid": 700, "start": 7000, "since": "2026-10-29T11:59:30.0000000Z",
            "owner": {"id": ids[2], "gen": "0"}, "until": "2026-10-29T12:10:00.0000000Z"}),
    );
    snapshot(
        &w,
        &caller,
        &ids.iter().map(|id| (*id, "running")).collect::<Vec<_>>(),
    );
    let reply = board(&w.ctx, &caller, false, true).unwrap();
    w.fakes.hc_done().unwrap();
    let got: Vec<String> = reply["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            format!(
                "{}={}",
                r["name"].as_str().unwrap(),
                r["activity"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(got, ["nd=blocked", "oc=on call", "dn=done", "wk=working"]);
}

#[test]
fn a_leader_with_a_live_lease_is_never_on_call() {
    let w = World::human("");
    w.clock(NOW);
    let caller = w.caller();
    let ids = ["0000000000a1", "0000000000b2"];
    let folders = [
        member(
            &w,
            ids[0],
            json!({"name": "lead", "pane": "%10", "depth": 0, "upTeam": "t"}),
        ),
        member(&w, ids[1], json!({"name": "mb", "pane": "%11", "depth": 1})),
    ];
    w.panes(&["%10|@5|300|0000000000a1|lead", "%11|@5|301|0000000000b2|mb"]);
    // W6: each holds a live lease of its own current owner.
    for (i, (folder, id)) in folders.iter().zip(ids).enumerate() {
        let pid = 700 + i as u32;
        w.fakes
            .process(crate::testing::row(pid, 300 + i as u32, 7000));
        write(
            &folder.join(format!("waiting-{pid}.json")),
            json!({"pid": pid, "start": 7000, "since": "2026-10-29T11:59:30.0000000Z",
                "owner": {"id": id, "gen": "0"}, "until": "2026-10-29T12:10:00.0000000Z"}),
        );
    }
    snapshot(
        &w,
        &caller,
        &ids.iter().map(|id| (*id, "running")).collect::<Vec<_>>(),
    );
    let reply = board(&w.ctx, &caller, false, true).unwrap();
    w.fakes.hc_done().unwrap();
    let activity = |name: &str| {
        reply["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == name)
            .map(|r| r["activity"].clone())
            .unwrap()
    };
    assert_eq!(activity("mb"), "on call", "the member's lease shows");
    assert_ne!(activity("lead"), "on call", "the leader is never on call");
}

#[test]
fn the_leader_shows_its_badge() {
    let w = World::human("");
    let leader = json!({"depth": 0, "upTeam": "t"});
    crate::agents::mail::add_mail(&w.ctx, &w.caller(), "111111111111", "a", "%5", "hi", None)
        .unwrap();
    assert_eq!(
        leader_badge(&w.ctx, &w.caller(), "111111111111", &leader),
        Some(1)
    );
    assert_eq!(
        leader_badge(&w.ctx, &w.caller(), "111111111111", &json!({"depth": 1})),
        None,
        "a member: none"
    );
}
use crate::agents::fixture::{ID, SESSION_ID, World, write};
use crate::agents::lock_key;
use crate::agents::mail::{add_mail, read_mail};
use serde_json::json;

use std::path::{Path, PathBuf};

use crate::agents::core_fixture as cf;

// Preserve the pane-authenticated launch while adding hc membership facts.
fn member(w: &World, id: &str, mut fields: Value) -> PathBuf {
    let pane = fields.get("pane").and_then(Value::as_str).unwrap_or("%9");
    fields["startCommand"] = crate::agents::fixture::launch_command(pane).into();
    cf::member(w, id, fields);
    w.folder(id)
}

// One finite board snapshot. Lifecycle is explicit, never inferred from notices.
fn snapshot(w: &World, caller: &Caller, facts: &[(&str, &str)]) {
    cf::online(w);
    cf::listing(w, &super::super::membership::team(caller), &[]);
    let mut facts = facts.to_vec();
    facts.sort_by_key(|(id, _)| *id);
    for (id, life) in facts {
        let record = read_json(
            &super::super::server_dir(&w.ctx, caller)
                .join("agents")
                .join(id)
                .join("record.json"),
        )
        .unwrap()
        .unwrap();
        let mut terminal = cf::wire(&record);
        match life {
            "running" => {}
            "lost" => terminal["argv"] = json!(["/bin/sh", "-l"]),
            "unknown" => terminal["labels"]["mux.gen"] = json!("1"),
            "stopped" => {
                terminal["state"] = json!("finished");
                terminal["reason"] = json!("killed");
            }
            code => {
                terminal["state"] = json!("finished");
                terminal["reason"] = json!("exited");
                terminal["exit_code"] = json!(code.parse::<i32>().unwrap());
            }
        }
        cf::get(w, record["hcAddress"].as_str().unwrap(), terminal);
    }
}

const ENDED_FACTS: &[(&str, &str)] = &[
    (ID, "running"),
    ("bbbbbbbbbbbb", "3"),
    ("cccccccccccc", "stopped"),
    ("dddddddddddd", "lost"),
    ("111111111111", "unknown"),
    ("222222222222", "lost"),
    ("333333333333", "stopped"),
];

const NOW: &str = "2026-10-29T12:00:00.0000000Z";

// Every file below `dir` with its bytes.
fn tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                folders.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                files.push((path, bytes));
            }
        }
    }
    files.sort();
    files
}

fn names(reply: &Map<String, Value>) -> Vec<String> {
    reply["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            format!(
                "{}={}",
                row["name"].as_str().unwrap(),
                row["state"].as_str().unwrap()
            )
        })
        .collect()
}

#[test]
fn doing_status_then_message_then_task() {
    let s = |text: &str| json!(text);
    let cases = [
        (Some(s("s")), Some(s("m")), Some(s("t"))),
        (Some(s("")), Some(s("m")), Some(s("t"))),
        (Some(s(" ")), Some(s("")), Some(s("t"))),
        (None, None, None),
        (Some(Value::Null), Some(Value::Null), Some(Value::Null)),
        (Some(s("a\tb\u{9b}\nc")), Some(s("m")), Some(s("t"))),
        (Some(json!(5)), None, None),
    ];
    let got: Vec<String> = cases
        .iter()
        .map(|(a, b, c)| {
            let (doing, from) = select_doing(a.as_ref(), b.as_ref(), c.as_ref());
            format!("{from}={doing}")
        })
        .collect();
    assert_eq!(
        got,
        [
            r#""status"="s""#,
            r#""message"="m""#,
            r#""task"="t""#,
            "null=null",
            "null=null",
            r#""status"="a b""#,
            r#""status"="5""#
        ]
    );
}

#[test]
fn board_row_field_order() {
    let w = World::human("");
    w.clock(NOW);
    let caller = w.caller();
    member(
        &w,
        ID,
        json!({"createdAt": "2026-10-29T11:56:00.0000000Z", "model": "sonnet", "role": "x", "color": "magenta"}),
    );
    member(
        &w,
        "bbbbbbbbbbbb",
        json!({"name": "idle", "pane": "%8", "client": "copilot", "taskLine": "",
                "createdAt": "2026-10-29T11:59:55.0000000Z"}),
    );
    w.panes(&[
        "%9|@6|300|aaaaaaaaaaaa|worker",
        "%8|@7|301|bbbbbbbbbbbb|idle",
    ]);
    for (from, pane) in [
        ("worker", "%9"),
        ("worker", "%9"),
        ("idle", "%8"),
        ("worker", "%4"),
    ] {
        add_mail(&w.ctx, &caller, "pane-1", from, pane, "x", None).unwrap();
    }
    let before = tree(&w.state());
    w.fakes.clear_log();
    snapshot(&w, &caller, &[(ID, "running"), ("bbbbbbbbbbbb", "running")]);
    let reply = board(&w.ctx, &caller, false, false).unwrap();
    w.fakes.hc_done().unwrap();
    let row = |name,
               client,
               pane,
               window,
               created,
               doing: Value,
               from: Value,
               age,
               unread,
               model: Value,
               role: Value,
               color: &str,
               address: &str| {
        json!({"name": name, "client": client, "pane": pane, "window": window, "context": "brief",
                "parent": "%1", "sessionId": SESSION_ID, "directory": "/work", "createdAt": created,
                "state": "running", "doing": doing, "doingFrom": from, "age": age, "unread": unread,
                "model": model, "role": role, "color": color, "activity": "working", "since": null, "changedAt": null, "life":"running", "view":"attached", "hcAddress":address})
    };
    // A record without a model (the client's own) or a role lists null; one without a color gets its name's hue.
    let want = json!({"running": 2, "agents": [
        row("worker", "claude", "%9", "@6", "2026-10-29T11:56:00.0000000Z", json!("build it"), json!("task"), 240, 2, json!("sonnet"), json!("x"), "magenta", "taaaaaaaaaaaa"),
        row("idle", "copilot", "%8", "@7", "2026-10-29T11:59:55.0000000Z", Value::Null, Value::Null, 5, 1, Value::Null, Value::Null, crate::paint::identity(None, "idle"), "tbbbbbbbbbbbb")
    ]});
    // Compared as text: the order of keys is part of the reply.
    assert_eq!(Value::Object(reply).to_string(), want.to_string());
    // One multiplexer call (no probe, no capture-pane), no state write, the offset unmoved.
    let calls = w.fakes.mux_calls();
    assert_eq!(calls.len(), 1);
    assert!(crate::agents::fixture::list_call(&calls[0]), "{calls:?}");
    assert_eq!(tree(&w.state()), before);
    assert_eq!(read_mail(&w.ctx, &caller, "pane-1").unwrap().len(), 4);
}

// A live worker, terminal Ended, Lost/Unknown, another session and an unreadable notice time.
fn ended_world() -> (World, Caller) {
    let w = World::human("");
    w.clock(NOW);
    let caller = w.caller();
    member(&w, ID, json!({}));
    w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
    for (id, name, kind, code, at, session) in [
        ("bbbbbbbbbbbb", "done", "exited", json!(3), "11:59", "$3"),
        (
            "cccccccccccc",
            "halted",
            "stopped",
            Value::Null,
            "11:50",
            "$3",
        ),
        (
            "dddddddddddd",
            "vanished",
            "lost",
            Value::Null,
            "11:55",
            "$3",
        ),
        ("eeeeeeeeeeee", "other", "exited", json!(0), "11:58", "$4"),
        ("111111111111", "odd", "paused", Value::Null, "11:57", "$3"),
        ("222222222222", "late", "lost", Value::Null, "garbage", "$3"),
        ("333333333333", "tie", "stopped", Value::Null, "11:50", "$3"),
    ] {
        let folder = member(
            &w,
            id,
            json!({"name": name, "pane": "%7", "session": session}),
        );
        let at = if at == "garbage" {
            at.to_string()
        } else {
            format!("2026-10-29T{at}:00.0000000Z")
        };
        write(
            &folder.join("notice.json"),
            json!({"kind": kind, "exitCode": if code.is_null() || code == json!(3) {json!(99)} else {code}, "at": at}),
        );
    }
    (w, caller)
}

#[test]
fn ended_newest_first_max_10() {
    let (w, caller) = ended_world();
    snapshot(&w, &caller, ENDED_FACTS);
    let all = board(&w.ctx, &caller, true, false).unwrap();
    w.fakes.hc_done().unwrap();
    assert_eq!(all["running"], 1);
    // Only terminal Ended counts; Lost and Unknown remain separate, other sessions excluded.
    assert_eq!(all["endedTotal"], 3);
    let keys: Vec<&str> = all.keys().map(String::as_str).collect();
    assert_eq!(keys, ["running", "endedTotal", "agents"]);
    // Newest notice first, ties by id, an unreadable time last.
    assert_eq!(
        names(&all),
        [
            "worker=running",
            "odd=unknown",
            "late=lost",
            "vanished=lost",
            "done=exited 3",
            "tie=stopped",
            "halted=stopped"
        ]
    );
    snapshot(&w, &caller, ENDED_FACTS);
    let plain = board(&w.ctx, &caller, false, false).unwrap();
    w.fakes.hc_done().unwrap();
    assert_eq!(
        names(&plain),
        [
            "worker=running",
            "odd=unknown",
            "late=lost",
            "vanished=lost"
        ]
    );
    // Without -All the reply is unchanged: no endedTotal.
    assert!(plain.get("endedTotal").is_none());

    // prelife-1c G10a (p1c-calls 4: `list` does not fail): a pre-lifecycle record with no usable hc identity is a
    // held team slot, not a row and not a refusal.
    let bad = World::human("");
    member(
        &bad,
        "444444444444",
        json!({"id":"bbbbbbbbbbbb", "name":"foreign"}),
    );
    let shown = board(&bad.ctx, &bad.caller(), true, false).unwrap();
    assert_eq!(
        shown["agents"],
        json!([]),
        "no row for the unidentified record"
    );
    bad.fakes.hc_done().unwrap();
    // The live row and exactly the 10 newest of 12 ended agents.
    let w = World::human("");
    w.clock(NOW);
    let caller = w.caller();
    member(&w, "ffffffffffff", json!({}));
    w.panes(&["%9|@5|300|ffffffffffff|worker"]);
    for n in 0..12u32 {
        let id = format!("{n:012x}");
        let folder = member(
            &w,
            &id,
            json!({"name": format!("a{n}"), "pane": format!("%{}", 20 + n)}),
        );
        let minutes = (n * 5) % 12 + 1;
        let at = format!("2026-10-29T11:{:02}:00.0000000Z", 60 - minutes);
        write(
            &folder.join("notice.json"),
            json!({"kind": "stopped", "exitCode": null, "at": at}),
        );
    }
    let ids: Vec<_> = (0..12u32).map(|n| format!("{n:012x}")).collect();
    let mut facts: Vec<_> = ids.iter().map(|id| (id.as_str(), "stopped")).collect();
    facts.push(("ffffffffffff", "running"));
    snapshot(&w, &caller, &facts);
    let all = board(&w.ctx, &caller, true, false).unwrap();
    w.fakes.hc_done().unwrap();
    // Uncapped: 12 ended, 10 rows.
    assert_eq!(all["endedTotal"], 12);
    let got: Vec<String> = names(&all)
        .into_iter()
        .map(|n| n.split('=').next().unwrap().to_string())
        .collect();
    assert_eq!(
        (all["running"].clone(), got),
        (
            json!(1),
            [
                "worker", "a0", "a5", "a10", "a3", "a8", "a1", "a6", "a11", "a4", "a9"
            ]
            .map(String::from)
            .to_vec()
        )
    );
}

#[test]
fn lost_row_says_closed_by_pane() {
    let w = World::human("");
    w.clock(NOW);
    let caller = w.caller();
    for (id, name, notice) in [
        (
            "bbbbbbbbbbbb",
            "vanished",
            json!({"kind": "exited", "exitCode": 99, "at": "2026-10-29T11:59:00.0000000Z", "closedBy": "pane"}),
        ),
        (
            "cccccccccccc",
            "halted",
            json!({"kind": "stopped", "exitCode": null, "at": "2026-10-29T11:58:00.0000000Z"}),
        ),
    ] {
        let folder = member(&w, id, json!({"name": name, "pane": "%7"}));
        write(&folder.join("notice.json"), notice);
    }
    w.panes(&[]);
    // Metadata and exitCode follow the 16 prefix fields, then the hc suffix in both feeds.
    for watch in [false, true] {
        snapshot(
            &w,
            &caller,
            &[("bbbbbbbbbbbb", "lost"), ("cccccccccccc", "stopped")],
        );
        let all = board(&w.ctx, &caller, true, watch).unwrap();
        w.fakes.hc_done().unwrap();
        let rows = all["agents"].as_array().unwrap();
        let lost = rows[0].as_object().unwrap();
        assert_eq!(lost["name"], "vanished");
        assert_eq!(lost["closedBy"], "pane");
        assert_eq!(lost["life"], "lost");
        assert_eq!(lost["exitCode"], Value::Null);
        let keys: Vec<&str> = lost.keys().map(String::as_str).collect();
        assert_eq!(
            keys[keys.len() - 5..],
            ["closedBy", "exitCode", "life", "view", "hcAddress"]
        );
        assert!(rows[1].get("closedBy").is_none(), "{}", rows[1]);
    }
}

#[test]
fn ended_rows_carry_exit_code_in_list_and_watch() {
    let (w, caller) = ended_world();
    // list -All and the bar's feed alike: every ended row, never a live one.
    for watch in [false, true] {
        snapshot(&w, &caller, ENDED_FACTS);
        let reply = board(&w.ctx, &caller, true, watch).unwrap();
        w.fakes.hc_done().unwrap();
        let codes: Vec<String> = reply["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let row = row.as_object().unwrap();
                match row.get("exitCode") {
                    // hc suffix follows the terminal fields.
                    Some(code) => {
                        assert_eq!(row.keys().next_back().unwrap(), "hcAddress");
                        format!("{}={code}", row["name"].as_str().unwrap())
                    }
                    None => format!("{}=none", row["name"].as_str().unwrap()),
                }
            })
            .collect();
        assert_eq!(
            codes,
            [
                "worker=none",
                "odd=null",
                "late=null",
                "vanished=null",
                "done=3",
                "tie=null",
                "halted=null"
            ],
            "watch={watch}"
        );
    }
}

// An ended agent's row counts the reports it left unread (outside a team, in its parent pane's mailbox), never the
// exit notice the reaper posts for it: with only that notice it stays in the Ended fold; one unread report puts it in
// Inbox; once the report is read it goes back.
#[test]
fn an_ended_agents_unread_counts_its_reports_never_its_exit_notice() {
    let w = World::human("");
    w.clock(NOW);
    let caller = w.caller();
    member(&w, ID, json!({"pane": null}));
    w.panes(&[]);
    let unread = || {
        snapshot(&w, &caller, &[(ID, "stopped")]);
        let watch = board(&w.ctx, &caller, true, true).unwrap();
        w.fakes.hc_done().unwrap();
        watch["agents"][0]["unread"].clone()
    };
    assert_eq!(unread(), json!(0), "the exit notice alone");
    let mailbox = std::fs::read_to_string(w.state().join("mail").join("pane-1.jsonl")).unwrap();
    assert!(
        mailbox.contains(r#""kind":"stopped""#),
        "the notice was posted: {mailbox}"
    );
    crate::agents::mail::add_member_mail(
        &w.ctx,
        &caller,
        "pane-1",
        "worker",
        ID,
        "%9",
        "done: 3 fixed",
    )
    .unwrap();
    assert_eq!(unread(), json!(1), "one unread report");
    crate::agents::mail::read_mail(&w.ctx, &caller, "pane-1").unwrap();
    assert_eq!(unread(), json!(0), "the report read");
}

// `m` shows exactly what the badge counts, by the same rule for every row: an ended viewless member's one unread report
// (matched by the id it names, the record keeping no pane) is badge 1 and popup 1; a team member whose only line is the
// notice of its own exit is badge 0 and popup 0.
#[test]
fn the_mail_popup_shows_exactly_what_the_badge_counts() {
    use crate::agents::mail::{add_member_mail, badge_mail};
    let w = World::human("");
    w.clock(NOW);
    let caller = w.caller();
    member(&w, ID, json!({"pane": null}));
    w.panes(&[]);
    let both = |facts: &[(&str, &str)], id: &str| {
        snapshot(&w, &caller, facts);
        let watch = board(&w.ctx, &caller, true, true).unwrap();
        w.fakes.hc_done().unwrap();
        let row = watch["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .cloned()
            .unwrap();
        snapshot(&w, &caller, facts);
        let popup = badge_mail(&w.ctx, &caller, id).unwrap().len();
        (row["unread"].clone(), popup)
    };
    assert_eq!(
        both(&[(ID, "stopped")], ID),
        (json!(0), 0),
        "the exit notice alone"
    );
    add_member_mail(
        &w.ctx,
        &caller,
        "pane-1",
        "worker",
        ID,
        "%9",
        "done: 3 fixed",
    )
    .unwrap();
    assert_eq!(
        both(&[(ID, "stopped")], ID),
        (json!(1), 1),
        "one unread report"
    );

    let w = World::human("");
    w.clock(NOW);
    let caller = w.caller();
    member(
        &w,
        ID,
        json!({"name": "lead", "pane": "%8", "parentPane": null, "parentAgent": null, "depth": 0, "upTeam": "T",
            "createdAt": "2026-10-29T11:00:00.0000000Z"}),
    );
    let kid = "cccccccccccc";
    member(
        &w,
        kid,
        json!({"name": "kid", "pane": "%9", "parentPane": "%8", "parentAgent": {"id": ID}, "depth": 1,
            "createdAt": "2026-10-29T11:30:00.0000000Z"}),
    );
    w.panes(&["%8|@5|300|aaaaaaaaaaaa|lead"]);
    let facts = [(ID, "running"), (kid, "stopped")];
    let (unread, popup) = {
        snapshot(&w, &caller, &facts);
        let watch = board(&w.ctx, &caller, true, true).unwrap();
        w.fakes.hc_done().unwrap();
        assert_eq!(watch.get("team"), Some(&json!(true)));
        let row = watch["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == kid)
            .cloned()
            .unwrap();
        snapshot(&w, &caller, &facts);
        (
            row["unread"].clone(),
            badge_mail(&w.ctx, &caller, kid).unwrap().len(),
        )
    };
    let notices = std::fs::read_to_string(w.state().join("mail").join("pane-8.jsonl")).unwrap();
    assert!(
        notices.contains(r#""kind":"stopped""#),
        "the exit notice was posted: {notices}"
    );
    assert_eq!(
        (unread, popup),
        (json!(0), 0),
        "a team member's own exit notice"
    );
}

// The bar's cap on older ends never drops a member with an unread report: of 11 members that ended more than an hour
// ago, the oldest (past the 10 kept) left one report unread; it is listed, and stands in Inbox.
#[test]
fn an_older_ended_member_with_an_unread_report_is_never_capped_away() {
    let w = World::human("");
    w.clock(NOW);
    let caller = w.caller();
    let mut facts = Vec::new();
    let ids: Vec<String> = (0..11u32).map(|n| format!("{:012x}", n + 1)).collect();
    for (n, id) in ids.iter().enumerate() {
        let folder = member(&w, id, json!({"name": format!("e{n}"), "pane": null}));
        // e0 ended first, e10 last, all more than an hour ago; the notice is already posted, so no new one lands.
        write(
            &folder.join("notice.json"),
            json!({"kind": "stopped", "exitCode": null, "at": format!("2026-10-29T10:{:02}:00.0000000Z", 10 + n)}),
        );
        crate::state::evidence::publish(
            &crate::agents::gens::path(&folder, 0, "notice"),
            &json!({"kind": "stopped", "exitCode": null, "at": format!("2026-10-29T10:{:02}:00.0000000Z", 10 + n)}),
        )
        .unwrap();
        facts.push((id.as_str(), "stopped"));
    }
    w.panes(&[]);
    crate::agents::mail::add_member_mail(
        &w.ctx,
        &caller,
        "pane-1",
        "e0",
        &ids[0],
        "",
        "the report",
    )
    .unwrap();
    snapshot(&w, &caller, &facts);
    let watch = board(&w.ctx, &caller, true, true).unwrap();
    w.fakes.hc_done().unwrap();
    let rows = watch["agents"].as_array().unwrap();
    let e0 = rows.iter().find(|r| r["name"] == "e0");
    assert_eq!(e0.map(|r| r["unread"].clone()), Some(json!(1)), "{rows:?}");
    let text = crate::sidebar::flat_text(&watch, 40, 30);
    let inbox = text
        .iter()
        .position(|r| r.starts_with(" Inbox"))
        .expect("an Inbox");
    assert!(text[inbox + 1].contains("e0"), "{text:?}");
}

// One unread rule for every reader: A's report, sent from %9, still counts after A's view reopens on %19, in `list`,
// `list -All`, the bar's ✉ badge and `m` alike, and it is what `read` of that mailbox delivers.
#[test]
fn list_badge_and_popup_count_a_report_from_a_reopened_view() {
    use crate::agents::mail::{add_member_mail, badge_mail, serve};
    let w = World::human("");
    w.clock(NOW);
    let caller = w.caller();
    let folder = member(&w, ID, json!({}));
    add_member_mail(&w.ctx, &caller, "pane-1", "worker", ID, "%9", "the report").unwrap();
    // A's view reopened on %19.
    let mut r = read_json(&folder.join("record.json")).unwrap().unwrap();
    r["pane"] = json!("%19");
    r["startCommand"] = crate::agents::fixture::launch_command("%19").into();
    write(&folder.join("record.json"), r);
    w.panes(&["%19|@5|300|aaaaaaaaaaaa|worker"]);
    let unread = |all: bool, watch: bool| {
        snapshot(&w, &caller, &[(ID, "running")]);
        let b = board(&w.ctx, &caller, all, watch).unwrap();
        w.fakes.hc_done().unwrap();
        b["agents"][0]["unread"].clone()
    };
    assert_eq!(
        [
            unread(false, false),
            unread(true, false),
            unread(true, true)
        ],
        [json!(1), json!(1), json!(1)],
        "list, list -All, the badge"
    );
    snapshot(&w, &caller, &[(ID, "running")]);
    assert_eq!(
        badge_mail(&w.ctx, &caller, ID).unwrap().len(),
        1,
        "the popup"
    );
    let owner = crate::agents::receipt::Owner {
        id: "root".into(),
        generation: "0".into(),
    };
    let served = serve(&w.ctx, &caller, "pane-1", &owner, &|| true).unwrap();
    assert!(
        served.is_some_and(|r| r.text.contains("the report")),
        "read delivers it"
    );
}

// Outside a team a lone member that left a report unread is counted whatever its life: a Lost or Unknown row's badge
// and its popup agree.
#[test]
fn a_lost_or_unknown_members_badge_and_popup_agree() {
    use crate::agents::mail::{add_member_mail, badge_mail};
    for life in ["lost", "unknown"] {
        let w = World::human("");
        w.clock(NOW);
        let caller = w.caller();
        member(&w, ID, json!({}));
        w.panes(&[]);
        add_member_mail(&w.ctx, &caller, "pane-1", "worker", ID, "%9", "the report").unwrap();
        snapshot(&w, &caller, &[(ID, life)]);
        let watch = board(&w.ctx, &caller, true, true).unwrap();
        w.fakes.hc_done().unwrap();
        let row = watch["agents"][0].clone();
        snapshot(&w, &caller, &[(ID, life)]);
        let popup = badge_mail(&w.ctx, &caller, ID).unwrap().len();
        assert_eq!(
            (row["state"].clone(), row["unread"].clone(), popup),
            (json!(life), json!(1), 1),
            "{life}"
        );
    }
}

#[test]
fn board_reads_inside_their_locks() {
    let w = World::human("");
    let caller = w.caller();
    let live = member(&w, ID, json!({}));
    w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
    let mailbox = w.state().join("mail").join("pane-1.jsonl");
    let (agent, mail) = (
        lock_key("agent", &caller, ID),
        lock_key("mail", &caller, "pane-1"),
    );
    let sent = live.join("last-sent.json");
    // last-sent.json exists only while the agent lock is held; a message lands as the mail lock is taken.
    w.fakes.lock_probe(move |event| {
        if event == format!("+{agent}") {
            write(&sent, json!({"line": "fresh", "at": "x"}));
        } else if event == format!("-{agent}") {
            std::fs::remove_file(&sent).unwrap();
        } else if event == format!("+{mail}") {
            let line = r#"{"id":"m1","from":"worker","pane":"%9","at":"x","body":"hi"}"#;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&mailbox)
                .unwrap();
            std::io::Write::write_all(&mut file, format!("{line}\n").as_bytes()).unwrap();
        }
        String::new()
    });
    snapshot(&w, &caller, &[(ID, "running")]);
    let reply = board(&w.ctx, &caller, false, false).unwrap();
    w.fakes.hc_done().unwrap();
    let row = &reply["agents"][0];
    assert_eq!(
        (&row["doing"], &row["doingFrom"], &row["unread"]),
        (&json!("fresh"), &json!("message"), &json!(1))
    );
}

#[test]
fn board_does_not_refresh_after_reap() {
    use std::cell::Cell;
    use std::rc::Rc;
    let w = World::human("");
    let caller = w.caller();
    let folder = member(&w, ID, json!({}));
    write(
        &folder.join("launcher.json"),
        json!({"pid": 300, "exitCode": 0}),
    );
    // %9 is listed dead until the conditional kill ran; then it is gone.
    let killed = Rc::new(Cell::new(false));
    let gone = killed.clone();
    w.fakes.mux_script("list-panes", move |argv| {
        let rows: &[&str] = if gone.get() {
            &[]
        } else {
            &["%9|@5|300|aaaaaaaaaaaa|worker"]
        };
        Ok(crate::mux::Output {
            code: 0,
            stdout: crate::agents::fixture::listed_dead(argv, rows, "\n", &["%9"]),
            stderr: String::new(),
        })
    });
    let kill = killed.clone();
    w.fakes.mux_script("if-shell", move |_| {
        kill.set(true);
        Ok(crate::mux::Output {
            code: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    });
    w.fakes.mux_rule("#{pane_id}|#{@agent-id}", 0, "|\n", "");
    w.fakes.clear_log();
    snapshot(&w, &caller, &[(ID, "0")]);
    let reply = board(&w.ctx, &caller, true, false).unwrap();
    w.fakes.hc_done().unwrap();
    assert!(!killed.get());
    assert!(folder.join("notice.json").exists());
    assert_eq!(w.fakes.hc_calls().len(), 3);
    assert_eq!(
        (names(&reply), &reply["running"]),
        (vec!["worker=exited 0".to_string()], &json!(0))
    );
    let lists = w
        .fakes
        .mux_calls()
        .iter()
        .filter(|c| crate::agents::fixture::list_call(c))
        .count();
    assert_eq!(lists, 1);
}

#[test]
fn lock_order_notice_mail_agent() {
    let w = World::human("");
    let caller = w.caller();
    let live = member(&w, ID, json!({}));
    member(&w, "bbbbbbbbbbbb", json!({"name": "gone", "pane": "%8"}));
    w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
    w.fakes.mux_first("#{pane_id}|#{@agent-id}", 0, "|\n", "");
    // status.json exists only while the row's agent lock is held, so a row that shows it read it there.
    let agent = lock_key("agent", &caller, ID);
    let status = live.join("status.json");
    let (held, file) = (agent.clone(), status.clone());
    w.fakes.lock_probe(move |event| {
        if event == format!("+{held}") {
            write(&file, json!({"text": "held", "at": "x"}));
        } else if event == format!("-{held}") {
            std::fs::remove_file(&file).unwrap();
        }
        String::new()
    });
    w.fakes.clear_log();
    snapshot(&w, &caller, &[(ID, "running"), ("bbbbbbbbbbbb", "0")]);
    let reply = board(&w.ctx, &caller, false, false).unwrap();
    w.fakes.hc_done().unwrap();
    let member_lock = lock_key("member", &caller, "bbbbbbbbbbbb");
    let notice = lock_key("notice", &caller, "bbbbbbbbbbbb");
    let mail = lock_key("mail", &caller, "pane-1");
    assert_eq!(
        w.fakes.locks(),
        [
            format!("+{member_lock}"),
            format!("+{notice}"),
            format!("+{mail}"),
            format!("-{mail}"),
            format!("-{notice}"),
            format!("-{member_lock}"),
            format!("+{mail}"),
            format!("-{mail}"),
            format!("+{agent}"),
            format!("-{agent}")
        ]
    );
    let row = &reply["agents"][0];
    assert_eq!(
        (&row["doing"], &row["doingFrom"]),
        (&json!("held"), &json!("status"))
    );
    assert!(!status.exists());
    // An unreadable status.json is skipped: the row falls to the last sent line.
    std::fs::write(&status, "{bad").unwrap();
    write(
        &live.join("last-sent.json"),
        json!({"line": "sent", "at": "x"}),
    );
    w.fakes.lock_probe(|_| String::new());
    snapshot(&w, &caller, &[(ID, "running"), ("bbbbbbbbbbbb", "0")]);
    let reply = board(&w.ctx, &caller, false, false).unwrap();
    w.fakes.hc_done().unwrap();
    let row = &reply["agents"][0];
    assert_eq!(
        (&row["doing"], &row["doingFrom"]),
        (&json!("sent"), &json!("message"))
    );
}

#[test]
fn activity_is_reported_seen_or_listening_and_ranks_the_rows() {
    let w = World::human("");
    w.clock(NOW);
    let caller = w.caller();
    // Five live agents in pane order a..e.
    let ids = [
        "aaaaaaaaaaaa",
        "bbbbbbbbbbbb",
        "cccccccccccc",
        "dddddddddddd",
        "eeeeeeeeeeee",
    ];
    let names = ["a", "b", "c", "d", "e"];
    let mut folders = Vec::new();
    let mut panes = Vec::new();
    for (i, (id, name)) in ids.iter().zip(names).enumerate() {
        folders.push(member(
            &w,
            id,
            json!({"name": name, "pane": format!("%{}", 10 + i)}),
        ));
        panes.push(format!("%{}|@5|{}|{id}|{name}", 10 + i, 300 + i));
    }
    let panes: Vec<&str> = panes.iter().map(String::as_str).collect();
    w.panes(&panes);
    let report = |i: usize, state: &str, since: &str| {
        write(
            &folders[i].join("activity.json"),
            json!({"state": state, "kind": null, "since": format!("2026-10-29T{since}.0000000Z")}),
        )
    };
    let seen = |i: usize, since: &str| {
        write(
            &folders[i].join("seen.json"),
            json!({"since": format!("2026-10-29T{since}.0000000Z")}),
        )
    };
    // a: idle since 11:50, that spell seen: idle. b: idle since 11:58, only an older spell seen: done.
    // c: working since 11:59. d: no report: working (list) or listening (watch). e: blocked since 11:40.
    report(0, "idle", "11:50:00");
    seen(0, "11:50:00");
    report(1, "idle", "11:58:00");
    seen(1, "11:57:00");
    report(2, "working", "11:59:00");
    report(4, "blocked", "11:40:00");
    w.fakes.process(crate::testing::row(700, 303, 7000));
    write(
        &folders[3].join("waiting-700.json"),
        json!({"pid": 700, "start": 7000, "since": "2026-10-29T11:59:30.0000000Z"}),
    );
    let rows = |watch: bool| -> Vec<String> {
        snapshot(
            &w,
            &caller,
            &ids.iter().map(|id| (*id, "running")).collect::<Vec<_>>(),
        );
        let reply = board(&w.ctx, &caller, false, watch).unwrap();
        w.fakes.hc_done().unwrap();
        reply["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                format!(
                    "{}={}/{}",
                    row["name"].as_str().unwrap(),
                    row["activity"],
                    row["since"]
                )
            })
            .collect()
    };
    // Needs you first, then done, working (newest change first, then pane order), idle.
    // list and the bar's feed agree: d listens in read -Wait, so it is idle in both.
    assert_eq!(rows(false), rows(true));
    assert_eq!(
        rows(true),
        [
            r#"e="blocked"/1200"#,
            r#"b="done"/120"#,
            r#"c="working"/60"#,
            r#"a="idle"/600"#,
            r#"d="idle"/null"#
        ]
    );
    // Its own spell seen: idle; no seen file at all: done; an unreadable report counts as none.
    seen(1, "11:58:00");
    report(0, "idle", "11:50:00");
    std::fs::remove_file(folders[0].join("seen.json")).unwrap();
    std::fs::write(folders[2].join("activity.json"), "{").unwrap();
    assert_eq!(
        rows(false),
        [
            r#"e="blocked"/1200"#,
            r#"a="done"/600"#,
            r#"c="working"/null"#,
            r#"b="idle"/120"#,
            r#"d="idle"/null"#
        ]
    );
    // An unknown word counts as no report.
    report(4, "dreaming", "11:40:00");
    assert_eq!(rows(false)[0], r#"a="done"/600"#);
    // changedAt names the spell; null without a report.
    snapshot(
        &w,
        &caller,
        &ids.iter().map(|id| (*id, "running")).collect::<Vec<_>>(),
    );
    let reply = board(&w.ctx, &caller, false, false).unwrap();
    w.fakes.hc_done().unwrap();
    let changed: Vec<String> = reply["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| format!("{}={}", row["name"].as_str().unwrap(), row["changedAt"]))
        .collect();
    assert_eq!(
        changed,
        [
            r#"a="2026-10-29T11:50:00.0000000Z""#,
            "c=null",
            "e=null",
            r#"b="2026-10-29T11:58:00.0000000Z""#,
            "d=null"
        ]
    );
    // Two changes within one second rank by their own times, not by whole seconds: the newer first, though
    // the older comes first in pane order.
    for (i, at) in [(2, "11:59:59.8000000Z"), (0, "11:59:59.1000000Z")] {
        write(
            &folders[i].join("activity.json"),
            json!({"state": "blocked", "kind": null, "since": format!("2026-10-29T{at}")}),
        );
    }
    w.clock("2026-10-29T12:00:00.9000000Z");
    assert_eq!(&rows(false)[..2], [r#"c="blocked"/1"#, r#"a="blocked"/1"#]);
}

#[test]
fn mark_seen_names_the_spell_under_the_agent_lock() {
    let w = World::human("");
    w.clock(NOW);
    let caller = w.caller();
    let folder = w.agent_record(ID, json!({}));
    w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
    let file = folder.join("seen.json");
    let seen = file.clone();
    w.fakes
        .lock_probe(move |_| format!("exists={}", seen.exists()));
    w.fakes.clear_log();
    assert!(mark_seen(&w.ctx, &caller, "%9", "2026-10-29T11:58:00.0000000Z").unwrap());
    let agent = lock_key("agent", &caller, ID);
    assert_eq!(
        w.fakes.locks(),
        [
            format!("+{agent} exists=false"),
            format!("-{agent} exists=true")
        ]
    );
    let body = || std::fs::read_to_string(&file).unwrap();
    assert_eq!(body(), r#"{"since":"2026-10-29T11:58:00.0000000Z"}"#);
    // Never back to an older spell; a newer one replaces it.
    mark_seen(&w.ctx, &caller, "%9", "2026-10-29T11:50:00.0000000Z").unwrap();
    assert_eq!(body(), r#"{"since":"2026-10-29T11:58:00.0000000Z"}"#);
    mark_seen(&w.ctx, &caller, "%9", "2026-10-29T11:59:00.0000000Z").unwrap();
    assert_eq!(body(), r#"{"since":"2026-10-29T11:59:00.0000000Z"}"#);
    // A wrong pane is no view; malformed time does not even take a pane snapshot or agent lock.
    assert!(!mark_seen(&w.ctx, &caller, "%4", "2026-10-29T12:00:00.0000000Z").unwrap());
    w.fakes.clear_log();
    assert!(!mark_seen(&w.ctx, &caller, "%9", "soon").unwrap());
    assert!(w.fakes.mux_calls().is_empty());
    assert!(w.fakes.locks().is_empty());
    assert_eq!(body(), r#"{"since":"2026-10-29T11:59:00.0000000Z"}"#);
    // A notice is history, not evidence that the committed attachment disappeared.
    write(&folder.join("notice.json"), json!({"kind":"stopped"}));
    w.fakes.clear_log();
    assert!(mark_seen(&w.ctx, &caller, "%9", "2026-10-29T12:00:00.0000000Z").unwrap());
    assert_eq!(w.fakes.mux_calls().len(), 1);
    assert!(crate::agents::fixture::list_call(&w.fakes.mux_calls()[0]));
    assert_eq!(body(), r#"{"since":"2026-10-29T12:00:00.0000000Z"}"#);
    // Replaced launches, duplicate pane rows and duplicate own records never prove a unique view.
    let mut replaced = crate::agents::fixture::row_of("%9|@5|300|aaaaaaaaaaaa|worker", &[]);
    replaced.command = "/bin/sh -l".into();
    w.pane_rows(&[replaced]);
    assert!(!mark_seen(&w.ctx, &caller, "%9", "2026-10-29T12:01:00.0000000Z").unwrap());
    w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker", "%9|@5|300||"]);
    assert!(!mark_seen(&w.ctx, &caller, "%9", "2026-10-29T12:01:00.0000000Z").unwrap());
    w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
    w.agent_record("bbbbbbbbbbbb", json!({"name":"duplicate", "pane":"%9"}));
    assert!(!mark_seen(&w.ctx, &caller, "%9", "2026-10-29T12:01:00.0000000Z").unwrap());
    assert_eq!(body(), r#"{"since":"2026-10-29T12:00:00.0000000Z"}"#);
    assert!(w.fakes.hc_calls().is_empty());
    w.fakes.hc_done().unwrap();
}

#[test]
fn watch_shows_waiting_only_for_a_live_newer_read() {
    let w = World::human("");
    w.clock(NOW);
    let caller = w.caller();
    let folder = member(&w, ID, json!({}));
    w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
    // The agent's `read -Wait`: pid 700, started at 7000.
    w.fakes.process(crate::testing::row(700, 300, 7000));
    let wait = |name: u32, pid: u32, start: i64, since: &str| {
        write(
            &folder.join(format!("waiting-{name}.json")),
            json!({"pid": pid, "start": start, "since": format!("2026-10-29T{since}.0000000Z")}),
        )
    };
    let status = |at: &str| {
        write(
            &folder.join("status.json"),
            json!({"text": "x", "at": format!("2026-10-29T{at}.0000000Z")}),
        )
    };
    let waiting = || {
        snapshot(&w, &caller, &[(ID, "running")]);
        let reply = board(&w.ctx, &caller, true, true).unwrap();
        w.fakes.hc_done().unwrap();
        reply["agents"][0]["waiting"].clone()
    };
    let files = || {
        let mut names: Vec<String> = std::fs::read_dir(&folder)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("waiting-"))
            .collect();
        names.sort();
        names
    };
    assert_eq!(waiting(), json!(false));
    // A live read, with no status or an older one.
    wait(700, 700, 7000, "11:59:00");
    assert_eq!(waiting(), json!(true));
    status("11:58:00");
    assert_eq!(waiting(), json!(true));
    // A status since the read began: working, and the live read's file stays.
    status("11:59:30");
    assert_eq!(waiting(), json!(false));
    assert_eq!(files(), ["waiting-700.json"]);
    std::fs::remove_file(folder.join("status.json")).unwrap();
    // The pid reused by another process (another start): not waiting, deleted.
    wait(700, 700, 6999, "11:59:00");
    assert_eq!(waiting(), json!(false));
    assert_eq!(files(), Vec::<String>::new());
    // The read was killed: its pid is gone, the file left behind is deleted.
    wait(701, 701, 7010, "11:59:00");
    assert_eq!(waiting(), json!(false));
    assert_eq!(files(), Vec::<String>::new());
    // An unreadable file, or one naming another pid than its own name, is deleted.
    std::fs::write(folder.join("waiting-702.json"), "{bad").unwrap();
    wait(703, 700, 7000, "11:59:00");
    assert_eq!(waiting(), json!(false));
    assert_eq!(files(), Vec::<String>::new());
    // Detached Running members still read their activity/wait markers.
    w.panes(&[]);
    wait(700, 700, 7000, "11:59:00");
    assert_eq!(waiting(), json!(true));
    // Only the bar's feed says waiting; list's rows are unchanged.
    wait(700, 700, 7000, "11:59:00");
    snapshot(&w, &caller, &[(ID, "running")]);
    let list = board(&w.ctx, &caller, true, false).unwrap();
    w.fakes.hc_done().unwrap();
    assert!(list["agents"][0].get("waiting").is_none());
    assert_eq!(files(), ["waiting-700.json"]);
}

// A resumed parent keeps its inherited mailbox, so its child's unread is counted there, not under its own id.
#[test]
fn watch_unread_counts_in_the_mailbox_a_parent_inherited() {
    let w = World::human("");
    w.clock(NOW);
    let caller = w.caller();
    member(&w, ID, json!({"mailbox": "cccccccccccc"}));
    member(
        &w,
        "bbbbbbbbbbbb",
        json!({"name": "child", "pane": "%8", "parentPane": "%9"}),
    );
    w.panes(&[
        "%9|@5|300|aaaaaaaaaaaa|worker",
        "%8|@5|301|bbbbbbbbbbbb|child",
    ]);
    add_mail(&w.ctx, &caller, "cccccccccccc", "child", "%8", "y", None).unwrap();
    for _ in 0..2 {
        add_mail(&w.ctx, &caller, ID, "child", "%8", "stale", None).unwrap();
    }
    snapshot(&w, &caller, &[(ID, "running"), ("bbbbbbbbbbbb", "running")]);
    let reply = board(&w.ctx, &caller, true, true).unwrap();
    w.fakes.hc_done().unwrap();
    let unread: Vec<String> = reply["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| format!("{}={}", r["name"].as_str().unwrap(), r["unread"]))
        .collect();
    assert_eq!(unread, ["worker=0", "child=1"]);
}

// A member a committed resume superseded has no row; `list -All` offers `resume <id>` on every other ended row
// (stopped or exited), never on a running one and never in the bar's feed.
#[test]
fn list_all_offers_resume_for_ended_members_no_resume_superseded() {
    let w = World::human("");
    w.clock(NOW);
    let caller = w.caller();
    member(&w, ID, json!({}));
    member(
        &w,
        "bbbbbbbbbbbb",
        json!({"pane": "%8", "resumedFrom": ID, "mailbox": ID}),
    );
    member(&w, "cccccccccccc", json!({"name": "other", "pane": "%7"}));
    w.panes(&[]);
    let rows = |facts: &[(&str, &str)], watch: bool| {
        snapshot(&w, &caller, facts);
        let reply = board(&w.ctx, &caller, true, watch).unwrap();
        w.fakes.hc_done().unwrap();
        reply["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                let resume = r.get("resume").map_or("-", |v| v.as_str().unwrap());
                format!("{}={resume}", r["name"].as_str().unwrap())
            })
            .collect::<Vec<_>>()
    };
    let y_running = [
        (ID, "stopped"),
        ("bbbbbbbbbbbb", "running"),
        ("cccccccccccc", "3"),
    ];
    assert_eq!(
        rows(&y_running, false),
        ["worker=-", "other=mux-agent resume cccccccccccc"]
    );
    let all_ended = [
        (ID, "stopped"),
        ("bbbbbbbbbbbb", "stopped"),
        ("cccccccccccc", "3"),
    ];
    let mut ended = rows(&all_ended, false);
    ended.sort();
    assert_eq!(
        ended,
        [
            "other=mux-agent resume cccccccccccc",
            "worker=mux-agent resume bbbbbbbbbbbb"
        ]
    );
    let mut feed = rows(&all_ended, true);
    feed.sort();
    assert_eq!(feed, ["other=-", "worker=-"]);
}

#[test]
fn watch_unread_is_the_agents_mail_in_its_parents_mailbox() {
    let w = World::human("");
    w.clock(NOW);
    let caller = w.caller();
    // worker (%9) reports to the human pane %1; child (%8) reports to worker, an agent, whose mailbox is its id.
    member(&w, ID, json!({}));
    member(
        &w,
        "bbbbbbbbbbbb",
        json!({"name": "child", "pane": "%8", "parentPane": "%9"}),
    );
    w.panes(&[
        "%9|@5|300|aaaaaaaaaaaa|worker",
        "%8|@5|301|bbbbbbbbbbbb|child",
    ]);
    for _ in 0..2 {
        add_mail(&w.ctx, &caller, "pane-1", "worker", "%9", "x", None).unwrap();
    }
    add_mail(&w.ctx, &caller, ID, "child", "%8", "y", None).unwrap();
    // To a sibling's mailbox: not the parent's, so no badge.
    add_mail(&w.ctx, &caller, "pane-2", "worker", "%9", "z", None).unwrap();
    // The bar's own pane (pane-7) has no mail; its reader is not used.
    let unread = || {
        snapshot(&w, &caller, &[(ID, "running"), ("bbbbbbbbbbbb", "running")]);
        let reply = board(&w.ctx, &caller, true, true).unwrap();
        w.fakes.hc_done().unwrap();
        reply["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| format!("{}={}", r["name"].as_str().unwrap(), r["unread"]))
            .collect::<Vec<_>>()
    };
    let mail = w.state().join("mail");
    let offsets = || {
        std::fs::read_dir(&mail)
            .unwrap()
            .filter(|e| {
                let path = e.as_ref().unwrap().path();
                path.extension().is_some_and(|x| x == "offset")
            })
            .count()
    };
    assert_eq!(unread(), ["worker=2", "child=1"]);
    // Drawing moves no cursor.
    assert_eq!(offsets(), 0);
    assert_eq!(read_mail(&w.ctx, &caller, "pane-1").unwrap().len(), 2);
    assert_eq!(unread(), ["worker=0", "child=1"]);
    assert_eq!(read_mail(&w.ctx, &caller, ID).unwrap().len(), 1);
    assert_eq!(unread(), ["worker=0", "child=0"]);
    // list keeps the calling reader's unread: the caller's own pane-1, never the bar's parents.
    add_mail(&w.ctx, &caller, "pane-1", "worker", "%9", "w", None).unwrap();
    snapshot(&w, &caller, &[(ID, "running"), ("bbbbbbbbbbbb", "running")]);
    let list = board(&w.ctx, &caller, false, false).unwrap();
    w.fakes.hc_done().unwrap();
    assert_eq!(list["agents"][0]["unread"], 1);
}

#[test]
fn a_dead_launch_a_failed_kill_left_is_not_running() {
    // A failed view cleanup does not end hc membership: the member runs detached until terminal facts.
    let w = World::human("");
    let caller = w.caller();
    member(&w, ID, json!({}));
    w.panes_dead(&["%9|@5|300|aaaaaaaaaaaa|worker"], &["%9"]);
    w.fakes.mux_rule("if-shell", 1, "", "no server");
    w.fakes
        .mux_rule("-t %9 #{pane_id}|#{@agent-id}", 0, "%9|aaaaaaaaaaaa\n", "");
    snapshot(&w, &caller, &[(ID, "running")]);
    let reply = board(&w.ctx, &caller, true, false).unwrap();
    w.fakes.hc_done().unwrap();
    assert_eq!(reply["running"], 1);
    assert_eq!(names(&reply), ["worker=running"]);
    assert_eq!(reply["agents"][0]["view"], "detached");
    assert_eq!(reply["agents"][0]["pane"], Value::Null);
    let killed = w
        .fakes
        .mux_calls()
        .iter()
        .filter(|c| c[1] == "if-shell")
        .count();
    assert_eq!(killed, 0);
    let found = registry::find_pane(&w.ctx, &caller, "worker").unwrap_err();
    assert_eq!((found.code, found.check), (2, "target"));
    assert!(!w.folder(ID).join("notice.json").exists());
    snapshot(&w, &caller, &[(ID, "0")]);
    let reply = board(&w.ctx, &caller, true, false).unwrap();
    w.fakes.hc_done().unwrap();
    assert_eq!(reply["running"], 0);
    assert_eq!(names(&reply), ["worker=exited 0"]);
    assert!(w.folder(ID).join("notice.json").exists());
    assert!(!w.fakes.mux_calls().iter().any(|c| c[1] == "if-shell"));
}
#[test]
fn board_hc_route_orders_counts_and_never_refreshes() {
    use crate::agents::core_fixture as cf;
    for watch in [false, true] {
        let w = World::human("");
        let c = w.caller();
        w.clock(NOW);
        let mut records = Vec::new();
        for (id, name) in [
            ("aaaaaaaaaaaa", "detached"),
            ("bbbbbbbbbbbb", "attached"),
            ("cccccccccccc", "lost"),
            ("dddddddddddd", "unknown"),
            ("eeeeeeeeeeee", "ended"),
        ] {
            records.push(cf::member(
                &w,
                id,
                json!({"name":name,"pane":if name=="attached" {"%8"} else {"%9"}}),
            ));
        }
        let mut row = crate::agents::fixture::row_of("%8|@7|300|aaaaaaaaaaaa|wrong", &[]);
        row.command = "hc attach tbbbbbbbbbbbb".into();
        w.pane_rows(&[row]);
        write(
            &w.folder("aaaaaaaaaaaa").join("notice.json"),
            json!({"kind":"stopped","exitCode":99}),
        );
        write(
            &w.folder("cccccccccccc").join("notice.json"),
            json!({"kind":"exited","exitCode":99,"closedBy":"pane"}),
        );
        add_mail(&w.ctx, &c, "pane-1", "detached", "%9", "keep", None).unwrap();
        for all in [false, true] {
            cf::online(&w);
            cf::listing(&w, &super::super::membership::team(&c), &[]);
            for (i, r) in records.iter().enumerate() {
                let mut t = cf::wire(r);
                if i == 2 {
                    t["argv"] = json!(["/bin/sh", "-l"]);
                }
                if i == 3 {
                    t["labels"]["mux.gen"] = json!("1");
                }
                if i == 4 {
                    t["state"] = json!("finished");
                    t["reason"] = json!("exited");
                    t["exit_code"] = json!(7);
                }
                cf::get(&w, r["hcAddress"].as_str().unwrap(), t);
            }
            w.fakes.clear_log();
            let out = board(&w.ctx, &c, all, watch).unwrap();
            w.fakes.hc_done().unwrap();
            assert_eq!(
                read_json(
                    &super::super::server_dir(&w.ctx, &c).join("agents/eeeeeeeeeeee/notice.json")
                )
                .unwrap()
                .unwrap()["exitCode"],
                7
            );
            assert_eq!(out["running"], 2);
            assert_eq!(out.get("endedTotal"), all.then_some(&json!(1)));
            let rows = out["agents"].as_array().unwrap();
            assert_eq!(
                rows.iter()
                    .map(|r| r["name"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                if all {
                    vec!["attached", "detached", "lost", "unknown", "ended"]
                } else {
                    vec!["attached", "detached", "lost", "unknown"]
                }
            );
            assert_eq!(
                (
                    &rows[1]["pane"],
                    &rows[1]["window"],
                    &rows[1]["life"],
                    &rows[1]["view"],
                    &rows[1]["unread"]
                ),
                (
                    &Value::Null,
                    &Value::Null,
                    &json!("running"),
                    &json!("detached"),
                    &json!(1)
                )
            );
            assert_eq!(rows[2]["state"], "lost");
            assert_eq!(rows[2]["exitCode"], Value::Null);
            assert_eq!(rows[2]["closedBy"], "pane");
            for row in rows {
                let keys = row
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                assert_eq!(&keys[keys.len() - 3..], ["life", "view", "hcAddress"]);
            }
            if all {
                assert_eq!(rows[4]["state"], "exited 7");
                assert_eq!(rows[4]["exitCode"], 7);
            }
            assert_eq!(w.fakes.hc_calls().len(), 7);
            assert_eq!(w.fakes.mux_calls().len(), 1);
            assert!(crate::agents::fixture::list_call(&w.fakes.mux_calls()[0]));
        }
    }
}
// Call 18a: a held member's row names its pending generation's session, never the active record's.
#[test]
fn a_held_members_row_names_the_pending_session() {
    use crate::agents::{core_fixture as cf, gens};
    use crate::state::evidence::publish;
    const PENDING: &str = "11111111-2222-4333-8444-555555555555";
    let w = World::human("");
    let c = w.caller();
    let mut r = cf::member(
        &w,
        ID,
        json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd"}),
    );
    r["pending"] = json!({"gen": 2, "hcName": "m4567cdef", "sessionId": PENDING,
            "hcAddress": "new-address"});
    let f = w.folder(ID);
    write(&f.join("record.json"), r);
    publish(&gens::path(&f, 2, "commit"), &json!({"committed": true})).unwrap();
    w.panes(&[]);
    let b = board(&w.ctx, &c, true, false).unwrap();
    assert_eq!(b["agents"][0]["sessionId"], PENDING, "{b:?}");
}
#[test]
fn board_hc_failures_do_not_invent_completion() {
    use crate::agents::core_fixture as cf;
    for mode in ["view-error", "core-error", "record-error", "notice-lock"] {
        let w = World::human("");
        let c = w.caller();
        let r = cf::member(&w, ID, json!({}));
        w.panes(&[]);
        if mode == "record-error" {
            std::fs::write(w.folder(ID).join("record.json"), "{").unwrap();
        } else if mode != "core-error" {
            cf::online(&w);
            cf::listing(&w, &super::super::membership::team(&c), &[]);
            let mut t = cf::wire(&r);
            if mode == "notice-lock" {
                t["state"] = json!("finished");
                t["reason"] = json!("exited");
                w.fakes.lock_timeout("notice|");
            }
            cf::get(&w, "taaaaaaaaaaaa", t);
        }
        if mode == "view-error" {
            w.fakes.mux_first("list-panes", 1, "", "down");
        }
        let got = board(&w.ctx, &c, true, false);
        w.fakes.hc_done().unwrap();
        match mode {
            "view-error" => {
                let b = got.unwrap();
                assert_eq!(b["running"], 1);
                assert_eq!(b["agents"][0]["view"], "detached");
            }
            "core-error" => {
                let b = got.unwrap();
                assert_eq!(b["running"], 0);
                assert_eq!(b["agents"][0]["life"], "unknown");
            }
            "record-error" => assert_eq!(got.unwrap_err().check, "evidence"),
            _ => {
                let e = got.unwrap_err();
                assert_eq!(
                    (e.code, e.check, e.message.as_str()),
                    (2, "lock", "timed out waiting for the agent lock")
                );
            }
        }
        assert!(!w.folder(ID).join("notice.json").exists());
    }
}
#[test]
fn mark_seen_is_view_proof_not_notice_lifecycle() {
    let w = World::human("");
    let c = w.caller();
    w.agent_record(ID, json!({}));
    w.panes(&["%9|@5|300||"]);
    write(&w.folder(ID).join("notice.json"), json!({"kind":"stopped"}));
    assert!(mark_seen(&w.ctx, &c, "%9", "2026-10-29T11:58:00.0000000Z").unwrap());
    assert_eq!(
        read_json(&w.folder(ID).join("seen.json")).unwrap().unwrap(),
        json!({"since":"2026-10-29T11:58:00.0000000Z"})
    );
    w.panes(&["%8|@5|300|aaaaaaaaaaaa|worker"]);
    assert!(!mark_seen(&w.ctx, &c, "%8", "2026-10-29T11:59:00.0000000Z").unwrap());
    assert!(w.fakes.hc_calls().is_empty());
    w.fakes.hc_done().unwrap();
}
// The side bar's Space on a done member with no view: marked by its id, under the same agent lock and never backward,
// with no pane read. An id no member has, or one that is no agent id, marks nothing.
#[test]
fn mark_seen_id_marks_a_member_without_a_view() {
    let w = World::human("");
    let c = w.caller();
    let folder = w.agent_record(ID, json!({}));
    let file = folder.join("seen.json");
    let seen = file.clone();
    w.fakes
        .lock_probe(move |_| format!("exists={}", seen.exists()));
    w.fakes.clear_log();
    assert!(mark_seen_id(&w.ctx, &c, ID, "2026-10-29T11:58:00.0000000Z").unwrap());
    let agent = lock_key("agent", &c, ID);
    assert_eq!(
        w.fakes.locks(),
        [
            format!("+{agent} exists=false"),
            format!("-{agent} exists=true")
        ]
    );
    assert!(w.fakes.mux_calls().is_empty(), "no pane read");
    let body = || std::fs::read_to_string(&file).unwrap();
    assert_eq!(body(), r#"{"since":"2026-10-29T11:58:00.0000000Z"}"#);
    mark_seen_id(&w.ctx, &c, ID, "2026-10-29T11:50:00.0000000Z").unwrap();
    assert_eq!(body(), r#"{"since":"2026-10-29T11:58:00.0000000Z"}"#);
    assert!(!mark_seen_id(&w.ctx, &c, ID, "soon").unwrap());
    for id in ["bbbbbbbbbbbb", "../x", ""] {
        assert!(
            !mark_seen_id(&w.ctx, &c, id, "2026-10-29T12:00:00.0000000Z").unwrap(),
            "{id:?}"
        );
    }
    assert!(!w.folder("bbbbbbbbbbbb").exists());
    assert!(w.fakes.hc_calls().is_empty());
    w.fakes.hc_done().unwrap();
}
#[test]
fn board_hc_rank_and_terminal_limit() {
    use crate::agents::core_fixture as cf;
    let w = World::human("");
    let c = w.caller();
    w.clock(NOW);
    let mut records = Vec::new();
    for (id, name, pane, state, at) in [
        ("aaaaaaaaaaaa", "detached-a", "%10", "working", "11:59:00"),
        ("bbbbbbbbbbbb", "attached-b", "%11", "working", "11:59:00"),
        ("cccccccccccc", "attached-c", "%12", "working", "11:59:00"),
        ("dddddddddddd", "detached-d", "%13", "blocked", "11:58:00"),
    ] {
        records.push(cf::member(&w, id, json!({"name":name,"pane":pane})));
        write(
            &w.folder(id).join("activity.json"),
            json!({"state":state,"since":format!("2026-10-29T{at}.0000000Z")}),
        );
    }
    let mut panes = vec![
        crate::agents::fixture::row_of("%12|@5|300||", &[]),
        crate::agents::fixture::row_of("%11|@5|301||", &[]),
    ];
    panes[0].command = "hc attach tcccccccccccc".into();
    panes[1].command = "hc attach tbbbbbbbbbbbb".into();
    w.pane_rows(&panes);
    for n in 0..12 {
        let id = format!("{n:012x}");
        records.push(cf::member(&w, &id, json!({"name":format!("e{n}")})));
        write(
            &w.folder(&id).join("notice.json"),
            json!({"kind":"stopped","at":format!("2026-10-29T11:59:{n:02}.0000000Z")}),
        );
    }
    records.sort_by_key(|r| r["hcAddress"].as_str().unwrap().to_string());
    cf::online(&w);
    cf::listing(&w, &super::super::membership::team(&c), &[]);
    for r in &records {
        let mut t = cf::wire(r);
        if r["name"].as_str().unwrap().starts_with('e') {
            t["state"] = json!("finished");
            t["reason"] = json!("killed");
        }
        cf::get(&w, r["hcAddress"].as_str().unwrap(), t);
    }
    let out = board(&w.ctx, &c, true, false).unwrap();
    w.fakes.hc_done().unwrap();
    assert_eq!(
        (&out["running"], &out["endedTotal"]),
        (&json!(4), &json!(12))
    );
    assert_eq!(
        out["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "detached-d",
            "attached-c",
            "attached-b",
            "detached-a",
            "e11",
            "e10",
            "e9",
            "e8",
            "e7",
            "e6",
            "e5",
            "e4",
            "e3",
            "e2"
        ]
    );
}

#[test]
fn a_parentless_ended_leader_shows_in_list_and_the_bar() {
    let w = World::human("");
    let caller = w.caller();
    let folder = member(
        &w,
        ID,
        json!({"name": "leader", "parentPane": null, "depth": 0}),
    );
    w.panes(&[]);
    for watch in [false, true] {
        snapshot(&w, &caller, &[(ID, "0")]);
        let reply = board(&w.ctx, &caller, true, watch).unwrap();
        w.fakes.hc_done().unwrap();
        assert_eq!(
            names(&reply),
            vec!["leader=exited 0".to_string()],
            "watch {watch}"
        );
    }
    assert!(
        folder.join("notice.json").exists()
            && w.state().join("mail").read_dir().unwrap().next().is_none()
    );
}

// The bar's feed keeps every agent that ended in the last hour, however many, then the newest `ENDED_MAX` older ones,
// so each recent end can be selected; each ended row carries `endedAgo`, the seconds since its end, which the fold
// judges by (an agent created 2 h ago that ended 10 s ago is recent). `list -All` keeps its 10 rows and no `endedAgo`.
#[test]
fn the_bar_keeps_every_recent_end_and_says_when_each_ended() {
    let w = World::human("");
    w.clock(NOW);
    let caller = w.caller();
    member(&w, ID, json!({}));
    w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
    let mut ids = Vec::new();
    for n in 0..22u32 {
        let id = format!("{:012x}", n + 1);
        let folder = member(
            &w,
            &id,
            json!({"name": format!("e{n}"), "pane": format!("%{}", 20 + n),
                "createdAt": "2026-10-29T10:00:00.0000000Z"}),
        );
        // e0..e10 ended in the last minute (e0 10 s ago), e11..e21 more than an hour ago.
        let at = match n {
            0..=10 => format!("2026-10-29T11:59:{:02}.0000000Z", 50 - n),
            _ => format!("2026-10-29T10:{:02}:00.0000000Z", 10 + n),
        };
        write(
            &folder.join("notice.json"),
            json!({"kind": "stopped", "exitCode": null, "at": at}),
        );
        ids.push(id);
    }
    let mut facts: Vec<_> = ids.iter().map(|id| (id.as_str(), "stopped")).collect();
    facts.push((ID, "running"));
    snapshot(&w, &caller, &facts);
    let watch = board(&w.ctx, &caller, true, true).unwrap();
    w.fakes.hc_done().unwrap();
    let rows = watch["agents"].as_array().unwrap();
    let ended: Vec<&Value> = rows.iter().filter(|r| r["name"] != "worker").collect();
    assert_eq!(
        ended.len(),
        11 + ENDED_MAX,
        "every recent end, then 10 older"
    );
    assert_eq!(watch["endedTotal"], 22);
    assert_eq!(
        (&ended[0]["name"], &ended[0]["endedAgo"]),
        (&json!("e0"), &json!(10))
    );
    assert_eq!(ended[10]["name"], "e10");
    assert!(ended.iter().all(|r| r["endedAgo"].is_u64()), "{ended:?}");
    snapshot(&w, &caller, &facts);
    let list = board(&w.ctx, &caller, true, false).unwrap();
    w.fakes.hc_done().unwrap();
    let rows = list["agents"].as_array().unwrap();
    assert_eq!(
        (rows.len(), &list["endedTotal"]),
        (1 + ENDED_MAX, &json!(22))
    );
    assert!(rows.iter().all(|r| r.get("endedAgo").is_none()));
}

#[test]
fn the_bar_reads_a_whole_team_as_a_tree_and_nothing_else_changes() {
    use crate::agents::team;
    use crate::parse::Client;
    for source in ["record", "upTeam", "none"] {
        let w = World::human("");
        w.clock(NOW);
        let caller = w.caller();
        let up = if source == "upTeam" {
            json!("T")
        } else {
            Value::Null
        };
        member(
            &w,
            ID,
            json!({"name": "lead", "pane": "%8", "parentPane": null, "parentAgent": null,
                "depth": 0, "upTeam": up, "createdAt": "2026-10-29T11:00:00.0000000Z"}),
        );
        write(
            &w.folder(ID).join("notice.json"),
            json!({"kind": "stopped", "at": "2026-10-29T11:00:30.0000000Z"}),
        );
        if source == "record" {
            let t = team::Team {
                team: "T".into(),
                session: "$3".into(),
                name: "t".into(),
                directory: "/work".into(),
                client: Client::Claude,
                flags: vec![],
                policy: team::policy(Client::Claude, &[]),
                leader: ID.into(),
                created_at: "x".into(),
            };
            assert!(team::create(&team::path(&w.state(), "$3"), &t).unwrap());
        }
        let ids: Vec<String> = (1..=11).map(|n| format!("{n:012x}")).collect();
        let mut facts = vec![(ID, "stopped")];
        for (n, id) in ids.iter().enumerate() {
            member(
                &w,
                id,
                json!({"name": format!("e{n}"), "pane": format!("%{}", 20 + n),
                    "parentAgent": {"id": ID}, "depth": 1, "createdAt": format!("2026-10-29T11:{:02}:00.0000000Z", 10 + n)}),
            );
            write(
                &w.folder(id).join("notice.json"),
                json!({"kind": "stopped", "at": format!("2026-10-29T11:59:{:02}.0000000Z", 10 + n)}),
            );
            facts.push((id.as_str(), "stopped"));
        }
        member(
            &w,
            "cccccccccccc",
            json!({"name": "kid", "pane": "%9", "parentAgent": {"id": ids[0]}, "depth": 2,
                "needsRecovery": true, "createdAt": "2026-10-29T11:30:00.0000000Z"}),
        );
        facts.push(("cccccccccccc", "running"));
        w.panes(&["%9|@5|300|cccccccccccc|kid"]);
        snapshot(&w, &caller, &facts);
        let watch = board(&w.ctx, &caller, true, true).unwrap();
        w.fakes.hc_done().unwrap();
        let team = source != "none";
        let rows = watch["agents"].as_array().unwrap();
        let row = |name: &str| rows.iter().find(|r| r["name"] == name);
        assert_eq!(watch.get("team"), team.then_some(&json!(true)), "{source}");
        // All 13 either way: every agent here ended in the last hour, which the bar's feed always keeps (the cap on
        // older ends is `the_bar_keeps_every_recent_end_and_says_when_each_ended`). In a team the ended leader and the
        // ended parent e0 head their children.
        assert_eq!(rows.len(), 13, "{source}");
        assert!(row("lead").is_some() && row("e0").is_some(), "{source}");
        let kid = row("kid").unwrap();
        assert_eq!(
            (&kid["id"], &kid["needsRecovery"], &kid["view"]),
            (&json!("cccccccccccc"), &json!(true), &json!("attached"))
        );
        assert!(rows.iter().all(|r| r["id"].is_string()), "{source}");
        let tree = |r: &Value| {
            (
                r["parentId"].clone(),
                r["depth"].clone(),
                r["leader"].clone(),
            )
        };
        if team {
            assert_eq!(
                tree(row("lead").unwrap()),
                (Value::Null, json!(0), json!(true))
            );
            assert_eq!(
                tree(row("e0").unwrap()),
                (json!(ID), json!(1), json!(false))
            );
            assert_eq!(tree(kid), (json!(ids[0]), json!(2), json!(false)));
        } else {
            assert!(
                rows.iter()
                    .all(|r| r.get("parentId").is_none() && r.get("leader").is_none())
            );
        }
        snapshot(&w, &caller, &facts);
        let list = board(&w.ctx, &caller, true, false).unwrap();
        w.fakes.hc_done().unwrap();
        let rows = list["agents"].as_array().unwrap();
        assert_eq!(
            (list.get("team"), rows.len()),
            (None, 11),
            "{source}: list keeps its cap"
        );
        assert!(
            rows.iter()
                .all(|r| r.get("id").is_none() && r.get("parentId").is_none())
        );
    }
}

#[test]
fn the_team_bar_counts_mail_for_every_member_it_draws() {
    for team in [true, false] {
        let w = World::human("");
        w.clock(NOW);
        let caller = w.caller();
        let up = if team { json!("T") } else { Value::Null };
        member(
            &w,
            ID,
            json!({"name": "lead", "pane": "%8", "parentPane": null, "parentAgent": null, "depth": 0, "upTeam": up,
                "createdAt": "2026-10-29T11:00:00.0000000Z"}),
        );
        member(
            &w,
            "cccccccccccc",
            json!({"name": "kid", "pane": "%9", "parentPane": "%8", "parentAgent": {"id": ID}, "depth": 1,
                "createdAt": "2026-10-29T11:30:00.0000000Z"}),
        );
        w.panes(&["%8|@5|300|aaaaaaaaaaaa|lead"]);
        // The Lost kid's mail to its parent sits in the running leader's mailbox, as `send parent` delivers it.
        let lead = read_json(&w.folder(ID).join("record.json"))
            .unwrap()
            .unwrap();
        add_mail(
            &w.ctx,
            &caller,
            &crate::agents::mail::member_mailbox(ID, &lead),
            "kid",
            "%9",
            "x",
            None,
        )
        .unwrap();
        snapshot(&w, &caller, &[(ID, "running"), ("cccccccccccc", "lost")]);
        let reply = board(&w.ctx, &caller, true, true).unwrap();
        w.fakes.hc_done().unwrap();
        let kid = reply["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "kid")
            .unwrap()
            .clone();
        // Every drawn member's count is read, in a team or not, whatever its life (`Mail::source`).
        assert_eq!(
            (&kid["life"], &kid["unread"]),
            (&json!("lost"), &json!(1)),
            "team {team}"
        );
    }
}

#[test]
fn a_child_unread_follows_its_parent_record_when_the_parent_loses_its_view() {
    for (case, life) in [
        ("attached", "running"),
        ("detached", "running"),
        ("lost", "lost"),
        ("ended", "stopped"),
    ] {
        let w = World::human("");
        w.clock(NOW);
        let caller = w.caller();
        // The parent inherited a mailbox (a resume): its record names it, and `send parent` delivered there.
        member(
            &w,
            ID,
            json!({"name": "lead", "pane": "%8", "parentPane": null, "parentAgent": null, "depth": 0, "upTeam": "T",
                "mailbox": "dddddddddddd", "createdAt": "2026-10-29T11:00:00.0000000Z"}),
        );
        member(
            &w,
            "cccccccccccc",
            json!({"name": "kid", "pane": "%9", "parentPane": "%8", "parentAgent": {"id": ID}, "depth": 1,
                "createdAt": "2026-10-29T11:30:00.0000000Z"}),
        );
        let mut panes = vec!["%9|@6|301|cccccccccccc|kid"];
        if case == "attached" {
            panes.push("%8|@5|300|aaaaaaaaaaaa|lead");
        }
        w.panes(&panes);
        add_mail(&w.ctx, &caller, "dddddddddddd", "kid", "%9", "x", None).unwrap();
        snapshot(&w, &caller, &[(ID, life), ("cccccccccccc", "running")]);
        let reply = board(&w.ctx, &caller, true, true).unwrap();
        w.fakes.hc_done().unwrap();
        let kid = reply["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "kid")
            .unwrap()
            .clone();
        assert_eq!(kid["unread"], json!(1), "{case}: the parent is {case}");
    }
}

// review: a team row's unread follows its member, not its pane. Real delivery (send's own mailer and the board's
// reaper posting the exit notice), the view closing and reopening on another pane, then the board.
#[test]
fn a_team_rows_unread_follows_its_member_across_a_reopened_view() {
    for team in [true, false] {
        let w = World::human("");
        w.clock(NOW);
        let caller = w.caller();
        let up = if team { json!("T") } else { Value::Null };
        member(
            &w,
            ID,
            json!({"name": "lead", "pane": "%8", "parentPane": null, "parentAgent": null, "depth": 0, "upTeam": up,
                "createdAt": "2026-10-29T11:00:00.0000000Z"}),
        );
        let kid = "cccccccccccc";
        member(
            &w,
            kid,
            json!({"name": "kid", "pane": "%9", "parentPane": "%8", "parentAgent": {"id": ID}, "depth": 1,
                "createdAt": "2026-10-29T11:30:00.0000000Z"}),
        );
        // `send parent` from the kid on %9: the running lead on %8 gets it, through the mailer `send` uses.
        let lead = read_json(&w.folder(ID).join("record.json"))
            .unwrap()
            .unwrap();
        let to = crate::agents::mail::member_mailbox(ID, &lead);
        crate::agents::mail::add_member_mail(&w.ctx, &caller, &to, "kid", kid, "%9", "hi").unwrap();
        // The kid's view closed and reopened on %19, as `restore_view` records it.
        let mut r = read_json(&w.folder(kid).join("record.json"))
            .unwrap()
            .unwrap();
        r["pane"] = json!("%19");
        r["startCommand"] = crate::agents::fixture::launch_command("%19").into();
        write(&w.folder(kid).join("record.json"), r);
        w.panes(&["%8|@5|300|aaaaaaaaaaaa|lead"]);
        // The kid then stopped: the board's reaper posts its exit notice to `pane-8` (registry::add_notice).
        snapshot(&w, &caller, &[(ID, "running"), (kid, "stopped")]);
        let reply = board(&w.ctx, &caller, true, true).unwrap();
        w.fakes.hc_done().unwrap();
        let notices = std::fs::read_to_string(w.state().join("mail").join("pane-8.jsonl")).unwrap();
        assert!(
            notices.contains("exit-cccccccccccc"),
            "team {team}: the notice was delivered: {notices}"
        );
        let row = reply["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "kid")
            .unwrap()
            .clone();
        // The kid's message counts, in a team or not; the notice of its own exit never does.
        assert_eq!(row["unread"], json!(1), "team {team}");
    }
}

// cascade-3d C8: the bar's refresh (`watch`) finishes a kept cascade first; `list` (no `watch`) does not.
#[test]
fn a_watch_board_finishes_a_cascade_with_one_try() {
    use crate::agents::cascade_fixture as cx;
    for watch in [false, true] {
        let w = World::human("");
        w.state();
        cx::member(&w, cx::A, None, 0, json!({}));
        cx::done(&w, cx::A);
        cx::seed(&w, cx::A, &[cx::A], true);
        let _ = board(&w.ctx, &w.caller(), true, watch); // its own hc reads are not scripted here
        assert_eq!(cx::journal_of(&w, cx::A).exists(), !watch, "watch {watch}");
    }
}

fn t_record(w: &World) {
    use crate::agents::team;
    use crate::parse::Client;
    let t = team::Team {
        team: "T".into(),
        session: "$3".into(),
        name: "t".into(),
        directory: "/work".into(),
        client: Client::Claude,
        flags: vec![],
        policy: team::policy(Client::Claude, &[]),
        leader: ID.into(),
        created_at: "x".into(),
    };
    assert!(team::create(&team::path(&w.state(), "$3"), &t).unwrap());
}

// Call 10, call 25 (checklist 9): the bar draws B under S, the successor of B's recorded parent A (A has no row of
// its own), and counts B's mail in S's mailbox, the one `send parent` reaches.
#[test]
fn a_successor_is_shown_as_the_parent_of_its_predecessors_children() {
    let w = World::human("");
    w.clock(NOW);
    let caller = w.caller();
    let (a, s, b) = ("aaaaaaaaaaa2", "555555555552", "bbbbbbbbbbb2");
    member(
        &w,
        ID,
        json!({"name": "lead", "pane": "%8", "parentPane": null, "parentAgent": null, "depth": 0,
            "upTeam": "T", "createdAt": "2026-10-29T11:00:00.0000000Z"}),
    );
    t_record(&w);
    member(
        &w,
        a,
        json!({"name": "a", "pane": "%21", "parentAgent": {"id": ID}, "depth": 1,
            "createdAt": "2026-10-29T11:01:00.0000000Z"}),
    );
    write(
        &w.folder(a).join("notice.json"),
        json!({"kind": "stopped", "at": "2026-10-29T11:02:00.0000000Z"}),
    );
    member(
        &w,
        s,
        json!({"name": "a", "pane": "%22", "parentAgent": {"id": ID}, "depth": 1, "resumedFrom": a,
            "createdAt": "2026-10-29T11:03:00.0000000Z"}),
    );
    member(
        &w,
        b,
        json!({"name": "b", "pane": "%23", "parentPane": "%21", "parentAgent": {"id": a}, "depth": 2,
            "createdAt": "2026-10-29T11:04:00.0000000Z"}),
    );
    w.panes(&["%22|@5|300|555555555552|a", "%23|@5|301|bbbbbbbbbbb2|b"]);
    add_mail(&w.ctx, &caller, s, "b", "%23", "to S", None).unwrap();
    snapshot(
        &w,
        &caller,
        &[
            (ID, "running"),
            (a, "stopped"),
            (s, "running"),
            (b, "running"),
        ],
    );
    let watch = board(&w.ctx, &caller, true, true).unwrap();
    w.fakes.hc_done().unwrap();
    let rows = watch["agents"].as_array().unwrap();
    let row = rows.iter().find(|r| r["name"] == "b").unwrap();
    assert_eq!(row["parentId"], json!(s), "B is drawn under S");
    assert_eq!(row["unread"], json!(1), "B's mail in S's mailbox");
}

// Call 21 (checklist 1): a parent loop never blanks the bar. Every member is listed; the two on the loop are marked
// unreadable and keep their raw parent ids; the rest are drawn as before.
#[test]
fn a_parent_loop_marks_its_members_and_the_bar_lists_the_rest() {
    let w = World::human("");
    w.clock(NOW);
    let caller = w.caller();
    let (kid, x, y) = ("eeeeeeeeee01", "dddddddddd01", "dddddddddd02");
    member(
        &w,
        ID,
        json!({"name": "lead", "pane": "%8", "parentPane": null, "parentAgent": null, "depth": 0,
            "upTeam": "T", "createdAt": "2026-10-29T11:00:00.0000000Z"}),
    );
    t_record(&w);
    for (id, name, pane, parent) in [
        (kid, "kid", "%21", ID),
        (x, "x", "%22", y),
        (y, "y", "%23", x),
    ] {
        member(
            &w,
            id,
            json!({"name": name, "pane": pane, "parentAgent": {"id": parent}, "depth": 1,
                "createdAt": "2026-10-29T11:01:00.0000000Z"}),
        );
    }
    w.panes(&[
        "%21|@5|300|eeeeeeeeee01|kid",
        "%22|@5|301|dddddddddd01|x",
        "%23|@5|302|dddddddddd02|y",
    ]);
    snapshot(
        &w,
        &caller,
        &[
            (ID, "running"),
            (kid, "running"),
            (x, "running"),
            (y, "running"),
        ],
    );
    let watch = board(&w.ctx, &caller, true, true).unwrap();
    w.fakes.hc_done().unwrap();
    let rows = watch["agents"].as_array().unwrap();
    let mut got: Vec<(String, Value, Value)> = rows
        .iter()
        .map(|r| {
            let mark = r.get("unreadable").cloned().unwrap_or(Value::Null);
            (
                r["name"].as_str().unwrap().to_string(),
                r["parentId"].clone(),
                mark,
            )
        })
        .collect();
    got.sort_by(|p, q| p.0.cmp(&q.0));
    assert_eq!(
        got,
        [
            ("kid".to_string(), json!(ID), Value::Null),
            ("lead".to_string(), Value::Null, Value::Null),
            ("x".to_string(), json!(y), json!(true)),
            ("y".to_string(), json!(x), json!(true)),
        ]
    );
}

// Re6a V3 (checklist R7, R8): a parent pane names its server through the record's `parentServerKey`. `parent_key`
// matches a running agent on the parent pane only on the same server (R7, non-team); its fallback and the team
// notice box are each the qualified `pane-<K2>-<n>`, never the anchor's `pane-<n>` (R8, team), read through the
// real board unread result.
#[test]
fn a_parent_pane_on_another_server_counts_only_its_qualified_mailbox() {
    const K2: &str = "0123456789abcdef";
    for team in [false, true] {
        let w = World::human("");
        w.clock(NOW);
        let caller = w.caller();
        let up = if team { json!("T") } else { Value::Null };
        member(
            &w,
            ID,
            json!({"name": "lead", "pane": "%8", "parentPane": null, "parentAgent": null, "depth": 0, "upTeam": up,
                "createdAt": "2026-10-29T11:00:00.0000000Z"}),
        );
        member(
            &w,
            "cccccccccccc",
            json!({"name": "kid", "pane": "%9", "parentPane": "%8", "parentServerKey": K2,
                "parentAgent": if team { json!({"id": ID}) } else { Value::Null }, "depth": 1,
                "createdAt": "2026-10-29T11:30:00.0000000Z"}),
        );
        w.panes(&["%8|@5|300|aaaaaaaaaaaa|lead", "%9|@5|301|cccccccccccc|kid"]);
        // The kid's mail: 1 in its parent's qualified box, 3 in the anchor's `pane-8`, and (outside a team, where
        // the lead is no parent) 2 in the lead's own box that runs on the same pane number here.
        let qualified = format!("pane-{K2}-8");
        let mut boxes = vec![(qualified.as_str(), 1), ("pane-8", 3)];
        if !team {
            boxes.push((ID, 2));
        }
        for (key, n) in boxes {
            for _ in 0..n {
                add_mail(&w.ctx, &caller, key, "kid", "%9", "x", None).unwrap();
            }
        }
        snapshot(&w, &caller, &[(ID, "running"), ("cccccccccccc", "running")]);
        let reply = board(&w.ctx, &caller, true, true).unwrap();
        w.fakes.hc_done().unwrap();
        let kid = reply["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "kid")
            .unwrap()
            .clone();
        assert_eq!(kid["unread"], json!(1), "team {team}: only {qualified}");
    }
}
