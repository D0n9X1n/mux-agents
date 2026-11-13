use crate::{
    agents::{
        core_fixture as cf,
        fixture::{ID, World},
    },
    caller,
    ctx::Env,
};
use serde_json::{Value, json};
fn worker() -> (World, caller::Caller, Value, std::path::PathBuf) {
    let mut w = World::agent("");
    let c = w.caller();
    let r = cf::member(
        &w,
        ID,
        json!({"startCommand":crate::agents::fixture::launch_command("%9")}),
    );
    let folder = w.folder(ID);
    crate::state::fs::write_json(
        &w.state().join("server.json"),
        &json!({"socket":c.socket,"serverPid":c.server_pid,"serverStart":c.server_start}),
    )
    .unwrap();
    w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker", "%1|@1|100||"]);
    w.recipients_live();
    w.fakes.forget(150);
    w.fakes.process(crate::testing::row(150, 31337, 1500));
    w.fakes.process(crate::testing::row(31337, 1, 1000));
    w.ctx.env = Env::from_pairs([("HC_TERMINAL_ID", r["hcAddress"].as_str().unwrap())]);
    w.fakes.clear_log();
    (w, c, r, folder)
}
fn wire(r: &Value) -> Value {
    let mut t = cf::wire(r);
    t["pid"] = 31337.into();
    t
}
fn proof(w: &World, r: &Value) {
    cf::online(w);
    cf::get(w, r["hcAddress"].as_str().unwrap(), wire(r));
}
#[test]
fn hc_worker_routes_without_mux_ancestry() {
    let (w, c, r, _) = worker();
    proof(&w, &r);
    let got = caller::resolve(&w.ctx).expect("managed hc worker must resolve without TMUX");
    assert_eq!(
        (
            got.kind,
            got.pane.as_str(),
            got.window.as_str(),
            got.agent_tag.as_str()
        ),
        (caller::Kind::Agent, "%9", "@5", ID)
    );
    assert_eq!(
        (
            got.socket,
            got.server_pid,
            got.server_start,
            got.server_key,
            got.session
        ),
        (
            c.socket,
            c.server_pid,
            c.server_start,
            c.server_key,
            c.session
        )
    );
    assert_eq!(
        got.chain.iter().map(|r| r.pid).collect::<Vec<_>>(),
        [4242, 170, 150, 31337]
    );
    assert!(got.agent.is_none());
    assert!(w.fakes.mux_calls().is_empty());
    w.fakes.hc_done().unwrap();
}
#[test]
fn hc_worker_rejects_unproven_identity_before_mux() {
    for case in [
        "unrelated",
        "missing-pid",
        "reused-pid",
        "broken-chain",
        "managed",
        "team",
        "id",
        "gen",
        "address",
        "argv",
        "finished",
        "missing-record",
        "uncommitted",
        "aborted",
        "record-team",
        "record-session",
        "record-generation",
        "record-id",
        "record-address",
        "record-client",
        "record-pane",
        "record-window",
        "unsafe-team",
        "unsafe-id",
        "server-missing",
        "server-start",
        "server-key",
        "server-socket",
        "get-error",
        "get-json",
        "version",
    ] {
        let (w, _, r, folder) = worker();
        let mut t = wire(&r);
        let mut record = r.clone();
        match case {
            "unrelated" => {
                t["pid"] = 31338.into();
                w.fakes.process(crate::testing::row(31338, 1, 900));
            }
            "missing-pid" => {
                t.as_object_mut().unwrap().remove("pid");
            }
            "reused-pid" => {
                w.fakes.forget(31337);
                w.fakes.process(crate::testing::row(31337, 1, 1600));
            }
            "broken-chain" => w.fakes.forget(170),
            "managed" => t["labels"]["mux.managed"] = "0".into(),
            "team" => t["labels"]["mux.team"] = "0000000000000000:$9".into(),
            "id" => t["labels"]["mux.id"] = "bbbbbbbbbbbb".into(),
            "gen" => t["labels"]["mux.gen"] = "1".into(),
            "address" => t["address"] = "t-other".into(),
            "argv" => t["argv"] = json!(["/bin/sh", "-l"]),
            "finished" => t["state"] = "finished".into(),
            "uncommitted" => std::fs::remove_file(folder.join("commit.json")).unwrap(),
            "aborted" => {
                crate::state::fs::write_json(&folder.join("commit.json"), &json!({"aborted":true}))
                    .unwrap()
            }
            "record-team" => record["team"] = "0000000000000000:$3".into(),
            "record-session" => record["session"] = "$4".into(),
            "record-generation" => record["generation"] = "1".into(),
            "record-id" => record["id"] = "bbbbbbbbbbbb".into(),
            "record-address" => record["hcAddress"] = "t-other".into(),
            "record-client" => record["client"] = "unknown".into(),
            "record-pane" => record["pane"] = "%bad".into(),
            "record-window" => record["window"] = "@bad".into(),
            "unsafe-team" => t["labels"]["mux.team"] = "../../outside:$3".into(),
            "unsafe-id" => t["labels"]["mux.id"] = "../outside".into(),
            "server-missing" => std::fs::remove_file(
                folder
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("server.json"),
            )
            .unwrap(),
            "server-start" => w.fakes.opens(900, &[Some(2)]),
            "server-key" | "server-socket" => {
                let p = folder
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("server.json");
                let mut v = crate::state::fs::read_json(&p).unwrap().unwrap();
                if case == "server-key" {
                    v["serverStart"] = 2.into();
                    w.fakes.opens(900, &[Some(2)]);
                } else {
                    v["socket"] = "relative".into();
                }
                crate::state::fs::write_json(&p, &v).unwrap();
            }
            "missing-record" | "get-error" | "get-json" | "version" => (),
            _ => unreachable!(),
        }
        crate::state::fs::write_json(&folder.join("record.json"), &record).unwrap();
        if case == "missing-record" {
            std::fs::remove_file(folder.join("record.json")).unwrap();
        }
        let a = r["hcAddress"].as_str().unwrap();
        if case == "version" {
            w.fakes.hc_install("hc");
            cf::answer(&w, &["--version"], "hc 0.1.20");
        } else {
            cf::online(&w);
            match case {
                "get-error" => w.fakes.hc_expect(
                    &["get", a, "--json"],
                    Err(crate::error::Refusal::internal("offline")),
                ),
                "get-json" => cf::answer(&w, &["get", a, "--json"], "{}"),
                _ => cf::get(&w, a, t),
            }
        }
        let e = caller::resolve(&w.ctx).unwrap_err();
        assert_eq!((e.code, e.check), (2, "herdr"), "{case}: {e:?}");
        assert!(w.fakes.mux_calls().is_empty(), "{case}");
        w.fakes.hc_done().unwrap();
        for f in ["status.json", "activity.json", "last-sent.json"] {
            assert!(!folder.join(f).exists(), "{case}: {f}");
        }
    }
}
