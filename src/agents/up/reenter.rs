//! reentry-6a E4.4-6, `drive`'s Reenter arm (call 8): reconcile, the team record, publication, then every Running
//! member's view in the view's session, a status window when none runs, the bar, select and switch, the release.
use serde_json::{Map, Value, json};

use super::{Run, at_view, index, leader_of, leaders, publish, server, team_exists, write_team};
use crate::agents::{gens, lifecycle, text};
use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::error::{Refusal, Res};
use crate::hc::Core;
use crate::mux::native;

pub struct Running {
    pub id: String,
    pub record: Value,
}

pub struct Views {
    pub shown: Vec<Value>,
    pub duplicates: usize,
    /// The first member whose view could not be opened (call 23): reported, never fatal.
    pub error: Option<String>,
}

/// E4 in call 8's order: reconcile (unless this call launched the leader; restore-6c adds leader resume here), the
/// team record create-only from the leader's current record, `publish::ensure`, views, bar and switch, `presented`,
/// the release. A leader this call launched is the reply's from its launch record (F15: no discovery after commit).
/// Later groups extend this arm (call 9).
pub(super) fn arm(
    ctx: &Ctx,
    caller: &Caller,
    core: &Core<'_>,
    team: &str,
    ix: &mut index::Index,
    run: Run,
    reentered: bool,
) -> Res<Map<String, Value>> {
    let a = ix
        .anchor
        .clone()
        .ok_or_else(|| Refusal::internal("no anchor"))?;
    let scope = crate::agents::team_state::Scope {
        key: a.key,
        session: a.session,
    };
    let mut reply = Map::new();
    let (mut created, mut replayed, mut team_error) = (run.created, run.replayed, run.team_error);
    let leader = if run.launched {
        run.record.clone()
    } else {
        reply.extend(run.reconciled.unwrap_or_default()); // restore-6c Task 7: the pass, run in `drive`
        let leader = leader_of(ctx, ix)?.1;
        let leader = leader
            .map(|id| leaders::leader_record(ctx, ix, &id))
            .transpose()?;
        if let Some(r) = &leader
            && !team_exists(ctx, ix)?
        {
            match write_team(ctx, ix, r) {
                Ok(()) => (created, replayed) = (true, true),
                Err(e) => team_error = Some(e.message),
            }
        }
        leader
    };
    let shown = match run.shown {
        Some(p) => p,
        None => publish::ensure(ctx, caller, ix)?,
    };
    if let Some(r) = &run.record {
        reply.extend(crate::agents::spawn::view_reply(r)); // A3b-2: `attached`, or `viewError` (call 23)
    }
    if let Some(e) = team_error {
        reply.insert("teamRecordError".into(), e.into());
    }
    for (k, v) in [
        ("team", json!(team)),
        ("session", json!(shown.view.session)),
        ("name", json!(shown.view.name)),
        ("created", json!(created)),
        ("replayed", json!(replayed)),
        ("reentered", json!(reentered)),
    ] {
        reply.insert(k.into(), v);
    }
    if let Some(r) = &leader {
        reply.insert(
            "leader".into(),
            json!({"id": r["id"], "name": r["name"], "hcAddress": r["hcAddress"]}),
        );
    }
    // E4.4-6: views, then the window to select (calls 23, 26: presentation never fails up after commit).
    let view_caller = at_view(caller, ix, &shown);
    let lazy = crate::hc::Lazy::new(ctx);
    let launched = run
        .launched
        .then(|| run.record.as_ref().map(|r| text(r.get("id"))))
        .flatten();
    let (running, members) = running(ctx, &lazy, &scope, launched.as_deref())?;
    // D30: a leader this call launched shows the view its launch attached (its reply's `pane`, `window`; none after
    // `viewError`), never looked up again (F15); every other Running member through `views`.
    let others: Vec<&Running> = running
        .iter()
        .filter(|m| Some(m.id.as_str()) != launched.as_deref())
        .collect();
    let mut got = views(ctx, &view_caller, core, &others);
    if let (Some(id), Some(r)) = (&launched, &run.record)
        && r.get("viewError").is_none()
        && let (Some(pane), Some(window)) = (r.get("pane"), r.get("window"))
    {
        let v = json!({"id": id, "name": r["name"], "pane": pane, "window": window});
        got.shown.insert(0, v);
    }
    // Call 26 (F15): selection is among the views actually shown.
    let starts: Vec<(String, Option<String>)> = running
        .iter()
        .filter(|m| got.shown.iter().any(|v| v["id"] == m.id.as_str()))
        .map(|m| (m.id.clone(), first_start(ctx, &scope, &m.id)))
        .collect();
    let leader_id = leader.as_ref().map(|r| text(r.get("id")));
    let chosen = choose(leader_id.as_deref(), &starts)
        .and_then(|id| got.shown.iter().find(|v| v["id"] == id.as_str()));
    let (window, pane) = match chosen {
        Some(v) => (text(v.get("window")), text(v.get("pane"))),
        None => match status_window(ctx, &view_caller) {
            Ok(w) => (w, String::new()),
            Err(e) => {
                reply.entry("viewError").or_insert(json!(e.message)); // call 26 (F14)
                (String::new(), String::new())
            }
        },
    };
    if let Some(e) = got.error {
        reply.entry("viewError").or_insert(json!(e)); // call 23: the launch's first, else the first member's
    }
    reply.insert("views".into(), json!(got.shown));
    reply.insert("duplicates".into(), json!(got.duplicates));
    reply.insert("members".into(), json!(members));
    present(ctx, caller, ix, &shown, (&window, &pane), &mut reply)?;
    Ok(reply)
}

/// E4.4 input (call 7, findings #1, #8, #9): the anchor session's lifecycle records with an active generation; a done
/// one is Ended without hc; `launched` (this call's `Step::Launch`) is Running without hc; the others by `gens::find`
/// and `classify`. Returns the Running members (by id) and every member's `{id, name, life}`.
pub fn running(
    ctx: &Ctx,
    lazy: &crate::hc::Lazy<'_>,
    scope: &crate::agents::team_state::Scope,
    launched: Option<&str>,
) -> Res<(Vec<Running>, Vec<Value>)> {
    let (mut run, mut all) = (Vec::new(), Vec::new());
    for (id, folder, member) in gens::members(&scope.agents(&ctx.state_root))? {
        let gens::Member::Life(g) = member else {
            continue;
        };
        let r = g.record().clone();
        if r.get("session").and_then(Value::as_str) != Some(scope.session.as_str()) {
            continue;
        }
        let Some(n) = g.active() else {
            continue;
        };
        let life = if Some(id.as_str()) == launched {
            lifecycle::Life::Running
        } else {
            life_of(lazy, &g, &folder, &id, n)
        };
        all.push(json!({"id": id, "name": r["name"], "life": format!("{life:?}").to_lowercase()}));
        if life == lifecycle::Life::Running {
            run.push(Running { id, record: r });
        }
    }
    Ok((run, all))
}

/// Call 27 (F16): hc name and launcher argv from the active generation's flushed `launch.json`, not the record.
/// Call 26 (F17): an unreadable status, launch or hc lookup is Unknown; the scan goes on.
fn life_of(
    lazy: &crate::hc::Lazy<'_>,
    g: &gens::Gen,
    folder: &std::path::Path,
    id: &str,
    n: u64,
) -> lifecycle::Life {
    match g.status().map(|s| s.active_done) {
        Ok(Some(true)) => return lifecycle::Life::Ended,
        Ok(_) => {}
        Err(_) => return lifecycle::Life::Unknown,
    }
    let Ok(Some(l)) = gens::read_flushed(folder, n, "launch") else {
        return lifecycle::Life::Unknown;
    };
    let Ok(core) = lazy.get() else {
        return lifecycle::Life::Unknown;
    };
    match gens::find(core, id, n, &text(l.get("hcName"))) {
        gens::Found::One(t) => {
            let argv: Vec<String> = l["launcherArgv"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a.as_str().map(String::from))
                .collect();
            lifecycle::classify(Some(&t), &argv)
        }
        gens::Found::Unknown(_) | gens::Found::Missing(_) => lifecycle::Life::Unknown,
    }
}

/// E4.4: each Running member's view in `view_caller`'s session, found, adopted or opened by views-6f's
/// `view_locked` under the member lock, never selected; a missing one is a new window (no split: `None`, call 29).
/// A member that fails is skipped, its message kept.
pub fn views(ctx: &Ctx, view_caller: &Caller, core: &Core<'_>, running: &[&Running]) -> Views {
    let mut got = Views {
        shown: Vec::new(),
        duplicates: 0,
        error: None,
    };
    let team = crate::agents::scope::team(view_caller);
    let lock = crate::agents::scope::state(view_caller);
    for m in running {
        let Some(id) = crate::agents::core_identity::Identity::read(&m.record, &m.id, &team) else {
            continue;
        };
        let found = crate::agents::lock(ctx, &lock.member_lock(&id.id), || {
            crate::agents::view::view_locked(ctx, Some(core), view_caller, &id, None, || None)
        });
        match found {
            Ok(Some(f)) => {
                got.duplicates += f.duplicates;
                got.shown.push(
                    json!({"id": m.id, "name": m.record["name"], "pane": f.pane.pane,
                    "window": f.pane.window}),
                );
            }
            Ok(None) => {}
            Err(e) => {
                got.error.get_or_insert(e.message);
            }
        }
    }
    got
}

/// E4.5, ambiguity 13, calls 28, 40: the session's status window, reused when one carries `STATUS`; a crash between
/// creation and marking leaves an unmarked `status` window, adopted and marked; else one is made with no command (the
/// default shell) by id, then marked by id, so its bar stays.
pub fn status_window(ctx: &Ctx, view_caller: &Caller) -> Res<String> {
    let target = format!("{}:", crate::agents::scope::shown(view_caller).session);
    let format = format!(
        "#{{window_id}}|#{{window_name}}|#{{{}}}",
        crate::sidebar::STATUS
    );
    let listed = native(ctx, &["list-windows", "-t", &target, "-F", &format], false)?;
    let rows: Vec<Vec<&str>> = listed
        .stdout
        .lines()
        .map(|l| l.trim_end().split('|').collect())
        .collect();
    if let Some(r) = rows.iter().find(|r| r.get(2) == Some(&"1")) {
        return Ok(r[0].to_string());
    }
    let id = match rows.iter().find(|r| r.get(1) == Some(&"status")) {
        Some(r) => r[0].to_string(),
        None => {
            let out = native(
                ctx,
                &[
                    "new-window",
                    "-d",
                    "-P",
                    "-F",
                    "#{window_id}",
                    "-n",
                    "status",
                    "-t",
                    &target,
                ],
                false,
            )?;
            out.stdout
                .lines()
                .next()
                .unwrap_or_default()
                .trim_end()
                .to_string()
        }
    };
    if id.is_empty() {
        return Err(Refusal::internal("the status window was not made"));
    }
    native(
        ctx,
        &["set-option", "-w", "-t", &id, crate::sidebar::STATUS, "1"],
        false,
    )?;
    Ok(id)
}

/// E4.6, ambiguity 14: the leader when Running; else the earliest first start (unreadable last), ties by id.
pub fn choose(leader: Option<&str>, running: &[(String, Option<String>)]) -> Option<String> {
    if let Some(l) = leader.filter(|l| running.iter().any(|r| r.0 == *l)) {
        return Some(l.to_string());
    }
    let mut v: Vec<&(String, Option<String>)> = running.iter().collect();
    v.sort_by(|a, b| (a.1.is_none(), &a.1, &a.0).cmp(&(b.1.is_none(), &b.1, &b.0)));
    v.first().map(|r| r.0.clone())
}

/// A member's first start (`gens/0/launcher.json` `startedAt`, through `read_flushed`); display order only.
fn first_start(ctx: &Ctx, scope: &crate::agents::team_state::Scope, id: &str) -> Option<String> {
    let folder = scope.agents(&ctx.state_root).join(id);
    gens::read_flushed(&folder, 0, "launcher")
        .ok()
        .flatten()
        .and_then(|v| v.get("startedAt").and_then(Value::as_str).map(String::from))
}

/// The bar of the chosen window, the window and its pane, the client (skipped with `viewError` and no view shown:
/// no layout without a view, call 23), then `halt("presented")`, then every outstanding release (ambiguity 7: last).
fn present(
    ctx: &Ctx,
    caller: &Caller,
    ix: &mut index::Index,
    shown: &publish::Published,
    (window, pane): (&str, &str),
    reply: &mut Map<String, Value>,
) -> Res<()> {
    let no_view = reply.contains_key("viewError")
        && reply
            .get("views")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty);
    if !no_view && !window.is_empty() {
        let dest = at_view(caller, ix, shown);
        crate::sidebar::keep(ctx, &dest, window, reply);
        let target = format!("={}", shown.view.name);
        let switched = server::same(ctx, &dest)
            .and_then(|_| native(ctx, &["select-window", "-t", window], false))
            .and_then(|o| match pane {
                "" => Ok(o),
                p => native(ctx, &["select-pane", "-t", p], false),
            })
            .and_then(|_| server::same(ctx, &dest))
            .and_then(|_| native(ctx, &["switch-client", "-t", &target], false));
        if let Err(e) = switched {
            reply.insert("layoutError".into(), e.message.into());
        }
    }
    super::halt("presented")?;
    // E5, calls 17, 25, 35 (finding A6): every outstanding release published and read back `Present`.
    if let Err(e) = publish::release_all(ctx, ix) {
        reply.insert("releaseError".into(), e.message.into());
    }
    Ok(())
}
