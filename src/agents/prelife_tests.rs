//! prelife-1c Task 8: a state folder written by v0.2.8 keeps working after the upgrade (G10, ambiguities 2, 4).
//! Task 9: the `limit` refusal names each held pre-lifecycle member and what frees it (p1c-calls 4, 10).
//! Task 7: an unmarked folder holding gens/1 evidence is never read past its record by the orphan fence, adoption or
//! `list`, and stays byte-identical (spec "Exclusion", p1c-calls 9).
use serde_json::json;

use crate::agents::core_fixture as cf;
use crate::agents::fixture::World;
use crate::agents::prelife_fixture::{self as pf, A, Answer, C, D, L, P};
use crate::agents::team_state::{Scope, open};

/// Outside a team nothing changes for 0.2.x folders: scope Outside; the 0.2 cap still counts hc members only. A
/// stop of a live one proven its own closes its pane and notices it `stopped` (all-calls 14); a resume never kills,
/// launches or writes.
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
    // all-calls 14: `stop` ends a live 0.2.x pane proven its own (an identity-checked kill) and notices it `stopped`.
    pf::v028_pane(&w, &ids[0], pf::Pane028::Live);
    let (code, out) = crate::cli::run(&["stop".to_string(), "old0".to_string()], || Ok(&w.ctx));
    assert_eq!(code, 0, "{out}");
    assert_eq!(pf::pane_kills(&w).len(), 1, "the pane is closed once");
    assert_eq!(pf::notice_kind(&w, &ids[0]).as_deref(), Some("stopped"));
    let stopped = w.folder(&ids[0]).join("notice.json");
    let rel = stopped.strip_prefix(w.state().join("agents")).unwrap();
    let stopped_bytes = pf::bytes(&w.state().join("agents"));
    let mut after = stopped_bytes.clone();
    after.remove(rel);
    assert_eq!(after, before, "only the stopped member's notice is new");
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
    assert_eq!(
        pf::bytes(&w.state().join("agents")),
        stopped_bytes,
        "resume writes nothing"
    );
    w.fakes.hc_done().unwrap();
}

/// All-calls 14: a person's `stop` of a 0.2.x pane-based member whose pane is gone kills nothing and posts the
/// `stopped` notice; one whose pane runs another command, or whose multiplexer cannot be read, is refused 2 with no
/// kill and no notice.
#[test]
fn a_v028_stop_closes_only_a_proven_pane_and_fails_closed() {
    for (pane, ok) in [
        (pf::Pane028::Gone, true),
        (pf::Pane028::Other, false),
        (pf::Pane028::Unreachable, false),
    ] {
        let w = World::human("");
        pf::v028(&w, P, "old", pf::End::Live);
        pf::v028_pane(&w, P, pane);
        let before = pf::bytes(&w.folder(P));
        let (code, out) = crate::cli::run(&["stop".to_string(), "old".to_string()], || Ok(&w.ctx));
        assert!(pf::pane_kills(&w).is_empty(), "{pane:?}: nothing killed");
        if ok {
            assert_eq!(code, 0, "{pane:?}: {out}");
            assert_eq!(pf::notice_kind(&w, P).as_deref(), Some("stopped"));
            assert_eq!(pf::delivered(&w, "%1", &format!("stop-{P}")), 1, "{pane:?}");
        } else {
            assert_eq!(code, 2, "{pane:?}: {out}");
            assert_eq!(
                pf::bytes(&w.folder(P)),
                before,
                "{pane:?}: no notice, no write"
            );
        }
    }
}

/// All-calls 14, F2: in a team, a v0.2.8 member recorded `lost` holds its slot (`limit`); while the multiplexer
/// cannot be read `stop` refuses and it stays held; once its pane is gone `stop` notices it `stopped` and the slot
/// frees, so the spawn is admitted.
#[test]
fn a_lost_v028_member_in_a_team_is_freed_by_stop() {
    let w = World::human("");
    let answers = pf::capacity_world(&w);
    pf::v028(&w, P, "legacy", pf::End::Lost);
    pf::script(&w, answers.clone());
    assert_eq!(pf::admit_spawn(&w).map(|_| ()).unwrap_err().check, "limit");
    w.fakes.hc_done().unwrap();
    pf::v028_pane(&w, P, pf::Pane028::Unreachable);
    let (code, out) = crate::cli::run(&["stop".to_string(), "legacy".to_string()], || Ok(&w.ctx));
    // Fail closed: in a team the stop is refused before it names its target (1 `rmux`); nothing is noticed.
    assert_ne!(code, 0, "{out}");
    assert_eq!(
        pf::notice_kind(&w, P).as_deref(),
        Some("lost"),
        "still held"
    );
    assert!(pf::pane_kills(&w).is_empty());
    pf::v028_pane(&w, P, pf::Pane028::Gone);
    let (code, out) = crate::cli::run(&["stop".to_string(), "legacy".to_string()], || Ok(&w.ctx));
    assert_eq!(code, 0, "{out}");
    assert!(pf::pane_kills(&w).is_empty());
    assert_eq!(pf::notice_kind(&w, P).as_deref(), Some("stopped"));
    pf::script(&w, answers);
    pf::admit_spawn(&w).expect("the stopped member frees its slot");
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

/// Task 7: a pre-lifecycle member whose gens/1 holds every file a wrong reader would open.
fn unmarked(w: &World, id: &str, parent: &str, depth: u64) -> serde_json::Value {
    let r = pf::pre(w, id, pf::fields(id, "legacy", Some(parent), depth));
    pf::dev_evidence(w, id);
    pf::publish(w, id, 1, "spec", json!({"sessionId": r["sessionId"]}));
    pf::ended(w, id);
    r
}

/// G8: `Tx::open` runs the orphan fence; nothing of the unmarked folder past its record is read.
#[test]
fn the_orphan_fence_never_reads_an_unmarked_folder() {
    let w = World::human("");
    pf::team(&w);
    pf::life(&w, L, pf::fields(L, "leader", None, 0));
    unmarked(&w, P, L, 1);
    let before = pf::bytes(&w.folder(P));
    let (got, reads) =
        crate::state::fs::record_reads(|| open(&w.ctx, &Scope::of(&w.caller()), |_| Ok(())));
    got.unwrap();
    assert_eq!(
        pf::reads_past_record(&reads, &w.folder(P)),
        Vec::<std::path::PathBuf>::new()
    );
    assert_eq!(pf::bytes(&w.folder(P)), before);
    assert!(
        w.fakes.hc_calls().is_empty(),
        "no lifecycle orphan, so no hc call"
    );
}

/// H3, H4.4 (O3 ambiguity 11): no record reservation and no read of its evidence; its live Claude, registered as
/// a client by the fake OS, holds as unmanaged. Checklist 4: reads are recorded around both queries.
#[test]
fn an_unmarked_folder_reserves_nothing_but_its_live_client_holds() {
    use crate::agents::occupancy::{Rule, held_on};
    use crate::parse::Client;
    use crate::state::root::OsKind;
    for os in [OsKind::Unix, OsKind::Windows] {
        let w = World::human("");
        pf::team(&w);
        let r = unmarked(&w, P, L, 1);
        let session = r["sessionId"].as_str().unwrap();
        let before = pf::bytes(&w.folder(P));
        let (none, reads) =
            crate::state::fs::record_reads(|| held_on(&w.ctx, session, Client::Claude, os, None));
        assert_eq!(
            pf::reads_past_record(&reads, &w.folder(P)),
            Vec::<std::path::PathBuf>::new(),
            "{os:?}: no reservation read"
        );
        assert!(none.is_none(), "{os:?}: no reservation");
        let mut row = crate::testing::row(4242, 1, 1500);
        let exe = if matches!(os, OsKind::Windows) {
            "claude.exe"
        } else {
            "claude"
        };
        (row.path, row.argv, row.image) = (
            Some(format!("/opt/{exe}")),
            Some(vec![exe.into()]),
            Some(exe.into()),
        );
        w.fakes.process(row);
        w.fakes.client(4242, Client::Claude);
        let (h, reads) =
            crate::state::fs::record_reads(|| held_on(&w.ctx, session, Client::Claude, os, None));
        assert_eq!(
            pf::reads_past_record(&reads, &w.folder(P)),
            Vec::<std::path::PathBuf>::new(),
            "{os:?}: no read for the live client either"
        );
        let h = h.expect("its live client holds");
        assert_eq!((h.rule, h.pid), (Rule::Unmanaged, Some(4242)), "{os:?}");
        assert_eq!(pf::bytes(&w.folder(P)), before, "{os:?}");
    }
}

/// adopt-3e under contracts 1-3: a pre-lifecycle member is never a source, an adopter or a moved child, nothing of
/// it past its record is read, and a chain that leaves the tree at it adopts nothing (checklist 5: also when a live
/// adopter sits below it). No adoption journal.
#[test]
fn adoption_never_reads_or_moves_a_pre_lifecycle_member() {
    const B: &str = "b0000000000b";
    for case in [
        "pre child of an ended source",
        "pre source",
        "chain leaves the tree",
        "leaves above a live adopter",
    ] {
        let w = World::human("");
        pf::team(&w);
        pf::life(&w, L, pf::fields(L, "leader", None, 0));
        match case {
            "pre child of an ended source" => {
                pf::life(&w, A, pf::fields(A, "w-a", Some(L), 1));
                pf::ended(&w, A);
                unmarked(&w, P, A, 2);
            }
            "pre source" => {
                unmarked(&w, P, L, 1);
                pf::life(&w, C, pf::fields(C, "w-c", Some(P), 2));
            }
            "chain leaves the tree" => {
                unmarked(&w, P, L, 1);
                pf::life(&w, A, pf::fields(A, "w-a", Some(P), 2));
                pf::ended(&w, A);
                pf::life(&w, D, pf::fields(D, "w-d", Some(A), 3));
            }
            _ => {
                unmarked(&w, P, L, 1);
                pf::life(&w, B, pf::fields(B, "w-b", Some(P), 2));
                pf::life(&w, A, pf::fields(A, "w-a", Some(B), 3));
                pf::ended(&w, A);
                pf::life(&w, D, pf::fields(D, "w-d", Some(A), 4));
            }
        }
        let agents = w.state().join("agents");
        let before = pf::bytes(&agents);
        let (got, reads) = crate::state::fs::record_reads(|| {
            open(&w.ctx, &Scope::of(&w.caller()), |tx| {
                crate::agents::adopt::adopt_locked(tx, &w.caller()).map(|_| ())
            })
        });
        got.unwrap_or_else(|e| panic!("{case}: {e:?}"));
        assert_eq!(
            pf::reads_past_record(&reads, &w.folder(P)),
            Vec::<std::path::PathBuf>::new(),
            "{case}"
        );
        assert_eq!(pf::bytes(&agents), before, "{case}");
        let journals = std::fs::read_dir(w.state().join("teams")).map_or(0, |d| {
            d.filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".adopt")
            })
            .count()
        });
        assert_eq!(journals, 0, "{case}: no adoption journal folder");
    }
}

/// Spec "mixed tree ... `list` behaves as today", through the public `list` verb: C's row (found by its emitted
/// `hcAddress`: `list` rows carry no `id`, checklist 1) is emitted as main emits it; C's folder is byte-identical.
#[test]
fn list_shows_a_pre_lifecycle_child_of_a_lifecycle_member_as_today() {
    let w = World::human("");
    w.panes(&[]);
    let (a, c) = pf::mixed(&w);
    let leader: serde_json::Value = crate::state::fs::read_json(&w.folder(L).join("record.json"))
        .unwrap()
        .unwrap();
    let answers = [&leader, &a, &c]
        .iter()
        .map(|r| {
            (
                r["hcAddress"].as_str().unwrap().to_string(),
                Answer::Wire(cf::wire(r)),
            )
        })
        .collect();
    pf::script(&w, answers);
    let before = pf::bytes(&w.folder(C));
    let (code, out) = crate::cli::run(&["list".to_string()], || Ok(&w.ctx));
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let row = v["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["hcAddress"] == c["hcAddress"])
        .unwrap_or_else(|| panic!("no row for C: {out}"));
    assert_eq!(row["name"], "w-c", "{out}");
    assert_eq!(pf::bytes(&w.folder(C)), before);
    w.fakes.hc_done().unwrap();
}

/// After the upgrade, `list` (the bar's feed too) shows a live 0.2.x pane-based member as 0.2.8 did, from its
/// record and its pane proven its launch; nothing is killed or written. A gone pane, or one running another command,
/// or a multiplexer that cannot be read, shows no running row.
#[test]
fn a_live_v028_pane_member_is_listed_after_the_upgrade() {
    for (pane, shown) in [
        (pf::Pane028::Live, true),
        (pf::Pane028::Gone, false),
        (pf::Pane028::Other, false),
        (pf::Pane028::Unreachable, false),
    ] {
        let w = World::human("");
        pf::v028(&w, P, "old", pf::End::Live);
        pf::v028_pane(&w, P, pane);
        pf::script(&w, Default::default());
        let before = pf::bytes(&w.state().join("agents"));
        let (code, out) = crate::cli::run(&["list".to_string()], || Ok(&w.ctx));
        assert_eq!(code, 0, "{pane:?}: {out}");
        let reply: serde_json::Value = serde_json::from_str(&out).unwrap();
        let names: Vec<&str> = reply["agents"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["state"] == "running")
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        if shown {
            assert_eq!(
                (reply["running"].as_u64(), names),
                (Some(1), vec!["old"]),
                "{pane:?}: {out}"
            );
            let row = &reply["agents"][0];
            assert_eq!(
                (&row["pane"], &row["client"]),
                (&json!("%9"), &json!("claude")),
                "{out}"
            );
        } else {
            assert_eq!(
                (reply["running"].as_u64(), names.len()),
                (Some(0), 0),
                "{pane:?}: {out}"
            );
        }
        assert!(
            pf::pane_kills(&w).is_empty(),
            "{pane:?}: list kills nothing"
        );
        assert_eq!(
            pf::bytes(&w.state().join("agents")),
            before,
            "{pane:?}: list writes nothing"
        );
    }
}
