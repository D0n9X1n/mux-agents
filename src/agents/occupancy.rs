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
    /// H4.4: an unmanaged client of the kind whose own newest report names the conversation.
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
            Rule::Unmanaged => {
                "it is a client this team did not start, and its hook last reported this conversation"
            }
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
    /// o3 call 34 N1: the (pid, start) the proof established for this client's registry reports (a Copilot npm
    /// client's loader), fixed at proof time; `None` when it could not be established.
    pub key: Option<(u32, i64)>,
}

/// Call 7: from the proof only. A member standing (an hc member, or a candidate re-proven under the lock) gives its
/// `tree::Member` client (pid, start); with no standing, a pane caller's proven agent; anything else nothing.
/// `caller.chain` is never read.
pub fn own_of(ctx: &Ctx, caller: &Caller, standing: Option<&Standing>) -> Option<Own> {
    match standing {
        // client.json names the client the launcher recorded: for a Copilot npm client, its loader (proof-0b K1a).
        Some(Standing::Member(m)) => Some(Own {
            kind: m.client,
            pid: m.pid,
            start: m.start,
            key: Some((m.pid, m.start)),
        }),
        Some(Standing::Person) => None,
        None => {
            let agent = caller.agent.as_ref()?;
            let kind = caller.client?;
            // The key report-2b writes under, read now while the proof's rows are live (`ancestry::identity`).
            let key = ctx
                .os
                .row(agent.pid)
                .filter(|r| r.start.is_some_and(|s| ctx.os.same_start(s, agent.start)))
                .and_then(|r| crate::agents::ancestry::identity(ctx, &r, kind));
            Some(Own {
                kind,
                pid: agent.pid,
                start: agent.start,
                key,
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
    // QA Q2 (call 31): the caller's pid listed without a readable start: its identity, and so the key of its report,
    // cannot be resolved from this table read. No exemption, and its report is not skipped: it holds as Evidence.
    if let (Some(o), None) = (own, exempted.as_ref())
        && let Some(r) = rows.iter().find(|r| r.pid == o.pid && r.start.is_none())
    {
        return Some(Hold::of(Rule::Evidence, r));
    }
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
    // Call 29: a managed client gone since the saved table still holds; its surviving partner row is named first.
    let mut gone: Option<Hold> = None;
    for row in &rows {
        if let Some(m) = managed_of(ctx, &c, row, &rows, os) {
            let rule = match m.session.as_deref() {
                None => Rule::Evidence,                  // finding 8: the known holder, named
                Some(s) if s == wanted => Rule::Managed, // H4.2; its argv adds nothing
                Some(_) => continue,
            };
            let hold = Hold {
                member: Some(m.id.clone()),
                ..Hold::of(rule, row)
            };
            if m.gone && row.pid == m.pid {
                gone.get_or_insert(hold);
                continue;
            }
            return Some(hold);
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
        match holds(ctx.os.as_ref(), row, session, kind, os) {
            // H4.4: a live client of the kind this team did not start holds the conversation only when its own hook
            // last reported it (`report -Session`, on start and on every in-session `/resume`), or when that report
            // cannot be read. One that reported another conversation, or none, holds nothing: another Claude Code or
            // Copilot session open on the machine must not block every resume. Known limit: a client without this
            // plugin's hooks that switched into the conversation by hand is not seen.
            Some(hold) if hold.rule == Rule::Unmanaged => {
                if let Some(rule) = unmanaged_report(ctx, row, kind, &wanted) {
                    return Some(Hold::of(rule, row));
                }
            }
            Some(hold) => return Some(hold), // H4.3, H4.5
            None => {}
        }
    }
    gone.or_else(|| {
        c.reserved
            .get(&wanted)
            .map(|id| Hold::member(Rule::Reserved, id)) // H4.6
    })
}

/// Checklist 7 (call 16) test seam: runs in `effective` right before its second `launcher.json` read.
#[cfg(test)]
pub(crate) mod hooks {
    use std::cell::RefCell;
    type Hook = Box<dyn Fn()>;
    thread_local! {
        static BEFORE: RefCell<Option<Hook>> = const { RefCell::new(None) };
        static ENTRY: RefCell<Option<Hook>> = const { RefCell::new(None) };
        static KEY: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }
    /// Runs right before an unmanaged client's report key is resolved, after the table read that listed it.
    pub fn before_key(hook: Option<Hook>) {
        KEY.with(|b| *b.borrow_mut() = hook);
    }
    pub(super) fn fire_before_key() {
        KEY.with(|b| {
            if let Some(hook) = b.borrow().as_ref() {
                hook()
            }
        });
    }
    /// Call 29: runs right before each registry read occupancy makes (its own table read for liveness).
    pub fn before_entry_read(hook: Option<Hook>) {
        ENTRY.with(|b| *b.borrow_mut() = hook);
    }
    pub(super) fn fire_before_entry_read() {
        ENTRY.with(|b| {
            if let Some(hook) = b.borrow().as_ref() {
                hook()
            }
        });
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

/// Every registry read occupancy makes, through one seam (call 29): the entry, and whether its process is now gone.
fn registry(ctx: &Ctx, kind: Client, pid: u32, start: i64) -> Res<live::Found> {
    #[cfg(test)]
    hooks::fire_before_entry_read();
    live::lookup(ctx, kind, pid, start)
}

#[path = "occupancy_generations.rs"]
mod generations;
use generations::*;

#[path = "occupancy_caller.rs"]
mod caller_id;
use caller_id::*;

#[path = "occupancy_table.rs"]
mod table;
pub(crate) use table::occupies;
use table::proc_hold;
pub use table::{ProcView, client_image, holds, proc_view};

#[cfg(test)]
mod caller_tests;
#[cfg(test)]
mod managed_tests;
#[cfg(test)]
mod reserve_tests;
#[cfg(test)]
#[path = "occupancy_test_support.rs"]
mod support;
#[cfg(test)]
mod table_tests;
