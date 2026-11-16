//! The one reader of generation evidence (#103 state-0 S5-S7): `gens/<n>.claim` and `gens/<n>/<file>.json`.
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::error::{Refusal, Res};
use crate::state::evidence::{self, Evidence, Published};

/// Every create-once file of a generation (S5); the claim lives beside the folder.
pub const FILES: [&str; 15] = [
    "spec",
    "launch",
    "commit",
    "launcher",
    "exit",
    "client",
    "report",
    "ready",
    "failure",
    "outcome",
    "ended",
    "terminating",
    "release",
    "released",
    "notice",
];

/// `gens/<n>.claim`.
pub fn claim_path(member: &Path, n: u64) -> PathBuf {
    member.join("gens").join(format!("{n}.claim"))
}

/// `gens/<n>/`.
pub fn dir(member: &Path, n: u64) -> PathBuf {
    member.join("gens").join(n.to_string())
}

/// `gens/<n>/<file>.json`; `file` is one of `FILES`.
pub fn path(member: &Path, n: u64, file: &str) -> PathBuf {
    debug_assert!(FILES.contains(&file), "{file}");
    dir(member, n).join(format!("{file}.json"))
}

/// Point 8: the one walk. `member` is `<state_root>/<key>/agents/<id>`, so the walk starts at the state root, three
/// folders up, and checks `<key>`, `agents`, `<id>`, then `names`. Every name but the last must be a real folder
/// (`symlink_metadata`, never following a link): missing is `Absent`; a link, a file or anything else is `Unreadable`.
/// The last name goes to `evidence::read`. The state root and anything above it are not walked.
pub fn walk(member: &Path, names: &[&str]) -> Evidence {
    let Some(root) = member.ancestors().nth(3) else {
        return Evidence::Unreadable;
    };
    let mut all: Vec<&std::ffi::OsStr> =
        member.strip_prefix(root).unwrap_or(member).iter().collect();
    all.extend(names.iter().map(std::ffi::OsStr::new));
    let Some((last, folders)) = all.split_last() else {
        return Evidence::Unreadable;
    };
    let mut at = root.to_path_buf();
    for name in folders {
        at.push(name);
        match std::fs::symlink_metadata(&at) {
            Ok(m) if m.is_dir() => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Evidence::Absent,
            _ => return Evidence::Unreadable,
        }
    }
    evidence::read(&at.join(last))
}

/// The member record, `<member>/record.json`, through the walk.
pub fn record(member: &Path) -> Evidence {
    walk(member, &["record.json"])
}

pub fn read(member: &Path, n: u64, file: &str) -> Evidence {
    debug_assert!(FILES.contains(&file), "{file}");
    walk(member, &["gens", &n.to_string(), &format!("{file}.json")])
}

pub fn claim(member: &Path, n: u64) -> Evidence {
    walk(member, &["gens", &format!("{n}.claim")])
}

/// R-W3: a write or the content flush before the link failed; the name was never made.
pub const NOT_PUBLISHED: &str = "generation evidence could not be published";
/// R-W3: a folder flush failed after the read or the link; the name may be visible but is not durable.
pub const NOT_FLUSHED: &str = "generation evidence could not be flushed";

fn state_refusal(message: &str) -> Refusal {
    Refusal {
        code: 1,
        check: "state",
        message: message.into(),
        reason: None,
        leftover: None,
        details: Default::default(),
    }
}

/// Call 18a, the one publisher of `gens/<n>/<file>.json` (R-W1).
pub fn publish_once(member: &Path, n: u64, file: &str, body: &Value) -> Res<Published> {
    debug_assert!(FILES.contains(&file), "{file}");
    let level = n.to_string();
    publish_at(member, &["gens", &level], &format!("{file}.json"), body)
}

/// Call 18a, the one publisher of `gens/<n>.claim`: as `publish_once`, one level up (a claim sits beside `gens/<n>/`).
pub fn publish_claim_once(member: &Path, n: u64, body: &Value) -> Res<Published> {
    publish_at(member, &["gens"], &format!("{n}.claim"), body)
}

/// R-W1. `member` is `<root>/<key>/agents/<id>`. Walks from the state root: `<key>` and `agents` must be real folders;
/// `<id>` and each of `levels` are made when missing, one level at a time, then must be real folders
/// (`symlink_metadata` never follows a link; a link, a junction or a file is `evidence`). A creation error other than
/// "already exists" is `evidence` too (call 27). Then one read of `name`: Present is `Exists` before anything is
/// written, Unreadable refuses. Absent: `claim_json` links a body whose content was flushed first (a racer that links
/// first makes this `Exists`). Then, on every call, the folder holding `name` and each folder above it up to
/// `agents` is flushed; a failed flush is a failed publication.
fn publish_at(member: &Path, levels: &[&str], name: &str, body: &Value) -> Res<Published> {
    let Some(root) = member.ancestors().nth(3) else {
        return Err(evidence::unreadable());
    };
    let names = member
        .strip_prefix(root)
        .map_err(|_| evidence::unreadable())?;
    let all: Vec<&std::ffi::OsStr> = names
        .iter()
        .chain(levels.iter().map(std::ffi::OsStr::new))
        .collect();
    let mut chain: Vec<PathBuf> = Vec::new();
    let mut at = root.to_path_buf();
    for (i, part) in all.iter().enumerate() {
        at.push(part);
        // <key> (0) and agents (1) are never made here; <id> (2) and below are.
        if i >= 2 {
            match crate::state::fs::create_dir(&at) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(evidence::unreadable()),
            }
        }
        if !std::fs::symlink_metadata(&at).is_ok_and(|m| m.is_dir()) {
            return Err(evidence::unreadable());
        }
        chain.push(at.clone());
    }
    let target = at.join(name);
    let found = match evidence::read(&target) {
        Evidence::Present(_) => Published::Exists,
        Evidence::Unreadable => return Err(evidence::unreadable()),
        Evidence::Absent => {
            #[cfg(test)]
            publish_hooks::fire(&target);
            match crate::state::fs::claim_json(&target, body) {
                Ok(true) => Published::Published,
                Ok(false) => Published::Exists,
                Err(_) => return Err(state_refusal(NOT_PUBLISHED)),
            }
        }
    };
    // chain: <key>, agents, <id>, levels...; flushed innermost first, down to agents (index 1). <key> holds no entry
    // this call can have made.
    for folder in chain[1..].iter().rev() {
        if crate::state::fs::sync_dir(folder).is_err() {
            return Err(state_refusal(NOT_FLUSHED));
        }
    }
    Ok(found)
}

/// Test hook: runs between the read and the link, so a racer can link first.
#[cfg(test)]
pub(crate) mod publish_hooks {
    use std::cell::RefCell;
    use std::path::Path;
    pub(crate) type Hook = Box<dyn Fn(&Path)>;
    thread_local! {
        static BEFORE_LINK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }
    pub(crate) fn before_link(hook: Option<Hook>) {
        BEFORE_LINK.with(|h| *h.borrow_mut() = hook);
    }
    pub(super) fn fire(target: &Path) {
        BEFORE_LINK.with(|h| {
            if let Some(f) = h.borrow().as_ref() {
                f(target);
            }
        });
    }
}

/// The notice delivery id of generation `n` of member `id`.
pub fn delivery_id(id: &str, n: u64) -> String {
    format!("exit-{id}-{n}")
}

/// Redesign 3: an id is 12 lowercase hex (`agents::agent_id`).
pub fn plain_id(id: &str) -> bool {
    crate::agents::agent_id(id)
}

/// A server key is 16 lowercase hex (`caller.rs`: the first 16 of a sha256).
pub fn plain_key(key: &str) -> bool {
    key.len() == 16
        && key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A generation is canonical decimal: digits only, no leading zero but "0" itself, fits u64.
pub fn plain_gen(n: &str) -> Option<u64> {
    let digits = !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit());
    (digits && (n == "0" || !n.starts_with('0')))
        .then(|| n.parse().ok())
        .flatten()
}

/// A record made by `tx.create` (`lifecycle: 1`); any other is pre-lifecycle (prelife-1c).
pub fn lifecycle(record: &Value) -> bool {
    record.get("lifecycle").and_then(Value::as_u64) == Some(1)
}

/// The active generation a lifecycle record names.
pub fn active(record: &Value) -> Option<u64> {
    record.get("generation")?.as_str()?.parse().ok()
}

/// The generation `record.pending` names.
pub fn pending(record: &Value) -> Option<u64> {
    record.get("pending")?.get("gen")?.as_u64()
}

/// What `record.json` says a member folder holds (S6, S7; R5). Every reader matches on it once.
#[derive(Debug, Clone, PartialEq)]
pub enum Member {
    Absent,
    /// A record without `lifecycle: 1` (prelife-1c G10).
    Pre(Value),
    Life(Gen),
    /// Torn, a FIFO, a folder, any link (at `record.json` or `<id>`), or invalid lifecycle fields.
    Unreadable(Refusal),
}

/// A lifecycle member as `member_of` read it once (R1): its folder, its record and the generations the record names.
#[derive(Debug, Clone, PartialEq)]
pub struct Gen {
    folder: PathBuf,
    record: Value,
    active: Option<u64>,
    pending: Option<u64>,
}

/// R5: one read through the walk; a refusal is never read again another way.
pub fn member_of(folder: &Path) -> Member {
    match record(folder) {
        Evidence::Absent => Member::Absent,
        Evidence::Present(r) if lifecycle(&r) => life_of(folder, r),
        Evidence::Present(r) => Member::Pre(r),
        Evidence::Unreadable => Member::Unreadable(evidence::unreadable()),
    }
}

/// R5: every `<agents>/<id>` entry but a regular file, classified once, links kept (so `member_of` refuses them).
pub fn members(agents: &Path) -> Res<Vec<(String, PathBuf, Member)>> {
    let entries = match std::fs::read_dir(agents) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut all = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !crate::agents::agent_id(&id) || entry.file_type()?.is_file() {
            continue;
        }
        let folder = entry.path();
        let member = member_of(&folder);
        all.push((id, folder, member));
    }
    all.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(all)
}

/// R5: invalid lifecycle fields refuse `internal`, including a pending generation equal to the active one.
fn life_of(folder: &Path, record: Value) -> Member {
    let invalid = || Member::Unreadable(Refusal::internal("invalid lifecycle record"));
    let active = match record.get("generation") {
        None => None,
        Some(g) => match g.as_str().and_then(plain_gen) {
            Some(n) => Some(n),
            None => return invalid(),
        },
    };
    let pending = match record.get("pending") {
        None => None,
        Some(p) => match p.get("gen").and_then(Value::as_u64) {
            Some(n) => Some(n),
            None => return invalid(),
        },
    };
    if pending.is_some() && pending == active {
        return invalid(); // G1.1: a pending generation is 1 + the highest named
    }
    Member::Life(Gen {
        folder: folder.to_path_buf(),
        record,
        active,
        pending,
    })
}

/// R1, R2: generation n's completion markers and commit decision, each read once; Unreadable refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marks {
    pub abandoned: bool,
    pub decision: Option<Decision>,
    pub ended: bool,
    pub released: bool,
}

impl Marks {
    /// A done marker (S6): claim `abandoned`, commit `aborted`, `ended`, `released`.
    pub fn any(&self) -> bool {
        self.abandoned || self.decision == Some(Decision::Aborted) || self.ended || self.released
    }
}

pub fn marks(member: &Path, n: u64) -> Res<Marks> {
    Ok(Marks {
        abandoned: claim(member, n).known()?.is_some_and(|c| c == "abandoned"),
        decision: read(member, n, "commit").known()?.map(|c| decide(&c)),
        ended: read(member, n, "ended").known()?.is_some(),
        released: read(member, n, "released").known()?.is_some(),
    })
}

/// R3, R4: every completion question about a lifecycle member, from one read (ambiguities 3, 13, 14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Status {
    /// The generation the member's identity stands for: pending if not done, else active.
    pub subject: Option<u64>,
    /// The subject is a pending generation (a held identity).
    pub held: bool,
    /// The subject's commit decides Run.
    pub committed: bool,
    /// The active generation's commit decides Run (presentation, notices, proof).
    pub active_committed: bool,
    /// Every generation the record names is done (R3): the member's name and slot are free.
    pub finished: bool,
    /// No active generation, and no pending one or one voided before it ran.
    pub never_ran: bool,
    /// The pending generation's own `done` (S6), or None when the record names none (R3: membership frees exactly
    /// the done generations' hc terminals, `core_facts::take`; a lifecycle notice waits for `active_done`).
    pub pending_done: Option<bool>,
    /// The active generation's own `done`, or None when the record names none.
    pub active_done: Option<bool>,
}

impl Gen {
    pub fn record(&self) -> &Value {
        &self.record
    }

    pub fn folder(&self) -> &Path {
        &self.folder
    }

    pub fn active(&self) -> Option<u64> {
        self.active
    }

    pub fn pending(&self) -> Option<u64> {
        self.pending
    }

    /// `gens/<n>/<file>.json` through the walk; the caller names the generation (R4).
    pub fn read(&self, n: u64, file: &str) -> Evidence {
        read(&self.folder, n, file)
    }

    /// R1: one `marks` per named generation; `record.json` reread at most once, for `completing` (S6).
    pub fn status(&self) -> Res<Status> {
        let pm = self.pending.map(|n| marks(&self.folder, n)).transpose()?;
        let am = self.active.map(|n| marks(&self.folder, n)).transpose()?;
        let marked = pm.is_some_and(|m| m.any()) || am.is_some_and(|m| m.any());
        let completing = if marked {
            let r = record(&self.folder)
                .known()?
                .ok_or_else(|| Refusal::internal("a generation has no member record"))?;
            r.get("completing")
                .and_then(|c| c.get("gen"))
                .and_then(Value::as_u64)
        } else {
            None
        };
        let done = |n: Option<u64>, m: Option<Marks>| {
            n.zip(m).map(|(n, m)| m.any() && completing != Some(n))
        };
        let (p_done, a_done) = (done(self.pending, pm), done(self.active, am));
        let held = p_done == Some(false);
        let run = |m: Option<Marks>| m.is_some_and(|m| m.decision == Some(Decision::Run));
        let p_void = pm.is_some_and(|m| m.abandoned || m.decision == Some(Decision::Aborted));
        Ok(Status {
            subject: if held { self.pending } else { self.active },
            held,
            committed: if held { run(pm) } else { run(am) },
            active_committed: run(am),
            finished: p_done != Some(false) && a_done != Some(false),
            // PM call 17: a void pending generation ran nothing only once it is done (`completing` holds it), so
            // `held` and `never_ran` are never both true.
            never_ran: self.active.is_none()
                && (self.pending.is_none() || (p_void && p_done == Some(true))),
            pending_done: p_done,
            active_done: a_done,
        })
    }
}

/// launch-1 G1.1: 1 + the highest generation any file under `gens/` or `pending` names; 0 for none.
pub fn next(member: &Path, record: Option<&Value>) -> Res<u64> {
    let mut high: Option<u64> = record.and_then(pending);
    if let Some(a) = record.and_then(active) {
        high = high.max(Some(a));
    }
    match std::fs::read_dir(member.join("gens")) {
        Ok(entries) => {
            for e in entries {
                let name = e?.file_name();
                let name = name.to_string_lossy();
                let stem = name.strip_suffix(".claim").unwrap_or(&name);
                if let Ok(n) = stem.parse::<u64>() {
                    high = high.max(Some(n));
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(high.map_or(0, |h| h + 1))
}

/// S6, the one completion predicate: markers first (claim `abandoned`, commit `aborted`, `ended`, `released`),
/// then a reread of `record.json`; never done while `record.completing.gen` is `n` (release-1d R4). `Unreadable`
/// anywhere is the `evidence` refusal, never "not done".
pub fn done(member: &Path, n: u64) -> Res<bool> {
    if !marks(member, n)?.any() {
        return Ok(false);
    }
    let record = record(member)
        .known()?
        .ok_or_else(|| Refusal::internal("a generation has no member record"))?;
    let completing = record
        .get("completing")
        .and_then(|c| c.get("gen"))
        .and_then(Value::as_u64);
    Ok(completing != Some(n))
}

/// What a whole `commit.json` decides (launch-1 G4): only a lone `committed: true` runs. `aborted: true` stops, also
/// beside `committed: true`; anything else is invalid and never runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Run,
    Aborted,
    Invalid,
}

pub fn decide(commit: &Value) -> Decision {
    let yes = |k: &str| commit.get(k) == Some(&Value::Bool(true));
    if yes("aborted") {
        Decision::Aborted
    } else if yes("committed") {
        Decision::Run
    } else {
        Decision::Invalid
    }
}

/// Whether generation `n` committed: its commit decides `Run` (`decide`); `Unreadable` refuses.
pub fn committed(member: &Path, n: u64) -> Res<bool> {
    Ok(read(member, n, "commit")
        .known()?
        .is_some_and(|c| decide(&c) == Decision::Run))
}

/// The client a generation proved (proof-0b K1a): the recorded launcher and the process it started, each as
/// (pid, start).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proven {
    pub launcher: (u32, i64),
    pub client: (u32, i64),
}

fn pid_start(v: &Value) -> Option<(u32, i64)> {
    Some((
        u32::try_from(v.get("pid")?.as_u64()?).ok()?,
        v.get("start")?.as_i64()?,
    ))
}

/// proof-0b K1a, the one accessor of the client of generation `n`: `client.json` Present; `exit.json` Absent or
/// Present without `client-conflict`; `launcher.json` Present; the client and the launcher both live, each with its
/// recorded start both as opened and as listed; the client a child of that launcher. Anything else, `Unreadable`
/// included, is unproven (None): the generation stays reserved and caller proof refuses. Liveness alone is never
/// enough.
pub fn client(ctx: &crate::ctx::Ctx, member: &Path, n: u64) -> Option<Proven> {
    let Evidence::Present(c) = read(member, n, "client") else {
        return None;
    };
    match read(member, n, "exit") {
        Evidence::Absent => {}
        Evidence::Present(e)
            if e.get("reason").and_then(Value::as_str) != Some("client-conflict") => {}
        _ => return None,
    }
    let Evidence::Present(l) = read(member, n, "launcher") else {
        return None;
    };
    let (launcher, client) = (pid_start(&l)?, pid_start(&c)?);
    child_of(ctx, launcher, client).then_some(Proven { launcher, client })
}

/// Both processes live, each with its recorded start both as opened and as listed, and `client` a child of
/// `launcher` (the launcher's row is read first, as `client` always did).
fn child_of(ctx: &crate::ctx::Ctx, launcher: (u32, i64), client: (u32, i64)) -> bool {
    let live = |(pid, start): (u32, i64)| {
        let row = ctx.os.row(pid)?;
        let opened = ctx.os.open(pid)?;
        (ctx.os.same_start(opened.start, start)
            && row.start.is_some_and(|s| ctx.os.same_start(s, start)))
        .then_some(row)
    };
    live(launcher).is_some_and(|l_row| live(client).is_some_and(|c_row| c_row.ppid == l_row.pid))
}

/// report-2b K5's answer (ambiguity 8).
#[derive(Debug, Clone, PartialEq)]
pub enum Ready {
    Outcome(Value),
    Failed,
    Ok,
    Undecided,
}

impl Ready {
    /// The reply word of `report -Session` (ambiguity 14).
    pub fn word(&self) -> &'static str {
        match self {
            Ready::Outcome(_) => "outcome",
            Ready::Failed => "failed",
            Ready::Ok => "ok",
            Ready::Undecided => "undecided",
        }
    }
}

/// What K5 decided for a caller that holds the gen lock, and the files it read that outcome-2 decides from, so that
/// caller never reads them again (R1; outcome-2 call 1, o2 call 16).
#[derive(Debug, Clone, PartialEq)]
pub struct Held {
    pub ready: Ready,
    /// Step 2's `failure.json`, when `ready` is `Failed`.
    pub failure: Option<Value>,
    /// Step 4's `report.json`, whenever step 4 found one (also when the pair does not match).
    pub report: Option<Value>,
    /// The `exit.json` step 4's client proof read, when it read one.
    pub exit: Option<Value>,
}

/// r2b call 21 (o2 call 15): the client generation `n` proved, three-valued. `client.json` Absent, a
/// `client-conflict` exit, an Absent `launcher.json`, a malformed pid or start, or a pair that is not live parent and
/// child is unproven (`Ok(None)`); an Unreadable `client.json`, `exit.json` or `launcher.json` refuses `evidence`.
/// `gens::client` stays two-valued for its existing callers.
pub fn client_evidence(ctx: &crate::ctx::Ctx, member: &Path, n: u64) -> Res<Option<(u32, i64)>> {
    client_proof(ctx, member, n).map(|(proven, _)| proven)
}

/// The proven client (pid, start), and the `exit.json` read beside it (None when none was read or found).
type ClientProof = (Option<(u32, i64)>, Option<Value>);

/// `client_evidence` and the `exit.json` it read.
fn client_proof(ctx: &crate::ctx::Ctx, member: &Path, n: u64) -> Res<ClientProof> {
    let Some(c) = read(member, n, "client").known()? else {
        return Ok((None, None));
    };
    let exit = read(member, n, "exit").known()?;
    let conflict = exit
        .as_ref()
        .is_some_and(|e| e.get("reason").and_then(Value::as_str) == Some("client-conflict"));
    if conflict {
        return Ok((None, exit));
    }
    let Some(l) = read(member, n, "launcher").known()? else {
        return Ok((None, exit));
    };
    let proven = match (pid_start(&l), pid_start(&c)) {
        (Some(launcher), Some(client)) if child_of(ctx, launcher, client) => Some(client),
        _ => None,
    };
    Ok((proven, exit))
}

/// report-2b K5 for a caller that already holds `gen_lock_key(key, id, n)` (outcome-2's adjudication). Takes no lock:
/// a second take of one key in one process refuses on Unix. Each file is read once through the walk (R1):
/// `outcome.json`, then `failure.json` (authoritative), then `ready.json`, then `report.json` whose pid and start equal
/// the client `client_evidence` proves. `ready.json` is written, and an existing one confirmed, only through
/// `publish_once` (PM call 14): no `ok` before its flushes succeed. Any Unreadable, failed publication or failed flush
/// refuses: decide nothing (ambiguity 8).
pub fn promote_held(ctx: &crate::ctx::Ctx, member: &Path, n: u64) -> Res<Held> {
    let held = |ready, failure, report, exit| Held {
        ready,
        failure,
        report,
        exit,
    };
    if let Some(outcome) = read(member, n, "outcome").known()? {
        return Ok(held(Ready::Outcome(outcome), None, None, None));
    }
    if let Some(failure) = read(member, n, "failure").known()? {
        return Ok(held(Ready::Failed, Some(failure), None, None));
    }
    if let Some(ready) = read(member, n, "ready").known()? {
        // Present: confirmed through the publisher, so a ready.json an earlier call linked but could not flush is
        // flushed now, or this call refuses (PM call 14).
        publish_once(member, n, "ready", &ready)?;
        return Ok(held(Ready::Ok, None, None, None));
    }
    let Some(report) = read(member, n, "report").known()? else {
        return Ok(held(Ready::Undecided, None, None, None));
    };
    let (proven, exit) = client_proof(ctx, member, n)?;
    let matches = |(pid, start): (u32, i64)| {
        pid_start(&report).is_some_and(|(p, s)| p == pid && ctx.os.same_start(s, start))
    };
    let Some((pid, start)) = proven.filter(|c| matches(*c)) else {
        return Ok(held(Ready::Undecided, None, Some(report), exit));
    };
    let ready = json!({"pid": pid, "start": start, "at": crate::util::utc_o(ctx.clock.now())});
    let won = match publish_once(member, n, "ready", &ready)? {
        Published::Published => Ready::Ok,
        // The sanctioned reread (PM call 11): someone else's ready.json landed between our read and our link.
        Published::Exists => match read(member, n, "ready") {
            Evidence::Present(_) => Ready::Ok,
            _ => return Err(evidence::unreadable()),
        },
    };
    Ok(held(won, None, Some(report), exit))
}

/// report-2b K5, the only writer of `ready.json`, for callers that do not hold the gen lock (the handshake, K4.3):
/// one hold of the gen lock around `promote_held`.
pub fn promote_ready(ctx: &crate::ctx::Ctx, key: &str, id: &str, n: u64) -> Res<Ready> {
    if !plain_key(key) || !plain_id(id) {
        return Err(Refusal::internal("promote_ready: not a plain member name"));
    }
    let member = ctx.state_root.join(key).join("agents").join(id);
    super::lock(ctx, &super::team_state::gen_lock_key(key, id, n), || {
        promote_held(ctx, &member, n).map(|h| h.ready)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::evidence::{Published, publish};
    use serde_json::json;

    fn member() -> (crate::testing::TempDir, PathBuf) {
        let d = crate::testing::TempDir::new();
        let m = d.path().join("agents/aaaaaaaaaaaa");
        std::fs::create_dir_all(&m).unwrap();
        let r = json!({"lifecycle": 1, "generation": "1"});
        crate::state::fs::write_json(&m.join("record.json"), &r).unwrap();
        (d, m)
    }

    /// A member `<root>/0123456789abcdef/agents/aaaaaaaaaaaa` (the walk starts three folders up) with `record`.
    fn life(record: Value) -> (crate::testing::TempDir, PathBuf) {
        let d = crate::testing::TempDir::new();
        let m = d.path().join("0123456789abcdef/agents/aaaaaaaaaaaa");
        std::fs::create_dir_all(&m).unwrap();
        crate::state::fs::write_json(&m.join("record.json"), &record).unwrap();
        (d, m)
    }
    fn gen_of(m: &Path) -> Gen {
        match member_of(m) {
            Member::Life(g) => g,
            other => panic!("not a lifecycle member: {other:?}"),
        }
    }
    fn unreadable(m: &Path) -> bool {
        matches!(member_of(m), Member::Unreadable(_))
    }

    // R5, ambiguity 1: one read through the walk, no fallback; invalid lifecycle fields are unreadable.
    #[test]
    fn member_of_classifies_once_without_fallback() {
        let (_d, m) = life(json!({"name": "w"}));
        assert!(
            matches!(member_of(&m), Member::Pre(r) if r["name"] == "w"),
            "prelife-1c G10"
        );
        assert!(matches!(
            member_of(&m.with_file_name("bbbbbbbbbbbb")),
            Member::Absent
        ));
        for bad in [
            json!({"lifecycle": 1, "generation": "01"}),
            json!({"lifecycle": 1, "generation": 1}),
            json!({"lifecycle": 1, "pending": {"gen": "1"}}),
            json!({"lifecycle": 1, "generation": "1", "pending": {"gen": 1}}),
        ] {
            let (_d, m) = life(bad.clone());
            assert!(unreadable(&m), "{bad}");
        }
        let (_d, m) = life(json!({}));
        std::fs::write(m.join("record.json"), b"{\"lifecy").unwrap();
        assert!(unreadable(&m), "torn");
        // Call 20: on Windows too (a directory symlink, else a junction); never skipped.
        let (d, m) = life(json!({}));
        let elsewhere = d.path().join("elsewhere.json");
        std::fs::remove_file(m.join("record.json")).unwrap();
        crate::testing::link_dir(&elsewhere, &m.join("record.json"));
        assert!(unreadable(&m), "a dangling link");
        crate::state::fs::write_json(&elsewhere, &json!({"name": "w"})).unwrap();
        assert!(unreadable(&m), "a link to a pre-lifecycle record");
        let (d, m) = life(json!({"name": "w"}));
        let moved = d.path().join("moved");
        std::fs::rename(&m, &moved).unwrap();
        crate::testing::link_dir(&moved, &m);
        assert!(unreadable(&m), "a linked member folder");
    }

    // R5 (audit A4): a FIFO at record.json is unreadable at once, never opened.
    #[cfg(unix)]
    #[test]
    fn a_fifo_record_is_unreadable_and_never_opened() {
        let (_d, m) = life(json!({}));
        std::fs::remove_file(m.join("record.json")).unwrap();
        let c = std::ffi::CString::new(m.join("record.json").to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        let at = m.clone();
        std::thread::spawn(move || tx.send(unreadable(&at)).unwrap());
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(2)),
            Ok(true),
            "never blocks"
        );
    }

    // R5 (audit A3): members keeps link entries so member_of refuses them; it skips only non-ids and regular files.
    #[test]
    fn members_keeps_links_and_skips_only_files_and_non_ids() {
        let (d, m) = life(json!({"name": "w"}));
        let agents = m.parent().unwrap().to_path_buf();
        std::fs::write(agents.join("cccccccccccc"), b"x").unwrap();
        std::fs::create_dir(agents.join("not-an-id")).unwrap();
        crate::testing::link_dir(&d.path().join("gone"), &agents.join("dddddddddddd"));
        let got: Vec<(String, bool)> = members(&agents)
            .unwrap()
            .into_iter()
            .map(|(id, _, member)| (id, matches!(member, Member::Unreadable(_))))
            .collect();
        let want = vec![
            ("aaaaaaaaaaaa".to_string(), false),
            ("dddddddddddd".to_string(), true),
        ];
        assert_eq!(got, want);
        assert!(
            members(&d.path().join("none")).unwrap().is_empty(),
            "no agents folder"
        );
    }

    // R4 (audit A8): Gen::read(n, ..) reads generation n only; a pending generation never stands for an active one.
    #[test]
    fn a_generation_reads_only_its_own_evidence() {
        let (_d, m) = life(json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd"}));
        crate::state::fs::write_json(&m.join("notice.json"), &json!({"kind": "stopped"})).unwrap();
        publish(&path(&m, 0, "notice"), &json!({"kind": "stopped"})).unwrap();
        let g = gen_of(&m);
        assert_eq!((g.active(), g.pending()), (Some(1), None));
        assert_eq!(
            g.read(1, "notice"),
            Evidence::Absent,
            "neither the folder nor generation 0"
        );
        publish(&path(&m, 1, "notice"), &json!({"kind": "exited"})).unwrap();
        assert!(
            matches!(gen_of(&m).read(1, "notice"), Evidence::Present(n) if n["kind"] == "exited")
        );
        let (_d, m) = life(json!({"lifecycle": 1, "pending": {"gen": 2}}));
        publish(&path(&m, 2, "commit"), &json!({"committed": true})).unwrap();
        let s = gen_of(&m).status().unwrap();
        assert_eq!(
            (s.subject, s.active_committed),
            (Some(2), false),
            "pending commit is the held identity's only"
        );
    }

    /// Seeds `files` (generation, file, value; `"claim"` is `gens/<n>.claim`; the value `"TORN"` writes torn bytes)
    /// beside `record`, then answers `status()`.
    fn status_of(record: Value, files: &[(u64, &str, Value)]) -> Res<Status> {
        let (_d, m) = life(record);
        for (n, f, v) in files {
            let at = if *f == "claim" {
                claim_path(&m, *n)
            } else {
                path(&m, *n, f)
            };
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            if *v == json!("TORN") {
                std::fs::write(&at, b"{\"x").unwrap();
            } else {
                crate::state::fs::write_json(&at, v).unwrap();
            }
        }
        gen_of(&m).status()
    }

    // R2, R3, R4 (audit A5, C3): one read answers subject, held, finished and never_ran; an unreadable marker refuses.
    #[test]
    fn status_answers_every_completion_question_from_one_read() {
        let l = |extra: Value| {
            let mut r = json!({"lifecycle": 1});
            for (k, v) in extra.as_object().unwrap() {
                r[k] = v.clone();
            }
            r
        };
        let p1 = || l(json!({"pending": {"gen": 1}}));
        let g1 = || l(json!({"generation": "1", "hcName": "m0123abcd"}));
        let g1p2 = || l(json!({"generation": "1", "hcName": "m0123abcd", "pending": {"gen": 2}}));
        let (ab, ok, end) = (
            json!({"aborted": true}),
            json!({"committed": true}),
            json!({"state": "finished"}),
        );
        // (case, record, files, (subject, held, finished, never_ran))
        type Case<'a> = (
            &'a str,
            Value,
            Vec<(u64, &'a str, Value)>,
            (Option<u64>, bool, bool, bool),
        );
        let cases: Vec<Case> = vec![
            ("none", l(json!({})), vec![], (None, false, true, true)),
            (
                "pending undecided",
                p1(),
                vec![],
                (Some(1), true, false, false),
            ),
            (
                "pending aborted",
                p1(),
                vec![(1, "commit", ab.clone())],
                (None, false, true, true),
            ),
            (
                "pending abandoned",
                p1(),
                vec![(1, "claim", json!("abandoned"))],
                (None, false, true, true),
            ),
            (
                "pending ended: it ran",
                p1(),
                vec![(1, "ended", end.clone())],
                (None, false, true, false),
            ),
            (
                "pending released: it ran",
                p1(),
                vec![(1, "released", json!({}))],
                (None, false, true, false),
            ),
            (
                "active running",
                g1(),
                vec![(1, "commit", ok.clone())],
                (Some(1), false, false, false),
            ),
            (
                "active ended",
                g1(),
                vec![(1, "ended", end.clone())],
                (Some(1), false, true, false),
            ),
            (
                "active completing",
                l(json!({"generation": "1", "hcName": "m0123abcd", "completing": {"gen": 1}})),
                vec![(1, "ended", end.clone())],
                (Some(1), false, false, false),
            ),
            (
                "active and pending",
                g1p2(),
                vec![(1, "ended", end.clone())],
                (Some(2), true, false, false),
            ),
            (
                "pending void, active runs",
                g1p2(),
                vec![(2, "commit", ab.clone())],
                (Some(1), false, false, false),
            ),
        ];
        for (case, record, files, want) in cases {
            let s = status_of(record, &files).unwrap();
            assert_eq!((s.subject, s.held, s.finished, s.never_ran), want, "{case}");
        }
        let s = status_of(p1(), &[(1, "commit", ok.clone())]).unwrap();
        assert_eq!(
            (s.committed, s.active_committed),
            (true, false),
            "the subject's commit, not the active one's"
        );
        let s = status_of(g1p2(), &[(1, "commit", ok.clone())]).unwrap();
        assert_eq!(
            (s.committed, s.active_committed),
            (false, true),
            "a held identity never takes 1's commit"
        );
        // R3 (re-audit, core_facts reservations): each named generation's own done, so membership frees exactly those.
        let s = status_of(g1p2(), &[(1, "ended", end.clone())]).unwrap();
        assert_eq!(
            (s.pending_done, s.active_done),
            (Some(false), Some(true)),
            "active done, pending not"
        );
        let s = status_of(p1(), &[(1, "commit", ab.clone())]).unwrap();
        assert_eq!(
            (s.pending_done, s.active_done),
            (Some(true), None),
            "pending void; no active generation"
        );
        for (case, files) in [
            (
                "torn claim, aborted commit",
                vec![(1, "claim", json!("TORN")), (1, "commit", ab.clone())],
            ),
            (
                "abandoned claim, torn commit",
                vec![
                    (1, "claim", json!("abandoned")),
                    (1, "commit", json!("TORN")),
                ],
            ),
            ("torn ended", vec![(1, "ended", json!("TORN"))]),
            (
                "aborted commit, torn released",
                vec![(1, "commit", ab.clone()), (1, "released", json!("TORN"))],
            ),
        ] {
            assert_eq!(
                status_of(p1(), &files).map_err(|e| e.check).err(),
                Some("evidence"),
                "{case}"
            );
        }
    }

    // R1: status opens each evidence file at most once, record.json once.
    #[test]
    fn status_opens_each_file_once() {
        let (_d, m) = life(
            json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd", "pending": {"gen": 2}}),
        );
        publish(&path(&m, 1, "ended"), &json!({"state": "finished"})).unwrap();
        publish(&path(&m, 2, "commit"), &json!({"committed": true})).unwrap();
        let g = gen_of(&m);
        let (s, reads) = crate::state::fs::record_reads(|| g.status());
        s.unwrap();
        let mut seen = reads.clone();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), reads.len(), "{reads:?}");
        assert_eq!(
            reads.iter().filter(|p| p.ends_with("record.json")).count(),
            1,
            "{reads:?}"
        );
    }

    // R1, S6 (re-audit: PM call 4's sanctioned reread): `completing` is read from record.json as it is now, never from
    // the record `member_of` held; a record torn since then refuses.
    #[test]
    fn status_rereads_the_record_for_completing() {
        let (_d, m) = life(json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd"}));
        publish(&path(&m, 1, "ended"), &json!({"state": "finished"})).unwrap();
        let g = gen_of(&m);
        assert!(g.status().unwrap().finished, "ended, not completing");
        let completing = json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd", "completing": {"gen": 1}});
        crate::state::fs::write_json(&m.join("record.json"), &completing).unwrap();
        assert!(
            !g.status().unwrap().finished,
            "completing written after member_of holds it"
        );
        std::fs::write(m.join("record.json"), b"{\"lifecy").unwrap();
        assert_eq!(
            g.status().map_err(|e| e.check).err(),
            Some("evidence"),
            "torn since member_of"
        );
    }

    #[test]
    fn generation_zero_reads_only_its_own_folder() {
        let (_d, m) = member();
        // A folder-level (pre-lifecycle) notice is not generation 0s.
        crate::state::fs::write_json(&m.join("notice.json"), &json!({})).unwrap();
        assert_eq!(read(&m, 0, "notice"), Evidence::Absent);
        assert_eq!(
            publish(&path(&m, 0, "notice"), &json!({})).unwrap(),
            Published::Published
        );
        assert!(matches!(read(&m, 0, "notice"), Evidence::Present(_)));
        assert_eq!(read(&m, 1, "notice"), Evidence::Absent);
        assert_eq!(delivery_id("aaaaaaaaaaaa", 1), "exit-aaaaaaaaaaaa-1");
        assert_ne!(
            delivery_id("aaaaaaaaaaaa", 0),
            delivery_id("aaaaaaaaaaaa", 1)
        );
    }

    #[test]
    fn next_counts_every_file_and_pending() {
        let (_d, m) = member();
        assert_eq!(next(&m, None).unwrap(), 0);
        publish(&claim_path(&m, 0), &json!("run")).unwrap();
        assert_eq!(next(&m, None).unwrap(), 1);
        publish(&path(&m, 3, "spec"), &json!({})).unwrap();
        assert_eq!(next(&m, None).unwrap(), 4);
        assert_eq!(next(&m, Some(&json!({"pending": {"gen": 6}}))).unwrap(), 7);
    }

    #[test]
    fn done_reads_markers_then_the_record_and_honours_completing() {
        let (_d, m) = member();
        assert!(!done(&m, 1).unwrap());
        publish(&path(&m, 1, "ended"), &json!({"state": "finished"})).unwrap();
        let (got, reads) = crate::state::fs::record_reads(|| done(&m, 1));
        assert!(got.unwrap());
        assert_eq!(
            reads.last().unwrap(),
            &m.join("record.json"),
            "the record is reread last"
        );
        let r = json!({"lifecycle": 1, "completing": {"gen": 1}});
        crate::state::fs::write_json(&m.join("record.json"), &r).unwrap();
        assert!(!done(&m, 1).unwrap(), "completing holds the reservation");
        for (file, value) in [
            ("commit", json!({"aborted": true})),
            ("released", json!({})),
        ] {
            let (_d, m) = member();
            publish(&path(&m, 2, file), &value).unwrap();
            assert!(done(&m, 2).unwrap(), "{file}");
        }
        let (_d, m) = member();
        publish(&claim_path(&m, 2), &json!("abandoned")).unwrap();
        assert!(done(&m, 2).unwrap());
    }

    #[test]
    fn an_unreadable_marker_stops_the_caller() {
        let (_d, m) = member();
        std::fs::create_dir_all(dir(&m, 1)).unwrap();
        std::fs::write(path(&m, 1, "ended"), br#"{"st"#).unwrap();
        assert_eq!(done(&m, 1).map_err(|e| e.check), Err("evidence"));
        std::fs::write(claim_path(&m, 1), br#""aban"#).unwrap();
        assert_eq!(done(&m, 1).map_err(|e| e.check), Err("evidence"));
    }

    // Ended is recorded: once ended.json is published, hc forgetting the terminal changes nothing.
    #[test]
    fn a_recorded_end_stays_done_after_hc_forgets() {
        let (_d, m) = member();
        let facts = json!({"address": "t1", "hcName": "m0123abcd", "labels": {"mux.gen": "1"},
            "state": "finished", "reason": "exited", "exitCode": 0, "at": "2026-11-14T00:00:00Z"});
        assert_eq!(
            publish(&path(&m, 1, "ended"), &facts).unwrap(),
            Published::Published
        );
        assert!(done(&m, 1).unwrap());
        assert_eq!(
            publish(&path(&m, 1, "ended"), &json!({})).unwrap(),
            Published::Exists
        );
        assert_eq!(read(&m, 1, "ended"), Evidence::Present(facts));
    }

    // S6: completing holds every kind of done, not only ended.
    #[test]
    fn completing_holds_every_kind_of_done() {
        for kind in ["abandoned", "aborted", "ended", "released"] {
            let (_d, m) = member();
            let at = match kind {
                "abandoned" => claim_path(&m, 1),
                "aborted" => path(&m, 1, "commit"),
                _ => path(&m, 1, kind),
            };
            let body = match kind {
                "abandoned" => json!("abandoned"),
                "aborted" => json!({"aborted": true}),
                _ => json!({}),
            };
            assert_eq!(publish(&at, &body).unwrap(), Published::Published);
            assert_eq!(done(&m, 1).ok(), Some(true), "{kind}");
            let r = json!({"lifecycle": 1, "completing": {"gen": 1}});
            crate::state::fs::write_json(&m.join("record.json"), &r).unwrap();
            assert_eq!(done(&m, 1).ok(), Some(false), "{kind}: completing holds it");
        }
    }

    // S6: a file where generation 0's folder belongs is Unreadable, never Absent: done stops, even with an abandoned
    // claim and a record that holds nothing.
    #[test]
    fn a_file_in_place_of_a_generation_folder_stops_done() {
        let (_d, m) = member();
        std::fs::create_dir_all(m.join("gens")).unwrap();
        std::fs::write(dir(&m, 0), b"").unwrap();
        publish(&claim_path(&m, 0), &json!("abandoned")).unwrap();
        assert_eq!(read(&m, 0, "commit"), Evidence::Unreadable);
        assert_eq!(done(&m, 0).map_err(|e| e.check), Err("evidence"));
    }

    // Point 8: the walk below a member folder: each folder must be a real folder; a missing one reads Absent; a file
    // where gens/<n>/ belongs reads Unreadable on every OS.
    #[test]
    fn the_walk_below_a_member_needs_real_folders() {
        let (_d, m) = member();
        assert_eq!(read(&m, 1, "commit"), Evidence::Absent, "no gens folder");
        std::fs::create_dir_all(m.join("gens")).unwrap();
        assert_eq!(
            read(&m, 1, "commit"),
            Evidence::Absent,
            "no generation folder"
        );
        std::fs::write(m.join("gens").join("1"), b"x").unwrap();
        assert_eq!(
            read(&m, 1, "commit"),
            Evidence::Unreadable,
            "a file where gens/1/ belongs"
        );
        assert!(
            matches!(record(&m), Evidence::Present(_)),
            "the member record"
        );
    }

    // Point 8: a link at agents/<id>, at gens or at gens/<n>, live or dangling, reads Unreadable and nothing behind it
    // is opened. Unix only.
    #[cfg(unix)]
    #[test]
    fn a_linked_folder_below_a_member_reads_unreadable() {
        use std::os::unix::fs::symlink;
        for case in [
            "gens/1 to gens/2",
            "dangling gens/1",
            "dangling gens",
            "agents/<id> to a copy",
        ] {
            let (_d, m) = member();
            publish(&path(&m, 2, "commit"), &json!({"committed": true})).unwrap();
            match case {
                "gens/1 to gens/2" => symlink(dir(&m, 2), dir(&m, 1)).unwrap(),
                "dangling gens/1" => symlink(dir(&m, 9), dir(&m, 1)).unwrap(),
                "agents/<id> to a copy" => {
                    let copy = m.with_file_name("copy");
                    std::fs::rename(&m, &copy).unwrap();
                    symlink(&copy, &m).unwrap();
                }
                _ => {
                    std::fs::remove_dir_all(m.join("gens")).unwrap();
                    symlink(m.join("nowhere"), m.join("gens")).unwrap();
                }
            }
            let (got, reads) =
                crate::state::fs::record_reads(|| (read(&m, 1, "commit"), claim(&m, 1)));
            assert_eq!(got.0, Evidence::Unreadable, "{case}");
            let refused = matches!(case, "dangling gens" | "agents/<id> to a copy");
            assert_eq!(got.1 == Evidence::Unreadable, refused, "{case}: the claim");
            assert!(reads.is_empty(), "{case}: opened {reads:?}");
            if case == "agents/<id> to a copy" {
                assert_eq!(
                    record(&m),
                    Evidence::Unreadable,
                    "the record behind a linked member folder"
                );
            }
        }
    }

    // Final audit: the walk starts at the state root, so an accidental link at <key> or at agents reads Unreadable
    // and nothing below it is opened. Its own fixture of the real shape <root>/<key>/agents/<id>. Unix only.
    #[cfg(unix)]
    #[test]
    fn a_linked_key_or_agents_folder_reads_unreadable() {
        for case in ["none", "key", "agents"] {
            let d = crate::testing::TempDir::new();
            let key = d.path().join("0123456789abcdef");
            let m = key.join("agents").join("aaaaaaaaaaaa");
            publish(&path(&m, 1, "commit"), &json!({"committed": true})).unwrap();
            crate::state::fs::write_json(&m.join("record.json"), &json!({"lifecycle": 1})).unwrap();
            let link = if case == "key" {
                key.clone()
            } else {
                key.join("agents")
            };
            if case != "none" {
                let copy = d.path().join("copy");
                std::fs::rename(&link, &copy).unwrap();
                std::os::unix::fs::symlink(&copy, &link).unwrap();
            }
            let (got, reads) =
                crate::state::fs::record_reads(|| (read(&m, 1, "commit"), record(&m)));
            if case == "none" {
                assert!(
                    matches!(got, (Evidence::Present(_), Evidence::Present(_))),
                    "control: {got:?}"
                );
            } else {
                assert_eq!(got, (Evidence::Unreadable, Evidence::Unreadable), "{case}");
                assert!(reads.is_empty(), "{case}: opened {reads:?}");
            }
        }
    }

    // launch-1 G4: a lone `committed: true` runs; `aborted: true` stops even beside it; anything else never runs.
    #[test]
    fn a_commit_decides_only_when_committed_alone() {
        for (body, want) in [
            (json!({"committed": true}), Decision::Run),
            (json!({"committed": true, "at": "x"}), Decision::Run),
            (json!({"aborted": true}), Decision::Aborted),
            (
                json!({"committed": true, "aborted": true}),
                Decision::Aborted,
            ),
            (json!({"committed": "true"}), Decision::Invalid),
            (json!({"committed": false}), Decision::Invalid),
            (json!({"committedAt": "x"}), Decision::Invalid),
            (json!({"x": 1}), Decision::Invalid),
            (json!(true), Decision::Invalid),
            (json!(null), Decision::Invalid),
        ] {
            assert_eq!(decide(&body), want, "{body}");
            let (_d, m) = member();
            publish(&path(&m, 1, "commit"), &body).unwrap();
            assert_eq!(committed(&m, 1).unwrap(), want == Decision::Run, "{body}");
        }
        let (_d, m) = member();
        assert!(!committed(&m, 1).unwrap(), "absent");
        std::fs::create_dir_all(dir(&m, 1)).unwrap();
        std::fs::write(path(&m, 1, "commit"), br#"{"comm"#).unwrap();
        assert_eq!(committed(&m, 1).map_err(|e| e.check), Err("evidence"));
    }

    // Redesign 3, coordinator point 2: every name is a plain value before any join: `..`, `.`, uppercase, a wrong
    // length and a leading zero are each refused (id, server key, generation).
    #[test]
    fn names_are_plain_values_before_any_join() {
        assert!(plain_id("0123456789ab") && plain_key("0123456789abcdef"));
        assert_eq!((plain_gen("0"), plain_gen("17")), (Some(0), Some(17)));
        for bad in [
            "..",
            ".",
            "AAAAAAAAAAAA",
            "aaaaaaaaaaa",
            "aaaaaaaaaaaag",
            "aaaaaaa.aaaa",
        ] {
            assert!(!plain_id(bad), "id {bad}");
        }
        for bad in [
            "..",
            ".",
            "0123456789ABCDEF",
            "0123456789abcde",
            "k",
            "01234567./abcdef",
        ] {
            assert!(!plain_key(bad), "key {bad}");
        }
        for bad in [
            "..",
            ".",
            "01",
            "00",
            "+1",
            "-1",
            "",
            "1.",
            "18446744073709551616",
        ] {
            assert_eq!(plain_gen(bad), None, "generation {bad}");
        }
    }

    // S6: an unreadable record.json stops done; it never means done.
    #[test]
    fn an_unreadable_record_stops_done() {
        let (_d, m) = member();
        publish(&path(&m, 1, "ended"), &json!({})).unwrap();
        std::fs::write(m.join("record.json"), b"{").unwrap();
        assert_eq!(done(&m, 1).map_err(|e| e.check), Err("evidence"));
    }
}

#[cfg(test)]
mod client_tests {
    use super::*;
    use crate::state::evidence::publish;
    use crate::testing::{Fakes, TempDir, row, unknown_start};
    use serde_json::json;

    // Launcher 500 (start 1000) and its child 600 (start 1500); generation 1 has its launcher.json.
    fn world() -> (Fakes, crate::ctx::Ctx, TempDir, PathBuf) {
        let fakes = Fakes::default();
        fakes.process(row(500, 1, 1000));
        fakes.process(row(600, 500, 1500));
        let ctx = fakes.ctx();
        let d = TempDir::new();
        let m = d.path().join("agents/aaaaaaaaaaaa");
        publish(
            &path(&m, 1, "launcher"),
            &json!({"pid": 500, "start": 1000, "startedAt": "x"}),
        )
        .unwrap();
        (fakes, ctx, d, m)
    }

    #[test]
    fn a_child_of_the_recorded_launcher_is_proven_from_client_json_alone() {
        let (_f, ctx, _d, m) = world();
        assert_eq!(client(&ctx, &m, 1), None, "absent: unproven");
        publish(&path(&m, 1, "client"), &json!({"pid": 600, "start": 1500})).unwrap();
        let want = Proven {
            launcher: (500, 1000),
            client: (600, 1500),
        };
        assert_eq!(client(&ctx, &m, 1), Some(want));
        assert_eq!(
            client(&ctx, &m, 2),
            None,
            "another generation has its own files"
        );
    }

    #[test]
    fn a_foreign_conflicting_torn_or_reparented_client_is_unproven() {
        // Foreign: live, but not a child of the recorded launcher.
        let (f, ctx, _d, m) = world();
        f.process(row(700, 1, 1600));
        publish(&path(&m, 1, "client"), &json!({"pid": 700, "start": 1600})).unwrap();
        assert_eq!(client(&ctx, &m, 1), None, "foreign");
        // exit.json says client-conflict.
        let (_f, ctx, _d, m) = world();
        publish(&path(&m, 1, "client"), &json!({"pid": 600, "start": 1500})).unwrap();
        publish(
            &path(&m, 1, "exit"),
            &json!({"exitCode": 1, "reason": "client-conflict"}),
        )
        .unwrap();
        assert_eq!(client(&ctx, &m, 1), None, "conflict");
        // Torn client.json: unproven, and never deleted.
        let (_f, ctx, _d, m) = world();
        std::fs::write(path(&m, 1, "client"), b"{").unwrap();
        assert_eq!(client(&ctx, &m, 1), None, "torn");
        assert!(path(&m, 1, "client").exists());
        // Launcher dead, the client reparented to 1.
        let (f, ctx, _d, m) = world();
        publish(&path(&m, 1, "client"), &json!({"pid": 600, "start": 1500})).unwrap();
        f.forget(500);
        f.forget(600);
        f.process(row(600, 1, 1500));
        assert_eq!(client(&ctx, &m, 1), None, "reparented");
        // A reused client pid: same pid, another start in both row and open.
        let (_f, ctx, _d, m) = world();
        publish(&path(&m, 1, "client"), &json!({"pid": 600, "start": 1499})).unwrap();
        assert_eq!(client(&ctx, &m, 1), None, "reused client pid");
        // A dangling exit.json is Unreadable, so the client is unproven.
        #[cfg(unix)]
        {
            let (_f, ctx, _d, m) = world();
            publish(&path(&m, 1, "client"), &json!({"pid": 600, "start": 1500})).unwrap();
            std::os::unix::fs::symlink(path(&m, 2, "exit"), path(&m, 1, "exit")).unwrap();
            assert_eq!(client(&ctx, &m, 1), None, "dangling exit.json");
        }
    }

    // Each guard alone: everything else valid (proven first), then one fact broken. Each case is the one that kills
    // its guard's mutation (Step 4 table); the opened start and the listed start are compared separately.
    #[test]
    fn each_guard_alone_leaves_the_client_unproven() {
        type Break = fn(&Fakes, &Path);
        let cases: [(&str, Break); 10] = [
            ("client opens with another start", |f, _| {
                f.opens(600, &[Some(1499)])
            }),
            ("client start unreadable at open", |f, _| {
                f.opens(600, &[None])
            }),
            ("client row lists another start", |f, _| {
                f.forget(600);
                f.process(row(600, 500, 1499));
                f.opens(600, &[Some(1500)]);
            }),
            ("client row start unknown", |f, _| {
                f.forget(600);
                f.process(unknown_start(600, 500));
                f.opens(600, &[Some(1500)]);
            }),
            ("launcher opens with another start", |f, _| {
                f.opens(500, &[Some(999)])
            }),
            ("launcher start unreadable at open", |f, _| {
                f.opens(500, &[None])
            }),
            ("launcher row lists another start", |f, _| {
                f.forget(500);
                f.process(row(500, 1, 999));
                f.opens(500, &[Some(1000)]);
            }),
            ("exit.json unreadable", |_, m| {
                std::fs::write(path(m, 1, "exit"), b"{").unwrap()
            }),
            ("launcher.json unreadable", |_, m| {
                std::fs::write(path(m, 1, "launcher"), b"{").unwrap()
            }),
            ("launcher.json absent", |_, m| {
                std::fs::remove_file(path(m, 1, "launcher")).unwrap()
            }),
        ];
        for (case, break_one) in cases {
            let (f, ctx, _d, m) = world();
            publish(&path(&m, 1, "client"), &json!({"pid": 600, "start": 1500})).unwrap();
            assert!(
                client(&ctx, &m, 1).is_some(),
                "{case}: proven before the break"
            );
            break_one(&f, &m);
            assert_eq!(client(&ctx, &m, 1), None, "{case}");
        }
    }
}

#[cfg(test)]
#[path = "gens_publish_tests.rs"]
mod publish_tests;

#[cfg(test)]
#[path = "gens_ready_tests.rs"]
mod ready_tests;
