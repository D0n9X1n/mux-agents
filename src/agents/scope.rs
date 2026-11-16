//! reentry-6a E0: the one place a caller becomes a state scope (6a-R1, call 10). State follows the anchor pair; the
//! multiplexer (socket, session, pane) stays the caller's own, reached through `shown`.
use crate::caller::Caller;

use super::team_state::Scope;

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

#[cfg(test)]
mod tests;
