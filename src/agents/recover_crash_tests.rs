//! restore-6c Task 4: recovery replays its own crash files; a core loss after a fresh commit restores the
//! fresh conversation; repeated and concurrent recoveries launch one successor.
use super::core_fixture as cf;
use super::core_launch::crash;
use super::core_launch::forward_tests as ft;
use super::fixture::World;
use super::reconcile_restore_tests::L;
use super::reconcile_tests::{X, found, member, ok, wire_of};
use super::reconcile_verb_tests::{person, team};
use super::recover_tests::{both_gone, child, cli, failed_ended, spec};
use super::resume::{Action, recover};
use super::{gens, life_fixture as lf};
use crate::state::evidence::publish;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Removes a killed child's temporary folder (its own destructors never ran) on every exit path.
pub(crate) struct ChildDir(PathBuf);
impl ChildDir {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self(dir)
    }
}
impl Drop for ChildDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The person world on a given state root and folder (a crash child's, or another thread's), with real locks
/// beside the state root, so the threads and crash children of one world share them.
pub(crate) fn person_at(root: &Path, cwd: &Path) -> World {
    let mut w = person();
    (w.ctx.state_root, w.ctx.cwd) = (root.into(), cwd.into());
    w.fakes.real_locks(
        &root
            .parent()
            .expect("a state root has a parent")
            .join("locks"),
    );
    w
}

/// hc's terminal of generation n as recorded: `launch.json`'s `hcName` and `launcherArgv` (or `argv`), the address
/// the record's pending (or active) generation names.
pub(crate) fn recorded_wire(w: &World, f: &Path, n: u64, argv: Option<&[&str]>) -> Value {
    let l = gens::read_flushed(f, n, "launch").unwrap().unwrap();
    let r = lf::record(f);
    let address = r["pending"]["hcAddress"]
        .as_str()
        .or(r["hcAddress"].as_str())
        .unwrap()
        .to_string();
    let id = f
        .file_name()
        .and_then(|s| s.to_str())
        .expect("a member folder is its id");
    let mut wire = wire_of(w, id, n, &[]);
    (wire["address"], wire["name"]) = (json!(address), l["hcName"].clone());
    wire["argv"] = argv.map_or(l["launcherArgv"].clone(), |a| json!(a));
    wire
}

fn args(action: Action) -> &'static [&'static str] {
    match action {
        Action::Retry => &["resume", X],
        Action::Fresh => &["resume", X, "-Fresh"],
    }
}

/// A crash child: X under L failed and ended; `resume X` (or `-Fresh`), killed (exit 86) at `MUX_AGENTS_CRASH`.
fn crash_child(action: Action) {
    let w = person();
    team(&w);
    child(&w);
    both_gone(&w);
    w.fakes.hc_answer(ft::address("t2"));
    println!("CRASH-ROOT {}", w.ctx.state_root.display());
    println!("CRASH-CWD {}", w.ctx.cwd.display());
    println!("CRASH-TEMP {}", w._dir.path().display());
    let _ = cli(&w, args(action));
}
#[test]
#[ignore = "a child of recovery_replays_its_own_crash_files_before_any_restore"]
fn child_fresh() {
    crash_child(Action::Fresh);
}
#[test]
#[ignore = "a child of recovery_replays_its_own_crash_files_before_any_restore"]
fn child_retry() {
    crash_child(Action::Retry);
}

/// Spec "fresh crash windows", calls 16, 25 (1-4 F8): a retry or fresh start killed after `pending`, `spec`, `run`
/// (no launcher claim: void), `run` with the launcher's claim, or `commit` (claimed: promoted). The next recovery
/// replays those files itself, before any `restore`. Uncommitted: void, the record keeps the predecessor's
/// conversation, and the rerun launches generation 3 (retry: the same conversation; fresh: a new one). Committed:
/// promoted, and a second recovery refuses `state`. Never two successors.
#[test]
fn recovery_replays_its_own_crash_files_before_any_restore() {
    for action in [Action::Fresh, Action::Retry] {
        let name = match action {
            Action::Fresh => "agents::recover_crash_tests::child_fresh",
            Action::Retry => "agents::recover_crash_tests::child_retry",
        };
        for (step, claim) in [
            ("pending", false),
            ("spec", false),
            ("run", false),
            ("run", true),
            ("commit", true),
        ] {
            let at = format!("{action:?} {step}/{claim}");
            let kv = crash::killed_at(name, step);
            let _tmp = ChildDir(crash::printed(&kv, "TEMP"));
            let w = person_at(&crash::printed(&kv, "ROOT"), &crash::printed(&kv, "CWD"));
            let f = w
                .ctx
                .state_root
                .join(w.caller().server_key)
                .join("agents")
                .join(X);
            let pending = lf::record(&f)["pending"]["sessionId"]
                .as_str()
                .unwrap()
                .to_string();
            assert_eq!(
                lf::record(&f)["sessionId"],
                json!(lf::SID),
                "{at}: the predecessor's session"
            );
            if claim {
                publish(&gens::claim_path(&f, 2), &json!("run")).unwrap();
            }
            if step == "commit" {
                found(&w, X, 2, &recorded_wire(&w, &f, 2, None)); // settle: Running by its identity -> promote
                cf::online(&w); // recover's Lazy: observe asks hc about generation 2 (not done, undecided)
                found(&w, X, 2, &recorded_wire(&w, &f, 2, None));
                let (code, again) = cli(&w, args(action));
                assert_eq!(
                    (code, &again["check"]),
                    (2, &json!("state")),
                    "{at}: {again}"
                );
                let r = lf::record(&f);
                assert_eq!(
                    (&r["generation"], &r["sessionId"]),
                    (&json!("2"), &json!(pending)),
                    "{at}: promoted"
                );
                assert!(spec(&f, 3).is_none(), "{at}: never two successors");
                w.fakes.hc_done().unwrap();
                continue;
            }
            both_gone(&w);
            w.fakes.hc_answer(ft::address("t3"));
            let (code, again) = cli(&w, args(action));
            assert_eq!(
                (code, &again["agent"]["generation"]),
                (0, &json!(3)),
                "{at}: {again}"
            );
            let sid = again["agent"]["sessionId"].clone();
            match action {
                Action::Retry => assert_eq!(sid, json!(lf::SID), "{at}: the same conversation"),
                Action::Fresh => assert!(
                    sid != json!(pending) && sid != json!(lf::SID),
                    "{at}: a new uuid"
                ),
            }
            assert!(
                gens::done(&f, 2).unwrap() && !gens::path(&f, 2, "ended").exists(),
                "{at}: void"
            );
            w.fakes.hc_done().unwrap();
        }
    }
}

/// Spec "fresh, ready, committed, then core loss": the fresh generation F reported ready and committed; the core
/// died before promotion. `restore`: settle promotes n+1 (the record names F), releases it `cause: restore`, then
/// relaunches n+2 resuming F; no task resent.
#[test]
fn a_committed_fresh_generation_lost_before_promotion_is_restored_from_its_session() {
    let fresh = "0f0f0f0f-2032-48ef-a920-0f0f0f0f0f0f";
    for argv in lf::LOST_ARGVS {
        let w = person();
        team(&w);
        let f = member(&w, X, json!({"parentAgent": {"id": L}}));
        failed_ended(&f, 1);
        lf::seed_gen(&f, 2, Some("run"), lf::committed());
        let l = gens::path(&f, 2, "launch");
        std::fs::remove_file(&l).unwrap();
        publish(
            &l,
            &json!({"hcName": lf::hc_name(2), "launcherArgv": lf::ARGV, "sessionId": fresh}),
        )
        .unwrap();
        ok(&f, 2); // ready and adjudicated ok before the loss
        let mut r = lf::record(&f);
        r["pending"] = json!({"gen": 2, "kind": "fresh", "hcName": lf::hc_name(2), "sessionId": fresh,
            "launcherArgv": lf::ARGV, "hcAddress": "t2"});
        crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
        let lost = recorded_wire(&w, &f, 2, Some(argv));
        // discovery takes `person()`'s `--version`; settle's own Lazy: row 6 Lost -> promote, then R1 (reread, relabel)
        cf::online(&w);
        found(&w, X, 2, &lost);
        cf::answer(&w, &["get", "t2", "--json"], &lost.to_string());
        lf::relabel(&w, "t2");
        cf::online(&w); // the pass's Lazy: row 0's cap, then n+2
        super::reconcile_await_tests::snapshot(&w, &[], &[&format!("a{L}"), "t2"]);
        w.fakes.hc_answer(ft::address("t3"));
        let (code, reply) = cli(&w, &["restore"]);
        assert_eq!(code, 0, "{argv:?} {reply}");
        assert_eq!(
            reply["restored"],
            json!([{"id": X, "name": "mbbb", "generation": 3, "sessionId": fresh}]),
            "{argv:?}: {reply}"
        );
        assert_eq!(
            spec(&f, 3).unwrap()["args"],
            json!(["--resume", fresh]),
            "{argv:?}: resumes F, no task"
        );
        w.fakes.hc_done().unwrap();
    }
}

/// Call 16: a repeated recovery after the successor was promoted decides from the successor (Running, undecided)
/// and refuses `state`; nothing launches twice.
#[test]
fn a_repeated_recovery_launches_no_second_successor() {
    for action in [Action::Retry, Action::Fresh] {
        let w = person();
        team(&w);
        let f = child(&w);
        both_gone(&w);
        w.fakes.hc_answer(ft::address("t2"));
        assert_eq!(cli(&w, args(action)).0, 0, "{action:?}: first");
        cf::online(&w); // the second recover's Lazy: observe asks about generation 2
        found(&w, X, 2, &recorded_wire(&w, &f, 2, None));
        let (code, again) = cli(&w, args(action));
        assert_eq!(
            (code, &again["check"]),
            (2, &json!("state")),
            "{action:?}: {again}"
        );
        assert_eq!(lf::count(&w, "run"), 1, "{action:?}: one successor");
        w.fakes.hc_done().unwrap();
    }
}

/// Call 16, 1-4 F9, checklist 15: two concurrent recoveries of X, for every action pair. A pauses after its
/// promotion holding `up|T`; B is proven waiting (the lock-retry hook) and its first lock is `up|T`; B then decides
/// from A's successor and refuses `state`. One `hc run` across both.
#[test]
fn concurrent_recoveries_launch_one_successor() {
    use Action::{Fresh, Retry};
    for (first, second) in [
        (Retry, Retry),
        (Fresh, Fresh),
        (Retry, Fresh),
        (Fresh, Retry),
    ] {
        race(first, second);
    }
}

fn race(first: Action, second: Action) {
    use std::sync::mpsc::channel;
    let at = format!("{first:?}/{second:?}");
    let w = person();
    team(&w);
    child(&w);
    let (root, cwd) = (w.ctx.state_root.clone(), w.ctx.cwd.clone());
    let (paused, at_promote) = channel();
    let (go, wait_go) = channel::<()>();
    let a = {
        let (root, cwd) = (root.clone(), cwd.clone());
        std::thread::spawn(move || {
            let w = person_at(&root, &cwd);
            both_gone(&w);
            w.fakes.hc_answer(ft::address("t2"));
            super::core_launch::hooks::after(Some(Box::new(move |s: &str| {
                if s == "promote" {
                    paused.send(()).unwrap();
                    wait_go.recv().unwrap();
                }
            })));
            let got = recover(&w.ctx, &w.caller(), first, X);
            super::core_launch::hooks::after(None);
            w.fakes.hc_done().unwrap();
            (
                got.map(|r| r.map(|m| m["agent"]["generation"].clone())),
                lf::count(&w, "run"),
            )
        })
    };
    at_promote
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("A reached promote");
    let (waited, saw_wait) = channel();
    let b = std::thread::spawn(move || {
        let w = person_at(&root, &cwd);
        let f = w
            .ctx
            .state_root
            .join(w.caller().server_key)
            .join("agents")
            .join(X);
        found(&w, X, 2, &recorded_wire(&w, &f, 2, None)); // observe, after A
        crate::state::lock::hooks::when_held(Some(Box::new(move || {
            let _ = waited.send(());
        })));
        let got = recover(&w.ctx, &w.caller(), second, X);
        crate::state::lock::hooks::when_held(None);
        let first_lock = w.fakes.locks().into_iter().next().unwrap_or_default();
        w.fakes.hc_done().unwrap();
        (got.map_err(|e| e.check), lf::count(&w, "run"), first_lock)
    });
    saw_wait
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("B waits while A holds its locks");
    go.send(()).unwrap();
    let (ra, runs_a) = a.join().unwrap();
    let (rb, runs_b, first_lock) = b.join().unwrap();
    assert_eq!(ra.unwrap(), Some(json!(2)), "{at}: A launched generation 2");
    assert_eq!(
        rb.err(),
        Some("state"),
        "{at}: B decided from A's successor"
    );
    assert_eq!(
        first_lock, "+up|T",
        "{at}: B's first lock is the team's up lock"
    );
    assert_eq!(runs_a + runs_b, 1, "{at}: one hc run");
}
