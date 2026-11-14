//! up: a person's verb that creates a folder's team (#103, 2a-iii).

pub mod discover;
pub mod identity;
pub mod server;
#[cfg(test)]
mod tests;

use std::{cell::Cell, path::Path};

use serde_json::{Map, Value, json};

use super::spawn::{CoreRequest, CoreSlot, client_named, launch_hc_locked, utf8};
use super::{admit, leader::Leader, lock, lock_key, registry, text, usage};
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
    flags: &'a [String],
    folder: &'a Path,
    team: &'a str,
}

pub fn up(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let client = request
        .value("Client")
        .and_then(client_named)
        .ok_or_else(|| usage("up"))?;
    let caller = person(ctx)?;
    let core = crate::hc::connect(ctx)?;
    let folder = identity::folder(ctx)?;
    let team = identity::team(ctx, &folder, client, request.value("Name").unwrap_or(""))?;
    admit(ctx, &format!("up|{team}"), || {
        discover::refuse_existing(ctx, &core, &team)?;
        create(
            ctx,
            &caller,
            &core,
            &Plan {
                client,
                flags: &request.rest,
                folder: &folder,
                team: &team,
            },
        )
    })
}

/// A person in a tmux or rmux pane; members and agents are refused `tagged`.
fn person(ctx: &Ctx) -> Res<Caller> {
    if crate::caller::hc::applies(ctx) {
        return refuse(2, "tagged", "a member cannot run up");
    }
    let pane = ctx.env.get("TMUX_PANE").unwrap_or("");
    if crate::mux::endpoint(&ctx.env, crate::state::root::HOST).is_none() || !super::pane_id(pane) {
        return refuse(1, "endpoint", OUTSIDE);
    }
    let caller = crate::caller::resolve(ctx)?;
    if caller.kind == Kind::Agent || caller.member.is_some() {
        return refuse(2, "tagged", "an agent cannot run up");
    }
    registry::untagged(ctx, &caller, "an agent pane cannot run up")?;
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
        if std::fs::write(&release, b"").is_err() || !step2.get() {
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
        chain: Vec::new(),
        pane: String::new(),
        window: window.clone(),
        session: session.clone(),
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
        flags: plan.flags.to_vec(),
    };
    let record = lock(ctx, &lock_key("spawn", &dest, &dest.session), || {
        server::same(ctx, &dest)?;
        let request = CoreRequest {
            client: plan.client,
            task: "",
            brief: None,
            directory: plan.folder,
            model: None,
            no_fork: true,
            split: None,
            resume: None,
            leader: Some(&leader),
        };
        launch_hc_locked(ctx, &dest, core, request, |_| {
            Ok(CoreSlot {
                name: "leader".into(),
                color: crate::paint::identity(None, "leader"),
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
    // Step 5 after the commit, still under the admission lock; step 6 reports each failure; the leader and session stay.
    let team_error = write_team(ctx, &dest, plan, &name, &record).err();
    if let Err(e) = layout(
        ctx,
        &dest,
        &release,
        &name,
        &text(record.get("window")),
        &mut reply,
    ) {
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
    let wrote = std::fs::write(release, b"");
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
        session: dest.session.clone(),
        name: name.into(),
        directory: utf8(plan.folder)?.into(),
        client: plan.client,
        flags: plan.flags.to_vec(),
        policy: team::policy(plan.client, plan.flags),
        leader: text(record.get("id")),
        created_at: crate::util::utc_o(ctx.clock.now()),
    };
    if team::create(
        &team::path(&super::server_dir(ctx, dest), &dest.session),
        &made,
    )? {
        Ok(())
    } else {
        refuse(1, "team", "the team record already exists")
    }
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
