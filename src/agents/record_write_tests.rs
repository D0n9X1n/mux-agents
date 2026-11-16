//! Call 19: `Guard` and `Tx::record_write`. One row per guard condition, one per flush; the one read under the lock.
use super::*;
use crate::agents::fixture::{ID, World};
use serde_json::json;
use std::cell::Cell;

struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        crate::state::fs::fail_sync_dir(None);
        crate::state::fs::fail_sync_file(None);
    }
}

fn world() -> World {
    let w = World::human("");
    w.state();
    w
}

/// R-W3: by check and stable phrase.
fn outcome(r: &Res<Option<Value>>) -> &'static str {
    match r {
        Ok(Some(_)) => "wrote",
        Ok(None) => "refused",
        Err(e) if e.check == "evidence" => "evidence",
        Err(e) if (e.check, e.message.as_str()) == ("state", NOT_WRITTEN) => "not written",
        Err(_) => "other",
    }
}

/// The write and whether its patch ran.
fn write_seen(w: &World, guard: Guard<'_>) -> (Res<Option<Value>>, bool) {
    let s = Scope::of(&w.caller());
    let ran = Cell::new(false);
    let got = open(&w.ctx, &s, |tx| {
        tx.record_write(ID, guard, |f| {
            ran.set(true);
            f.insert("x".into(), json!(1));
            Ok(())
        })
    });
    (got, ran.get())
}

fn write(w: &World, guard: Guard<'_>) -> Res<Option<Value>> {
    write_seen(w, guard).0
}

fn plant(w: &World, bytes: &[u8]) {
    let f = w.folder(ID);
    crate::agents::spawn::private_folder(&w.ctx, &f).unwrap();
    std::fs::write(f.join("record.json"), bytes).unwrap();
}

/// A lifecycle record with the given `pending` and `completing`.
fn life(pending: Option<Value>, completing: Option<Value>) -> Value {
    let mut r = json!({"lifecycle": 1, "id": ID, "name": "worker", "generation": "0"});
    if let Some(p) = pending {
        r["pending"] = p;
    }
    if let Some(c) = completing {
        r["completing"] = c;
    }
    r
}

fn p(n: u64, name: &str) -> Option<Value> {
    Some(json!({"gen": n, "hcName": name}))
}

/// Pre-lifecycle, but naming everything a guard looks for, so only the lifecycle check refuses it.
fn pre(pending: bool) -> Value {
    let mut r = json!({"id": ID, "name": "worker", "completing": {"gen": 0}});
    if pending {
        r["pending"] = json!({"gen": 1, "hcName": "m1"});
    }
    r
}

/// The same record naming another member (call 35).
fn other(mut r: Value) -> Value {
    r["id"] = json!("bbbbbbbbbbbb");
    r
}

/// (row, guard, planted record, answer). Planted `None`: no record; `"torn"`: torn bytes.
type Row = (&'static str, Guard<'static>, Option<Value>, &'static str);
fn rows() -> Vec<Row> {
    let own = Guard::Pending {
        n: 1,
        hc_name: "m1",
    };
    let none = Guard::PendingGen { n: None };
    let one = Guard::PendingGen { n: Some(1) };
    let done = Guard::CompletingGen { n: 0 };
    vec![
        ("create, no record", Guard::Create, None, "wrote"),
        (
            "create, a record",
            Guard::Create,
            Some(life(None, None)),
            "refused",
        ),
        ("pending, no record", own, None, "refused"),
        ("pending, owned", own, Some(life(p(1, "m1"), None)), "wrote"),
        (
            "pending, another generation",
            own,
            Some(life(p(2, "m1"), None)),
            "refused",
        ),
        (
            "pending, another name",
            own,
            Some(life(p(1, "m2"), None)),
            "refused",
        ),
        (
            "pending, no gen",
            own,
            Some(life(Some(json!({"hcName": "m1"})), None)),
            "refused",
        ),
        (
            "pending, no hcName",
            own,
            Some(life(Some(json!({"gen": 1})), None)),
            "refused",
        ),
        ("pending, pre-lifecycle", own, Some(pre(true)), "refused"),
        (
            "pending gen none, none pending",
            none,
            Some(life(None, None)),
            "wrote",
        ),
        (
            "pending gen none, a pending",
            none,
            Some(life(p(1, "m1"), None)),
            "refused",
        ),
        (
            "pending gen none, a null pending",
            none,
            Some(life(Some(Value::Null), None)),
            "refused",
        ),
        ("pending gen none, no record", none, None, "refused"),
        (
            "pending gen none, pre-lifecycle",
            none,
            Some(pre(false)),
            "refused",
        ),
        (
            "pending gen, owned",
            one,
            Some(life(p(1, "m9"), None)),
            "wrote",
        ),
        (
            "pending gen, another generation",
            one,
            Some(life(p(2, "m1"), None)),
            "refused",
        ),
        (
            "pending gen, no pending",
            one,
            Some(life(None, None)),
            "refused",
        ),
        (
            "pending gen, pre-lifecycle",
            one,
            Some(pre(true)),
            "refused",
        ),
        (
            "completing, owned",
            done,
            Some(life(None, Some(json!({"gen": 0})))),
            "wrote",
        ),
        (
            "completing, another generation",
            done,
            Some(life(None, Some(json!({"gen": 1})))),
            "refused",
        ),
        ("completing, none", done, Some(life(None, None)), "refused"),
        (
            "completing, pre-lifecycle",
            done,
            Some(pre(false)),
            "refused",
        ),
        (
            "create, another id",
            Guard::Create,
            Some(other(life(None, None))),
            "refused",
        ),
        (
            "pending, another id",
            own,
            Some(other(life(p(1, "m1"), None))),
            "refused",
        ),
        (
            "pending gen none, another id",
            none,
            Some(other(life(None, None))),
            "refused",
        ),
        (
            "pending gen, another id",
            one,
            Some(other(life(p(1, "m9"), None))),
            "refused",
        ),
        (
            "completing, another id",
            done,
            Some(other(life(None, Some(json!({"gen": 0}))))),
            "refused",
        ),
        ("torn", own, Some(json!("torn")), "evidence"),
    ]
}

// R-W2 guard conditions (findings 1to2-I5 to I8): each condition removed alone fails one row. A written record
// carries the patch, `lifecycle: 1` and its id, and is what the call answers; anything else leaves the bytes as
// they were (no record stays no record) and never calls the patch.
#[test]
fn every_guard_condition_alone_fails_one_row() {
    for (row, guard, planted, want) in rows() {
        let w = world();
        match &planted {
            Some(v) if v == &json!("torn") => plant(&w, b"{\"lifec"),
            Some(v) => plant(&w, v.to_string().as_bytes()),
            None => {}
        }
        let at = w.folder(ID).join("record.json");
        let before = std::fs::read(&at).ok();
        let (got, patched) = write_seen(&w, guard);
        assert_eq!(outcome(&got), want, "{row}: {got:?}");
        assert_eq!(
            patched,
            want == "wrote",
            "{row}: the patch runs only when the guard admits"
        );
        if want == "wrote" {
            let stored = crate::state::fs::read_json(&at).unwrap().unwrap();
            assert_eq!(
                (&stored["x"], &stored["lifecycle"], &stored["id"]),
                (&json!(1), &json!(1), &json!(ID)),
                "{row}"
            );
            assert_eq!(
                got.unwrap(),
                Some(stored),
                "{row}: the answer is the record written"
            );
        } else {
            assert_eq!(std::fs::read(&at).ok(), before, "{row}: nothing written");
        }
    }
}

// R5 (finding 1to2-C3), every OS: a member folder that is a link refuses every guard; nothing is written through
// it. The real folder holds a record each guard would admit.
#[test]
fn a_link_at_the_member_folder_refuses_every_guard() {
    let cases = [
        (
            "pending",
            Guard::Pending {
                n: 1,
                hc_name: "m1",
            },
            Some(life(p(1, "m1"), None)),
        ),
        (
            "pending gen none",
            Guard::PendingGen { n: None },
            Some(life(None, None)),
        ),
        (
            "pending gen",
            Guard::PendingGen { n: Some(1) },
            Some(life(p(1, "m1"), None)),
        ),
        (
            "completing",
            Guard::CompletingGen { n: 0 },
            Some(life(None, Some(json!({"gen": 0})))),
        ),
        ("create", Guard::Create, None),
    ];
    for (case, guard, real_record) in cases {
        let w = world();
        let real = w.folder("bbbbbbbbbbbb");
        crate::agents::spawn::private_folder(&w.ctx, &real).unwrap();
        if let Some(r) = &real_record {
            std::fs::write(real.join("record.json"), r.to_string()).unwrap();
        }
        let before = std::fs::read(real.join("record.json")).ok();
        crate::testing::link_dir(&real, &w.folder(ID));
        let got = write(&w, guard);
        assert_eq!(outcome(&got), "evidence", "{case}: {got:?}");
        assert_eq!(
            std::fs::read(real.join("record.json")).ok(),
            before,
            "{case}: nothing through the link"
        );
    }
}

// R-W2, finding 1to2-I4: record.json is read once, through the walk, and nothing else is read. The capture holds
// the writer alone: `open`'s replay hooks (group B's `release::complete`) read member records before the body.
#[test]
fn a_write_reads_the_record_exactly_once() {
    let w = world();
    plant(&w, life(p(1, "m1"), None).to_string().as_bytes());
    let s = Scope::of(&w.caller());
    let f = w.folder(ID);
    let own = Guard::Pending {
        n: 1,
        hc_name: "m1",
    };
    let (got, reads) = open(&w.ctx, &s, |tx| {
        Ok(crate::state::fs::record_reads(|| {
            tx.record_write(ID, own, |_| Ok(()))
        }))
    })
    .unwrap();
    assert_eq!(outcome(&got), "wrote", "{got:?}");
    assert_eq!(reads, [f.join("record.json")], "one walk-checked read");
}

// R-W2, call 15: the guard judges the record as it is while the member lock is held. Another writer takes the
// pending at the moment the lock is acquired; the write is refused and that record stands.
#[test]
fn the_guard_reads_inside_the_member_lock() {
    let w = world();
    let f = w.folder(ID);
    plant(&w, life(p(1, "m1"), None).to_string().as_bytes());
    let theirs = life(p(1, "m2"), None);
    let key = format!("+{}", Scope::of(&w.caller()).member_lock(ID));
    let (at, body) = (f.join("record.json"), theirs.to_string());
    w.fakes.lock_probe(move |event| {
        if event == key {
            std::fs::write(&at, &body).unwrap();
        }
        String::new()
    });
    let got = write(
        &w,
        Guard::Pending {
            n: 1,
            hc_name: "m1",
        },
    );
    assert_eq!(outcome(&got), "refused", "{got:?}");
    let stored = crate::state::fs::read_json(&f.join("record.json"))
        .unwrap()
        .unwrap();
    assert_eq!(
        stored, theirs,
        "the record taken while the lock was held stands"
    );
}

// R-W2 flushes: the content before the rename (the old record stands), the member folder after it (the rename
// landed), and agents after a created record. Each failure is NOT_WRITTEN.
#[test]
fn every_flush_alone_fails_one_row() {
    for at in ["content", "member", "agents"] {
        let w = world();
        let _reset = Reset;
        let f = w.folder(ID);
        let create = at == "agents";
        if !create {
            plant(&w, life(p(1, "m1"), None).to_string().as_bytes());
        }
        let record = f.join("record.json");
        let before = std::fs::read(&record).ok();
        match at {
            "content" => crate::state::fs::fail_sync_file(Some(&record)),
            "member" => crate::state::fs::fail_sync_dir(Some(&f)),
            _ => crate::state::fs::fail_sync_dir(Some(f.parent().unwrap())),
        }
        let guard = if create {
            Guard::Create
        } else {
            Guard::Pending {
                n: 1,
                hc_name: "m1",
            }
        };
        let got = write(&w, guard);
        assert_eq!(outcome(&got), "not written", "{at}: {got:?}");
        let after = std::fs::read(&record).ok();
        if at == "content" {
            assert_eq!(after, before, "{at}: the old record stands");
        } else {
            assert_ne!(after, before, "{at}: the rename landed, its flush failed");
        }
    }
}

// A patch error is returned and nothing is written; a refused create makes no folder.
#[test]
fn a_patch_error_writes_nothing() {
    for create in [false, true] {
        let w = world();
        if !create {
            plant(&w, life(p(1, "m1"), None).to_string().as_bytes());
        }
        let f = w.folder(ID);
        let before = std::fs::read(f.join("record.json")).ok();
        let guard = if create {
            Guard::Create
        } else {
            Guard::Pending {
                n: 1,
                hc_name: "m1",
            }
        };
        let s = Scope::of(&w.caller());
        let got = open(&w.ctx, &s, |tx| {
            tx.record_write(ID, guard, |_| Err(Refusal::internal("patch")))
        });
        assert_eq!(
            got.unwrap_err().message,
            "unexpected error (patch)",
            "{create}"
        );
        assert_eq!(
            std::fs::read(f.join("record.json")).ok(),
            before,
            "{create}: nothing written"
        );
        assert_eq!(
            f.exists(),
            !create,
            "{create}: no folder for a create that wrote nothing"
        );
    }
}

// Call 28 (b-call 28): `record_write` takes the member lock inside the caller's team lock. The write adds exactly
// +team, +member, -member, -team to the lock log, in that order (setup locks are sliced off).
#[test]
fn the_member_lock_is_taken_inside_the_team_lock() {
    let w = world();
    plant(&w, life(p(1, "m1"), None).to_string().as_bytes());
    let s = Scope::of(&w.caller());
    let before = w.fakes.locks().len();
    let got = write(
        &w,
        Guard::Pending {
            n: 1,
            hc_name: "m1",
        },
    );
    assert_eq!(outcome(&got), "wrote", "{got:?}");
    let (team, member) = (s.team_lock(), s.member_lock(ID));
    let want = [
        format!("+{team}"),
        format!("+{member}"),
        format!("-{member}"),
        format!("-{team}"),
    ];
    assert_eq!(
        w.fakes.locks()[before..],
        want,
        "the member lock nests inside the team lock"
    );
}
