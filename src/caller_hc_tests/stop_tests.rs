//! A3c checklist 10 (a3c call 42): a proven, viewless lifecycle (hc) member stops a child it spawned, through `stop`'s
//! handler (`stop_hc` dispatch), outside a team and inside one. An hc caller's authority is its proven member
//! identity matched against the target's recorded parent member, never a pane; an empty pane matches nothing.
use super::k2_tests::kill_server;
use super::*;
use crate::agents::{life_fixture as lf, tree::tests as tt};

const KID: &str = "bbbbbbbbbbbb";

/// The lifecycle worker `ID` as a viewless caller: its view gone and its multiplexer server killed.
fn viewless() -> (World, Value, std::path::PathBuf) {
    let (w, r, folder) = lifecycle_worker(ok(), ok());
    std::fs::remove_file(folder.join("view.json")).unwrap();
    kill_server(&w, &folder);
    (w, r, folder)
}

/// Lifecycle member `KID` ("kid") at `tkid`, generation 0 committed, its record over `fields`. Its folder comes from
/// the worker's, so no caller is resolved here (that would spend a queued proof).
fn kid(worker: &std::path::Path, r: &Value, fields: Value) -> std::path::PathBuf {
    let folder = worker.parent().unwrap().join(KID);
    std::fs::create_dir_all(&folder).unwrap();
    let mut k = json!({"lifecycle": 1, "id": KID, "name": "kid", "client": "claude", "session": r["session"],
        "sessionId": "2e22fc2f-2032-48ef-a920-4af2a8e5af52", "team": r["team"], "generation": "0",
        "hcAddress": "tkid", "hcName": lf::hc_name(0), "launcherArgv": lf::ARGV});
    for (key, v) in fields.as_object().unwrap() {
        k[key] = v.clone();
    }
    crate::state::fs::write_json(&folder.join("record.json"), &k).unwrap();
    lf::seed_gen(&folder, 0, Some("run"), lf::committed());
    folder
}

/// What a spawn by member `parent` records as the child's parent (`tree::admit`'s shape): its client and its id.
fn parent_agent(parent: &str) -> Value {
    json!({"client": "claude", "pid": 150, "start": 1500, "sessionId": SESSION, "id": parent})
}
const SESSION: &str = "0f11fc2f-2032-48ef-a920-4af2a8e5af50";

/// `terminate` of the kid's running generation 0: the search, the exact reread, then the kill.
fn kills_kid(w: &World, r: &Value) {
    let wire = json!({"address": "tkid", "name": lf::hc_name(0), "state": "live", "reason": null,
        "argv": lf::ARGV, "exit_code": null, "labels": {"mux.managed": "1", "mux.team": r["team"],
        "mux.id": KID, "mux.gen": "0", "mux.name": "kid"}});
    cf::online(w);
    let id = format!("mux.id={KID}");
    cf::answer(
        w,
        &[
            "ls",
            "--all",
            "--label",
            &id,
            "--label",
            "mux.gen=0",
            "--json",
        ],
        &json!({"terminals": [{"address": "tkid"}]}).to_string(),
    );
    cf::get(w, "tkid", wire.clone());
    cf::get(w, "tkid", wire);
    cf::answer(w, &["kill", "tkid", "--json"], "{}");
}

/// The hc answers a CLI verb's caller resolution takes (two proofs, as in k2's handler test).
fn resolves(w: &World, r: &Value) {
    proof(w, r);
    proof(w, r);
}

/// `stop kid` through the CLI, as the member's client runs it.
fn stop_kid(w: &World) -> (u8, Value) {
    let (code, out) = crate::cli::run(&["stop".to_string(), "kid".to_string()], || Ok(&w.ctx));
    (code, serde_json::from_str(&out).unwrap_or(Value::Null))
}

// Checklist 10, outside a team: the kid a team placed under this member (its parentAgent names the member), the team
// record since gone. The member's views are down (presentation None, pane ""): it still stops its own child.
#[test]
fn a_viewless_member_stops_its_own_child_outside_a_team() {
    let (w, r, folder) = viewless();
    let k = kid(
        &folder,
        &r,
        json!({"parentPane": "%9", "parentAgent": parent_agent(ID), "depth": 1}),
    );
    resolves(&w, &r);
    kills_kid(&w, &r);
    let (code, got) = stop_kid(&w);
    assert_eq!(
        (code, &got["life"]),
        (0, &json!("ended")),
        "{got} {:?}",
        lf::calls(&w)
    );
    assert!(
        crate::agents::gens::read(&k, 0, "ended")
            .known()
            .unwrap()
            .is_some()
    );
    w.fakes.hc_done().unwrap();
}

// Checklist 10, call 42: an empty pane matches nothing. A viewless member is not the parent of a kid whose recorded
// parent is another member, even when that kid's parentPane is empty too; nothing is killed.
#[test]
fn a_viewless_member_never_stops_another_members_child() {
    for parent_pane in ["", "%9"] {
        let (w, r, folder) = viewless();
        kid(
            &folder,
            &r,
            json!({"parentPane": parent_pane, "parentAgent": parent_agent("cccccccccccc"), "depth": 1}),
        );
        resolves(&w, &r);
        let (code, got) = stop_kid(&w);
        assert_eq!(
            (code, &got["check"]),
            (2, &json!("parent")),
            "parentPane {parent_pane:?}: {got}"
        );
        assert_eq!(lf::count(&w, "kill"), 0, "parentPane {parent_pane:?}");
    }
}

// Checklist 10, inside a team: the leader's member spawned the kid down the tree; viewless, it stops it by ancestry.
#[test]
fn a_viewless_member_stops_its_own_child_inside_a_team() {
    let (w, mut r, folder) = viewless();
    (r["depth"], r["parentAgent"]) = (json!(1), json!({"id": tt::LEADER}));
    crate::state::fs::write_json(&folder.join("record.json"), &r).unwrap();
    proof(&w, &r);
    tt::team_record(&w, &[]);
    let leader = folder.parent().unwrap().join(tt::LEADER);
    std::fs::create_dir_all(&leader).unwrap();
    let mut l = tt::leader_record();
    (l["lifecycle"], l["generation"], l["team"]) = (json!(1), json!("0"), r["team"].clone());
    (l["hcAddress"], l["hcName"], l["launcherArgv"]) =
        (json!("tlead"), json!(lf::hc_name(0)), json!(lf::ARGV));
    (l["session"], l["sessionId"]) = (r["session"].clone(), json!(lf::SID));
    crate::state::fs::write_json(&leader.join("record.json"), &l).unwrap();
    lf::seed_gen(&leader, 0, Some("run"), lf::committed());
    let k = kid(
        &folder,
        &r,
        json!({"parentPane": "%9", "parentAgent": parent_agent(ID), "depth": 2}),
    );
    resolves(&w, &r);
    proof(&w, &r); // the standing re-proof under the team lock
    kills_kid(&w, &r);
    let (code, got) = stop_kid(&w);
    assert_eq!(
        (code, &got["life"]),
        (0, &json!("ended")),
        "{got} {:?}",
        lf::calls(&w)
    );
    assert!(
        crate::agents::gens::read(&k, 0, "ended")
            .known()
            .unwrap()
            .is_some()
    );
    w.fakes.hc_done().unwrap();
}
