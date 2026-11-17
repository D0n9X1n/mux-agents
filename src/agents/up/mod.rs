//! up: a person's verb that creates a folder's team (#103, 2a-iii).

pub mod discover;
#[cfg(test)]
mod existing_tests;
pub mod identity;
pub mod index;
pub mod publish;
#[cfg(test)]
mod publish_tests;
pub mod server;
#[cfg(test)]
pub(crate) mod tests;

use std::{cell::Cell, path::Path};

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

pub const OUTSIDE: &str = "up needs tmux or rmux for now";

struct Plan<'a> {
    client: Client,
    /// The client options of the person (options-6e E1): the leader replays them, the ceiling is fixed from them.
    options: &'a [String],
    /// The prompt tokens of the person: run once, never replayed.
    prompt: &'a [String],
    folder: &'a Path,
    team: &'a str,
}

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
    let team = identity::team(ctx, &folder, client, request.value("Name").unwrap_or(""))?;
    admit(ctx, &format!("up|{team}"), || {
        // cascade-3d C8: the caller's session's journals first (lock order up|T, then the team lock).
        let scope = super::team_state::Scope::of(&caller);
        if super::cascade::waiting(ctx, &scope) {
            super::team_state::open(ctx, &scope, |tx| super::cascade::finish(tx, &caller))?;
        }
        // 6b ambiguities 7, 8: an existing lifecycle team is reconciled at its anchor, not refused.
        if let Some(scope) =
            discover::existing(ctx, &core, &super::scope::state(&caller).key, &team)?
        {
            let anchor = super::reconcile::anchor_caller(&caller, &scope);
            let mut reply = super::reconcile::run(ctx, &anchor)?;
            reply.insert("team".into(), json!(team));
            reply.insert("created".into(), json!(false));
            return Ok(reply);
        }
        create(
            ctx,
            &caller,
            &core,
            &Plan {
                client,
                options: &classified.options,
                prompt: &classified.prompt,
                folder: &folder,
                team: &team,
            },
        )
    })
}

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

fn create(ctx: &Ctx, caller: &Caller, core: &Core<'_>, plan: &Plan<'_>) -> Res<Map<String, Value>> {
    let up_dir = ctx.state_root.join("up");
    #[cfg(unix)]
    crate::state::private_unix::private_dirs(&ctx.state_root, &[&up_dir])?;
    #[cfg(windows)]
    crate::state::private_windows::private_dirs(&[&up_dir])?;
    let release = up_dir.join(format!("{}.release", crate::util::hex32()?));
    let name = identity::session_name(plan.folder, plan.team)?;
    // Step 1, never -A: a taken name refuses here with nothing to clean.
    let out = native(
        ctx,
        &[
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{session_id} #{window_id}",
            "-s",
            &name,
            "-c",
            utf8(plan.folder)?,
            utf8(&ctx.exe)?,
            "hold",
            utf8(&release)?,
        ],
        false,
    )?;
    // Before the commit every failure writes the release file, so `hold` ends. Until step 2 has succeeded, or when the
    // write fails, the session may stay: the refusal names it.
    let step2 = Cell::new(false);
    let fail = |mut e: Refusal| {
        if crate::state::evidence::publish(&release, &serde_json::json!({})).is_err()
            || !step2.get()
        {
            e.leftover = Some(name.clone());
        }
        e
    };
    let (session, window) =
        ids(&out.stdout).ok_or_else(|| fail(Refusal::internal("unproven new session")))?;
    let dest = Caller {
        kind: Kind::Human,
        client: None,
        agent: None,
        member: None,
        candidate: None,
        chain: Vec::new(),
        pane: String::new(),
        window: window.clone(),
        session: session.clone(),
        presentation: Some((String::new(), window.clone())),
        ..caller.clone()
    };
    server::same(ctx, &dest).map_err(fail)?;
    native(
        ctx,
        &[
            "set-option",
            "-w",
            "-t",
            &format!("={name}:{window}"),
            "remain-on-exit",
            "off",
        ],
        false,
    )
    .map_err(fail)?;
    step2.set(true);
    let leader = Leader {
        team: plan.team.into(),
        options: plan.options.to_vec(),
        prompt: plan.prompt.to_vec(),
    };
    let record = lock(ctx, &super::team_lock(&dest), || {
        server::same(ctx, &dest)?;
        let request = CoreRequest {
            client: plan.client,
            task: "",
            brief: None,
            directory: plan.folder,
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
    .map_err(fail)?;
    let mut reply = Map::new();
    for (k, v) in [
        ("team", json!(plan.team)),
        ("session", json!(session)),
        ("name", json!(name)),
        (
            "leader",
            json!({"id": record["id"], "name": record["name"], "hcAddress": record["hcAddress"]}),
        ),
        ("created", json!(true)),
    ] {
        reply.insert(k.into(), v);
    }
    // PM call 10 (G1.6): a view that failed after the commit is reported. The leader, its session and the team
    // record stay; there is no window to lay out, so `layout` is skipped.
    let view_error = record.get("viewError").cloned();
    if let Some(e) = &view_error {
        reply.insert("viewError".into(), e.clone());
    }
    // Step 5 after the commit, still under the admission lock; step 6 reports each failure; the leader and session stay.
    let team_error = write_team(ctx, &dest, plan, &name, &record).err();
    if view_error.is_none()
        && let Err(e) = layout(
            ctx,
            &dest,
            &release,
            &name,
            &text(record.get("window")),
            &mut reply,
        )
    {
        reply.insert("layoutError".into(), e.message.into());
    }
    if let Some(e) = team_error {
        reply.insert("teamRecordError".into(), e.message.into());
    }
    Ok(reply)
}

/// Step 6: release, the bar of the view's window (#122 sidebar::keep), the view's window by its `@id`, the client.
fn layout(
    ctx: &Ctx,
    dest: &Caller,
    release: &Path,
    name: &str,
    view: &str,
    reply: &mut Map<String, Value>,
) -> Res<()> {
    let wrote = crate::state::evidence::publish(release, &serde_json::json!({})).map(|_| ());
    crate::sidebar::keep(ctx, dest, view, reply);
    wrote?;
    server::same(ctx, dest)?;
    native(ctx, &["select-window", "-t", view], false)?;
    server::same(ctx, dest)?;
    native(ctx, &["switch-client", "-t", &format!("={name}")], false).map(|_| ())
}

/// Step 5: the team record, create-only (#118 team::create).
fn write_team(ctx: &Ctx, dest: &Caller, plan: &Plan<'_>, name: &str, record: &Value) -> Res<()> {
    use super::team;
    let made = team::Team {
        team: plan.team.into(),
        session: super::scope::state(dest).session,
        name: name.into(),
        directory: utf8(plan.folder)?.into(),
        client: plan.client,
        flags: plan.options.to_vec(),
        policy: team::policy(plan.client, plan.options),
        leader: text(record.get("id")),
        created_at: crate::util::utc_o(ctx.clock.now()),
    };
    if team::create(
        &team::path(
            &super::server_dir(ctx, dest),
            &super::scope::state(dest).session,
        ),
        &made,
    )? {
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

/// `$N @M` from new-session's `-P -F`.
fn ids(stdout: &str) -> Option<(String, String)> {
    let (s, w) = stdout.trim_end_matches(['\r', '\n']).split_once(' ')?;
    let id = |t: &str, p: char| {
        t.strip_prefix(p)
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    (id(s, '$') && id(w, '@')).then(|| (s.into(), w.into()))
}
