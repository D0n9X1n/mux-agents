//! prelife-1c Task 8: a state folder written by v0.2.8 keeps working after the upgrade (G10, ambiguities 2, 4).
use crate::agents::fixture::World;
use crate::agents::prelife_fixture::{self as pf, P};

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
