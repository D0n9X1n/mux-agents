//! Discovery under the admission lock: any leader record of team T under the state root refuses `up`, by its life.

use std::path::Path;

use serde_json::Value;

use crate::agents::{
    core_identity::Identity,
    lifecycle::{Life, classify},
    text,
};
use crate::{
    ctx::Ctx,
    error::{Res, refuse},
    hc::Core,
    state::fs::read_json,
};

pub fn refuse_existing(ctx: &Ctx, core: &Core<'_>, team: &str) -> Res<()> {
    for (folder, record) in crate::agents::resume::every_record(ctx)? {
        if record.get("upTeam").and_then(Value::as_str) != Some(team) || ran_nothing(&folder) {
            continue;
        }
        let (name, session) = (text(record.get("name")), text(record.get("session")));
        return refuse(
            2,
            "leader",
            match life(core, &folder, &record) {
                Life::Ended => {
                    format!("the leader {name} ended; resuming a leader is not supported yet")
                }
                Life::Lost => format!("the leader {name} is lost; restore will resume it"),
                Life::Running | Life::Unknown => {
                    format!("the team already has a leader: {name} in {session}")
                }
            },
        );
    }
    Ok(())
}

/// Decision 1: an uncommitted launch whose abort evidence proves nothing runs (no uncertain run, no uncertain kill).
fn ran_nothing(folder: &Path) -> bool {
    !folder.join("commit.json").exists()
        && matches!(read_json(&folder.join("abort.json")), Ok(Some(a))
            if a.get("runUncertain") != Some(&Value::Bool(true))
                && a.get("kill").is_none_or(|k| k == "acknowledged"))
}

fn life(core: &Core<'_>, folder: &Path, record: &Value) -> Life {
    if matches!(read_json(&folder.join("notice.json")), Ok(Some(n))
        if n.get("kind").and_then(Value::as_str) == Some("stopped"))
    {
        return Life::Ended;
    }
    let address = text(record.get("hcAddress"));
    if address.is_empty() || address == "pending" {
        return Life::Unknown;
    }
    // As resume's `ended`: a terminal not proven this record's is Unknown.
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
            match refuse_existing(&w.ctx, &core, "T") {
                Ok(()) => assert_eq!(want, "", "{life}"),
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
}
