//! settle-1b: the replay table (G3), the orphan fence (G8), and ending one generation (G11).
use std::path::Path;

use serde_json::{Value, json};

use super::gens::{self, Decision, Found};
use super::lifecycle::{Life, classify};
use super::release::{self, Cause, halt};
use super::team_state::{Guard, Tx};
use crate::error::{Refusal, Res};
use crate::hc::{Lazy, State, Terminal};
use crate::state::evidence::{Evidence, Published};
use crate::util::utc_o;

/// G3's two modes: `Normal` promotes a committed pending generation; `Fence` (G8, G11) never does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Fence,
}

/// What one settle decided (B-R4).
#[derive(Debug, Clone)]
pub enum Outcome {
    Done,
    Promoted,
    Running(Terminal),
    Unknown(Refusal),
    /// G3 row 6: no terminal matches the generation's recorded identity (call 60), with the launch's hcName.
    /// Reserved like Unknown; the only outcome `terminate` may take as a kill hc forgot.
    Missing(String, Refusal),
}

impl PartialEq for Outcome {
    fn eq(&self, o: &Outcome) -> bool {
        match (self, o) {
            (Outcome::Done, Outcome::Done) => true,
            (Outcome::Promoted, Outcome::Promoted) => true,
            (Outcome::Running(a), Outcome::Running(b)) => a.address == b.address,
            (Outcome::Missing(a, x), Outcome::Missing(b, y)) => a == b && x.check == y.check,
            (Outcome::Unknown(a), Outcome::Unknown(b)) => a.check == b.check,
            _ => false,
        }
    }
}

#[cfg(test)]
thread_local! {
    static RACE: std::cell::Cell<Option<fn(&Path)>> = const { std::cell::Cell::new(None) };
}
/// Test only: `f` runs once, between a row's read and its publish (rows 3 and 4, "lost: reread").
#[cfg(test)]
pub fn race_before_publish(f: Option<fn(&Path)>) {
    RACE.with(|r| r.set(f));
}
fn race(_member: &Path) {
    #[cfg(test)]
    if let Some(f) = RACE.with(|r| r.take()) {
        f(_member);
    }
}

/// One generation being settled. `Tx` and `Lazy` share the context lifetime `'c` (`Lazy` is invariant in it).
struct At<'a, 'c> {
    tx: &'a Tx<'c>,
    lazy: &'a Lazy<'c>,
    id: &'a str,
    n: u64,
    mode: Mode,
    pending: bool,
}

impl At<'_, '_> {
    /// G3 Done: drops `pending` only while it names n (`Guard::PendingGen`), so a replay drops it once.
    fn done(&self) -> Res<Outcome> {
        if self.pending {
            drop_field(
                self.tx,
                self.id,
                Guard::PendingGen { n: Some(self.n) },
                "pending",
            )?;
        }
        Ok(Outcome::Done)
    }
    /// G3 "lost: reread": the winner of a lost publication is read once more, through the whole table.
    fn again(&self) -> Res<Outcome> {
        settle_gen(self.tx, self.lazy, self.id, self.n, self.mode, self.pending)
    }
}

/// B-R2 over the one record writer: drops `field` while `guard` holds. A refused guard (`Ok(None)`) means the record
/// no longer names this generation there, so nothing is left to drop: a replay drops it once. A read or write
/// failure propagates.
fn drop_field(tx: &Tx<'_>, id: &str, guard: Guard<'_>, field: &'static str) -> Res<()> {
    tx.record_write(id, guard, |f| {
        f.remove(field);
        Ok(())
    })
    .map(|_| ())
}

/// B-R4: a continuation's refusal is Unknown (reserved); a halt (check `halted`, b-calls 36) stays an error.
fn unresolved(e: Refusal) -> Res<Outcome> {
    if e.check == "halted" {
        Err(e)
    } else {
        Ok(Outcome::Unknown(e))
    }
}

/// Call 63: the record read from `<agents>/<id>` must name `id`; another member's record there is Unreadable
/// evidence for this member, read before any generation evidence, publication or hc call.
fn bound(id: &str, r: &Value) -> Res<()> {
    if r.get("id").and_then(Value::as_str) == Some(id) {
        Ok(())
    } else {
        Err(crate::state::evidence::unreadable())
    }
}

impl Tx<'_> {
    /// G3 for the `pending` the record names now; none is Done (ambiguity 12). The record is read through
    /// `gens::record`, the walk from the state root (b-calls 34), so a linked member folder refuses `evidence`. No
    /// record (a forward killed at `allocated`) is Done with nothing to do (b-calls 35).
    pub fn settle(&self, id: &str, mode: Mode) -> Res<Outcome> {
        let Some(r) = gens::record(&self.folder(id)).known()? else {
            return Ok(Outcome::Done);
        };
        bound(id, &r)?;
        match gens::pending(&r) {
            None => Ok(Outcome::Done),
            Some(n) => settle_gen(self, &Lazy::new(self.ctx), id, n, mode, true),
        }
    }
}

/// Calls 49, 58 (calls 46, 57 extended to settle): `ended`, `released`, `release`, the claim or the commit read once through `gens::read_flushed`, so
/// rows 0 and 0r act on it only once its folders are durable. `Ok`: Present (flushed), Absent, or Unreadable (row 1
/// decides). `Err`: the flush failed (check `state`, `gens::NOT_FLUSHED`).
fn flushed(member: &Path, n: u64, file: &str) -> Res<Evidence> {
    let read = if file == "claim" {
        gens::claim_flushed(member, n)
    } else {
        gens::read_flushed(member, n, file)
    };
    match read {
        Ok(Some(v)) => Ok(Evidence::Present(v)),
        Ok(None) => Ok(Evidence::Absent),
        Err(e) if e.check == "evidence" => Ok(Evidence::Unreadable),
        Err(e) => Err(e),
    }
}

/// G3, total and in the spec's order, for generation n (`pending`: it is the record's `pending`). Each of the seven
/// files is read once (ambiguity 4); `ended`, `released`, `release` through `flushed` (call 49). Used by `settle`,
/// `fence` (G8) and `terminate` (G11).
pub fn settle_gen<'c>(
    tx: &Tx<'c>,
    lazy: &Lazy<'c>,
    id: &str,
    n: u64,
    mode: Mode,
    pending: bool,
) -> Res<Outcome> {
    let at = At {
        tx,
        lazy,
        id,
        n,
        mode,
        pending,
    };
    let m = tx.folder(id);
    // Call 49: a failed flush stops the row with nothing done.
    let ended = match flushed(&m, n, "ended") {
        Ok(e) => e,
        Err(e) => return Ok(Outcome::Unknown(e)),
    };
    let released = match flushed(&m, n, "released") {
        Ok(e) => e,
        Err(e) => return Ok(Outcome::Unknown(e)),
    };
    // Row 0: a marker is decisive, even beside a torn sibling.
    if matches!(ended, Evidence::Present(_)) || matches!(released, Evidence::Present(_)) {
        return at.done();
    }
    let rel = match flushed(&m, n, "release") {
        Ok(e) => e,
        Err(e) => return Ok(Outcome::Unknown(e)),
    };
    // Row 0r: before row 1; finished from step 3 with the body read here (b-calls 15), never by a kill.
    if let (Evidence::Absent, Evidence::Present(body)) = (&released, &rel) {
        return match release::conclude(tx, lazy, id, n, body) {
            Ok(()) => at.done(),
            Err(e) => unresolved(e),
        };
    }
    // Call 58: rows 2 and 5 act on the claim and the commit only once they are durable.
    let claim = match flushed(&m, n, "claim") {
        Ok(e) => e,
        Err(e) => return Ok(Outcome::Unknown(e)),
    };
    let commit = match flushed(&m, n, "commit") {
        Ok(e) => e,
        Err(e) => return Ok(Outcome::Unknown(e)),
    };
    let term = gens::read(&m, n, "terminating");
    let launch = gens::read(&m, n, "launch");
    // Row 1.
    let all = [&ended, &released, &rel, &claim, &commit, &term, &launch];
    if all.iter().any(|e| **e == Evidence::Unreadable) {
        return Ok(Outcome::Unknown(crate::state::evidence::unreadable()));
    }
    let claim = match &claim {
        Evidence::Present(c) => Some(c.as_str()),
        _ => None,
    };
    match claim {
        // Row 3.
        None => {
            race(&m);
            match gens::publish_claim_once(&m, n, &json!("abandoned"))? {
                Published::Published => {
                    halt("abandoned")?;
                    at.done()
                }
                Published::Exists => at.again(),
            }
        }
        // Row 2.
        Some(Some("abandoned")) => at.done(),
        Some(Some("run")) => match commit {
            // Row 4.
            Evidence::Absent => {
                race(&m);
                match gens::publish_once(&m, n, "commit", &json!({"aborted": true}))? {
                    Published::Published => {
                        halt("aborted")?;
                        at.done()
                    }
                    Published::Exists => at.again(),
                }
            }
            Evidence::Present(c) => match gens::decide(&c) {
                // Row 5.
                Decision::Aborted => at.done(),
                Decision::Run => row6(&at, &term, &launch),
                // Ambiguity 3.
                Decision::Invalid => Ok(Outcome::Unknown(Refusal::internal(
                    "the generation's commit is invalid",
                ))),
            },
            Evidence::Unreadable => Ok(Outcome::Unknown(crate::state::evidence::unreadable())),
        },
        // Ambiguity 3: a claim neither run nor abandoned.
        Some(_) => Ok(Outcome::Unknown(Refusal::internal(
            "the generation's claim is invalid",
        ))),
    }
}

/// G3 row 6: a committed generation, matched by `launch.json`'s identity only (launch-1 G12; ambiguities 1, 2, 5,
/// 6). Every value comes from `settle_gen`'s reads (A3a R1).
fn row6(at: &At<'_, '_>, term: &Evidence, launch: &Evidence) -> Res<Outcome> {
    let Evidence::Present(launch) = launch else {
        return Ok(Outcome::Unknown(Refusal::internal(
            "a run claim without launch.json",
        )));
    };
    // Ambiguity 5: terminating forbids promotion. Only `Tx::settle` passes Normal, always for pending.
    let normal = at.mode == Mode::Normal && !matches!(term, Evidence::Present(_));
    let hc_name = launch
        .get("hcName")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let argv: Vec<String> = launch
        .get("launcherArgv")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let Ok(core) = at.lazy.get() else {
        return Ok(Outcome::Unknown(gens::unclear("hc is unreachable")));
    };
    let t = match gens::find(core, at.id, at.n, hc_name) {
        Found::One(t) => t,
        Found::Unknown(e) => return Ok(Outcome::Unknown(e)),
        Found::Missing(e) => return Ok(Outcome::Missing(hc_name.to_owned(), e)),
    };
    // Ambiguity 2: the team `forward` gave `hc run`, never the record's.
    let Some(team) = t.labels.get("mux.team").cloned() else {
        return Ok(Outcome::Unknown(gens::unclear(
            "the terminal has no team label",
        )));
    };
    let now = utc_o(at.tx.ctx.clock.now());
    let folder = at.tx.folder(at.id);
    // `Ok(None)`: pending no longer names n. Under the team lock nothing else writes the record, so it is internal.
    let promote = |c: Option<Value>| -> Res<()> {
        at.tx
            .promote(at.id, at.n, launch, &t.address, &team, c)?
            .ok_or_else(|| {
                Refusal::internal("the pending generation changed under the team lock")
            })?;
        halt("promoted")
    };
    match (classify(Some(&t), &argv), normal) {
        (Life::Running, true) => promote(None).map(|()| Outcome::Promoted),
        (Life::Running, false) => Ok(Outcome::Running(t.clone())),
        (Life::Lost, true) => {
            let c =
                json!({"gen": at.n, "intent": "release", "cause": "restore", "address": t.address});
            promote(Some(c))?;
            let step = release::release_named(
                at.tx,
                at.lazy,
                at.id,
                at.n,
                &t.address,
                hc_name,
                Cause::Restore,
            );
            continued(at, step)
        }
        (Life::Ended, true) => {
            let facts = gens::end_facts(&t, hc_name, &now);
            promote(Some(
                json!({"gen": at.n, "intent": "ended", "facts": facts}),
            ))?;
            continued(
                at,
                gens::publish_end(&folder, at.n, &facts).and_then(|_| halt("ended")),
            )
        }
        (Life::Lost, false) => {
            match release::release_named(
                at.tx,
                at.lazy,
                at.id,
                at.n,
                &t.address,
                hc_name,
                Cause::Lost,
            ) {
                Ok(()) => at.done(),
                Err(e) => unresolved(e),
            }
        }
        // `find` checked name, mux.id, mux.gen and `classify` Ended: `record_end`'s guards, without its rereads.
        (Life::Ended, false) => {
            match gens::publish_end(&folder, at.n, &gens::end_facts(&t, hc_name, &now)) {
                Ok(_) => {
                    halt("ended")?;
                    at.done()
                }
                Err(e) => unresolved(e),
            }
        }
        (Life::Unknown, _) => Ok(Outcome::Unknown(gens::unclear(
            "hc does not show the generation's life",
        ))),
    }
}

/// Ambiguity 6, R4: after a promotion with `completing`, the continuation's result; Ok clears `completing` while it
/// names n. A failure keeps it, and the next `Tx::open` finishes it; the outcome stays `Promoted`. A halt stays Err.
fn continued(at: &At<'_, '_>, step: Res<()>) -> Res<Outcome> {
    let cleared = step
        .and_then(|()| drop_field(at.tx, at.id, Guard::CompletingGen { n: at.n }, "completing"));
    match cleared {
        Ok(()) => Ok(Outcome::Promoted),
        Err(e) if e.check == "halted" => Err(e),
        Err(_) => Ok(Outcome::Promoted),
    }
}

/// settle-1b G11: ends generation n of member `id`, under the team lock the caller holds. Never writes desired
/// state, touches another member, or promotes. `Ok`: n is done. `Err`: unresolved (no kill; or a kill hc refused);
/// a retry finishes it (G11.4). Callers: cascade-3d C6, restore-6c P2, A3b-2 Task 6 (stop).
/// Call 54 (call 22), call 55: `kill.json` (`{address, hcName, labels, at}`) is published right before `hc kill`; a kill hc accepts records the end at once,
/// with no further `hc get`; a replay that finds `kill.json` and hc `not_found` records the same end.
pub fn terminate(tx: &Tx<'_>, id: &str, n: u64) -> Res<()> {
    let folder = tx.folder(id);
    // b-calls 34: the walk-checked read; a linked `<id>` refuses `evidence`.
    let r = gens::record(&folder)
        .known()?
        .ok_or_else(|| Refusal::internal("the member has no record"))?;
    bound(id, &r)?;
    let pending = gens::pending(&r) == Some(n);
    let lazy = Lazy::new(tx.ctx);
    // G11.1, G11.2: Fence never promotes. Done (rows 0, 0r, 1-5; Lost released; Ended recorded) is Ok.
    let t = match settle_gen(tx, &lazy, id, n, Mode::Fence, pending)? {
        Outcome::Done | Outcome::Promoted => return Ok(()),
        // Call 60: only "no terminal matches" may be a kill hc forgot; every other Unknown stays unresolved.
        Outcome::Missing(hc_name, e) => return forgotten(tx, &lazy, id, n, &r, &hc_name, e),
        Outcome::Unknown(e) => return Err(e),
        Outcome::Running(t) => t,
    };
    if pending {
        gens::publish_once(
            &folder,
            n,
            "terminating",
            &json!({"at": utc_o(tx.ctx.clock.now())}),
        )?;
        halt("terminating")?;
    }
    let core = lazy.get()?;
    // G11.3, b-calls 12: the exact terminal matched, reread immediately before the kill, nothing between them.
    let now = core
        .get(&t.address)
        .map_err(|_| gens::unclear("hc cannot reread the terminal"))?;
    if !same_running(&t, &now) {
        return Err(gens::unclear("the terminal changed before the kill"));
    }
    let hc_name = t.name.clone().unwrap_or_default();
    // Call 54: the recorded attempt, then the kill.
    gens::publish_once(
        &folder,
        n,
        "kill",
        &json!({"address": t.address, "hcName": hc_name,
        "labels": t.labels, "at": utc_o(tx.ctx.clock.now())}),
    )?;
    halt("kill")?;
    core.kill(&t.address)
        .map_err(|_| gens::unclear("the kill is uncertain"))?;
    end_by_kill(tx, id, n, pending, &t.address, &hc_name, &t.labels)
}

/// Call 54: the end of a kill hc accepted (now, or by a recorded attempt hc no longer knows): `ended.json` with
/// `reason: "terminated"`, no exit code; then `pending` dropped while it names n.
fn end_by_kill(
    tx: &Tx<'_>,
    id: &str,
    n: u64,
    pending: bool,
    address: &str,
    hc_name: &str,
    labels: &std::collections::BTreeMap<String, String>,
) -> Res<()> {
    let facts = json!({"address": address, "hcName": hc_name, "labels": labels, "state": "finished",
        "reason": "terminated", "exitCode": null, "at": utc_o(tx.ctx.clock.now())});
    gens::publish_end(&tx.folder(id), n, &facts)?;
    halt("ended")?;
    if pending {
        drop_field(tx, id, Guard::PendingGen { n: Some(n) }, "pending")?;
    }
    Ok(())
}

/// Calls 54, 55, 60 replay: settle found no terminal of generation n (`Outcome::Missing`). Only a valid recorded
/// attempt of this generation counts: `kill.json`'s labels carry `mux.id` `id` and `mux.gen` n, its `hcName` is the
/// launch's, and its address is the one the record names for n (pending's, or the active one's). hc `not_found`
/// there records the kill path's end. Anything else leaves `e`: no attempt, a foreign or invalid one, a generation
/// the record names no address for (an orphan), or any other hc answer.
fn forgotten(
    tx: &Tx<'_>,
    lazy: &Lazy<'_>,
    id: &str,
    n: u64,
    r: &Value,
    hc_name: &str,
    e: Refusal,
) -> Res<()> {
    let Some(k) = gens::read_flushed(&tx.folder(id), n, "kill")? else {
        return Err(e);
    };
    let Some(labels) = k.get("labels").and_then(|l| {
        serde_json::from_value::<std::collections::BTreeMap<String, String>>(l.clone()).ok()
    }) else {
        return Err(e);
    };
    let pending = gens::pending(r) == Some(n);
    let recorded = if pending {
        r["pending"]["hcAddress"].as_str()
    } else if gens::active(r) == Some(n) {
        r["hcAddress"].as_str()
    } else {
        None
    };
    let text = |f: &str| k.get(f).and_then(Value::as_str);
    let label = |l: &str| labels.get(l).map(String::as_str);
    let generation = n.to_string();
    let ours = !hc_name.is_empty()
        && text("hcName") == Some(hc_name)
        && label("mux.id") == Some(id)
        && label("mux.gen") == Some(generation.as_str());
    let (Some(address), true) = (recorded.filter(|a| text("address") == Some(*a)), ours) else {
        return Err(e);
    };
    match lazy.get()?.get(address) {
        Err(g) if g.check == "not_found" => {
            end_by_kill(tx, id, n, pending, address, hc_name, &labels)
        }
        _ => Err(e),
    }
}

/// b-calls 12: the reread shows the matched terminal unchanged: same address, Live, same argv (a recovery shell
/// differs: Lost), same name, same labels.
fn same_running(t: &Terminal, now: &Terminal) -> bool {
    now.address == t.address
        && now.state == State::Live
        && now.argv == t.argv
        && now.name == t.name
        && now.labels == t.labels
}

/// settle-1b G8, a `Tx::open` replay hook after `release::complete`: every generation a file or claim names that
/// is neither active nor pending is settled in Fence mode by its own launch.json identity. Running stays reserved
/// (orphan-running) until `terminate`. A linked or unreadable member is skipped. Non-fatal per member (ambiguity 10).
pub fn fence(tx: &Tx<'_>) -> Res<()> {
    let Ok(all) = gens::members(&tx.scope.agents(&tx.ctx.state_root)) else {
        return Ok(());
    };
    let lazy = Lazy::new(tx.ctx);
    for (id, folder, member) in all {
        let gens::Member::Life(g) = member else {
            continue;
        };
        if g.record().get("session").and_then(Value::as_str) != Some(tx.scope.session.as_str()) {
            continue;
        }
        // Call 63: a record naming another member is never fenced from this folder.
        if bound(&id, g.record()).is_err() {
            continue;
        }
        let Ok(named) = gens::named(&folder) else {
            continue;
        };
        for n in named {
            if Some(n) == g.active() || Some(n) == g.pending() {
                continue;
            }
            let _ = settle_gen(tx, &lazy, &id, n, Mode::Fence, false);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::core_fixture as cf;
    use crate::agents::fixture::{ID, World};
    use crate::agents::life_fixture as lf;
    use crate::agents::team_state::Scope;
    use serde_json::Value;

    /// Every launch kind `pending.kind` can carry (spawn, resume, leader from A3b-1; retry, restore, fresh from
    /// A3b-2 and 6c). Settle never reads the kind; each row runs for each (F13).
    const KINDS: [&str; 6] = ["spawn", "resume", "leader", "retry", "restore", "fresh"];

    /// A Tx without `open`'s hooks, so each test scripts only the hc calls its own rows make.
    fn tx<'a>(w: &'a World, s: &Scope) -> Tx<'a> {
        Tx {
            ctx: &w.ctx,
            scope: s.clone(),
        }
    }
    fn settle(w: &World, s: &Scope, mode: Mode) -> Res<Outcome> {
        tx(w, s).settle(ID, mode)
    }
    /// A lifecycle member whose pending generation 1 is of `kind`.
    fn pending_of(w: &World, kind: &str) -> std::path::PathBuf {
        let mut p = lf::pending(1);
        p["kind"] = json!(kind);
        lf::life(w, json!({"pending": p}))
    }
    fn pending_member(w: &World) -> std::path::PathBuf {
        pending_of(w, "spawn")
    }
    /// Torn bytes at `at` (every OS).
    fn torn(at: &std::path::Path) {
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(at);
        std::fs::write(at, b"{").unwrap();
    }
    /// release-1d R1 step 1's body, as `release` publishes it.
    fn release_body() -> Value {
        json!({"address": "t1", "hcName": lf::hc_name(1), "cause": "lost", "at": "2026-11-15T00:00:00Z"})
    }

    // G3 rows 0, 2, 3, 4, 5 x each kind x each mode: Done, pending dropped once, no hc call.
    #[test]
    fn rows_zero_to_five_end_done_and_drop_pending_once() {
        let rows: [(&str, Option<&str>, Option<Value>, &str); 6] = [
            ("0 ended", Some("run"), lf::committed(), "ended"),
            ("0 released", Some("run"), lf::committed(), "released"),
            ("2 abandoned", Some("abandoned"), None, ""),
            ("3 no claim", None, None, ""),
            ("4 run, no commit", Some("run"), None, ""),
            ("5 aborted", Some("run"), Some(json!({"aborted": true})), ""),
        ];
        for (row, claim, commit, marker) in rows {
            for kind in KINDS {
                for mode in [Mode::Normal, Mode::Fence] {
                    let (w, s) = lf::world();
                    let m = pending_of(&w, kind);
                    lf::seed_gen(&m, 1, claim, commit.clone());
                    if !marker.is_empty() {
                        gens::publish_once(&m, 1, marker, &json!({})).unwrap();
                    }
                    let got = settle(&w, &s, mode).unwrap();
                    assert_eq!(got, Outcome::Done, "{row} {kind} {mode:?}");
                    assert!(lf::record(&m).get("pending").is_none(), "{row}: pending");
                    assert!(gens::done(&m, 1).unwrap(), "{row}: done");
                    assert!(lf::calls(&w).is_empty(), "{row}: hc called");
                    assert_eq!(
                        settle(&w, &s, mode).unwrap(),
                        Outcome::Done,
                        "{row}: replay"
                    );
                }
            }
        }
    }

    // G3 row 0r x each kind x each mode (F7, F8, F13; b-calls 15): a readable release.json without released.json is
    // finished from R1 step 3, before row 1, so a torn claim, commit, terminating or launch beside it never blocks
    // it. One relabel, Done, pending dropped; launch.json is never read and release.json is read once.
    #[test]
    fn a_half_done_release_is_finished_before_any_torn_sibling() {
        for sibling in ["none", "claim", "commit", "terminating", "launch"] {
            for kind in KINDS {
                for mode in [Mode::Normal, Mode::Fence] {
                    let (w, s) = lf::world();
                    let m = pending_of(&w, kind);
                    lf::seed_gen(&m, 1, Some("run"), lf::committed());
                    gens::publish_once(&m, 1, "release", &release_body()).unwrap();
                    match sibling {
                        "none" => {}
                        "claim" => torn(&gens::claim_path(&m, 1)),
                        f => torn(&gens::path(&m, 1, f)),
                    }
                    cf::online(&w);
                    lf::get(&w, "t1", &lf::lost(&w, 1, lf::LOST_ARGVS[0]));
                    lf::relabel(&w, "t1");
                    let (got, reads) = crate::state::fs::record_reads(|| settle(&w, &s, mode));
                    let case = format!("{sibling} {kind} {mode:?}");
                    assert_eq!(got.unwrap(), Outcome::Done, "{case}");
                    assert!(
                        matches!(gens::read(&m, 1, "released"), Evidence::Present(_)),
                        "{case}"
                    );
                    assert!(lf::record(&m).get("pending").is_none(), "{case}: pending");
                    assert_eq!(lf::count(&w, "label"), 1, "{case}");
                    lf::no_effect(&w);
                    w.fakes.hc_done().unwrap();
                    let n = |f: &str| reads.iter().filter(|p| **p == gens::path(&m, 1, f)).count();
                    assert_eq!(
                        (n("launch"), n("release")),
                        (0, 1),
                        "{case}: read once, no launch.json"
                    );
                }
            }
        }
    }

    // G3 row 1 x each kind x each mode (R7): any other Unreadable read (every OS: torn bytes) writes nothing further
    // and asks no hc. `release` first: with row 1 gone it would fall to row 4 (F18). A torn `released` beside a
    // readable `release` is row 1, not 0r. A marker Present is decisive beside a torn sibling.
    #[test]
    fn an_unreadable_read_writes_nothing() {
        for case in [
            "release",
            "released",
            "ended",
            "claim",
            "commit",
            "terminating",
            "launch",
        ] {
            for kind in KINDS {
                for mode in [Mode::Normal, Mode::Fence] {
                    let (w, s) = lf::world();
                    let m = pending_of(&w, kind);
                    lf::seed_gen(&m, 1, Some("run"), None);
                    match case {
                        "claim" => torn(&gens::claim_path(&m, 1)),
                        "released" => {
                            gens::publish_once(&m, 1, "release", &release_body()).unwrap();
                            torn(&gens::path(&m, 1, "released"));
                        }
                        f => torn(&gens::path(&m, 1, f)),
                    }
                    let c = format!("{case} {kind} {mode:?}");
                    let before = std::fs::read(m.join("record.json")).unwrap();
                    let got = settle(&w, &s, mode).unwrap();
                    assert!(
                        matches!(&got, Outcome::Unknown(e) if e.check == "evidence"),
                        "{c}: {got:?}"
                    );
                    assert_eq!(std::fs::read(m.join("record.json")).unwrap(), before, "{c}");
                    let commit = if case == "commit" {
                        Evidence::Unreadable
                    } else {
                        Evidence::Absent
                    };
                    assert_eq!(
                        gens::read(&m, 1, "commit"),
                        commit,
                        "{c}: nothing published"
                    );
                    assert!(lf::calls(&w).is_empty(), "{c}");
                }
            }
        }
        let (w, s) = lf::world();
        let m = pending_member(&w);
        lf::seed_gen(&m, 1, Some("run"), lf::committed());
        gens::publish_once(&m, 1, "released", &json!({})).unwrap();
        torn(&gens::path(&m, 1, "ended"));
        assert_eq!(
            settle(&w, &s, Mode::Normal).unwrap(),
            Outcome::Done,
            "a marker is decisive"
        );
    }

    // Ambiguity 3 x each kind x each mode (F13, R7): a claim neither run nor abandoned, or a commit that decides
    // nothing, is Unknown (`internal`), reserved: pending kept, nothing written, no hc. (Absent launch.json is row
    // 6, Task 9.)
    #[test]
    fn an_invalid_claim_or_commit_stays_reserved() {
        for (case, claim, commit) in [
            ("claim", "maybe", None),
            ("commit", "run", Some(json!({"x": 1}))),
        ] {
            for kind in KINDS {
                for mode in [Mode::Normal, Mode::Fence] {
                    let (w, s) = lf::world();
                    let m = pending_of(&w, kind);
                    lf::seed_gen(&m, 1, Some(claim), commit.clone());
                    let c = format!("{case} {kind} {mode:?}");
                    let before = std::fs::read(m.join("record.json")).unwrap();
                    let got = settle(&w, &s, mode).unwrap();
                    assert!(
                        matches!(&got, Outcome::Unknown(e) if e.check == "internal"),
                        "{c}: {got:?}"
                    );
                    assert_eq!(
                        std::fs::read(m.join("record.json")).unwrap(),
                        before,
                        "{c}: pending kept"
                    );
                    assert!(lf::calls(&w).is_empty(), "{c}");
                }
            }
        }
    }

    // b-calls 35 (R8): a forward killed at `allocated` left no record. settle is Done with nothing to do: nothing
    // made, no hc call.
    #[test]
    fn a_member_without_a_record_is_done() {
        for mode in [Mode::Normal, Mode::Fence] {
            let (w, s) = lf::world();
            assert_eq!(
                settle(&w, &s, mode).map_err(|e| e.check),
                Ok(Outcome::Done),
                "{mode:?}"
            );
            assert!(!w.folder(ID).exists(), "{mode:?}: nothing was made");
            assert!(lf::calls(&w).is_empty(), "{mode:?}");
        }
    }

    // settle-1b "Abandonment gap" and "Pending without spec": stopped after row 3's abandoned claim, before the
    // drop; three settles drop pending once; the claim is unchanged; nothing stays reserved.
    #[test]
    fn the_abandonment_gap_closes_once() {
        let (w, s) = lf::world();
        let m = pending_member(&w);
        release::halt_after(Some("abandoned"));
        let first = settle(&w, &s, Mode::Normal);
        release::halt_after(None);
        assert_eq!(first.map_err(|e| e.check), Err("halted"), "the halt");
        assert!(
            lf::record(&m).get("pending").is_some(),
            "pending survives the halt"
        );
        for _ in 0..3 {
            assert_eq!(settle(&w, &s, Mode::Normal).unwrap(), Outcome::Done);
        }
        assert_eq!(gens::claim(&m, 1), Evidence::Present(json!("abandoned")));
        assert!(lf::record(&m).get("pending").is_none());
        let gens::Member::Life(g) = gens::member_of(&m) else {
            panic!("not a lifecycle member")
        };
        assert!(g.status().unwrap().finished, "nothing reserved");
    }

    // G3 rows 3, 4, "lost: reread": the launcher's claim, or its give-up, lands between the read and the publish.
    #[test]
    fn a_lost_publish_is_reread() {
        let (w, s) = lf::world();
        let m = pending_member(&w);
        race_before_publish(Some(|m: &Path| {
            gens::publish_claim_once(m, 1, &json!("run")).unwrap();
        }));
        let got = settle(&w, &s, Mode::Normal);
        race_before_publish(None);
        assert_eq!(got.unwrap(), Outcome::Done);
        assert_eq!(
            gens::claim(&m, 1),
            Evidence::Present(json!("run")),
            "the launcher's claim stands"
        );
        assert_eq!(
            gens::read(&m, 1, "commit"),
            Evidence::Present(json!({"aborted": true})),
            "row 4 after reread"
        );
        // Row 4 lost to a commit: the reread reaches row 6 (Task 8: Unknown); pending kept.
        let (w, s) = lf::world();
        let m = pending_member(&w);
        lf::seed_gen(&m, 1, Some("run"), None);
        race_before_publish(Some(|m: &Path| {
            gens::publish_once(m, 1, "commit", &json!({"committed": true})).unwrap();
        }));
        let got = settle(&w, &s, Mode::Normal);
        race_before_publish(None);
        assert!(
            matches!(got.unwrap(), Outcome::Unknown(_)),
            "row 4: the committed winner is reread"
        );
        assert!(
            lf::record(&m).get("pending").is_some(),
            "row 4: pending kept"
        );
    }

    /// Fault armed on generation 1's folder flush for `f`, then cleared.
    fn armed<T>(m: &std::path::Path, f: impl FnOnce() -> T) -> T {
        crate::state::fs::fail_sync_dir(Some(&gens::dir(m, 1)));
        let got = f();
        crate::state::fs::fail_sync_dir(None);
        got
    }
    fn not_flushed(got: &Outcome) -> bool {
        matches!(got, Outcome::Unknown(e) if e.check == "state" && e.message == gens::NOT_FLUSHED)
    }

    // Call 49, row 0: a marker settles only once its folders flush. Armed: Unknown (`state`, NOT_FLUSHED), the
    // record (pending) unchanged, no hc. Cleared: Done, pending dropped once.
    #[test]
    fn a_marker_settles_only_once_flushed() {
        for marker in ["ended", "released"] {
            let (w, s) = lf::world();
            let m = pending_member(&w);
            lf::seed_gen(&m, 1, Some("run"), lf::committed());
            gens::publish_once(&m, 1, marker, &json!({})).unwrap();
            let before = std::fs::read(m.join("record.json")).unwrap();
            let got = armed(&m, || settle(&w, &s, Mode::Normal)).unwrap();
            assert!(not_flushed(&got), "{marker} armed: {got:?}");
            let after = std::fs::read(m.join("record.json")).unwrap();
            assert_eq!(after, before, "{marker} armed: the record changed");
            assert!(lf::calls(&w).is_empty(), "{marker} armed: hc");
            assert_eq!(
                settle(&w, &s, Mode::Normal).unwrap(),
                Outcome::Done,
                "{marker} cleared"
            );
            assert!(
                lf::record(&m).get("pending").is_none(),
                "{marker} cleared: pending"
            );
        }
    }

    // Call 58, row 2: an abandoned claim settles only once its folders flush (gens, member, agents). Armed: Unknown
    // (state, NOT_FLUSHED), the record unchanged, no hc. Cleared: Done, pending dropped once.
    #[test]
    fn an_abandoned_claim_settles_only_once_flushed() {
        let (w, s) = lf::world();
        let m = pending_member(&w);
        lf::seed_gen(&m, 1, Some("abandoned"), None);
        let before = std::fs::read(m.join("record.json")).unwrap();
        crate::state::fs::fail_sync_dir(Some(&m.join("gens")));
        let got = settle(&w, &s, Mode::Normal);
        crate::state::fs::fail_sync_dir(None);
        let got = got.unwrap();
        assert!(not_flushed(&got), "armed: {got:?}");
        assert_eq!(
            std::fs::read(m.join("record.json")).unwrap(),
            before,
            "armed: the record changed"
        );
        assert!(lf::calls(&w).is_empty(), "armed: hc");
        assert_eq!(
            settle(&w, &s, Mode::Normal).unwrap(),
            Outcome::Done,
            "cleared"
        );
        assert!(lf::record(&m).get("pending").is_none(), "cleared: pending");
        assert_eq!(
            settle(&w, &s, Mode::Normal).unwrap(),
            Outcome::Done,
            "replay"
        );
    }

    // Call 58, row 5: an aborted commit settles only once its folders flush (gens/<n>, gens, member, agents). Armed:
    // Unknown (state, NOT_FLUSHED), the record unchanged, no hc. Cleared: Done, pending dropped once.
    #[test]
    fn an_aborted_commit_settles_only_once_flushed() {
        let (w, s) = lf::world();
        let m = pending_member(&w);
        lf::seed_gen(&m, 1, Some("run"), Some(json!({"aborted": true})));
        let before = std::fs::read(m.join("record.json")).unwrap();
        let got = armed(&m, || settle(&w, &s, Mode::Normal)).unwrap();
        assert!(not_flushed(&got), "armed: {got:?}");
        assert_eq!(
            std::fs::read(m.join("record.json")).unwrap(),
            before,
            "armed: the record changed"
        );
        assert!(lf::calls(&w).is_empty(), "armed: hc");
        assert_eq!(
            settle(&w, &s, Mode::Normal).unwrap(),
            Outcome::Done,
            "cleared"
        );
        assert!(lf::record(&m).get("pending").is_none(), "cleared: pending");
        assert_eq!(
            settle(&w, &s, Mode::Normal).unwrap(),
            Outcome::Done,
            "replay"
        );
    }

    // Call 49, row 0r: release.json is concluded only once its folders flush. Armed: Unknown (`state`), no hc call,
    // no relabel, no released.json, the record unchanged. Cleared: one relabel, released.json, Done.
    #[test]
    fn a_half_done_release_is_concluded_only_once_flushed() {
        let (w, s) = lf::world();
        let m = pending_member(&w);
        lf::seed_gen(&m, 1, Some("run"), lf::committed());
        gens::publish_once(&m, 1, "release", &release_body()).unwrap();
        cf::online(&w);
        lf::get(&w, "t1", &lf::lost(&w, 1, lf::LOST_ARGVS[0]));
        lf::relabel(&w, "t1");
        let before = std::fs::read(m.join("record.json")).unwrap();
        let got = armed(&m, || settle(&w, &s, Mode::Normal)).unwrap();
        assert!(
            lf::calls(&w).is_empty(),
            "armed: hc asked {:?}",
            lf::calls(&w)
        );
        assert!(not_flushed(&got), "armed: {got:?}");
        assert_eq!(
            gens::read(&m, 1, "released"),
            Evidence::Absent,
            "armed: released.json"
        );
        assert_eq!(
            std::fs::read(m.join("record.json")).unwrap(),
            before,
            "armed: the record changed"
        );
        assert_eq!(
            settle(&w, &s, Mode::Normal).unwrap(),
            Outcome::Done,
            "cleared"
        );
        assert_eq!(lf::count(&w, "label"), 1, "cleared: one relabel");
        assert!(
            matches!(gens::read(&m, 1, "released"), Evidence::Present(_)),
            "cleared: released.json"
        );
        assert!(lf::record(&m).get("pending").is_none(), "cleared: pending");
        lf::no_effect(&w);
        w.fakes.hc_done().unwrap();
    }
    /// Generation 1 of `kind`, committed. hc is not installed: each settle that reaches hc queues its `cf::online`.
    fn committed_of(w: &World, kind: &str) -> std::path::PathBuf {
        let m = pending_of(w, kind);
        lf::seed_gen(&m, 1, Some("run"), lf::committed());
        m
    }
    fn reads_of(reads: &[std::path::PathBuf], at: &std::path::Path) -> usize {
        reads.iter().filter(|p| p.as_path() == at).count()
    }

    // Row 6 Running x each kind: Fence leaves it reserved (Running); Normal promotes from launch.json (session,
    // identity) with the team of the matched terminal's mux.team, never the record's (F21: the record says stale).
    #[test]
    fn a_running_committed_generation_is_promoted_in_normal_mode_only() {
        for kind in KINDS {
            let (w, s) = lf::world();
            let m = committed_of(&w, kind);
            let mut r = lf::record(&m);
            r["team"] = json!("stale:$3");
            crate::state::fs::write_json(&m.join("record.json"), &r).unwrap();
            cf::online(&w);
            lf::found(&w, 1, &lf::running(&w, 1));
            assert!(
                matches!(settle(&w, &s, Mode::Fence).unwrap(), Outcome::Running(_)),
                "{kind}: fence"
            );
            assert!(
                lf::record(&m).get("pending").is_some(),
                "{kind}: fence keeps pending"
            );
            cf::online(&w);
            lf::found(&w, 1, &lf::running(&w, 1));
            assert_eq!(
                settle(&w, &s, Mode::Normal).unwrap(),
                Outcome::Promoted,
                "{kind}"
            );
            let r = lf::record(&m);
            let got = (&r["generation"], &r["sessionId"], &r["hcName"]);
            assert_eq!(
                got,
                (&json!("1"), &json!(lf::SID), &json!(lf::hc_name(1))),
                "{kind}"
            );
            assert_eq!(
                (&r["hcAddress"], &r["team"]),
                (&json!("t1"), &json!(lf::team(&w))),
                "{kind}: team"
            );
            assert!(r.get("pending").is_none());
            lf::no_effect(&w);
            w.fakes.hc_done().unwrap();
        }
    }

    // launch-1 G12 x each kind x each mode (F13, R7): a run claim and a commit without launch.json is Unknown
    // (`internal`); no hc.
    #[test]
    fn a_committed_generation_without_launch_json_stays_reserved() {
        for kind in KINDS {
            for mode in [Mode::Normal, Mode::Fence] {
                let (w, s) = lf::world();
                let m = committed_of(&w, kind);
                std::fs::remove_file(gens::path(&m, 1, "launch")).unwrap();
                let got = settle(&w, &s, mode).unwrap();
                assert!(
                    matches!(&got, Outcome::Unknown(e) if e.check == "internal"),
                    "{kind} {mode:?}: {got:?}"
                );
                assert!(
                    lf::record(&m).get("pending").is_some() && lf::calls(&w).is_empty(),
                    "{kind} {mode:?}"
                );
            }
        }
    }

    // "Recorded identity": the pending record's argv may change (a moved exe); launch.json decides. Another argv,
    // hcName or mux.gen label never matches: Unknown, pending kept.
    #[test]
    fn the_recorded_identity_alone_matches() {
        for case in [
            "moved exe",
            "another argv",
            "another name",
            "another gen label",
        ] {
            let (w, s) = lf::world();
            let m = committed_of(&w, "spawn");
            let mut r = lf::record(&m);
            r["pending"]["launcherArgv"] = json!(["/moved/mux-agent", "launcher", "/s/spec.json"]);
            crate::state::fs::write_json(&m.join("record.json"), &r).unwrap();
            let mut t = lf::running(&w, 1);
            match case {
                "another argv" => {
                    t["argv"] = json!(["/other/mux-agent", "launcher", "/s/spec.json"])
                }
                "another name" => t["name"] = json!("m00000009"),
                "another gen label" => t["labels"]["mux.gen"] = json!("2"),
                _ => {}
            }
            cf::online(&w);
            lf::found(&w, 1, &t);
            let got = settle(&w, &s, Mode::Normal).unwrap();
            if case == "moved exe" {
                assert_eq!(got, Outcome::Promoted, "{case}");
            } else {
                assert!(
                    matches!(got, Outcome::Unknown(_) | Outcome::Missing(..)),
                    "{case}: {got:?}"
                );
                assert!(lf::record(&m).get("pending").is_some(), "{case}");
            }
        }
    }

    // Row 6 Lost (R3) x each mode x each kind x both recovery shells: Normal promotes (the record names n's
    // session), then releases with cause restore; Fence releases with cause lost, never active. ready.json stays.
    // F8: launch.json is read once; record.json only by settle's read and each conditional patch.
    #[test]
    fn a_lost_committed_generation_is_promoted_then_released_or_released_only() {
        for mode in [Mode::Normal, Mode::Fence] {
            for kind in KINDS {
                for argv in lf::LOST_ARGVS {
                    let (w, s) = lf::world();
                    let m = committed_of(&w, kind);
                    gens::publish_once(&m, 1, "ready", &json!({})).unwrap();
                    cf::online(&w);
                    lf::found(&w, 1, &lf::lost(&w, 1, argv));
                    lf::get(&w, "t1", &lf::lost(&w, 1, argv));
                    lf::relabel(&w, "t1");
                    let (got, reads) = crate::state::fs::record_reads(|| settle(&w, &s, mode));
                    let got = got.unwrap();
                    let case = format!("{mode:?} {kind}");
                    let r = lf::record(&m);
                    let cause =
                        gens::read(&m, 1, "release").known().unwrap().unwrap()["cause"].clone();
                    if mode == Mode::Normal {
                        assert_eq!(got, Outcome::Promoted, "{case}");
                        let want = (&json!("1"), &json!(lf::SID), &json!("restore"));
                        assert_eq!((&r["generation"], &r["sessionId"], &cause), want, "{case}");
                        assert!(r.get("completing").is_none(), "{case}: completing cleared");
                    } else {
                        assert_eq!(got, Outcome::Done, "{case}");
                        assert_eq!(
                            (r.get("generation"), &cause),
                            (None, &json!("lost")),
                            "{case}: never active"
                        );
                    }
                    assert!(r.get("pending").is_none(), "{case}");
                    assert!(
                        gens::done(&m, 1).unwrap() && gens::path(&m, 1, "ready").exists(),
                        "{case}"
                    );
                    assert_eq!(lf::count(&w, "label"), 1, "{case}");
                    lf::no_effect(&w);
                    w.fakes.hc_done().unwrap();
                    assert_eq!(
                        reads_of(&reads, &gens::path(&m, 1, "launch")),
                        1,
                        "{case}: launch.json reread"
                    );
                    let record = if mode == Mode::Normal { 3 } else { 2 };
                    assert_eq!(
                        reads_of(&reads, &m.join("record.json")),
                        record,
                        "{case}: record.json reads"
                    );
                }
            }
        }
    }

    // Row 6 Ended x each mode x each kind: Normal promotes with completing {ended, facts}, publishes ended.json,
    // clears it; Fence publishes ended.json only and drops pending. F8: launch.json once; no record_end reread.
    #[test]
    fn an_ended_committed_generation_records_its_end() {
        for mode in [Mode::Normal, Mode::Fence] {
            for kind in KINDS {
                let (w, s) = lf::world();
                let m = committed_of(&w, kind);
                cf::online(&w);
                lf::found(&w, 1, &lf::ended(&w, 1));
                let (got, reads) = crate::state::fs::record_reads(|| settle(&w, &s, mode));
                let want = if mode == Mode::Normal {
                    Outcome::Promoted
                } else {
                    Outcome::Done
                };
                let case = format!("{mode:?} {kind}");
                assert_eq!(got.unwrap(), want, "{case}");
                let r = lf::record(&m);
                assert_eq!(
                    gens::read(&m, 1, "ended").known().unwrap().unwrap()["hcName"],
                    json!(lf::hc_name(1))
                );
                assert_eq!(
                    r.get("generation").is_some(),
                    mode == Mode::Normal,
                    "{case}: generation"
                );
                assert!(
                    r.get("pending").is_none() && r.get("completing").is_none(),
                    "{case}"
                );
                assert!(gens::done(&m, 1).unwrap(), "{case}");
                lf::no_effect(&w);
                w.fakes.hc_done().unwrap();
                assert_eq!(
                    reads_of(&reads, &gens::path(&m, 1, "launch")),
                    1,
                    "{case}: launch.json reread"
                );
                let record = if mode == Mode::Normal { 3 } else { 2 };
                assert_eq!(
                    reads_of(&reads, &m.join("record.json")),
                    record,
                    "{case}: record.json reads"
                );
            }
        }
    }

    // "Promotion before Ended marker": stopped after the promote patch (completing written, pending dropped), before
    // ended.json. The next Tx::open publishes ended.json from the recorded facts and clears completing with no hc
    // call, so hc having expired the terminal cannot matter. n is active and done; nothing is reserved.
    #[test]
    fn a_promotion_before_the_ended_marker_is_finished_by_the_next_open() {
        let (w, s) = lf::world();
        let m = committed_of(&w, "spawn");
        cf::online(&w);
        lf::found(&w, 1, &lf::ended(&w, 1));
        release::halt_after(Some("promoted"));
        let first = settle(&w, &s, Mode::Normal);
        release::halt_after(None);
        assert_eq!(first.map_err(|e| e.check), Err("halted"), "the halt");
        let r = lf::record(&m);
        assert!(r.get("completing").is_some(), "completing written");
        assert!(r.get("pending").is_none(), "pending dropped");
        assert!(!gens::done(&m, 1).unwrap(), "reserved before the marker");
        let asked = lf::calls(&w).len();
        crate::agents::team_state::open(&w.ctx, &s, |_| Ok(())).unwrap();
        assert_eq!(lf::calls(&w).len(), asked, "hc called");
        assert!(gens::done(&m, 1).unwrap() && lf::record(&m).get("completing").is_none());
        assert_eq!(lf::record(&m)["generation"], "1");
        let gens::Member::Life(g) = gens::member_of(&m) else {
            panic!("not a lifecycle member")
        };
        assert!(g.status().unwrap().finished, "nothing reserved");
    }

    // Row 6 Unknown x each kind x each mode (R7): none, two, hc unreachable, or an unclear life: Unknown (`hc`),
    // pending kept, nothing written. `terminating` Present forbids promotion (Normal: Running, reserved); torn is
    // row 1.
    #[test]
    fn an_unknown_or_terminating_generation_stays_reserved() {
        let id = format!("mux.id={ID}");
        let ls = [
            "ls",
            "--all",
            "--label",
            id.as_str(),
            "--label",
            "mux.gen=1",
            "--json",
        ];
        for case in ["none", "two", "unreachable", "unclear", "terminating"] {
            for kind in KINDS {
                for mode in [Mode::Normal, Mode::Fence] {
                    let (w, s) = lf::world();
                    let m = committed_of(&w, kind);
                    cf::online(&w);
                    match case {
                        "none" => lf::ls(&w, 1, &[]),
                        "two" => {
                            lf::ls(&w, 1, &["t1", "t9"]);
                            lf::get(&w, "t1", &lf::running(&w, 1));
                            lf::get(&w, "t9", &lf::running(&w, 1));
                        }
                        "unreachable" => lf::fails(&w, &ls),
                        "unclear" => lf::found(
                            &w,
                            1,
                            &lf::wire(&w, 1, "finished", Some("server_stop"), &lf::ARGV),
                        ),
                        _ => {
                            gens::publish_once(&m, 1, "terminating", &json!({})).unwrap();
                            lf::found(&w, 1, &lf::running(&w, 1));
                        }
                    }
                    let c = format!("{case} {kind} {mode:?}");
                    let got = settle(&w, &s, mode).unwrap();
                    if case == "terminating" {
                        assert!(
                            matches!(got, Outcome::Running(_)),
                            "{c}: terminating never promotes: {got:?}"
                        );
                    } else {
                        assert!(
                            matches!(&got, Outcome::Unknown(e) | Outcome::Missing(_, e) if e.check == "hc"),
                            "{c}: {got:?}"
                        );
                    }
                    let r = lf::record(&m);
                    assert!(
                        r.get("pending").is_some() && r.get("generation").is_none(),
                        "{c}"
                    );
                    assert_eq!(gens::read(&m, 1, "ended"), Evidence::Absent, "{c}");
                    w.fakes.hc_done().unwrap();
                }
            }
        }
    }

    // b-calls 36 (R9): a halt inside a continuation (row 0r's conclude; row 6's release or end, with and without a
    // promotion) stays an error with check `halted`: `continued` and `unresolved` never turn it into an Outcome.
    // The file written before the halt stands; nothing after it runs.
    #[test]
    fn a_halt_inside_a_continuation_stays_an_error() {
        let cases = [
            ("ended", Mode::Normal, "ended"),
            ("ended", Mode::Fence, "ended"),
            ("lost", Mode::Normal, "release"),
            ("lost", Mode::Fence, "release"),
            ("half-done", Mode::Normal, "released"),
        ];
        for (case, mode, step) in cases {
            let (w, s) = lf::world();
            let m = committed_of(&w, "spawn");
            cf::online(&w);
            match case {
                "ended" => lf::found(&w, 1, &lf::ended(&w, 1)),
                "lost" => lf::found(&w, 1, &lf::lost(&w, 1, lf::LOST_ARGVS[0])),
                _ => {
                    gens::publish_once(&m, 1, "release", &release_body()).unwrap();
                    lf::get(&w, "t1", &lf::lost(&w, 1, lf::LOST_ARGVS[0]));
                    lf::relabel(&w, "t1");
                }
            }
            let c = format!("{case} {mode:?}");
            release::halt_after(Some(step));
            let got = settle(&w, &s, mode);
            release::halt_after(None);
            assert_eq!(got.map_err(|e| e.check), Err("halted"), "{c}: the halt");
            assert!(
                matches!(gens::read(&m, 1, step), Evidence::Present(_)),
                "{c}: {step}.json before the halt"
            );
            let promoted = mode == Mode::Normal && case != "half-done";
            assert_eq!(
                lf::record(&m).get("completing").is_some(),
                promoted,
                "{c}: completing kept"
            );
            w.fakes.hc_done().unwrap();
        }
    }

    fn kill(w: &World, address: &str) {
        cf::answer(w, &["kill", address, "--json"], "{}");
    }
    fn killed(w: &World, n: u64) -> Value {
        lf::wire(w, n, "finished", Some("killed"), &lf::ARGV)
    }

    // "Fence on a committed pending generation": active 1, pending 2, both Running. terminate(2): terminating,
    // reread, kill.json, kill, ended (reason terminated), pending dropped; terminate(1): the same without
    // terminating. Each call: its own connection, one search, the exact reread, the kill; nothing after the kill
    // (call 54). generation stays "1"; nothing promoted.
    #[test]
    fn terminate_ends_a_pending_generation_then_the_active_one() {
        let (w, s) = lf::world();
        let m = lf::life(
            &w,
            json!({"generation": "1", "hcAddress": "t1", "pending": lf::pending(2)}),
        );
        lf::seed_gen(&m, 1, Some("run"), lf::committed());
        lf::seed_gen(&m, 2, Some("run"), lf::committed());
        for n in [2, 1] {
            let a = format!("t{n}");
            cf::online(&w);
            lf::found(&w, n, &lf::running(&w, n));
            lf::get(&w, &a, &lf::running(&w, n));
            kill(&w, &a);
            let asked = lf::calls(&w).len();
            terminate(&tx(&w, &s), ID, n).unwrap();
            let end = gens::read(&m, n, "ended").known().unwrap().unwrap();
            assert_eq!(end["reason"], "terminated", "{n}");
            assert!(
                matches!(gens::read(&m, n, "kill"), Evidence::Present(_)),
                "{n}: kill.json"
            );
            let verbs: Vec<String> = lf::calls(&w)[asked..]
                .iter()
                .map(|c| c[0].clone())
                .collect();
            assert_eq!(
                verbs,
                ["--version", "ls", "get", "get", "kill"],
                "{n}: the exact reread, then the kill"
            );
        }
        assert!(
            matches!(gens::read(&m, 2, "terminating"), Evidence::Present(_)),
            "terminating for the pending one"
        );
        assert_eq!(
            gens::read(&m, 1, "terminating"),
            Evidence::Absent,
            "the active one is not pending"
        );
        let r = lf::record(&m);
        assert_eq!((&r["generation"], r.get("pending")), (&json!("1"), None));
        assert_eq!(lf::count(&w, "kill"), 2);
        w.fakes.hc_done().unwrap();
    }

    // b-calls 12: the exact terminal reread right before the kill has changed: finished, Lost (the core restarted: a
    // recovery shell), renamed, relabelled, another argv, another address, or gone. No kill, no kill.json,
    // unresolved, pending kept. "Revalidate before kill": the Lost one is then released by the next terminate.
    #[test]
    fn a_terminal_changed_at_the_reread_is_never_killed() {
        for case in [
            "finished",
            "lost",
            "renamed",
            "relabelled",
            "argv",
            "address",
            "gone",
        ] {
            let (w, s) = lf::world();
            let m = committed_of(&w, "spawn");
            cf::online(&w);
            lf::found(&w, 1, &lf::running(&w, 1));
            let mut t = lf::running(&w, 1);
            match case {
                "finished" => t = killed(&w, 1),
                "lost" => t = lf::lost(&w, 1, lf::LOST_ARGVS[1]),
                "renamed" => t["name"] = json!("m00000009"),
                "relabelled" => t["labels"]["mux.team"] = json!("other:$3"),
                "argv" => t["argv"] = json!(["/other/mux-agent", "launcher", "/s/spec.json"]),
                "address" => t["address"] = json!("t9"),
                _ => {}
            }
            if case == "gone" {
                lf::gone(&w, "t1")
            } else {
                lf::get(&w, "t1", &t)
            }
            assert!(terminate(&tx(&w, &s), ID, 1).is_err(), "{case}: refused");
            assert_eq!(lf::count(&w, "kill"), 0, "{case}: killed");
            assert_eq!(
                gens::read(&m, 1, "kill"),
                Evidence::Absent,
                "{case}: kill.json"
            );
            assert!(
                lf::record(&m).get("pending").is_some(),
                "{case}: pending kept"
            );
            assert!(
                matches!(gens::read(&m, 1, "terminating"), Evidence::Present(_)),
                "{case}"
            );
            w.fakes.hc_done().unwrap();
            if case == "lost" {
                cf::online(&w);
                lf::found(&w, 1, &lf::lost(&w, 1, lf::LOST_ARGVS[1]));
                lf::get(&w, "t1", &lf::lost(&w, 1, lf::LOST_ARGVS[1]));
                lf::relabel(&w, "t1");
                terminate(&tx(&w, &s), ID, 1).unwrap();
                assert_eq!(
                    gens::read(&m, 1, "release").known().unwrap().unwrap()["cause"],
                    "lost"
                );
                assert!(
                    lf::record(&m).get("pending").is_none() && gens::done(&m, 1).unwrap(),
                    "released"
                );
                lf::no_effect(&w);
            }
        }
    }

    // G11.4, call 54: a kill hc refuses, or a stop right after kill.json (no kill sent): unresolved, pending kept, no
    // ended.json, and kill.json recorded before any kill. The next terminate (its own connection) kills once and
    // records the end. A kill hc accepts records the end at once with no further hc call; a replay is Done.
    #[test]
    fn an_uncertain_kill_is_finished_by_the_next_terminate() {
        for case in ["refused", "halted at kill", "kill succeeded, end recorded"] {
            let (w, s) = lf::world();
            let m = committed_of(&w, "spawn");
            cf::online(&w);
            lf::found(&w, 1, &lf::running(&w, 1));
            lf::get(&w, "t1", &lf::running(&w, 1));
            match case {
                "refused" => lf::fails(&w, &["kill", "t1", "--json"]),
                "halted at kill" => {}
                _ => kill(&w, "t1"),
            }
            release::halt_after((case == "halted at kill").then_some("kill"));
            let first = terminate(&tx(&w, &s), ID, 1);
            release::halt_after(None);
            assert!(
                matches!(gens::read(&m, 1, "kill"), Evidence::Present(_)),
                "{case}: kill.json"
            );
            w.fakes.hc_done().unwrap();
            if case == "kill succeeded, end recorded" {
                first.unwrap();
                let end = gens::read(&m, 1, "ended").known().unwrap().unwrap();
                let got = (&end["reason"], &end["state"], &end["exitCode"]);
                assert_eq!(
                    got,
                    (&json!("terminated"), &json!("finished"), &Value::Null),
                    "{case}"
                );
                assert!(
                    lf::record(&m).get("pending").is_none(),
                    "{case}: pending dropped"
                );
                let asked = lf::calls(&w).len();
                terminate(&tx(&w, &s), ID, 1).unwrap();
                assert_eq!(lf::calls(&w).len(), asked, "{case}: the replay asked hc");
                continue;
            }
            assert!(first.is_err(), "{case}: unresolved");
            if case == "halted at kill" {
                assert_eq!(lf::count(&w, "kill"), 0, "{case}: a kill before kill.json");
            }
            assert!(
                lf::record(&m).get("pending").is_some(),
                "{case}: pending kept"
            );
            assert_eq!(gens::read(&m, 1, "ended"), Evidence::Absent, "{case}");
            cf::online(&w);
            lf::found(&w, 1, &lf::running(&w, 1));
            lf::get(&w, "t1", &lf::running(&w, 1));
            kill(&w, "t1");
            terminate(&tx(&w, &s), ID, 1).unwrap();
            assert!(
                matches!(gens::read(&m, 1, "ended"), Evidence::Present(_)),
                "{case}: ended"
            );
            assert!(
                lf::record(&m).get("pending").is_none(),
                "{case}: pending dropped"
            );
            let kills = lf::count(&w, "kill");
            assert_eq!(
                kills,
                if case == "refused" { 2 } else { 1 },
                "{case}: kill calls (one landed)"
            );
            w.fakes.hc_done().unwrap();
        }
    }

    // Calls 22, 54, 55: hc forgets the terminal after our kill. A recorded kill.json and hc `not_found` at its
    // address record the same end the kill path writes (field by field, `at` excepted). Without kill.json,
    // `not_found` stays unresolved: nothing recorded, pending kept, hc not asked about the address.
    #[test]
    fn a_recorded_kill_hc_forgot_records_the_same_end() {
        let (w, s) = lf::world();
        let m = committed_of(&w, "spawn");
        cf::online(&w);
        lf::found(&w, 1, &lf::running(&w, 1));
        lf::get(&w, "t1", &lf::running(&w, 1));
        kill(&w, "t1");
        terminate(&tx(&w, &s), ID, 1).unwrap();
        let by_kill = gens::read(&m, 1, "ended").known().unwrap().unwrap();
        // The kill lands but its answer is lost; then hc forgets the terminal.
        let (w, s) = lf::world();
        let m = committed_of(&w, "spawn");
        cf::online(&w);
        lf::found(&w, 1, &lf::running(&w, 1));
        lf::get(&w, "t1", &lf::running(&w, 1));
        lf::fails(&w, &["kill", "t1", "--json"]);
        assert!(
            terminate(&tx(&w, &s), ID, 1).is_err(),
            "the kill's answer is lost"
        );
        assert!(
            matches!(gens::read(&m, 1, "kill"), Evidence::Present(_)),
            "the attempt is recorded"
        );
        assert_eq!(
            gens::read(&m, 1, "ended"),
            Evidence::Absent,
            "no end before the replay"
        );
        w.fakes.hc_done().unwrap();
        cf::online(&w);
        lf::ls(&w, 1, &[]);
        lf::gone(&w, "t1");
        terminate(&tx(&w, &s), ID, 1).unwrap();
        let replayed = gens::read(&m, 1, "ended").known().unwrap().unwrap();
        let but_at = |v: &Value| {
            let mut v = v.clone();
            v.as_object_mut().unwrap().remove("at");
            v
        };
        assert_eq!(
            but_at(&replayed),
            but_at(&by_kill),
            "the same end, `at` excepted"
        );
        assert!(lf::record(&m).get("pending").is_none(), "pending dropped");
        w.fakes.hc_done().unwrap();
        // No recorded attempt.
        let (w, s) = lf::world();
        let m = committed_of(&w, "spawn");
        cf::online(&w);
        lf::ls(&w, 1, &[]);
        assert!(
            terminate(&tx(&w, &s), ID, 1).is_err(),
            "no kill.json: unresolved"
        );
        assert_eq!(
            gens::read(&m, 1, "ended"),
            Evidence::Absent,
            "no kill.json: an end recorded"
        );
        assert!(
            lf::record(&m).get("pending").is_some(),
            "no kill.json: pending dropped"
        );
        w.fakes.hc_done().unwrap();
    }

    // G11.1: a half-done release (release.json, no released.json) is finished by terminate, never killed.
    #[test]
    fn terminate_finishes_a_half_done_release_without_a_kill() {
        let (w, s) = lf::world();
        let m = committed_of(&w, "spawn");
        gens::publish_once(&m, 1, "release", &release_body()).unwrap();
        cf::online(&w);
        lf::get(&w, "t1", &lf::lost(&w, 1, lf::LOST_ARGVS[0]));
        lf::relabel(&w, "t1");
        terminate(&tx(&w, &s), ID, 1).unwrap();
        assert!(matches!(
            gens::read(&m, 1, "released"),
            Evidence::Present(_)
        ));
        assert!(lf::record(&m).get("pending").is_none() && gens::done(&m, 1).unwrap());
        assert_eq!(
            gens::read(&m, 1, "terminating"),
            Evidence::Absent,
            "no terminating before a release"
        );
        assert_eq!(gens::read(&m, 1, "kill"), Evidence::Absent, "no kill.json");
        lf::no_effect(&w);
        w.fakes.hc_done().unwrap();
    }

    // "Orphan stop": a Running orphan 2 beside active 1 and pending 3: only 2 ends; the record is untouched; no
    // terminating.json (2 is not pending).
    #[test]
    fn an_orphan_is_ended_and_the_record_is_untouched() {
        let (w, s) = lf::world();
        let m = lf::life(
            &w,
            json!({"generation": "1", "hcAddress": "t1", "pending": lf::pending(3)}),
        );
        lf::seed_gen(&m, 2, Some("run"), lf::committed());
        let before = std::fs::read(m.join("record.json")).unwrap();
        cf::online(&w);
        lf::found(&w, 2, &lf::running(&w, 2));
        lf::get(&w, "t2", &lf::running(&w, 2));
        kill(&w, "t2");
        terminate(&tx(&w, &s), ID, 2).unwrap();
        assert!(matches!(gens::read(&m, 2, "ended"), Evidence::Present(_)));
        assert_eq!(gens::read(&m, 2, "terminating"), Evidence::Absent);
        assert_eq!(
            std::fs::read(m.join("record.json")).unwrap(),
            before,
            "the record is untouched"
        );
        w.fakes.hc_done().unwrap();
    }

    // B-R4: none or several terminals: unresolved, no kill, pending kept, no terminating.json, no kill.json.
    #[test]
    fn an_unresolved_generation_is_never_killed() {
        for case in ["none", "two"] {
            let (w, s) = lf::world();
            let m = committed_of(&w, "spawn");
            cf::online(&w);
            if case == "none" {
                lf::ls(&w, 1, &[]);
            } else {
                lf::ls(&w, 1, &["t1", "t9"]);
                lf::get(&w, "t1", &lf::running(&w, 1));
                lf::get(&w, "t9", &lf::running(&w, 1));
            }
            assert!(terminate(&tx(&w, &s), ID, 1).is_err(), "{case}");
            assert_eq!(lf::count(&w, "kill"), 0, "{case}");
            assert!(lf::record(&m).get("pending").is_some(), "{case}");
            assert_eq!(gens::read(&m, 1, "terminating"), Evidence::Absent, "{case}");
            assert_eq!(gens::read(&m, 1, "kill"), Evidence::Absent, "{case}");
        }
    }

    // b-calls 37, G11.2: `terminating.json` is published before the end request. Stopped right after it: it is
    // there, no kill.json, no `kill` sent, pending kept. The next terminate rereads, kills once, records the end.
    #[test]
    fn terminating_is_published_before_the_kill() {
        let (w, s) = lf::world();
        let m = committed_of(&w, "spawn");
        cf::online(&w);
        lf::found(&w, 1, &lf::running(&w, 1));
        release::halt_after(Some("terminating"));
        let first = terminate(&tx(&w, &s), ID, 1);
        release::halt_after(None);
        assert_eq!(first.map_err(|e| e.check), Err("halted"), "the halt");
        assert!(
            matches!(gens::read(&m, 1, "terminating"), Evidence::Present(_)),
            "terminating.json first"
        );
        assert_eq!(
            gens::read(&m, 1, "kill"),
            Evidence::Absent,
            "kill.json before terminating.json"
        );
        assert_eq!(lf::count(&w, "kill"), 0, "a kill before terminating.json");
        assert!(lf::record(&m).get("pending").is_some(), "pending kept");
        w.fakes.hc_done().unwrap();
        cf::online(&w);
        lf::found(&w, 1, &lf::running(&w, 1));
        lf::get(&w, "t1", &lf::running(&w, 1));
        kill(&w, "t1");
        terminate(&tx(&w, &s), ID, 1).unwrap();
        assert!(
            matches!(gens::read(&m, 1, "ended"), Evidence::Present(_)),
            "ended"
        );
        assert!(lf::record(&m).get("pending").is_none(), "pending dropped");
        assert_eq!(lf::count(&w, "kill"), 1, "one kill");
        w.fakes.hc_done().unwrap();
    }

    fn open(w: &World, s: &Scope) -> Res<()> {
        crate::agents::team_state::open(&w.ctx, s, |_| Ok(()))
    }
    /// The generations `gens::find` listed, from the `asked`-th hc call on.
    fn listed(w: &World, asked: usize) -> Vec<String> {
        lf::calls(w)[asked..]
            .iter()
            .filter(|c| c[0] == "ls")
            .map(|c| c[5].clone())
            .collect()
    }

    // G8: only orphans are settled (active 4 and pending 7 untouched): 1 has no claim (abandoned), 3 is committed
    // and Ended (recorded), 5 is committed and Running (reserved), 6 is committed and Lost (released, cause lost).
    // `complete` finds no completing, so it never connects; the fence connects once. A second open asks hc only
    // about the Running orphan.
    #[test]
    fn the_fence_settles_only_orphans() {
        let (w, s) = lf::world();
        let m = lf::life(
            &w,
            json!({"generation": "4", "hcAddress": "t4", "pending": lf::pending(7)}),
        );
        lf::seed_gen(&m, 1, None, None);
        for n in [3, 4, 5, 6, 7] {
            lf::seed_gen(&m, n, Some("run"), lf::committed());
        }
        cf::online(&w);
        lf::found(&w, 3, &lf::ended(&w, 3));
        lf::found(&w, 5, &lf::running(&w, 5));
        lf::found(&w, 6, &lf::lost(&w, 6, lf::LOST_ARGVS[0]));
        lf::get(&w, "t6", &lf::lost(&w, 6, lf::LOST_ARGVS[0]));
        lf::relabel(&w, "t6");
        let before = std::fs::read(m.join("record.json")).unwrap();
        open(&w, &s).unwrap();
        assert_eq!(
            listed(&w, 0),
            ["mux.gen=3", "mux.gen=5", "mux.gen=6"],
            "only orphans are asked about"
        );
        assert_eq!(gens::claim(&m, 1), Evidence::Present(json!("abandoned")));
        assert!(
            matches!(gens::read(&m, 3, "ended"), Evidence::Present(_)),
            "orphan 3's end is recorded"
        );
        assert_eq!(
            gens::read(&m, 5, "ended"),
            Evidence::Absent,
            "orphan-running stays reserved"
        );
        assert_eq!(
            gens::read(&m, 6, "release").known().unwrap().unwrap()["cause"],
            "lost"
        );
        for n in [4, 7] {
            assert_eq!(
                gens::read(&m, n, "ended"),
                Evidence::Absent,
                "{n} untouched"
            );
        }
        assert_eq!(
            std::fs::read(m.join("record.json")).unwrap(),
            before,
            "the record is untouched"
        );
        lf::no_effect(&w);
        w.fakes.hc_done().unwrap();
        let asked = lf::calls(&w).len();
        cf::online(&w);
        lf::found(&w, 5, &lf::running(&w, 5));
        open(&w, &s).unwrap();
        assert_eq!(listed(&w, asked), ["mux.gen=5"], "only the Running orphan");
        w.fakes.hc_done().unwrap();
    }

    // Ambiguity 10: a fence hc cannot decide leaves the orphan reserved and the body runs; another session's
    // member is never settled (no hc call at all: neither hook connects).
    #[test]
    fn an_undecidable_fence_stays_reserved_and_another_session_is_untouched() {
        let (w, s) = lf::world();
        let m = lf::life(&w, json!({"generation": "2", "hcAddress": "t2"}));
        lf::seed_gen(&m, 1, Some("run"), lf::committed());
        cf::online(&w);
        let id = format!("mux.id={ID}");
        lf::fails(
            &w,
            &[
                "ls",
                "--all",
                "--label",
                id.as_str(),
                "--label",
                "mux.gen=1",
                "--json",
            ],
        );
        let ran = std::cell::Cell::new(false);
        crate::agents::team_state::open(&w.ctx, &s, |_| {
            ran.set(true);
            Ok(())
        })
        .unwrap();
        assert!(ran.get(), "the body runs");
        assert_eq!(gens::read(&m, 1, "ended"), Evidence::Absent);
        w.fakes.hc_done().unwrap();
        let mut r = lf::record(&m);
        r["session"] = json!("$4");
        crate::state::fs::write_json(&m.join("record.json"), &r).unwrap();
        let asked = lf::calls(&w).len();
        open(&w, &s).unwrap();
        assert_eq!(
            lf::calls(&w).len(),
            asked,
            "another session's orphan was settled"
        );
    }

    // b-calls 5, release-1d R4 ("run before any classification"): `complete` runs before `fence`. Each hook opens
    // its own lazy connection, and the fake answers in one fixed order: completion's (`--version`, `get t1`,
    // `label t1`) and then the fence's (`--version`, `ls` and `get` of orphan 0). Reordered, the fence's `ls`
    // meets `get t1` and completion's `--version` meets `label t1`: both refuse, `completing` stays.
    #[test]
    fn completion_runs_before_the_orphan_fence() {
        let (w, s) = lf::world();
        let c = json!({"gen": 1, "intent": "release", "cause": "restore", "address": "t1"});
        let m = lf::life(
            &w,
            json!({"generation": "1", "hcAddress": "t1", "completing": c}),
        );
        lf::seed_gen(&m, 0, Some("run"), lf::committed());
        lf::seed_gen(&m, 1, Some("run"), lf::committed());
        cf::online(&w);
        lf::get(&w, "t1", &lf::lost(&w, 1, lf::LOST_ARGVS[0]));
        lf::relabel(&w, "t1");
        cf::online(&w);
        lf::found(&w, 0, &lf::running(&w, 0));
        open(&w, &s).unwrap();
        assert!(
            lf::record(&m).get("completing").is_none(),
            "completing cleared"
        );
        assert!(matches!(
            gens::read(&m, 1, "released"),
            Evidence::Present(_)
        ));
        assert_eq!(
            gens::read(&m, 0, "ended"),
            Evidence::Absent,
            "orphan 0 is Running: reserved"
        );
        w.fakes.hc_done().unwrap();
    }

    // b-calls 13, 14 (F17): `<agents>/<id>` is a link (Unix symlink; Windows directory symlink or junction through
    // A3b-1's `crate::testing::link_dir`; every OS) to a folder holding pending 1 and orphan 2, neither claimed.
    // `settle` refuses `evidence`; `open`'s fence skips the member; nothing is published behind the link; hc is
    // never asked.
    #[test]
    fn a_linked_member_is_never_settled_or_fenced() {
        use crate::testing::link_dir;
        let (w, s) = lf::world();
        let m = pending_member(&w);
        lf::seed_gen(&m, 1, None, None);
        lf::seed_gen(&m, 2, None, None);
        let elsewhere = w.ctx.state_root.parent().unwrap().join("elsewhere");
        std::fs::rename(&m, &elsewhere).unwrap();
        link_dir(&elsewhere, &m);
        let got = settle(&w, &s, Mode::Normal);
        assert_eq!(
            got.map_err(|e| e.check),
            Err("evidence"),
            "settle through a link"
        );
        cf::online(&w);
        open(&w, &s).unwrap();
        for n in [1, 2] {
            assert!(
                !gens::claim_path(&elsewhere, n).exists(),
                "{n}: published through the link"
            );
        }
        assert_eq!(lf::calls(&w).len(), 0, "hc asked about a linked member");
    }

    // settle-1b G5 in fence mode (b-calls 3, 10): a committed pending generation released (Lost) or ended in Fence
    // leaves a record that names no generation. It ran: `never_ran_recorded` is false; the member is finished.
    #[test]
    fn a_committed_generation_fenced_never_counts_as_never_ran() {
        for life in ["lost", "ended"] {
            let (w, s) = lf::world();
            let m = committed_of(&w, "spawn");
            cf::online(&w);
            if life == "lost" {
                lf::found(&w, 1, &lf::lost(&w, 1, lf::LOST_ARGVS[0]));
                lf::get(&w, "t1", &lf::lost(&w, 1, lf::LOST_ARGVS[0]));
                lf::relabel(&w, "t1");
            } else {
                lf::found(&w, 1, &lf::ended(&w, 1));
            }
            assert_eq!(
                settle(&w, &s, Mode::Fence).unwrap(),
                Outcome::Done,
                "{life}"
            );
            let r = lf::record(&m);
            assert!(
                r.get("generation").is_none() && r.get("pending").is_none(),
                "{life}: names nothing"
            );
            let gens::Member::Life(g) = gens::member_of(&m) else {
                panic!("{life}: not a lifecycle member")
            };
            let st = g.status().unwrap();
            assert!(!g.never_ran_recorded(&st).unwrap(), "{life}: it ran");
            assert!(st.finished, "{life}");
            w.fakes.hc_done().unwrap();
        }
    }

    // Call 60 (TL #144 F1, F2; QA #144 F1): a forgotten-kill replay acts only when settle finds no terminal of the
    // generation and kill.json is this generation's valid attempt: its labels' mux.id and mux.gen, the launch's
    // hcName, and the address the record names for it. A foreign attempt, an evidence refusal (torn launch.json; a
    // torn claim beside a foreign attempt) or several live matches stay unresolved: no ended.json, pending kept, though
    // hc would answer `not_found` at the attempt's address.
    #[test]
    fn a_kill_replay_acts_only_on_this_generations_attempt_with_no_match() {
        let attempt = |w: &World, id: &str, n: u64, name: &str, address: &str| {
            let mut labels = lf::running(w, 1)["labels"].clone();
            labels["mux.id"] = json!(id);
            labels["mux.gen"] = json!(n.to_string());
            json!({"address": address, "hcName": name, "labels": labels, "at": "x"})
        };
        for case in [
            "foreign member",
            "foreign generation",
            "foreign gen label",
            "foreign name",
            "foreign address",
            "torn launch",
            "torn claim, foreign attempt",
            "several matches",
        ] {
            let (w, s) = lf::world();
            let m = committed_of(&w, "spawn");
            let body = match case {
                "foreign member" => attempt(&w, "bbbbbbbbbbbb", 1, &lf::hc_name(1), "t1"),
                "foreign generation" | "torn claim, foreign attempt" => {
                    attempt(&w, ID, 2, &lf::hc_name(2), "t2")
                }
                // Only mux.gen differs from a valid attempt (isolates that check).
                "foreign gen label" => attempt(&w, ID, 2, &lf::hc_name(1), "t1"),
                "foreign name" => attempt(&w, ID, 1, &lf::hc_name(9), "t1"),
                "foreign address" => attempt(&w, ID, 1, &lf::hc_name(1), "t9"),
                _ => attempt(&w, ID, 1, &lf::hc_name(1), "t1"),
            };
            gens::publish_once(&m, 1, "kill", &body).unwrap();
            match case {
                "torn launch" => torn(&gens::path(&m, 1, "launch")),
                "torn claim, foreign attempt" => torn(&gens::claim_path(&m, 1)),
                _ => {}
            }
            cf::online(&w);
            match case {
                "torn launch" | "torn claim, foreign attempt" => {}
                "several matches" => {
                    lf::ls(&w, 1, &["t1", "t9"]);
                    lf::get(&w, "t1", &lf::running(&w, 1));
                    lf::get(&w, "t9", &lf::running(&w, 1));
                }
                _ => lf::ls(&w, 1, &[]),
            }
            // What a replay would ask at the attempt's address: hc forgot it.
            lf::gone(&w, body["address"].as_str().unwrap());
            let got = terminate(&tx(&w, &s), ID, 1);
            assert!(got.is_err(), "{case}: resolved");
            assert_eq!(
                gens::read(&m, 1, "ended"),
                Evidence::Absent,
                "{case}: an end recorded"
            );
            assert!(
                lf::record(&m).get("pending").is_some(),
                "{case}: pending dropped"
            );
        }
    }

    // Call 63 (QA #144 F2): every settle entry point binds the record's id to its member folder first. A's folder
    // holding B's record: `Tx::settle` and `terminate` refuse `evidence`, the fence skips it. Nothing is published
    // (no abandoned claim, no aborted commit, no terminating, kill or ended), the record is unchanged, hc is never asked
    // (no kill).
    #[test]
    fn a_record_naming_another_member_is_never_settled() {
        const B: &str = "bbbbbbbbbbbb";
        for case in ["settle", "fence", "terminate"] {
            let (w, s) = lf::world();
            let m = match case {
                "settle" => lf::life(&w, json!({"id": B, "pending": lf::pending(1)})),
                "fence" => {
                    let m = lf::life(&w, json!({"id": B, "generation": "2", "hcAddress": "t2"}));
                    lf::seed_gen(&m, 1, Some("run"), None);
                    m
                }
                _ => {
                    let m = lf::life(&w, json!({"id": B, "pending": lf::pending(1)}));
                    lf::seed_gen(&m, 1, Some("run"), lf::committed());
                    cf::online(&w);
                    lf::found(&w, 1, &lf::running(&w, 1));
                    lf::get(&w, "t1", &lf::running(&w, 1));
                    kill(&w, "t1");
                    m
                }
            };
            let before = std::fs::read(m.join("record.json")).unwrap();
            let got = match case {
                "settle" => settle(&w, &s, Mode::Normal).map(|_| ()),
                "fence" => open(&w, &s),
                _ => terminate(&tx(&w, &s), ID, 1),
            };
            if case == "fence" {
                got.unwrap();
            } else {
                assert_eq!(got.map_err(|e| e.check), Err("evidence"), "{case}: bound");
            }
            assert!(
                lf::calls(&w).is_empty(),
                "{case}: hc asked {:?}",
                lf::calls(&w)
            );
            assert_ne!(
                gens::claim(&m, 1),
                Evidence::Present(json!("abandoned")),
                "{case}: abandoned"
            );
            let aborted =
                matches!(gens::read(&m, 1, "commit"), Evidence::Present(c) if c["aborted"] == true);
            assert!(!aborted, "{case}: aborted published");
            for f in ["terminating", "kill", "ended"] {
                assert_eq!(gens::read(&m, 1, f), Evidence::Absent, "{case}: {f}.json");
            }
            assert_eq!(
                std::fs::read(m.join("record.json")).unwrap(),
                before,
                "{case}: the record changed"
            );
        }
    }
}
