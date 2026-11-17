//! prelife-1c G10a (calls 13-15, 17, 21-23): a pre-lifecycle folder holds one team slot until it is proven ended.
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::agents::core_fixture as cf;
use crate::agents::fixture::{ID, World, write};
use crate::agents::membership::{Snapshot, locals, team};
use crate::agents::prelife_fixture::{self as pf, Answer, L, P};

/// Checklist 17: a reader that opens a FIFO blocks until a writer comes; after 2 s this watchdog opens it to write
/// (non-blocking), so a regression ends with a torn read and the elapsed-time assertion fails instead of hanging.
struct Unblock {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    dog: Option<std::thread::JoinHandle<()>>,
    begun: Instant,
}

impl Unblock {
    fn new(at: &Path) -> Unblock {
        use std::sync::atomic::Ordering;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (done, fifo, begun) = (stop.clone(), at.to_path_buf(), Instant::now());
        let dog = std::thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                if begun.elapsed() > Duration::from_secs(2) {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        let _ = std::fs::OpenOptions::new()
                            .write(true)
                            .custom_flags(libc::O_NONBLOCK)
                            .open(&fifo);
                    }
                    #[cfg(not(unix))]
                    let _ = &fifo;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        Unblock {
            stop,
            dog: Some(dog),
            begun,
        }
    }

    /// Stops the watchdog; true when the reader never waited for it.
    fn quick(mut self) -> bool {
        let took = self.begun.elapsed();
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.dog.take().unwrap().join().unwrap();
        took < Duration::from_secs(2)
    }
}

/// The capacity matrix, through spawn's real team admission: five lifecycle members, one pre-lifecycle folder `P`,
/// then a spawn. Held cases refuse `limit`; proven-ended or never-ran cases admit.
#[test]
fn a_pre_lifecycle_folder_holds_its_slot_until_it_is_proven_ended() {
    let held = [
        "partial",
        "no session",
        "not an object",
        "pane era",
        "missing",
        "lost",
        "unreachable",
        "dev pre-run",
        "lost notice",
        "torn notice",
        "null abort",
        "gen-1 claim",
        "invalid generation",
        "dev abort, kill pending",
        "dev abort, other identity",
    ];
    let freed = [
        "exited",
        "killed",
        "stopped notice, lost",
        "exited notice",
        "abandoned claim",
        "ran nothing",
        "dev abort",
        "other session",
        "pane era, stopped notice",
        "none",
    ];
    for case in held.iter().chain(freed.iter()) {
        let w = World::human("");
        let mut answers = pf::capacity_world(&w);
        let f = pf::fields(P, "legacy", Some(L), 1);
        let address = format!("t{P}");
        match *case {
            "none" => {}
            "partial" => drop(pf::partial(&w, P, f)),
            "dev abort" | "dev abort, kill pending" | "dev abort, other identity" => {
                // call 21: v0.3-dev's rollback abort, after hc run named the address; no usable identity is left
                let r = pf::partial(&w, P, f);
                std::fs::remove_file(w.folder(P).join("commit.json")).unwrap();
                let mut identity = r.clone();
                if *case == "dev abort, other identity" {
                    identity["id"] = json!("e0000000000e");
                }
                let kill = if *case == "dev abort, kill pending" {
                    "pending"
                } else {
                    "acknowledged"
                };
                write(
                    &w.folder(P).join("abort.json"),
                    json!({"hcAddress": address, "identity": identity, "pane": null, "kill": kill,
                        "viewCleanup": "unproven"}),
                );
            }
            "no session"
            | "not an object"
            | "pane era"
            | "lost notice"
            | "torn notice"
            | "ran nothing"
            | "null abort"
            | "other session"
            | "invalid generation"
            | "pane era, stopped notice" => {
                let folder = w.agent_record(P, json!({"name": "legacy", "session": "$3"}));
                match *case {
                    "no session" => write(
                        &folder.join("record.json"),
                        json!({"id": P, "name": "legacy"}),
                    ),
                    "not an object" => write(&folder.join("record.json"), json!(["legacy"])),
                    "other session" => write(
                        &folder.join("record.json"),
                        json!({"id": P, "session": "$4"}),
                    ),
                    "lost notice" => write(&folder.join("notice.json"), json!({"kind": "lost"})),
                    // G10a: no usable identity, freed by its folder notice alone (row 2.3)
                    "pane era, stopped notice" => write(
                        &folder.join("notice.json"),
                        json!({"kind": "stopped", "exitCode": null, "at": "x"}),
                    ),
                    "torn notice" => {
                        std::fs::write(folder.join("notice.json"), b"{\"kind\":\"sto").unwrap()
                    }
                    "ran nothing" => {
                        std::fs::remove_file(folder.join("commit.json")).unwrap();
                        write(
                            &folder.join("abort.json"),
                            json!({"abortedAt": "x", "pane": null}),
                        );
                    }
                    "null abort" => {
                        // call 15: `null` is no writer's abort shape, so it proves nothing
                        std::fs::remove_file(folder.join("commit.json")).unwrap();
                        write(&folder.join("abort.json"), Value::Null);
                    }
                    "invalid generation" => {
                        // call 21: a present but invalid generation proves nothing, not even gens/0's abandoned claim
                        std::fs::remove_file(folder.join("commit.json")).unwrap();
                        let mut r: Value = crate::state::fs::read_json(&folder.join("record.json"))
                            .unwrap()
                            .unwrap();
                        r["generation"] = json!("01");
                        write(&folder.join("record.json"), r);
                        std::fs::create_dir_all(folder.join("gens")).unwrap();
                        write(&folder.join("gens").join("0.claim"), json!("abandoned"));
                    }
                    _ => {} // pane era: v0.2.x fields only, committed, no notice
                }
            }
            "dev pre-run" | "abandoned claim" | "gen-1 claim" => {
                let mut r = f.clone();
                (r["resumedFrom"], r["sessionId"]) = (
                    json!("e0000000000e"),
                    json!("33333333-3333-4333-8333-333333333333"),
                );
                let folder = w.uncommitted(P, r);
                std::fs::create_dir_all(folder.join("gens")).unwrap();
                if *case == "gen-1 claim" {
                    // call 15: the record names generation 1; an abandoned gens/0 claim proves nothing about it
                    let mut r: Value = crate::state::fs::read_json(&folder.join("record.json"))
                        .unwrap()
                        .unwrap();
                    r["generation"] = json!("1");
                    write(&folder.join("record.json"), r);
                }
                let claim = if *case == "dev pre-run" {
                    "run"
                } else {
                    "abandoned"
                };
                write(&folder.join("gens").join("0.claim"), json!(claim));
            }
            _ => {
                let r = pf::pre(&w, P, f);
                let mut t = cf::wire(&r);
                let lost = crate::agents::life_fixture::LOST_ARGVS[0];
                let answer = match *case {
                    "missing" => Answer::Missing,
                    "unreachable" => Answer::Fail,
                    "lost" => {
                        t["argv"] = json!(lost);
                        Answer::Wire(t)
                    }
                    "stopped notice, lost" => {
                        t["argv"] = json!(lost);
                        write(
                            &w.folder(P).join("notice.json"),
                            json!({"kind": "stopped", "at": "x"}),
                        );
                        Answer::Wire(t)
                    }
                    "exited notice" => {
                        write(
                            &w.folder(P).join("notice.json"),
                            json!({"kind": "exited", "exitCode": 0}),
                        );
                        Answer::Missing
                    }
                    _ => {
                        let reason = if *case == "exited" {
                            "exited"
                        } else {
                            "killed"
                        };
                        (t["state"], t["reason"]) = (json!("finished"), json!(reason));
                        Answer::Wire(t)
                    }
                };
                answers.insert(address, answer);
            }
        }
        pf::script(&w, answers);
        let got = pf::admit_spawn(&w);
        if held.contains(case) {
            let e = got.unwrap_err();
            assert!(
                e.check == "limit"
                    && e.message
                        .starts_with("the team already has 6 members besides the leader"),
                "{case}: {e:?}"
            );
        } else {
            got.unwrap_or_else(|e| panic!("{case}: {e:?}"));
        }
        w.fakes.hc_done().unwrap();
    }
}

/// G10a: a pre-lifecycle record membership cannot read as an identity holds one slot and never refuses the
/// snapshot; a readable member beside it is listed as before.
#[test]
fn an_unidentified_record_holds_a_slot_and_never_refuses() {
    let other = "000000000b07";
    for case in [
        "not an object",
        "another id",
        "no name",
        "no session",
        "non-string session",
        "partial hc",
    ] {
        let w = World::human("");
        let r = cf::member(&w, ID, json!({}));
        let mut bad = cf::member(&w, other, json!({"name": "w"}));
        let path = w.folder(other).join("record.json");
        match case {
            "not an object" => std::fs::write(&path, "[\"w\"]").unwrap(),
            _ => {
                match case {
                    "another id" => bad["id"] = "000000000b06".into(),
                    "no name" => bad["name"] = Value::Null,
                    "no session" => drop(bad.as_object_mut().unwrap().remove("session")),
                    "partial hc" => drop(bad.as_object_mut().unwrap().remove("launcherArgv")),
                    _ => bad["session"] = 3.into(),
                }
                write(&path, bad);
            }
        }
        cf::online(&w);
        cf::listing(&w, &team(&w.caller()), &[]);
        cf::get(&w, r["hcAddress"].as_str().unwrap(), cf::wire(&r));
        let core = crate::hc::connect(&w.ctx).unwrap();
        let s = Snapshot::for_spawn(&w.ctx, &w.caller(), &core)
            .unwrap_or_else(|e| panic!("{case}: {e:?}"));
        assert_eq!(s.held, [other], "{case}: one held slot");
        assert!(
            s.records.contains_key(ID) && !s.records.contains_key(other),
            "{case}"
        );
        assert_eq!(
            s.slots(),
            2,
            "{case}: the readable member and the held folder"
        );
        w.fakes.hc_done().unwrap();
    }
}

/// Call 13: a held folder whose record names an `hcAddress` hc lists counts once (the folder, not also a
/// reservation), with a good or a failing `get` of that terminal. Four children + the folder = 5: admitted.
#[test]
fn a_held_folder_whose_terminal_hc_lists_counts_once() {
    for good in [true, false] {
        let w = World::human("");
        let mut answers = pf::capacity_world_of(&w, 4);
        let r = pf::partial(&w, P, pf::fields(P, "legacy", Some(L), 1));
        let address = format!("t{P}");
        answers.insert(
            address.clone(),
            if good {
                Answer::Wire(cf::wire(&r))
            } else {
                Answer::Fail
            },
        );
        pf::script_listed(&w, &[&address], answers);
        pf::admit_spawn(&w).unwrap_or_else(|e| panic!("good get {good}: {e:?}"));
        w.fakes.hc_done().unwrap();
    }
}

/// Call 23: a pre-lifecycle folder freed by its `stopped` notice still names its address, so its listed (Lost)
/// terminal is never counted as another root's reservation. Five children + nothing = admitted.
#[test]
fn a_freed_pre_lifecycle_folder_still_suppresses_its_address() {
    let w = World::human("");
    let mut answers = pf::capacity_world(&w);
    let r = pf::partial(&w, P, pf::fields(P, "legacy", Some(L), 1));
    write(
        &w.folder(P).join("notice.json"),
        json!({"kind": "stopped", "exitCode": null, "at": "x"}),
    );
    let address = format!("t{P}");
    let mut t = cf::wire(&r);
    t["argv"] = json!(crate::agents::life_fixture::LOST_ARGVS[0]);
    answers.insert(address.clone(), Answer::Wire(t));
    pf::script_listed(&w, &[&address], answers);
    pf::admit_spawn(&w).expect("freed, and its listed terminal is not counted again");
    w.fakes.hc_done().unwrap();
}

/// Call 14: a FIFO (Unix) or a link at `gens/0.claim`, `abort.json` or `notice.json` of a held folder is
/// Unreadable through the walk and keeps it held; it never blocks and never frees through the link's target.
#[test]
fn a_fifo_or_link_at_held_evidence_keeps_the_folder_held() {
    for file in ["gens/0.claim", "abort.json", "notice.json"] {
        for kind in ["link", "fifo"] {
            if kind == "fifo" && cfg!(not(unix)) {
                continue;
            }
            let w = World::human("");
            let answers = pf::capacity_world(&w);
            let folder = w.agent_record(P, json!({"name": "legacy"})); // pane era: held
            std::fs::remove_file(folder.join("commit.json")).unwrap();
            std::fs::create_dir_all(folder.join("gens")).unwrap();
            let at = folder.join(file);
            if kind == "link" {
                // the target would free it if followed: an abandoned claim, a writer-shaped abort, a stopped notice
                let target = w.state().join(format!("target-{}", file.replace('/', "-")));
                let body = match file {
                    "gens/0.claim" => json!("abandoned"),
                    "abort.json" => json!({"abortedAt": "x", "pane": null}),
                    _ => json!({"kind": "stopped"}),
                };
                write(&target, body);
                if let Err(e) = crate::testing::link_file(&target, &at) {
                    eprintln!("{file} link: skipped, no file link: {e}");
                    continue;
                }
            } else {
                #[cfg(unix)]
                crate::testing::fifo(&at);
            }
            pf::script(&w, answers);
            let dog = Unblock::new(&at);
            let e = pf::admit_spawn(&w).unwrap_err();
            // The 2 s bound proves no blocking only at a FIFO (Unix). A link cannot block, so there the bound would
            // measure only wall-clock time under load (coordinator note, restore-6b: flaky on Windows); the link
            // case is held by `e.check` alone.
            let quick = dog.quick();
            if kind == "fifo" {
                assert!(quick, "{file} {kind}: the read blocked");
            }
            assert_eq!(e.check, "limit", "{file} {kind}: held");
            w.fakes.hc_done().unwrap();
        }
    }
}

/// Call 14: a resumed successor's `gens/0.claim` is read through the walk. A link (here to an `abandoned` claim,
/// which would make it no member if followed) is Unreadable, so claimed: the successor stays a local member whose
/// life comes from hc (row 2.22).
#[test]
fn a_linked_successor_claim_is_claimed() {
    let w = World::human("");
    let mut f = pf::fields(P, "legacy", Some(L), 1);
    (f["resumedFrom"], f["sessionId"]) = (
        json!("e0000000000e"),
        json!("33333333-3333-4333-8333-333333333333"),
    );
    let folder = w.uncommitted(P, f);
    std::fs::create_dir_all(folder.join("gens")).unwrap();
    let target = w.state().join("target-claim");
    write(&target, json!("abandoned"));
    if let Err(e) = crate::testing::link_file(&target, &folder.join("gens").join("0.claim")) {
        eprintln!("skipped, no file link: {e}");
        return;
    }
    let local = locals(&w.ctx, &w.caller()).unwrap();
    let ids: Vec<&str> = local.iter().map(|l| l.identity.id.as_str()).collect();
    assert_eq!(
        ids,
        [P],
        "claimed through the walk, never abandoned through the link"
    );
}

/// Call 22: on the readable-identity branch `commit.json` is read through the walk too: a link (to a whole commit)
/// or a FIFO is not committed, and the scan never blocks.
#[test]
fn a_linked_or_fifo_commit_is_not_committed() {
    for kind in ["link", "fifo"] {
        if kind == "fifo" && cfg!(not(unix)) {
            continue;
        }
        let w = World::human("");
        cf::member(&w, ID, json!({}));
        let at = w.folder(ID).join("commit.json");
        std::fs::remove_file(&at).unwrap();
        if kind == "link" {
            let target = w.state().join("target-commit.json");
            write(&target, json!({"committedAt": "x"}));
            if let Err(e) = crate::testing::link_file(&target, &at) {
                eprintln!("skipped, no file link: {e}");
                continue;
            }
        } else {
            #[cfg(unix)]
            crate::testing::fifo(&at);
        }
        let dog = Unblock::new(&at);
        let local = locals(&w.ctx, &w.caller()).unwrap();
        assert!(dog.quick(), "{kind}: the read blocked");
        assert_eq!(local.len(), 1, "{kind}");
        assert!(
            !local[0].stored.committed,
            "{kind}: never committed through a {kind}"
        );
    }
}
