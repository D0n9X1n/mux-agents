//! occupy-3 reservations and evidence (Task 4): orphans, siblings, absent specs, mixed case, wrong types, boots.
use super::support::held;
use super::support::*;
use super::*;

// Finding 8, call 2, checklist 10: a managed client whose entry cannot be used holds every conversation as `Evidence`
// and still names its known holder (pid 7100, image claude, member Y): a torn entry, `{}`, an invalid id (finding 12),
// a valid `at` with no `sessionId`, no `at`, and a valid entry with no launch time to compare (`launcher.json` without
// `startedAt`).
#[test]
fn an_unusable_entry_names_its_known_holder() {
    let no_at = format!("{{\"sessionId\":\"{X}\",\"seq\":1}}");
    let cases: [(&str, &[u8]); 6] = [
        ("torn", b"{\"se"),
        ("empty", b"{}"),
        (
            "invalid id",
            b"{\"sessionId\":\"invalid\",\"at\":\"2099-01-01T00:00:00.0000000Z\",\"seq\":1}",
        ),
        (
            "no session id",
            b"{\"at\":\"2099-01-01T00:00:00.0000000Z\",\"seq\":1}",
        ),
        ("no at", no_at.as_bytes()),
        ("no launch time", b""),
    ];
    for (case, bytes) in cases {
        let w = World::human("");
        let f = life(&w, "claude", S);
        if case == "no launch time" {
            publish(
                &gens::path(&f, 1, "launcher"),
                &json!({"pid": 31337, "start": 1000}),
            )
            .unwrap();
        }
        launched(&w, &f, 1);
        w.state();
        let entry = live::path(&w.ctx, Client::Claude, 7100, 1500);
        if case == "no launch time" {
            live::report(&w.ctx, Client::Claude, 7100, 1500, X, "resume").unwrap();
        } else {
            plant(&entry, bytes);
        }
        for s in [S, X, B] {
            let got = named(held_on(&w.ctx, s, Client::Claude, OsKind::Unix, None));
            assert_eq!(got, client_evidence(), "{case}: {s}");
        }
    }
}

// Call 11: an unresolved orphan (a generation the record no longer names, not done; group B's `Gen::orphans()`)
// reserves its own session until it is done.
#[test]
fn an_unresolved_orphan_reserves_its_own_session() {
    let w = World::human("");
    let f = life(&w, "claude", S); // generation 1 active
    publish(&gens::path(&f, 0, "commit"), &json!({"committed": true})).unwrap();
    publish(&gens::path(&f, 0, "spec"), &json!({"sessionId": X})).unwrap();
    assert_eq!(
        held(&w, X, Client::Claude),
        Some((Rule::Reserved, None)),
        "orphan 0 reserves its X"
    );
    publish(&gens::path(&f, 0, "ended"), &json!({"state": "finished"})).unwrap();
    assert_eq!(held(&w, X, Client::Claude), None, "done: free");
}

// Checklist 5: an orphan list that cannot be read (generation 0's completion evidence torn) holds every conversation
// as evidence naming the member; it never reads as "no orphans".
#[test]
fn an_unreadable_orphan_list_holds_as_evidence() {
    let w = World::human("");
    let f = life(&w, "claude", S); // generation 1 active
    torn(&gens::path(&f, 0, "ended"));
    for kind in [Client::Claude, Client::Copilot] {
        let got = held_on(&w.ctx, X, kind, OsKind::Unix, None).map(|h| (h.rule, h.pid, h.member));
        assert_eq!(
            got,
            Some((Rule::Evidence, None, Some(Y.to_string()))),
            "{kind:?}"
        );
    }
}

// Call 14, checklist 6: a spec-less orphan never had a client (`forward` publishes `spec.json` before `hc run`), so an
// empty `gens/1` beside active generation 2 (spec X; record S) reserves nothing: S, which only a fallback to the
// record could reserve, stays free. A torn `gens/1/spec.json` still refuses `evidence`, naming Y.
#[test]
fn a_spec_less_orphan_reserves_nothing_and_a_torn_spec_refuses() {
    for case in ["empty", "torn spec"] {
        let w = World::human("");
        let f = w.uncommitted(
            Y,
            json!({"lifecycle": 1, "generation": "2", "hcName": "m0123abcd", "client": "claude", "sessionId": S}),
        );
        publish(&gens::path(&f, 2, "commit"), &json!({"committed": true})).unwrap();
        publish(&gens::path(&f, 2, "spec"), &json!({"sessionId": X})).unwrap();
        std::fs::create_dir_all(gens::dir(&f, 1)).unwrap();
        if case == "torn spec" {
            torn(&gens::path(&f, 1, "spec"));
        }
        let got = held_on(&w.ctx, S, Client::Claude, OsKind::Unix, None)
            .map(|h| (h.rule, h.pid, h.member));
        let want = (case == "torn spec").then(|| (Rule::Evidence, None, Some(Y.to_string())));
        assert_eq!(got, want, "{case}: S");
        // Generation 2's own X: reserved; with a torn orphan spec the whole member refuses.
        let rule = if case == "empty" {
            Rule::Reserved
        } else {
            Rule::Evidence
        };
        assert_eq!(held(&w, X, Client::Claude), Some((rule, None)), "{case}: X");
    }
}

// Call 13, the audit's two sibling repros: an open active generation (spec S) beside a done pending one (spec X)
// leaves X free; an open pending generation (spec X) beside a done active one (record and spec S) leaves S free.
#[test]
fn a_generation_never_reserves_a_siblings_session() {
    for (case, done, own, sibling) in [("active open", 2u64, S, X), ("pending open", 1, X, S)] {
        let w = World::human("");
        let f = w.uncommitted(
            Y,
            json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd", "client": "claude", "sessionId": S,
                "pending": {"gen": 2, "hcName": "m4567cdef", "sessionId": X}}),
        );
        for (n, s) in [(1, S), (2, X)] {
            publish(&gens::path(&f, n, "commit"), &json!({"committed": true})).unwrap();
            publish(&gens::path(&f, n, "spec"), &json!({"sessionId": s})).unwrap();
        }
        publish(
            &gens::path(&f, done, "ended"),
            &json!({"state": "finished"}),
        )
        .unwrap();
        assert_eq!(
            held(&w, own, Client::Claude),
            Some((Rule::Reserved, None)),
            "{case}: its own"
        );
        assert_eq!(
            held(&w, sibling, Client::Claude),
            None,
            "{case}: the sibling's"
        );
    }
}

// Call 13, checklist 8, the bound-pending sibling repro: active generation 1 (no client) reserves S; pending generation
// 2's spec names X and its proven client 7100 reports B. X is free (the client moved; a bound generation reserves
// nothing beside its client), B is held by the managed client, S by the reservation.
#[test]
fn a_bound_pending_generation_reserves_nothing_beside_its_client() {
    let w = World::human("");
    let f = w.uncommitted(
        Y,
        json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd", "client": "claude", "sessionId": S,
            "pending": {"gen": 2, "hcName": "m4567cdef", "sessionId": X}}),
    );
    for (n, s) in [(1, S), (2, X)] {
        publish(&gens::path(&f, n, "commit"), &json!({"committed": true})).unwrap();
        publish(&gens::path(&f, n, "spec"), &json!({"sessionId": s})).unwrap();
    }
    launched(&w, &f, 2);
    live::report(&w.ctx, Client::Claude, 7100, 1500, B, "resume").unwrap();
    assert_eq!(
        held(&w, B, Client::Claude),
        Some((Rule::Managed, Some(7100))),
        "the client's report"
    );
    assert_eq!(
        held(&w, X, Client::Claude),
        None,
        "pending's own X: its client moved"
    );
    assert_eq!(
        held(&w, S, Client::Claude),
        Some((Rule::Reserved, None)),
        "active's S"
    );
}

// Findings 11, 13: with no spec, the active generation reserves the record's `sessionId` and the pending one its own
// `pending.sessionId`; both are mixed case in the record and held lowercased.
#[test]
fn absent_specs_fall_back_to_each_generations_own_record_session() {
    let w = World::human("");
    let f = w.uncommitted(
        Y,
        json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd", "client": "claude", "sessionId": M,
            "pending": {"gen": 2, "hcName": "m4567cdef", "sessionId": N}}),
    );
    for n in [1, 2] {
        publish(&gens::path(&f, n, "commit"), &json!({"committed": true})).unwrap();
    }
    for s in [M, N] {
        assert_eq!(
            held(&w, &s.to_ascii_lowercase(), Client::Claude),
            Some((Rule::Reserved, None)),
            "{s}"
        );
    }
    assert_eq!(held(&w, B, Client::Claude), None);
}

// Checklist 9: ids compare without case, each side tested on its own: a mixed-case spec id against a lowercase query,
// a lowercase spec id against an uppercase query, and a managed client's session against an uppercase query.
#[test]
fn mixed_case_ids_compare_without_case() {
    let lower = "abcdef03-2222-4333-8444-555555555555";
    let cases = [
        ("mixed spec, lowercase query", M, M.to_ascii_lowercase()),
        (
            "lowercase spec, uppercase query",
            lower,
            lower.to_ascii_uppercase(),
        ),
    ];
    for (case, spec, query) in cases {
        let w = World::human("");
        life(&w, "claude", spec);
        assert_eq!(
            held(&w, &query, Client::Claude),
            Some((Rule::Reserved, None)),
            "{case}"
        );
    }
    let w = World::human("");
    let f = life(&w, "claude", S);
    launched(&w, &f, 1);
    let upper = S.to_ascii_uppercase();
    assert_eq!(
        held(&w, &upper, Client::Claude),
        Some((Rule::Managed, Some(7100))),
        "managed, uppercase query"
    );
}

// Call 12: members are enumerated through the walk-checked enumeration. A file or a link (on Windows `link_dir`
// makes a symlink or a junction) at `<key>/agents` refuses as `Evidence` naming no member; no `agents`: no members.
#[test]
fn a_wrong_type_agents_folder_refuses() {
    for case in ["file", "link", "absent"] {
        let w = World::human("");
        let key = w.ctx.state_root.join("0123456789abcdef");
        std::fs::create_dir_all(&key).unwrap();
        match case {
            "file" => std::fs::write(key.join("agents"), b"x").unwrap(),
            "link" => {
                let elsewhere = w.ctx.state_root.join("elsewhere");
                std::fs::create_dir_all(elsewhere.join("agents")).unwrap();
                crate::testing::link_dir(&elsewhere.join("agents"), &key.join("agents"));
            }
            _ => {}
        }
        let got = held_on(&w.ctx, X, Client::Claude, OsKind::Unix, None)
            .map(|h| (h.rule, h.pid, h.member));
        let want = (case != "absent").then_some((Rule::Evidence, None, None));
        assert_eq!(got, want, "{case}");
    }
}

// Checklist 7 (call 16): `effective` reads the create-once `launcher.json` a second time for `startedAt`. If that read
// finds it gone or torn after `gens::client` proved the client, the client's session is unknown: every query holds as
// `Evidence`, still naming the client. (Seam: `hooks::before_launch_reread`.)
#[test]
fn a_launch_record_lost_after_the_proof_holds_as_evidence() {
    for case in ["gone", "torn"] {
        let w = World::human("");
        let f = life(&w, "claude", S);
        launched(&w, &f, 1);
        live::report(&w.ctx, Client::Claude, 7100, 1500, X, "resume").unwrap();
        assert_eq!(
            held(&w, X, Client::Claude),
            Some((Rule::Managed, Some(7100))),
            "{case}: before"
        );
        let launcher = gens::path(&f, 1, "launcher");
        let gone = case == "gone";
        hooks::before_launch_reread(Some(Box::new(move || {
            if gone {
                let _ = std::fs::remove_file(&launcher);
            } else {
                std::fs::write(&launcher, b"{\"pi").unwrap();
            }
        })));
        let got = named(held_on(&w.ctx, X, Client::Claude, OsKind::Unix, None));
        hooks::before_launch_reread(None);
        assert_eq!(got, client_evidence(), "{case}");
    }
}

// Checklist 15 (r2b calls 24, 27): an entry recorded in another boot is dead (`live::entry` answers `Ok(None)`): the
// managed client holds its generation's session, as with no entry (call 9).
#[test]
fn an_entry_from_another_boot_proves_nothing() {
    let w = World::human("");
    w.fakes.set_boot(Boot::Id("boot-a".into()));
    let f = life(&w, "claude", S);
    launched(&w, &f, 1);
    live::report(&w.ctx, Client::Claude, 7100, 1500, X, "resume").unwrap();
    assert_eq!(
        held(&w, X, Client::Claude),
        Some((Rule::Managed, Some(7100))),
        "this boot: it counts"
    );
    w.fakes.set_boot(Boot::Id("boot-b".into()));
    assert_eq!(held(&w, X, Client::Claude), None, "another boot's X");
    assert_eq!(
        held(&w, S, Client::Claude),
        Some((Rule::Managed, Some(7100))),
        "the generation's S"
    );
}

// Checklist 16 (r2b call 29): where starts are boot-relative, an entry whose boot cannot be verified (the current boot
// unreadable, or an entry recorded with no boot) is never used: every query holds as `Evidence` naming the client.
#[test]
fn an_unverifiable_boot_holds_as_evidence_naming_the_client() {
    for case in ["boot unreadable", "entry without a boot"] {
        let w = World::human("");
        if case == "boot unreadable" {
            w.fakes.set_boot(Boot::Id("boot-a".into()));
        }
        let f = life(&w, "claude", S);
        launched(&w, &f, 1);
        live::report(&w.ctx, Client::Claude, 7100, 1500, X, "resume").unwrap();
        w.fakes.set_boot(match case {
            "boot unreadable" => Boot::Unreadable,
            _ => Boot::Id("boot-a".into()),
        });
        for s in [S, X, B] {
            let got = named(held_on(&w.ctx, s, Client::Claude, OsKind::Unix, None));
            assert_eq!(got, client_evidence(), "{case}: {s}");
        }
    }
}
