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
