//! occupy-3 H2/H3: what each member's not-done generations claim: managed clients and their effective
//! sessions, and the reservations of generations no live client binds.
use super::*;

/// A live process bound to a not-done generation of member `id` (H2), keyed by the client.json (pid, start), and the
/// one conversation it holds, lowercased. `None`: its newest entry cannot be used (finding 8), so it holds every
/// conversation as `Evidence`, still named by pid, image and member.
pub(super) struct Managed {
    pub(super) id: String,
    pub(super) pid: u32,
    pub(super) start: i64,
    pub(super) session: Option<String>,
    /// Call 29: the client read Dead at the registry read, after the saved table listed it.
    pub(super) gone: bool,
}

/// H2, H3 from one read of every member (R1): managed clients, and each reserved conversation with the member that
/// reserves it (call 2 names it).
pub(super) struct Claims {
    pub(super) managed: Vec<Managed>,
    pub(super) reserved: BTreeMap<String, String>,
}

/// `Err` holds every conversation (R2): `Hold::member(Evidence, id)` for an unreadable member, status, orphan list or
/// spec; `Hold::machine(Evidence)` when the enumeration itself refuses (call 12).
pub(super) fn claims(ctx: &Ctx, rows: &[Row]) -> Result<Claims, Hold> {
    let mut c = Claims {
        managed: Vec::new(),
        reserved: BTreeMap::new(),
    };
    for (folder, member) in all_members(ctx).map_err(|_| Hold::machine(Rule::Evidence))? {
        let id = folder
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        claim(ctx, rows, &id, member, &mut c).map_err(|_| Hold::member(Rule::Evidence, &id))?;
    }
    Ok(c)
}

/// Call 12: every member of every `<key>/agents` under the state root, each level below the root checked by
/// `gens::real_dir` (the walk's own check; never follows a link). The state root itself is not walked (as
/// `gens::walk`): absent means no members. A plain file at the key level is no server folder and is skipped; a key
/// with no `agents` has no members. A link at a key or at `agents`, a file at `agents`, or anything unreadable
/// refuses `evidence`.
pub(crate) fn all_members(ctx: &Ctx) -> Res<Vec<(PathBuf, Member)>> {
    let keys = match std::fs::read_dir(&ctx.state_root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(evidence::unreadable()),
    };
    let mut all = Vec::new();
    for entry in keys {
        let key = entry.map_err(|_| evidence::unreadable())?.path();
        if std::fs::symlink_metadata(&key).is_ok_and(|m| m.is_file()) {
            continue;
        }
        if !gens::real_dir(&key)? || !gens::real_dir(&key.join("agents"))? {
            continue;
        }
        let members = gens::members(&key.join("agents"))?;
        all.extend(members.into_iter().map(|(_, folder, m)| (folder, m)));
    }
    Ok(all)
}

/// Which generation of a record: the session fallback when its spec is absent differs (call 13).
#[derive(Clone, Copy)]
pub(super) enum Role {
    Pending,
    Active,
    Orphan,
}

/// One member's claims, added to `c`. Every not-done generation counts: pending, active and group B's unresolved
/// orphans (call 11). Each reserves only its own session (call 13), or binds its listed proven client (H2).
pub(super) fn claim(ctx: &Ctx, rows: &[Row], id: &str, member: Member, c: &mut Claims) -> Res<()> {
    let g = match member {
        Member::Absent | Member::Pre(_) => return Ok(()), // prelife-1c G10: no record reservation
        Member::Unreadable(e) => return Err(e),
        Member::Life(g) => g,
    };
    let status = g.status()?;
    let mut open = Vec::new();
    if let Some(n) = g.pending().filter(|_| status.pending_done != Some(true)) {
        open.push((n, Role::Pending));
    }
    if let Some(n) = g.active().filter(|_| status.active_done != Some(true)) {
        open.push((n, Role::Active));
    }
    open.extend(g.orphans()?.into_iter().map(|n| (n, Role::Orphan)));
    for (n, role) in open {
        let Some(own) = own_session(&g, n, role)? else {
            continue; // call 14: a spec-less orphan never had a client
        };
        match bound(ctx, &g, n, rows) {
            Some((pid, start)) => {
                let (session, gone) = effective(ctx, &g, n, pid, start, own);
                c.managed.push(Managed {
                    id: id.to_string(),
                    pid,
                    start,
                    session,
                    gone,
                });
            }
            None => {
                c.reserved.insert(own, id.to_string());
            }
        }
    }
    Ok(())
}

/// R4, call 13: generation n's own conversation: its `spec.json` `sessionId`; only when that file is Absent, the
/// record's `sessionId` (active) or `pending.sessionId` (pending). Call 14: an orphan with no `spec.json` has no
/// session and reserves nothing (`None`): `forward` publishes the spec before `hc run`, so it never had a client.
/// Unreadable (a torn spec included), or no valid id: `evidence`. Lowercased.
pub(super) fn own_session(g: &gens::Gen, n: u64, role: Role) -> Res<Option<String>> {
    let record = g.record();
    let id = match (g.read(n, "spec").known()?, role) {
        (Some(spec), _) => crate::agents::text(spec.get("sessionId")),
        (None, Role::Pending) => {
            crate::agents::text(record.get("pending").and_then(|p| p.get("sessionId")))
        }
        (None, Role::Active) => crate::agents::text(record.get("sessionId")),
        (None, Role::Orphan) => return Ok(None),
    };
    crate::checks::session_id(&id)
        .then(|| Some(id.to_ascii_lowercase()))
        .ok_or_else(evidence::unreadable)
}

/// H2: generation n's client, proven by K1a, listed in this table read with that pid and start.
pub(super) fn bound(ctx: &Ctx, g: &gens::Gen, n: u64, rows: &[Row]) -> Option<(u32, i64)> {
    let (pid, start) = gens::client(ctx, g.folder(), n)?.client;
    rows.iter()
        .any(|r| r.pid == pid && r.start.is_some_and(|s| ctx.os.same_start(s, start)))
        .then_some((pid, start))
}

/// H2, calls 9, 16 and 2: the newest report of this very process (keyed by pid and start; `live::entry` drops one of
/// another boot) counts when its `at` is not earlier than generation n's launch evidence time (`launcher.json`
/// `startedAt`); an earlier one (another boot: Linux start ticks repeat) is ignored and the generation's own session
/// holds. `None` (unknown: holds every conversation, the holder still named): a torn entry or one whose boot cannot
/// be verified, one without a valid `sessionId` or `at`, a kind the record does not name, or no launch time to
/// compare. A clock that moved back only ignores an entry (fail closed).
pub(super) fn effective(
    ctx: &Ctx,
    g: &gens::Gen,
    n: u64,
    pid: u32,
    start: i64,
    own: String,
) -> (Option<String>, bool) {
    let Some(kind) =
        crate::agents::spawn::client_named(&crate::agents::text(g.record().get("client")))
    else {
        return (None, false);
    };
    match registry(ctx, kind, pid, start) {
        Ok(found) => (session_of(g, n, found.entry, found.gone, own), found.gone),
        Err(_) => (None, false),
    }
}

/// `effective`'s session from the entry read. Call 29: a client gone since the saved table falls back to nothing
/// (unknown, holds), never to its generation's session, when it left no entry or an ignored one.
fn session_of(
    g: &gens::Gen,
    n: u64,
    entry: Option<serde_json::Value>,
    gone: bool,
    own: String,
) -> Option<String> {
    let Some(entry) = entry else {
        return (!gone).then_some(own);
    };
    let at = crate::util::parse_utc(&crate::agents::text(entry.get("at")))?;
    #[cfg(test)]
    hooks::fire_before_launch_reread();
    // Call 16: the second read of a create-once `launcher.json` (`gens::client` read it once). It can only answer
    // what the first read saw, or Absent/Unreadable, which hold (None).
    let launched = g.read(n, "launcher").known().ok()??;
    let launched = crate::util::parse_utc(&crate::agents::text(launched.get("startedAt")))?;
    if at < launched {
        return (!gone).then_some(own);
    }
    let id = crate::agents::text(entry.get("sessionId"));
    crate::checks::session_id(&id).then(|| id.to_ascii_lowercase())
}

/// The managed client a row is: its own (pid, start), or on Unix the native child that `pair_parent` pairs to a
/// managed loader, the parent taken from this table read only (calls 8, 17).
pub(super) fn managed_of<'a>(
    ctx: &Ctx,
    c: &'a Claims,
    row: &Row,
    rows: &[Row],
    os: OsKind,
) -> Option<&'a Managed> {
    let own = |r: &Row| {
        c.managed
            .iter()
            .find(|m| m.pid == r.pid && r.start.is_some_and(|s| ctx.os.same_start(s, m.start)))
    };
    own(row).or_else(|| {
        let parent = rows.iter().find(|r| r.pid == row.ppid && row.ppid != 0)?;
        let loader = crate::agents::ancestry::pair_parent(ctx, parent, row, os)
            .filter(|l| l.pid != row.pid)?;
        own(&loader)
    })
}
