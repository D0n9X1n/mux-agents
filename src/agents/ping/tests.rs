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
    // In a team: sends to the leader, to w1's child, to a sibling (mc3, also under the leader), and a reply to the
    // leader's message.
    let w = team_world();
    let leader = crate::agents::tree::tests::LEADER;
    crate::agents::core_fixture::member(
        &w,
        crate::agents::team_mail::tests::C,
        json!({"name": "mc3", "depth": 1, "parentAgent": {"id": leader}, "pane": "%8", "window": "@1"}),
    );
    online(&w);
    online(&w);
    let id = ping(&w, "ma1")["id"].as_str().unwrap().to_string();
    crate::agents::mail::deliver_once(&w.ctx, &w.caller(), A, &json!({"id": "2222222222222222", "from": "leader",
        "fromId": leader, "pane": "%4", "at": "x", "body": "q", "fromBox": leader, "re": null, "hop": 0, "cast": null}))
    .unwrap();
    for args in [
        &["send", "leader", "-Body", "x"][..],
        &["send", "mb2", "-Body", "x"],
        &["send", "mc3", "-Body", "x"],
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

// Behaviour 3a: a crash after the record and before the notice, or after the notice and before `sent`: the next ping
// keeps the same id and ends `sent: true` (at worst two notices naming that one id), and the member's answer to that
// id closes it.
#[test]
fn a_crash_on_either_side_of_the_notice_keeps_the_ping_id() {
    for (point, mails) in [("published", 0), ("notified", 1)] {
        let w = team_world();
        online(&w);
        if point == "notified" {
            online(&w);
        }
        crash_after(Some(point));
        let e = ping_as(&w.ctx, &w.caller(), "ma1").unwrap_err();
        crash_after(None);
        w.fakes.hc_done().unwrap();
        assert_eq!((e.code, e.check), (1, "internal"), "{point}");
        let r = record(&w, A);
        assert_eq!(
            (r["sent"].clone(), inbox(&w, A).len()),
            (json!(false), mails),
            "{point}"
        );
        let id = r["id"].as_str().unwrap().to_string();
        online(&w);
        online(&w);
        let again = ping(&w, "ma1");
        assert_eq!(
            (again["id"].clone(), again["notified"].clone()),
            (json!(id), json!(true)),
            "{point}"
        );
        assert_eq!(record(&w, A)["sent"], json!(true), "{point}");
        let mail = inbox(&w, A);
        assert_eq!(mail.len(), mails + 1, "{point}");
        assert!(
            mail.iter()
                .all(|m| m["body"].as_str().unwrap().contains(&id)),
            "{point}: every notice names the one id"
        );
        w.file("a.md", b"ok");
        online(&w);
        answer_as(&w.ctx, &as_member(&w, A), &id, "a.md").unwrap();
        assert_eq!(open(&w.folder(A), &json!("0")), None, "{point}");
        w.fakes.hc_done().unwrap();
    }
}

// Behaviour 3a: ping A is delivered and paused before `sent`; the member answers A; ping B publishes and stops before
// its notice; A resumes: B's record is left as it is (`sent: false`), and B's next ping sends B's notice.
#[test]
fn a_late_confirm_never_marks_a_newer_ping_sent() {
    let w = team_world();
    let c = w.caller();
    online(&w);
    online(&w);
    let a = publish(&w.ctx, &c, "ma1").unwrap();
    notify(&w.ctx, &c, &a).unwrap();
    w.file("a.md", b"ok");
    online(&w);
    answer_as(&w.ctx, &as_member(&w, A), &a.open.id, "a.md").unwrap();
    online(&w);
    crash_after(Some("published"));
    ping_as(&w.ctx, &c, "ma1").unwrap_err();
    crash_after(None);
    let b = record(&w, A);
    assert_ne!(b["id"], json!(a.open.id));
    confirm(&w.ctx, &c, &a).unwrap();
    assert_eq!(record(&w, A), b, "B's record is untouched");
    assert_eq!(b["sent"], json!(false));
    online(&w);
    online(&w);
    let again = ping(&w, "ma1");
    assert_eq!(
        (again["id"].clone(), again["notified"].clone()),
        (b["id"].clone(), json!(true))
    );
    let mail = inbox(&w, A);
    assert_eq!(mail.len(), 2, "A's notice, then B's");
    assert!(
        mail[1]["body"]
            .as_str()
            .unwrap()
            .contains(b["id"].as_str().unwrap())
    );
    w.fakes.hc_done().unwrap();
}

// Behaviour 3a: a ping of w1 and w1's own send, at once on the real locks, both finish: the ping holds no lock across
// its notice, and each takes the team lock, then the agent lock.
#[test]
fn a_ping_and_the_members_own_send_both_finish() {
    use crate::agents::tree::tests::LEADER;
    let w = team_world();
    let (locks, state) = (
        w.ctx.state_root.parent().unwrap().join("locks"),
        w.ctx.state_root.clone(),
    );
    let world = move || {
        let mut v = World::human("");
        v.ctx.state_root = state.clone();
        v.fakes.real_locks(&locks);
        v.pane_rows(&[]);
        v
    };
    let pinger = {
        let world = world.clone();
        std::thread::spawn(move || {
            let v = world();
            (0..5)
                .map(|_| {
                    online(&v);
                    online(&v);
                    ping_as(&v.ctx, &v.caller(), "ma1").map(|r| r["id"].clone())
                })
                .collect::<Vec<_>>()
        })
    };
    let sender = std::thread::spawn(move || {
        let v = world();
        (0..5)
            .map(|_| {
                online(&v);
                let request = v.request(&["send", "leader", "-Body", "x"]);
                crate::agents::message::send_as(&v.ctx, &as_member(&v, A), &request).map(|_| ())
            })
            .collect::<Vec<_>>()
    });
    let pings = pinger.join().unwrap();
    let sends = sender.join().unwrap();
    assert!(sends.iter().all(Result::is_ok), "{sends:?}");
    let ids: Vec<Value> = pings.into_iter().map(Result::unwrap).collect();
    assert!(ids.iter().all(|id| *id == ids[0]), "one open ping: {ids:?}");
    assert_eq!(record(&w, A)["sent"], json!(true));
    assert_eq!(inbox(&w, A).len(), 1, "one notice");
    assert_eq!(
        inbox(&w, LEADER).len(),
        5,
        "every send of the member landed"
    );
}

// Behaviour 5: a member resumed into a new generation after a ping never inherits it: the old record is no open
// ping, its id answers nothing, and the next ping is a new one at the new generation.
#[test]
fn a_new_generation_ignores_the_old_ping() {
    let w = team_world();
    online(&w);
    online(&w);
    let old = ping(&w, "ma1")["id"].as_str().unwrap().to_string();
    let mut r = file(&w.folder(A), "record.json");
    (r["generation"], r["hcName"]) = (json!("1"), json!("m0123abcd"));
    crate::state::fs::write_json(&w.folder(A).join("record.json"), &r).unwrap();
    assert_eq!(open(&w.folder(A), &json!("1")), None);
    w.file("a.md", b"ok");
    online(&w);
    let e = answer_as(&w.ctx, &as_member(&w, A), &old, "a.md").unwrap_err();
    assert_eq!(said(e), "2 ping no open ping with that id");
    online(&w);
    online(&w);
    let new = ping(&w, "ma1");
    assert_ne!(new["id"], json!(old));
    assert_eq!(
        (new["notified"].clone(), record(&w, A)["generation"].clone()),
        (json!(true), json!("1"))
    );
    w.fakes.hc_done().unwrap();
}

// Behaviour 3a, and the one lock order: the team lock, then the member's member lock, then its agent lock, never the
// other way round. Resume and recovery change the generation under the member lock (inside the team lock), so a ping or
// an answer that took them the other way could each wait for a lock the other holds. The ping holds none of them
// across its notice: the notice's send takes the team lock with the member's locks free.
#[test]
fn the_notice_is_sent_with_the_members_locks_free_and_in_one_order() {
    let w = team_world();
    let c = w.caller();
    let (team, member, agent) = (
        crate::agents::team_lock(&c),
        crate::agents::lock_key("member", &c, A),
        crate::agents::lock_key("agent", &c, A),
    );
    // Each event checked against the locks held at that moment; the holds of the team, member and agent locks.
    let order = |events: Vec<String>| {
        let (mut held, mut holds) = (Vec::<String>::new(), [0; 3]);
        for e in events {
            let (sign, key) = e.split_at(1);
            match sign {
                "-" => held.retain(|k| k != key),
                "+" => {
                    if key == team {
                        holds[0] += 1;
                        assert!(
                            !held.contains(&member) && !held.contains(&agent),
                            "the team lock taken under a member's lock"
                        );
                    } else if key == member {
                        holds[1] += 1;
                        assert!(
                            !held.contains(&agent),
                            "the member lock taken under the agent lock"
                        );
                    } else if key == agent {
                        holds[2] += 1;
                        assert!(
                            held.contains(&member),
                            "the agent lock taken without the member lock"
                        );
                    }
                    held.push(key.into());
                }
                _ => {}
            }
        }
        holds
    };
    online(&w);
    online(&w);
    w.fakes.clear_log();
    let id = ping(&w, "ma1")["id"].as_str().unwrap().to_string();
    assert_eq!(
        order(w.fakes.locks()),
        [2, 2, 2],
        "the team lock for publish and for the notice's send; the member's locks for publish, then sent"
    );
    w.file("a.md", b"ok");
    online(&w);
    w.fakes.clear_log();
    answer_as(&w.ctx, &as_member(&w, A), &id, "a.md").unwrap();
    assert_eq!(
        order(w.fakes.locks()),
        [0, 1, 1],
        "answer: the member lock, then the agent lock"
    );
    w.fakes.hc_done().unwrap();
}

/// At the first hold of member `id`'s member or agent lock, runs `f` once: a change that lands after a check made
/// before that hold and before the write after it.
fn at_first_hold(w: &World, id: &str, f: impl Fn() + 'static) {
    let (agent, member) = (
        format!("+{}", crate::agents::lock_key("agent", &w.caller(), id)),
        format!("+{}", crate::agents::lock_key("member", &w.caller(), id)),
    );
    let done = std::rc::Rc::new(std::cell::Cell::new(false));
    w.fakes.lock_probe(move |event| {
        if !done.get() && (event == agent || event == member) {
            done.set(true);
            f();
        }
        String::new()
    });
}

/// The member in `folder` resumed into generation 1 (a new hc name), as resume and recovery rewrite its record.
fn next_generation(folder: &std::path::Path) {
    let mut r = file(folder, "record.json");
    (r["generation"], r["hcName"]) = (json!("1"), json!("m0123abcd"));
    crate::state::fs::write_json(&folder.join("record.json"), &r).unwrap();
}

// The generation `answer` checks and the files it writes belong to one hold of the member's locks. A member resumed
// into generation 1 after `answer` proved it at generation 0, but before it takes the locks, is a new generation:
// generation 0's ping is no ping of it, so the answer is refused and generation 1's status stays.
#[test]
fn an_answer_racing_a_new_generation_is_refused_and_writes_nothing() {
    let w = team_world();
    online(&w);
    online(&w);
    let id = ping(&w, "ma1")["id"].as_str().unwrap().to_string();
    w.file("a.md", b"generation 0's answer");
    let folder = w.folder(A);
    let (f, at) = (folder.clone(), utc_o(w.ctx.clock.now()));
    at_first_hold(&w, A, move || {
        next_generation(&f);
        crate::state::fs::write_json(
            &f.join("status.json"),
            &json!({"text": "generation 1 at work", "at": at}),
        )
        .unwrap();
    });
    online(&w);
    let got = answer_as(&w.ctx, &as_member(&w, A), &id, "a.md");
    w.fakes.hc_done().unwrap();
    assert_eq!(
        got.map_err(said).err().as_deref(),
        Some("2 ping no open ping with that id")
    );
    assert!(!folder.join("answer.json").exists(), "no answer written");
    assert_eq!(file(&folder, "status.json")["text"], "generation 1 at work");
}

// The generation `ping` records and the record it publishes belong to one hold of the member's locks: a member
// resumed into generation 1 after the snapshot named it, but before `ping` takes its locks, gets a ping of generation 1.
#[test]
fn a_ping_racing_a_new_generation_records_the_new_one() {
    let w = team_world();
    let f = w.folder(A);
    at_first_hold(&w, A, move || next_generation(&f));
    online(&w);
    let p = publish(&w.ctx, &w.caller(), "ma1").unwrap();
    w.fakes.hc_done().unwrap();
    let r = record(&w, A);
    assert_eq!(
        (
            r["id"].clone(),
            r["generation"].clone(),
            p.generation.clone()
        ),
        (json!(p.open.id), json!("1"), json!("1"))
    );
    assert_eq!(
        open(&w.folder(A), &json!("1")).map(|o| o.id),
        Some(p.open.id)
    );
}

// The reviewer's interleaving: ping publishes at generation 0 and pauses; the member is resumed into generation 1;
// the original notice and confirm then go on. The old id cannot be answered, generation 1 inherits no ping, the late
// confirm alters nothing, and the next ping creates and delivers a new id that generation 1 answers.
#[test]
fn a_ping_paused_across_a_resume_leaves_the_new_generation_unpinged() {
    let w = team_world();
    let c = w.caller();
    let folder = w.folder(A);
    online(&w);
    let old = publish(&w.ctx, &c, "ma1").unwrap();
    assert_eq!(old.generation, json!("0"));
    next_generation(&folder);
    online(&w);
    notify(&w.ctx, &c, &old).unwrap();
    let before = record(&w, A);
    confirm(&w.ctx, &c, &old).unwrap();
    assert_eq!(record(&w, A), before, "the late confirm alters nothing");
    assert_eq!(before["sent"], json!(false));
    assert_eq!(
        open(&folder, &json!("1")),
        None,
        "generation 1 inherits no ping"
    );
    w.file("a.md", b"ok");
    online(&w);
    let e = answer_as(&w.ctx, &as_member(&w, A), &old.open.id, "a.md").unwrap_err();
    assert_eq!(said(e), "2 ping no open ping with that id");
    online(&w);
    online(&w);
    let new = ping(&w, "ma1");
    let id = new["id"].as_str().unwrap().to_string();
    assert_ne!(id, old.open.id);
    assert_eq!(
        (
            new["notified"].clone(),
            record(&w, A)["generation"].clone(),
            record(&w, A)["sent"].clone()
        ),
        (json!(true), json!("1"), json!(true))
    );
    let mail = inbox(&w, A);
    assert_eq!(mail.len(), 2, "the old notice, then the new one");
    assert!(
        mail[1]["body"]
            .as_str()
            .unwrap()
            .contains(&format!("mux-agent answer {id} "))
    );
    online(&w);
    answer_as(&w.ctx, &as_member(&w, A), &id, "a.md").unwrap();
    assert_eq!(
        open(&folder, &json!("1")),
        None,
        "generation 1 answered its own ping"
    );
    w.fakes.hc_done().unwrap();
}
