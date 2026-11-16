//! occupy-3 caller exemption (Task 4b) and the documented limits (Task 6).
use super::support::held;
use super::support::*;
use super::*;

// Call 1, test 1: a machine whose only client is the caller's own resumes B. Without the exemption the same row holds.
#[test]
fn a_caller_alone_frees_every_other_conversation() {
    let w = World::agent("");
    let own = pane_own(&w);
    assert_eq!(
        held(&w, B, Client::Claude),
        Some((Rule::Unmanaged, Some(CLAUDE))),
        "no exemption: H4.4"
    );
    assert_eq!(held_as(&w, B, Client::Claude, OsKind::Unix, &own), None);
    assert_eq!(held_as(&w, B, Client::Copilot, OsKind::Unix, &own), None);
}

// Call 1, test 2: the caller whose argv names B blocks B (H4.3 still applies to it), and only B.
#[test]
fn the_callers_argv_naming_the_session_holds_it() {
    let w = World::agent("");
    let own = pane_own(&w);
    w.fakes.forget(CLAUDE);
    w.fakes.process(Row {
        argv: Some(vec!["claude".into(), "--resume".into(), B.into()]),
        ..crate::testing::row(CLAUDE, PANE, 1500)
    });
    assert_eq!(
        held_as(&w, B, Client::Claude, OsKind::Unix, &own),
        Some((Rule::Argv, Some(CLAUDE)))
    );
    assert_eq!(held_as(&w, X, Client::Claude, OsKind::Unix, &own), None);
}

// Call 1, test 3: a second unmanaged Claude still blocks B, beside the exempt caller.
#[test]
fn a_second_unmanaged_client_still_holds_beside_the_caller() {
    let w = World::agent("");
    let own = pane_own(&w);
    w.fakes.process(row(
        Some(1),
        Some("/usr/bin/claude"),
        Some(&["claude"]),
        Some("claude"),
    ));
    w.fakes.client(7001, Client::Claude);
    assert_eq!(
        held_as(&w, B, Client::Claude, OsKind::Unix, &own),
        Some((Rule::Unmanaged, Some(7001)))
    );
}

// Call 1: the caller's own newest entry naming B holds B (`Rule::Caller`); one naming X frees B; a torn entry holds as
// evidence (R2). Each refusal names the caller's pid (call 2).
#[test]
fn the_callers_own_newest_report_holds_its_session() {
    let cases = [
        ("names B", Some((Rule::Caller, Some(CLAUDE)))),
        ("names X", None),
        ("torn", Some((Rule::Evidence, Some(CLAUDE)))),
    ];
    for (case, want) in cases {
        let w = World::agent("");
        let own = pane_own(&w);
        w.state();
        match case {
            "torn" => torn(&live::path(&w.ctx, Client::Claude, CLAUDE, 1500)),
            _ => {
                let s = if case == "names B" { B } else { X };
                live::report(&w.ctx, Client::Claude, CLAUDE, 1500, s, "resume").unwrap();
            }
        }
        assert_eq!(
            held_as(&w, B, Client::Claude, OsKind::Unix, &own),
            want,
            "{case}"
        );
    }
}

// Call 7: `Own` is the proof's (pid, start), never the chain's. The chain names A (150); the member proof names B
// (160): only B is exempt and A still holds. A candidate or member without proof, and a person, exempt nothing. A
// listed start other than the proof's (a reused pid) is not exempt.
#[test]
fn own_is_the_proof_never_the_chain() {
    let w = World::agent("");
    let mut hc = w.caller();
    (hc.agent, hc.member) = (None, Some(ID.to_string()));
    let proof = crate::agents::tree::Member {
        id: ID.into(),
        record: json!({}),
        client: Client::Claude,
        pid: 160,
        start: 1600,
        flags: Vec::new(),
    };
    assert_eq!(
        own_of(&hc, None),
        None,
        "no proof, no exemption; the chain is not read"
    );
    assert_eq!(
        own_of(&hc, Some(&crate::agents::tree::Standing::Person)),
        None
    );
    let own = own_of(&hc, Some(&crate::agents::tree::Standing::Member(proof))).unwrap();
    assert_eq!((own.kind, own.pid, own.start), (Client::Claude, 160, 1600));
    w.fakes.process(Row {
        argv: Some(vec!["claude".into()]),
        ..crate::testing::row(160, PANE, 1600)
    });
    w.fakes.client(160, Client::Claude);
    assert_eq!(
        held_as(&w, B, Client::Claude, OsKind::Unix, &own),
        Some((Rule::Unmanaged, Some(CLAUDE))),
        "A holds"
    );
    w.fakes.forget(CLAUDE);
    assert_eq!(
        held_as(&w, B, Client::Claude, OsKind::Unix, &own),
        None,
        "B alone: exempt"
    );
    let reused = Own { start: 1601, ..own };
    assert_eq!(
        held_as(&w, B, Client::Claude, OsKind::Unix, &reused),
        Some((Rule::Unmanaged, Some(160))),
        "reused pid"
    );
}

// Call 8: a Copilot caller proven as the native copilot 7101 under its npm loader 7100. On Unix `pair_parent` pairs
// them: both are exempt and the loader keys the entry; a hold names the proven 7101 (call 2). A newer process at the
// loader pid (start 9999 > 2101) is not the loader: the caller's report cannot be read, so `Evidence` on 7101. On
// Windows there is no pair: the node row is an unmanaged Copilot and holds.
#[test]
fn a_copilot_callers_loader_joins_only_by_report_2bs_rule() {
    for (os, loader_start) in [
        (OsKind::Unix, 2100),
        (OsKind::Unix, 9999),
        (OsKind::Windows, 2100),
    ] {
        let w = World::agent("");
        let own = Own {
            kind: Client::Copilot,
            pid: 7101,
            start: 2101,
        };
        copilot_pair_rows(&w, Some(loader_start));
        let case = format!("{os:?} loader start {loader_start}");
        let want = match (os, loader_start) {
            (OsKind::Windows, _) => Some((Rule::Unmanaged, Some(7100))),
            (_, 9999) => Some((Rule::Evidence, Some(7101))),
            _ => None,
        };
        assert_eq!(held_as(&w, B, Client::Copilot, os, &own), want, "{case}");
        if want.is_some() {
            continue;
        }
        w.state();
        live::report(&w.ctx, Client::Copilot, 7100, 2100, B, "resume").unwrap();
        assert_eq!(
            held_as(&w, B, Client::Copilot, os, &own),
            Some((Rule::Caller, Some(7101))),
            "{case}: keyed by the loader"
        );
    }
}

// Call 17, the auditor's mixed-read repro: the saved table lists the loader 7100 without a start (a later read could
// answer (7100, 2100)); the caller's entry under (7100, 2100) names S. The loader is taken from the saved table only:
// it joins nothing, the caller's report cannot be read, and every query holds (`Evidence`, naming 7101); never free.
// No `Os::row` read is made for 7100.
#[test]
fn a_startless_saved_loader_holds_and_is_never_read_again() {
    let w = World::agent("");
    let own = Own {
        kind: Client::Copilot,
        pid: 7101,
        start: 2101,
    };
    copilot_pair_rows(&w, None);
    w.state();
    live::report(&w.ctx, Client::Copilot, 7100, 2100, S, "startup").unwrap();
    for kind in [Client::Claude, Client::Copilot] {
        assert_eq!(
            held_as(&w, S, kind, OsKind::Unix, &own),
            Some((Rule::Evidence, Some(7101))),
            "{kind:?}"
        );
    }
    assert!(
        !w.fakes.os_calls().iter().any(|c| c == "row 7100"),
        "{:?}",
        w.fakes.os_calls()
    );
}

// Call 19, reverse pairing: the proven client is the loader 7100 (2100); on Unix its native child 7101, which
// `pair_parent` pairs back to it, is exempt too, and the loader keys the entry. On Windows nothing pairs: the native
// child is an unmanaged Copilot and holds.
#[test]
fn a_loader_callers_native_child_is_exempt() {
    for os in [OsKind::Unix, OsKind::Windows] {
        let w = World::agent("");
        let own = Own {
            kind: Client::Copilot,
            pid: 7100,
            start: 2100,
        };
        copilot_pair_rows(&w, Some(2100));
        if os == OsKind::Windows {
            assert_eq!(
                held_as(&w, B, Client::Copilot, os, &own),
                Some((Rule::Unmanaged, Some(7101)))
            );
            continue;
        }
        assert_eq!(
            held_as(&w, B, Client::Copilot, os, &own),
            None,
            "loader and its native child exempt"
        );
        w.state();
        live::report(&w.ctx, Client::Copilot, 7100, 2100, B, "resume").unwrap();
        assert_eq!(
            held_as(&w, B, Client::Copilot, os, &own),
            Some((Rule::Caller, Some(7100)))
        );
    }
}

// H5: the two accepted limits are stated where agents read them.
#[test]
fn the_skill_states_the_known_limits() {
    let skill = include_str!("../../skills/mux-agents/SKILL.md");
    for needle in [
        "`resume` refuses while any process on this machine may hold that conversation",
        "switches conversation with `/resume` while no hook fires",
        "is judged by its image name",
        "Your own client is exempt",
        "mounted with `hidepid` (other than 0) or `subset=pid`",
        "with other Claude or Copilot sessions open on this machine",
    ] {
        assert!(skill.contains(needle), "{needle}");
    }
}

// Call 29, caller path: proven native 7101, keyed by its loader 7100. The loader exits between the saved table and
// the registry read (seam `hooks::before_entry_read`) while the child survives. Its report naming X still holds X, as
// Caller naming 7101 (the entry is never dropped); with no entry at all the report is unknown (Evidence).
#[test]
fn a_callers_loader_exiting_between_reads_still_holds_its_report() {
    let own = Own {
        kind: Client::Copilot,
        pid: 7101,
        start: 2101,
    };
    let pair = |report: bool| {
        let w = World::agent("");
        copilot_pair_rows(&w, Some(2100));
        w.state();
        if report {
            live::report(&w.ctx, Client::Copilot, 7100, 2100, X, "resume").unwrap();
        }
        w
    };
    let query = |w: &World| held_as(w, X, Client::Copilot, OsKind::Unix, &own);
    let exited = |w: &World| {
        let fakes = w.fakes.clone();
        hooks::before_entry_read(Some(Box::new(move || fakes.forget(7100))));
        let got = query(w);
        hooks::before_entry_read(None);
        got
    };
    for (report, live_loader, gone) in [
        (
            true,
            Some((Rule::Caller, Some(7101))),
            Some((Rule::Caller, Some(7101))),
        ),
        (false, None, Some((Rule::Evidence, Some(7101)))),
    ] {
        assert_eq!(
            query(&pair(report)),
            live_loader,
            "report {report}: control"
        );
        assert_eq!(
            exited(&pair(report)),
            gone,
            "report {report}: the loader exits"
        );
    }
}
