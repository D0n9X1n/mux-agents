use super::fixture::World;
use crate::spawn::Out;
use serde_json::{Value, json};

pub fn answer(w: &World, argv: &[&str], stdout: &str) {
    w.fakes.hc_expect(
        argv,
        Ok(Out {
            stdout: stdout.into(),
            ..Out::default()
        }),
    );
}

pub fn online(w: &World) {
    w.fakes.hc_install("hc");
    answer(w, &["--version"], "hc 0.1.21");
}

pub fn listing(w: &World, team: &str, addresses: &[&str]) {
    answer(
        w,
        &[
            "ls",
            "--all",
            "--label",
            "mux.managed=1",
            "--label",
            &format!("mux.team={team}"),
            "--json",
        ],
        &json!({"terminals": addresses.iter().map(|a| json!({"address":a})).collect::<Vec<_>>() })
            .to_string(),
    );
}

pub fn get(w: &World, address: &str, wire: Value) {
    answer(w, &["get", address, "--json"], &wire.to_string());
}

pub fn member(w: &World, id: &str, fields: Value) -> Value {
    let caller = w.caller();
    let spec = w
        .ctx
        .state_root
        .join(&caller.server_key)
        .join("agents")
        .join(id)
        .join("spec.json");
    let mut defaults = json!({"team":format!("{}:{}", caller.server_key, caller.session),"generation":"0",
        "hcAddress":format!("t{id}"),"launcherArgv":crate::launcher_scope::argv(&w.ctx, &spec).unwrap(),
        "startCommand":format!("hc attach t{id}")});
    for (k, v) in fields.as_object().unwrap() {
        defaults[k] = v.clone();
    }
    let folder = w.agent_record(id, defaults);
    crate::state::fs::read_json(&folder.join("record.json"))
        .unwrap()
        .unwrap()
}

/// Member `id`'s D1 evidence: launcher.json (L `l` started 1000, C `c` started 1500) and its recorded flags.
pub fn proven(w: &World, id: &str, l: u32, c: u32) {
    let f = w.folder(id);
    crate::state::fs::write_json(
        &f.join("launcher.json"),
        &json!({"pid": l, "start": 1000, "client": {"pid": c, "start": 1500}}),
    )
    .unwrap();
    crate::state::fs::write_json(
        &f.join("spec.json"),
        &json!({"flags": ["--effort", "high"]}),
    )
    .unwrap();
}

pub fn wire(r: &Value) -> Value {
    json!({"address":r["hcAddress"],"state":"live","argv":r["launcherArgv"],"labels":{
        "mux.managed":"1","mux.team":r["team"],"mux.id":r["id"],"mux.gen":r["generation"],"mux.name":r["name"]}})
}

#[test]
fn core_fixture_is_opt_in_and_scoped() {
    use super::{core_identity::Identity, fixture::ID};
    let mut w = World::human("");
    let mut env: Vec<_> = w
        .ctx
        .env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    env.extend([
        ("PATH".into(), "caller-path".into()),
        ("HC_SOCKET".into(), "do-not-copy".into()),
    ]);
    w.ctx.env = crate::ctx::Env::from_pairs(env);
    let r = member(&w, ID, json!({}));
    let caller = w.caller();
    let team = format!("{}:$3", caller.server_key);
    let id = Identity::read(&r, ID, &team).unwrap();
    assert_eq!(id.generation, "0");
    assert_eq!(id.address, "taaaaaaaaaaaa");
    let spec = w
        .ctx
        .state_root
        .join(&caller.server_key)
        .join("agents")
        .join(ID)
        .join("spec.json");
    assert_eq!(
        id.launcher,
        vec![
            w.ctx.exe.to_str().unwrap(),
            "launcher",
            spec.to_str().unwrap(),
            "--state-root",
            w.ctx.state_root.to_str().unwrap(),
            "--path",
            "caller-path"
        ]
    );
    assert_eq!(r["startCommand"], "hc attach taaaaaaaaaaaa");
    assert!(w.fakes.hc_calls().is_empty());
    online(&w);
    listing(&w, &team, &["taaaaaaaaaaaa"]);
    get(&w, "taaaaaaaaaaaa", wire(&r));
    let core = crate::hc::connect(&w.ctx).unwrap();
    let facts = super::core_facts::take(&core, &team, &[id]).unwrap();
    assert_eq!(facts.count(), 1);
    assert_eq!(facts.names(), ["worker"]);
    w.fakes.hc_done().unwrap();
    let legacy = w.agent_record("bbbbbbbbbbbb", json!({}));
    let legacy = crate::state::fs::read_json(&legacy.join("record.json"))
        .unwrap()
        .unwrap();
    assert!(Identity::read(&legacy, "bbbbbbbbbbbb", &team).is_none());
}
