use super::*;
use crate::agents::fixture::World;
use crate::agents::team_mail::tests::{A, inbox, online, team_world};
use serde_json::{Value, json};

/// `ping <name>` as the caller the world resolves (a person in `team_world`); the hc answers it uses must all be used.
fn ping(w: &World, name: &str) -> Value {
    let got = ping_as(&w.ctx, &w.caller(), name).map(Value::Object);
    w.fakes.hc_done().unwrap();
    got.unwrap()
}

fn record(w: &World, id: &str) -> Value {
    crate::state::fs::read_json(&w.folder(id).join("ping.json"))
        .unwrap()
        .unwrap()
}

// Behaviour 1: the ping is a record; ordinary team mail (no `kind`) names its id; a second ping before an answer
// sends nothing and returns the same id.
#[test]
fn a_person_pings_once_and_a_second_ping_returns_the_same_id() {
    let w = team_world();
    online(&w);
    online(&w);
    let first = ping(&w, "ma1");
    let id = first["id"].as_str().unwrap().to_string();
    assert_eq!(
        (first["name"].clone(), first["notified"].clone()),
        (json!("ma1"), json!(true))
    );
    let r = record(&w, A);
    assert_eq!(
        (
            r["id"].clone(),
            r["generation"].clone(),
            r["sent"].clone(),
            r["by"].clone()
        ),
        (json!(id), json!("0"), json!(true), Value::Null)
    );
    assert_eq!(r["askedAt"], first["askedAt"]);
    let mail = inbox(&w, A);
    assert_eq!(mail.len(), 1, "{mail:?}");
    assert_eq!(
        mail[0]["body"],
        format!(
            "Status? Answer with: mux-agent answer {id} -BodyFile <file: doing / done / blocked on what>"
        )
    );
    assert!(mail[0].get("kind").is_none(), "ordinary mail: {}", mail[0]);
    online(&w);
    let second = ping(&w, "ma1");
    assert_eq!(
        (second["id"].clone(), second["notified"].clone()),
        (json!(id), json!(false))
    );
    assert_eq!(inbox(&w, A).len(), 1, "nothing sent");
}

/// One membership snapshot's hc answers in a world whose caller no longer resolves (`Mw::hc`): every hc member of
/// session $3 under `state` running, in address order.
fn snapshot(w: &World, state: &std::path::Path, team: &str) {
    let mut records: Vec<Value> = std::fs::read_dir(state.join("agents"))
        .unwrap()
        .filter_map(|e| {
            crate::state::fs::read_json(&e.unwrap().path().join("record.json"))
                .ok()
                .flatten()
        })
        .filter(|r| r.get("hcAddress").is_some() && r["session"] == "$3")
        .collect();
    records.sort_by_key(|r| crate::agents::text(r.get("hcAddress")));
    crate::agents::core_fixture::online(w);
    crate::agents::core_fixture::listing(w, team, &[]);
    for r in &records {
        crate::agents::core_fixture::get(
            w,
            r["hcAddress"].as_str().unwrap(),
            crate::agents::core_fixture::wire(r),
        );
    }
}

fn mail_of(state: &std::path::Path, key: &str) -> Vec<Value> {
    std::fs::read_to_string(state.join("mail").join(format!("{key}.jsonl")))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn run(w: &World, tokens: &[&str]) -> (u8, Value) {
    let argv: Vec<String> = tokens.iter().map(|t| t.to_string()).collect();
    let (code, out) = crate::cli::run(&argv, || Ok(&w.ctx));
    (
        code,
        serde_json::from_str(out.trim()).unwrap_or(Value::Null),
    )
}

// Behaviour 1: the team's leader pings as the person does; the record names it as the asker.
#[test]
fn the_leader_pings_a_member() {
    use crate::agents::tree::tests::{OTHER, leader_world, term};
    let m = leader_world();
    m.rounds(3, &term(&m.r, Some(150)));
    let team = crate::agents::membership::team(&m.c);
    snapshot(&m.w, &m.state, &team);
    snapshot(&m.w, &m.state, &team);
    let (code, got) = run(&m.w, &["ping", "mbb"]);
    assert_eq!(code, 0, "{got}");
    let id = got["id"].as_str().unwrap();
    let r: Value = crate::state::fs::read_json(&m.folder(OTHER).join("ping.json"))
        .unwrap()
        .unwrap();
    assert_eq!(
        (r["id"].as_str(), r["by"].as_str(), r["sent"].clone()),
        (Some(id), Some(crate::agents::fixture::ID), json!(true))
    );
    let mail = mail_of(&m.state, OTHER);
    assert_eq!(mail.len(), 1, "{mail:?}");
    assert!(
        mail[0]["body"]
            .as_str()
            .unwrap()
            .contains(&format!("mux-agent answer {id} -BodyFile"))
    );
    m.w.fakes.hc_done().unwrap();
}

// Behaviour 2: only the team's leader or the person pings, and only a live member of the caller's team.
#[test]
fn a_non_leader_member_an_unknown_and_an_ended_name_are_refused() {
    use crate::agents::tree::tests::{OTHER, member_world, term};
    let mut m = member_world();
    m.hc();
    m.rounds(3, &term(&m.r, Some(150)));
    let (code, got) = run(&m.w, &["ping", "mbb"]);
    assert_eq!(
        (code, got["check"].clone(), got["message"].clone()),
        (
            2,
            json!("authority"),
            json!("only the team's leader or a person can ping mbb")
        ),
        "{got}"
    );
    assert!(!m.folder(OTHER).join("ping.json").exists());
    m.w.fakes.hc_done().unwrap();
    for (name, case) in [("nobody", ""), ("ma1", "ended")] {
        let w = team_world();
        crate::agents::team_mail::tests::online_but(&w, A, case);
        let e = ping_as(&w.ctx, &w.caller(), name).unwrap_err();
        assert_eq!(
            (e.code, e.check, e.message.as_str()),
            (2, "target", "no agent of this team has that name"),
            "{name}"
        );
        assert!(!w.folder(A).join("ping.json").exists(), "{name}");
        assert!(inbox(&w, A).is_empty(), "{name}");
    }
}

fn as_member(w: &World, id: &str) -> crate::caller::Caller {
    let mut c = w.caller();
    c.member = Some(id.into());
    c
}

fn file(folder: &std::path::Path, name: &str) -> Value {
    crate::state::fs::read_json(&folder.join(name))
        .unwrap()
        .unwrap()
}

fn said(e: crate::error::Refusal) -> String {
    format!("{} {} {}", e.code, e.check, e.message)
}

// Behaviour 3: the member's own `answer <id>` closes the ping, and its text is the member's status; an older ping's id
// answers nothing. Answered is the id match, never a clock: at the ping's own instant and with its `askedAt` ahead of
// the clock (the clock moved backward) alike.
#[test]
fn the_members_answer_closes_the_ping_and_an_older_id_does_not() {
    let w = team_world();
    online(&w);
    online(&w);
    let first = ping(&w, "ma1")["id"].as_str().unwrap().to_string();
    w.file("a.md", b"tests green; opening the PR\nmore\n");
    online(&w);
    let got = answer_as(&w.ctx, &as_member(&w, A), &first, "a.md").unwrap();
    w.fakes.hc_done().unwrap();
    assert_eq!(
        (
            got["name"].clone(),
            got["ping"].clone(),
            got["text"].clone()
        ),
        (
            json!("ma1"),
            json!(first),
            json!("tests green; opening the PR")
        )
    );
    let folder = w.folder(A);
    assert_eq!(file(&folder, "answer.json")["ping"], json!(first));
    assert_eq!(
        file(&folder, "answer.json")["text"],
        "tests green; opening the PR"
    );
    assert_eq!(
        file(&folder, "status.json")["text"],
        "tests green; opening the PR"
    );
    assert_eq!(
        open(&folder, &json!("0")),
        None,
        "answered at the ping's own instant"
    );
    // A new ping after the answer is a new id; the old id answers nothing and leaves it open.
    online(&w);
    online(&w);
    let second = ping(&w, "ma1")["id"].as_str().unwrap().to_string();
    assert_ne!(second, first);
    online(&w);
    let e = answer_as(&w.ctx, &as_member(&w, A), &first, "a.md").unwrap_err();
    assert_eq!(said(e), "2 ping no open ping with that id");
    assert_eq!(
        open(&folder, &json!("0")).map(|o| o.id),
        Some(second.clone())
    );
    // Its askedAt an hour ahead of the clock (the clock moved backward): its own id still closes it.
    let mut r = file(&folder, "ping.json");
    r["askedAt"] = json!(utc_o(
        w.ctx.clock.now() + std::time::Duration::from_secs(3600)
    ));
    crate::state::fs::write_json(&folder.join("ping.json"), &r).unwrap();
    online(&w);
    answer_as(&w.ctx, &as_member(&w, A), &second, "a.md").unwrap();
    assert_eq!(
        open(&folder, &json!("0")),
        None,
        "answered with the clock behind it"
    );
    w.fakes.hc_done().unwrap();
}

// Behaviour 2: another member, an unknown id, a person and `-Body` cannot answer; the ping stays open.
#[test]
fn another_member_an_unknown_id_a_person_and_body_cannot_answer() {
    use crate::agents::team_mail::tests::B;
    let w = team_world();
    online(&w);
    online(&w);
    let id = ping(&w, "ma1")["id"].as_str().unwrap().to_string();
    w.file("a.md", b"done");
    online(&w);
    let e = answer_as(&w.ctx, &as_member(&w, B), &id, "a.md").unwrap_err();
    assert_eq!(
        said(e),
        "2 ping no open ping with that id",
        "another member"
    );
    online(&w);
    let e = answer_as(&w.ctx, &as_member(&w, A), "ffffffffffffffff", "a.md").unwrap_err();
    assert_eq!(said(e), "2 ping no open ping with that id", "an unknown id");
    let e = answer_as(&w.ctx, &w.caller(), &id, "a.md").unwrap_err();
    assert_eq!(said(e), "2 target only a member answers a ping", "a person");
    let (code, got) = run(&w, &["answer", &id, "-Body", "done"]);
    assert_eq!(
        (code, got["check"].clone(), got["message"].clone()),
        (
            2,
            json!("usage"),
            json!("usage: mux-agent answer <pingId> -BodyFile <path> [-Text]")
        )
    );
    let (code, got) = run(&w, &["answer", &id]);
    assert_eq!(
        (code, got["check"].clone()),
        (2, json!("usage")),
        "no -BodyFile"
    );
    assert_eq!(open(&w.folder(A), &json!("0")).map(|o| o.id), Some(id));
    assert!(!w.folder(A).join("answer.json").exists());
    assert!(!w.folder(B).join("answer.json").exists());
    w.fakes.hc_done().unwrap();
}

// Behaviour 3: the member's status, its `report idle`, its sends (to the person, with and without -Re; to the leader
// and a sibling in a team) and its reading and acking the notice never answer the ping.
#[test]
fn status_report_sends_and_reading_the_notice_never_answer() {
    use crate::caller_hc_tests::{proof, wire, worker};
    let (w, c, r, folder) = worker();
    w.file("body.txt", b"hello");
    let generation = r["generation"].clone();
    let asked = "0123456789abcdef";
    crate::state::fs::write_json(
        &folder.join("ping.json"),
        &json!({"id": asked, "askedAt": utc_o(w.ctx.clock.now()), "by": null, "generation": generation, "sent": true}),
    )
    .unwrap();
    let notice = "1111111111111111";
    crate::agents::mail::deliver_once(
        &w.ctx,
        &c,
        &crate::agents::mail::member_mailbox(crate::agents::fixture::ID, &r),
        &json!({"id": notice, "from": "root", "pane": "%1", "at": "x",
            "body": format!("Status? Answer with: mux-agent answer {asked} -BodyFile <file>"),
            "fromBox": "pane-1", "re": null, "hop": 0, "cast": null}),
    )
    .unwrap();
    let address = r["hcAddress"].as_str().unwrap();
    let mut batch = String::new();
    for args in [
        vec!["status", "-BodyFile", "body.txt"],
        vec!["report", "idle"],
        vec!["send", "parent", "-BodyFile", "body.txt"],
        vec!["send", "parent", "-BodyFile", "body.txt", "-Re", notice],
        vec!["read"],
        vec!["read", "-Ack", "<batch>"],
    ] {
        proof(&w, &r);
        proof(&w, &r);
        crate::agents::core_fixture::online(&w);
        crate::agents::core_fixture::listing(&w, &crate::agents::membership::team(&c), &[address]);
        crate::agents::core_fixture::get(&w, address, wire(&r));
        if args[0] == "read" {
            crate::agents::core_fixture::online(&w);
            crate::agents::core_fixture::get(&w, address, wire(&r));
        }
        let args: Vec<String> = args.iter().map(|a| a.replace("<batch>", &batch)).collect();
        let (code, out) = crate::cli::run(&args, || Ok(&w.ctx));
        assert_eq!(code, 0, "{args:?}: {out}");
        let out: Value = serde_json::from_str(&out).unwrap();
        if let Some(b) = out.get("batch").and_then(Value::as_str) {
            batch = b.into();
        }
        assert_eq!(
            open(&folder, &generation).map(|o| o.id),
            Some(asked.into()),
            "{args:?}"
        );
        w.fakes.hc_done().unwrap();
    }
    assert!(!batch.is_empty(), "the notice was read");
    // In a team: sends to the leader and to another member, and a reply to the leader's message.
    let w = team_world();
    online(&w);
    online(&w);
    let id = ping(&w, "ma1")["id"].as_str().unwrap().to_string();
    let leader = crate::agents::tree::tests::LEADER;
    crate::agents::mail::deliver_once(&w.ctx, &w.caller(), A, &json!({"id": "2222222222222222", "from": "leader",
        "fromId": leader, "pane": "%4", "at": "x", "body": "q", "fromBox": leader, "re": null, "hop": 0, "cast": null}))
    .unwrap();
    for args in [
        &["send", "leader", "-Body", "x"][..],
        &["send", "mb2", "-Body", "x"],
        &["send", "leader", "-Body", "x", "-Re", "2222222222222222"],
    ] {
        online(&w);
        crate::agents::message::send_as(&w.ctx, &as_member(&w, A), &w.request(args)).unwrap();
        w.fakes.hc_done().unwrap();
        assert_eq!(
            open(&w.folder(A), &json!("0")).map(|o| o.id),
            Some(id.clone()),
            "{args:?}"
        );
    }
}
