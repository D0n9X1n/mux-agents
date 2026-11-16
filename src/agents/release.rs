//! release-1d: a generation whose terminal hc kept as a recovery shell is ended by release, never by a kill (R1);
//! a promotion of a generation found Lost or Ended is finished by the `complete` replay hook (R4).
use serde_json::{Value, json};

use super::gens;
use super::team_state::{Guard, Tx};
use crate::error::{Refusal, Res};
use crate::hc::{Core, Lazy, State, Terminal};
use crate::state::evidence::Published;
use crate::util::utc_o;

/// R2: why a generation is released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    Restore,
    Recovery,
    Lost,
}

impl Cause {
    pub fn word(self) -> &'static str {
        match self {
            Cause::Restore => "restore",
            Cause::Recovery => "recovery",
            Cause::Lost => "lost",
        }
    }
    pub fn parse(word: &str) -> Option<Cause> {
        [Cause::Restore, Cause::Recovery, Cause::Lost]
            .into_iter()
            .find(|c| c.word() == word)
    }
}

#[cfg(test)]
thread_local! {
    static HALT: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}
/// Test crash point (contract missing 3): the next `halt(step)` refuses, so nothing after that write runs.
#[cfg(test)]
pub fn halt_after(step: Option<&'static str>) {
    HALT.with(|h| h.set(step));
}
/// After each durable write of group B: A3b-1's `crash_point`, and in tests the in-process stop: check `halted`,
/// message `halted after <step>` (b-calls 36). A guard that recognises a halt matches the check, never the text.
pub fn halt(step: &str) -> Res<()> {
    crate::state::evidence::crash_point(step);
    #[cfg(test)]
    if HALT.with(|h| h.get()) == Some(step) {
        HALT.with(|h| h.set(None));
        return Err(Refusal {
            code: 1,
            check: "halted",
            message: format!("halted after {step}"),
            reason: None,
            leftover: None,
            details: Default::default(),
        });
    }
    Ok(())
}

/// b-calls 16: this team, the Tx scope's `<key>:<session>`, which `forward` gave hc as `mux.team`.
fn team(tx: &Tx<'_>) -> String {
    format!("{}:{}", tx.scope.key, tx.scope.session)
}

/// R1 step 1: `release.json` = `{address, hcName (launch.json), cause, at}`, create-once (an existing one
/// stands and is the one concluded); then steps 3 and 4. No kill, send or keys.
pub fn release(
    tx: &Tx<'_>,
    lazy: &Lazy<'_>,
    id: &str,
    n: u64,
    address: &str,
    cause: Cause,
) -> Res<()> {
    let folder = tx.folder(id);
    let launch = gens::read(&folder, n, "launch")
        .known()?
        .ok_or_else(|| Refusal::internal("the generation has no launch.json"))?;
    let hc_name = launch
        .get("hcName")
        .and_then(Value::as_str)
        .unwrap_or_default();
    release_named(tx, lazy, id, n, address, hc_name, cause)
}

/// R1 for a caller that already read `launch.json`'s `hcName` (settle row 6, read once): step 1, then steps 3 and
/// 4. An existing `release.json` stands and is the one concluded (the sanctioned lost-publication reread). No kill,
/// send or keys.
pub fn release_named(
    tx: &Tx<'_>,
    lazy: &Lazy<'_>,
    id: &str,
    n: u64,
    address: &str,
    hc_name: &str,
    cause: Cause,
) -> Res<()> {
    let folder = tx.folder(id);
    let mut body = json!({"address": address, "hcName": hc_name, "cause": cause.word(),
        "at": utc_o(tx.ctx.clock.now())});
    if gens::publish_once(&folder, n, "release", &body)? == Published::Exists {
        body = gens::read(&folder, n, "release")
            .known()?
            .ok_or_else(|| Refusal::internal("release.json vanished"))?;
    }
    halt("release")?;
    conclude(tx, lazy, id, n, &body)
}

/// R1's replay entry (b-calls 15): `released.json` Present is done, with no hc call; `release.json` Present is
/// finished from step 3 with the body just read (never `launch.json`, never a kill). Neither: `false`, nothing
/// done; a caller with an intent runs `release`.
pub fn finish(tx: &Tx<'_>, lazy: &Lazy<'_>, id: &str, n: u64) -> Res<bool> {
    let folder = tx.folder(id);
    // Call 46: visible evidence is acted on only once its flushes succeed (`gens::read_flushed`); a flush that fails
    // stops here with nothing done.
    if gens::read_flushed(&folder, n, "released")?.is_some() {
        return Ok(true);
    }
    let Some(rel) = gens::read_flushed(&folder, n, "release")? else {
        return Ok(false);
    };
    conclude(tx, lazy, id, n, &rel)?;
    Ok(true)
}

/// R1 steps 3 and 4 from a `release.json` body, in R1's order (b-calls 17): `not_found` or `finished` at the
/// recorded address is skipped before any name check; another name stops; `mux.kept=<this team>` is skipped, any
/// other `mux.kept` stops (b-calls 16); otherwise `relabel`. Then `released.json`.
/// a3c call 47: an empty, missing or malformed recorded address or hcName refuses `evidence` before any hc call.
pub fn conclude(tx: &Tx<'_>, lazy: &Lazy<'_>, id: &str, n: u64, rel: &Value) -> Res<()> {
    use super::ident;
    let text = |k: &str| rel.get(k).and_then(Value::as_str).unwrap_or_default();
    let (address, hc_name, ours) = (text("address"), text("hcName"), team(tx));
    if !ident::hc_address(address) {
        return Err(ident::refused("release address"));
    }
    if !ident::hc_name(hc_name) {
        return Err(ident::refused("release hcName"));
    }
    let core = lazy.get()?;
    match core.get(address) {
        Err(e) if e.check == "not_found" => {}
        Err(_) => return Err(gens::unclear("hc cannot show the released shell")),
        Ok(t) if t.state == State::Finished => {}
        Ok(t) if !ident::same(ident::hc_name, t.name.as_deref(), Some(hc_name)) => {
            return Err(gens::unclear("another terminal holds the released address"));
        }
        Ok(t) => match t.labels.get("mux.kept") {
            Some(k) if *k == ours => {}
            Some(_) => return Err(gens::unclear("the shell is kept by another team")),
            None => relabel(core, address, &t, id, n, &ours)?,
        },
    }
    gens::publish_once(
        &tx.folder(id),
        n,
        "released",
        &json!({"at": utc_o(tx.ctx.clock.now())}),
    )?;
    halt("released")
}

/// R1.3: only this member's generation n of this team is relabelled, one guard each (b-calls 20); one call clears
/// every `mux.*` key and sets `mux.kept=<team>`, `mux.keptName=<mux.name>`.
fn relabel(core: &Core<'_>, address: &str, t: &Terminal, id: &str, n: u64, team: &str) -> Res<()> {
    let label = |k: &str| t.labels.get(k).map(String::as_str).unwrap_or_default();
    if label("mux.id") != id {
        return Err(gens::unclear("the shell is another member's"));
    }
    if label("mux.gen") != n.to_string() {
        return Err(gens::unclear("the shell is another generation's"));
    }
    if label("mux.team") != team {
        return Err(gens::unclear("the shell is another team's"));
    }
    let pairs = [
        ("mux.managed", ""),
        ("mux.team", ""),
        ("mux.id", ""),
        ("mux.gen", ""),
        ("mux.name", ""),
        ("mux.up", ""),
        ("mux.kept", team),
        ("mux.keptName", label("mux.name")),
    ];
    core.label(address, &pairs)
        .map_err(|_| gens::unclear("hc could not relabel the shell"))?;
    halt("relabel")
}

/// R4, a `Tx::open` replay hook (state-0 S3) run before the body: finishes each `record.completing` of this
/// session's lifecycle members and clears it. Non-fatal per member (ambiguity 10): a failure keeps `completing`,
/// so `gens::done` stays false and the generation stays reserved. hc is connected only when a member needs it.
pub fn complete(tx: &Tx<'_>) -> Res<()> {
    let Ok(all) = gens::members(&tx.scope.agents(&tx.ctx.state_root)) else {
        return Ok(());
    };
    let lazy = Lazy::new(tx.ctx);
    for (id, _, member) in all {
        let gens::Member::Life(g) = member else {
            continue;
        };
        if g.record().get("session").and_then(Value::as_str) != Some(tx.scope.session.as_str()) {
            continue;
        }
        let _ = complete_member(tx, &lazy, &id, g.record());
    }
    Ok(())
}

/// R4 for the record read in member `id`'s folder (call 44): the record is bound to `id` by call 35's rule
/// (`Guard::admits`) before any publication or hc effect; a record naming another member refuses with nothing done.
pub fn complete_member(tx: &Tx<'_>, lazy: &Lazy<'_>, id: &str, record: &Value) -> Res<()> {
    let Some(c) = record.get("completing") else {
        return Ok(());
    };
    let n = c
        .get("gen")
        .and_then(Value::as_u64)
        .ok_or_else(|| Refusal::internal("invalid completing"))?;
    if !(Guard::CompletingGen { n }).admits(id, Some(record)) {
        return Err(Refusal::internal("the record names another member"));
    }
    complete_one(tx, lazy, id, c)
}

/// R4 for one member. `release`: `finish` first (b-calls 15: a recorded `release.json` is finished from step 3,
/// `released.json` is done, neither rereads `launch.json`); only when neither exists, R1 with the recorded cause
/// and address. `ended`: the recorded facts through `gens::publish_end`. Then `completing` is cleared through
/// `record_write` under `Guard::CompletingGen`, only while it still names its generation.
pub fn complete_one(tx: &Tx<'_>, lazy: &Lazy<'_>, id: &str, c: &Value) -> Res<()> {
    let n = c
        .get("gen")
        .and_then(Value::as_u64)
        .ok_or_else(|| Refusal::internal("invalid completing"))?;
    match c.get("intent").and_then(Value::as_str) {
        Some("release") => {
            if !finish(tx, lazy, id, n)? {
                let cause = c
                    .get("cause")
                    .and_then(Value::as_str)
                    .and_then(Cause::parse)
                    .ok_or_else(|| Refusal::internal("invalid completing cause"))?;
                let address = c.get("address").and_then(Value::as_str).unwrap_or_default();
                release(tx, lazy, id, n, address, cause)?;
            }
        }
        Some("ended") => {
            let facts = c
                .get("facts")
                .filter(|f| f.is_object())
                .ok_or_else(|| Refusal::internal("invalid completing facts"))?;
            gens::publish_end(&tx.folder(id), n, facts)?;
            halt("ended")?;
        }
        _ => return Err(Refusal::internal("invalid completing intent")),
    }
    tx.record_write(id, Guard::CompletingGen { n }, |f| {
        f.remove("completing");
        Ok(())
    })
    .map(|_| ())
}

/// Live tests only (no production writer of lifecycle records yet): `seed` turns a spawned pre-lifecycle member
/// into a lifecycle one with generation n's launch, claim and commit; `release` runs R1 at its address (cause lost).
/// The seed is a fixture and writes through `Tx::patch` (b-calls 29). B2's `orphan` verb is dropped (b-calls 40).
#[cfg(feature = "live")]
pub fn live(tx: &Tx<'_>, verb: &str, id: &str, n: u64) -> Res<Value> {
    let folder = tx.folder(id);
    let r = gens::record(&folder)
        .known()?
        .ok_or_else(|| Refusal::internal("no record"))?;
    let text = |k: &str| {
        r.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    match verb {
        "seed" => {
            let name = r
                .get("hcName")
                .and_then(Value::as_str)
                .map_or_else(|| format!("m{}", &id[..8]), str::to_owned);
            let launch = json!({"hcName": name, "launcherArgv": r["launcherArgv"], "sessionId": text("sessionId")});
            gens::publish_once(&folder, n, "launch", &launch)?;
            gens::publish_claim_once(&folder, n, &json!("run"))?;
            gens::publish_once(&folder, n, "commit", &json!({"committed": true}))?;
            tx.patch(id, &[("lifecycle", json!(1)), ("hcName", json!(name))], &[])?;
        }
        "release" => release(
            tx,
            &Lazy::new(tx.ctx),
            id,
            n,
            &text("hcAddress"),
            Cause::Lost,
        )?,
        // settle-1b live (b-calls 38): A3b-1's real `forward` for a new generation of this member (kind `retry`,
        // the record's session, team, name and directory); `n` is unused, `forward` allocates its own.
        "forward" => {
            let launch = super::core_launch::Launch {
                id: id.to_owned(),
                kind: "retry",
                session_id: text("sessionId"),
                create: None,
                spec: json!({"schema": 1, "stateDirectory": tx.ctx.state_root, "client": text("client")}),
                team: team(tx),
                name: text("name"),
                up_team: None,
                directory: text("directory"),
            };
            let lazy = Lazy::new(tx.ctx);
            super::core_launch::forward(tx, lazy.get()?, launch, || Ok(()), no_view)?;
        }
        "terminate" => super::settle::terminate(tx, id, n)?,
        _ => return Err(Refusal::internal("unknown __life verb")),
    }
    Ok(
        json!({"ok": true, "released": gens::read(&folder, n, "released").known()?.is_some(),
        "ended": gens::read(&folder, n, "ended").known()?.is_some()}),
    )
}

/// Live tests only: `forward` attaches no view.
#[cfg(feature = "live")]
fn no_view(_: &Core<'_>, _: &Value) -> Res<Value> {
    Ok(json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::core_fixture as cf;
    use crate::agents::fixture::{ID, World};
    use crate::agents::gens;
    use crate::agents::life_fixture as lf;
    use crate::agents::team_state::{Scope, open};
    use crate::state::evidence::Evidence;
    use serde_json::json;
    use std::path::{Path, PathBuf};

    fn run(w: &World, s: &Scope, cause: Cause) -> Res<()> {
        open(&w.ctx, s, |tx| {
            release(tx, &Lazy::new(tx.ctx), ID, 1, "t1", cause)
        })
    }
    fn replay(w: &World, s: &Scope) -> Res<bool> {
        open(&w.ctx, s, |tx| finish(tx, &Lazy::new(tx.ctx), ID, 1))
    }
    fn released(m: &Path) -> bool {
        matches!(gens::read(m, 1, "released"), Evidence::Present(_))
    }
    fn lost_member(w: &World) -> PathBuf {
        let m = lf::life(w, json!({"generation": "1", "hcAddress": "t1"}));
        lf::seed_gen(&m, 1, Some("run"), lf::committed());
        m
    }
    fn script_lost(w: &World, argv: &[&str]) {
        cf::online(w);
        lf::get(w, "t1", &lf::lost(w, 1, argv));
        lf::relabel(w, "t1");
    }

    // R1, R2: a Lost generation, every cause, both recovery shells: one relabel, no kill, send or keys; done.
    #[test]
    fn a_lost_generation_is_released_by_one_relabel() {
        for cause in [Cause::Restore, Cause::Recovery, Cause::Lost] {
            for argv in lf::LOST_ARGVS {
                let (w, s) = lf::world();
                let m = lost_member(&w);
                script_lost(&w, argv);
                run(&w, &s, cause).unwrap();
                let rel = gens::read(&m, 1, "release").known().unwrap().unwrap();
                assert_eq!(
                    (&rel["address"], &rel["hcName"]),
                    (&json!("t1"), &json!(lf::hc_name(1)))
                );
                assert_eq!(rel["cause"], cause.word(), "{argv:?}");
                assert!(released(&m), "{argv:?}");
                assert_eq!(lf::count(&w, "label"), 1);
                lf::no_effect(&w);
                w.fakes.hc_done().unwrap();
                assert!(gens::done(&m, 1).unwrap());
            }
        }
    }

    // R1 crash: stopped after each step; `released.json` never before its own step; `finish` replays from step 3
    // (b-calls 15): one relabel in all, `released.json` once; with `released.json` present, no hc call at all.
    #[test]
    fn a_crash_after_each_step_converges() {
        for step in ["release", "relabel", "released"] {
            let (w, s) = lf::world();
            let m = lost_member(&w);
            if step != "release" {
                script_lost(&w, lf::LOST_ARGVS[0]);
            }
            halt_after(Some(step));
            let first = run(&w, &s, Cause::Lost);
            halt_after(None);
            let want = Err(("halted", format!("halted after {step}"))); // b-calls 36: a halt is its check
            assert_eq!(first.map_err(|e| (e.check, e.message)), want, "{step}");
            assert_eq!(
                released(&m),
                step == "released",
                "{step}: released.json before its step"
            );
            match step {
                "release" => script_lost(&w, lf::LOST_ARGVS[0]),
                "relabel" => {
                    cf::online(&w);
                    lf::get(&w, "t1", &lf::kept(&w, 1));
                }
                _ => {}
            }
            assert!(replay(&w, &s).unwrap(), "{step}: finished");
            assert_eq!(lf::count(&w, "label"), 1, "{step}");
            assert!(released(&m), "{step}");
            lf::no_effect(&w);
            w.fakes.hc_done().unwrap();
        }
    }

    // R1.3 skip: kept by this team, finished (also under another name: b-calls 17, finished is skipped before any
    // name check), or gone: released with no relabel. No release.json: `finish` answers false and calls no hc.
    #[test]
    fn a_kept_finished_or_gone_shell_is_released_without_a_relabel() {
        let (w, s) = lf::world();
        lost_member(&w);
        assert!(!replay(&w, &s).unwrap(), "nothing to finish");
        assert!(lf::calls(&w).is_empty());
        for case in ["kept", "finished", "finished elsewhere", "gone"] {
            let (w, s) = lf::world();
            let m = lost_member(&w);
            cf::online(&w);
            let mut done = lf::ended(&w, 1);
            match case {
                "kept" => lf::get(&w, "t1", &lf::kept(&w, 1)),
                "finished" => lf::get(&w, "t1", &done),
                "finished elsewhere" => {
                    done["name"] = json!("m0123abcd");
                    lf::get(&w, "t1", &done);
                }
                _ => lf::gone(&w, "t1"),
            }
            run(&w, &s, Cause::Lost).unwrap();
            assert_eq!(lf::count(&w, "label"), 0, "{case}");
            assert!(released(&m), "{case}");
            w.fakes.hc_done().unwrap();
        }
    }

    // R1.3 stop: hc unclear, the relabel failing, or a live shell that is not this generation's: `hc`, no
    // `released.json`, `release.json` kept for replay. One case per guard (b-calls 16, 17, 20): another name,
    // `mux.id`, `mux.gen`, `mux.team`, or `mux.kept` naming another team.
    #[test]
    fn an_unclear_shell_stops_before_released() {
        let cases = [
            "get fails",
            "label fails",
            "another name",
            "another member",
            "another generation",
            "another team",
            "kept by another team",
        ];
        for case in cases {
            let (w, s) = lf::world();
            let m = lost_member(&w);
            cf::online(&w);
            let mut t = lf::lost(&w, 1, lf::LOST_ARGVS[0]);
            match case {
                "another name" => t["name"] = json!("m0123abcd"),
                "another member" => t["labels"]["mux.id"] = json!("bbbbbbbbbbbb"),
                "another generation" => t["labels"]["mux.gen"] = json!("2"),
                "another team" => t["labels"]["mux.team"] = json!("0123456789abcdef:$9"),
                "kept by another team" => {
                    t = lf::kept(&w, 1);
                    t["labels"]["mux.kept"] = json!("0123456789abcdef:$9");
                }
                _ => {}
            }
            match case {
                "get fails" => lf::fails(&w, &["get", "t1", "--json"]),
                "label fails" => {
                    lf::get(&w, "t1", &t);
                    let argv = lf::relabel_argv(&w, "t1");
                    lf::fails(&w, &argv.iter().map(String::as_str).collect::<Vec<_>>());
                }
                _ => lf::get(&w, "t1", &t),
            }
            let got = run(&w, &s, Cause::Lost);
            if case != "label fails" {
                assert_eq!(lf::count(&w, "label"), 0, "{case}: relabelled");
            }
            assert_eq!(got.map_err(|e| e.check), Err("hc"), "{case}");
            assert!(!released(&m), "{case}");
            assert!(
                matches!(gens::read(&m, 1, "release"), Evidence::Present(_)),
                "{case}"
            );
            lf::no_effect(&w);
        }
    }

    // R1, torn evidence: an unreadable release.json or released.json stops with `evidence` before any hc call;
    // once whole, `finish` completes it once.
    #[test]
    fn a_torn_release_record_stops_until_it_is_whole() {
        for torn in ["release", "released"] {
            let (w, s) = lf::world();
            let m = lost_member(&w);
            let whole =
                json!({"address": "t1", "hcName": lf::hc_name(1), "cause": "lost", "at": "x"});
            std::fs::create_dir_all(gens::dir(&m, 1)).unwrap();
            let body = if torn == "release" {
                b"{".to_vec()
            } else {
                whole.to_string().into_bytes()
            };
            std::fs::write(gens::path(&m, 1, "release"), body).unwrap();
            if torn == "released" {
                std::fs::write(gens::path(&m, 1, "released"), b"{").unwrap();
            }
            assert_eq!(
                replay(&w, &s).map_err(|e| e.check),
                Err("evidence"),
                "{torn}"
            );
            assert!(
                lf::calls(&w).is_empty(),
                "{torn}: hc called on torn evidence"
            );
            std::fs::write(gens::path(&m, 1, "release"), whole.to_string()).unwrap();
            if torn == "released" {
                std::fs::remove_file(gens::path(&m, 1, "released")).unwrap();
            }
            script_lost(&w, lf::LOST_ARGVS[0]);
            assert!(replay(&w, &s).unwrap(), "{torn}");
            assert_eq!(lf::count(&w, "label"), 1, "{torn}");
            w.fakes.hc_done().unwrap();
        }
    }

    // B-R6 (B2 re-audit 6): a failed write or flush of release.json or released.json refuses at its call site with
    // check `state` and the publisher's phrase. No release.json: no hc call, so no relabel. No released.json: the
    // next `finish` completes it with no second relabel.
    #[test]
    fn a_failed_release_publication_refuses_state() {
        for case in [
            "release content",
            "release flush",
            "released content",
            "released flush",
        ] {
            let (w, s) = lf::world();
            let m = lost_member(&w);
            let file = if case.starts_with("released") {
                "released"
            } else {
                "release"
            };
            if file == "released" {
                let rel =
                    json!({"address": "t1", "hcName": lf::hc_name(1), "cause": "lost", "at": "x"});
                gens::publish_once(&m, 1, "release", &rel).unwrap();
                script_lost(&w, lf::LOST_ARGVS[0]);
            }
            if case.ends_with("content") {
                crate::state::fs::fail_sync_file(Some(&gens::path(&m, 1, file)));
            } else {
                crate::state::fs::fail_sync_dir(Some(&gens::dir(&m, 1)));
            }
            let got = if file == "release" {
                run(&w, &s, Cause::Lost)
            } else {
                replay(&w, &s).map(|_| ())
            };
            crate::state::fs::fail_sync_file(None);
            crate::state::fs::fail_sync_dir(None);
            let want = if case.ends_with("content") {
                gens::NOT_PUBLISHED
            } else {
                gens::NOT_FLUSHED
            };
            assert_eq!(
                got.map_err(|e| (e.check, e.message)),
                Err(("state", want.to_string())),
                "{case}"
            );
            if file == "release" {
                assert!(
                    lf::calls(&w).is_empty(),
                    "{case}: hc called without a durable release.json"
                );
                assert!(!released(&m), "{case}");
                continue;
            }
            // Call 46: the gens/1 folder fault also fails release.json's flush, so `finish` stops before the relabel;
            // a file fault on released.json alone comes after it.
            assert_eq!(
                lf::count(&w, "label"),
                usize::from(case == "released content"),
                "{case}"
            );
            if case == "released content" {
                assert!(!released(&m), "{case}");
                cf::online(&w);
                lf::get(&w, "t1", &lf::kept(&w, 1));
            }
            assert!(replay(&w, &s).unwrap(), "{case}: finished");
            assert_eq!(lf::count(&w, "label"), 1, "{case}: one relabel");
            assert!(released(&m), "{case}");
            w.fakes.hc_done().unwrap();
        }
    }

    // R1 step 1, create-once (build checklist 13): `release` losing publication to an existing release.json with a
    // different body concludes the recorded body (its address), never its own proposal; the recorded body stands.
    #[test]
    fn a_release_meeting_a_recorded_body_concludes_that_body() {
        let (w, s) = lf::world();
        let m = lost_member(&w);
        let recorded =
            json!({"address": "t7", "hcName": lf::hc_name(1), "cause": "restore", "at": "x"});
        gens::publish_once(&m, 1, "release", &recorded).unwrap();
        cf::online(&w);
        lf::gone(&w, "t7");
        run(&w, &s, Cause::Lost).unwrap();
        assert_eq!(
            gens::read(&m, 1, "release").known().unwrap().unwrap(),
            recorded,
            "the recorded body stands"
        );
        assert!(released(&m));
        assert_eq!(
            lf::calls(&w)[1],
            ["get", "t7", "--json"],
            "the recorded address"
        );
        w.fakes.hc_done().unwrap();
    }

    fn completing(w: &World, c: Value) -> PathBuf {
        let m = lf::life(
            w,
            json!({"generation": "1", "hcAddress": "t1", "sessionId": lf::SID, "completing": c}),
        );
        lf::seed_gen(&m, 1, Some("run"), lf::committed());
        m
    }
    fn release_intent() -> Value {
        json!({"gen": 1, "intent": "release", "cause": "restore", "address": "t1"})
    }
    fn ended_intent(w: &World) -> Value {
        let facts = gens::end_facts(&lf::terminal(w, &lf::ended(w, 1)), &lf::hc_name(1), "x");
        w.fakes.clear_log();
        json!({"gen": 1, "intent": "ended", "facts": facts})
    }

    // R4 "Promoted, not released": the next Tx::open releases once (cause restore), keeps n's session, clears
    // completing; again with the terminal expired: released, no relabel. A second open calls no hc (B-R5).
    #[test]
    fn a_promoted_generation_is_released_once_by_the_next_open() {
        for expired in [false, true] {
            let (w, s) = lf::world();
            let m = completing(&w, release_intent());
            cf::online(&w);
            if expired {
                lf::gone(&w, "t1");
            } else {
                lf::get(&w, "t1", &lf::lost(&w, 1, lf::LOST_ARGVS[1]));
                lf::relabel(&w, "t1");
            }
            open(&w.ctx, &s, |_| Ok(())).unwrap();
            let r = lf::record(&m);
            assert_eq!(
                (r["sessionId"].as_str(), r.get("completing")),
                (Some(lf::SID), None),
                "{expired}"
            );
            assert_eq!(
                gens::read(&m, 1, "release").known().unwrap().unwrap()["cause"],
                "restore"
            );
            assert_eq!(lf::count(&w, "label"), usize::from(!expired));
            assert!(gens::done(&m, 1).unwrap());
            lf::no_effect(&w);
            w.fakes.hc_done().unwrap();
            w.fakes.clear_log();
            open(&w.ctx, &s, |_| Ok(())).unwrap();
            assert!(lf::calls(&w).is_empty(), "nothing left to do calls no hc");
        }
    }

    // R4 "Marker before clear": stopped after ended.json (ended) or released.json (release), before the clear:
    // not done, reserved; the next open clears completing once, with no second relabel or hc call.
    #[test]
    fn a_marker_before_the_clear_holds_the_reservation() {
        for intent in ["ended", "release"] {
            let (w, s) = lf::world();
            let c = if intent == "ended" {
                ended_intent(&w)
            } else {
                release_intent()
            };
            let m = completing(&w, c);
            if intent == "release" {
                cf::online(&w);
                lf::get(&w, "t1", &lf::lost(&w, 1, lf::LOST_ARGVS[0]));
                lf::relabel(&w, "t1");
            }
            halt_after(Some(if intent == "ended" {
                "ended"
            } else {
                "released"
            }));
            let ran = std::cell::Cell::new(false);
            open(&w.ctx, &s, |_| {
                ran.set(true);
                Ok(())
            })
            .unwrap();
            halt_after(None);
            assert!(ran.get(), "{intent}: the body runs");
            assert!(
                !gens::done(&m, 1).unwrap(),
                "{intent}: done before the clear"
            );
            assert!(lf::record(&m).get("completing").is_some(), "{intent}");
            let before = lf::calls(&w).len();
            open(&w.ctx, &s, |_| Ok(())).unwrap();
            assert!(lf::record(&m).get("completing").is_none(), "{intent}");
            assert!(gens::done(&m, 1).unwrap(), "{intent}");
            assert_eq!(lf::calls(&w).len(), before, "{intent}: no second hc call");
            assert!(
                !gens::path(&m, 1, "notice").exists(),
                "{intent}: no notice from the hook"
            );
        }
    }

    // R4 "Straddling reader": a snapshot taken before the promotion; the writer promotes with completing,
    // publishes ended.json and dies. done is false and the old snapshot is not finished; the next open frees n.
    #[test]
    fn a_straddling_reader_never_sees_done_before_the_clear() {
        let (w, s) = lf::world();
        let m = lf::life(&w, json!({"pending": lf::pending(1)}));
        lf::seed_gen(&m, 1, Some("run"), lf::committed());
        let gens::Member::Life(before) = gens::member_of(&m) else {
            panic!("not a lifecycle member")
        };
        let c = ended_intent(&w);
        let mut r = lf::record(&m);
        r["generation"] = json!("1");
        r["completing"] = c.clone();
        r.as_object_mut().unwrap().remove("pending");
        crate::state::fs::write_json(&m.join("record.json"), &r).unwrap();
        gens::publish_end(&m, 1, &c["facts"]).unwrap();
        assert!(!gens::done(&m, 1).unwrap(), "done before the clear");
        assert!(
            !before.status().unwrap().finished,
            "the straddling snapshot"
        );
        open(&w.ctx, &s, |_| Ok(())).unwrap();
        assert!(gens::done(&m, 1).unwrap());
    }

    // Ambiguity 10: a completion hc cannot finish stays (reserved) and the body runs; another session's member is
    // never read for hc.
    #[test]
    fn an_unfinishable_completion_stays_and_another_session_is_untouched() {
        let (w, s) = lf::world();
        let m = completing(&w, release_intent());
        cf::online(&w);
        lf::fails(&w, &["get", "t1", "--json"]);
        let ran = std::cell::Cell::new(false);
        open(&w.ctx, &s, |_| {
            ran.set(true);
            Ok(())
        })
        .unwrap();
        assert!(ran.get(), "the body runs");
        assert!(lf::record(&m).get("completing").is_some(), "kept");
        let mut r = lf::record(&m);
        r["session"] = json!("$4");
        crate::state::fs::write_json(&m.join("record.json"), &r).unwrap();
        w.fakes.clear_log();
        open(&w.ctx, &s, |_| Ok(())).unwrap();
        assert!(
            lf::calls(&w).is_empty(),
            "another session's member was read for hc"
        );
        assert!(lf::record(&m).get("completing").is_some());
    }

    // b-calls 15 (B2 finding 6): a recorded release.json is finished from step 3 and released.json is done with no
    // hc; neither rereads launch.json (torn here).
    #[test]
    fn a_recorded_release_finishes_without_launch_json() {
        for case in ["release", "released"] {
            let (w, s) = lf::world();
            let m = completing(&w, release_intent());
            let rel =
                json!({"address": "t1", "hcName": lf::hc_name(1), "cause": "restore", "at": "x"});
            gens::publish_once(&m, 1, "release", &rel).unwrap();
            if case == "released" {
                gens::publish_once(&m, 1, "released", &json!({"at": "x"})).unwrap();
            } else {
                cf::online(&w);
                lf::gone(&w, "t1");
            }
            std::fs::write(gens::path(&m, 1, "launch"), b"{").unwrap();
            open(&w.ctx, &s, |_| Ok(())).unwrap();
            assert!(
                lf::record(&m).get("completing").is_none(),
                "{case}: still completing"
            );
            assert!(released(&m), "{case}");
            assert_eq!(
                lf::calls(&w).len(),
                if case == "release" { 2 } else { 0 },
                "{case}: hc calls"
            );
            w.fakes.hc_done().unwrap();
        }
    }

    // R4 guard at the call site (B2 re-audit 5): a stale intent for generation 1 records 1's end but never clears a
    // `completing` naming generation 2 (`Guard::CompletingGen { n }`). Another session, so the hook leaves it be.
    #[test]
    fn a_completion_of_another_generation_clears_nothing() {
        let (w, s) = lf::world();
        let c = ended_intent(&w);
        let m = lf::life(
            &w,
            json!({"generation": "2", "session": "$4", "completing": {"gen": 2, "intent": "ended"}}),
        );
        lf::seed_gen(&m, 1, Some("run"), lf::committed());
        open(&w.ctx, &s, |tx| {
            complete_one(tx, &Lazy::new(tx.ctx), ID, &c)
        })
        .unwrap();
        assert!(
            matches!(gens::read(&m, 1, "ended"), Evidence::Present(_)),
            "generation 1's end"
        );
        assert_eq!(
            lf::record(&m)["completing"]["gen"],
            2,
            "another generation's completing cleared"
        );
    }

    // Call 44: another member's record (B's, with its `completing`) in A's folder under this session: the hook binds
    // the record it read to A's id before any publication or hc effect, so A gets no evidence, hc no call, and the
    // record stands as it was.
    #[test]
    fn a_record_naming_another_member_completes_nothing() {
        for intent in ["ended", "release"] {
            let (w, s) = lf::world();
            let c = if intent == "ended" {
                ended_intent(&w)
            } else {
                release_intent()
            };
            let m = completing(&w, c);
            let mut r = lf::record(&m);
            r["id"] = json!("bbbbbbbbbbbb");
            crate::state::fs::write_json(&m.join("record.json"), &r).unwrap();
            if intent == "release" {
                cf::online(&w);
                lf::get(&w, "t1", &lf::lost(&w, 1, lf::LOST_ARGVS[0]));
                lf::relabel(&w, "t1");
            }
            w.fakes.clear_log();
            let got = open(&w.ctx, &s, |tx| {
                complete_member(tx, &Lazy::new(tx.ctx), ID, &r)
            });
            assert_eq!(
                got.map_err(|e| (e.check, e.message)),
                Err((
                    "internal",
                    "unexpected error (the record names another member)".to_string()
                )),
                "{intent}: reported"
            );
            for file in ["ended", "release", "released"] {
                assert!(!gens::path(&m, 1, file).exists(), "{intent}: {file}.json");
            }
            assert!(lf::calls(&w).is_empty(), "{intent}: {:?}", lf::calls(&w));
            assert_eq!(lf::record(&m), r, "{intent}: the record stands");
        }
    }

    // Call 46 (QA #142): `finish` acts on a visible release.json or released.json only after that file's flushes
    // succeed (`publish_once` on the body read). With gens/1's folder flush failing across two opens: no label, no
    // released.json, completing kept. Fault cleared: exactly one label (release.json) or none (released.json), and
    // completing cleared.
    #[test]
    fn visible_release_evidence_is_acted_on_only_once_flushed() {
        for case in ["release", "released"] {
            let (w, s) = lf::world();
            let m = completing(&w, release_intent());
            let rel =
                json!({"address": "t1", "hcName": lf::hc_name(1), "cause": "restore", "at": "x"});
            gens::publish_once(&m, 1, "release", &rel).unwrap();
            if case == "released" {
                gens::publish_once(&m, 1, "released", &json!({"at": "x"})).unwrap();
            } else {
                cf::online(&w);
                lf::get(&w, "t1", &lf::lost(&w, 1, lf::LOST_ARGVS[0]));
                lf::relabel(&w, "t1");
            }
            w.fakes.clear_log();
            crate::state::fs::fail_sync_dir(Some(&gens::dir(&m, 1)));
            for attempt in 0..2 {
                open(&w.ctx, &s, |_| Ok(())).unwrap();
                assert_eq!(lf::count(&w, "label"), 0, "{case} {attempt}: labelled");
                assert!(
                    lf::record(&m).get("completing").is_some(),
                    "{case} {attempt}: completing cleared"
                );
                assert_eq!(released(&m), case == "released", "{case} {attempt}");
            }
            crate::state::fs::fail_sync_dir(None);
            open(&w.ctx, &s, |_| Ok(())).unwrap();
            assert_eq!(
                lf::count(&w, "label"),
                usize::from(case == "release"),
                "{case}"
            );
            assert!(lf::record(&m).get("completing").is_none(), "{case}");
            assert!(released(&m), "{case}");
            w.fakes.hc_done().unwrap();
        }
    }

    // R4 (build checklist 10): an `ended` completion publishes exactly the facts it recorded, no other body.
    #[test]
    fn an_ended_completion_publishes_exactly_its_recorded_facts() {
        let (w, s) = lf::world();
        let c = ended_intent(&w);
        let m = completing(&w, c.clone());
        open(&w.ctx, &s, |_| Ok(())).unwrap();
        assert_eq!(
            gens::read(&m, 1, "ended").known().unwrap().unwrap(),
            c["facts"],
            "ended.json is completing.facts"
        );
        assert!(lf::record(&m).get("completing").is_none());
    }

    // B-R6 (build checklist 12): a failed completion clear answers check `state`, `team_state::NOT_WRITTEN`; the
    // recorded end stands and `completing` stays, so the generation stays reserved until the next open clears it.
    #[test]
    fn a_failed_completion_clear_refuses_state_and_keeps_completing() {
        let (w, s) = lf::world();
        let c = ended_intent(&w);
        let m = completing(&w, c.clone());
        let mut r = lf::record(&m);
        r["session"] = json!("$4"); // the hook leaves it be; the direct call below is the one under test
        crate::state::fs::write_json(&m.join("record.json"), &r).unwrap();
        crate::state::fs::fail_sync_file(Some(&m.join("record.json")));
        let got = open(&w.ctx, &s, |tx| {
            complete_one(tx, &Lazy::new(tx.ctx), ID, &c)
        });
        crate::state::fs::fail_sync_file(None);
        assert_eq!(
            got.map_err(|e| (e.check, e.message)),
            Err(("state", crate::agents::team_state::NOT_WRITTEN.to_string()))
        );
        assert_eq!(
            gens::read(&m, 1, "ended").known().unwrap().unwrap(),
            c["facts"],
            "the end stands"
        );
        assert!(
            lf::record(&m).get("completing").is_some(),
            "completing kept"
        );
        assert!(!gens::done(&m, 1).unwrap(), "reserved");
        let mut r = lf::record(&m);
        r["session"] = json!(w.caller().session);
        crate::state::fs::write_json(&m.join("record.json"), &r).unwrap();
        open(&w.ctx, &s, |_| Ok(())).unwrap();
        assert!(
            lf::record(&m).get("completing").is_none(),
            "the next open clears it"
        );
        assert!(gens::done(&m, 1).unwrap());
    }

    // a3c call 47 (TL #149, production readiness): a release intent whose address or hcName is empty, missing or
    // malformed never owns a terminal, even one carrying the same name with matching labels, nor an address hc
    // answers `not_found` for. `finish` refuses `evidence` before any hc call: no relabel, no released.json, the
    // intent kept.
    #[test]
    fn an_invalid_release_identity_owns_nothing() {
        for case in [
            "empty name",
            "missing name",
            "malformed name",
            "empty address",
            "missing address",
        ] {
            let (w, s) = lf::world();
            let m = lost_member(&w);
            let mut body =
                json!({"address": "t1", "hcName": lf::hc_name(1), "cause": "lost", "at": "x"});
            let mut shell = lf::lost(&w, 1, lf::LOST_ARGVS[0]);
            match case {
                "empty name" => body["hcName"] = json!(""),
                "missing name" => {
                    body.as_object_mut().unwrap().remove("hcName");
                }
                "malformed name" => body["hcName"] = json!("../x"),
                "empty address" => body["address"] = json!(""),
                _ => {
                    body.as_object_mut().unwrap().remove("address");
                }
            }
            shell["name"] = match case {
                "malformed name" => json!("../x"),
                "empty name" | "missing name" => json!(""),
                _ => shell["name"].clone(),
            };
            gens::publish_once(&m, 1, "release", &body).unwrap();
            cf::online(&w);
            if case.ends_with("address") {
                // What main would ask: hc does not know "" (`not_found`).
                lf::gone(&w, "");
            } else {
                lf::get(&w, "t1", &shell);
                lf::relabel(&w, "t1");
            }
            let got = replay(&w, &s);
            assert_eq!(got.map_err(|e| e.check), Err("evidence"), "{case}");
            assert_eq!(lf::count(&w, "label"), 0, "{case}: relabelled");
            assert!(!released(&m), "{case}: released.json");
            assert!(
                matches!(gens::read(&m, 1, "release"), Evidence::Present(_)),
                "{case}: the intent kept"
            );
            lf::no_effect(&w);
        }
    }
}
