//! occupy-3 managed clients (Task 4): binding, effective sessions, loader pairs, launch-time checks.
use super::support::held;
use super::support::*;
use super::*;

// H2, H4.2: managed Y (its client.json matches 7100) holds only its effective session: the spec's S, then the X its
// newest hook report names. Its argv naming S adds nothing; its record's S never blocks another subject on S.
#[test]
fn a_managed_client_holds_only_its_effective_session() {
    let w = World::human("");
    let f = life(&w, "claude", S);
    launched(&w, &f, 1);
    assert_eq!(
        held(&w, S, Client::Claude),
        Some((Rule::Managed, Some(7100))),
        "no report yet: the generation's session"
    );
    let h = held_on(&w.ctx, S, Client::Claude, OsKind::Unix, None).unwrap();
    assert_eq!(
        (h.member.as_deref(), h.image.as_deref()),
        (Some(Y), Some("claude")),
        "PM call 2: member and image"
    );
    assert_eq!(
        held(&w, B, Client::Claude),
        None,
        "managed: B is free to restore"
    );
    assert_eq!(
        live::report(&w.ctx, Client::Claude, 7100, 1500, X, "resume").map_err(|r| r.check),
        Ok(1)
    );
    assert_eq!(
        held(&w, X, Client::Claude),
        Some((Rule::Managed, Some(7100))),
        "the newest report"
    );
    assert_eq!(
        held(&w, S, Client::Claude),
        None,
        "argv and record S prove nothing once the client moved"
    );
    assert_eq!(held(&w, B, Client::Claude), None);
}

// H4.4: an unmanaged live Claude started on A holds A (its argv, H4.3) and nothing else while its own newest hook
// report names A: another Claude Code session open on the machine must not block every resume. Once its hook reports
// an in-session `/resume` into X, X is held too, as H4.4 (`Unmanaged`); its argv still names A. Another kind's query
// never reads its report.
#[test]
fn an_unmanaged_client_holds_only_what_its_argv_or_own_report_names() {
    let w = World::human("");
    w.fakes.process(Row {
        pid: 7200,
        ppid: 1,
        start: Some(1200),
        path: Some("/usr/bin/claude".into()),
        argv: Some(vec!["claude".into(), "--resume".into(), A.into()]),
        image: Some("claude".into()),
    });
    w.fakes.client(7200, Client::Claude);
    w.state();
    assert_eq!(
        live::report(&w.ctx, Client::Claude, 7200, 1200, A, "startup").map_err(|r| r.check),
        Ok(1)
    );
    assert_eq!(
        held(&w, X, Client::Claude),
        None,
        "its report names A, not X"
    );
    assert_eq!(held(&w, A, Client::Claude), Some((Rule::Argv, Some(7200))));
    assert_eq!(held(&w, X, Client::Copilot), None);
    assert_eq!(
        live::report(&w.ctx, Client::Claude, 7200, 1200, X, "resume").map_err(|r| r.check),
        Ok(2)
    );
    assert_eq!(
        held(&w, X, Client::Claude),
        Some((Rule::Unmanaged, Some(7200))),
        "its newest report names X"
    );
    assert_eq!(
        held(&w, A, Client::Claude),
        Some((Rule::Argv, Some(7200))),
        "its argv still names A"
    );
    assert_eq!(held(&w, X, Client::Copilot), None);
}

// H4.4 end to end through `held`: an unmanaged Claude 7200 (start 1200) whose argv names no conversation is judged
// only by its own newest hook report, keyed by its own (pid, start) as `report -Session` writes it. No report, or one
// naming another conversation, holds nothing: another session open on the machine must not block every resume. One
// naming X holds X as `Unmanaged`, naming the pid. A torn report, or one naming no valid conversation id, cannot say
// where it is, so it holds as `Evidence` (R2, fail closed). Its process gone between the table read and the registry
// read with no entry left is unknown too: `Evidence`. A key that cannot be resolved (its handle opens with no start)
// names no report its hook could have written, so it holds nothing.
// H4.4 stays fail closed where the row cannot be read: a client of the kind whose argv or start cannot be read, with no
// report of its own, may be on any conversation, so it holds as unbound (H4.5), as an unidentified row would. Its
// own report still decides when there is one: naming another conversation frees it, naming X holds X.
#[test]
fn an_unmanaged_client_whose_argv_cannot_be_read_holds_unless_its_report_says_otherwise() {
    for (case, want) in [
        ("no report", Some((Rule::Unbound, Some(7200)))),
        ("names another", None),
        ("names X", Some((Rule::Unmanaged, Some(7200)))),
    ] {
        let w = World::human("");
        w.fakes.process(Row {
            pid: 7200,
            ppid: 1,
            start: Some(1200),
            path: Some("/usr/bin/claude".into()),
            argv: None,
            image: Some("claude".into()),
        });
        w.fakes.client(7200, Client::Claude);
        match case {
            "names another" => {
                live::report(&w.ctx, Client::Claude, 7200, 1200, A, "startup").unwrap()
            }
            "names X" => live::report(&w.ctx, Client::Claude, 7200, 1200, X, "startup").unwrap(),
            _ => 0,
        };
        assert_eq!(held(&w, X, Client::Claude), want, "{case}");
    }
}

#[test]
fn an_unmanaged_client_is_held_only_by_its_own_report() {
    let cases = [
        ("no report", None),
        ("names another", None),
        ("names X", Some((Rule::Unmanaged, Some(7200)))),
        ("torn", Some((Rule::Evidence, Some(7200)))),
        ("no valid id", Some((Rule::Evidence, Some(7200)))),
        ("gone, no entry", None),
        ("key unresolved", Some((Rule::Evidence, Some(7200)))),
        ("exited, key unresolved", None),
    ];
    for (case, want) in cases {
        let w = World::human("");
        w.fakes.process(Row {
            pid: 7200,
            ppid: 1,
            start: Some(1200),
            path: Some("/usr/bin/claude".into()),
            argv: Some(vec!["claude".into()]),
            image: Some("claude".into()),
        });
        w.fakes.client(7200, Client::Claude);
        let report = |s: &str| {
            live::report(&w.ctx, Client::Claude, 7200, 1200, s, "startup").unwrap();
        };
        match case {
            "names another" => report(A),
            "names X" => report(X),
            "torn" => torn(&live::path(&w.ctx, Client::Claude, 7200, 1200)),
            "no valid id" => report("not-a-conversation"),
            "key unresolved" => w.fakes.opens(7200, &[None]),
            _ => {}
        }
        let got = if case == "exited, key unresolved" {
            // Listed by the table read, then exits before its key is resolved: a client that has exited holds nothing.
            let fakes = w.fakes.clone();
            hooks::before_key(Some(Box::new(move || fakes.forget(7200))));
            let got = held(&w, X, Client::Claude);
            hooks::before_key(None);
            got
        } else if case == "gone, no entry" {
            let fakes = w.fakes.clone();
            hooks::before_entry_read(Some(Box::new(move || fakes.forget(7200))));
            let got = held(&w, X, Client::Claude);
            hooks::before_entry_read(None);
            got
        } else {
            held(&w, X, Client::Claude)
        };
        assert_eq!(got, want, "{case}");
        // Another kind's query never reads a Claude report.
        assert_eq!(held(&w, X, Client::Copilot), None, "{case}: Copilot");
    }
}

// H4.4, keyed as the hook writes it: an unmanaged native `copilot` 7101 (start 2101) under its npm loader `node` 7100
// (start 2100), both Copilot clients and neither member-launched. The native is listed first, so it answers first. On
// a Unix host `ancestry::identity` keys the native by its loader: a report under (7100, 2100) naming X holds X as
// `Unmanaged` on 7101, the native; a report under the native's own (7101, 2101) is none its hook could have written,
// so it holds nothing (the loader, keyed by itself, has no report either). On Windows nothing pairs and each row is
// keyed by its own (pid, start), so the loader's report names the loader and the native's names the native.
#[test]
fn an_unmanaged_native_copilot_is_judged_by_its_loaders_report() {
    let proc_row = |pid: u32, ppid: u32, start: i64, file: &str| Row {
        pid,
        ppid,
        start: Some(start),
        path: Some(format!("/usr/lib/{file}")),
        argv: Some(vec![file.into()]),
        image: Some(file.into()),
    };
    let unix = HOST == OsKind::Unix;
    let cases = [
        (
            (7100, 2100),
            Some((Rule::Unmanaged, Some(if unix { 7101 } else { 7100 }))),
        ),
        (
            (7101, 2101),
            (!unix).then_some((Rule::Unmanaged, Some(7101))),
        ),
    ];
    for ((pid, start), want) in cases {
        let w = World::human("");
        w.fakes.process(proc_row(7101, 7100, 2101, "copilot"));
        w.fakes.process(proc_row(7100, 1, 2100, "node"));
        w.fakes.client(7100, Client::Copilot);
        w.fakes.client(7101, Client::Copilot);
        assert_eq!(
            who(held_on(&w.ctx, X, Client::Copilot, HOST, None)),
            None,
            "no report"
        );
        live::report(&w.ctx, Client::Copilot, pid, start, X, "startup").unwrap();
        assert_eq!(
            who(held_on(&w.ctx, X, Client::Copilot, HOST, None)),
            want,
            "report under {pid}"
        );
        assert_eq!(
            who(held_on(&w.ctx, A, Client::Copilot, HOST, None)),
            None,
            "report under {pid}: A is free"
        );
    }
}

// H4.4, a Unix native `copilot` whose npm loader was killed while it lived on: keyed by itself now, it is judged by the
// entry its hook also wrote under its own key (`report -Session`): naming X holds X; naming another frees X.
#[test]
fn a_native_copilot_that_outlived_its_loader_is_judged_by_its_own_entry() {
    for (named, want) in [(X, Some((Rule::Unmanaged, Some(7101)))), (A, None)] {
        let w = World::human("");
        w.fakes.process(Row {
            pid: 1,
            ppid: 0,
            start: Some(1),
            path: Some("/sbin/launchd".into()),
            argv: Some(vec!["launchd".into()]),
            image: Some("launchd".into()),
        });
        w.fakes.process(Row {
            pid: 7101,
            ppid: 1,
            start: Some(2101),
            path: Some("/usr/lib/copilot".into()),
            argv: Some(vec!["copilot".into()]),
            image: Some("copilot".into()),
        });
        w.fakes.client(7101, Client::Copilot);
        live::report(&w.ctx, Client::Copilot, 7101, 2101, named, "resume").unwrap();
        assert_eq!(
            who(held_on(&w.ctx, X, Client::Copilot, HOST, None)),
            want,
            "own entry names {named}"
        );
    }
}

// H4.4 fails closed when an unmanaged client's report cannot be looked up: a key that cannot be resolved (the process
// cannot be opened) holds as evidence, since its report could name the conversation.
#[test]
fn an_unmanaged_client_whose_key_cannot_be_resolved_holds() {
    let w = World::human("");
    w.fakes.process(Row {
        pid: 7200,
        ppid: 1,
        start: Some(1200),
        path: Some("/usr/bin/claude".into()),
        argv: Some(vec!["claude".into()]),
        image: Some("claude".into()),
    });
    w.fakes.client(7200, Client::Claude);
    w.fakes.opens(7200, &[None]);
    assert_eq!(
        held(&w, X, Client::Claude),
        Some((Rule::Evidence, Some(7200))),
        "a key that cannot be resolved"
    );
}

// H3, call 13: no client.json yet (pending, or the launcher paused after launcher.json): the generation reserves its
// own session, the spec's X, for every kind; the record's S is not its session and stays free, as does B.
#[test]
fn a_generation_without_its_client_reserves_its_own_session() {
    for paused in [false, true] {
        let w = World::human("");
        let f = life(&w, "claude", X);
        if paused {
            publish(
                &gens::path(&f, 1, "launcher"),
                &json!({"pid": 31337, "start": 1000}),
            )
            .unwrap();
            w.fakes.process(crate::testing::row(31337, 1, 1000));
        }
        for kind in [Client::Claude, Client::Copilot] {
            assert_eq!(
                held(&w, X, kind),
                Some((Rule::Reserved, None)),
                "paused {paused} {kind:?}: a reservation holds every kind"
            );
        }
        assert_eq!(
            held(&w, S, Client::Claude),
            None,
            "paused {paused}: the record's S is not this generation's"
        );
        assert_eq!(held(&w, B, Client::Claude), None, "paused {paused}");
    }
}

// H2 via K1a: a pid matching client.json with another start, a foreign (client-conflict) or torn client.json, or a
// done generation's client: unmanaged, so it is judged by its own newest report (H4.4), which here names B: it holds B
// as `Unmanaged`. Were it wrongly bound, the same report would answer `Managed`. It pins `bound`.
#[test]
fn an_unproven_or_done_client_is_unmanaged_and_holds() {
    for case in ["other start", "foreign", "torn client", "done"] {
        let w = World::human("");
        let f = life(&w, "claude", S);
        // `publish` is create-once: a file written first stands and `launched`'s publish answers Exists.
        match case {
            "other start" => publish(
                &gens::path(&f, 1, "client"),
                &json!({"pid": 7100, "start": 1499}),
            )
            .map(drop)
            .unwrap(),
            "foreign" => publish(
                &gens::path(&f, 1, "exit"),
                &json!({"reason": "client-conflict"}),
            )
            .map(drop)
            .unwrap(),
            "torn client" => torn(&gens::path(&f, 1, "client")),
            _ => publish(&gens::path(&f, 1, "ended"), &json!({"state": "finished"}))
                .map(drop)
                .unwrap(),
        }
        launched(&w, &f, 1);
        live::report(&w.ctx, Client::Claude, 7100, 1500, B, "resume").unwrap();
        assert_eq!(
            held(&w, B, Client::Claude),
            Some((Rule::Unmanaged, Some(7100))),
            "{case}"
        );
    }
}

// H3, fresh pending crash: active generation 1 (open, no spec) runs S, the record's; pending generation 2 committed
// for X, its launcher killed before client.json. Each reserves its own session (call 13): S and X held; B free.
#[test]
fn a_fresh_pending_crash_reserves_the_record_and_the_generation_session() {
    let w = World::human("");
    let f = w.uncommitted(
        Y,
        json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd", "client": "claude", "sessionId": S,
            "pending": {"gen": 2, "hcName": "m4567cdef", "sessionId": X}}),
    );
    publish(&gens::path(&f, 1, "commit"), &json!({"committed": true})).unwrap();
    publish(&gens::path(&f, 2, "commit"), &json!({"committed": true})).unwrap();
    publish(&gens::path(&f, 2, "spec"), &json!({"sessionId": X})).unwrap();
    publish(
        &gens::path(&f, 2, "launcher"),
        &json!({"pid": 31337, "start": 1000}),
    )
    .unwrap();
    assert_eq!(
        held(&w, S, Client::Claude),
        Some((Rule::Reserved, None)),
        "the record's S"
    );
    assert_eq!(
        held(&w, X, Client::Claude),
        Some((Rule::Reserved, None)),
        "the generation's X"
    );
    let h = held_on(&w.ctx, X, Client::Claude, OsKind::Unix, None).unwrap();
    assert_eq!(
        h.member.as_deref(),
        Some(Y),
        "a reservation names its member (PM call 2)"
    );
    assert_eq!(held(&w, B, Client::Claude), None);
}

// R2: unreadable evidence of a member holds every conversation, for every kind: a torn record, a torn spec, a torn
// ended marker, or a pending generation with no session anywhere. (A managed client's unusable entry names that
// client instead: `an_unusable_entry_names_its_known_holder`.)
#[test]
fn unreadable_member_evidence_holds_every_conversation() {
    for case in [
        "torn record",
        "torn spec",
        "torn ended",
        "pending without a session",
    ] {
        let w = World::human("");
        let f = match case {
            "pending without a session" => w.uncommitted(
                Y,
                json!({"lifecycle": 1, "client": "claude", "pending": {"gen": 1, "hcName": "m0123abcd"}}),
            ),
            "torn spec" => {
                let f = w.uncommitted(Y, json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd", "client": "claude"}));
                torn(&gens::path(&f, 1, "spec"));
                f
            }
            _ => life(&w, "claude", S),
        };
        match case {
            "torn record" => torn(&f.join("record.json")),
            "torn ended" => torn(&gens::path(&f, 1, "ended")),
            _ => {}
        }
        for kind in [Client::Claude, Client::Copilot] {
            let h = held_on(&w.ctx, B, kind, OsKind::Unix, None).unwrap();
            assert_eq!((h.rule, h.pid), (Rule::Evidence, None), "{case} {kind:?}");
            assert_eq!(
                h.member.as_deref(),
                Some(Y),
                "{case}: the refusal names the member (PM call 2)"
            );
        }
    }
}

// Ambiguity 6: an entry of an earlier process at pid 7100 (start 999) is never read for the client started at 1500.
#[test]
fn a_reused_pid_entry_proves_nothing() {
    let w = World::human("");
    let f = life(&w, "claude", S);
    w.fakes.process(crate::testing::row(7100, 1, 999));
    assert_eq!(
        live::report(&w.ctx, Client::Claude, 7100, 999, X, "resume").map_err(|r| r.check),
        Ok(1)
    );
    w.fakes.forget(7100);
    launched(&w, &f, 1);
    assert_eq!(held(&w, X, Client::Claude), None, "the old process's entry");
    assert_eq!(
        held(&w, S, Client::Claude),
        Some((Rule::Managed, Some(7100))),
        "the generation's own session"
    );
}

// H2, loader pair (proof-0b K1a): Copilot member Y's client.json names the npm loader 7100 (`node`); its native
// child 7101 (`copilot`) is the same managed client on Unix and holds only S: its argv resuming B adds nothing
// (H4.2). On Windows there is no pair: 7101 is an unmanaged Copilot, and its argv naming B holds B (H4.3). Argv, not a
// report, marks it: a report's key follows the host's pairing rule (`ancestry::identity`), not the `os` under test.
#[test]
fn a_copilot_loader_and_its_native_child_hold_one_session() {
    for os in [OsKind::Unix, OsKind::Windows] {
        let w = World::human("");
        let f = life(&w, "copilot", S);
        publish(
            &gens::path(&f, 1, "launcher"),
            &json!({"pid": 31337, "start": 1000}),
        )
        .unwrap();
        publish(
            &gens::path(&f, 1, "client"),
            &json!({"pid": 7100, "start": 1500}),
        )
        .unwrap();
        w.fakes.process(crate::testing::row(31337, 1, 1000));
        let proc_row = |pid: u32, ppid: u32, file: &str| Row {
            pid,
            ppid,
            start: Some(1500 + i64::from(pid - 7100)),
            path: Some(format!("/usr/lib/{file}")),
            argv: Some(vec![file.into()]),
            image: Some(file.into()),
        };
        w.fakes.process(proc_row(7100, 31337, "node"));
        w.fakes.process(Row {
            argv: Some(vec!["copilot".into(), "--resume".into(), B.into()]),
            ..proc_row(7101, 7100, "copilot")
        });
        w.fakes.client(7100, Client::Copilot);
        w.fakes.client(7101, Client::Copilot);
        assert_eq!(
            who(held_on(&w.ctx, S, Client::Copilot, os, None)),
            Some((Rule::Managed, Some(7100))),
            "{os:?}"
        );
        let want = (os == OsKind::Windows).then_some((Rule::Argv, Some(7101)));
        assert_eq!(
            who(held_on(&w.ctx, B, Client::Copilot, os, None)),
            want,
            "{os:?}"
        );
    }
}

// S6: a done generation (ended, released) reserves nothing; one whose `completing` names it is not done yet and
// reserves its own session X only (call 13), never the record's S.
#[test]
fn a_done_or_released_generation_reserves_nothing() {
    for (marker, completing) in [("ended", false), ("released", false), ("ended", true)] {
        let w = World::human("");
        let f = life(&w, "claude", X);
        publish(&gens::path(&f, 1, marker), &json!({})).unwrap();
        if completing {
            let mut r: Value = crate::state::fs::read_json(&f.join("record.json"))
                .unwrap()
                .unwrap();
            r["completing"] = json!({"gen": 1, "intent": "ended"});
            crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
        }
        for (s, own) in [(S, false), (X, true)] {
            let want = (completing && own).then_some((Rule::Reserved, None));
            assert_eq!(
                held(&w, s, Client::Claude),
                want,
                "{marker} completing {completing}: {s}"
            );
        }
    }
}

// Call 9 (boot reuse): an entry whose `at` is earlier than the generation's launch evidence time (`launcher.json`
// `startedAt`) comes from an earlier boot (Linux start ticks repeat): it is ignored and the client holds its
// generation's S. One at or after the launch counts, and its id is compared lowercased (finding 13).
#[test]
fn a_report_from_before_the_launch_proves_nothing() {
    let w = World::human("");
    let f = life(&w, "claude", S);
    w.state();
    live::report(&w.ctx, Client::Claude, 7100, 1500, X, "resume").unwrap(); // `at` = now
    w.fakes.advance(std::time::Duration::from_secs(60));
    launched(&w, &f, 1); // `startedAt` = now + 60 s; same pid and start
    assert_eq!(
        held(&w, S, Client::Claude),
        Some((Rule::Managed, Some(7100))),
        "earlier boot: the generation's S"
    );
    assert_eq!(held(&w, X, Client::Claude), None, "the earlier boot's X");
    live::report(&w.ctx, Client::Claude, 7100, 1500, M, "resume").unwrap(); // at the launch instant
    let m = M.to_ascii_lowercase();
    assert_eq!(
        held(&w, &m, Client::Claude),
        Some((Rule::Managed, Some(7100))),
        "not earlier: it counts"
    );
    assert_eq!(held(&w, S, Client::Claude), None);
}

// Call 29: the saved table pairs Copilot loader 7100 with native child 7101. The loader exits between the saved table
// and the registry read (seam `hooks::before_entry_read`) while the child survives. Its report naming X still holds X,
// as Managed, named by the surviving child; with no entry at all the session is unknown (Evidence), never S alone.
#[test]
fn a_pair_whose_loader_exits_between_reads_still_holds_its_report() {
    let pair = |report: bool| {
        let w = World::human("");
        let f = life(&w, "copilot", S);
        let at = crate::util::utc_o(w.ctx.clock.now());
        publish(
            &gens::path(&f, 1, "launcher"),
            &json!({"pid": 31337, "start": 1000, "startedAt": at}),
        )
        .unwrap();
        publish(
            &gens::path(&f, 1, "client"),
            &json!({"pid": 7100, "start": 1500}),
        )
        .unwrap();
        w.fakes.process(crate::testing::row(31337, 1, 1000));
        let proc_row = |pid: u32, ppid: u32, file: &str| Row {
            pid,
            ppid,
            start: Some(1500 + i64::from(pid - 7100)),
            path: Some(format!("/usr/lib/{file}")),
            argv: Some(vec![file.into()]),
            image: Some(file.into()),
        };
        w.fakes.process(proc_row(7100, 31337, "node"));
        w.fakes.process(proc_row(7101, 7100, "copilot"));
        w.fakes.client(7100, Client::Copilot);
        w.fakes.client(7101, Client::Copilot);
        if report {
            live::report(&w.ctx, Client::Copilot, 7100, 1500, X, "resume").unwrap();
        }
        w
    };
    let query = |w: &World| who(held_on(&w.ctx, X, Client::Copilot, OsKind::Unix, None));
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
            Some((Rule::Managed, Some(7100))),
            Some((Rule::Managed, Some(7101))),
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
