//! reentry-6a E0: the one place a caller becomes a state scope (6a-R1, call 10). State follows the anchor pair; the
//! multiplexer (socket, session, pane) stays the caller's own, reached through `shown`.
use crate::caller::{Anchor, Caller, Kind};
use crate::ctx::Ctx;
use crate::error::{Refusal, Res};

use super::team_state::Scope;
use super::up::index::{self, Index, Proof};

/// The scope a caller's state lives in: its anchor, else its own server key and session.
pub fn state(c: &Caller) -> Scope {
    match &c.anchor {
        Some(a) => Scope {
            key: a.key.clone(),
            session: a.session.clone(),
        },
        None => shown(c),
    }
}

/// Calls 12, 39: the caller is on its state scope's own server, so it may bind that key's `server.json`.
pub fn on_anchor(c: &Caller) -> bool {
    state(c).key == shown(c).key
}

/// The presentation pair: the caller's own server key and session (bar locks, view placement).
pub fn shown(c: &Caller) -> Scope {
    Scope {
        key: c.server_key.clone(),
        session: c.session.clone(),
    }
}

/// `<key>:<session>` of the state scope: `mux.team`, `membership::team`, a record's `team`.
pub fn team(c: &Caller) -> String {
    let s = state(c);
    format!("{}:{}", s.key, s.session)
}

/// Re6a call 43: the lock of the shown session (the bar's), `<kind>|<shown key>|<shown session>`.
pub fn shown_lock(kind: &str, c: &Caller) -> String {
    let s = shown(c);
    format!("{kind}|{}|{}", s.key, s.session)
}

/// E2: the refusal of a person in a session that is not its team's view.
pub const MOVED: &str = "this session is not the team's view; run up";

/// What the indexes say about one session (ambiguity 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    Anchor(Anchor),
    Moved,
    Free,
}

/// Ambiguity 4: what the indexes say about the session `id` (named `name` when read) on the caller's server `key`.
/// A view claims only when its server is proven live; two claims are 1 `index`.
pub fn claim(ctx: &Ctx, all: &[Index], key: &str, id: &str, name: Option<&str>) -> Res<Claim> {
    let mut found = Vec::new();
    for ix in all {
        if let (Some(v), Some(a)) = (&ix.view, &ix.anchor)
            && v.server.key == key
            && v.session == id
            && index::prove(ctx, &v.server)? == Proof::Live
        {
            found.push(Claim::Anchor(a.clone()));
        }
        let pending = (ix.pending.as_ref())
            .is_some_and(|p| p.server.key == key && Some(p.name.as_str()) == name);
        let alias = ix
            .aliases
            .iter()
            .any(|v| v.server.key == key && v.session == id);
        if pending || alias {
            found.push(Claim::Moved);
        }
    }
    match found.len() {
        0 => Ok(Claim::Free),
        1 => Ok(found.remove(0)),
        _ => Err(index::bad("two teams claim this session")),
    }
}

/// E2 for a person (human, no member): an anchor sets `c.anchor`; `moved` refuses; no claim keeps today's scope.
/// The session name is read only when some pending names the caller's server.
pub fn admit(ctx: &Ctx, c: &mut Caller) -> Res<()> {
    if c.kind != Kind::Human || c.member.is_some() {
        return Ok(());
    }
    let all = index::every(ctx)?;
    let key = c.server_key.clone();
    let name = if all
        .iter()
        .any(|ix| ix.pending.as_ref().is_some_and(|p| p.server.key == key))
    {
        let args = [
            "display-message",
            "-p",
            "-t",
            c.session.as_str(),
            "#{session_name}",
        ];
        let out = crate::mux::native(ctx, &args, false)?;
        Some(out.stdout.trim_end_matches(['\r', '\n']).to_string())
    } else {
        None
    };
    match claim(ctx, &all, &key, &c.session, name.as_deref())? {
        Claim::Anchor(a) => c.anchor = Some(a),
        Claim::Moved => {
            return Err(Refusal {
                code: 1,
                check: "moved",
                message: MOVED.into(),
                reason: None,
                leftover: None,
                details: Default::default(),
            });
        }
        Claim::Free => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests;
