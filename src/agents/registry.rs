//! The session's agent panes, their records and exit notices (MuxAgents.ps1:535-546, 1014-1018, 1150-1204,
//! 1287-1296).
//!
//! An agent is found by its pane and the exact command that pane started, never by its tags (#48): rmux 0.10.0's
//! rotate-window moves panes and leaves their `@agent-*` options behind, so a tag may sit on any pane.

use serde_json::{Map, Value, json};

use super::mail;
use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::error::{Res, refuse};

/// One pane of the caller's session as one `list-panes` read it; every field as the multiplexer printed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub pane: String,
    pub window: String,
    pub pid: String,
    // `#{pane_dead}` 1: its program ended and remain-on-exit kept the pane. Anything else is live.
    pub dead: bool,
    // `#{pane_start_command}`.
    pub command: String,
    // `@agent-id` and `@agent-name`: opaque, never a target.
    pub tag: String,
    pub name: String,
}

/// What makes a pane an agent's own launch: the exact command it started (record.json `startCommand`, or for a
/// 0.2.0-0.2.2 record the command of the pane proven to run its launcher) and, for such a record, that pid too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub command: String,
    pub pid: Option<u32>,
}

impl Launch {
    /// The multiplexer condition that holds on a pane while it is still this launch: `<launched>`.
    pub fn launched(&self) -> String {
        let command = format!("#{{==:#{{pane_start_command}},{}}}", escape(&self.command));
        match self.pid {
            None => command,
            Some(pid) => format!("#{{&&:{command},#{{==:#{{pane_pid}},{pid}}}}}"),
        }
    }

    /// `<launched>` on a live pane.
    pub fn live(&self) -> String {
        format!("#{{&&:#{{==:#{{pane_dead}},0}},{}}}", self.launched())
    }

    /// `<launched>` on a dead pane.
    pub fn dead(&self) -> String {
        format!("#{{&&:#{{pane_dead}},{}}}", self.launched())
    }
}

/// A literal inside a format conditional: `#` as `##`, `,` as `#,` and `}` as `#}`.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '#' => out.push_str("##"),
            ',' => out.push_str("#,"),
            '}' => out.push_str("#}"),
            c => out.push(c),
        }
    }
    out
}

/// `if-shell -F -t <pane> <condition> <command>`: the server checks and acts in one command.
pub fn bound(pane: &str, condition: String, command: String) -> Vec<String> {
    vec![
        "if-shell".into(),
        "-F".into(),
        "-t".into(),
        pane.into(),
        condition,
        command,
    ]
}

/// The class of an own record (spec D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Committed, no notice, its pane live and its own launch: the only addressable agent.
    Running,
    /// Its pane dead and its own launch: reaped only.
    DeadLaunch,
    /// Its pane live and its own launch, but uncommitted or already noticed: counted, never addressed or cleaned.
    LiveLaunch,
    /// Its pane is there but is not its launch (or a 0.2.x launch cannot be proven).
    Unproven,
    /// No pane at record.pane.
    Gone,
}

/// One own record: record.json naming its folder's id and the caller's session.
#[derive(Debug, Clone, PartialEq)]
pub struct Agent {
    pub id: String,
    pub record: Value,
    pub committed: bool,
    pub noticed: bool,
    pub class: Class,
    // The row at record.pane, if listed.
    pub row: Option<Row>,
    // Its launch, when the row is proven to be it.
    pub launch: Option<Launch>,
}

/// A running agent: its pane, as the snapshot read it, and its record.
#[derive(Debug, Clone, PartialEq)]
pub struct Pane {
    pub pane: String,
    pub window: String,
    pub pid: String,
    pub id: String,
    pub name: String,
    pub record: Value,
    pub launch: Launch,
}

/// The caller's pane as the snapshot sees it (spec D5).
#[derive(Debug, Clone, PartialEq)]
pub enum Own {
    /// A running agent's pane.
    Agent(Box<Pane>),
    /// An own record's pane in any other class.
    Unproven,
    None,
}

/// One `list-panes` of the session and every own record, classified (spec D2, D3).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub rows: Vec<Row>,
    // Own records, by id.
    pub agents: Vec<Agent>,
    // Tagged ids with no own record here (a spawn in progress, commit.json alone, or another state root), each once,
    // with the names their tags carry.
    pub reserved: Vec<(String, Vec<String>)>,
}

const FIELDS: [&str; 7] = [
    "#{pane_id}",
    "#{window_id}",
    "#{pane_pid}",
    "#{pane_dead}",
    "#{pane_start_command}",
    "#{@agent-id}",
    "#{@agent-name}",
];

/// The pane list format: every field framed by `marker`, so no field's text can split a field or form a row.
pub(crate) fn list_format(marker: &str) -> String {
    let mut format = marker.to_string();
    for field in FIELDS {
        format.push_str(field);
        format.push_str(marker);
    }
    format
}

const GONE_PROBE: &str = "#{pane_id}|#{@agent-id}";

/// Every pane of the caller's session, one `list-panes` call. The output is split by the call's fresh random marker:
/// a field may hold `|`, quotes, CR or LF and is still one field. Output in any other shape refuses; a row whose pane
/// is not a pane id is skipped.
pub(crate) fn rows(ctx: &Ctx, caller: &Caller) -> Res<Vec<Row>> {
    let marker = crate::util::guid()?.replace('-', "");
    let format = list_format(&marker);
    let listed = crate::mux::native(
        ctx,
        &["list-panes", "-s", "-t", &caller.session, "-F", &format],
        false,
    )?;
    parse_rows(&listed.stdout, &marker).map_or_else(
        || refuse(1, "rmux", "the pane list is not in its format"),
        Ok,
    )
}

/// The rows of a `list_format(marker)` output: an empty text before the first marker, then per row seven fields and
/// the line end between rows.
fn parse_rows(stdout: &str, marker: &str) -> Option<Vec<Row>> {
    if stdout.trim_matches(['\r', '\n']).is_empty() {
        return Some(Vec::new());
    }
    let tokens: Vec<&str> = stdout.split(marker).collect();
    let (first, rest) = tokens.split_first()?;
    if !first.is_empty() {
        return None;
    }
    let (chunks, left) = rest.as_chunks::<8>();
    if !left.is_empty() {
        return None;
    }
    let mut rows = Vec::new();
    for [pane, window, pid, dead, command, tag, name, end] in chunks {
        if !end.trim_matches(['\r', '\n']).is_empty() {
            return None;
        }
        if !super::pane_id(pane) {
            continue;
        }
        rows.push(Row {
            pane: pane.to_string(),
            window: window.to_string(),
            pid: pid.to_string(),
            dead: *dead == "1",
            command: command.to_string(),
            tag: tag.to_string(),
            name: name.to_string(),
        });
    }
    Some(rows)
}

/// A live pane's pid: a decimal of at least 2 (never the kernel's or init's tree).
fn live_pid(pid: &str) -> Option<u32> {
    (!pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
        .then(|| pid.parse::<u32>().ok())
        .flatten()
        .filter(|&pid| pid >= 2)
}

fn window_ok(window: &str) -> bool {
    window
        .strip_prefix('@')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

impl Snapshot {
    /// One `list-panes` and the state folder's own records, classified.
    pub fn take(ctx: &Ctx, caller: &Caller) -> Res<Snapshot> {
        let rows = rows(ctx, caller)?;
        let agents_dir = super::server_dir(ctx, caller).join("agents");
        let mut agents = Vec::new();
        let entries = match std::fs::read_dir(&agents_dir) {
            Ok(entries) => Some(entries),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        for entry in entries.into_iter().flatten() {
            let entry = entry?;
            let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let folder = entry.path();
            if !super::agent_id(&id) || !folder.is_dir() {
                continue;
            }
            // An unreadable record is no own record: its tag, if any, is only a reservation.
            let Some(record) = own_record(ctx, caller, &id).ok().flatten() else {
                continue;
            };
            let committed = folder.join("commit.json").is_file();
            let noticed = folder.join("notice.json").exists();
            let pane = super::text(record.get("pane"));
            let row = rows.iter().find(|r| r.pane == pane).cloned();
            let launch = row
                .as_ref()
                .and_then(|row| launch_of(ctx, caller, &id, &record, row));
            let class = classify(row.as_ref(), launch.is_some(), committed, noticed);
            agents.push(Agent {
                id,
                record,
                committed,
                noticed,
                class,
                row,
                launch,
            });
        }
        agents.sort_by(|a, b| a.id.cmp(&b.id));
        let mut reserved: Vec<(String, Vec<String>)> = Vec::new();
        for row in &rows {
            if !super::agent_id(&row.tag) || agents.iter().any(|a| a.id == row.tag) {
                continue;
            }
            match reserved.iter_mut().find(|(id, _)| *id == row.tag) {
                Some((_, names)) => names.push(row.name.clone()),
                None => reserved.push((row.tag.clone(), vec![row.name.clone()])),
            }
        }
        Ok(Snapshot {
            rows,
            agents,
            reserved,
        })
    }

    /// The running agents in pane-list order.
    pub fn running(&self) -> Vec<Pane> {
        self.rows
            .iter()
            .filter_map(|row| {
                self.agents
                    .iter()
                    .find(|a| a.class == Class::Running && a.row.as_ref() == Some(row))
            })
            .filter_map(pane_of)
            .collect()
    }

    /// The running agent named `name`.
    pub fn find(&self, name: &str) -> Option<Pane> {
        self.running().into_iter().find(|p| p.name == name)
    }

    /// The running agent whose pane is `pane`.
    pub fn at(&self, pane: &str) -> Option<Pane> {
        self.running().into_iter().find(|p| p.pane == pane)
    }

    /// The caller's pane: a running agent's, another own record's, or neither.
    pub fn own(&self, caller: &Caller) -> Own {
        let mine: Vec<&Agent> = self
            .agents
            .iter()
            .filter(|a| super::text(a.record.get("pane")) == caller.pane)
            .collect();
        match mine[..] {
            [] => Own::None,
            [agent] => pane_of(agent).map_or(Own::Unproven, |p| Own::Agent(Box::new(p))),
            _ => Own::Unproven,
        }
    }

    /// What the cap counts: every own record whose pane is there, and every reservation, each id once.
    pub fn count(&self) -> usize {
        self.agents
            .iter()
            .filter(|a| a.class != Class::Gone)
            .count()
            + self.reserved.len()
    }

    /// The names reservations carry: taken, but never an agent's.
    pub fn reserved_names(&self) -> Vec<String> {
        self.reserved
            .iter()
            .flat_map(|(_, names)| names.iter().cloned())
            .collect()
    }
}

/// A running agent as a `Pane`; none in any other class.
fn pane_of(agent: &Agent) -> Option<Pane> {
    let (row, launch) = (agent.row.as_ref()?, agent.launch.clone()?);
    (agent.class == Class::Running).then(|| Pane {
        pane: row.pane.clone(),
        window: row.window.clone(),
        pid: row.pid.clone(),
        id: agent.id.clone(),
        name: super::text(agent.record.get("name")),
        record: agent.record.clone(),
        launch,
    })
}

/// D3 from the row at record.pane and whether it is proven to be the launch.
fn classify(row: Option<&Row>, launched: bool, committed: bool, noticed: bool) -> Class {
    let Some(row) = row else {
        return Class::Gone;
    };
    if !launched {
        return Class::Unproven;
    }
    if row.dead {
        return Class::DeadLaunch;
    }
    if committed && !noticed && live_pid(&row.pid).is_some() && window_ok(&row.window) {
        Class::Running
    } else {
        Class::LiveLaunch
    }
}

/// The launch of record `id` at `row`, when the row is it. A record with `startCommand` is the row whose start
/// command equals it. A 0.2.0-0.2.2 record (no `startCommand`) is a live row proven to run its launcher (D1b); the
/// row's own command then stands for it, bound to that pid. Anything else (an empty or non-text startCommand, a dead
/// 0.2.x pane) is not.
fn launch_of(ctx: &Ctx, caller: &Caller, id: &str, record: &Value, row: &Row) -> Option<Launch> {
    match record.get("startCommand") {
        Some(Value::String(command)) if !command.is_empty() => {
            (row.command == *command).then(|| Launch {
                command: command.clone(),
                pid: None,
            })
        }
        Some(_) => None,
        None => {
            let pid = live_pid(&row.pid).filter(|_| !row.dead && !row.command.is_empty())?;
            proven_launcher(ctx, caller, id, pid).then(|| Launch {
                command: row.command.clone(),
                pid: Some(pid),
            })
        }
    }
}

/// Whether `pid` is agent `id`'s launcher: launcher.json's pid, with its start, and an argv ending
/// `launcher <its spec path>` (any executable path).
pub(crate) fn proven_launcher(ctx: &Ctx, caller: &Caller, id: &str, pid: u32) -> bool {
    let folder = super::server_dir(ctx, caller).join("agents").join(id);
    let recorded = crate::state::fs::read_json(&folder.join("launcher.json"))
        .ok()
        .flatten()
        .and_then(|l| Some((l.get("pid")?.as_u64()?, l.get("start")?.as_i64()?)));
    let Some((recorded_pid, start)) = recorded else {
        return false;
    };
    if recorded_pid != u64::from(pid) {
        return false;
    }
    let Some(spec) = folder.join("spec.json").to_str().map(str::to_string) else {
        return false;
    };
    ctx.os.row(pid).is_some_and(|row| {
        let argv_ok = row.argv.as_ref().is_some_and(|argv| {
            argv.len() >= 3 && argv[argv.len() - 2] == "launcher" && argv[argv.len() - 1] == spec
        });
        argv_ok
            && row
                .start
                .is_some_and(|listed| ctx.os.same_start(listed, start))
    })
}

/// record.json of a well-formed id; `None` when missing or not an object.
pub fn record(ctx: &Ctx, caller: &Caller, id: &str) -> Res<Option<Value>> {
    if !super::agent_id(id) {
        return Ok(None);
    }
    let path = super::server_dir(ctx, caller)
        .join("agents")
        .join(id)
        .join("record.json");
    Ok(crate::state::fs::read_json(&path)?.filter(Value::is_object))
}

/// The name of every agent record of the caller's session under this state root, live or ended, any age: a name
/// is never reused in its session. A folder with no record.json, or a record naming another session, names none.
/// It fails closed: a record that cannot be read, is not an object, is not its folder's, names no session or has
/// no name could hide a taken name, so it refuses 2 `name`.
pub(crate) fn recorded_names(ctx: &Ctx, caller: &Caller) -> Res<Vec<String>> {
    let agents = super::server_dir(ctx, caller).join("agents");
    let entries = match std::fs::read_dir(&agents) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !super::agent_id(&id) || !entry.path().is_dir() {
            continue;
        }
        let unreadable = || {
            refuse(
                2,
                "name",
                format!("agent record {id} cannot be read, so names cannot be checked"),
            )
        };
        let record = match crate::state::fs::read_json(&entry.path().join("record.json")) {
            Ok(None) => continue,
            Ok(Some(record)) if record.is_object() => record,
            _ => return unreadable(),
        };
        if record.get("id").and_then(Value::as_str) != Some(id.as_str()) {
            return unreadable();
        }
        // Skipped only when it positively names another session: one naming none could be this session's.
        match record.get("session").and_then(Value::as_str) {
            Some(session) if session == caller.session => {}
            Some(_) => continue,
            None => return unreadable(),
        }
        match record.get("name").and_then(Value::as_str) {
            Some(name) => names.push(name.to_string()),
            None => return unreadable(),
        }
    }
    Ok(names)
}

fn no_agent<T>() -> Res<T> {
    refuse(2, "target", "no live agent has that name")
}

/// The running agent named `name` in the caller's session; refuses 2 `target` otherwise. A malformed name refuses
/// before any multiplexer call.
pub fn find_pane(ctx: &Ctx, caller: &Caller, name: &str) -> Res<Pane> {
    if !super::agent_name(name) {
        return no_agent();
    }
    Snapshot::take(ctx, caller)?
        .find(name)
        .map_or_else(no_agent, Ok)
}

/// A snapshot after a reap: one list, and a second only when the reap killed a pane or posted a notice.
pub fn live_after_reap(ctx: &Ctx, caller: &Caller) -> Res<Snapshot> {
    let mut snapshot = Snapshot::take(ctx, caller)?;
    if reap(ctx, caller, Some(&snapshot), false)? {
        snapshot = Snapshot::take(ctx, caller)?;
    }
    Ok(snapshot)
}

/// As `find_pane`, after a reap.
pub fn find_reaped(ctx: &Ctx, caller: &Caller, name: &str) -> Res<Pane> {
    if !super::agent_name(name) {
        return no_agent();
    }
    live_after_reap(ctx, caller)?
        .find(name)
        .map_or_else(no_agent, Ok)
}

/// Refuses 2 `tagged` unless the caller's pane carries no agent tag and is no own record's pane.
pub fn untagged(ctx: &Ctx, caller: &Caller, message: &str) -> Res<()> {
    if caller.agent_tag.is_empty() && Snapshot::take(ctx, caller)?.own(caller) == Own::None {
        return Ok(());
    }
    refuse(2, "tagged", message)
}

/// The caller's name and mailbox key: a running agent's pane is its record's name and id; any other pane is root.
#[derive(Debug, Clone, PartialEq)]
pub struct Me {
    pub name: String,
    pub key: String,
    pub record: Option<Value>,
}

/// The caller as `snapshot` sees its pane: a running agent is itself; an own record's pane in any other class, or a
/// pane carrying an agent tag that is no running agent's, refuses 2 `parent`; any other pane is root.
pub fn self_of(caller: &Caller, snapshot: &Snapshot) -> Res<Me> {
    match snapshot.own(caller) {
        Own::Agent(pane) => Ok(Me {
            name: pane.name,
            key: pane.id,
            record: Some(pane.record),
        }),
        Own::Unproven => refuse(
            2,
            "parent",
            "this pane's agent cannot be proven to run here",
        ),
        Own::None if !caller.agent_tag.is_empty() => {
            refuse(2, "parent", "this agent pane has no record")
        }
        Own::None => Ok(Me {
            name: "root".into(),
            key: mail::mailbox_key(&caller.pane, "")?,
            record: None,
        }),
    }
}

/// The mailbox of `pane`: the running agent's id there, else `pane-<n>`.
pub fn mailbox_of(snapshot: &Snapshot, pane: &str) -> Res<String> {
    match snapshot.at(pane) {
        Some(agent) => Ok(agent.id),
        None => mail::mailbox_key(pane, ""),
    }
}

/// An agent's one exit notice.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    Exited(Value),
    Stopped,
    Lost,
}

/// Posts agent `id`'s one exit notice to its parent pane's mailbox; false when already posted. The caller holds
/// the notice lock. The message goes first and notice.json last, so a failed post is retried by the next list,
/// read or spawn; the fixed message id keeps a retry after a failed marker write single.
pub fn add_notice(
    ctx: &Ctx,
    caller: &Caller,
    id: &str,
    record: &Value,
    notice: Notice,
) -> Res<bool> {
    let marker = super::server_dir(ctx, caller)
        .join("agents")
        .join(id)
        .join("notice.json");
    if marker.exists() {
        return Ok(false);
    }
    let name = super::text(record.get("name"));
    let (kind, code, body) = match notice {
        Notice::Exited(code) => {
            let body = format!("{name} exited with code {}.", super::text(Some(&code)));
            ("exited", code, body)
        }
        Notice::Stopped => ("stopped", Value::Null, format!("{name} was stopped.")),
        Notice::Lost => (
            "lost",
            Value::Null,
            format!("{name} is gone: its pane closed without an exit record."),
        ),
    };
    let key = mail::mailbox_key(&super::text(record.get("parentPane")), "")?;
    let pane = super::text(record.get("pane"));
    let mut fields = Map::new();
    fields.insert("kind".into(), kind.into());
    fields.insert("exitCode".into(), code.clone());
    let delivered = mail::add_once(
        ctx,
        caller,
        &key,
        &name,
        &pane,
        &body,
        &format!("exit-{id}"),
        &fields,
    )?;
    // The first notice wins: a message already delivered (a marker write that failed after it) decides the marker.
    let (kind, code) = match delivered {
        Some(message) => delivered_notice(&message, &name),
        None => (kind.to_string(), code),
    };
    let at = crate::util::utc_o(ctx.clock.now());
    let mut written = json!({"kind": kind, "exitCode": code, "at": at});
    if kind == "lost" {
        written["closedBy"] = "pane".into();
    }
    crate::state::fs::write_json(&marker, &written)?;
    Ok(true)
}

/// The (kind, exitCode) of a delivered exit message: its `kind` and `exitCode`, else (a 0.2.1 message) its fixed
/// body `<name> exited with code <N>.`, `<name> was stopped.` or `<name> is gone: ...`; anything else is lost.
fn delivered_notice(message: &Value, name: &str) -> (String, Value) {
    let code = message.get("exitCode").filter(|c| c.is_i64());
    match message.get("kind").and_then(Value::as_str) {
        Some("exited") if code.is_some() => {
            return ("exited".into(), code.cloned().unwrap_or(Value::Null));
        }
        Some(kind @ ("stopped" | "lost")) => return (kind.into(), Value::Null),
        _ => {}
    }
    let body = super::text(message.get("body"));
    let rest = body.strip_prefix(name).unwrap_or_default();
    if rest == " was stopped." {
        return ("stopped".into(), Value::Null);
    }
    let exited = rest
        .strip_prefix(" exited with code ")
        .and_then(|n| n.strip_suffix('.'))
        .and_then(|n| n.parse::<i64>().ok());
    match exited {
        Some(n) => ("exited".into(), n.into()),
        None => ("lost".into(), Value::Null),
    }
}

/// An uncommitted agent is closed only after the launcher's 30 s commit wait and 30 s more.
const UNCOMMITTED_MAX: std::time::Duration = std::time::Duration::from_secs(60);

/// The one server-side command that kills `pane` only while it is dead and still `launch`: the check and the kill
/// run in one command, so a respawn between them keeps the pane. Tags play no part.
pub fn kill_if_dead(pane: &str, launch: &Launch) -> Vec<String> {
    bound(pane, launch.dead(), format!("kill-pane -t {pane}"))
}

fn run(ctx: &Ctx, argv: &[String]) -> Res<crate::mux::Output> {
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    crate::mux::native(ctx, &argv, true)
}

/// Closes the caller session's agents whose pane is gone, with one notice each, and their dead launches. True when
/// it killed a pane or posted a notice: the caller then takes a new snapshot.
///
/// - A committed agent's dead launch (noticed or not) is killed by `kill_if_dead` under its notice lock, and an agent
///   without a notice is then noticed as below.
/// - A committed agent without a notice whose pane is gone, which a fresh probe confirms: `exited` with
///   launcher.json's code, else `lost`.
/// - An uncommitted agent created over 60 s ago, likewise (its dead launch killed, then noticed unless it is), but
///   only under the session's spawn lock (taken here unless `spawn_lock_held`, as inside spawn) and then its notice
///   lock, and only while it is still uncommitted and still that old. Lock order: spawn, then notice. Outside spawn
///   the spawn lock is tried once, never waited for.
/// - A live launch, an unproven pane and any pane that is no own record's are never touched.
///
/// `snapshot` is one the caller already has; without one, panes are listed only when some agent could need a
/// notice or its launcher recorded an exit (its pane may be dead). A pid is never signalled.
pub fn reap(
    ctx: &Ctx,
    caller: &Caller,
    snapshot: Option<&Snapshot>,
    spawn_lock_held: bool,
) -> Res<bool> {
    let agents_dir = super::server_dir(ctx, caller).join("agents");
    if snapshot.is_none() && !worth_listing(ctx, caller)? {
        return Ok(false);
    }
    let taken;
    let snapshot = match snapshot {
        Some(snapshot) => snapshot,
        None => {
            taken = Snapshot::take(ctx, caller)?;
            &taken
        }
    };
    let mut acted = false;
    // Committed: dead launches in pane-list order, then gone agents without a notice by id.
    let dead = snapshot.rows.iter().filter_map(|row| {
        snapshot
            .agents
            .iter()
            .find(|a| a.committed && a.class == Class::DeadLaunch && a.row.as_ref() == Some(row))
    });
    for agent in dead {
        let (Some(row), Some(launch)) = (&agent.row, &agent.launch) else {
            continue;
        };
        super::lock(ctx, &super::lock_key("notice", caller, &agent.id), || {
            run(ctx, &kill_if_dead(&row.pane, launch))?;
            post_if_gone(ctx, caller, &agent.id, &agent.record)
        })?;
        acted = true;
    }
    for agent in snapshot
        .agents
        .iter()
        .filter(|a| a.committed && !a.noticed && a.class == Class::Gone)
    {
        // The snapshot may predate a spawn that committed since: the fresh probe decides.
        acted |= super::lock(ctx, &super::lock_key("notice", caller, &agent.id), || {
            post_if_gone(ctx, caller, &agent.id, &agent.record)
        })?;
    }
    // A stale uncommitted agent: its dead launch is killed, and one gone without a notice is probed.
    let candidates: Vec<&Agent> = snapshot
        .agents
        .iter()
        .filter(|a| {
            !a.committed
                && (a.class == Class::DeadLaunch || (a.class == Class::Gone && !a.noticed))
                && stale(ctx, caller, &a.id)
        })
        .collect();
    if !candidates.is_empty() {
        let close = || -> Res<bool> {
            let mut did = false;
            for agent in &candidates {
                did |= super::lock(ctx, &super::lock_key("notice", caller, &agent.id), || {
                    // A commit that landed while reap waited for the locks makes it a live agent's; the age is
                    // checked again under them.
                    if agents_dir.join(&agent.id).join("commit.json").exists()
                        || !stale(ctx, caller, &agent.id)
                    {
                        return Ok(false);
                    }
                    let mut killed = false;
                    if let (Class::DeadLaunch, Some(row), Some(launch)) =
                        (agent.class, &agent.row, &agent.launch)
                    {
                        run(ctx, &kill_if_dead(&row.pane, launch))?;
                        killed = true;
                    }
                    Ok(post_if_gone(ctx, caller, &agent.id, &agent.record)? || killed)
                })?;
            }
            Ok(did)
        };
        // Locks are not reentrant: inside spawn, which holds the spawn lock, it is never taken again. Elsewhere it is
        // tried once, never waited for: another spawn holding it is a launch in progress, so its candidates wait for
        // a later trigger and list, read and the side bar return at once.
        acted |= if spawn_lock_held {
            close()?
        } else {
            super::try_lock(
                ctx,
                &super::lock_key("spawn", caller, &caller.session),
                close,
            )?
            .unwrap_or(false)
        };
    }
    Ok(acted)
}

/// Without a snapshot, panes are worth listing only when a committed agent has no notice, a stale uncommitted one
/// has none, or a launcher recorded an exit (its pane may be dead).
fn worth_listing(ctx: &Ctx, caller: &Caller) -> Res<bool> {
    let agents = super::server_dir(ctx, caller).join("agents");
    let entries = match std::fs::read_dir(&agents) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let folder = entry.path();
        if !super::agent_id(&id) || !folder.is_dir() {
            continue;
        }
        let noticed = folder.join("notice.json").exists();
        let committed = folder.join("commit.json").is_file();
        if !noticed && (committed || stale(ctx, caller, &id)) {
            return Ok(true);
        }
        let exited = crate::state::fs::read_json(&folder.join("launcher.json"))
            .ok()
            .flatten()
            .and_then(|l| l.get("exitCode").filter(|c| !c.is_null()).cloned())
            .is_some();
        if exited {
            return Ok(true);
        }
    }
    Ok(false)
}

/// record.json of agent `id` when it names that id and the caller's session.
fn own_record(ctx: &Ctx, caller: &Caller, id: &str) -> Res<Option<Value>> {
    Ok(record(ctx, caller, id)?.filter(|record| {
        record.get("id").and_then(Value::as_str) == Some(id)
            && record.get("session").and_then(Value::as_str) == Some(caller.session.as_str())
    }))
}

/// Whether agent `id`'s record was created over 60 s ago; an unreadable record or time is not.
fn stale(ctx: &Ctx, caller: &Caller, id: &str) -> bool {
    record(ctx, caller, id)
        .ok()
        .flatten()
        .and_then(|r| {
            r.get("createdAt")
                .and_then(Value::as_str)
                .and_then(crate::util::parse_utc)
        })
        .and_then(|created| ctx.clock.now().duration_since(created).ok())
        .is_some_and(|age| age > UNCOMMITTED_MAX)
}

/// Whether `pane` is gone: only exit 0 and empty fields prove it; a live pane or a failed probe is no proof.
pub(crate) fn pane_gone(ctx: &Ctx, pane: &str) -> Res<bool> {
    let probe = crate::mux::native(
        ctx,
        &["display-message", "-p", "-t", pane, GONE_PROBE],
        true,
    )?;
    Ok(probe.code == 0 && probe.stdout.trim_end_matches(['\r', '\n']) == "|")
}

/// Under agent `id`'s notice lock: when it has no notice and a fresh probe shows its pane gone, posts `exited` with
/// launcher.json's code, else `lost`. True when it posted.
pub(crate) fn post_if_gone(ctx: &Ctx, caller: &Caller, id: &str, record: &Value) -> Res<bool> {
    let folder = super::server_dir(ctx, caller).join("agents").join(id);
    if folder.join("notice.json").exists() || !pane_gone(ctx, &super::text(record.get("pane")))? {
        return Ok(false);
    }
    let launcher = crate::state::fs::read_json(&folder.join("launcher.json"))?;
    let notice = match launcher.as_ref().and_then(|l| l.get("exitCode")) {
        Some(code) if !code.is_null() => Notice::Exited(code.clone()),
        _ => Notice::Lost,
    };
    add_notice(ctx, caller, id, record, notice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::fixture::{ID, Listed, World, launch_command, list_call, write};
    use crate::agents::mail::read_mail;
    use crate::agents::{lock_key, text};
    use serde_json::json;

    fn refusal<T: std::fmt::Debug>(result: Res<T>) -> String {
        let r = result.unwrap_err();
        format!("{} {} {}", r.code, r.check, r.message)
    }

    fn snapshot(w: &World) -> Snapshot {
        Snapshot::take(&w.ctx, &w.caller()).unwrap()
    }

    fn classes(s: &Snapshot) -> Vec<(String, Class)> {
        s.agents.iter().map(|a| (a.id.clone(), a.class)).collect()
    }

    fn listed_row(pane: &str, pid: &str, command: &str, tag: &str, name: &str) -> Listed {
        Listed {
            pane: pane.into(),
            window: "@5".into(),
            pid: pid.into(),
            dead: false,
            command: command.into(),
            tag: tag.into(),
            name: name.into(),
        }
    }

    #[test]
    fn snapshot_lists_once_and_classifies_by_pane_and_launch_never_tags() {
        // Nothing is made while listing; a tagged pane with no record here is only a reservation.
        let w = World::human("");
        w.panes(&["%2|@1|300|0123456789ab|claude-1"]);
        let s = snapshot(&w);
        assert!(s.running().is_empty() && s.agents.is_empty());
        assert_eq!(
            s.reserved,
            [("0123456789ab".to_string(), vec!["claude-1".to_string()])]
        );
        assert_eq!(s.count(), 1);
        assert!(!w.ctx.state_root.exists());

        w.agent_record("0123456789ab", json!({"pane": "%2", "name": "one"}));
        w.uncommitted("abcdef012345", json!({"pane": "%3"}));
        // A folder named commit.json does not commit its agent.
        let odd = w.uncommitted("fedcba987654", json!({"pane": "%6"}));
        std::fs::create_dir(odd.join("commit.json")).unwrap();
        let noticed = w.agent_record("111111111111", json!({"pane": "%4"}));
        write(&noticed.join("notice.json"), json!({"kind": "exited"}));
        w.agent_record("222222222222", json!({"pane": "%5"}));
        w.agent_record("333333333333", json!({"pane": "%40"}));
        // Another session's record is no own record: its tag is a reservation.
        w.agent_record("444444444444", json!({"pane": "%7", "session": "$4"}));
        // Tags rotated away from their panes, missing, malformed, and repeated.
        let rows = [
            "%2|@1|300|abcdef012345|claude-2",
            "%3|@1|310||",
            "%4|@2|320|0123456789AB|bad",
            "%5|@2|330|0123456789ab|one",
            "%6|@2|340|0123456789abc|long",
            "%7|@2|350|444444444444|theirs",
            "%8|@3|360|555555555555|a|b",
            "%9|@3|370|555555555555|dup",
        ];
        w.panes_dead(&rows, &["%5"]);
        let caller = w.caller();
        w.fakes.clear_log();
        let s = Snapshot::take(&w.ctx, &caller).unwrap();
        let calls = w.fakes.mux_calls();
        assert_eq!(calls.len(), 1);
        assert!(list_call(&calls[0]), "{calls:?}");
        assert_eq!(
            classes(&s),
            [
                ("0123456789ab".to_string(), Class::Running),
                ("111111111111".to_string(), Class::LiveLaunch),
                ("222222222222".to_string(), Class::DeadLaunch),
                ("333333333333".to_string(), Class::Gone),
                ("abcdef012345".to_string(), Class::LiveLaunch),
                ("fedcba987654".to_string(), Class::LiveLaunch),
            ]
        );
        // The running agent is its pane's and its record's, whatever tag sits on that pane.
        let running = s.running();
        assert_eq!(running.len(), 1);
        assert_eq!(
            (
                running[0].pane.as_str(),
                running[0].id.as_str(),
                running[0].name.as_str()
            ),
            ("%2", "0123456789ab", "one")
        );
        assert_eq!(s.find("one").unwrap().pane, "%2");
        for name in ["claude-2", "theirs", "dup", "worker"] {
            assert!(s.find(name).is_none(), "{name}");
        }
        // Each reservation once, with every name its tags carry.
        assert_eq!(
            s.reserved,
            [
                ("444444444444".to_string(), vec!["theirs".to_string()]),
                (
                    "555555555555".to_string(),
                    vec!["a|b".to_string(), "dup".to_string()]
                ),
            ]
        );
        assert_eq!(s.reserved_names(), ["theirs", "a|b", "dup"]);
        // Running, live launches, the dead launch and two reservations: each id once, the gone agent not.
        assert_eq!(s.count(), 7);
        // A failed list refuses.
        w.fakes.mux_first("list-panes", 1, "", "SECRET");
        let refused = Snapshot::take(&w.ctx, &w.caller()).unwrap_err();
        assert_eq!((refused.code, refused.check), (1, "rmux"));
    }

    #[test]
    fn rows_are_framed_field_by_field() {
        let w = World::human("");
        let caller = w.caller();
        // Every field may hold `|`, quotes, a backslash, CR or LF: none splits a field or forms a row.
        let command = "\"/a b/mux-agent\" launcher '/s|t\"a\\te/%8|@5|300|x\n%9/spec.json'";
        let forged = "evil\n%8|@5|320|ffffffffffff|forged\r\n";
        w.pane_rows(&[
            listed_row("%2", "300", command, "aaaaaaaaaaaa", forged),
            listed_row("%3", "x3", "c\rd", "", "cr\rname"),
        ]);
        let got = super::rows(&w.ctx, &caller).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(
            (got[0].pane.as_str(), got[0].command.as_str()),
            ("%2", command)
        );
        assert_eq!(got[0].name, forged);
        assert_eq!(
            (got[1].pid.as_str(), got[1].command.as_str()),
            ("x3", "c\rd")
        );
        // A record whose startCommand is that text is that pane's launch.
        w.agent_record(ID, json!({"pane": "%2", "startCommand": command}));
        let s = snapshot(&w);
        assert_eq!(classes(&s), [(ID.to_string(), Class::Running)]);
        assert_eq!(s.running()[0].launch.command, command);
        // Each call frames its fields with a fresh marker, so no stored text can know it.
        let lists: Vec<Vec<String>> = w
            .fakes
            .mux_calls()
            .into_iter()
            .filter(|c| list_call(c))
            .collect();
        let n = lists.len();
        assert!(n >= 2);
        assert_ne!(lists[n - 1][6], lists[n - 2][6]);
        // Output in any other shape refuses; nothing is no rows.
        for out in ["garbage\n", "%2|@1|300|aaaaaaaaaaaa|x\n"] {
            w.fakes.mux_first("list-panes", 0, out, "");
            let refused = rows(&w.ctx, &caller).unwrap_err();
            assert_eq!((refused.code, refused.check), (1, "rmux"), "{out:?}");
        }
        w.fakes.mux_first("list-panes", 0, "\n", "");
        assert!(rows(&w.ctx, &caller).unwrap().is_empty());
        // A row whose pane is not a pane id is skipped.
        assert_eq!(
            parse_rows("Mp4Mw4Mp4M0McMtMnM\nM%5M@1M1M0McMtMnM", "M")
                .unwrap()
                .into_iter()
                .map(|r| r.pane)
                .collect::<Vec<_>>(),
            ["%5"]
        );
        assert_eq!(parse_rows("xM%5M@1M1M0McMtMnM", "M"), None);
    }

    #[test]
    fn launched_condition_escapes_the_command() {
        assert_eq!(escape("a,b#c}d{e"), "a#,b##c#}d{e");
        let launch = Launch {
            command: "x,y}#".into(),
            pid: None,
        };
        assert_eq!(launch.launched(), "#{==:#{pane_start_command},x#,y#}##}");
        assert_eq!(
            launch.live(),
            "#{&&:#{==:#{pane_dead},0},#{==:#{pane_start_command},x#,y#}##}}"
        );
        assert_eq!(
            launch.dead(),
            "#{&&:#{pane_dead},#{==:#{pane_start_command},x#,y#}##}}"
        );
        let legacy = Launch {
            command: "c".into(),
            pid: Some(300),
        };
        assert_eq!(
            legacy.launched(),
            "#{&&:#{==:#{pane_start_command},c},#{==:#{pane_pid},300}}"
        );
    }

    #[test]
    fn replacements_are_unproven_counted_never_addressed() {
        let spec = launch_command("%9");
        // The record's own pane, running something else: a pager on its spec, another agent's launch, a near match.
        for command in [
            "less /state/agents/9/spec.json".to_string(),
            "/bin/mux-agent launcher /state/agents/8/spec.json".to_string(),
            format!("{spec} "),
            format!("{spec}*"),
            spec.replace("launcher", "launche?"),
            String::new(),
        ] {
            let w = World::human("");
            w.agent_record(ID, json!({"pane": "%9"}));
            w.pane_rows(&[listed_row("%9", "300", &command, ID, "worker")]);
            let s = snapshot(&w);
            assert_eq!(
                classes(&s),
                [(ID.to_string(), Class::Unproven)],
                "{command:?}"
            );
            assert!(s.running().is_empty(), "{command:?}");
            assert_eq!(s.count(), 1, "{command:?}");
            assert_eq!(
                refusal(find_pane(&w.ctx, &w.caller(), "worker")),
                "2 target no live agent has that name",
                "{command:?}"
            );
        }
        // A record whose startCommand is empty or not text is never launched.
        for start in [json!(""), json!(5), Value::Null] {
            let w = World::human("");
            w.agent_record(ID, json!({"pane": "%9", "startCommand": start}));
            w.pane_rows(&[listed_row("%9", "300", "", ID, "worker")]);
            assert_eq!(classes(&snapshot(&w)), [(ID.to_string(), Class::Unproven)]);
        }
    }

    // A 0.2.0-0.2.2 record (no startCommand) of agent ID at %9, its launcher.json pid 300 start 3000, and process
    // 300 as `argv`.
    fn legacy_world(argv: &[&str], start: i64) -> World {
        let w = World::human("");
        let folder = w.agent_record(ID, json!({"pane": "%9"}));
        w.legacy(ID);
        write(
            &folder.join("launcher.json"),
            json!({"pid": 300, "start": 3000}),
        );
        let spec = folder.join("spec.json").to_str().unwrap().to_string();
        let argv: Vec<String> = argv.iter().map(|a| a.replace("<spec>", &spec)).collect();
        w.fakes.process(crate::proc::Row {
            pid: 300,
            ppid: crate::agents::fixture::SERVER,
            start: Some(start),
            path: argv.first().cloned(),
            argv: Some(argv),
        });
        w
    }

    #[test]
    fn legacy_record_proven_by_its_launcher() {
        // The launcher at any executable path, pid and start as launcher.json recorded: running, bound to that pid.
        let w = legacy_world(&["/elsewhere/mux-agent", "launcher", "<spec>"], 3000);
        w.pane_rows(&[listed_row("%9", "300", "old command", ID, "worker")]);
        let s = snapshot(&w);
        assert_eq!(classes(&s), [(ID.to_string(), Class::Running)]);
        assert_eq!(
            s.running()[0].launch,
            Launch {
                command: "old command".into(),
                pid: Some(300)
            }
        );
        // Anything else is unproven: another start, another spec, another program, another pid, a dead pane.
        let cases: [(&[&str], i64, &str, bool); 6] = [
            (&["/x/mux-agent", "launcher", "<spec>"], 3001, "300", false),
            (
                &["/x/mux-agent", "launcher", "/other/spec.json"],
                3000,
                "300",
                false,
            ),
            (&["less", "<spec>"], 3000, "300", false),
            (&["/x/mux-agent", "watch", "<spec>"], 3000, "300", false),
            (&["/x/mux-agent", "launcher", "<spec>"], 3000, "301", false),
            (&["/x/mux-agent", "launcher", "<spec>"], 3000, "300", true),
        ];
        for (argv, start, pid, dead) in cases {
            let w = legacy_world(argv, start);
            let mut row = listed_row("%9", pid, "old command", ID, "worker");
            row.dead = dead;
            w.pane_rows(&[row]);
            assert_eq!(
                classes(&snapshot(&w)),
                [(ID.to_string(), Class::Unproven)],
                "{argv:?} {start} {pid} {dead}"
            );
        }
    }

    #[test]
    fn two_own_records_on_one_pane_are_no_identity() {
        // Pane ids are never reused within a server, so two own records naming one pane cannot both be it: the pane
        // fails closed as unproven, never either agent.
        let w = World::agent("");
        w.agent_record(ID, json!({"name": "me", "pane": "%1"}));
        let ended = w.agent_record("bbbbbbbbbbbb", json!({"name": "old", "pane": "%1"}));
        write(&ended.join("notice.json"), json!({"kind": "stopped"}));
        w.panes(&["%1|@1|100||"]);
        let (caller, s) = (w.caller(), snapshot(&w));
        assert_eq!(s.own(&caller), Own::Unproven);
        assert_eq!(
            refusal(self_of(&caller, &s)),
            "2 parent this pane's agent cannot be proven to run here"
        );
    }

    #[test]
    fn find_pane_named_committed_live() {
        let w = World::human("");
        let caller = w.caller();
        w.agent_record(ID, json!({}));
        w.uncommitted("bbbbbbbbbbbb", json!({"name": "idle", "pane": "%8"}));
        w.panes(&[
            "%9|@5|300|aaaaaaaaaaaa|worker",
            "%8|@6|301|bbbbbbbbbbbb|idle",
        ]);
        let found = find_pane(&w.ctx, &caller, "worker").unwrap();
        assert_eq!(
            (
                found.pane.as_str(),
                found.window.as_str(),
                found.pid.as_str(),
                found.id.as_str()
            ),
            ("%9", "@5", "300", ID)
        );
        let none = "2 target no live agent has that name";
        w.fakes.clear_log();
        // A malformed name refuses before any multiplexer call.
        for name in ["Worker", "1x", "", "a_b", &"a".repeat(25)] {
            assert_eq!(refusal(find_pane(&w.ctx, &caller, name)), none, "{name}");
        }
        assert!(w.fakes.mux_calls().is_empty());
        for name in ["idle", "nobody", "worke"] {
            assert_eq!(refusal(find_pane(&w.ctx, &caller, name)), none, "{name}");
        }
        // Tags rotated: worker's tag on %8, idle's on %9. worker is still %9, idle still not addressable.
        w.panes(&[
            "%9|@5|300|bbbbbbbbbbbb|idle",
            "%8|@6|301|aaaaaaaaaaaa|worker",
        ]);
        assert_eq!(find_pane(&w.ctx, &caller, "worker").unwrap().pane, "%9");
        assert_eq!(refusal(find_pane(&w.ctx, &caller, "idle")), none);
    }

    #[test]
    fn caller_is_its_running_agent_by_pane_never_its_tag() {
        let pane1 = |tag: &str| format!("%1|@1|100|{tag}|x");
        // A human pane, no record there, no tag: root.
        let w = World::human("");
        w.panes(&[&pane1("")]);
        let me = self_of(&w.caller(), &snapshot(&w)).unwrap();
        assert_eq!(
            (me.name.as_str(), me.key.as_str(), me.record),
            ("root", "pane-1", None)
        );
        untagged(&w.ctx, &w.caller(), "no").unwrap();

        // A running agent's pane is that agent, whatever tag it carries (none, or another agent's).
        for tag in ["", "bbbbbbbbbbbb", ID] {
            let w = World::agent(tag);
            w.agent_record(ID, json!({"name": "me", "pane": "%1"}));
            w.panes(&[&pane1(tag)]);
            let s = snapshot(&w);
            let caller = w.caller();
            assert!(
                matches!(s.own(&caller), Own::Agent(ref p) if p.id == ID),
                "{tag}"
            );
            let me = self_of(&caller, &s).unwrap();
            assert_eq!((me.name.as_str(), me.key.as_str()), ("me", ID), "{tag}");
            assert_eq!(
                refusal(untagged(
                    &w.ctx,
                    &caller,
                    "an agent pane cannot spawn agents"
                )),
                "2 tagged an agent pane cannot spawn agents",
                "{tag}"
            );
        }

        // A tag with no running agent at the pane: refused, whatever record the tag names.
        let w = World::agent(ID);
        w.agent_record(ID, json!({"name": "other", "pane": "%9"}));
        w.panes(&[&pane1(ID), "%9|@5|300||"]);
        let (caller, s) = (w.caller(), snapshot(&w));
        assert_eq!(s.own(&caller), Own::None);
        assert_eq!(
            refusal(self_of(&caller, &s)),
            "2 parent this agent pane has no record"
        );
        assert_eq!(refusal(untagged(&w.ctx, &caller, "no")), "2 tagged no");

        // An own record's pane in any other class (uncommitted, replaced, dead), tagless: unproven, refused.
        for (fields, command, dead) in [
            (json!({"pane": "%1"}), launch_command("%1"), false),
            (json!({"pane": "%1"}), "sh".to_string(), false),
            (json!({"pane": "%1"}), launch_command("%1"), true),
        ] {
            let w = World::human("");
            let folder = w.uncommitted(ID, fields.clone());
            if command == "sh" {
                write(&folder.join("commit.json"), json!({}));
            }
            let mut row = listed_row("%1", "100", &command, "", "");
            row.dead = dead;
            w.pane_rows(&[row]);
            let (caller, s) = (w.caller(), snapshot(&w));
            assert_eq!(s.own(&caller), Own::Unproven, "{command} {dead}");
            assert_eq!(
                refusal(self_of(&caller, &s)),
                "2 parent this pane's agent cannot be proven to run here"
            );
            assert_eq!(refusal(untagged(&w.ctx, &caller, "no")), "2 tagged no");
            assert_eq!(mailbox_of(&s, "%1").unwrap(), "pane-1");
        }
    }

    #[test]
    fn notice_once_message_before_marker() {
        let w = World::human("");
        let caller = w.caller();
        let folder = w.agent_record(ID, json!({}));
        let record = record(&w.ctx, &caller, ID).unwrap().unwrap();
        let marker = folder.join("notice.json");
        let seen = marker.clone();
        w.fakes
            .lock_probe(move |_| format!("marker={}", seen.exists()));
        assert!(add_notice(&w.ctx, &caller, ID, &record, Notice::Stopped).unwrap());
        let mail = lock_key("mail", &caller, "pane-1");
        assert_eq!(
            w.fakes.locks(),
            [
                format!("+{mail} marker=false"),
                format!("-{mail} marker=false")
            ]
        );
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            r#"{"kind":"stopped","exitCode":null,"at":"1970-01-01T00:00:00.0000000Z"}"#
        );
        // Posted once: the second call takes no lock and adds nothing.
        w.fakes.clear_log();
        assert!(!add_notice(&w.ctx, &caller, ID, &record, Notice::Lost).unwrap());
        assert!(w.fakes.locks().is_empty());
        // A marker lost after the message went out: the retry finds the message id and posts nothing new.
        std::fs::remove_file(&marker).unwrap();
        assert!(add_notice(&w.ctx, &caller, ID, &record, Notice::Stopped).unwrap());
        let messages = read_mail(&w.ctx, &caller, "pane-1").unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(
            (
                text(messages[0].get("id")),
                text(messages[0].get("from")),
                text(messages[0].get("pane")),
                text(messages[0].get("text"))
            ),
            (
                "exit-aaaaaaaaaaaa".into(),
                "worker".into(),
                "%9".into(),
                "[mux-agent from worker (%9) #exit-aaaaaaaaaaaa]\nworker was stopped.".into()
            )
        );
    }

    #[test]
    fn notice_failed_delivery_writes_no_marker() {
        let w = World::human("");
        let caller = w.caller();
        let folder = w.agent_record(ID, json!({}));
        let record = record(&w.ctx, &caller, ID).unwrap().unwrap();
        // A folder where the parent's mailbox goes: the append cannot happen.
        let mailbox = w.state().join("mail").join("pane-1.jsonl");
        std::fs::create_dir(&mailbox).unwrap();
        assert!(add_notice(&w.ctx, &caller, ID, &record, Notice::Stopped).is_err());
        assert!(!folder.join("notice.json").exists());
        // Once the mailbox can take it, the retry posts it.
        std::fs::remove_dir(&mailbox).unwrap();
        assert!(add_notice(&w.ctx, &caller, ID, &record, Notice::Stopped).unwrap());
        assert!(folder.join("notice.json").is_file());
        assert_eq!(read_mail(&w.ctx, &caller, "pane-1").unwrap().len(), 1);
    }

    // (the stored message's body, its kind and exitCode if any, the notice tried, notice.json's three fields after)
    type Delivered = (&'static str, Option<Value>, Notice, (Value, Value, Value));
    // (createdAt, launcher.json, notice.json already, the marker's kind and code after if any)
    type Uncommitted = (
        &'static str,
        Option<Value>,
        bool,
        Option<(&'static str, Value)>,
    );

    // notice.json as (kind, exitCode, closedBy).
    fn marker_of(folder: &std::path::Path) -> (Value, Value, Value) {
        let marker = crate::state::fs::read_json(&folder.join("notice.json"))
            .unwrap()
            .unwrap();
        let field = |name: &str| marker.get(name).cloned().unwrap_or(Value::Null);
        (field("kind"), field("exitCode"), field("closedBy"))
    }

    #[test]
    fn notice_message_carries_kind_and_lost_is_closed_by_pane() {
        for (notice, kind, code, closed) in [
            (Notice::Exited(json!(3)), "exited", json!(3), Value::Null),
            (Notice::Stopped, "stopped", Value::Null, Value::Null),
            (Notice::Lost, "lost", Value::Null, json!("pane")),
        ] {
            let w = World::human("");
            let caller = w.caller();
            let folder = w.agent_record(ID, json!({}));
            let record = record(&w.ctx, &caller, ID).unwrap().unwrap();
            assert!(add_notice(&w.ctx, &caller, ID, &record, notice).unwrap());
            let mailbox = w.state().join("mail").join("pane-1.jsonl");
            let stored: Value =
                serde_json::from_str(std::fs::read_to_string(mailbox).unwrap().trim_end()).unwrap();
            assert_eq!(
                (&stored["kind"], &stored["exitCode"]),
                (&json!(kind), &code)
            );
            assert_eq!(marker_of(&folder), (json!(kind), code, closed));
        }
    }

    #[test]
    fn notice_follows_the_message_already_delivered() {
        let gone = "worker is gone: its pane closed without an exit record.";
        // (the stored message's body and fields, the notice this post tries, the notice.json it must write)
        let cases: Vec<Delivered> = vec![
            // A message with kind and exitCode decides, whatever this post says.
            (
                "worker was stopped.",
                Some(json!({"kind": "stopped", "exitCode": null})),
                Notice::Exited(json!(0)),
                (json!("stopped"), Value::Null, Value::Null),
            ),
            (
                "worker exited with code 3.",
                Some(json!({"kind": "exited", "exitCode": 3})),
                Notice::Lost,
                (json!("exited"), json!(3), Value::Null),
            ),
            (
                gone,
                Some(json!({"kind": "lost", "exitCode": null})),
                Notice::Exited(json!(0)),
                (json!("lost"), Value::Null, json!("pane")),
            ),
            // A 0.2.1 message (body only) is read from its fixed body.
            (
                "worker was stopped.",
                None,
                Notice::Exited(json!(0)),
                (json!("stopped"), Value::Null, Value::Null),
            ),
            (
                "worker exited with code 3.",
                None,
                Notice::Stopped,
                (json!("exited"), json!(3), Value::Null),
            ),
            (
                "worker exited with code -1073741510.",
                None,
                Notice::Stopped,
                (json!("exited"), json!(-1073741510), Value::Null),
            ),
            (
                gone,
                None,
                Notice::Exited(json!(0)),
                (json!("lost"), Value::Null, json!("pane")),
            ),
            // Unparseable: lost, and the message stays the only one.
            (
                "something else",
                None,
                Notice::Exited(json!(0)),
                (json!("lost"), Value::Null, json!("pane")),
            ),
            (
                "worker exited with code x.",
                None,
                Notice::Stopped,
                (json!("lost"), Value::Null, json!("pane")),
            ),
            (
                "worker was stopped.",
                Some(json!({"kind": "paused"})),
                Notice::Exited(json!(0)),
                (json!("stopped"), Value::Null, Value::Null),
            ),
        ];
        for (body, fields, notice, want) in cases {
            let w = World::human("");
            let caller = w.caller();
            let folder = w.agent_record(ID, json!({}));
            let record = record(&w.ctx, &caller, ID).unwrap().unwrap();
            let id = format!("exit-{ID}");
            match &fields {
                Some(fields) => {
                    crate::agents::mail::add_once(
                        &w.ctx,
                        &caller,
                        "pane-1",
                        "worker",
                        "%9",
                        body,
                        &id,
                        fields.as_object().unwrap(),
                    )
                    .unwrap();
                }
                None => {
                    crate::agents::mail::add_mail(
                        &w.ctx,
                        &caller,
                        "pane-1",
                        "worker",
                        "%9",
                        body,
                        Some(&id),
                    )
                    .unwrap();
                }
            }
            assert!(add_notice(&w.ctx, &caller, ID, &record, notice).unwrap());
            assert_eq!(marker_of(&folder), want, "{body} {fields:?}");
            let messages = read_mail(&w.ctx, &caller, "pane-1").unwrap();
            assert_eq!(messages.len(), 1, "{body}");
            assert_eq!(text(messages[0].get("text")).split('\n').nth(1), Some(body));
        }
    }

    #[test]
    fn reap_needs_gone_probe() {
        // No agents folder: nothing listed, nothing made.
        let w = World::human("");
        let caller = w.caller();
        w.fakes.clear_log();
        reap(&w.ctx, &caller, None, false).unwrap();
        assert!(w.fakes.mux_calls().is_empty());
        assert!(!w.ctx.state_root.exists());

        let folder = w.agent_record(ID, json!({}));
        let marker = folder.join("notice.json");
        // The snapshot misses the agent, but its pane answers live: no notice.
        w.panes(&["%1|@1|100||"]);
        let gone = "#{pane_id}|#{@agent-id}";
        w.fakes.mux_first(gone, 0, "%9|aaaaaaaaaaaa\n", "");
        w.fakes.clear_log();
        reap(&w.ctx, &caller, None, false).unwrap();
        assert!(!marker.exists());
        let calls = w.fakes.mux_calls();
        assert_eq!(calls.len(), 2);
        assert!(list_call(&calls[0]), "{calls:?}");
        assert_eq!(calls[1], ["-N", "display-message", "-p", "-t", "%9", gone]);
        let notice = lock_key("notice", &caller, ID);
        let mail = lock_key("mail", &caller, "pane-1");
        assert_eq!(
            w.fakes.locks(),
            [format!("+{notice}"), format!("-{notice}")]
        );
        // A failed or odd probe is no proof.
        for (code, out) in [(1, "|\n"), (0, "|x\n"), (0, ""), (0, " |\n")] {
            w.fakes.mux_first(gone, code, out, "");
            reap(&w.ctx, &caller, None, false).unwrap();
            assert!(!marker.exists(), "{code} {out:?}");
        }
        // Gone: exit 0 and empty fields.
        w.fakes.mux_first(gone, 0, "|\r\n", "");
        w.fakes.clear_log();
        reap(&w.ctx, &caller, None, false).unwrap();
        assert!(marker.exists());
        assert_eq!(
            w.fakes.locks(),
            [
                format!("+{notice}"),
                format!("+{mail}"),
                format!("-{mail}"),
                format!("-{notice}")
            ]
        );
        // Posted: no candidate left, so no list, probe or lock.
        w.fakes.clear_log();
        reap(&w.ctx, &caller, None, false).unwrap();
        assert!(w.fakes.mux_calls().is_empty() && w.fakes.locks().is_empty());

        // Live in the snapshot, of another session, uncommitted, or without a record: never probed.
        w.agent_record("bbbbbbbbbbbb", json!({}));
        w.agent_record("cccccccccccc", json!({"session": "$4"}));
        w.uncommitted("dddddddddddd", json!({}));
        let bare = w.folder("eeeeeeeeeeee");
        std::fs::create_dir(&bare).unwrap();
        write(&bare.join("commit.json"), json!({}));
        w.agent_record("bbbbbbbbbbbb", json!({"pane": "%7"}));
        w.panes(&["%7|@2|310|bbbbbbbbbbbb|live"]);
        let s = snapshot(&w);
        w.fakes.clear_log();
        reap(&w.ctx, &caller, Some(&s), false).unwrap();
        assert!(w.fakes.mux_calls().is_empty());
        assert!(w.fakes.locks().is_empty());
    }

    #[test]
    fn notice_lost_vs_exited_from_launcher_json() {
        let lost = "worker is gone: its pane closed without an exit record.";
        for (launcher, kind, body, code) in [
            (None, "lost", lost, Value::Null),
            (Some(json!({"pid": 1})), "lost", lost, Value::Null),
            (
                Some(json!({"pid": 1, "exitCode": null})),
                "lost",
                lost,
                Value::Null,
            ),
            (Some(json!(5)), "lost", lost, Value::Null),
            (
                Some(json!({"pid": 1, "exitCode": 7})),
                "exited",
                "worker exited with code 7.",
                json!(7),
            ),
            (
                Some(json!({"pid": 1, "exitCode": 0})),
                "exited",
                "worker exited with code 0.",
                json!(0),
            ),
            (
                Some(json!({"pid": 1, "exitCode": -1073741510})),
                "exited",
                "worker exited with code -1073741510.",
                json!(-1073741510),
            ),
        ] {
            let w = World::human("");
            let caller = w.caller();
            let folder = w.agent_record(ID, json!({}));
            if let Some(launcher) = &launcher {
                write(&folder.join("launcher.json"), launcher.clone());
            }
            w.panes(&[]);
            w.fakes.mux_first("#{pane_id}|#{@agent-id}", 0, "|\n", "");
            reap(&w.ctx, &caller, None, false).unwrap();
            let marker = crate::state::fs::read_json(&folder.join("notice.json"))
                .unwrap()
                .unwrap();
            assert_eq!(
                (&marker["kind"], &marker["exitCode"]),
                (&json!(kind), &code)
            );
            let messages = read_mail(&w.ctx, &caller, "pane-1").unwrap();
            assert_eq!(messages.len(), 1, "{launcher:?}");
            let text = text(messages[0].get("text"));
            assert_eq!(text.split('\n').nth(1), Some(body), "{launcher:?}");
        }
    }

    // Agent ID's pane %9 listed dead, still its launch.
    fn dead_snapshot(w: &World) -> Snapshot {
        w.panes_dead(&["%9|@5|300|aaaaaaaaaaaa|worker"], &["%9"]);
        snapshot(w)
    }

    fn launch9() -> Launch {
        Launch {
            command: launch_command("%9"),
            pid: None,
        }
    }

    // The mailbox lines as stored, without moving the read offset.
    fn stored(w: &World) -> Vec<Value> {
        let path = w.state().join("mail").join("pane-1.jsonl");
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    // The conditional kill of %9, as the multiplexer receives it: dead and still its launch, never by tag.
    fn kill_call() -> Vec<String> {
        [
            "-N",
            "if-shell",
            "-F",
            "-t",
            "%9",
            "#{&&:#{pane_dead},#{==:#{pane_start_command},/bin/mux-agent launcher /state/agents/9/spec.json}}",
            "kill-pane -t %9",
        ]
        .map(String::from)
        .to_vec()
    }

    #[test]
    fn snapshot_reads_pane_dead() {
        let w = World::human("");
        w.agent_record(ID, json!({}));
        w.agent_record("bbbbbbbbbbbb", json!({"name": "idle", "pane": "%8"}));
        w.panes_dead(
            &[
                "%9|@5|300|aaaaaaaaaaaa|worker",
                "%8|@5|301|bbbbbbbbbbbb|idle",
            ],
            &["%9"],
        );
        let b = "bbbbbbbbbbbb".to_string();
        assert_eq!(
            classes(&snapshot(&w)),
            [
                (ID.to_string(), Class::DeadLaunch),
                (b.clone(), Class::Running)
            ]
        );
        // tmux 3.8 answers no pid for a dead pane: still a dead launch, so reap can close it. A live pane with no pid
        // is never running.
        w.panes_dead(
            &["%9|@5||aaaaaaaaaaaa|worker", "%8|@5||bbbbbbbbbbbb|idle"],
            &["%9"],
        );
        assert_eq!(
            classes(&snapshot(&w)),
            [(ID.to_string(), Class::DeadLaunch), (b, Class::LiveLaunch)]
        );
        assert_eq!(kill_if_dead("%9", &launch9()), kill_call()[1..]);
    }

    #[test]
    fn reap_kills_a_dead_pane_by_one_conditional_then_notices() {
        let w = World::human("");
        let caller = w.caller();
        let folder = w.agent_record(ID, json!({}));
        write(
            &folder.join("launcher.json"),
            json!({"pid": 300, "exitCode": 4}),
        );
        w.panes_dead(&["%9|@5|300|aaaaaaaaaaaa|worker"], &["%9"]);
        // Once the conditional ran, the pane answers gone.
        w.fakes.mux_rule("#{pane_id}|#{@agent-id}", 0, "|\n", "");
        w.fakes.clear_log();
        assert!(reap(&w.ctx, &caller, None, false).unwrap());
        let calls = w.fakes.mux_calls();
        assert!(list_call(&calls[0]), "{calls:?}");
        assert_eq!(calls[1], kill_call());
        assert_eq!(
            calls[2],
            ["-N", "display-message", "-p", "-t", "%9", GONE_PROBE]
        );
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert_eq!(marker_of(&folder).0, "exited");
        assert_eq!(marker_of(&folder).1, 4);
        assert_eq!(stored(&w).len(), 1);
        let (notice, mail) = (
            lock_key("notice", &caller, ID),
            lock_key("mail", &caller, "pane-1"),
        );
        assert_eq!(
            w.fakes.locks(),
            [
                format!("+{notice}"),
                format!("+{mail}"),
                format!("-{mail}"),
                format!("-{notice}")
            ]
        );
        // Never a pid signal: the pane goes by the multiplexer only.
        assert!(!w.fakes.os_calls().iter().any(|c| c.starts_with("kill ")));

        // Already noticed (the launcher posted before it exited): killed again, no probe, no second message. Its tag
        // rotated away to another pane changes nothing.
        w.panes_dead(&["%9|@5|300|bbbbbbbbbbbb|other"], &["%9"]);
        let s = snapshot(&w);
        w.fakes.clear_log();
        assert!(reap(&w.ctx, &caller, Some(&s), false).unwrap());
        assert_eq!(w.fakes.mux_calls(), [kill_call()]);
        assert_eq!(stored(&w).len(), 1);
        assert_eq!(
            w.fakes.locks(),
            [format!("+{notice}"), format!("-{notice}")]
        );

        // A noticed agent whose launcher recorded an exit is reaped without a snapshot too.
        w.fakes.clear_log();
        assert!(reap(&w.ctx, &caller, None, false).unwrap());
        let calls = w.fakes.mux_calls();
        assert!(list_call(&calls[0]) && calls[1] == kill_call(), "{calls:?}");
    }

    #[test]
    fn reap_never_kills_a_pane_not_its_own() {
        let w = World::human("");
        let caller = w.caller();
        // Another session's record, a tag without a folder (a dead tagged root), and a dead replacement of an own
        // record's pane: never killed, never locked.
        w.agent_record("bbbbbbbbbbbb", json!({"session": "$4", "pane": "%7"}));
        let noticed = w.agent_record(ID, json!({"pane": "%9"}));
        write(&noticed.join("notice.json"), json!({"kind": "exited"}));
        w.pane_rows(&[
            Listed {
                dead: true,
                ..listed_row("%7", "", &launch_command("%7"), "bbbbbbbbbbbb", "x")
            },
            Listed {
                dead: true,
                ..listed_row("%1", "", "sh", "cccccccccccc", "root")
            },
            Listed {
                dead: true,
                ..listed_row("%9", "", "sh -c exit", ID, "worker")
            },
        ]);
        let s = snapshot(&w);
        assert_eq!(classes(&s), [(ID.to_string(), Class::Unproven)]);
        w.fakes.clear_log();
        assert!(!reap(&w.ctx, &caller, Some(&s), false).unwrap());
        assert!(w.fakes.mux_calls().is_empty());
        assert!(w.fakes.locks().is_empty());
    }

    // A multiplexer that keeps pane %9 as (dead, start command) and evaluates the conditional as it would.
    fn emulated(
        w: &World,
        dead: bool,
        command: &str,
    ) -> std::rc::Rc<std::cell::RefCell<Option<(bool, String)>>> {
        let pane = std::rc::Rc::new(std::cell::RefCell::new(Some((dead, command.to_string()))));
        let state = pane.clone();
        w.fakes.mux_script("if-shell", move |argv| {
            let condition = &argv[5];
            let mut held = state.borrow_mut();
            let holds = held.as_ref().is_some_and(|(dead, command)| {
                let launch = Launch {
                    command: command.clone(),
                    pid: None,
                };
                *dead && *condition == launch.dead()
            });
            if holds && argv[6] == "kill-pane -t %9" {
                *held = None;
            }
            Ok(crate::mux::Output {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });
        let state = pane.clone();
        w.fakes.mux_script("#{pane_id}|#{@agent-id}", move |_| {
            let stdout = match state.borrow().as_ref() {
                Some(_) => "%9|\n".to_string(),
                None => "|\n".into(),
            };
            Ok(crate::mux::Output {
                code: 0,
                stdout,
                stderr: String::new(),
            })
        });
        pane
    }

    #[test]
    fn reap_conditional_keeps_a_replaced_or_live_pane() {
        // The snapshot saw %9 dead and its launch; by the conditional it was respawned (another command), or
        // respawned live: kept.
        let ours = launch_command("%9");
        for (dead_now, command, kept) in [
            (true, "sh -c exit", true),
            (false, ours.as_str(), true),
            (true, ours.as_str(), false),
        ] {
            let w = World::human("");
            let caller = w.caller();
            let folder = w.agent_record(ID, json!({}));
            let s = dead_snapshot(&w);
            let pane = emulated(&w, dead_now, command);
            reap(&w.ctx, &caller, Some(&s), false).unwrap();
            assert_eq!(pane.borrow().is_some(), kept, "{dead_now} {command}");
            assert_eq!(
                folder.join("notice.json").exists(),
                !kept,
                "{dead_now} {command}"
            );
            assert_eq!(stored(&w).len(), usize::from(!kept), "{dead_now} {command}");
        }
    }

    const NOW: &str = "2026-10-29T12:00:00.0000000Z";

    #[test]
    fn reap_uncommitted_after_60s_under_spawn_then_notice() {
        let (old, edge, young) = (
            "2026-10-29T11:58:59.0000000Z",
            "2026-10-29T11:59:00.0000000Z",
            "2026-10-29T11:59:30.0000000Z",
        );
        // (createdAt, launcher.json, notice.json already, the marker kind and code after, a message posted)
        let cases: Vec<Uncommitted> = vec![
            (old, None, false, Some(("lost", Value::Null))),
            (
                old,
                Some(json!({"pid": 300, "aborted": true, "exitCode": 1})),
                false,
                Some(("exited", json!(1))),
            ),
            (old, Some(json!({"pid": 300, "exitCode": 1})), true, None),
            (edge, None, false, None),
            (young, None, false, None),
        ];
        for (created, launcher, noticed, want) in cases {
            let w = World::human("");
            w.clock(NOW);
            let caller = w.caller();
            let folder = w.uncommitted(ID, json!({"createdAt": created}));
            if let Some(launcher) = &launcher {
                write(&folder.join("launcher.json"), launcher.clone());
            }
            if noticed {
                write(
                    &folder.join("notice.json"),
                    json!({"kind": "exited", "exitCode": 1, "at": "x"}),
                );
            }
            w.panes(&[]);
            w.fakes.mux_rule("#{pane_id}|#{@agent-id}", 0, "|\n", "");
            w.fakes.clear_log();
            let acted = reap(&w.ctx, &caller, None, false).unwrap();
            let label = format!("{created} {launcher:?} {noticed}");
            assert_eq!(acted, want.is_some(), "{label}");
            match &want {
                Some((kind, code)) => {
                    let (k, c, closed) = marker_of(&folder);
                    assert_eq!((k, c), (json!(kind), code.clone()), "{label}");
                    assert_eq!(closed == json!("pane"), *kind == "lost", "{label}");
                    assert_eq!(stored(&w).len(), 1, "{label}");
                    let spawn = lock_key("spawn", &caller, "$3");
                    let notice = lock_key("notice", &caller, ID);
                    let mail = lock_key("mail", &caller, "pane-1");
                    assert_eq!(
                        w.fakes.locks(),
                        [
                            format!("+{spawn}"),
                            format!("+{notice}"),
                            format!("+{mail}"),
                            format!("-{mail}"),
                            format!("-{notice}"),
                            format!("-{spawn}")
                        ],
                        "{label}"
                    );
                }
                None => {
                    assert!(stored(&w).is_empty(), "{label}");
                    assert!(w.fakes.locks().is_empty(), "{label}");
                    assert_eq!(folder.join("notice.json").exists(), noticed, "{label}");
                }
            }
        }
    }

    // An uncommitted agent (record.json, no commit.json) whose pane %9 is listed dead; its pane answers gone once the
    // conditional ran. Answers the world and whether the conditional ran.
    fn dead_uncommitted(
        created: &str,
        noticed: bool,
    ) -> (World, std::rc::Rc<std::cell::Cell<bool>>) {
        let w = World::human("");
        w.clock(NOW);
        let folder = w.uncommitted(ID, json!({"createdAt": created}));
        if noticed {
            // The launcher's aborted commit wait posted `exited 1` before its pane died.
            write(
                &folder.join("launcher.json"),
                json!({"pid": 300, "aborted": true, "exitCode": 1}),
            );
            write(
                &folder.join("notice.json"),
                json!({"kind": "exited", "exitCode": 1, "at": "x"}),
            );
        }
        w.panes_dead(&["%9|@5|300|aaaaaaaaaaaa|worker"], &["%9"]);
        let killed = std::rc::Rc::new(std::cell::Cell::new(false));
        let kill = killed.clone();
        w.fakes.mux_script("if-shell", move |_| {
            kill.set(true);
            Ok(crate::mux::Output {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });
        let gone = killed.clone();
        w.fakes.mux_script("#{pane_id}|#{@agent-id}", move |_| {
            let stdout = if gone.get() {
                "|\n"
            } else {
                "%9|aaaaaaaaaaaa\n"
            };
            Ok(crate::mux::Output {
                code: 0,
                stdout: stdout.into(),
                stderr: String::new(),
            })
        });
        (w, killed)
    }

    #[test]
    fn reap_dead_uncommitted_pane_only_by_the_spawn_lock_path() {
        let (old, young) = (
            "2026-10-29T11:58:00.0000000Z",
            "2026-10-29T11:59:30.0000000Z",
        );
        // (createdAt, noticed already, spawn lock: free, held by another spawn, or held by this one; killed)
        for (created, noticed, lock, want) in [
            (young, false, "free", false),
            (young, true, "free", false),
            (young, false, "inside", false),
            (old, false, "elsewhere", false),
            (old, true, "elsewhere", false),
            (old, false, "free", true),
            (old, true, "free", true),
            (old, false, "inside", true),
        ] {
            let label = format!("{created} noticed={noticed} {lock}");
            let (w, killed) = dead_uncommitted(created, noticed);
            let caller = w.caller();
            let spawn = lock_key("spawn", &caller, "$3");
            let notice = lock_key("notice", &caller, ID);
            if lock == "elsewhere" {
                w.fakes.lock_timeout("spawn|");
            }
            w.fakes.clear_log();
            // A spawn held elsewhere never fails the caller: the pane waits for the next try.
            let acted = match lock {
                "inside" => {
                    crate::agents::lock(&w.ctx, &spawn, || reap(&w.ctx, &caller, None, true))
                }
                _ => reap(&w.ctx, &caller, None, false),
            }
            .unwrap_or_else(|r| panic!("{label}: {} {}", r.check, r.message));
            assert_eq!((acted, killed.get()), (want, want), "{label}");
            let conditionals = w
                .fakes
                .mux_calls()
                .iter()
                .filter(|c| c[1] == "if-shell")
                .count();
            assert_eq!(conditionals, usize::from(want), "{label}");
            let folder = w.folder(ID);
            assert_eq!(
                folder.join("notice.json").exists(),
                want || noticed,
                "{label}"
            );
            // One notice at most: a noticed agent gets no second message.
            assert_eq!(stored(&w).len(), usize::from(want && !noticed), "{label}");
            if want && !noticed {
                assert_eq!(marker_of(&folder).2, json!("pane"), "{label}");
            }
            let locks = w.fakes.locks();
            if want {
                // The spawn lock (taken here, or held by the caller) comes before the notice lock.
                let at = |key: &str| {
                    locks
                        .iter()
                        .position(|l| *l == format!("+{key}"))
                        .expect(key)
                };
                assert!(at(&spawn) < at(&notice), "{label} {locks:?}");
                assert_eq!(
                    locks.iter().filter(|l| **l == format!("+{spawn}")).count(),
                    1,
                    "{label}"
                );
            } else {
                assert!(!locks.contains(&format!("+{notice}")), "{label} {locks:?}");
            }
        }
    }

    // Another spawn holds the session's spawn lock while an uncommitted agent over 60 s old has its pane gone, or
    // dead: list never waits for that lock. It returns at once with no cleanup; a later list, the lock free, closes
    // the agent with exactly one notice.
    #[test]
    fn list_never_waits_for_a_busy_spawn_lock() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::{Duration, Instant};
        for dead in [false, true] {
            let w = World::human("");
            w.clock(NOW);
            let caller = w.caller();
            let folder = w.uncommitted(ID, json!({"createdAt": "2026-10-29T11:58:00.0000000Z"}));
            let killed = std::rc::Rc::new(std::cell::Cell::new(false));
            let listed_now = killed.clone();
            w.fakes.mux_script("list-panes", move |argv| {
                let rows: &[&str] = if dead && !listed_now.get() {
                    &["%9|@5|300|aaaaaaaaaaaa|worker"]
                } else {
                    &[]
                };
                Ok(crate::mux::Output {
                    code: 0,
                    stdout: crate::agents::fixture::listed_dead(argv, rows, "\n", &["%9"]),
                    stderr: String::new(),
                })
            });
            let kill = killed.clone();
            w.fakes.mux_script("if-shell", move |_| {
                kill.set(true);
                Ok(crate::mux::Output {
                    code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            });
            let probed = killed.clone();
            w.fakes.mux_script("#{pane_id}|#{@agent-id}", move |_| {
                let stdout = if dead && !probed.get() {
                    "%9|aaaaaaaaaaaa\n"
                } else {
                    "|\n"
                };
                Ok(crate::mux::Output {
                    code: 0,
                    stdout: stdout.into(),
                    stderr: String::new(),
                })
            });
            let gate = Arc::new(crate::testing::Gate::default());
            w.fakes.share_locks(gate.clone());
            let spawn = lock_key("spawn", &caller, "$3");
            let release = AtomicBool::new(false);
            let took = std::thread::scope(|scope| {
                let holder = scope.spawn(|| {
                    // Another process's spawn, mid-launch: its own fakes, the same lock.
                    let other = World::human("");
                    other.fakes.share_locks(gate.clone());
                    crate::agents::lock(&other.ctx, &spawn, || {
                        let end = Instant::now() + Duration::from_secs(3);
                        while !release.load(Ordering::SeqCst) && Instant::now() < end {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Ok(())
                    })
                    .unwrap();
                });
                let end = Instant::now() + Duration::from_secs(5);
                while !gate.held(&spawn) {
                    assert!(Instant::now() < end, "the spawn lock was never held");
                    std::thread::sleep(Duration::from_millis(1));
                }
                w.fakes.clear_log();
                let at = Instant::now();
                let reply = crate::agents::view::list(&w.ctx, &w.request(&["list"]));
                let took = at.elapsed();
                release.store(true, Ordering::SeqCst);
                holder.join().unwrap();
                assert!(reply.is_ok(), "dead={dead} {reply:?}");
                took
            });
            assert!(
                took < Duration::from_secs(1),
                "dead={dead}: list waited {took:?} for the spawn lock"
            );
            assert!(w.fakes.slept().is_empty(), "dead={dead}");
            assert!(!folder.join("notice.json").exists(), "dead={dead}");
            assert!(!killed.get(), "dead={dead}");
            assert!(stored(&w).is_empty(), "dead={dead}");
            // The lock free: the next list closes it, once; the one after posts nothing more.
            for _ in 0..2 {
                crate::agents::view::list(&w.ctx, &w.request(&["list"])).unwrap();
            }
            assert_eq!(killed.get(), dead, "dead={dead}");
            assert_eq!(marker_of(&folder).0, json!("lost"), "dead={dead}");
            assert_eq!(stored(&w).len(), 1, "dead={dead}");
        }
    }

    #[test]
    fn reap_uncommitted_untouched_once_committed_or_live() {
        let old = json!({"createdAt": "2026-10-29T11:58:00.0000000Z"});
        // commit.json lands while reap waits for the spawn lock: untouched.
        let w = World::human("");
        w.clock(NOW);
        let caller = w.caller();
        let folder = w.uncommitted(ID, old.clone());
        w.panes(&[]);
        w.fakes.mux_rule("#{pane_id}|#{@agent-id}", 0, "|\n", "");
        let spawn = lock_key("spawn", &caller, "$3");
        let commit = folder.join("commit.json");
        w.fakes.lock_probe(move |event| {
            if event == format!("+{spawn}") {
                write(&commit, json!({"committedAt": "x"}));
            }
            String::new()
        });
        reap(&w.ctx, &caller, None, false).unwrap();
        assert!(!folder.join("notice.json").exists());
        assert!(stored(&w).is_empty());
        // Its pane answers live: untouched.
        let w = World::human("");
        w.clock(NOW);
        let caller = w.caller();
        let folder = w.uncommitted(ID, old.clone());
        w.panes(&[]);
        w.fakes
            .mux_rule("#{pane_id}|#{@agent-id}", 0, "%9|aaaaaaaaaaaa\n", "");
        assert!(!reap(&w.ctx, &caller, None, false).unwrap());
        assert!(!folder.join("notice.json").exists());
        // Inside spawn's own lock, reap never takes it again.
        let w = World::human("");
        w.clock(NOW);
        let caller = w.caller();
        let folder = w.uncommitted(ID, old);
        w.panes(&[]);
        w.fakes.mux_rule("#{pane_id}|#{@agent-id}", 0, "|\n", "");
        let spawn = lock_key("spawn", &caller, "$3");
        w.fakes.clear_log();
        crate::agents::lock(&w.ctx, &spawn, || reap(&w.ctx, &caller, None, true)).unwrap();
        assert!(folder.join("notice.json").exists());
        let notice = lock_key("notice", &caller, ID);
        assert_eq!(
            w.fakes.locks()[..2],
            [format!("+{spawn}"), format!("+{notice}")]
        );
        assert_eq!(
            w.fakes
                .locks()
                .iter()
                .filter(|l| l.contains("spawn|"))
                .count(),
            2
        );
    }
}
