use super::*;
use crate::agents::fixture::World;
use serde_json::json;

const T: &str = "0123456789abcdef";

fn inputs(w: &World) -> Inputs {
    Inputs {
        client: "claude".into(),
        options: vec!["--model".into(), "a b".into()],
        prompt: vec!["q".into()],
        name: String::new(),
        folder: w.ctx.cwd.to_str().unwrap().into(),
    }
}
fn here(w: &World) -> Server {
    server_of(&w.caller())
}
fn full(w: &World, s: &Server) -> Index {
    let release = w
        .ctx
        .state_root
        .join("up")
        .join("r.release")
        .to_str()
        .unwrap()
        .to_string();
    Index {
        team: T.into(),
        inputs: inputs(w),
        anchor: Some(Anchor {
            key: s.key.clone(),
            session: "$3".into(),
        }),
        view: Some(View {
            server: s.clone(),
            session: "$3".into(),
            name: "p-01234567-x".into(),
        }),
        pending: Some(Pending {
            server: s.clone(),
            name: "p-01234567-y".into(),
            release,
        }),
        aliases: vec![View {
            server: s.clone(),
            session: "$1".into(),
            name: "old".into(),
        }],
        releases: vec![
            w.ctx
                .state_root
                .join("up")
                .join("q.release")
                .to_str()
                .unwrap()
                .into(),
        ],
    }
}
/// What `read` answered, as `(code, check, message)` on a refusal; a read that succeeds names its case in the assert.
fn refused(w: &World) -> Result<String, (u8, &'static str, String)> {
    match read(&w.ctx, T) {
        Ok(found) => Ok(format!("{found:?}")),
        Err(e) => Err((e.code, e.check, e.message)),
    }
}
fn not_flushed(e: Refusal) -> (&'static str, String) {
    (e.check, e.message)
}
fn flush_refusal() -> (&'static str, String) {
    ("state", crate::agents::gens::NOT_FLUSHED.to_string())
}

// E1, ambiguity 2, call 14: what is written is what is read; absent is None; every other shape, a present null,
// a server tuple whose key does not hash its own fields, and (call 21) a view or alias without an anchor are
// 1 `index` naming the file's full path (call 2).
#[test]
fn an_index_round_trips_and_every_bad_shape_is_index() {
    let w = World::human("");
    let s = here(&w);
    assert_eq!(read(&w.ctx, T).unwrap(), None, "absent");
    let ix = full(&w, &s);
    write(&w.ctx, &ix).unwrap();
    assert_eq!(read(&w.ctx, T).unwrap(), Some(ix.clone()));
    let at = path(&w.ctx, T);
    let good: Value = serde_json::from_slice(&std::fs::read(&at).unwrap()).unwrap();
    let named = format!(
        "the team index cannot be read; repair or remove {}",
        at.display()
    );
    let bad = |mutate: &dyn Fn(&mut Value), what: &str| {
        let mut v = good.clone();
        mutate(&mut v);
        std::fs::write(&at, v.to_string()).unwrap();
        assert_eq!(refused(&w), Err((1, "index", named.clone())), "{what}");
    };
    bad(
        &|v| v["team"] = json!("ffffffffffffffff"),
        "team is not the file's T",
    );
    bad(&|v| v["schema"] = json!(2), "schema");
    bad(
        &|v| v["inputs"]["options"] = json!("x"),
        "options not a list",
    );
    bad(
        &|v| v["inputs"]["client"] = json!("not-a-client"),
        "an unknown client (call 36)",
    );
    bad(
        &|v| v["releases"] = json!(["rel.release"]),
        "a release not absolute (call 35)",
    );
    bad(
        &|v| {
            v.as_object_mut().unwrap().remove("releases");
        },
        "releases missing (call 35)",
    );
    bad(&|v| v["view"]["serverPid"] = json!("1"), "pid not a number");
    bad(
        &|v| v["pending"]["release"] = json!("rel.release"),
        "release not absolute",
    );
    bad(&|v| v["aliases"] = json!({}), "aliases not a list");
    bad(&|v| v["anchor"] = Value::Null, "a present null anchor");
    bad(&|v| v["view"] = Value::Null, "a present null view");
    bad(&|v| v["pending"] = Value::Null, "a present null pending");
    bad(
        &|v| v["aliases"][0]["serverStart"] = json!(9),
        "an alias whose key does not hash",
    );
    bad(
        &|v| v["pending"]["serverKey"] = json!("ffffffffffffffff"),
        "a pending whose key does not hash",
    );
    bad(
        &|v| {
            v.as_object_mut().unwrap().remove("anchor");
        },
        "a view with no anchor (call 21)",
    );
    bad(
        &|v| {
            v.as_object_mut().unwrap().remove("inputs");
        },
        "inputs missing",
    );
    std::fs::write(&at, b"{").unwrap();
    assert_eq!(refused(&w), Err((1, "index", named.clone())), "torn");
}

// Calls 14, 31, 38, 6a-R2: every newly created folder up to an existing ancestor is flushed, and the chain above
// it to the volume root; each flush failing alone refuses `state` (`gens::NOT_FLUSHED`); team and inputs are fixed;
// a folder at the final name is never renamed over.
#[test]
fn write_flushes_every_created_folder_and_checks_the_final_name() {
    let mut w = World::human("");
    w.ctx.state_root = w._dir.path().join("fresh").join("state");
    let s = here(&w);
    let ix = full(&w, &s);
    let (r, synced) = crate::state::fs::record_syncs(|| write(&w.ctx, &ix));
    r.unwrap();
    let root = w.ctx.state_root.clone();
    for dir in [
        root.join("up"),
        root.clone(),
        root.parent().unwrap().to_path_buf(),
    ] {
        assert!(
            synced.contains(&dir),
            "{} flushed: {synced:?}",
            dir.display()
        );
    }
    // Call 31: the first ancestor that already existed (`_dir`) is flushed too, so `fresh`'s entry is durable.
    let base = w._dir.path().to_path_buf();
    assert!(
        synced.contains(&base),
        "{} flushed: {synced:?}",
        base.display()
    );
    // Its flush failing alone refuses `state` and leaves no folder this call made, so a retry flushes them again.
    let mut v = World::human("");
    let ix2 = full(&v, &here(&v));
    v.ctx.state_root = v._dir.path().join("fresh").join("state");
    crate::state::fs::fail_sync_dir(Some(v._dir.path()));
    let e = write(&v.ctx, &ix2).unwrap_err();
    crate::state::fs::fail_sync_dir(None);
    assert_eq!(not_flushed(e), flush_refusal(), "the existing ancestor");
    assert!(
        !v._dir.path().join("fresh").exists(),
        "the made folders removed"
    );
    let (r, synced) = crate::state::fs::record_syncs(|| write(&v.ctx, &ix2));
    r.unwrap();
    assert!(
        synced.contains(&v._dir.path().to_path_buf()),
        "the retry flushes the chain"
    );
    for dir in [
        root.join("up"),
        root.clone(),
        root.parent().unwrap().to_path_buf(),
        base.clone(),
    ] {
        crate::state::fs::fail_sync_dir(Some(&dir));
        let e = write(&w.ctx, &ix).unwrap_err();
        crate::state::fs::fail_sync_dir(None);
        assert_eq!(not_flushed(e), flush_refusal(), "{}", dir.display());
    }
    // The index file itself (its temp sibling, flushed before the rename): fails alone, refuses `state`.
    crate::state::fs::fail_sync_file(Some(&path(&w.ctx, T)));
    let e = write(&w.ctx, &ix).unwrap_err();
    crate::state::fs::fail_sync_file(None);
    assert_eq!(not_flushed(e), flush_refusal(), "the index file's flush");
    let mut other = ix.clone();
    other.inputs.options = vec!["--model".into(), "z".into()];
    assert_eq!(
        write(&w.ctx, &other).unwrap_err().check,
        "internal",
        "inputs are fixed"
    );
    let at = path(&w.ctx, T);
    std::fs::remove_file(&at).unwrap();
    std::fs::create_dir(&at).unwrap();
    assert_eq!(
        write(&w.ctx, &ix).unwrap_err().check,
        "index",
        "a folder at the final name"
    );
    assert!(at.is_dir(), "never renamed over");
}

// R14: the writer's own folder flush (a fresh final name, so no old index is read first) failing is `NOT_FLUSHED`,
// never `NOT_PUBLISHED`.
#[test]
fn the_writers_folder_flush_failing_is_not_flushed() {
    let w = World::human("");
    let up = w.ctx.state_root.join("up");
    std::fs::create_dir_all(&up).unwrap();
    let ix = full(&w, &here(&w));
    crate::state::fs::fail_sync_dir(Some(&up));
    let e = write(&w.ctx, &ix).unwrap_err();
    crate::state::fs::fail_sync_dir(None);
    assert_eq!(not_flushed(e), flush_refusal(), "the writer's up/ flush");
}

// Call 38 (R1): folders made by a process that died before flushing them are finished by the next writer and
// reader, which flush the whole chain without knowing what was made; until that succeeds both refuse.
#[test]
fn folders_left_unflushed_by_a_crash_are_flushed_by_the_next_access() {
    let mut w = World::human("");
    let ix = full(&w, &here(&w));
    let existing = w._dir.path().to_path_buf();
    w.ctx.state_root = existing.join("fresh").join("state");
    std::fs::create_dir_all(w.ctx.state_root.join("up")).unwrap(); // made, then the crash: never flushed
    crate::state::fs::fail_sync_dir(Some(&existing));
    let e = write(&w.ctx, &ix).unwrap_err();
    assert_eq!(
        not_flushed(e),
        flush_refusal(),
        "the writer finishes the chain"
    );
    crate::state::fs::fail_sync_dir(None);
    write(&w.ctx, &ix).unwrap();
    crate::state::fs::fail_sync_dir(Some(&existing));
    let e = read(&w.ctx, T).unwrap_err();
    crate::state::fs::fail_sync_dir(None);
    assert_eq!(
        not_flushed(e),
        flush_refusal(),
        "the reader finishes the chain"
    );
    assert_eq!(read(&w.ctx, T).unwrap(), Some(ix));
}

// Call 14 (group B calls 46/50): an index whose publication flush failed is visible but not durable; readers
// complete the flushes first and act on nothing until they succeed.
#[test]
fn a_visible_index_is_flushed_before_it_is_used() {
    let w = World::human("");
    let s = here(&w);
    let up = w.ctx.state_root.join("up");
    std::fs::create_dir_all(&up).unwrap();
    crate::state::fs::fail_sync_dir(Some(&up));
    assert_eq!(write(&w.ctx, &full(&w, &s)).unwrap_err().check, "state");
    assert!(path(&w.ctx, T).is_file(), "visible");
    assert_eq!(
        read(&w.ctx, T).unwrap_err().message,
        crate::agents::gens::NOT_FLUSHED,
        "not acted on"
    );
    assert_eq!(
        every(&w.ctx).unwrap_err().message,
        crate::agents::gens::NOT_FLUSHED,
        "nor scanned"
    );
    crate::state::fs::fail_sync_dir(None);
    assert!(read(&w.ctx, T).unwrap().is_some(), "flushed, then read");
}

// Call 14: a linked or junctioned `up/` refuses reads, scans and writes as `index`, naming the folder.
#[test]
fn a_linked_up_folder_refuses_every_access() {
    let w = World::human("");
    let s = here(&w);
    write(&w.ctx, &full(&w, &s)).unwrap();
    let (up, real) = (
        w.ctx.state_root.join("up"),
        w.ctx.state_root.join("real-up"),
    );
    std::fs::rename(&up, &real).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &up).unwrap();
    #[cfg(windows)]
    crate::testing::link_dir(&real, &up);
    let linked = format!("{} is not a real folder", up.display());
    let e = read(&w.ctx, T).unwrap_err();
    assert_eq!((e.check, e.message), ("index", linked.clone()));
    assert_eq!(every(&w.ctx).unwrap_err().message, linked, "scan");
    assert_eq!(
        write(&w.ctx, &full(&w, &s)).unwrap_err().message,
        linked,
        "write"
    );
}

// Call 31: a regular file in the state root's place, or in any ancestor's, is 1 `index` naming it on every OS
// (Windows reports `NotFound` below it); never an absent index, and nothing is written.
#[test]
fn a_wrong_type_state_ancestor_is_index_never_absent() {
    let mut w = World::human("");
    let ix = full(&w, &here(&w));
    let blocker = w._dir.path().join("blocker");
    std::fs::write(&blocker, b"x").unwrap();
    w.ctx.state_root = blocker.join("state");
    let named = format!("{} is not a real folder", blocker.display());
    for e in [
        read(&w.ctx, T).unwrap_err(),
        every(&w.ctx).unwrap_err(),
        write(&w.ctx, &ix).unwrap_err(),
    ] {
        assert_eq!((e.code, e.check, e.message), (1, "index", named.clone()));
    }
}

// R15: a metadata error other than a missing name (permission denied) on `up/`, or on an ancestor while `up/` is
// missing, is 1 `index` naming that path for reads, scans and writes; never an absent index.
#[test]
fn an_unreadable_up_or_ancestor_is_index_never_absent() {
    let mut w = World::human("");
    let ix = full(&w, &here(&w));
    write(&w.ctx, &ix).unwrap();
    let up = w.ctx.state_root.join("up");
    fail_metadata(Some(&up));
    let named = format!("{} cannot be read", up.display());
    for e in [
        read(&w.ctx, T).unwrap_err(),
        every(&w.ctx).unwrap_err(),
        write(&w.ctx, &ix).unwrap_err(),
    ] {
        assert_eq!(
            (e.code, e.check, e.message.as_str()),
            (1, "index", named.as_str()),
            "up/"
        );
    }
    let fresh = w._dir.path().join("fresh");
    std::fs::create_dir(&fresh).unwrap();
    w.ctx.state_root = fresh.join("state");
    fail_metadata(Some(&w.ctx.state_root));
    let named = format!("{} cannot be read", w.ctx.state_root.display());
    for e in [
        read(&w.ctx, T).unwrap_err(),
        every(&w.ctx).unwrap_err(),
        write(&w.ctx, &ix).unwrap_err(),
    ] {
        assert_eq!(
            (e.code, e.check, e.message.as_str()),
            (1, "index", named.as_str()),
            "an ancestor"
        );
    }
    fail_metadata(None);
}

// Ambiguity 3: live only with the recorded start and a key hashing its own fields; gone on no row or another
// start. `every` reads each `<16 hex>.json` only; a torn one fails the scan naming its path (calls 2, 14).
#[test]
fn servers_are_proven_and_every_skips_release_and_temp_files() {
    let w = World::human("");
    let s = here(&w);
    assert_eq!(prove(&w.ctx, &s).unwrap(), Proof::Live);
    let pipe = format!("{0}{0}.{0}pipe{0}rmux-x", char::from(92u8));
    for socket in ["/tmp/tmux-501/default", pipe.as_str()] {
        let other = Server {
            socket: socket.into(),
            pid: 5252,
            start: 7,
            key: key_of(socket, 5252, 7),
        };
        assert_eq!(
            prove(&w.ctx, &other).unwrap(),
            Proof::Gone,
            "{socket}: no such pid"
        );
        w.fakes.process(crate::testing::row(5252, 1, 7));
        assert_eq!(prove(&w.ctx, &other).unwrap(), Proof::Live, "{socket}");
        let restarted = Server {
            start: 8,
            key: key_of(socket, 5252, 8),
            ..other
        };
        assert_eq!(
            prove(&w.ctx, &restarted).unwrap(),
            Proof::Gone,
            "{socket}: restarted"
        );
        w.fakes.forget(5252);
    }
    let wrong = Server {
        key: "ffffffffffffffff".into(),
        ..s.clone()
    };
    assert_eq!(prove(&w.ctx, &wrong).unwrap_err().check, "index");
    write(&w.ctx, &full(&w, &s)).unwrap();
    let up = w.ctx.state_root.join("up");
    std::fs::write(up.join("0123456789abcdef0123456789abcdef.release"), b"{}").unwrap();
    std::fs::write(up.join(format!("{T}.json.abcd.tmp")), b"{").unwrap();
    assert_eq!(every(&w.ctx).unwrap().len(), 1);
    let torn = up.join("ffffffffffffffff.json");
    std::fs::write(&torn, b"{").unwrap();
    let e = every(&w.ctx).unwrap_err();
    let named = format!(
        "the team index cannot be read; repair or remove {}",
        torn.display()
    );
    assert_eq!((e.check, e.message), ("index", named));
}
