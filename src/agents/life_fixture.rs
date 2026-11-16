//! Group B fixtures: lifecycle member `ID` of the caller's session, its generations, and hc terminals of them.
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::core_fixture as cf;
use super::fixture::{ID, SESSION_ID, World};
use super::gens;
use super::team_state::Scope;
use crate::spawn::Out;
use crate::state::evidence::publish;

pub const ARGV: [&str; 3] = ["/bin/mux-agent", "launcher", "/s/spec.json"];
/// The generation's own session id (`pending.sessionId`), distinct from the record's `SESSION_ID`.
pub const SID: &str = "1e22fc2f-2032-48ef-a920-4af2a8e5af51";

/// launch-1 G2: `m` and 8 lowercase hex digits (`core_identity::is_hc_name`).
pub fn hc_name(n: u64) -> String {
    format!("m{n:08x}")
}
pub fn world() -> (World, Scope) {
    let w = World::human("");
    w.state();
    let s = Scope::of(&w.caller());
    (w, s)
}
/// The caller's team, `<server key>:<session>`, as hc's `mux.team` and `mux.kept` name it.
pub fn team(w: &World) -> String {
    let c = w.caller();
    format!("{}:{}", c.server_key, c.session)
}
/// A lifecycle record of the caller's session with `fields` over it, seeded directly.
pub fn life(w: &World, fields: Value) -> PathBuf {
    let folder = w.folder(ID);
    std::fs::create_dir_all(&folder).unwrap();
    let mut r = json!({"lifecycle": 1, "id": ID, "name": "worker", "session": w.caller().session,
        "sessionId": SESSION_ID, "team": team(w)});
    for (k, v) in fields.as_object().unwrap() {
        r[k] = v.clone();
    }
    crate::state::fs::write_json(&folder.join("record.json"), &r).unwrap();
    folder
}
/// The record through the walk.
pub fn record(folder: &Path) -> Value {
    gens::record(folder).known().unwrap().unwrap()
}
/// Generation n: `launch.json` (launch-1 G12), and its claim and commit when given.
pub fn seed_gen(folder: &Path, n: u64, claim: Option<&str>, commit: Option<Value>) {
    let launch = json!({"hcName": hc_name(n), "launcherArgv": ARGV, "sessionId": SID});
    publish(&gens::path(folder, n, "launch"), &launch).unwrap();
    if let Some(c) = claim {
        publish(&gens::claim_path(folder, n), &json!(c)).unwrap();
    }
    if let Some(c) = commit {
        publish(&gens::path(folder, n, "commit"), &c).unwrap();
    }
}
pub fn committed() -> Option<Value> {
    Some(json!({"committed": true}))
}
/// hc's `get` reply for generation n's terminal `t<n>`.
pub fn wire(w: &World, n: u64, state: &str, reason: Option<&str>, argv: &[&str]) -> Value {
    json!({"address": format!("t{n}"), "name": hc_name(n), "state": state, "reason": reason, "argv": argv,
        "exit_code": if state == "finished" { json!(0) } else { Value::Null },
        "labels": {"mux.managed": "1", "mux.team": team(w), "mux.id": ID, "mux.gen": n.to_string(),
            "mux.name": "worker"}})
}
pub fn running(w: &World, n: u64) -> Value {
    wire(w, n, "live", None, &ARGV)
}
pub fn ended(w: &World, n: u64) -> Value {
    wire(w, n, "finished", Some("exited"), &ARGV)
}
/// `gens::find`'s list call.
pub fn ls(w: &World, n: u64, addresses: &[&str]) {
    let terminals: Vec<Value> = addresses.iter().map(|a| json!({"address": a})).collect();
    let (id, g) = (format!("mux.id={ID}"), format!("mux.gen={n}"));
    cf::answer(
        w,
        &["ls", "--all", "--label", &id, "--label", &g, "--json"],
        &json!({"terminals": terminals}).to_string(),
    );
}
pub fn get(w: &World, address: &str, wire: &Value) {
    cf::answer(w, &["get", address, "--json"], &wire.to_string());
}
/// `ls`, then `get` of generation n's one terminal.
pub fn found(w: &World, n: u64, wire: &Value) {
    ls(w, n, &[&format!("t{n}")]);
    get(w, &format!("t{n}"), wire);
}
/// hc answers `argv` with a failure that decides nothing.
pub fn fails(w: &World, argv: &[&str]) {
    w.fakes.hc_expect(
        argv,
        Ok(Out {
            code: 1,
            stdout: String::new(),
            stderr: "x".into(),
        }),
    );
}
/// A terminal decoded from a wire, through a connection of its own.
pub fn terminal(w: &World, wire: &Value) -> crate::hc::Terminal {
    cf::online(w);
    get(w, wire["address"].as_str().unwrap(), wire);
    let core = crate::hc::connect(&w.ctx).unwrap();
    core.get(wire["address"].as_str().unwrap()).unwrap()
}
