//! restore-6c P2: `stop X -Generation` ends every generation of X that is not done, and nothing else.
use serde_json::{Map, Value, json};

use super::super::resume::Subject;
use super::super::text;
use crate::cli::Request;
use crate::ctx::Ctx;
use crate::error::{Res, refuse};

/// P2 / settle-1b G11 (ambiguity 6; calls 15, 25): a person ends every not-done generation of X, a member of the
/// anchor session, newest first, through `gens::terminate` alone: no cascade journal, no desired write, no other
/// member. A Lost one is released, never killed. Each generation is decided on its own: an unreadable `done` or a
/// refused `terminate` is reported in `unresolved` and the rest go on (checklist 28). All done: 2 `state`.
pub fn stop_generations(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let target = super::super::view::first(request);
    let person = super::super::up::person_as(ctx, "stop a generation")?;
    let here = super::super::scope::state(&person); // 6a call 10
    let Some(team) = super::super::reconcile::session_team(ctx, &here)? else {
        return refuse(
            1,
            "team",
            "no team in this session; run up from the team folder",
        );
    };
    super::super::admit(ctx, &format!("up|{team}"), || {
        super::super::team_state::open(ctx, &here, |tx| {
            let Subject::Life(id) = super::super::resume::subject(tx, target)? else {
                return refuse(2, "target", "no member of this team has that name or id");
            };
            let folder = tx.folder(&id);
            let super::super::gens::Member::Life(g) = super::super::gens::member_of(&folder) else {
                return Err(crate::state::evidence::unreadable());
            };
            // every generation a file or claim names, plus pending and active; never classified before its own read
            let mut all = super::super::gens::named(&folder)?;
            all.extend(g.pending().into_iter().chain(g.active()));
            all.sort_unstable_by(|a, b| b.cmp(a));
            all.dedup();
            let unresolved_of = |n: u64, e: &crate::error::Refusal| json!({"generation": n, "check": e.check, "reason": e.message});
            let (mut ended, mut unresolved) = (Vec::new(), Vec::new());
            let mut open = 0;
            for n in all {
                match super::super::gens::done(&folder, n) {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(e) => {
                        unresolved.push(unresolved_of(n, &e));
                        continue;
                    }
                }
                open += 1;
                match super::super::settle::terminate(tx, &id, n) {
                    Ok(()) => ended.push(n),
                    Err(e) if e.check == "halted" => return Err(e),
                    Err(e) => unresolved.push(unresolved_of(n, &e)),
                }
            }
            let name = text(g.record().get("name"));
            if open == 0 && unresolved.is_empty() {
                return refuse(2, "state", format!("every generation of {name} is done"));
            }
            let mut m = Map::new();
            m.insert("name".into(), name.into());
            m.insert("ended".into(), json!(ended));
            m.insert("unresolved".into(), json!(unresolved));
            Ok(m)
        })
    })
}
