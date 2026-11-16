//! release-1d: a generation whose terminal hc kept as a recovery shell is ended by release, never by a kill (R1);
//! a promotion of a generation found Lost or Ended is finished by the `complete` replay hook (R4).
use serde_json::{Value, json};

use super::gens;
use super::team_state::Tx;
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
    if gens::read(&folder, n, "released").known()?.is_some() {
        return Ok(true);
    }
    let Some(rel) = gens::read(&folder, n, "release").known()? else {
        return Ok(false);
    };
    conclude(tx, lazy, id, n, &rel)?;
    Ok(true)
}

/// R1 steps 3 and 4 from a `release.json` body, in R1's order (b-calls 17): `not_found` or `finished` at the
/// recorded address is skipped before any name check; another name stops; `mux.kept=<this team>` is skipped, any
/// other `mux.kept` stops (b-calls 16); otherwise `relabel`. Then `released.json`.
pub fn conclude(tx: &Tx<'_>, lazy: &Lazy<'_>, id: &str, n: u64, rel: &Value) -> Res<()> {
    let text = |k: &str| rel.get(k).and_then(Value::as_str).unwrap_or_default();
    let (address, hc_name, ours) = (text("address"), text("hcName"), team(tx));
    let core = lazy.get()?;
    match core.get(address) {
        Err(e) if e.check == "not_found" => {}
        Err(_) => return Err(gens::unclear("hc cannot show the released shell")),
        Ok(t) if t.state == State::Finished => {}
        Ok(t) if t.name.as_deref() != Some(hc_name) => {
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
            assert_eq!(lf::count(&w, "label"), 1, "{case}");
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
}
