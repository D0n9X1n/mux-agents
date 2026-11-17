//! Discovery under the admission lock: a lifecycle leader of team T that ran names the anchor `up` reconciles at;
//! a pre-lifecycle leader refuses `up`, by its life.

use std::path::Path;

use serde_json::Value;

use crate::agents::{
    core_identity::Identity,
    gens::Member,
    lifecycle::{Life, classify},
    team_state::Scope,
    text,
};
use crate::{
    ctx::Ctx,
    error::{Refusal, Res, refuse},
    hc::Core,
    state::fs::read_json,
};

/// 6b ambiguity 8, r6b call 6: a lifecycle leader of T that ran (`never_ran_recorded`; torn evidence refuses) names
/// the anchor (its folder key, its record session). A pre-lifecycle leader refuses as before (prelife-1c G10).
pub fn existing(ctx: &Ctx, core: &Core<'_>, key: &str, team: &str) -> Res<Option<Scope>> {
    let own = ctx.state_root.join(key);
    for (folder, member) in crate::agents::resume::every_member(ctx)? {
        let record = match &member {
            Member::Absent => continue,
            Member::Unreadable(e) if folder.starts_with(&own) => return Err(e.clone()),
            Member::Unreadable(_) => continue,
            Member::Pre(r) => r,
            Member::Life(g) => g.record(),
        };
        if record.get("upTeam").and_then(Value::as_str) != Some(team) {
            continue;
        }
        if let Member::Life(g) = &member {
            if g.never_ran_recorded(&g.status()?)? {
                continue;
            }
            let anchor = folder
                .parent()
                .and_then(Path::parent)
                .and_then(Path::file_name)
                .and_then(|k| k.to_str())
                .ok_or_else(|| Refusal::internal("a leader folder has no server key"))?;
            let session = text(record.get("session"));
            return Ok(Some(Scope {
                key: anchor.into(),
                session,
            }));
        }
        refuse_pre(core, &folder, &member, record)?;
    }
    Ok(None)
}

fn refuse_pre(core: &Core<'_>, folder: &Path, member: &Member, record: &Value) -> Res<()> {
    if ran_nothing(folder, member)? {
        return Ok(());
    }
    refuse_by(life(core, folder, record), record)
}

fn refuse_by(life: Life, record: &Value) -> Res<()> {
    let (name, session) = (text(record.get("name")), text(record.get("session")));
    refuse(
        2,
        "leader",
        match life {
            Life::Ended => {
                format!("the leader {name} ended; resuming a leader is not supported yet")
            }
            Life::Lost => format!("the leader {name} is lost; restore will resume it"),
            Life::Running | Life::Unknown => {
                format!("the team already has a leader: {name} in {session}")
            }
        },
    )
}

/// Decision 1, R2: a lifecycle member ran nothing only by `Gen::never_ran_recorded` (b-calls 10, 23); a
/// pre-lifecycle one when its
/// uncommitted launch's abort evidence proves nothing runs (no uncertain run, no uncertain kill). Unreadable refuses
/// (ambiguity 15).
pub(crate) fn ran_nothing(folder: &Path, member: &Member) -> Res<bool> {
    match member {
        Member::Life(g) => {
            let s = g.status()?;
            g.never_ran_recorded(&s)
        }
        Member::Unreadable(e) => Err(e.clone()),
        Member::Pre(_) | Member::Absent => Ok(!folder.join("commit.json").exists()
            && matches!(read_json(&folder.join("abort.json")), Ok(Some(a))
                if a.get("runUncertain") != Some(&Value::Bool(true))
                    && a.get("kill").is_none_or(|k| k == "acknowledged"))),
    }
}

/// A pre-lifecycle leader: its Stopped notice, else hc.
fn life(core: &Core<'_>, folder: &Path, record: &Value) -> Life {
    if matches!(read_json(&folder.join("notice.json")), Ok(Some(n))
        if n.get("kind").and_then(Value::as_str) == Some("stopped"))
    {
        return Life::Ended;
    }
    hc_life(core, folder, record)
}

/// hc's classification of the leader's own terminal. As resume's `ended`: a terminal not proven this record's is
/// Unknown.
fn hc_life(core: &Core<'_>, folder: &Path, record: &Value) -> Life {
    let address = text(record.get("hcAddress"));
    if address.is_empty() || address == "pending" {
        return Life::Unknown;
    }
    let id = folder.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let identity = record
        .get("team")
        .and_then(Value::as_str)
        .and_then(|team| Identity::read(record, id, team));
    identity.map_or(Life::Unknown, |i| {
        classify(
            core.get(&i.address).ok().filter(|t| i.matches(t)).as_ref(),
            &i.launcher,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{core_fixture as cf, fixture::World};
    use serde_json::json;

    #[test]
    fn an_existing_leader_refuses_by_its_life() {
        let (has, ended, lost) = (
            "the team already has a leader: leader in ",
            "the leader leader ended; resuming a leader is not supported yet",
            "the leader leader is lost; restore will resume it",
        );
        for (life, want) in [
            ("pre-run", has),
            ("running", has),
            ("unknown", has),
            ("foreign", has),
            ("foreign-lost", has),
            ("exited", ended),
            ("killed", ended),
            ("stopped", ended),
            ("lost", lost),
            ("aborted", ""),
        ] {
            let w = World::human("");
            cf::online(&w);
            let core = crate::hc::connect(&w.ctx).unwrap();
            let r = cf::member(
                &w,
                "cccccccccccc",
                json!({"name": "leader", "upTeam": "T", "depth": 0}),
            );
            let (folder, mut t) = (w.folder("cccccccccccc"), cf::wire(&r));
            let uncommit = || std::fs::remove_file(folder.join("commit.json")).unwrap();
            match life {
                "pre-run" => {
                    uncommit();
                    crate::state::fs::write_json(
                        &folder.join("record.json"),
                        &json!({"name": "leader", "session": "$3", "upTeam": "T"}),
                    )
                    .unwrap();
                }
                "aborted" => {
                    uncommit();
                    crate::state::fs::write_json(
                        &folder.join("abort.json"),
                        &json!({"runUncertain": false}),
                    )
                    .unwrap();
                }
                "stopped" => crate::state::fs::write_json(
                    &folder.join("notice.json"),
                    &json!({"kind": "stopped"}),
                )
                .unwrap(),
                "unknown" => t["labels"]["mux.gen"] = json!("1"),
                // Another member's finished or recovery-shell terminal at the same address: without the identity
                // check it reads "ended" or "lost"; with it, Unknown, so "already has a leader".
                "foreign" | "foreign-lost" => {
                    t["labels"]["mux.id"] = json!("dddddddddddd");
                    if life == "foreign" {
                        t["state"] = json!("finished");
                        t["reason"] = json!("exited");
                        t["exit_code"] = json!(0);
                    } else {
                        t["argv"] = json!(["/bin/sh", "-l"]);
                    }
                }
                "lost" => t["argv"] = json!(["/bin/sh", "-l"]),
                "exited" | "killed" => {
                    t["state"] = json!("finished");
                    t["reason"] = json!(life);
                    t["exit_code"] = json!(0);
                }
                _ => {}
            }
            if !["pre-run", "aborted", "stopped"].contains(&life) {
                cf::get(&w, r["hcAddress"].as_str().unwrap(), t);
            }
            match existing(&w.ctx, &core, &w.caller().server_key, "T") {
                Ok(None) => assert_eq!(want, "", "{life}"),
                Ok(Some(_)) => panic!("{life}: a pre-lifecycle leader is never an anchor"),
                Err(e) => assert!(
                    (e.code, e.check) == (2, "leader")
                        && !want.is_empty()
                        && e.message.starts_with(want),
                    "{life} {}",
                    e.message
                ),
            }
        }
    }

    /// A lifecycle leader `cccccccccccc` of team T with `fields`, no folder commit, a folder abort that proves nothing
    /// ran, and a folder Stopped notice: all main's evidence that it ran nothing or ended.
    fn leader(w: &World, fields: Value) -> (Value, std::path::PathBuf) {
        let mut all = json!({"name": "leader", "upTeam": "T", "depth": 0, "lifecycle": 1});
        for (k, v) in fields.as_object().unwrap() {
            all[k] = v.clone();
        }
        let r = cf::member(w, "cccccccccccc", all);
        let f = w.folder("cccccccccccc");
        std::fs::remove_file(f.join("commit.json")).unwrap();
        crate::state::fs::write_json(&f.join("abort.json"), &json!({"runUncertain": false}))
            .unwrap();
        crate::state::fs::write_json(&f.join("notice.json"), &json!({"kind": "stopped"})).unwrap();
        (r, f)
    }
    /// One `up` admission check: connects first (its own `--version` answer), then runs discovery. True when it
    /// names an anchor (6b ambiguity 8: `up` reconciles it).
    fn up(w: &World) -> Res<bool> {
        cf::online(w);
        let core = crate::hc::connect(&w.ctx).unwrap();
        existing(&w.ctx, &core, &w.caller().server_key, "T").map(|s| s.is_some())
    }

    // 6b ambiguity 8 (r6b call 7): a lifecycle leader that ran names its anchor whatever its life; hc is not
    // asked (reconcile decides each member, the leader included).
    #[test]
    fn a_lifecycle_leader_that_ran_is_the_anchor_whatever_its_life() {
        use crate::{agents::gens, state::evidence::publish};
        let w = World::human("");
        let (_, f) = leader(&w, json!({"generation": "1", "hcName": "m0123abcd"}));
        publish(&gens::path(&f, 0, "ended"), &json!({"state": "finished"})).unwrap();
        publish(&gens::path(&f, 1, "commit"), &json!({"committed": true})).unwrap();
        let want = Scope {
            key: w.caller().server_key,
            session: "$3".into(),
        };
        for ended in [false, true] {
            if ended {
                publish(&gens::path(&f, 1, "ended"), &json!({"state": "finished"})).unwrap();
            }
            cf::online(&w);
            let core = crate::hc::connect(&w.ctx).unwrap();
            let got = existing(&w.ctx, &core, &w.caller().server_key, "T").unwrap();
            assert_eq!(got.as_ref(), Some(&want), "ended {ended}");
        }
        let asked: Vec<_> = w.fakes.hc_calls().into_iter().map(|c| c.1).collect();
        assert!(asked.iter().all(|c| c[0] == "--version"), "{asked:?}");
    }

    // R2, R5 (af434f CRITICAL 5; audits C3, C4): an undecidable leader refuses `up`; it is never skipped or ended.
    #[test]
    fn an_undecidable_lifecycle_leader_refuses_up() {
        use crate::agents::gens;
        for (case, want) in [
            ("01", "internal"),
            ("torn ended", "evidence"),
            ("torn claim", "evidence"),
            ("linked folder", "evidence"),
        ] {
            let w = World::human("");
            let fields = match case {
                "01" => json!({"generation": "01"}),
                "torn claim" => {
                    json!({"pending": {"gen": 1, "hcName": "m0123abcd", "sessionId": "x"}})
                }
                _ => json!({"generation": "1", "hcName": "m0123abcd"}),
            };
            let (_, f) = leader(&w, fields);
            if case == "torn claim" {
                let mut r = crate::state::fs::read_json(&f.join("record.json"))
                    .unwrap()
                    .unwrap();
                r.as_object_mut().unwrap().remove("generation");
                crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
            }
            std::fs::create_dir_all(gens::dir(&f, 1)).unwrap();
            match case {
                "torn ended" => std::fs::write(gens::path(&f, 1, "ended"), b"{\"st").unwrap(),
                "torn claim" => {
                    std::fs::write(gens::claim_path(&f, 1), b"\"aban").unwrap();
                    crate::state::fs::write_json(
                        &gens::path(&f, 1, "commit"),
                        &json!({"aborted": true}),
                    )
                    .unwrap();
                }
                // Call 20: on Windows too (a directory symlink, else a junction); never skipped.
                "linked folder" => {
                    let moved = w.state().join("moved");
                    std::fs::rename(&f, &moved).unwrap();
                    crate::testing::link_dir(&moved, &f);
                }
                _ => {}
            }
            assert_eq!(up(&w).map_err(|e| e.check).err(), Some(want), "{case}");
        }
    }

    // R2, R3 (audit C7): `tree::scope` reads the leader through the same rule: a lifecycle leader whose generation 1
    // committed ran, whatever the folder says; an unreadable one refuses.
    #[test]
    fn scope_judges_a_lifecycle_leader_by_its_generations() {
        use crate::{agents::gens, state::evidence::publish};
        let w = World::human("");
        let (_, f) = leader(&w, json!({"generation": "1", "hcName": "m0123abcd"}));
        publish(&gens::path(&f, 1, "commit"), &json!({"committed": true})).unwrap();
        let got = crate::agents::tree::scope(&w.ctx, &w.caller()).unwrap();
        assert!(
            matches!(got, crate::agents::tree::Scope::Leaderless),
            "folder abort says nothing of generation 1"
        );
        let w = World::human("");
        leader(&w, json!({"generation": "01"}));
        let got = crate::agents::tree::scope(&w.ctx, &w.caller())
            .map(|_| ())
            .map_err(|e| e.check);
        assert_eq!(got, Err("internal"), "an unreadable leader refuses");
    }

    // PM call 6 (R2 availability): an unreadable member blocks only its own scope. Under the caller's own server key
    // it refuses `up`; under another server key it is skipped and `up` proceeds. A readable member of another team, or
    // a leader of another session, is skipped before its evidence is read.
    #[test]
    fn an_unreadable_member_blocks_only_its_own_scope() {
        use crate::agents::gens;
        let w = World::human("");
        let (_, f) = leader(&w, json!({"generation": "01"}));
        let other = w.ctx.state_root.join("ffffffffffffffff").join("agents");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::rename(&f, other.join("cccccccccccc")).unwrap();
        assert_eq!(
            up(&w).map_err(|e| e.check),
            Ok(false),
            "another server key: skipped, up proceeds"
        );
        std::fs::rename(other.join("cccccccccccc"), &f).unwrap();
        assert_eq!(
            up(&w).map_err(|e| e.check).err(),
            Some("internal"),
            "own server key: refused"
        );
        let torn = |fields: Value| {
            let w = World::human("");
            let (_, f) = leader(&w, fields);
            std::fs::create_dir_all(gens::dir(&f, 1)).unwrap();
            std::fs::write(gens::path(&f, 1, "ended"), b"{\"st").unwrap();
            w
        };
        let w = torn(json!({"generation": "1", "hcName": "m0123abcd", "upTeam": "U"}));
        assert_eq!(
            up(&w).map_err(|e| e.check),
            Ok(false),
            "another team's torn evidence: skipped"
        );
        let w = torn(json!({"generation": "1", "hcName": "m0123abcd", "session": "$9"}));
        let got = crate::agents::tree::scope(&w.ctx, &w.caller());
        assert!(
            matches!(got, Ok(crate::agents::tree::Scope::Outside)),
            "another session's leader: skipped"
        );
    }

    // R3, ambiguity 14: a lifecycle leader whose only launch is void ran nothing; it is no leader.
    #[test]
    fn a_void_lifecycle_leader_is_no_leader() {
        use crate::{agents::gens, state::evidence::publish};
        let w = World::human("");
        let (_, f) = leader(
            &w,
            json!({"pending": {"gen": 1, "hcName": "m0123abcd", "sessionId": "x"}}),
        );
        let mut r = crate::state::fs::read_json(&f.join("record.json"))
            .unwrap()
            .unwrap();
        r.as_object_mut().unwrap().remove("generation");
        crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
        publish(&gens::path(&f, 1, "commit"), &json!({"aborted": true})).unwrap();
        assert_eq!(
            up(&w).map_err(|e| e.message),
            Ok(false),
            "a void first launch is no leader"
        );
    }

    // PM call 17 (#138 QA CRITICAL): a void pending generation that `completing` still holds is not done, so the
    // leader is held and never "ran nothing": `up` reconciles it (6b ambiguity 8) and `scope` is not Outside.
    #[test]
    fn a_held_void_pending_leader_is_still_a_leader() {
        use crate::{agents::gens, state::evidence::publish};
        let w = World::human("");
        let (_, f) = leader(
            &w,
            json!({"pending": {"gen": 1, "hcName": "m0123abcd", "sessionId": "x"}, "completing": {"gen": 1}}),
        );
        let mut r = crate::state::fs::read_json(&f.join("record.json"))
            .unwrap()
            .unwrap();
        r.as_object_mut().unwrap().remove("generation");
        crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
        publish(&gens::path(&f, 1, "commit"), &json!({"aborted": true})).unwrap();
        let gens::Member::Life(g) = gens::member_of(&f) else {
            panic!("not a lifecycle member");
        };
        let st = g.status().unwrap();
        let seen = (st.held, st.finished, st.pending_done, st.never_ran);
        assert_eq!(
            seen,
            (true, false, Some(false), false),
            "held is never ran-nothing"
        );
        assert_eq!(up(&w).map_err(|e| e.check), Ok(true), "up reconciles it");
        let got = crate::agents::tree::scope(&w.ctx, &w.caller()).unwrap();
        assert!(
            !matches!(got, crate::agents::tree::Scope::Outside),
            "scope is not Outside"
        );
    }
}
