use super::forward_tests::*;
use crate::agents::fixture::{ID, SESSION_ID, World};
use crate::agents::gens;
use serde_json::json; // finding 4-1: the glob import of forward_tests does not bring the macro
use std::path::{Path, PathBuf};

const STEPS: [&str; 8] = [
    "pending", "desired", "spec", "launch", "run", "address", "commit", "promote",
];

/// A root without the Windows verbatim prefix, as `launcher::tests::setup` makes it (a spec path must classify).
pub(crate) fn plain(root: &Path) -> PathBuf {
    #[cfg(windows)]
    if let Some(p) = root.to_str().and_then(|r| r.strip_prefix(r"\\?\")) {
        return PathBuf::from(p);
    }
    root.to_path_buf()
}

/// Every crash child is bounded: past `LIMIT` it is killed (`SIGKILL` on Unix, `TerminateProcess` on Windows).
pub(crate) const LIMIT: std::time::Duration = std::time::Duration::from_secs(120);

/// Runs `child` again in this binary with MUX_AGENTS_CRASH=step; answers the `CRASH-<KEY> <value>` lines.
pub(crate) fn killed_at(child: &str, step: &str) -> Vec<(String, String)> {
    killed_within(child, step, LIMIT)
}

/// Runs `child` again in this binary with MUX_AGENTS_CRASH=step, at most `limit`; answers its `CRASH-` lines.
// The module is test-only (`core_launch.rs`); this gate keeps `tests/repo.rs`'s per-file spawn scan exempting the one
// child this file starts, as the inline module's gate did.
#[cfg(test)]
pub(crate) fn killed_within(
    child: &str,
    step: &str,
    limit: std::time::Duration,
) -> Vec<(String, String)> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut proc = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            child,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("MUX_AGENTS_CRASH", step)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Both pipes are drained on threads, so a chatty child never blocks on a full pipe.
    let drain = |mut r: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = r.read_to_string(&mut s);
            s
        })
    };
    let out = drain(Box::new(proc.stdout.take().unwrap()));
    let err = drain(Box::new(proc.stderr.take().unwrap()));
    let deadline = std::time::Instant::now() + limit;
    let status = loop {
        if let Some(s) = proc.try_wait().unwrap() {
            break s;
        }
        if std::time::Instant::now() > deadline {
            let _ = proc.kill();
            let _ = proc.wait();
            panic!("{step}: {child} ran past {limit:?} and was killed");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let (text, err) = (out.join().unwrap(), err.join().unwrap());
    assert_eq!(status.code(), Some(86), "{step}: {err}");
    text.lines()
        .filter_map(|l| l.split_once("CRASH-").map(|(_, kv)| kv))
        .filter_map(|kv| kv.split_once(char::is_whitespace))
        .map(|(k, v)| (k.to_string(), v.trim().to_string()))
        .collect()
}
pub(crate) fn printed(kv: &[(String, String)], key: &str) -> PathBuf {
    PathBuf::from(&kv.iter().find(|(k, _)| k == key).expect(key).1)
}

/// Appends a heartbeat to the file named by MUX_AGENTS_CRASH (the "step" `killed_within` passes) every 50 ms
/// for 60 s, unless killed.
#[test]
#[ignore = "a child of a_hung_child_is_killed_at_the_bound"]
fn child_hangs() {
    use std::io::Write;
    let beat = std::env::var("MUX_AGENTS_CRASH").unwrap();
    for _ in 0..1200 {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&beat)
            .unwrap();
        let _ = f.write_all(b".");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

// Fix-cycle finding 12: the bound returns in time, reports the kill, and the child is gone (its heartbeat
// stops). Removing `proc.kill()` fails `the child is gone`; removing the deadline fails `was killed`.
#[test]
fn a_hung_child_is_killed_at_the_bound() {
    use std::time::{Duration, Instant};
    let beat = std::env::temp_dir().join(format!("mux-agents-beat-{}", std::process::id()));
    let _ = std::fs::remove_file(&beat);
    let started = Instant::now();
    let child = "agents::core_launch::crash::child_hangs";
    let got = std::panic::catch_unwind(|| {
        killed_within(child, beat.to_str().unwrap(), Duration::from_secs(2))
    });
    let message = got
        .unwrap_err()
        .downcast::<String>()
        .map(|m| *m)
        .unwrap_or_default();
    assert!(message.contains("was killed"), "{message}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "returned under the bound"
    );
    let len = || std::fs::metadata(&beat).map_or(0, |m| m.len());
    let first = len();
    std::thread::sleep(Duration::from_millis(400));
    assert!(first > 0, "the child ran");
    assert_eq!(len(), first, "the child is gone");
    let _ = std::fs::remove_file(&beat);
}

#[test]
#[ignore = "a child of a_kill_after_each_forward_step_leaves_a_state_the_readers_classify"]
fn child_forward() {
    let mut w = World::human("");
    w.ctx.state_root = plain(&w.ctx.state_root);
    w.state();
    w.fakes.hc_install("hc");
    replies(&w, address("t1"));
    println!("CRASH-ROOT {}", w.ctx.state_root.display());
    let _ = go(&w, launch_of(&w, true, SESSION_ID), free, no_view);
    std::mem::forget(w);
}

/// The fake client on PATH and its process row (parent: this process), as `launcher::tests::setup` makes them.
fn with_client(w: &mut World) {
    let bin = w.ctx.state_root.parent().unwrap().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = bin.join(if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    });
    std::fs::write(&exe, b"").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut pairs: Vec<(String, String)> = w
        .ctx
        .env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    pairs.push(("PATH".into(), bin.to_str().unwrap().into()));
    w.ctx.env = crate::ctx::Env::from_pairs(pairs);
    w.fakes.process(crate::testing::row(4300, w.ctx.pid, 1600));
}

/// The real launcher of generation 0, in this process: its exit code and how many clients it started.
fn launcher_once(w: &World, f: &Path) -> (i32, usize) {
    let spec = gens::path(f, 0, "spec");
    let started = std::cell::Cell::new(0);
    let code = crate::launcher::run(
        &w.ctx,
        spec.to_str().unwrap(),
        &mut |_| {},
        &mut |_, _, _, told| {
            started.set(started.get() + 1);
            told(4300);
            Ok(0)
        },
    );
    (code, started.get())
}

fn decided_at(f: &Path) -> Option<gens::Decision> {
    gens::read(f, 0, "commit")
        .known()
        .unwrap()
        .map(|c| gens::decide(&c))
}

// launch-1 crash guarantee (finding aa9286): killed after each step, the files are what that step left, the
// readers hold the slot, a retry is refused while undecided, and the launcher starts the client at most
// once and only after the commit.
#[test]
fn a_kill_after_each_forward_step_leaves_a_state_the_readers_classify() {
    for (i, step) in STEPS.iter().enumerate() {
        let root = printed(
            &killed_at("agents::core_launch::crash::child_forward", step),
            "ROOT",
        );
        let mut w = World::human("");
        w.ctx.state_root = root.clone();
        let f = w.folder(ID);
        let at = |s: &str| i >= STEPS.iter().position(|x| *x == s).unwrap();
        let r = crate::state::fs::read_json(&f.join("record.json"))
            .unwrap()
            .unwrap();
        assert_eq!(
            r.get("pending").is_some(),
            !at("promote"),
            "{step}: pending until promotion"
        );
        let pending_gen = if at("promote") { json!(null) } else { json!(0) };
        assert_eq!(r["pending"]["gen"], pending_gen, "{step}");
        let addressed = r["pending"].get("hcAddress").is_some();
        assert_eq!(addressed, at("address") && !at("promote"), "{step}");
        assert_eq!(
            r.get("sessionId").is_some(),
            at("promote"),
            "{step}: the session only at promotion"
        );
        // cascade-3d C1: `running` is durable from step `desired` on; a crash at `desired` is abandoned as at
        // `pending` (G3 row 3).
        let desired = crate::agents::cascade::desired_of(&f).unwrap();
        assert_eq!(
            (f.join("desired.json").is_file(), desired),
            (at("desired"), crate::agents::cascade::Desired::Running),
            "{step}: desired"
        );
        assert_eq!(gens::path(&f, 0, "spec").is_file(), at("spec"), "{step}");
        assert_eq!(
            gens::path(&f, 0, "launch").is_file(),
            at("launch"),
            "{step}"
        );
        assert_eq!(gens::committed(&f, 0).unwrap(), at("commit"), "{step}");
        assert!(!gens::dir(&f, 1).exists(), "{step}: one generation");
        // A3a readers: the member holds its name and slot (held before promotion), never finished.
        let gens::Member::Life(g) = gens::member_of(&f) else {
            panic!("{step}: not a lifecycle member")
        };
        let s = g.status().unwrap();
        let got = (s.subject, s.held, s.committed, s.finished);
        assert_eq!(
            got,
            (Some(0), !at("promote"), at("commit"), false),
            "{step}"
        );
        let locals = crate::agents::membership::locals(&w.ctx, &w.caller()).unwrap();
        assert_eq!(locals.len(), 1, "{step}: the slot is held");
        assert_eq!(locals[0].held, !at("promote"), "{step}");
        // Nothing launches twice: a retry is refused while the generation is undecided.
        if !at("promote") {
            w.fakes.hc_install("hc");
            version(&w);
            let e = go(&w, launch_of(&w, false, NEW), free, never).unwrap_err();
            assert_eq!(e.check, "launch", "{step}: {e:?}");
            assert!(!gens::dir(&f, 1).exists() && runs(&w).is_empty(), "{step}");
        }
        with_client(&mut w);
        let (code, started) = launcher_once(&w, &f);
        match (at("launch"), at("commit")) {
            // Finding 4-2: no spec.json yet, so the launcher cannot canonicalize it (main: exit 2, launcher.rs
            // line 212, its test at line 997); a spec without launch.json is not its own (exit 1).
            (false, _) if !at("spec") => assert_eq!((code, started), (2, 0), "{step}: no spec"),
            (false, _) => {
                assert_eq!((code, started), (1, 0), "{step}: not a launch of its own")
            }
            (true, false) => {
                assert_eq!(started, 0, "{step}: no commit, no client");
                assert_eq!(
                    decided_at(&f),
                    Some(gens::Decision::Aborted),
                    "{step}: the launcher gave up"
                );
                let gens::Member::Life(g) = gens::member_of(&f) else {
                    panic!("{step}")
                };
                assert!(
                    g.status().unwrap().finished,
                    "{step}: void, so the slot is free"
                );
            }
            (true, true) => {
                assert_eq!(started, 1, "{step}");
                assert_eq!(
                    launcher_once(&w, &f),
                    (0, 0),
                    "{step}: a second launcher starts nothing"
                );
            }
        }
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }
}
// F3 (finding re3to4-1): killed after `allocated`, before any write: exit 86, no member, no slot held, and a
// first launch then succeeds as generation 0.
#[test]
fn a_kill_after_allocation_leaves_no_member_and_a_launch_succeeds() {
    let root = printed(
        &killed_at("agents::core_launch::crash::child_forward", "allocated"),
        "ROOT",
    );
    let mut w = World::human("");
    w.ctx.state_root = root.clone();
    let f = w.folder(ID);
    assert!(
        matches!(gens::member_of(&f), gens::Member::Absent),
        "nothing was written"
    );
    assert!(
        crate::agents::membership::locals(&w.ctx, &w.caller())
            .unwrap()
            .is_empty(),
        "no slot is held"
    );
    w.fakes.hc_install("hc");
    replies(&w, address("t1"));
    let r = go(&w, launch_of(&w, true, SESSION_ID), free, no_view).unwrap();
    assert_eq!(
        (&r["generation"], r.get("pending")),
        (&json!("0"), None),
        "the first launch converges"
    );
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

// Finding re3to4-2: killed after launch, run or address, the launcher gives up (commit Aborted) and the
// generation is void; a retry of forward then converges: generation 1 is launched and promoted.
#[test]
fn after_the_launcher_gives_up_a_retry_converges() {
    for step in ["launch", "run", "address"] {
        let root = printed(
            &killed_at("agents::core_launch::crash::child_forward", step),
            "ROOT",
        );
        let mut w = World::human("");
        w.ctx.state_root = root.clone();
        let f = w.folder(ID);
        with_client(&mut w);
        assert_eq!(
            launcher_once(&w, &f).1,
            0,
            "{step}: no client without a commit"
        );
        assert_eq!(
            decided_at(&f),
            Some(gens::Decision::Aborted),
            "{step}: the launcher gave up"
        );
        // The retry runs in a fresh world on the same root: the launcher PATH of `with_client` stays out of it.
        let mut w2 = World::human("");
        w2.ctx.state_root = root.clone();
        w2.fakes.hc_install("hc");
        replies(&w2, address("t2"));
        let r = go(&w2, launch_of(&w2, false, NEW), free, no_view)
            .unwrap_or_else(|e| panic!("{step}: the retry converges: {e:?}"));
        let got = (
            &r["generation"],
            &r["hcAddress"],
            &r["sessionId"],
            r.get("pending"),
        );
        assert_eq!(
            got,
            (&json!("1"), &json!("t2"), &json!(NEW), None),
            "{step}"
        );
        assert!(
            gens::committed(&f, 1).unwrap(),
            "{step}: generation 1 committed"
        );
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }
}

// Call 34: the commit is visible and its `gens/0` flush failed (forward answers NOT_FLUSHED). With the fault
// still armed the launcher cannot make the commit durable: it starts no client and exits as an uncommitted
// launch. With the fault cleared before it runs, it starts the client once, and a second launcher starts none.
#[test]
fn a_commit_the_launcher_cannot_flush_starts_no_client() {
    for armed in [true, false] {
        let _reset = super::hooks::Reset;
        let mut w = World::human("");
        w.ctx.state_root = plain(&w.ctx.state_root);
        w.state();
        w.fakes.hc_install("hc");
        let f = w.folder(ID);
        let n0 = gens::dir(&f, 0);
        super::hooks::after(Some(Box::new(move |s: &str| {
            if s == "address" {
                crate::state::fs::fail_sync_dir(Some(&n0));
            }
        })));
        replies(&w, address("t1"));
        let e = go(&w, launch_of(&w, true, SESSION_ID), free, never).unwrap_err();
        let got = (e.check, e.message.as_str());
        assert_eq!(got, ("state", gens::NOT_FLUSHED), "{armed}: {e:?}");
        assert_eq!(
            decided_at(&f),
            Some(gens::Decision::Run),
            "{armed}: the commit is visible"
        );
        super::hooks::after(None);
        if !armed {
            crate::state::fs::fail_sync_dir(None);
        }
        with_client(&mut w);
        if armed {
            assert_eq!(
                launcher_once(&w, &f),
                (1, 0),
                "an unflushed commit starts no client"
            );
        } else {
            let (first, synced) = crate::state::fs::record_syncs(|| launcher_once(&w, &f));
            assert_eq!(first, (0, 1), "a flushed commit starts the client");
            // The commit's folders, innermost first, as `publish_once` flushes them (call 34).
            let parent = f.parent().unwrap().to_path_buf();
            let want = [gens::dir(&f, 0), f.join("gens"), f.clone(), parent];
            assert!(
                synced.windows(4).any(|x| x == want),
                "the launcher flushes the commit's folders: {synced:?}"
            );
            assert_eq!(
                launcher_once(&w, &f),
                (0, 0),
                "a second launcher starts nothing"
            );
        }
    }
}

// settle-1b "Forward crash, replayed", on the files A3b-1's real harness leaves: a forward killed after each
// step, `allocated` included (b-calls 35, R8), settled once (Normal). At `allocated` there is no record: Done,
// nothing made, no claim. From `pending` until promotion no launcher claimed, so row 3 abandons generation 0;
// after promotion there is no pending. A replay changes nothing and asks no hc; a launcher woken afterwards
// starts the client only for the promoted generation, and a second launcher starts none: nothing runs twice.
#[test]
fn a_forward_crash_after_each_step_is_settled_once_and_runs_nothing_twice() {
    use crate::agents::settle::{Mode, Outcome};
    use crate::agents::team_state::{Scope, Tx};
    for step in std::iter::once("allocated").chain(STEPS) {
        let root = printed(
            &killed_at("agents::core_launch::crash::child_forward", step),
            "ROOT",
        );
        let mut w = World::human("");
        w.ctx.state_root = root.clone();
        w.fakes.hc_install("hc");
        let f = w.folder(ID);
        let settled = |w: &World| {
            Tx {
                ctx: &w.ctx,
                scope: Scope::of(&w.caller()),
            }
            .settle(ID, Mode::Normal)
        };
        assert_eq!(settled(&w).unwrap(), Outcome::Done, "{step}");
        let (allocated, promoted) = (step == "allocated", step == "promote");
        let claim = gens::claim(&f, 0).known().unwrap();
        let abandoned = !allocated && !promoted;
        assert_eq!(
            claim,
            abandoned.then(|| json!("abandoned")),
            "{step}: claim"
        );
        let r = crate::state::fs::read_json(&f.join("record.json")).unwrap();
        assert_eq!(
            r.is_none(),
            allocated,
            "{step}: a record only after allocation"
        );
        assert!(
            r.as_ref().and_then(|r| r.get("pending")).is_none(),
            "{step}: pending"
        );
        let before = std::fs::read(f.join("record.json")).ok();
        assert_eq!(settled(&w).unwrap(), Outcome::Done, "{step}: replay");
        assert_eq!(
            std::fs::read(f.join("record.json")).ok(),
            before,
            "{step}: the replay wrote"
        );
        assert!(w.fakes.hc_calls().is_empty(), "{step}: settle asked hc");
        with_client(&mut w);
        assert_eq!(
            launcher_once(&w, &f).1,
            usize::from(promoted),
            "{step}: clients started"
        );
        assert_eq!(
            launcher_once(&w, &f).1,
            0,
            "{step}: a second launcher starts nothing"
        );
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }
}
