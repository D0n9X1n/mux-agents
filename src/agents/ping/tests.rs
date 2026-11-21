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
