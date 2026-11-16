//! W1, PM call 20: the view writer and reader agree on the subject generation.
use super::tests::{record, world};
use super::*;
use crate::agents::fixture::{ID, World};
use crate::agents::gens;
use crate::state::evidence::publish;

/// `presented` as a reader calls it: one `member_of`, one `status`.
pub(super) fn try_present(w: &World) -> Res<Value> {
    match gens::member_of(&w.folder(ID)) {
        gens::Member::Life(g) => presented(&g, &g.status()?),
        other => panic!("not a lifecycle member: {other:?}"),
    }
}
pub(super) fn present(w: &World) -> Value {
    try_present(w).unwrap()
}
fn check(w: &World, s: &Scope, v: &Value) -> Option<(u8, &'static str)> {
    view(&w.ctx, s, ID, v).map_err(|e| (e.code, e.check)).err()
}

/// Makes `ID`'s lifecycle record active at generation `n` (address `t<n>`, hcName `m<n as 8 hex>`), publishes that
/// generation's launch.json, and answers its view descriptor (the A3b adapter's shape).
pub(super) fn at_gen(w: &World, s: &Scope, n: u64) -> Value {
    let name = format!("m{n:08x}");
    let (g, a) = (json!(n.to_string()), json!(format!("t{n}")));
    open(&w.ctx, s, |tx| match tx.record(ID)? {
        None => tx.create(ID, json!({"generation": g, "hcAddress": a, "hcName": name})),
        Some(_) => tx.patch(
            ID,
            &[
                ("generation", g.clone()),
                ("hcAddress", a.clone()),
                ("hcName", json!(name)),
            ],
            &[],
        ),
    })
    .unwrap();
    publish(
        &gens::path(&w.folder(ID), n, "launch"),
        &json!({"hcName": name}),
    )
    .unwrap();
    json!({"serverKey": s.key, "session": s.session, "pane": "%9", "window": "@5",
        "startCommand": format!("hc attach t{n}"), "gen": n, "hcAddress": format!("t{n}"), "hcName": name})
}

// W1 (a280e7 CRITICAL 1): an active subject accepts only its own descriptor; gen, hcAddress and launch.json's
// hcName each refuse alone, and nothing is written.
#[test]
fn a_view_of_another_generation_is_never_written() {
    let (w, s) = world();
    let old = at_gen(&w, &s, 0);
    let new = at_gen(&w, &s, 1);
    let at = w.folder(ID).join("view.json");
    assert_eq!(
        check(&w, &s, &old),
        Some((2, "target")),
        "generation 0's view for generation 1"
    );
    assert!(!at.exists(), "nothing written");
    for (k, bad) in [
        ("gen", json!(0)),
        ("hcAddress", json!("t0")),
        ("hcName", json!("m00000000")),
    ] {
        let mut v = new.clone();
        v[k] = bad;
        assert_eq!(check(&w, &s, &v), Some((2, "target")), "{k}");
        assert!(!at.exists(), "{k}: nothing written");
    }
    // The record's own hcName is never the reference: launch.json's is.
    open(&w.ctx, &s, |tx| {
        tx.patch(ID, &[("hcName", json!("m12345678"))], &[])
            .map(|_| ())
    })
    .unwrap();
    assert_eq!(check(&w, &s, &new), None, "launch.json's hcName");
    let r = present(&w);
    assert_eq!((&r["pane"], &r["window"]), (&json!("%9"), &json!("@5")));
}

// R2, W1: an unreadable record or launch.json refuses `evidence`; a missing launch.json names no hcName (`target`).
#[test]
fn an_unreadable_member_refuses_the_view() {
    let (w, s) = world();
    let v = at_gen(&w, &s, 0);
    let f = w.folder(ID);
    std::fs::write(gens::path(&f, 0, "launch"), b"{\"hc").unwrap();
    assert_eq!(check(&w, &s, &v), Some((1, "evidence")), "torn launch.json");
    std::fs::remove_file(gens::path(&f, 0, "launch")).unwrap();
    assert_eq!(check(&w, &s, &v), Some((2, "target")), "no launch.json");
    std::fs::write(f.join("record.json"), b"{\"lifec").unwrap();
    assert_eq!(check(&w, &s, &v), Some((1, "evidence")), "torn record");
    assert!(!f.join("view.json").exists());
}

// Ambiguity 1: a pre-lifecycle member keeps its view in record.json; only pane, window, startCommand change, and
// only for its own generation and address.
#[test]
fn a_pre_lifecycle_view_sets_only_its_view_fields() {
    let (w, s) = world();
    let f = w.agent_record(
        ID,
        json!({"generation": "0", "hcAddress": "taaaaaaaaaaaa", "parentAgent": "bbbbbbbbbbbb"}),
    );
    let before = record(&w);
    let v = json!({"serverKey": s.key, "session": s.session, "pane": "%19", "window": "@8",
        "startCommand": "hc attach taaaaaaaaaaaa", "gen": 0, "hcAddress": "taaaaaaaaaaaa", "hcName": "maaaaaaaa"});
    view(&w.ctx, &s, ID, &v).unwrap();
    let mut want = before.clone();
    for k in VIEW_FIELDS {
        want[k] = v[k].clone();
    }
    assert_eq!(record(&w), want, "only the three view fields");
    assert!(
        !f.join("view.json").exists(),
        "a pre-lifecycle member has no view.json"
    );
    for (k, bad) in [("gen", json!(1)), ("hcAddress", json!("tother"))] {
        let mut x = v.clone();
        x[k] = bad;
        assert_eq!(check(&w, &s, &x), Some((2, "target")), "{k}");
        assert_eq!(record(&w), want, "{k}: unchanged");
    }
}

// v11:25 (kept from resume's focus test, whose member is lifecycle now): a pre-lifecycle record rewrite is durable.
// A failed flush of the member folder after the write is reported; the record on disk is whole JSON with the new
// view, and no temp file is left. With the flush working, the write succeeds.
#[test]
fn a_pre_lifecycle_view_is_rewritten_durably() {
    let (w, s) = world();
    let f = w.agent_record(
        ID,
        json!({"generation": "0", "hcAddress": "taaaaaaaaaaaa", "resumedFrom": "bbbbbbbbbbbb"}),
    );
    let v = json!({"serverKey": s.key, "session": s.session, "pane": "%19", "window": "@8",
        "startCommand": "hc attach taaaaaaaaaaaa", "gen": 0, "hcAddress": "taaaaaaaaaaaa", "hcName": "maaaaaaaa"});
    crate::state::fs::fail_sync_dir(Some(&f));
    let got = view(&w.ctx, &s, ID, &v);
    crate::state::fs::fail_sync_dir(None);
    assert!(got.is_err(), "the failed flush is reported: {got:?}");
    let r = record(&w);
    assert_eq!(
        (&r["pane"], &r["resumedFrom"]),
        (&json!("%19"), &json!("bbbbbbbbbbbb")),
        "whole JSON"
    );
    let left: Vec<_> = std::fs::read_dir(&f)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    view(&w.ctx, &s, ID, &v).unwrap();
}

// A3a's contract for `presented`, kept by the writer: a torn active launch.json refuses `evidence` in reader and
// writer, even beside a stale view whose gen and hcAddress already differ.
#[test]
fn a_torn_active_launch_refuses_beside_a_stale_view() {
    let (w, s) = world();
    let old = at_gen(&w, &s, 0);
    let _ = at_gen(&w, &s, 1);
    let f = w.folder(ID);
    crate::state::fs::write_json(&f.join("view.json"), &old).unwrap();
    std::fs::write(gens::path(&f, 1, "launch"), b"{\"hc").unwrap();
    assert_eq!(
        try_present(&w).map_err(|e| (e.code, e.check)).err(),
        Some((1, "evidence")),
        "reader"
    );
    assert_eq!(check(&w, &s, &old), Some((1, "evidence")), "writer");
}

// R1, W1 (checklist 7): the writer reads record.json once and calls `status` once: every piece of member evidence
// (record.json and each generation file) is read at most once, and record.json exactly once.
#[test]
fn the_view_writer_reads_the_member_once() {
    let (w, s) = world();
    let v = at_gen(&w, &s, 0);
    let f = w.folder(ID);
    let rec = f.join("record.json");
    let (got, reads) = crate::state::fs::record_reads(|| view(&w.ctx, &s, ID, &v));
    got.unwrap();
    assert_eq!(reads.iter().filter(|p| **p == rec).count(), 1, "{reads:?}");
    let (got, reads) = crate::state::evidence::record_evidence_reads(|| view(&w.ctx, &s, ID, &v));
    got.unwrap();
    for (file, want) in [
        (rec.clone(), 1),
        (gens::path(&f, 0, "launch"), 1),
        (gens::path(&f, 0, "commit"), 1),
        (gens::path(&f, 0, "ended"), 1),
    ] {
        let n = reads.iter().filter(|p| **p == file).count();
        assert_eq!(n, want, "{}: {reads:?}", file.display());
    }
    let mut seen = std::collections::HashSet::new();
    for p in &reads {
        assert!(seen.insert(p), "read twice: {}: {reads:?}", p.display());
    }
}

// PM calls 15, 23: an absent member gets no view: 2 `target`, nothing written.
#[test]
fn an_absent_member_gets_no_view() {
    let (w, s) = world();
    let v = json!({"pane": "%9", "gen": 0, "hcAddress": "t0", "hcName": "m00000000"});
    assert_eq!(check(&w, &s, &v), Some((2, "target")));
    assert!(!w.folder(ID).exists(), "nothing written");
}

// PM call 20: a held identity's view is its pending generation's: pending's gen, hcAddress and hcName. The active
// generation's view, and each field of the pending one changed alone, refuse; the reader presents the held view.
#[test]
fn a_held_identity_gets_only_its_pending_view() {
    let (w, s) = world();
    let active = at_gen(&w, &s, 0);
    let pending = json!({"gen": 1, "kind": "spawn", "hcName": "m00000001", "sessionId": "s1", "hcAddress": "t1"});
    open(&w.ctx, &s, |tx| {
        tx.patch(ID, &[("pending", pending)], &[]).map(|_| ())
    })
    .unwrap();
    let at = w.folder(ID).join("view.json");
    assert_eq!(
        check(&w, &s, &active),
        Some((2, "target")),
        "the active generation's view is not the held one's"
    );
    assert!(!at.exists(), "nothing written");
    let held = json!({"serverKey": s.key, "session": s.session, "pane": "%7", "window": "@3",
        "startCommand": "hc attach t1", "gen": 1, "hcAddress": "t1", "hcName": "m00000001"});
    for (k, bad) in [
        ("gen", json!(0)),
        ("hcAddress", json!("t0")),
        ("hcName", json!("m00000000")),
    ] {
        let mut v = held.clone();
        v[k] = bad;
        assert_eq!(check(&w, &s, &v), Some((2, "target")), "held {k}");
        assert!(!at.exists(), "held {k}: nothing written");
    }
    assert_eq!(
        check(&w, &s, &held),
        None,
        "the pending generation's own view"
    );
    let r = present(&w);
    assert_eq!(
        (&r["pane"], &r["window"]),
        (&json!("%7"), &json!("@3")),
        "the reader presents the held view"
    );
}

// W1: a member with no subject (no active generation; its pending one void and done) gets no view.
#[test]
fn no_subject_gets_no_view() {
    let (w, s) = world();
    let pending = json!({"gen": 1, "kind": "spawn", "hcName": "m00000001", "sessionId": "s1", "hcAddress": "t1"});
    open(&w.ctx, &s, |tx| {
        tx.create(ID, json!({"pending": pending})).map(|_| ())
    })
    .unwrap();
    publish(
        &gens::path(&w.folder(ID), 1, "commit"),
        &json!({"aborted": true}),
    )
    .unwrap();
    let v = json!({"pane": "%9", "window": "@5", "startCommand": "hc attach t1", "gen": 1,
        "hcAddress": "t1", "hcName": "m00000001"});
    assert_eq!(check(&w, &s, &v), Some((2, "target")));
    assert!(!w.folder(ID).join("view.json").exists(), "nothing written");
}

// state-0 A-1: a view writer paused right after its one read; a patch of parentAgent arrives then and waits on the
// member lock; both survive. The pre-lifecycle arm is the one that rewrites record.json.
#[test]
fn a_paused_view_writer_and_a_patch_both_survive() {
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};
    let (w, s) = world();
    w.agent_record(
        ID,
        json!({"generation": "0", "hcAddress": "taaaaaaaaaaaa", "parentAgent": null}),
    );
    let gate = Arc::new(crate::testing::Gate::default());
    w.fakes.share_locks(gate.clone());
    let v = json!({"pane": "%19", "window": "@8", "startCommand": "hc attach taaaaaaaaaaaa", "gen": 0,
        "hcAddress": "taaaaaaaaaaaa", "hcName": "maaaaaaaa"});
    let (root, scope, key) = (w.ctx.state_root.clone(), s.clone(), s.member_lock(ID));
    let (read_tx, read_rx) = mpsc::channel();
    let g = gate.clone();
    let writer = std::thread::spawn(move || {
        let mut other = World::human("");
        other.ctx.state_root = root;
        other.fakes.share_locks(g.clone());
        after_read(Some(Box::new(move || {
            read_tx.send(()).unwrap();
            let end = Instant::now() + Duration::from_secs(3);
            while g.waiting(&key) == 0 {
                assert!(
                    Instant::now() < end,
                    "the patch never waited on the member lock"
                );
                std::thread::yield_now();
            }
        })));
        let got = view(&other.ctx, &scope, ID, &v);
        after_read(None);
        got
    });
    read_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    open(&w.ctx, &s, |tx| {
        tx.patch(ID, &[("parentAgent", json!("bbbbbbbbbbbb"))], &[])
            .map(|_| ())
    })
    .unwrap();
    writer.join().unwrap().unwrap();
    let r = record(&w);
    assert_eq!(
        (&r["pane"], &r["parentAgent"]),
        (&json!("%19"), &json!("bbbbbbbbbbbb")),
        "both survive"
    );
    assert_eq!(gate.waited(&s.member_lock(ID)), 1);
}

/// a3c call 47 (TL #149, MEDIUM): `view.json` written directly, as a stale or forged file would be, for the reader.
fn put_view(w: &World, v: &Value) {
    crate::state::fs::write_json(&w.folder(ID).join("view.json"), v).unwrap();
}

// Call 47: an empty hcName on launch.json and on the view is no ownership. The writer refuses and writes nothing;
// the reader presents nothing.
#[test]
fn an_empty_hc_name_owns_no_view() {
    let (w, s) = world();
    open(&w.ctx, &s, |tx| {
        tx.create(
            ID,
            json!({"generation": "0", "hcAddress": "t0", "hcName": ""}),
        )
        .map(|_| ())
    })
    .unwrap();
    publish(
        &gens::path(&w.folder(ID), 0, "launch"),
        &json!({"hcName": ""}),
    )
    .unwrap();
    let v = json!({"serverKey": s.key, "session": s.session, "pane": "%9", "window": "@5",
        "startCommand": "hc attach t0", "gen": 0, "hcAddress": "t0", "hcName": ""});
    assert_eq!(check(&w, &s, &v), Some((2, "target")), "the writer");
    assert!(!w.folder(ID).join("view.json").exists(), "nothing written");
    put_view(&w, &v);
    assert_eq!(
        present(&w)["pane"],
        Value::Null,
        "the reader presents nothing"
    );
}

// Call 47: a held pending generation and its view with the same empty, or the same null, hcAddress are no ownership.
#[test]
fn an_empty_or_null_held_address_owns_no_view() {
    for address in [json!(""), Value::Null] {
        let (w, s) = world();
        at_gen(&w, &s, 0);
        let pending = json!({"gen": 1, "kind": "spawn", "hcName": "m00000001", "sessionId": "s1",
            "hcAddress": address});
        open(&w.ctx, &s, |tx| {
            tx.patch(ID, &[("pending", pending)], &[]).map(|_| ())
        })
        .unwrap();
        let v = json!({"serverKey": s.key, "session": s.session, "pane": "%7", "window": "@3",
            "startCommand": "hc attach t1", "gen": 1, "hcAddress": address, "hcName": "m00000001"});
        assert_eq!(
            check(&w, &s, &v),
            Some((2, "target")),
            "{address}: the writer"
        );
        assert!(
            !w.folder(ID).join("view.json").exists(),
            "{address}: nothing written"
        );
        put_view(&w, &v);
        assert_eq!(
            present(&w)["pane"],
            Value::Null,
            "{address}: the reader presents nothing"
        );
    }
}

// Call 47: a pre-lifecycle record and a descriptor that both lack hcAddress (None == None) are no ownership; the record
// is not rewritten.
#[test]
fn a_pre_lifecycle_view_needs_an_address_on_both_sides() {
    let (w, s) = world();
    w.agent_record(ID, json!({"generation": "0"}));
    let before = record(&w);
    assert!(
        before.get("hcAddress").is_none(),
        "the record has no address"
    );
    let v = json!({"serverKey": s.key, "session": s.session, "pane": "%19", "window": "@8",
        "startCommand": "hc attach x", "gen": 0, "hcName": "maaaaaaaa"});
    assert_eq!(
        check(&w, &s, &v),
        Some((2, "target")),
        "no address on either side"
    );
    assert_eq!(record(&w), before, "the record is not rewritten");
}
