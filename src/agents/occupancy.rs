//! occupy-3: which live processes may hold a conversation (H1-H6). Fail closed: anything unreadable holds.

use crate::ctx::Ctx;
use crate::parse::Client;
use crate::proc::Row;
use crate::state::root::{HOST, OsKind};

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
    _own: Option<&Own>,
) -> Option<Hold> {
    if let Some(hold) = proc_hold(ctx) {
        return Some(hold); // calls 10, 20
    }
    let Ok(rows) = ctx.os.table() else {
        return Some(Hold::machine(Rule::Table));
    };
    rows.iter()
        .find_map(|row| holds(ctx.os.as_ref(), row, session, kind, os))
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

#[cfg(test)]
#[path = "occupancy_tests.rs"]
mod tests;
