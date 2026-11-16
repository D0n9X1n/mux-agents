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
        let lifecycle = agents::gens::lifecycle(&r);
        // K2, W3: a lifecycle member is proven by hc, gens and its client; its view and live server only present it.
        // A pre-lifecycle member still needs both, as before.
        let (pid, start, socket) = server_of(&inside(&[key, "server.json"])?, key)?;
        let live = ctx.os.open(pid).is_some_and(|o| o.start == start);
        let view = view_of(&r);
        let presentation = view.filter(|_| live);
        if !lifecycle && presentation.is_none() {
            return None;
        }
        // 4to6 #2: pane and window are the presentation's, empty when the views are down (ambiguity 9).
        let (pane, window) = presentation.clone().unwrap_or_default();
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
            pane: pane.clone(),
            window: window.clone(),
            session: session.into(),
            server_pid: pid,
            server_start: start,
            socket,
            server_key: key.into(),
            agent_tag: id.into(),
            member: proven.then(|| id.to_string()),
            candidate: (!proven).then(|| id.to_string()),
            presentation,
        })
    };
    Ok(read())
}

/// `server.json` bound to `key` (ambiguity 11): its socket, pid and start hash to the key and the socket is an
/// endpoint. Whether that server still runs is not asked here.
fn server_of(at: &Path, key: &str) -> Option<(u32, i64, String)> {
    // 4to6 #18: evidence::read opens only a regular file; a FIFO, folder or link is Unreadable, never opened.
    let crate::state::evidence::Evidence::Present(server) = crate::state::evidence::read(at) else {
        return None;
    };
    let pid = u32::try_from(server["serverPid"].as_u64()?).ok()?;
    let start = server["serverStart"].as_i64()?;
    let socket = server["socket"].as_str()?.to_string();
    crate::mux::endpoint(
        &crate::ctx::Env::from_pairs([("TMUX", format!("{socket},{pid},0"))]),
        crate::state::root::HOST,
    )?;
    (crate::util::sha256_hex(format!("{socket}|{pid}|{start}"))[..16] == *key)
        .then_some((pid, start, socket))
}

/// The presented record's pane and window, when both are well formed.
fn view_of(r: &serde_json::Value) -> Option<(String, String)> {
    let (pane, window) = (r["pane"].as_str()?, r["window"].as_str()?);
    (agents::pane_id(pane) && window.strip_prefix('@').is_some_and(super::digits))
        .then(|| (pane.to_string(), window.to_string()))
}
