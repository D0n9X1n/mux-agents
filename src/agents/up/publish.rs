//! reentry-6a E3, under `up|T`: repair a `pending`, reuse a proven live `view` on the caller's server, else write
//! `pending` durably and make the session. Nothing here kills (6a-R3); an unwanted session ends by its release,
//! which is kept in the index's `releases` until E5 publishes it (calls 25, 35).
use super::index::{self, Index, Pending, Proof, Server, View};
use crate::caller::{Anchor, Caller};
use crate::ctx::Ctx;
use crate::error::{Refusal, Res};

/// What a `list-sessions -F "#{session_id} #{session_name}"` answered: exit code and stdout.
pub type Listing = (i32, String);

#[cfg(test)]
thread_local! {
    static SESSIONS: std::cell::RefCell<std::collections::HashMap<String, Option<Listing>>> = Default::default();
    static ANSWERING: std::cell::RefCell<std::collections::HashMap<String, Option<u32>>> = Default::default();
}
/// Test seam (ambiguity 6): another server's raw listing; `None` panics if that server is ever listed.
#[cfg(test)]
pub fn test_sessions(socket: &str, answer: Option<Listing>) {
    SESSIONS.with(|s| s.borrow_mut().insert(socket.into(), answer));
}
/// Test seam (call 34): the pid another server's endpoint answers; `None` runs the real probe. Unset: the
/// recorded pid.
#[cfg(test)]
pub fn test_answering(socket: &str, pid: Option<u32>) {
    ANSWERING.with(|s| s.borrow_mut().insert(socket.into(), pid));
}

fn unlisted(s: &Server) -> crate::error::Refusal {
    index::bad(format!(
        "the sessions of server {} could not be listed",
        s.socket
    ))
}

/// Calls 16, 36: exit 0 and every line `$<digits> <name>`, the name the rest of the line (spaces allowed), else
/// 1 `index` naming the server; no line is skipped.
pub fn parse_listing(s: &Server, (code, stdout): Listing) -> Res<Vec<(String, String)>> {
    if code != 0 {
        return Err(unlisted(s));
    }
    let mut out = Vec::new();
    for line in stdout.lines().map(str::trim_end).filter(|l| !l.is_empty()) {
        let (id, name) = line.split_once(' ').ok_or_else(|| unlisted(s))?;
        let digits = id
            .strip_prefix('$')
            .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()));
        if !digits || name.is_empty() {
            return Err(unlisted(s));
        }
        out.push((id.to_string(), name.to_string()));
    }
    Ok(out)
}

/// One command on another server through its own endpoint and process start (`RealMux::for_server`, F5).
fn on_other(ctx: &Ctx, s: &Server, args: &[&str]) -> Res<crate::mux::Output> {
    use crate::mux::Mux;
    let endpoint = crate::mux::Endpoint {
        socket: s.socket.clone(),
        server_pid: s.pid,
    };
    let mux = crate::mux::RealMux::for_server(ctx, endpoint, s.start)?;
    let mut argv = vec!["-N"];
    argv.extend(args);
    mux.run(&argv)
}

/// `(session id, name)` of every session on `s`: the caller's own server through `native`, another through its
/// endpoint. Call 36: any transport failure is 1 `index` (a recovery-time failure keeps `pending`).
pub fn sessions_on(ctx: &Ctx, me: &Caller, s: &Server) -> Res<Vec<(String, String)>> {
    let args = ["list-sessions", "-F", "#{session_id} #{session_name}"];
    if s.key == me.server_key {
        let out = crate::mux::native(ctx, &args, true).map_err(|_| unlisted(s))?;
        return parse_listing(s, (out.code, out.stdout));
    }
    #[cfg(test)]
    if let Some(answer) = SESSIONS.with(|m| m.borrow().get(&s.socket).cloned()) {
        return parse_listing(
            s,
            answer.unwrap_or_else(|| panic!("{} must not be listed", s.socket)),
        );
    }
    let out = on_other(ctx, s, &args).map_err(|_| unlisted(s))?;
    parse_listing(s, (out.code, out.stdout))
}

/// Call 34: the server that answered is the recorded one: its endpoint answers the recorded pid and the process
/// still has the recorded start (the caller's own server through `server::same`). Else 1 `index`, nothing written.
fn answered(ctx: &Ctx, me: &Caller, s: &Server) -> Res<()> {
    let moved = || index::bad("the server restarted while its sessions were listed");
    if s.key == me.server_key {
        return super::server::same(ctx, me).map_err(|_| moved());
    }
    #[cfg(test)]
    let seam = ANSWERING
        .with(|m| m.borrow().get(&s.socket).copied())
        .unwrap_or(Some(s.pid));
    #[cfg(not(test))]
    let seam: Option<u32> = None;
    let pid = seam.or_else(|| {
        let out = on_other(ctx, s, &["display-message", "-p", "#{pid}"])
            .ok()
            .filter(|o| o.code == 0)?;
        out.stdout.trim().parse::<u32>().ok()
    });
    if pid != Some(s.pid) || index::prove(ctx, s)? != Proof::Live {
        return Err(moved());
    }
    Ok(())
}

/// Publishes `v`: the old view (if another) becomes an alias, the first publication anchors, and `pending` goes
/// with its release moved into `releases`, in the same index write (calls 25, 35).
pub(crate) fn publish(ix: &mut Index, v: View) {
    if let Some(old) = ix.view.take().filter(|o| *o != v) {
        ix.aliases.push(old);
    }
    if ix.anchor.is_none() {
        ix.anchor = Some(Anchor {
            key: v.server.key.clone(),
            session: v.session.clone(),
        });
    }
    ix.view = Some(v);
    if let Some(p) = ix.pending.take() {
        ix.releases.push(p.release);
    }
}

/// E5, calls 17, 25, 35: publishes every outstanding release, each counted only when it reads back `Present`; the
/// published ones leave `releases` in one index write. A torn one stays listed: 1 `index` naming it.
pub fn release_all(ctx: &Ctx, ix: &mut Index) -> Res<()> {
    let before = ix.releases.len();
    let mut kept = Vec::new();
    for r in std::mem::take(&mut ix.releases) {
        let at = std::path::Path::new(&r);
        let _ = crate::state::evidence::publish(at, &serde_json::json!({"released": true}));
        if !matches!(
            crate::state::evidence::read(at),
            crate::state::evidence::Evidence::Present(_)
        ) {
            kept.push(r);
        }
    }
    let torn = kept.first().cloned();
    ix.releases = kept;
    if ix.releases.len() != before {
        index::write(ctx, ix)?;
    }
    match torn {
        Some(t) => Err(index::bad(format!(
            "the release {t} cannot be read; repair or remove it"
        ))),
        None => Ok(()),
    }
}

/// E3.1, under `up|T`: one session named `pending.name` is published; none (or its server gone) drops `pending`
/// with its release moved into `releases` (call 35); several refuse `index`. Every listing is followed by the
/// answering-server proof (calls 16, 34) before anything changes. Gone aliases go. One index write if changed.
pub fn repair(ctx: &Ctx, me: &Caller, ix: &mut Index) -> Res<()> {
    let before = ix.clone();
    if let Some(p) = ix.pending.clone() {
        let found = match index::prove(ctx, &p.server)? {
            Proof::Gone => None,
            Proof::Live => {
                let all = sessions_on(ctx, me, &p.server)?;
                let named: Vec<&(String, String)> = all.iter().filter(|s| s.1 == p.name).collect();
                if named.len() > 1 {
                    return Err(index::bad(format!(
                        "several sessions are named {}; close all but one",
                        p.name
                    )));
                }
                answered(ctx, me, &p.server)?;
                named.first().map(|one| View {
                    server: p.server.clone(),
                    session: one.0.clone(),
                    name: one.1.clone(),
                })
            }
        };
        match found {
            Some(v) => publish(ix, v),
            None => {
                ix.releases.push(p.release);
                ix.pending = None;
            }
        }
    }
    let mut aliases = Vec::new();
    for a in std::mem::take(&mut ix.aliases) {
        if index::prove(ctx, &a.server)? != Proof::Gone {
            aliases.push(a);
        }
    }
    ix.aliases = aliases;
    if *ix != before {
        index::write(ctx, ix)?;
    }
    Ok(())
}

pub const UNPROVEN: &str = "the new session could not be proven; run up again";

/// What `ensure` and `start` leave: the view on the caller's server. Its hold release, if any, is in the index's
/// `releases`, published by E5 (calls 25, 35).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub view: View,
    /// The hold window `new-session` answered (`@N`); empty when the view was reused or found by a listing.
    pub window: String,
}

/// E3.3's `pending` (Task 9: also E6.1's): the session name from `inputs.folder` and T, a fresh
/// `<root>/up/<32 hex>.release`, the caller's server.
pub(crate) fn new_pending(ctx: &Ctx, me: &Caller, ix: &Index) -> Res<Pending> {
    let name = super::identity::session_name(std::path::Path::new(&ix.inputs.folder), &ix.team)?;
    let rel = ctx
        .state_root
        .join("up")
        .join(format!("{}.release", crate::util::hex32()?));
    Ok(Pending {
        server: index::server_of(me),
        name,
        release: crate::agents::spawn::utf8(&rel)?.to_string(),
    })
}

/// E3.1, then E3.2 (a proven live view on the caller's server whose session is listed, the answering server
/// re-proven after the listing: F4, call 34), else E3.3: a new `pending` written durably, then `start`.
pub fn ensure(ctx: &Ctx, me: &Caller, ix: &mut Index) -> Res<Published> {
    repair(ctx, me, ix)?;
    if let Some(v) = ix.view.clone()
        && v.server.key == me.server_key
        && index::prove(ctx, &v.server)? == Proof::Live
        && sessions_on(ctx, me, &v.server)?
            .iter()
            .any(|s| s.0 == v.session)
    {
        answered(ctx, me, &v.server)?;
        return Ok(Published {
            view: v,
            window: String::new(),
        });
    }
    let p = new_pending(ctx, me, ix)?;
    ix.pending = Some(p);
    index::write(ctx, ix)?;
    super::halt("pending")?;
    start(ctx, me, ix)
}

/// E3.3 for `ix.pending` (already durable): `new-session` running `hold <release>`; then the answering server is
/// proven the recorded one (calls 16, 34) before anything is published; a lost or unclear reply runs E3.1 at once
/// and ends published on this server or in 1 `index` (`pending` kept, or dropped with its release kept when the
/// listing proves no such session).
pub fn start(ctx: &Ctx, me: &Caller, ix: &mut Index) -> Res<Published> {
    let p = ix
        .pending
        .clone()
        .ok_or_else(|| Refusal::internal("no pending session"))?;
    // Calls 33, 39: the caller's own server is bound before any write can name it the anchor.
    if ix.anchor.is_none() {
        super::bind_anchor_server(ctx, me, &me.server_key)?;
    }
    let exe = crate::agents::spawn::utf8(&ctx.exe)?;
    let args = [
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{session_id} #{window_id}",
        "-s",
        &p.name,
        "-c",
        &ix.inputs.folder,
        exe,
        "hold",
        &p.release,
    ];
    let reply = crate::mux::native(ctx, &args, true);
    super::halt("session")?;
    let here = index::server_of(me);
    match reply
        .ok()
        .filter(|o| o.code == 0)
        .and_then(|o| super::ids(&o.stdout))
    {
        Some((session, window)) => {
            answered(ctx, me, &here)?;
            publish(
                ix,
                View {
                    server: here,
                    session,
                    name: p.name.clone(),
                },
            );
            index::write(ctx, ix)?;
            super::halt("published")?;
            Ok(Published {
                view: ix.view.clone().expect("published"),
                window,
            })
        }
        None => {
            repair(ctx, me, ix).map_err(|_| index::bad(UNPROVEN))?;
            match ix.view.clone() {
                Some(v)
                    if ix.pending.is_none()
                        && v.server.key == me.server_key
                        && v.name == p.name =>
                {
                    Ok(Published {
                        view: v,
                        window: String::new(),
                    })
                }
                _ => Err(index::bad(UNPROVEN)),
            }
        }
    }
}
