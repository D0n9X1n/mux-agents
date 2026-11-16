use super::{Caller, Kind};
use crate::{
    agents::{self, core_identity::Identity},
    ctx::Ctx,
    error::Res,
};
use std::path::{Path, PathBuf};
/// `state_root` joined with `rel`, only where it is itself: its canonical path is the state root's canonical path
/// joined with the same names, so no link on the way out (#104 phase 1c). The worker proof and the member proof (D1,
/// #103) read every file through it. shortcut: checked before the open, not by it; the state root is this user's own,
/// private folder.
pub(crate) fn within_root(state_root: &Path, rel: &[&str]) -> Option<PathBuf> {
    let root = std::fs::canonicalize(state_root).ok()?;
    let at = rel.iter().fold(state_root.to_path_buf(), |p, r| p.join(r));
    let real = rel.iter().fold(root, |p, r| p.join(r));
    (std::fs::canonicalize(&at).ok()? == real).then_some(at)
}
pub(crate) fn applies(ctx: &Ctx) -> bool {
    let set = |k| ctx.env.get(k).is_some_and(|s| !s.is_empty());
    set("HC_TERMINAL_ID") && !set("TMUX") && !set("RMUX")
}
pub(crate) fn resolve(ctx: &Ctx) -> Res<Caller> {
    prove(ctx)
        .ok()
        .flatten()
        .ok_or_else(|| crate::error::Refusal {
            code: 2,
            check: "herdr",
            message: crate::cli::HERDR.into(),
            reason: None,
            leftover: None,
            details: Default::default(),
        })
}
fn prove(ctx: &Ctx) -> Res<Option<Caller>> {
    let core = crate::hc::connect(ctx)?;
    let t = core.get(ctx.env.get("HC_TERMINAL_ID").unwrap_or(""))?;
    let read = || -> Option<Caller> {
        let walked = t.pid.and_then(|pid| super::walk(ctx, pid).ok());
        let address = ctx.env.get("HC_TERMINAL_ID")?;
        let team = t.labels.get("mux.team")?;
        let (key, session) = team.split_once(':')?;
        if key.len() != 16
            || !key
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !session.strip_prefix('$').is_some_and(super::digits)
        {
            return None;
        }
        let id = t.labels.get("mux.id")?;
        if !agents::agent_id(id) {
            return None;
        }
        // A file is read only where it is itself, inside the state root: no link on the way out (#104 phase 1c).
        let inside = |rel: &[&str]| within_root(&ctx.state_root, rel);
        let folder = inside(&[key, "agents", id])?;
        inside(&[key, "agents", id, "record.json"])?; // R5: checked before anything is opened
        // R1, R2, R4: one member_of, one status; the active generation commits and presents.
        let (r, committed) = match agents::gens::member_of(&folder) {
            agents::gens::Member::Life(g) => {
                let s = g.status().ok()?; // R2: Unreadable is no caller (refused `herdr`)
                if !s.active_committed {
                    return None;
                }
                (agents::team_state::presented(&g, &s).ok()?, true)
            }
            agents::gens::Member::Pre(r) => {
                inside(&[key, "agents", id, "commit.json"])?;
                (r, agents::folder_committed(&folder))
            }
            agents::gens::Member::Absent | agents::gens::Member::Unreadable(_) => return None,
        };
        if !committed {
            return None;
        }
        let identity = Identity::read(&r, id, team)?;
        if identity.address != address || !agents::tree::launch_holds(&identity, &t, committed) {
            return None;
        }
        let pane = r["pane"].as_str()?;
        let window = r["window"].as_str()?;
        if !agents::pane_id(pane) || !window.strip_prefix('@').is_some_and(super::digits) {
            return None;
        }
        let server = crate::state::fs::read_json(&inside(&[key, "server.json"])?).ok()??;
        let pid = u32::try_from(server["serverPid"].as_u64()?).ok()?;
        let start = server["serverStart"].as_i64()?;
        let socket = server["socket"].as_str()?;
        crate::mux::endpoint(
            &crate::ctx::Env::from_pairs([("TMUX", format!("{socket},{pid},0"))]),
            crate::state::root::HOST,
        )?;
        if ctx.os.open(pid)?.start != start
            || crate::util::sha256_hex(format!("{socket}|{pid}|{start}"))[..16] != *key
        {
            return None;
        }
        let proven = walked.is_some() && {
            // The reading the proof uses is checked as the first was: hc may name the foreground only on a later one.
            let t = agents::tree::sampled(ctx, &core, t.clone());
            agents::tree::launch_holds(&identity, &t, committed)
                && agents::tree::prove_member(ctx, team, id, &r, &t).is_some()
        };
        Some(Caller {
            kind: Kind::Agent,
            client: Some(agents::spawn::client_named(r["client"].as_str()?)?),
            agent: None,
            chain: walked.unwrap_or_default(),
            pane: pane.into(),
            window: window.into(),
            session: session.into(),
            server_pid: pid,
            server_start: start,
            socket: socket.into(),
            server_key: key.into(),
            agent_tag: id.into(),
            member: proven.then(|| id.to_string()),
            candidate: (!proven).then(|| id.to_string()),
            presentation: Some((pane.into(), window.into())),
        })
    };
    Ok(read())
}
