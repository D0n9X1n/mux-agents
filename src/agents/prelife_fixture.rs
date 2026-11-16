//! prelife-1c fixtures: a team of lifecycle members, and pre-lifecycle folders beside it.
use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::agents::core_fixture as cf;
use crate::agents::fixture::World;
use crate::spawn::Out;

pub const L: &str = "111111111111";
pub const P: &str = "d0000000000d";

/// The team record of session $3, leader `L`.
pub fn team(w: &World) {
    use crate::agents::team as t;
    use crate::parse::Client;
    let made = t::Team {
        team: "t".into(),
        session: "$3".into(),
        name: "s".into(),
        directory: "/work".into(),
        client: Client::Claude,
        policy: t::policy(Client::Claude, &[]),
        flags: Vec::new(),
        leader: L.into(),
        created_at: "x".into(),
    };
    assert!(t::create(&t::path(&w.state(), "$3"), &made).unwrap());
}

/// A record's own fields over `cf::member`'s hc defaults; `parent` None is the leader (depth 0, `upTeam`).
pub fn fields(id: &str, name: &str, parent: Option<&str>, depth: u64) -> Value {
    let mut r = json!({"id": id, "name": name, "client": "claude", "session": "$3", "depth": depth,
        "directory": "/work", "pane": "%7", "sessionId": format!("{}-0000-4000-8000-{id}", &id[..8])});
    match parent {
        None => (r["upTeam"], r["parentAgent"], r["pane"]) = (json!("t"), Value::Null, json!("%4")),
        Some(p) => r["parentAgent"] = json!({"id": p}),
    }
    r
}

/// Generation `n`'s create-once `file` of member `id`.
pub fn publish(w: &World, id: &str, n: u64, file: &str, body: Value) {
    let path = crate::agents::gens::path(&w.folder(id), n, file);
    crate::state::evidence::publish(&path, &body).unwrap();
}

/// A lifecycle member at generation 1, committed (state-0 S2; `hcName` is `m` and the first 8 of the id), with
/// its generation's view so `subject_of` presents it.
pub fn life(w: &World, id: &str, mut f: Value) -> Value {
    let hc_name = format!("m{}", &id[..8]);
    (f["lifecycle"], f["generation"], f["hcName"]) = (json!(1), json!("1"), json!(hc_name));
    let r = cf::member(w, id, f);
    publish(w, id, 1, "commit", json!({"committed": true}));
    publish(w, id, 1, "launch", json!({"hcName": hc_name}));
    crate::agents::fixture::write(
        &w.folder(id).join("view.json"),
        json!({"pane": r["pane"], "window": "@5", "startCommand": r["startCommand"], "gen": 1,
            "hcAddress": r["hcAddress"], "hcName": hc_name}),
    );
    r
}

/// A pre-lifecycle hc member as main writes one: generation 0, `hcAddress`, `launcherArgv`, folder `commit.json`.
pub fn pre(w: &World, id: &str, f: Value) -> Value {
    cf::member(w, id, f)
}

/// `pre` with `launcherArgv` removed: hc fields but no usable identity (held, never looked up).
pub fn partial(w: &World, id: &str, f: Value) -> Value {
    let r = pre(w, id, f);
    let mut p = r.clone();
    p.as_object_mut().unwrap().remove("launcherArgv");
    crate::agents::fixture::write(&w.folder(id).join("record.json"), p);
    r
}

/// How hc answers `get <address>` in a scripted snapshot.
#[derive(Clone)]
pub enum Answer {
    Wire(Value),
    /// exit 1, "no such terminal": the terminal is gone from hc.
    Missing,
    /// the call fails (ambiguity 3: the core answered the listing, then this get failed).
    Fail,
}

/// The snapshot `for_spawn` reads: connect, the team listing (`listed` addresses), then each address in sorted
/// order (core_facts reads every listed and known address once).
pub fn script_listed(w: &World, listed: &[&str], answers: BTreeMap<String, Answer>) {
    cf::online(w);
    cf::listing(w, &crate::agents::membership::team(&w.caller()), listed);
    for (address, a) in answers {
        let argv = ["get", address.as_str(), "--json"];
        match a {
            Answer::Wire(v) => cf::get(w, &address, v),
            Answer::Missing => w.fakes.hc_expect(
                &argv,
                Ok(Out {
                    code: 1,
                    stderr: "no such terminal".into(),
                    ..Out::default()
                }),
            ),
            Answer::Fail => w.fakes.hc_expect(
                &argv,
                Err(crate::error::Refusal::internal("hc call failed")),
            ),
        }
    }
}

/// `script_listed` with an empty team listing.
pub fn script(w: &World, answers: BTreeMap<String, Answer>) {
    script_listed(w, &[], answers);
}

/// The team (leader `L`) and `children` Running lifecycle children `w1`..; their wires by address.
pub fn capacity_world_of(w: &World, children: u64) -> BTreeMap<String, Answer> {
    team(w);
    let mut out = BTreeMap::new();
    let leader = life(w, L, fields(L, "leader", None, 0));
    out.insert(format!("t{L}"), Answer::Wire(cf::wire(&leader)));
    for n in 1..=children {
        let id = format!("a{n}000000000{n}");
        let r = life(w, &id, fields(&id, &format!("w{n}"), Some(L), 1));
        out.insert(format!("t{id}"), Answer::Wire(cf::wire(&r)));
    }
    out
}

/// Five children: the team is one slot from `limit`.
pub fn capacity_world(w: &World) -> BTreeMap<String, Answer> {
    capacity_world_of(w, 5)
}

/// Spawn's team admission for a person, in spawn's order (spawn.rs:209-235): connect, team, admit, snapshot, place.
pub fn admit_spawn(w: &World) -> crate::error::Res<crate::agents::tree::Placed> {
    use crate::agents::tree;
    use crate::parse::Client;
    let caller = w.caller();
    let core = crate::hc::connect(&w.ctx)?;
    let found = tree::team_of(&w.ctx, &caller)?.expect("a team");
    let a = tree::admit(&w.ctx, &caller, &found, tree::Standing::Person)?;
    let s = crate::agents::membership::Snapshot::for_spawn(&w.ctx, &caller, &core)?;
    tree::place(&w.ctx, &caller, &a, &s, Client::Claude, None, || {
        s.name_only(None, Client::Claude, "add subtract")
    })
}
