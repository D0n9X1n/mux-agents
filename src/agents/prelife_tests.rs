//! prelife-1c Task 8: a state folder written by v0.2.8 keeps working after the upgrade (G10, ambiguities 2, 4).
//! Task 9: the `limit` refusal names each held pre-lifecycle member and what frees it (p1c-calls 4, 10).
use serde_json::json;

use crate::agents::core_fixture as cf;
use crate::agents::fixture::World;
use crate::agents::prelife_fixture::{self as pf, Answer, C, D, L, P};

/// Outside a team nothing changes for 0.2.x folders: scope Outside; the 0.2 cap still counts hc members only; a
/// stop or resume of one never kills, launches or writes.
#[test]
fn a_v028_state_folder_changes_nothing_outside_a_team() {
    let w = World::human("");
    let ids: Vec<String> = (1..=6).map(|n| format!("b{n}000000000{n}")).collect();
    for (n, id) in ids.iter().enumerate() {
        pf::v028(&w, id, &format!("old{n}"), pf::End::Live);
    }
    pf::v028(&w, "b7000000000e", "done", pf::End::Exited);
    let before = pf::bytes(&w.state().join("agents"));
    let caller = w.caller();
    assert!(matches!(
        crate::agents::tree::scope(&w.ctx, &caller).unwrap(),
        crate::agents::tree::Scope::Outside
    ));
    pf::script(&w, Default::default());
    let core = crate::hc::connect(&w.ctx).unwrap();
    let s = crate::agents::membership::Snapshot::for_spawn(&w.ctx, &caller, &core).unwrap();
    assert_eq!(
        s.held, ids,
        "the six live ones hold team slots (none is in a team)"
    );
    s.admit_name(None, crate::parse::Client::Claude, "add subtract")
        .expect("the 0.2 cap counts hc members only");
    w.fakes.hc_done().unwrap();
    let (code, out) = crate::cli::run(&["stop".to_string(), "old0".to_string()], || Ok(&w.ctx));
    assert_eq!(code, 2, "{out}");
    w.panes(&[]);
    let (code, out) = crate::cli::run(&["resume".to_string(), "old0".to_string()], || Ok(&w.ctx));
    assert_eq!(
        (
            code,
            out.contains(r#""check":"state""#),
            out.contains("pre-lifecycle")
        ),
        (2, true, true),
        "{out}"
    );
    assert!(!pf::killed(&w) && !pf::verbs(&w).iter().any(|v| v == "run"));
    assert_eq!(pf::bytes(&w.state().join("agents")), before);
    w.fakes.hc_done().unwrap();
}

/// In a team (a person's `up` after the upgrade), a live v0.2.8 member holds its slot until its notice proves an
/// end; `lost` proves nothing.
#[test]
fn a_v028_member_in_a_team_holds_its_slot_until_its_notice_proves_an_end() {
    for (end, held) in [
        (pf::End::Live, true),
        (pf::End::Lost, true),
        (pf::End::Exited, false),
        (pf::End::Stopped, false),
    ] {
        let w = World::human("");
        let answers = pf::capacity_world(&w);
        pf::v028(&w, P, "legacy", end);
        pf::script(&w, answers);
        let got = pf::admit_spawn(&w);
        assert_eq!(
            got.as_ref().map_err(|e| e.check).err(),
            held.then_some("limit"),
            "{end:?}"
        );
        w.fakes.hc_done().unwrap();
    }
}

/// p1c-calls 4 (Task 9): the `limit` refusal names each held pre-lifecycle member and what frees it; with none
/// held it is main's message.
#[test]
fn the_limit_refusal_names_each_held_pre_lifecycle_member() {
    let base = "the team already has 6 members besides the leader";
    for (case, hint) in [
        (
            "lost",
            Some("run mux-agent stop legacy once its client is gone"),
        ),
        ("running", Some("running; mux-agent stop legacy stops it")),
        (
            "missing",
            Some("hc cannot show its terminal; it stays held"),
        ),
        (
            "partial",
            Some("its record names no usable hc terminal; it stays held"),
        ),
        ("none", None),
    ] {
        let w = World::human("");
        let mut answers = pf::capacity_world(&w);
        if case == "none" {
            let id = "a60000000006".to_string();
            let r = pf::life(&w, &id, pf::fields(&id, "w6", Some(L), 1));
            answers.insert(format!("t{id}"), Answer::Wire(cf::wire(&r)));
        } else {
            let p = pf::pre(&w, P, pf::fields(P, "legacy", Some(L), 1));
            let mut t = cf::wire(&p);
            match case {
                "lost" => {
                    t["argv"] = json!(crate::agents::life_fixture::LOST_ARGVS[0]);
                    answers.insert(format!("t{P}"), Answer::Wire(t));
                }
                "running" => {
                    answers.insert(format!("t{P}"), Answer::Wire(t));
                }
                "missing" => {
                    answers.insert(format!("t{P}"), Answer::Missing);
                }
                _ => {
                    let mut r = p.clone();
                    r.as_object_mut().unwrap().remove("launcherArgv");
                    // held, never looked up
                    crate::agents::fixture::write(&w.folder(P).join("record.json"), r);
                }
            }
        }
        pf::script(&w, answers);
        let e = pf::admit_spawn(&w).unwrap_err();
        let want = match hint {
            Some(h) => format!("{base}; held by pre-lifecycle members: legacy ({h})"),
            None => base.to_string(),
        };
        assert_eq!(
            (e.check, e.message.as_str()),
            ("limit", want.as_str()),
            "{case}"
        );
        w.fakes.hc_done().unwrap();
    }
}

/// Call 10: several readable holders and one unidentified holder are all named, sorted by name (their ids sort the
/// other way, so neither a missing sort nor a truncation survives).
#[test]
fn the_limit_refusal_names_every_holder_sorted_by_name() {
    let w = World::human("");
    let mut answers = pf::capacity_world(&w);
    let beta = pf::pre(&w, C, pf::fields(C, "beta", Some(L), 1)); // Running
    answers.insert(format!("t{C}"), Answer::Wire(cf::wire(&beta)));
    let legacy = pf::pre(&w, P, pf::fields(P, "legacy", Some(L), 1)); // Lost
    let mut t = cf::wire(&legacy);
    t["argv"] = json!(crate::agents::life_fixture::LOST_ARGVS[0]);
    answers.insert(format!("t{P}"), Answer::Wire(t));
    // no usable identity: held, never looked up
    let mut alpha = pf::pre(&w, D, pf::fields(D, "alpha", Some(L), 1));
    alpha.as_object_mut().unwrap().remove("launcherArgv");
    crate::agents::fixture::write(&w.folder(D).join("record.json"), alpha);
    pf::script(&w, answers);
    let e = pf::admit_spawn(&w).unwrap_err();
    assert_eq!(
        e.message,
        "the team already has 6 members besides the leader; held by pre-lifecycle members: \
         alpha (its record names no usable hc terminal; it stays held), \
         beta (running; mux-agent stop beta stops it), \
         legacy (run mux-agent stop legacy once its client is gone)"
    );
    w.fakes.hc_done().unwrap();
}
