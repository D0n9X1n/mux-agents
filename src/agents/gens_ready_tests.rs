//! K5 (#103 report-2b): `promote_ready` under the gen lock, every file read once through the walk (R1, PM call 11),
//! `ready.json` written only through `publish_once` (PM call 14).
use super::*;
use crate::state::evidence::publish;
use crate::testing::{Fakes, TempDir, Wrong, plant, private_tree, row, untouched};
use serde_json::json;

const KEY: &str = "0123456789abcdef";
const ID: &str = "aaaaaaaaaaaa";

// Launcher 100 (start 1000) and its client 150 (start 1500); generation 1 has its launcher.json. The state root is
// private (0700 on Unix, PM call 16) before anything is written under it.
fn world() -> (Fakes, crate::ctx::Ctx, TempDir, PathBuf) {
    let f = Fakes::default();
    f.process(row(100, 1, 1000));
    f.process(row(150, 100, 1500));
    let mut ctx = f.ctx();
    let d = TempDir::new();
    ctx.state_root = d.path().join("state");
    let m = ctx.state_root.join(KEY).join("agents").join(ID);
    private_tree(&ctx.state_root, &m);
    publish(
        &path(&m, 1, "launcher"),
        &json!({"pid": 100, "start": 1000, "startedAt": "x"}),
    )
    .unwrap();
    (f, ctx, d, m)
}

fn reported(m: &Path, pid: u32, start: i64) {
    let r = json!({"sessionId": "0f11fc2f-2032-48ef-a920-4af2a8e5af50", "pid": pid, "start": start, "at": "x"});
    publish(&path(m, 1, "report"), &r).unwrap();
}

fn client_json(m: &Path) {
    publish(&path(m, 1, "client"), &json!({"pid": 150, "start": 1500})).unwrap();
}

fn bytes(m: &Path, file: &str) -> Option<Vec<u8>> {
    std::fs::read(path(m, 1, file)).ok()
}

/// The refusal's check, so RED and mutation runs print a value.
fn promote(ctx: &crate::ctx::Ctx) -> Result<Ready, &'static str> {
    promote_ready(ctx, KEY, ID, 1).map_err(|r| r.check)
}

fn kind_of(p: &Path) -> Option<std::fs::FileType> {
    std::fs::symlink_metadata(p).ok().map(|m| m.file_type())
}

// K5 order: outcome, then failure (authoritative), then ready, then a matching report and client.
#[test]
fn promote_ready_decides_in_order() {
    let (f, ctx, _d, m) = world();
    reported(&m, 150, 1500);
    assert_eq!(promote(&ctx), Ok(Ready::Undecided), "no client.json yet");
    assert_eq!(bytes(&m, "ready"), None);
    client_json(&m);
    assert_eq!(promote(&ctx), Ok(Ready::Ok));
    let r = crate::state::fs::read_json(&path(&m, 1, "ready"))
        .unwrap()
        .unwrap();
    assert_eq!((&r["pid"], &r["start"]), (&json!(150), &json!(1500)));
    let first = bytes(&m, "ready");
    f.forget(150);
    assert_eq!(promote(&ctx), Ok(Ready::Ok), "ready.json alone decides");
    assert_eq!(bytes(&m, "ready"), first, "written once");
    publish(&path(&m, 1, "outcome"), &json!({"verdict": "ok"})).unwrap();
    assert_eq!(promote(&ctx), Ok(Ready::Outcome(json!({"verdict": "ok"}))));
}

// (fix 1, PM call 14) A failed flush is a failed publication: no `ok` until `gens/1` is flushed, also when an earlier
// call left `ready.json` linked but unflushed (step 3's Present goes through `publish_once` too).
#[test]
fn a_ready_json_that_cannot_be_flushed_is_not_ready() {
    let (_f, ctx, _d, m) = world();
    reported(&m, 150, 1500);
    client_json(&m);
    crate::state::fs::fail_sync_dir(Some(&dir(&m, 1)));
    let first = promote(&ctx);
    let again = promote(&ctx);
    crate::state::fs::fail_sync_dir(None);
    assert!(
        first.is_err(),
        "a failed flush is a failed publication: {first:?}"
    );
    assert!(
        again.is_err(),
        "an unflushed ready.json is not ready: {again:?}"
    );
    assert_eq!(
        promote(&ctx),
        Ok(Ready::Ok),
        "flushed once the fault clears"
    );
}

/// (fix 2, PM call 19) The generation-1 files read, in order, at the level the publisher reads (`evidence::read`).
fn gen1_reads(
    m: &Path,
    f: impl FnOnce() -> Result<Ready, &'static str>,
) -> (Result<Ready, &'static str>, Vec<String>) {
    let (got, paths) = crate::state::evidence::record_evidence_reads(f);
    let g1 = dir(m, 1);
    let names = paths
        .iter()
        .filter(|p| p.parent() == Some(g1.as_path()))
        .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    (got, names)
}

// (fix 2, PM calls 11 and 19) R1, exactly: one decision read of each file, then `publish_once`'s own read of
// `ready.json`, the one trailing read allowed.
#[test]
fn promote_ready_reads_each_file_once() {
    let (_f, ctx, _d, m) = world();
    reported(&m, 150, 1500);
    client_json(&m);
    let (got, reads) = gen1_reads(&m, || promote(&ctx));
    assert_eq!(got, Ok(Ready::Ok));
    let want = [
        "outcome", "failure", "ready", "report", "client", "exit", "launcher", "ready",
    ];
    assert_eq!(
        reads, want,
        "seven decision reads, then the publisher's one"
    );
    let (got, reads) = gen1_reads(&m, || promote(&ctx));
    assert_eq!(got, Ok(Ready::Ok));
    assert_eq!(
        reads,
        ["outcome", "failure", "ready", "ready"],
        "an existing ready.json: one decision read, one publisher read"
    );
}

// (fix 2, re-audit #1, #8) The sanctioned reread (PM call 11): a racer links ready.json between the publisher's read
// and its link, through A3b-1's own seam `publish_hooks::before_link` (`Box<dyn Fn(&Path)>`, `life-plan-A3b-2.md`);
// the publication answers `Exists`, and K5 reads ready.json once more. A torn racer's file decides nothing.
#[test]
fn a_lost_ready_publication_rereads_only_ready_json() {
    let whole = br#"{"pid":150,"start":1500,"at":"x"}"#.to_vec();
    let want = [
        "outcome", "failure", "ready", "report", "client", "exit", "launcher", "ready", "ready",
    ];
    for (planted, ok) in [(whole, true), (b"{".to_vec(), false)] {
        let (_f, ctx, _d, m) = world();
        reported(&m, 150, 1500);
        client_json(&m);
        let p = planted.clone();
        publish_hooks::before_link(Some(Box::new(move |target: &Path| {
            std::fs::write(target, &p).unwrap()
        })));
        let (got, reads) = gen1_reads(&m, || promote(&ctx));
        publish_hooks::before_link(None);
        assert_eq!(
            reads, want,
            "reread (whole racer: {ok}): the publisher's one read, then K5's reread"
        );
        if ok {
            assert_eq!(got, Ok(Ready::Ok), "a whole racer's ready.json");
        } else {
            assert!(
                got.is_err(),
                "a torn racer's ready.json decides nothing: {got:?}"
            );
        }
        assert_eq!(bytes(&m, "ready"), Some(planted), "never overwritten");
    }
}

#[test]
fn a_recorded_failure_is_never_reversed() {
    let (_f, ctx, _d, m) = world();
    reported(&m, 150, 1500);
    publish(
        &path(&m, 1, "failure"),
        &json!({"reason": "no readiness report"}),
    )
    .unwrap();
    client_json(&m);
    assert_eq!(promote(&ctx), Ok(Ready::Failed));
    assert_eq!(bytes(&m, "ready"), None);
    // (fix 1) failure.json beside an existing ready.json: failure decides.
    publish(
        &path(&m, 1, "ready"),
        &json!({"pid": 150, "start": 1500, "at": "x"}),
    )
    .unwrap();
    let ready = bytes(&m, "ready");
    assert_eq!(
        promote(&ctx),
        Ok(Ready::Failed),
        "failure before an existing ready.json"
    );
    assert_eq!(bytes(&m, "ready"), ready);
    publish(&path(&m, 1, "outcome"), &json!({"verdict": "failed"})).unwrap();
    assert_eq!(
        promote(&ctx),
        Ok(Ready::Outcome(json!({"verdict": "failed"}))),
        "outcome first"
    );
}

// K5 step 4 needs the same pid and start that gens::client proves (proof-0b K1a); anything else is Undecided.
#[test]
fn an_unproven_client_leaves_promote_ready_undecided() {
    let (_f, ctx, _d, m) = world();
    reported(&m, 150, 1500);
    client_json(&m);
    assert_eq!(promote(&ctx), Ok(Ready::Ok), "control: proven and matching");
    type Break = fn(&Fakes, &Path);
    let cases: [(&str, Break); 6] = [
        ("foreign client", |f, m| {
            f.process(row(170, 1, 1700));
            std::fs::write(path(m, 1, "client"), br#"{"pid":170,"start":1700}"#).unwrap();
        }),
        ("client-conflict", |_, m| {
            publish(
                &path(m, 1, "exit"),
                &json!({"exitCode": null, "reason": "client-conflict"}),
            )
            .unwrap();
        }),
        ("reused client pid", |f, _| {
            f.forget(150);
            f.process(row(150, 100, 1600));
        }),
        ("report names another pid", |_, m| {
            std::fs::write(path(m, 1, "report"), br#"{"pid":151,"start":1500}"#).unwrap();
        }),
        // (fix 1) Start equality: the pid matches, the start does not.
        ("report names another start", |_, m| {
            std::fs::write(path(m, 1, "report"), br#"{"pid":150,"start":1499}"#).unwrap();
        }),
        ("no report.json", |_, m| {
            std::fs::remove_file(path(m, 1, "report")).unwrap()
        }),
    ];
    for (case, break_one) in cases {
        let (f, ctx, _d, m) = world();
        reported(&m, 150, 1500);
        client_json(&m);
        assert!(client(&ctx, &m, 1).is_some(), "{case}: proven before");
        break_one(&f, &m);
        assert_eq!(promote(&ctx), Ok(Ready::Undecided), "{case}");
        assert_eq!(bytes(&m, "ready"), None, "{case}: nothing written");
    }
}

// r2b call 21 (o2 call 15): the client proof is three-valued. A torn or wrong-type client.json, launcher.json or
// exit.json refuses `evidence` and writes nothing; it is never "unproven".
#[test]
fn an_unreadable_client_proof_refuses_and_writes_nothing() {
    for file in ["client", "launcher", "exit"] {
        let mut kinds: Vec<Option<Wrong>> = vec![None];
        kinds.extend(Wrong::all().into_iter().map(Some));
        for kind in kinds {
            let (_f, ctx, _d, m) = world();
            reported(&m, 150, 1500);
            client_json(&m);
            let at = path(&m, 1, file);
            let _ = std::fs::remove_file(&at);
            match kind {
                None => std::fs::write(&at, b"{").unwrap(),
                Some(k) => plant(k, &at),
            }
            assert_eq!(promote(&ctx), Err("evidence"), "{file} {kind:?}");
            assert_eq!(
                kind_of(&path(&m, 1, "ready")),
                None,
                "{file} {kind:?}: no ready.json"
            );
            assert!(untouched(&at), "{file} {kind:?}: nothing written through");
        }
    }
}

// r2b call 21: `client_evidence` answers the proven (pid, start), None when unproven, `evidence` when unreadable.
#[test]
fn client_evidence_is_three_valued() {
    let (f, ctx, _d, m) = world();
    client_json(&m);
    let got = |ctx: &crate::ctx::Ctx, m: &Path| client_evidence(ctx, m, 1).map_err(|r| r.check);
    assert_eq!(got(&ctx, &m), Ok(Some((150, 1500))), "proven");
    f.forget(150);
    assert_eq!(got(&ctx, &m), Ok(None), "the client is gone: unproven");
    std::fs::write(path(&m, 1, "client"), b"{").unwrap();
    assert_eq!(got(&ctx, &m), Err("evidence"), "torn client.json");
    let (_f, ctx, _d, m) = world();
    assert_eq!(got(&ctx, &m), Ok(None), "no client.json");
}

// (fix 1, PM call 13) Torn or wrong-type evidence at any K5 read, or a wrong type at `gens/<n>`: `evidence`, nothing
// written, nothing changed.
#[test]
fn torn_or_wrong_type_evidence_stops_promote_ready_and_writes_nothing() {
    for file in ["outcome", "failure", "ready", "report"] {
        let mut kinds: Vec<Option<Wrong>> = vec![None];
        kinds.extend(Wrong::all().into_iter().map(Some));
        for kind in kinds {
            let (_f, ctx, _d, m) = world();
            reported(&m, 150, 1500);
            client_json(&m);
            let at = path(&m, 1, file);
            let _ = std::fs::remove_file(&at);
            match kind {
                None => std::fs::write(&at, b"{").unwrap(),
                Some(k) => plant(k, &at),
            }
            let before = kind_of(&at);
            assert_eq!(promote(&ctx), Err("evidence"), "{file} {kind:?}");
            assert_eq!(kind_of(&at), before, "{file} {kind:?}: left as it was");
            assert!(untouched(&at), "{file} {kind:?}: nothing written through");
            if file != "ready" {
                assert_eq!(
                    kind_of(&path(&m, 1, "ready")),
                    None,
                    "{file} {kind:?}: no ready.json"
                );
            }
        }
    }
    for kind in Wrong::all().into_iter().filter(|k| *k != Wrong::Folder) {
        let (_f, ctx, _d, m) = world();
        plant(kind, &dir(&m, 2));
        let got = promote_ready(&ctx, KEY, ID, 2).map_err(|r| r.check);
        assert_eq!(got, Err("evidence"), "{kind:?} at gens/2");
        assert!(
            untouched(&dir(&m, 2)),
            "{kind:?} at gens/2: nothing written through"
        );
    }
}

#[test]
fn promote_ready_holds_the_gen_lock_and_refuses_unplain_names() {
    let (f, ctx, _d, m) = world();
    reported(&m, 150, 1500);
    client_json(&m);
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let l = log.clone();
    f.lock_probe(move |e| {
        l.borrow_mut().push(e.to_string());
        String::new()
    });
    assert_eq!(promote(&ctx), Ok(Ready::Ok));
    let key = crate::agents::team_state::gen_lock_key(KEY, ID, 1);
    assert_eq!(*log.borrow(), [format!("+{key}"), format!("-{key}")]);
    log.borrow_mut().clear();
    assert_eq!(
        promote_ready(&ctx, "../x", ID, 1).map_err(|r| r.check),
        Err("internal")
    );
    assert_eq!(
        promote_ready(&ctx, KEY, "../aaaaaaaaa", 1).map_err(|r| r.check),
        Err("internal")
    );
    assert!(log.borrow().is_empty(), "refused before any lock");
}

// (outcome-2 call 1) promote_held runs under a gen lock its caller already holds, takes none itself, and hands back
// the failure.json and report.json it read. A nested take of the held key panics the fake lock ("taken while held")
// and refuses on Unix.
#[test]
fn promote_held_takes_no_lock_and_hands_back_what_it_read() {
    let (f, ctx, _d, m) = world();
    reported(&m, 150, 1500);
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let l = log.clone();
    f.lock_probe(move |e| {
        l.borrow_mut().push(e.to_string());
        String::new()
    });
    let key = crate::agents::team_state::gen_lock_key(KEY, ID, 1);
    let held = crate::agents::lock(&ctx, &key, || promote_held(&ctx, &m, 1)).map_err(|r| r.check);
    let report = crate::state::fs::read_json(&path(&m, 1, "report")).unwrap();
    let want = Held {
        ready: Ready::Undecided,
        failure: None,
        report: report.clone(),
        exit: None,
    };
    assert_eq!(held, Ok(want), "inside the caller's own hold");
    assert_eq!(
        *log.borrow(),
        [format!("+{key}"), format!("-{key}")],
        "only the caller's hold"
    );
    client_json(&m);
    let held = crate::agents::lock(&ctx, &key, || promote_held(&ctx, &m, 1)).map_err(|r| r.check);
    let want = Held {
        ready: Ready::Ok,
        failure: None,
        report: report.clone(),
        exit: None,
    };
    assert_eq!(held, Ok(want), "success: the report it read");
    // Build checklist item 12, r2b call 21: a report naming another pid, beside an exit.json without a conflict.
    let (_f, ctx, _d, m) = world();
    let other = json!({"sessionId": "0f11fc2f-2032-48ef-a920-4af2a8e5af50", "pid": 151, "start": 1500, "at": "x"});
    publish(&path(&m, 1, "report"), &other).unwrap();
    client_json(&m);
    let exit = json!({"exitCode": 0, "reason": "exited"});
    publish(&path(&m, 1, "exit"), &exit).unwrap();
    let held = promote_held(&ctx, &m, 1).map_err(|r| r.check);
    let want = Held {
        ready: Ready::Undecided,
        failure: None,
        report: Some(other),
        exit: Some(exit),
    };
    assert_eq!(
        held,
        Ok(want),
        "mismatch: the report and the exit.json it read"
    );
    let failure = json!({"reason": "no readiness report"});
    let (_f, ctx, _d, m) = world();
    reported(&m, 150, 1500);
    publish(&path(&m, 1, "failure"), &failure).unwrap();
    let held = promote_held(&ctx, &m, 1).map_err(|r| r.check);
    let want = Held {
        ready: Ready::Failed,
        failure: Some(failure),
        report: None,
        exit: None,
    };
    assert_eq!(held, Ok(want), "the failure it read");
}

// r2b call 23 (TL #143): on the report path the exit evidence is read and kept whether or not client.json exists.
// Report present, client absent: a whole exit.json is carried; a torn or wrong-type one refuses `evidence`.
#[test]
fn the_exit_evidence_is_kept_when_client_json_is_absent() {
    let (_f, ctx, _d, m) = world();
    reported(&m, 150, 1500);
    let report = crate::state::fs::read_json(&path(&m, 1, "report")).unwrap();
    let exit = json!({"exitCode": 1, "reason": "exited"});
    publish(&path(&m, 1, "exit"), &exit).unwrap();
    let held = promote_held(&ctx, &m, 1).map_err(|r| r.check);
    let want = Held {
        ready: Ready::Undecided,
        failure: None,
        report,
        exit: Some(exit),
    };
    assert_eq!(held, Ok(want), "a whole exit.json, no client.json");
    let mut kinds: Vec<Option<Wrong>> = vec![None];
    kinds.extend(Wrong::all().into_iter().map(Some));
    for kind in kinds {
        let (_f, ctx, _d, m) = world();
        reported(&m, 150, 1500);
        let at = path(&m, 1, "exit");
        match kind {
            None => std::fs::write(&at, b"{").unwrap(),
            Some(k) => plant(k, &at),
        }
        let held = promote_held(&ctx, &m, 1)
            .map(|h| h.ready)
            .map_err(|r| r.check);
        assert_eq!(held, Err("evidence"), "exit {kind:?}, no client.json");
        assert_eq!(
            kind_of(&path(&m, 1, "ready")),
            None,
            "exit {kind:?}: no ready.json"
        );
        assert!(untouched(&at), "exit {kind:?}: nothing written through");
    }
}

// r2b call 26 (QA #143): a parent is older than its child. A client whose start is earlier than its launcher's is not
// that launcher's child (a reused pid), so it is unproven: launcher {100, 2000}, client {150, ppid 100, 1500}.
#[test]
fn a_client_older_than_its_launcher_is_unproven() {
    let (f, ctx, _d, m) = world();
    reported(&m, 150, 1500);
    client_json(&m);
    f.forget(100);
    f.process(row(100, 1, 2000));
    std::fs::remove_file(path(&m, 1, "launcher")).unwrap();
    publish(
        &path(&m, 1, "launcher"),
        &json!({"pid": 100, "start": 2000, "startedAt": "x"}),
    )
    .unwrap();
    assert_eq!(
        client_evidence(&ctx, &m, 1).map_err(|r| r.check),
        Ok(None),
        "client_evidence"
    );
    assert_eq!(promote(&ctx), Ok(Ready::Undecided), "promote_ready");
    assert_eq!(bytes(&m, "ready"), None, "nothing written");
}
