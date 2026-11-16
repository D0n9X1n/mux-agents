//! occupy-3: which live processes may hold a conversation (H1-H6). Fail closed: anything unreadable holds.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::agents::gens::{self, Member};
use crate::agents::tree::Standing;
use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::error::Res;
use crate::parse::Client;
use crate::proc::Row;
use crate::state::root::{HOST, OsKind};
use crate::state::{evidence, live};

/// The rule that held (PM call 2): every refusal names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// H4.1: the process table cannot be read.
    Table,
    /// Calls 10, 20: /proc may hide other users' processes (`hidepid`, `subset=pid`, another mount at /proc).
    Hidden,
    /// R2: a member's evidence (record, status, spec, registry entry) cannot be read.
    Evidence,
    /// H4.2: a managed client whose effective session is this one.
    Managed,
    /// H4.3: a command line naming the session.
    Argv,
    /// PM call 1: the caller's own client, whose newest registry entry names the session.
    Caller,
    /// H4.4: an unmanaged client of the kind.
    Unmanaged,
    /// H4.5: a process that cannot be read and may be a client.
    Unbound,
    /// H4.6: a not-done generation without a bound client reserves the session.
    Reserved,
}

impl Rule {
    pub fn phrase(self) -> &'static str {
        match self {
            Rule::Table => "the process table cannot be read",
            Rule::Hidden => {
                "/proc hides other users' processes or may (hidepid, subset=pid, or another mount at /proc)"
            }
            Rule::Evidence => "its evidence cannot be read",
            Rule::Managed => "its member's client last reported this conversation",
            Rule::Argv => "its command line names this conversation",
            Rule::Caller => "your own client last reported this conversation",
            Rule::Unmanaged => "it is a client this team did not start",
            Rule::Unbound => "it cannot be read and may be a client",
            Rule::Reserved => "a launch of it that has not ended reserves this conversation",
        }
    }
}

/// One refusal: the rule, and what holds: a process (pid and image), or a member (id), or neither (this machine).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
    pub rule: Rule,
    pub pid: Option<u32>,
    pub image: Option<String>,
    pub member: Option<String>,
}

impl Hold {
    pub fn of(rule: Rule, row: &Row) -> Hold {
        Hold {
            rule,
            pid: Some(row.pid),
            image: row.image.clone(),
            member: None,
        }
    }

    pub fn member(rule: Rule, id: &str) -> Hold {
        Hold {
            rule,
            pid: None,
            image: None,
            member: Some(id.to_string()),
        }
    }

    pub fn machine(rule: Rule) -> Hold {
        Hold {
            rule,
            pid: None,
            image: None,
            member: None,
        }
    }

    /// `process <pid> (<image>|image unreadable)`, `member <id>` or `this machine`, the rule, and the retry hint.
    pub fn message(&self) -> String {
        let who = match (self.pid, &self.member) {
            (Some(pid), _) => format!(
                "process {pid} ({})",
                self.image.as_deref().unwrap_or("image unreadable")
            ),
            (None, Some(id)) => format!("member {id}"),
            (None, None) => "this machine".to_string(),
        };
        format!(
            "{who} may hold that conversation: {}; if you just stopped it, retry in a few seconds",
            self.rule.phrase()
        )
    }
}

/// Calls 1, 7: the client caller proof established (`own_of`, Task 4b), exempt from H4.4 and H4.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Own {
    pub kind: Client,
    pub pid: u32,
    pub start: i64,
}

/// Call 7: from the proof only. A member standing (an hc member, or a candidate re-proven under the lock) gives its
/// `tree::Member` client (pid, start); with no standing, a pane caller's proven agent; anything else nothing.
/// `caller.chain` is never read.
pub fn own_of(caller: &Caller, standing: Option<&Standing>) -> Option<Own> {
    match standing {
        Some(Standing::Member(m)) => Some(Own {
            kind: m.client,
            pid: m.pid,
            start: m.start,
        }),
        Some(Standing::Person) => None,
        None => {
            let agent = caller.agent.as_ref()?;
            Some(Own {
                kind: caller.client?,
                pid: agent.pid,
                start: agent.start,
            })
        }
    }
}

pub fn held(ctx: &Ctx, session: &str, kind: Client, own: Option<&Own>) -> Option<Hold> {
    held_on(ctx, session, kind, HOST, own)
}

/// H4.1, H4.3-5 (Task 4 adds H4.2 and H4.6, Task 4b the caller): /proc's mounts first, then one table read; the
/// first row that may hold `session` answers.
pub fn held_on(
    ctx: &Ctx,
    session: &str,
    kind: Client,
    os: OsKind,
    own: Option<&Own>,
) -> Option<Hold> {
    if let Some(hold) = proc_hold(ctx) {
        return Some(hold); // calls 10, 20
    }
    let Ok(rows) = ctx.os.table() else {
        return Some(Hold::machine(Rule::Table)); // H4.1
    };
    let c = match claims(ctx, &rows) {
        Ok(c) => c,
        Err(hold) => return Some(hold), // R2: names the unreadable member
    };
    let wanted = session.to_ascii_lowercase();
    let exempted = own.and_then(|o| exempt(ctx, o, &rows, os));
    // A managed caller is judged per row (H4.2); an unmanaged one by its own newest report, read once here (call 17).
    let unmanaged = exempted
        .as_ref()
        .filter(|e| managed_of(ctx, &c, &e.proven, &rows, os).is_none());
    if let (Some(o), Some(e)) = (own, unmanaged) {
        let rule = match e.key {
            None => Some(Rule::Evidence),
            Some(key) => own_report(ctx, o.kind, key, &wanted),
        };
        if let Some(rule) = rule {
            return Some(Hold::of(rule, &e.proven));
        }
    }
    for row in &rows {
        if let Some(m) = managed_of(ctx, &c, row, &rows, os) {
            let rule = match m.session.as_deref() {
                None => Rule::Evidence,                  // finding 8: the known holder, named
                Some(s) if s == wanted => Rule::Managed, // H4.2; its argv adds nothing
                Some(_) => continue,
            };
            return Some(Hold {
                member: Some(m.id.clone()),
                ..Hold::of(rule, row)
            });
        }
        if exempted.as_ref().is_some_and(|e| e.covers(ctx, row)) {
            // Calls 1, 7: exempt from H4.4 and H4.5; still held by its argv (H4.3). Its report was read above.
            if row
                .argv
                .as_deref()
                .is_some_and(|argv| occupies(argv, session))
            {
                return Some(Hold::of(Rule::Argv, row));
            }
            continue;
        }
        if let Some(hold) = holds(ctx.os.as_ref(), row, session, kind, os) {
            return Some(hold); // H4.3-5
        }
    }
    c.reserved
        .get(&wanted)
        .map(|id| Hold::member(Rule::Reserved, id)) // H4.6
}

/// A live process bound to a not-done generation of member `id` (H2), keyed by the client.json (pid, start), and the
/// one conversation it holds, lowercased. `None`: its newest entry cannot be used (finding 8), so it holds every
/// conversation as `Evidence`, still named by pid, image and member.
struct Managed {
    id: String,
    pid: u32,
    start: i64,
    session: Option<String>,
}

/// H2, H3 from one read of every member (R1): managed clients, and each reserved conversation with the member that
/// reserves it (call 2 names it).
struct Claims {
    managed: Vec<Managed>,
    reserved: BTreeMap<String, String>,
}

/// `Err` holds every conversation (R2): `Hold::member(Evidence, id)` for an unreadable member, status, orphan list or
/// spec; `Hold::machine(Evidence)` when the enumeration itself refuses (call 12).
fn claims(ctx: &Ctx, rows: &[Row]) -> Result<Claims, Hold> {
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
enum Role {
    Pending,
    Active,
    Orphan,
}

/// One member's claims, added to `c`. Every not-done generation counts: pending, active and group B's unresolved
/// orphans (call 11). Each reserves only its own session (call 13), or binds its listed proven client (H2).
fn claim(ctx: &Ctx, rows: &[Row], id: &str, member: Member, c: &mut Claims) -> Res<()> {
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
                let session = effective(ctx, &g, n, pid, start, own);
                c.managed.push(Managed {
                    id: id.to_string(),
                    pid,
                    start,
                    session,
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
fn own_session(g: &gens::Gen, n: u64, role: Role) -> Res<Option<String>> {
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
fn bound(ctx: &Ctx, g: &gens::Gen, n: u64, rows: &[Row]) -> Option<(u32, i64)> {
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
fn effective(
    ctx: &Ctx,
    g: &gens::Gen,
    n: u64,
    pid: u32,
    start: i64,
    own: String,
) -> Option<String> {
    let kind = crate::agents::spawn::client_named(&crate::agents::text(g.record().get("client")))?;
    let entry = match live::entry(ctx, kind, pid, start) {
        Ok(None) => return Some(own),
        Ok(Some(entry)) => entry,
        Err(_) => return None,
    };
    let at = crate::util::parse_utc(&crate::agents::text(entry.get("at")))?;
    #[cfg(test)]
    hooks::fire_before_launch_reread();
    // Call 16: the second read of a create-once `launcher.json` (`gens::client` read it once). It can only answer
    // what the first read saw, or Absent/Unreadable, which hold (None).
    let launched = g.read(n, "launcher").known().ok()??;
    let launched = crate::util::parse_utc(&crate::agents::text(launched.get("startedAt")))?;
    if at < launched {
        return Some(own);
    }
    let id = crate::agents::text(entry.get("sessionId"));
    crate::checks::session_id(&id).then(|| id.to_ascii_lowercase())
}

/// The managed client a row is: its own (pid, start), or on Unix the native child that `pair_parent` pairs to a
/// managed loader, the parent taken from this table read only (calls 8, 17).
fn managed_of<'a>(
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

/// The one exemption match (call 7): this pid, listed with a start the OS calls the same.
fn same(ctx: &Ctx, row: &Row, pid: u32, start: i64) -> bool {
    row.pid == pid && row.start.is_some_and(|s| ctx.os.same_start(s, start))
}

/// The exempt rows, each (pid, start); the (pid, start) keying the caller's own entry, `None` when a proven Unix native
/// `copilot`'s loader cannot be resolved from this table read (call 17: its report cannot be read, so `Evidence`);
/// and the proven row, which every caller hold names (call 2).
struct Exempt {
    rows: Vec<(u32, i64)>,
    key: Option<(u32, i64)>,
    proven: Row,
}

impl Exempt {
    fn covers(&self, ctx: &Ctx, row: &Row) -> bool {
        self.rows
            .iter()
            .any(|&(pid, start)| same(ctx, row, pid, start))
    }
}

/// The proven row, plus its Copilot partner by report-2b's rule (`ancestry::pair_parent`, call 8), every parent taken
/// from this table read only (call 17, never a fresh `Os::row`): its loader (a parent no newer than it), or a native
/// child whose loader it is (reverse pairing, call 19). The loader keys the entry. Not listed: nothing exempt.
fn exempt(ctx: &Ctx, own: &Own, rows: &[Row], os: OsKind) -> Option<Exempt> {
    let proven = rows
        .iter()
        .find(|r| same(ctx, r, own.pid, own.start))?
        .clone();
    let start = proven.start?;
    let parent = |r: &Row| rows.iter().find(|p| p.pid == r.ppid && r.ppid != 0);
    let paired =
        |r: &Row| parent(r).and_then(|p| crate::agents::ancestry::pair_parent(ctx, p, r, os));
    let native = own.kind == Client::Copilot
        && os == OsKind::Unix
        && proven.path.as_deref().and_then(|p| p.rsplit('/').next()) == Some("copilot");
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
fn own_report(ctx: &Ctx, kind: Client, key: (u32, i64), wanted: &str) -> Option<Rule> {
    match live::entry(ctx, kind, key.0, key.1) {
        Err(_) => Some(Rule::Evidence),
        Ok(None) => None,
        Ok(Some(entry)) => {
            let id = crate::agents::text(entry.get("sessionId"));
            if !crate::checks::session_id(&id) {
                return Some(Rule::Evidence);
            }
            id.eq_ignore_ascii_case(wanted).then_some(Rule::Caller)
        }
    }
}

/// One unmanaged row: H4.3 argv, H4.4 a client of `kind`, H4.5 unbound unless its image names no client of `kind`.
pub fn holds(
    os_calls: &dyn crate::proc::Os,
    row: &Row,
    session: &str,
    kind: Client,
    os: OsKind,
) -> Option<Hold> {
    if row
        .argv
        .as_deref()
        .is_some_and(|argv| occupies(argv, session))
    {
        return Some(Hold::of(Rule::Argv, row));
    }
    if os_calls.client_of(row) == Some(kind) {
        return Some(Hold::of(Rule::Unmanaged, row));
    }
    let unbound = row.start.is_none() || row.path.is_none() || row.argv.is_none();
    let cleared = row
        .image
        .as_deref()
        .is_some_and(|image| !client_image(image, kind, os));
    (unbound && !cleared).then(|| Hold::of(Rule::Unbound, row))
}

/// Whether an image name may be a client of `kind` (ambiguity 3): Unix exact and case-sensitive (`claude`; `copilot`
/// or `node`); Windows by `parse::windows_client` (any case, `.exe.old*` kept), plus `node.exe` for Copilot.
pub fn client_image(image: &str, kind: Client, os: OsKind) -> bool {
    match os {
        OsKind::Unix => match kind {
            Client::Claude => image == "claude",
            Client::Copilot => image == "copilot" || image == "node",
        },
        OsKind::Windows => {
            crate::parse::windows_client(image) == Some(kind)
                || (kind == Client::Copilot && image.eq_ignore_ascii_case("node.exe"))
        }
    }
}

/// Calls 10, 20: what /proc's mounts let occupancy see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcView {
    Open,
    Hidden,
    Unreadable,
}

/// Calls 10, 20: fail closed on any doubt. Every mountinfo line whose mount point (field 5) is `/proc` counts: more
/// than one (stacked, in any order), one that is not `proc` (a tmpfs over it), one that cannot be split, or one with
/// `hidepid` other than `0`/`off` or `subset=pid` in its mount options (field 6) or its super options (after ` - `:
/// fstype, source, super options) is Hidden. No `/proc` line is Unreadable. One plain `proc` at /proc is Open.
pub fn proc_view(mountinfo: &str) -> ProcView {
    let mut at_proc = 0;
    let mut doubt = false;
    for line in mountinfo.lines() {
        let mut fields = line.split(' ');
        if fields.nth(4) != Some("/proc") {
            continue;
        }
        at_proc += 1;
        let mount_opts = fields.next().unwrap_or("");
        let Some((_, tail)) = line.split_once(" - ") else {
            doubt = true;
            continue;
        };
        let mut tail = tail.split(' ');
        let (fstype, super_opts) = (tail.next(), tail.nth(1).unwrap_or(""));
        let hides = mount_opts.split(',').chain(super_opts.split(',')).any(|o| {
            o == "subset=pid"
                || o.strip_prefix("hidepid=")
                    .is_some_and(|v| v != "0" && v != "off")
        });
        doubt |= fstype != Some("proc") || hides;
    }
    match at_proc {
        0 => ProcView::Unreadable,
        1 if !doubt => ProcView::Open,
        _ => ProcView::Hidden,
    }
}

/// Calls 10, 20, before any table read: a doubtful /proc holds as `Hidden`; mountinfo that cannot be read, or that
/// names no /proc, as `Table`. No seam (not Linux): nothing.
fn proc_hold(ctx: &Ctx) -> Option<Hold> {
    match ctx.os.proc_mounts()? {
        Err(_) => Some(Hold::machine(Rule::Table)),
        Ok(text) => match proc_view(&text) {
            ProcView::Open => None,
            ProcView::Hidden => Some(Hold::machine(Rule::Hidden)),
            ProcView::Unreadable => Some(Hold::machine(Rule::Table)),
        },
    }
}

/// Whether a process with `argv` runs a client on `session`: the uuid follows `--session-id` (or
/// `--session-id=`), or `--resume`, `-r` (or `--resume=`) in a command line without `--fork-session`. A Claude fork
/// (`--resume <source> --fork-session --session-id <new>`) occupies only the new conversation; a Copilot fork resumes
/// its own copy under the new id, so its `--resume` already names only the new one. Words after `--` are no flags.
pub(crate) fn occupies(argv: &[String], session: &str) -> bool {
    let flags: Vec<&str> = argv
        .iter()
        .map(String::as_str)
        .take_while(|a| *a != "--")
        .collect();
    let fork = flags.contains(&"--fork-session");
    flags.iter().enumerate().any(|(i, a)| {
        let (name, inline) = match a.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (*a, None),
        };
        let resume = name == "--resume" || (name == "-r" && inline.is_none());
        if !(name == "--session-id" || (resume && !fork)) {
            return false;
        }
        inline.or_else(|| flags.get(i + 1).copied()) == Some(session)
    })
}

/// Checklist 7 (call 16) test seam: runs in `effective` right before its second `launcher.json` read.
#[cfg(test)]
pub(crate) mod hooks {
    use std::cell::RefCell;
    type Hook = Box<dyn Fn()>;
    thread_local! {
        static BEFORE: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }
    pub fn before_launch_reread(hook: Option<Hook>) {
        BEFORE.with(|b| *b.borrow_mut() = hook);
    }
    pub(super) fn fire_before_launch_reread() {
        BEFORE.with(|b| {
            if let Some(hook) = b.borrow().as_ref() {
                hook()
            }
        });
    }
}

#[cfg(test)]
#[path = "occupancy_tests.rs"]
mod tests;
