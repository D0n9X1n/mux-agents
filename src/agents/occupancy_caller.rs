//! occupy-3 calls 1, 7, 8, 17, 19: the caller's own proven client, exempt from H4.4/H4.5, holding by its argv or
//! its own newest report.
use super::*;

/// The one exemption match (call 7): this pid, listed with a start the OS calls the same.
pub(super) fn same(ctx: &Ctx, row: &Row, pid: u32, start: i64) -> bool {
    row.pid == pid && row.start.is_some_and(|s| ctx.os.same_start(s, start))
}

/// The exempt rows, each (pid, start); the (pid, start) keying the caller's own entry, `None` when a proven Unix native
/// `copilot`'s loader cannot be resolved from this table read (call 17: its report cannot be read, so `Evidence`);
/// and the proven row, which every caller hold names (call 2).
pub(super) struct Exempt {
    pub(super) rows: Vec<(u32, i64)>,
    pub(super) key: Option<(u32, i64)>,
    pub(super) proven: Row,
}

impl Exempt {
    pub(super) fn covers(&self, ctx: &Ctx, row: &Row) -> bool {
        self.rows
            .iter()
            .any(|&(pid, start)| same(ctx, row, pid, start))
    }
}

/// The proven row, plus its Copilot partner by report-2b's rule (`ancestry::pair_parent`, call 8), every parent taken
/// from this table read only (call 17, never a fresh `Os::row`): its loader (a parent no newer than it), or a native
/// child whose loader it is (reverse pairing, call 19). The loader keys the entry. Not listed: nothing exempt.
pub(super) fn exempt(ctx: &Ctx, own: &Own, rows: &[Row], os: OsKind) -> Option<Exempt> {
    let proven = rows
        .iter()
        .find(|r| same(ctx, r, own.pid, own.start))?
        .clone();
    let start = proven.start?;
    let parent = |r: &Row| rows.iter().find(|p| p.pid == r.ppid && r.ppid != 0);
    let paired =
        |r: &Row| parent(r).and_then(|p| crate::agents::ancestry::pair_parent(ctx, p, r, os));
    // QA Q1 (call 31): an unreadable path may be the native `copilot`, whose loader keys the report.
    let native = own.kind == Client::Copilot
        && os == OsKind::Unix
        && proven
            .path
            .as_deref()
            .is_none_or(|p| p.rsplit('/').next() == Some("copilot"));
    let mut e = Exempt {
        rows: vec![(proven.pid, start)],
        key: Some((proven.pid, start)),
        proven: proven.clone(),
    };
    match paired(&proven).map(|l| (l.pid, l.start)) {
        Some((pid, Some(s))) if pid != proven.pid => {
            e.rows.push((pid, s));
            e.key = Some((pid, s));
        }
        Some((pid, None)) if pid != proven.pid => e.key = None,
        Some(_) => {} // a parent named otherwise: the native is its own identity
        None if native => e.key = None, // call 17: missing, startless or unrecognized saved parent
        None => {}
    }
    // o3 call 34 N1: the report key is the one the proof established, never re-derived from a parent that may have
    // replaced an exited loader. A possibly-native caller whose proof established none cannot be keyed: Evidence.
    e.key = match own.key {
        Some(key) => Some(key),
        None if native => None,
        None => e.key,
    };
    for r in rows.iter().filter(|r| r.ppid == proven.pid) {
        let joins = paired(r).is_some_and(|l| l.pid != r.pid && same(ctx, &l, proven.pid, start));
        if let (true, Some(s)) = (joins, r.start) {
            e.rows.push((r.pid, s));
        }
    }
    Some(e)
}

/// The caller's own newest entry: `Caller` when it names `wanted`; `Evidence` when it cannot be read or names no valid
/// conversation (R2); none when absent or naming another.
pub(super) fn own_report(ctx: &Ctx, kind: Client, key: (u32, i64), wanted: &str) -> Option<Rule> {
    match registry(ctx, kind, key.0, key.1) {
        Err(_) => Some(Rule::Evidence),
        // Call 29: the key's process gone since the saved table, with no entry left: unknown, so it holds.
        Ok(live::Found { entry: None, gone }) => gone.then_some(Rule::Evidence),
        Ok(live::Found {
            entry: Some(entry), ..
        }) => {
            let id = crate::agents::text(entry.get("sessionId"));
            if !crate::checks::session_id(&id) {
                return Some(Rule::Evidence);
            }
            id.eq_ignore_ascii_case(wanted).then_some(Rule::Caller)
        }
    }
}
