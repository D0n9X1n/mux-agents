//! up: a person's verb that creates a folder's team (#103, 2a-iii).

pub mod discover;
#[cfg(test)]
mod existing_tests;
pub mod identity;
pub mod index;
#[cfg(test)]
mod leader_tests;
mod leaders;
pub mod publish;
#[cfg(test)]
mod publish_tests;
pub mod reenter;
#[cfg(test)]
mod reenter_tests;
pub mod replay;
pub mod server;
#[cfg(test)]
pub(crate) mod tests;

use std::path::Path;

use serde_json::{Map, Value, json};

use super::spawn::{CoreRequest, CoreSlot, client_named, launch_hc_locked, utf8};
use super::{admit, leader::Leader, lock, registry, text, usage};
use crate::{
    caller::{Caller, Kind},
    cli::Request,
    ctx::Ctx,
    error::{Refusal, Res, refuse},
    hc::Core,
    mux::native,
    parse::Client,
};
pub(crate) use leaders::leader_of;

pub const OUTSIDE: &str = "up needs tmux or rmux for now";

pub fn up(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let client = request
        .value("Client")
        .and_then(client_named)
        .ok_or_else(|| usage("up"))?;
    // E1: an unknown client flag refuses usage before anything is made or connected; session selectors are dropped.
    let classified = super::options::classify(client, &request.rest)?;
    // Windows accepts a relative MUX_AGENTS_STATE. `hold` and the launcher take only an absolute root, and `up` cannot
    // hand its later steps another root (they read ctx.state_root); a verb or bar run later would also resolve the same
    // text against its own folder. So a relative root is refused before anything is made (#103).
    if !ctx.state_root.is_absolute() {
        return refuse(1, "state", "the state folder path is not absolute");
    }
    let caller = person(ctx)?;
    let core = crate::hc::connect(ctx)?;
    let folder = identity::folder(ctx)?;
    let name = request.value("Name").unwrap_or("").to_string();
    let team = identity::team(ctx, &folder, client, &name)?;
    admit(ctx, &format!("up|{team}"), || {
        // cascade-3d C8: the caller's session's journals first (lock order up|T, then the team lock).
        let scope = super::team_state::Scope::of(&caller);
        if super::cascade::waiting(ctx, &scope) {
            super::team_state::open(ctx, &scope, |tx| super::cascade::finish(tx, &caller))?;
        }
        let fresh = index::read(ctx, &team)?.is_none();
        if fresh {
            // Ambiguity 9, call 1: a lifecycle leader of T with no index was made by an earlier build.
            if discover::existing(ctx, &core, &super::scope::state(&caller).key, &team)?.is_some() {
                return Err(index::bad(NO_INDEX));
            }
            let inputs = index::Inputs {
                client: super::spawn::client_name(client).into(),
                options: classified.options.clone(),
                prompt: classified.prompt.clone(),
                name: name.clone(),
                folder: utf8(&folder)?.into(),
            };
            let mut made = index::Index {
                team: team.clone(),
                inputs,
                anchor: None,
                view: None,
                pending: None,
                aliases: vec![],
                releases: vec![],
            };
            made.pending = Some(publish::new_pending(ctx, &caller, &made)?);
            let up_dir = ctx.state_root.join("up");
            #[cfg(unix)]
            crate::state::private_unix::private_dirs(&ctx.state_root, &[&up_dir])?;
            #[cfg(windows)]
            crate::state::private_windows::private_dirs(&[&up_dir])?;
            index::write(ctx, &made)?; // E6.1, call 20: {team, inputs, pending}
            halt("index")?;
        }
        drive(ctx, &caller, &core, &team, fresh)
    })
}

/// Call 1.
pub const NO_INDEX: &str = "this team has a leader but no index (a development build made it); run mux-agent stop leader in the team, then up again";

/// A person in a tmux or rmux pane; members and agents are refused `tagged`.
/// reentry-6a E2: `up` needs only person proof, never the index (`resolve_person`).
pub(crate) fn person(ctx: &Ctx) -> Res<Caller> {
    gate(ctx, "run up", false)
}

/// The person gate of every person verb that reuses `up`'s (views-6f ambiguity 13): `what` completes the
/// refusals, so `up`'s own texts ("a member cannot run up", ..) are unchanged. These verbs are admitted (E2).
pub(crate) fn person_as(ctx: &Ctx, what: &str) -> Res<Caller> {
    gate(ctx, what, true)
}

fn gate(ctx: &Ctx, what: &str, admitted: bool) -> Res<Caller> {
    if crate::caller::hc::applies(ctx) {
        return refuse(2, "tagged", format!("a member cannot {what}"));
    }
    let pane = ctx.env.get("TMUX_PANE").unwrap_or("");
    if crate::mux::endpoint(&ctx.env, crate::state::root::HOST).is_none() || !super::pane_id(pane) {
        return refuse(1, "endpoint", OUTSIDE);
    }
    let caller = if admitted {
        crate::caller::resolve(ctx)?
    } else {
        crate::caller::resolve_person(ctx)?
    };
    if caller.kind == Kind::Agent || caller.member.is_some() {
        return refuse(2, "tagged", format!("an agent cannot {what}"));
    }
    registry::untagged(ctx, &caller, &format!("an agent pane cannot {what}"))?;
    Ok(caller)
}

/// The team record at the anchor, by the typed reader: this team's (T and session), absent, or 1 `index` (calls 21,
/// 36; F7: `Team::from_json`).
fn team_exists(ctx: &Ctx, ix: &index::Index) -> Res<bool> {
    use crate::state::evidence::{Evidence, read};
    let Some(a) = &ix.anchor else {
        return Ok(false);
    };
    let at = super::team::path(&ctx.state_root.join(&a.key), &a.session);
    let unreadable = || index::bad(format!("the team record {} cannot be read", at.display()));
    // Call 36 (finding A7): through `evidence` (a wrong-type ancestor is `index`; a FIFO is never opened).
    let v = match read(&at) {
        Evidence::Absent => return Ok(false),
        Evidence::Unreadable => return Err(unreadable()),
        Evidence::Present(v) => v,
    };
    let t = super::team::Team::from_json(&v).ok_or_else(unreadable)?;
    if t.team == ix.team && t.session == a.session {
        Ok(true)
    } else {
        Err(index::bad(format!(
            "the team record {} is another team's",
            at.display()
        )))
    }
}

/// E6 replay (calls 19, 21): every `up` resumes the first unfinished step from the index, the anchor's leader and its
/// team record. `fresh`: this `up` wrote the index, so its `pending` is this call's and is started, not repaired.
fn drive(
    ctx: &Ctx,
    caller: &Caller,
    core: &Core<'_>,
    team: &str,
    fresh: bool,
) -> Res<Map<String, Value>> {
    use replay::Step;
    let mut ix = index::read(ctx, team)?.ok_or_else(|| Refusal::internal("index vanished"))?;
    let reentered = team_exists(ctx, &ix)?;
    leaders::readable(ctx, &ix)?; // restore-6c decision 9
    let mut run = Run::default();
    if fresh {
        run.shown = Some(publish::start(ctx, caller, &mut ix)?);
    }
    for _ in 0..8 {
        let (leader, id) = leader_of(ctx, &ix)?;
        // Finding A22: after a committed launch a team record that cannot be read never hides the launch reply.
        let has_team = match team_exists(ctx, &ix) {
            Ok(t) => t,
            Err(e) if run.record.is_some() => {
                run.team_error = Some(e.message);
                return reenter::arm(ctx, caller, core, team, &mut ix, run, reentered);
            }
            Err(e) => return Err(e),
        };
        match replay::next(&ix, leader, has_team, run.launched) {
            Step::Repair => {
                publish::repair(ctx, caller, &mut ix)?; // its releases stay in the index for E5 (call 35)
                halt("repaired")?;
            }
            Step::Session => run.shown = Some(publish::ensure(ctx, caller, &mut ix)?),
            Step::Launch => {
                let p = match run.shown.take() {
                    Some(p) => p,
                    None => publish::ensure(ctx, caller, &mut ix)?,
                };
                let key = ix
                    .anchor
                    .as_ref()
                    .map(|a| a.key.clone())
                    .unwrap_or_default();
                bind_anchor_server(ctx, caller, &key)?; // calls 33, 39: absent off the anchor is `index`
                run.record = Some(launch(ctx, caller, core, &ix, &p)?);
                (run.shown, run.launched) = (Some(p), true);
                run.replayed |= !fresh;
            }
            Step::Settle => {
                let id = id.ok_or_else(|| Refusal::internal("no leader id"))?;
                leaders::settle_leader(ctx, &ix, &id)?;
                run.replayed = true;
            }
            Step::Team => {
                let r = run
                    .record
                    .clone()
                    .ok_or_else(|| Refusal::internal("no launch record"))?;
                if let Err(e) = write_team(ctx, &ix, &r) {
                    run.team_error = Some(e.message); // finding A22: soft after commit (D25)
                    return reenter::arm(ctx, caller, core, team, &mut ix, run, reentered);
                }
                halt("team")?;
                run.created = true;
                run.replayed |= !fresh;
            }
            Step::Reenter => {
                if !run.launched {
                    // D22: E4.2's team record before the pass, so a restore of a leader whose first `up` died
                    // after commit finds it (restore-6c checklist 69 refuses an absent record).
                    if !has_team && let Some(id) = &id {
                        let r = leaders::leader_record(ctx, &ix, id)?;
                        match write_team(ctx, &ix, &r) {
                            Ok(()) => (run.created, run.replayed) = (true, true),
                            Err(e) => run.team_error = Some(e.message),
                        }
                    }
                    run.reconciled = Some(leaders::reconciled(ctx, caller, &ix, id.as_deref())?);
                }
                return reenter::arm(ctx, caller, core, team, &mut ix, run, reentered);
            }
        }
    }
    Err(Refusal::internal("up did not converge"))
}

/// What the replay loop carries into the Reenter arm.
#[derive(Default)]
struct Run {
    shown: Option<publish::Published>,
    record: Option<Value>,
    team_error: Option<String>,
    launched: bool,
    created: bool,
    replayed: bool,
    /// restore-6c Task 7: reconcile's pass, run before this arm (call 18).
    reconciled: Option<Map<String, Value>>,
}

/// Calls 33, 39 (findings A1, F1-F3): the anchor key `key`'s `server.json`. On its own server it is published
/// create-once in a private key folder before any index write names the anchor; elsewhere it is never invented. A
/// binding that is absent, malformed or whose tuple does not hash to `key` is 1 `index`.
pub(crate) fn bind_anchor_server(ctx: &Ctx, caller: &Caller, key: &str) -> Res<()> {
    let dir = ctx.state_root.join(key);
    let at = dir.join("server.json");
    if key == caller.server_key {
        super::spawn::private_folder(ctx, &dir)?;
        let body = json!({"socket": caller.socket, "serverPid": caller.server_pid, "serverStart": caller.server_start});
        crate::state::evidence::publish(&at, &body)?;
    }
    let bound = match crate::state::evidence::read(&at) {
        crate::state::evidence::Evidence::Present(v) => {
            let socket = v.get("socket").and_then(Value::as_str);
            let pid = v
                .get("serverPid")
                .and_then(Value::as_u64)
                .and_then(|p| u32::try_from(p).ok());
            let start = v.get("serverStart").and_then(Value::as_i64);
            matches!((socket, pid, start), (Some(s), Some(p), Some(t)) if index::key_of(s, p, t) == key)
        }
        _ => false,
    };
    if bound {
        Ok(())
    } else {
        Err(index::bad(format!(
            "the anchor's server binding {} is missing or not its own; run up on that server",
            at.display()
        )))
    }
}

/// The caller as a person in the view's session, its state at the anchor (multiplexer calls on the view).
fn at_view(caller: &Caller, ix: &index::Index, shown: &publish::Published) -> Caller {
    Caller {
        kind: Kind::Human,
        client: None,
        agent: None,
        member: None,
        candidate: None,
        chain: Vec::new(),
        pane: String::new(),
        window: shown.window.clone(),
        session: shown.view.session.clone(),
        presentation: Some((String::new(), shown.window.clone())),
        anchor: ix.anchor.clone(),
        ..caller.clone()
    }
}

/// E6.3: the leader's launch transaction at the anchor, from `inputs` (launch-1 G1 through `launch_hc_locked`).
fn launch(
    ctx: &Ctx,
    caller: &Caller,
    core: &Core<'_>,
    ix: &index::Index,
    shown: &publish::Published,
) -> Res<Value> {
    let dest = at_view(caller, ix, shown);
    server::same(ctx, &dest)?;
    let hold_window = format!("={}:{}", shown.view.name, shown.window);
    native(
        ctx,
        &[
            "set-option",
            "-w",
            "-t",
            &hold_window,
            "remain-on-exit",
            "off",
        ],
        false,
    )?;
    let client =
        client_named(&ix.inputs.client).ok_or_else(|| Refusal::internal("index client"))?;
    let leader = Leader {
        team: ix.team.clone(),
        options: ix.inputs.options.clone(),
        prompt: ix.inputs.prompt.clone(),
    };
    lock(ctx, &super::team_lock(&dest), || {
        server::same(ctx, &dest)?;
        let request = CoreRequest {
            client,
            task: "",
            brief: None,
            directory: Path::new(&ix.inputs.folder),
            model: None,
            no_fork: true,
            effort: None,
            split: None,
            resume: None,
            leader: Some(&leader),
        };
        launch_hc_locked(ctx, &dest, core, request, |_| {
            Ok(CoreSlot {
                name: "leader".into(),
                color: crate::paint::identity(None, "leader"),
                placed: None,
                debit: false,
            })
        })
    })
}

/// E6.4: the team record at the anchor, create-only (#118 team::create).
fn write_team(ctx: &Ctx, ix: &index::Index, record: &Value) -> Res<()> {
    let a = ix
        .anchor
        .as_ref()
        .ok_or_else(|| Refusal::internal("no anchor"))?;
    let client: Client =
        client_named(&ix.inputs.client).ok_or_else(|| Refusal::internal("index client"))?;
    let made = super::team::Team {
        team: ix.team.clone(),
        session: a.session.clone(),
        name: ix.view.as_ref().map(|v| v.name.clone()).unwrap_or_default(),
        directory: ix.inputs.folder.clone(),
        client,
        flags: ix.inputs.options.clone(),
        policy: super::team::policy(client, &ix.inputs.options),
        leader: text(record.get("id")),
        created_at: crate::util::utc_o(ctx.clock.now()),
    };
    let at = super::team::path(&ctx.state_root.join(&a.key), &a.session);
    if super::team::create(&at, &made)? {
        Ok(())
    } else {
        refuse(1, "team", "the team record already exists")
    }
}

#[cfg(test)]
thread_local! {
    static HALT: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}
/// Ambiguity 18: unit tests stop `up` after `step` (1 `halted`, "halted after <step>").
#[cfg(test)]
pub fn halt_after(step: Option<&'static str>) {
    HALT.with(|h| h.set(step));
}
/// A durable step of `up` is done: the live crash point, and the unit-test halt.
pub(crate) fn halt(step: &'static str) -> Res<()> {
    crate::state::evidence::crash_point(step);
    #[cfg(test)]
    if HALT.with(|h| h.get()) == Some(step) {
        return refuse(1, "halted", format!("halted after {step}"));
    }
    Ok(())
}
