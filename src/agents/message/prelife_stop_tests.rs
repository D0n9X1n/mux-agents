//! prelife-1c G10c with p1c-calls 4, 5, 12, 24: the legacy stop reads the terminal immediately before `hc kill`.
use super::*;
use crate::agents::core_fixture as cf;
use crate::agents::fixture::World;
use crate::agents::prelife_fixture::{self as pf, Answer, C, L, P};
use serde_json::json;

/// C's terminal as the validating get answers it.
fn at_get(w: &World, c: &Value, state: &str, lost_argv: &[&str]) {
    let address = c["hcAddress"].as_str().unwrap();
    let argv = ["get", address, "--json"];
    let mut t = cf::wire(c);
    match state {
        "lost" => t["argv"] = json!(lost_argv),
        "ended" => (t["state"], t["reason"]) = (json!("finished"), json!("exited")),
        // a restarted core's terminal at the address is not C's
        "restarted" => t["labels"]["mux.gen"] = json!("1"),
        "other launcher" => {
            t["argv"] = json!(["/bin/mux-agent", "launcher", "/elsewhere/spec.json"])
        }
        _ => {}
    }
    match state {
        "missing" => w.fakes.hc_expect(
            &argv,
            Ok(crate::spawn::Out {
                code: 1,
                stderr: "no such terminal".into(),
                ..Default::default()
            }),
        ),
        "unreachable" => w.fakes.hc_expect(
            &argv,
            Err(crate::error::Refusal::internal("hc call failed")),
        ),
        _ => cf::get(w, address, t),
    }
}

fn notice_kind(w: &World, id: &str) -> Option<Value> {
    crate::state::fs::read_json(&w.folder(id).join("notice.json"))
        .ok()
        .flatten()
        .map(|n| n["kind"].clone())
}

/// A person's `stop C`: Running is killed and noticed; Lost and Ended are noticed and never killed; Unknown is
/// refused `state`, nothing killed, C byte-identical.
#[test]
fn a_legacy_stop_acts_only_on_the_validating_read() {
    for lost in crate::agents::life_fixture::LOST_ARGVS {
        for state in [
            "running",
            "lost",
            "ended",
            "missing",
            "restarted",
            "other launcher",
            "unreachable",
        ] {
            let w = World::human("");
            let (_, c) = pf::mixed(&w);
            cf::online(&w);
            at_get(&w, &c, state, lost);
            let before = pf::bytes(&w.folder(C));
            if state == "running" {
                let address = c["hcAddress"].as_str().unwrap();
                cf::answer(&w, &["kill", address, "--json"], "{}");
                let mut done = cf::wire(&c);
                (done["state"], done["reason"]) = (json!("finished"), json!("killed"));
                cf::get(&w, address, done);
            }
            let got = stop_hc(&w.ctx, &w.request(&["stop", "w-c"]));
            match state {
                "running" => {
                    assert_eq!(got.unwrap()["life"], "ended");
                    assert_eq!(
                        pf::verbs(&w),
                        ["--version", "get", "kill", "get"],
                        "P-R3: the get is the last call before the kill"
                    );
                }
                "lost" | "ended" => {
                    assert_eq!(got.unwrap()["life"], state, "{state}: main's reply");
                    assert_eq!(pf::verbs(&w), ["--version", "get"], "{state}: never killed");
                }
                _ => {
                    let e = got.unwrap_err();
                    assert_eq!((e.code, e.check), (2, "state"), "{state}: {}", e.message);
                    assert!(e.message.contains("not running"), "{state}");
                    assert!(!pf::killed(&w), "{state}: the shell lives");
                    assert_eq!(
                        pf::bytes(&w.folder(C)),
                        before,
                        "{state}: no notice, no write"
                    );
                }
            }
            if matches!(state, "running" | "lost" | "ended") {
                assert_eq!(notice_kind(&w, C), Some(json!("stopped")), "{state}");
            }
            w.fakes.hc_done().unwrap();
        }
    }
}

/// Spec "Legacy Lost child" with p1c-calls 4: agent A's `stop C` (A's standing; authority admits it). Unknown
/// (missing), then after the private core restarts (the address answers another terminal): refused `state`, no
/// kill, C byte-identical. Then Lost: no kill, the `stopped` notice, reply `life: lost`; the shell lives.
#[test]
fn an_ancestor_agent_never_kills_a_lost_or_unknown_legacy_child() {
    let w = World::human("");
    let (a, c) = pf::mixed(&w);
    let caller = w.caller();
    let standing = crate::agents::tree::Standing::Member(crate::agents::tree::Member {
        id: pf::A.into(),
        record: a,
        client: crate::parse::Client::Claude,
        pid: 150,
        start: 1500,
        flags: Vec::new(),
    });
    let team = crate::agents::membership::team(&caller);
    let id = crate::agents::member_target::StopTarget::Member(
        crate::agents::core_identity::Identity::read(&c, C, &team).unwrap(),
    );
    let lost = crate::agents::life_fixture::LOST_ARGVS[0];
    let before = pf::bytes(&w.folder(C));
    for state in ["missing", "restarted"] {
        cf::online(&w);
        at_get(&w, &c, state, lost);
        let e = stop_pre(&w.ctx, &caller, Some(&standing), &id).unwrap_err();
        assert_eq!((e.code, e.check), (2, "state"), "{state}: {}", e.message);
        assert_eq!(pf::bytes(&w.folder(C)), before, "{state}");
        w.fakes.hc_done().unwrap();
    }
    cf::online(&w);
    at_get(&w, &c, "lost", lost);
    let (reply, _) = stop_pre(&w.ctx, &caller, Some(&standing), &id).unwrap();
    assert_eq!(reply["life"], "lost");
    assert_eq!(notice_kind(&w, C), Some(json!("stopped")));
    assert!(!pf::killed(&w), "no stop ever killed the recovery shell");
    w.fakes.hc_done().unwrap();
}

/// p1c-calls 4, 12: a Lost legacy member holds its team slot (spawn refused `limit`), even with reap's `lost`
/// notice; a person's `stop legacy` kills nothing, upgrades the notice to `stopped` with a new message under
/// `stop-<id>` (never the Lost delivery `exit-<id>`); that notice frees the slot and the next spawn is admitted.
#[test]
fn stopping_a_lost_legacy_member_frees_its_slot_without_a_kill() {
    let w = World::human("");
    let mut answers = pf::capacity_world(&w);
    let p = pf::pre(&w, P, pf::fields(P, "legacy", Some(L), 1));
    let mut t = cf::wire(&p);
    t["argv"] = json!(crate::agents::life_fixture::LOST_ARGVS[0]);
    answers.insert(format!("t{P}"), Answer::Wire(t.clone()));
    pf::script(&w, answers.clone());
    assert_eq!(
        pf::admit_spawn(&w).unwrap_err().check,
        "limit",
        "the Lost member holds its slot"
    );
    w.fakes.hc_done().unwrap();
    registry::add_notice(&w.ctx, &w.caller(), P, &p, registry::Notice::Lost).unwrap();
    assert_eq!(notice_kind(&w, P), Some(json!("lost")));
    let (exit, stop) = (format!("exit-{P}"), format!("stop-{P}"));
    // P's parent pane is the record default `%1`
    assert_eq!(
        (
            pf::delivered(&w, "%1", &exit),
            pf::delivered(&w, "%1", &stop)
        ),
        (1, 0)
    );
    pf::script(&w, answers.clone());
    assert_eq!(
        pf::admit_spawn(&w).unwrap_err().check,
        "limit",
        "a lost notice frees nothing"
    );
    w.fakes.hc_done().unwrap();
    cf::online(&w);
    cf::get(&w, &format!("t{P}"), t);
    assert_eq!(
        stop_hc(&w.ctx, &w.request(&["stop", "legacy"])).unwrap()["life"],
        "lost"
    );
    assert_eq!(
        notice_kind(&w, P),
        Some(json!("stopped")),
        "call 12: upgraded from lost, never first-wins"
    );
    assert_eq!(
        (
            pf::delivered(&w, "%1", &exit),
            pf::delivered(&w, "%1", &stop)
        ),
        (1, 1),
        "call 12: one new `stopped` message under stop-<id>"
    );
    pf::script(&w, answers);
    pf::admit_spawn(&w).expect("the stopped notice freed the slot");
    assert!(!pf::killed(&w));
    w.fakes.hc_done().unwrap();
}

/// Call 24: the `lost` -> `stopped` upgrade appends its message durably before the marker. The mailbox flush fails:
/// the stop refuses and the marker stays `lost`; the retry writes it, with one `stopped` message.
#[test]
fn a_stopped_upgrade_is_never_marked_before_its_message_is_durable() {
    let w = World::human("");
    let p = pf::pre(&w, P, pf::fields(P, "legacy", Some(L), 1));
    pf::team(&w);
    registry::add_notice(&w.ctx, &w.caller(), P, &p, registry::Notice::Lost).unwrap();
    let mut t = cf::wire(&p);
    t["argv"] = json!(crate::agents::life_fixture::LOST_ARGVS[0]);
    let key = crate::agents::mail::mailbox_key("%1", "").unwrap();
    let mailbox = w.state().join("mail").join(format!("{key}.jsonl"));
    cf::online(&w);
    cf::get(&w, &format!("t{P}"), t.clone());
    crate::state::fs::fail_sync_file(Some(&mailbox));
    let got = stop_hc(&w.ctx, &w.request(&["stop", "legacy"]));
    crate::state::fs::fail_sync_file(None);
    assert!(
        got.is_err(),
        "the unflushed message refuses the stop: {got:?}"
    );
    assert_eq!(
        notice_kind(&w, P),
        Some(json!("lost")),
        "no stopped marker without a durable message"
    );
    w.fakes.hc_done().unwrap();
    cf::online(&w);
    cf::get(&w, &format!("t{P}"), t);
    assert_eq!(
        stop_hc(&w.ctx, &w.request(&["stop", "legacy"])).unwrap()["life"],
        "lost"
    );
    assert_eq!(notice_kind(&w, P), Some(json!("stopped")));
    assert_eq!(
        pf::delivered(&w, "%1", &format!("stop-{P}")),
        1,
        "one delivery"
    );
    assert!(!pf::killed(&w));
    w.fakes.hc_done().unwrap();
}

/// Call 12: a torn, wrong-type or unknown-kind existing notice refuses `evidence` before any hc call; nothing is
/// killed, written or freed.
#[test]
fn a_torn_legacy_notice_refuses_the_stop_before_hc() {
    for body in [
        &b"{\"kind\":\"lo"[..],
        b"[\"lost\"]",
        b"{\"kind\":3}",
        b"{\"kind\":\"paused\"}",
    ] {
        let w = World::human("");
        pf::mixed(&w);
        std::fs::write(w.folder(C).join("notice.json"), body).unwrap();
        let before = pf::bytes(&w.folder(C));
        let e = stop_hc(&w.ctx, &w.request(&["stop", "w-c"])).unwrap_err();
        let shown = String::from_utf8_lossy(body);
        assert_eq!(e.check, "evidence", "{shown}: {e:?}");
        assert!(pf::verbs(&w).is_empty(), "{shown}: no hc call");
        assert_eq!(pf::bytes(&w.folder(C)), before, "{shown}");
    }
}
