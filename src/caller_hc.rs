use super::{Caller, Kind};
use crate::{
    agents::{self, core_identity::Identity},
    ctx::Ctx,
    error::Res,
};
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
        })
}
fn prove(ctx: &Ctx) -> Res<Option<Caller>> {
    let t = crate::hc::connect(ctx)?.get(ctx.env.get("HC_TERMINAL_ID").unwrap_or(""))?;
    let read = || -> Option<Caller> {
        let chain = super::walk(ctx, t.pid?).ok()?;
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
        // shortcut: checked before the open, not by it; the state root is this user's own, private folder.
        let root = std::fs::canonicalize(&ctx.state_root).ok()?;
        let inside = |rel: &[&str]| -> Option<std::path::PathBuf> {
            let at = rel.iter().fold(ctx.state_root.clone(), |p, r| p.join(r));
            let real = rel.iter().fold(root.clone(), |p, r| p.join(r));
            (std::fs::canonicalize(&at).ok()? == real).then_some(at)
        };
        let folder = inside(&[key, "agents", id])?;
        inside(&[key, "agents", id, "commit.json"])?;
        if !agents::committed(&folder) {
            return None;
        }
        let record = inside(&[key, "agents", id, "record.json"])?;
        let r = crate::state::fs::read_json(&record).ok()??;
        let identity = Identity::read(&r, id, team)?;
        if identity.address != address
            || !identity.matches(&t)
            || agents::lifecycle::classify(Some(&t), &identity.launcher)
                != agents::lifecycle::Life::Running
        {
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
        Some(Caller {
            kind: Kind::Agent,
            client: Some(agents::spawn::client_named(r["client"].as_str()?)?),
            agent: None,
            chain,
            pane: pane.into(),
            window: window.into(),
            session: session.into(),
            server_pid: pid,
            server_start: start,
            socket: socket.into(),
            server_key: key.into(),
            agent_tag: id.into(),
            member: Some(id.into()),
        })
    };
    Ok(read())
}
