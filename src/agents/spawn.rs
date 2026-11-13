//! spawn: authenticates the caller, connects hc, then admits and launches under the session's spawn lock.
//! Hc lifecycle facts reserve capacity and names; commit.json releases the launcher, then the sidebar is best effort.

use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::mail::doing_line;
use super::placement::{DEFAULT as DEFAULT_PLACEMENT, read_placement};
use super::{copilot, lock, lock_key, registry, state_dir, text, usage};
use crate::caller::{Caller, Kind, resolve};
use crate::checks::{Context, Plan, assert_spawn_allowed, context_plan, session_id};
use crate::cli::Request;
use crate::ctx::Ctx;
use crate::error::{Refusal, Res, refuse};
use crate::mux::Kind as MuxKind;
use crate::parse::{Client, cmdline_length};
use crate::sidebar::{self, Bar};
use crate::state::fs::{BRIEF_MAX, TASK_MAX, read_text, write_json};
use crate::state::root::OsKind;
use crate::util::{guid, hex_id, utc_o};

const CLAUDE_FLAGS: [&str; 3] = ["--dangerously-skip-permissions", "--effort", "high"];
const COPILOT_FLAGS: [&str; 2] = ["--no-sandbox", "--allow-all"];
// The client command line holds 32767 UTF-16 units after quoting; 1024 of them are kept for the executable path.
pub(crate) const CMDLINE_MAX: usize = 32767;
const EXE_RESERVE: usize = 1024;
const MODEL_MAX: usize = 64;
const ENTRY: &str =
    "mux-agent entry path may hold only letters, digits, spaces and _ \\ / : . ( ) -";

/// The launcher's flags before the client arguments.
pub(crate) fn client_flags(client: Client) -> &'static [&'static str] {
    match client {
        Client::Claude => &CLAUDE_FLAGS,
        Client::Copilot => &COPILOT_FLAGS,
    }
}

pub(crate) fn client_name(client: Client) -> &'static str {
    match client {
        Client::Claude => "claude",
        Client::Copilot => "copilot",
    }
}

/// `claude` or `copilot` exactly.
pub(crate) fn client_named(name: &str) -> Option<Client> {
    match name {
        "claude" => Some(Client::Claude),
        "copilot" => Some(Client::Copilot),
        _ => None,
    }
}

fn session_variable(client: Client) -> &'static str {
    match client {
        Client::Claude => "CLAUDE_CODE_SESSION_ID",
        Client::Copilot => "COPILOT_AGENT_SESSION_ID",
    }
}

fn context_name(context: Context) -> &'static str {
    match context {
        Context::Fork => "fork",
        Context::Brief => "brief",
    }
}

/// `^[A-Za-z0-9][A-Za-z0-9._\[\]-]{0,63}$`: a model name or alias (`sonnet`, `sonnet[1m]`), never a flag or a
/// second argument.
fn model_name(model: &str) -> bool {
    let mut bytes = model.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
        && bytes.all(|b| b.is_ascii_alphanumeric() || b".-_[]".contains(&b))
        && model.len() <= MODEL_MAX
}

pub(crate) fn utf8(path: &Path) -> Res<&str> {
    path.to_str()
        .ok_or_else(|| Refusal::internal("a path is not UTF-8"))
}

/// The new agent as its header names it.
pub struct Identity<'a> {
    pub name: &'a str,
    pub id: &'a str,
    pub client: Client,
}

pub fn spawn(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let split = request.value("Split");
    let tab = request.switch("Tab");
    let model = request.value("Model");
    let one_task = request.value("Task").is_some() != request.value("TaskFile").is_some();
    let Some(client) = request.value("Client").and_then(client_named) else {
        return Err(usage("spawn"));
    };
    if !one_task
        || !split.is_none_or(|s| s == "h" || s == "v")
        || (tab && split.is_some())
        || !model.is_none_or(model_name)
    {
        return Err(usage("spawn"));
    }
    let caller = resolve(ctx)?;
    registry::untagged(ctx, &caller, "an agent pane cannot spawn agents")?;
    // Relative files are the caller's; the task travels on the client command line, so it stays at 16 KB.
    let task = match (request.value("Task"), request.value("TaskFile")) {
        (Some(_), _) if caller.kind == Kind::Agent => {
            return refuse(
                2,
                "usage",
                "agents pass free text only in files: use -TaskFile",
            );
        }
        (Some(task), _) => task.to_string(),
        (None, file) => read_text(&ctx.cwd.join(file.unwrap_or_default()), TASK_MAX)?,
    };
    if task.is_empty() {
        return refuse(2, "usage", "the task is empty");
    }
    let brief = request
        .value("BriefFile")
        .map(|file| read_text(&ctx.cwd.join(file), BRIEF_MAX))
        .transpose()?;
    // An agent caller passes C3, C2 and C5; its children start where it started.
    let directory = if caller.kind == Kind::Agent && !ctx.admitted {
        assert_spawn_allowed(ctx, &caller)?
    } else {
        ctx.cwd.clone()
    };
    // After every check of the caller, before the lock: a read that fails is the default, never a refusal.
    let (placement, source) = match (split, tab) {
        (Some(split), _) => (split, "flag"),
        (None, true) => (DEFAULT_PLACEMENT, "flag"),
        (None, false) => match read_placement(ctx, &caller) {
            Ok(Some(stored)) => (stored, "session"),
            _ => (DEFAULT_PLACEMENT, "default"),
        },
    };
    let record = launch_hc(
        ctx,
        &caller,
        CoreRequest {
            client,
            task: &task,
            brief: brief.as_deref(),
            directory: &directory,
            model,
            no_fork: request.switch("NoFork"),
            split: (placement != DEFAULT_PLACEMENT).then_some(placement),
        },
        |core| {
            let snapshot = super::membership::Snapshot::for_spawn(ctx, &caller, core)?;
            registry::reap_hc(ctx, &caller, &snapshot)?;
            let name = snapshot.admit_name(request.value("Name"), client, &task)?;
            let mut used = [0usize; crate::paint::PALETTE.len()];
            for o in snapshot
                .facts
                .local
                .iter()
                .filter(|o| o.life != super::lifecycle::Life::Ended)
            {
                let r = &snapshot.records[&o.identity.id].record;
                let color = crate::paint::identity(
                    r.get("color").and_then(Value::as_str),
                    &o.identity.name,
                );
                if let Some(i) = crate::paint::PALETTE.iter().position(|p| *p == color) {
                    used[i] += 1;
                }
            }
            let i = (0..used.len()).min_by_key(|i| (used[*i], *i)).unwrap_or(0);
            Ok(CoreSlot {
                name,
                color: crate::paint::PALETTE[i],
            })
        },
    )?;
    let mut reply = Map::new();
    for key in [
        "name",
        "id",
        "pane",
        "window",
        "client",
        "sessionId",
        "context",
        "reason",
    ] {
        reply.insert(key.into(), record[key].clone());
    }
    reply.insert("model".into(), model.unwrap_or("inherited").into());
    reply.insert("placement".into(), placement.into());
    reply.insert("placementSource".into(), source.into());
    // The agent is committed and the spawn lock released: the bar is best effort and never fails the spawn, not
    // even by a panic, which unwinding has already freed the sidebar lock from.
    let bar = panic::catch_unwind(AssertUnwindSafe(|| sidebar::ensure(ctx, &caller)))
        .unwrap_or_else(|_| refuse(1, "internal", "unexpected error (panic)"));
    match bar {
        Ok(Bar::Opened(pane) | Bar::Existing(pane)) => {
            reply.insert("sidebar".into(), pane.into());
        }
        Ok(Bar::Disabled) => {
            reply.insert("sidebar".into(), Value::Null);
        }
        Err(refusal) => {
            reply.insert("sidebar".into(), Value::Null);
            reply.insert("sidebarError".into(), refusal.message.into());
        }
    }
    reply.insert("life".into(), "running".into());
    reply.insert("view".into(), "attached".into());
    reply.insert("hcAddress".into(), record["hcAddress"].clone());
    Ok(reply)
}

/// The reply's `key: value` lines, then `sidebar <pane>`, `sidebar off` or `sidebar error: <message>`.
pub fn spawn_text(reply: &Map<String, Value>, _color: bool) -> String {
    let fields: Map<String, Value> = reply
        .iter()
        .filter(|(key, _)| *key != "sidebar" && *key != "sidebarError")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let bar = match (reply.get("sidebarError"), reply.get("sidebar")) {
        (Some(error), _) => format!("sidebar error: {}", text(Some(error))),
        (None, Some(Value::String(pane))) => format!("sidebar {pane}"),
        _ => "sidebar off".into(),
    };
    format!("{}\n{bar}", crate::json::to_text(&fields))
}

/// A checked request, run under the spawn lock.
struct Launch<'a> {
    ctx: &'a Ctx,
    caller: &'a Caller,
    client: Client,
    // None: the client's own default.
    model: Option<&'a str>,
    no_fork: bool,
    task: String,
    brief: Option<String>,
    directory: PathBuf,
}

/// The agent being started.
struct Agent {
    id: String,
    name: String,
    // The reserved identity hue, kept in record.json as `color`.
    color: &'static str,
    session: String,
    plan: Plan,
    folder: PathBuf,
}

impl Launch<'_> {
    fn allocate(&self, state: &Path, name: String, color: &'static str) -> Res<Agent> {
        let (ctx, caller) = (self.ctx, self.caller);
        let mut plan = context_plan(caller, self.client, &ctx.env, self.no_fork);
        let session = guid()?;
        let id = hex_id()?;
        if plan.context == Context::Fork && self.client == Client::Copilot {
            let parent = plan.parent_session.clone().unwrap_or_default();
            if !copilot::copy_session(&ctx.home, &parent, &session) {
                plan = Plan {
                    context: Context::Brief,
                    reason: "copy-verification-failed",
                    parent_session: None,
                };
            }
        }
        // A verified fork copy is retained as transaction evidence if a later step fails.
        if plan.context == Context::Brief && caller.kind == Kind::Agent && self.brief.is_none() {
            return refuse(
                2,
                "brief",
                "this agent starts brief-only, so an agent caller passes -BriefFile",
            );
        }
        Ok(Agent {
            folder: state.join("agents").join(&id),
            id,
            name,
            color,
            session,
            plan,
        })
    }

    fn record(
        &self,
        agent: &Agent,
        new_pane: &str,
        window: &str,
        start_command: &str,
    ) -> Res<Value> {
        let (ctx, caller) = (self.ctx, self.caller);
        let parent_agent = match (&caller.agent, caller.client) {
            (Some(parent), Some(client)) => {
                let session = ctx
                    .env
                    .get(session_variable(client))
                    .filter(|s| session_id(s));
                json!({"client": client_name(client), "pid": parent.pid, "start": parent.start, "sessionId": session})
            }
            _ => Value::Null,
        };
        let client = client_name(self.client);
        let context = context_name(agent.plan.context);
        let mut record = json!({"schema": 1, "id": agent.id, "name": agent.name, "client": client,
            "sessionId": agent.session, "context": context, "reason": agent.plan.reason,
            "session": caller.session, "pane": new_pane, "window": window, "parentPane": caller.pane,
            "parentAgent": parent_agent, "directory": utf8(&self.directory)?,
            "taskLine": doing_line(&self.task), "createdAt": utc_o(ctx.clock.now())});
        record["color"] = agent.color.into();
        // The exact command the pane started, as the multiplexer renders it: the agent is the pane running it (#48).
        record["startCommand"] = start_command.into();
        // No model is the client's own; a chosen one is the record's last field.
        if let Some(model) = self.model {
            record["model"] = model.into();
        }
        Ok(record)
    }

    /// Before any pane: header, prompt and args, the length check, then the private folder, brief.md and spec.json.
    fn prepare(&self, agent: &Agent) -> Res<PathBuf> {
        let ctx = self.ctx;
        let brief_path = self.brief.as_ref().map(|_| agent.folder.join("brief.md"));
        let transcript = parent_transcript(ctx, self.caller)?;
        let me = Identity {
            name: &agent.name,
            id: &agent.id,
            client: self.client,
        };
        let lines = header(
            ctx,
            self.caller,
            &me,
            &agent.plan,
            brief_path.as_deref(),
            transcript.as_deref(),
        )?;
        let prompt = format!("{}\n\n{}", lines.join("\n"), self.task);
        let recipe = match agent.plan.context {
            Context::Fork => Recipe::Fork {
                parent: agent.plan.parent_session.as_deref().unwrap_or_default(),
                task: Some(&prompt),
            },
            Context::Brief => Recipe::First(Some(&prompt)),
        };
        let args = client_args(self.client, recipe, &agent.session, self.model);
        let mut argv: Vec<&str> = client_flags(self.client).to_vec();
        argv.extend(args.iter().map(String::as_str));
        if EXE_RESERVE + cmdline_length(&argv) > CMDLINE_MAX {
            return refuse(2, "usage", "task is too long for the client command line");
        }
        private_folder(ctx, &agent.folder)?;
        if let (Some(path), Some(brief)) = (&brief_path, &self.brief) {
            std::fs::write(path, brief)?;
        }
        let state = agent
            .folder
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| Refusal::internal("the agent folder has no state folder"))?;
        let spec = agent.folder.join("spec.json");
        write_json(
            &spec,
            &json!({"schema": 1, "stateDirectory": utf8(state)?, "id": agent.id,
                "client": client_name(self.client), "parentPane": self.caller.pane, "args": args,
                "prompt": prompt}),
        )?;
        Ok(spec)
    }
}

/// commit.json, the last write: from here the agent is live. It is made once, whole: a launcher that gave up waiting
/// made it first (`"aborted": true`), and then the spawn refuses and rolls back rather than answer for an agent that
/// will never start (#97). Nothing the spawn does after it can fail.
fn commit(ctx: &Ctx, folder: &Path) -> Res<()> {
    #[cfg(test)]
    COMMIT_PROBE.with(|probe| {
        if let Some(probe) = probe.borrow().as_ref() {
            probe(folder);
        }
    });
    let made = crate::state::fs::claim_json(
        &folder.join("commit.json"),
        &json!({"committedAt": utc_o(ctx.clock.now())}),
    )?;
    if !made {
        return refuse(1, "launch", "the launcher gave up before the commit");
    }
    Ok(())
}

#[cfg(test)]
type CommitProbe = Box<dyn Fn(&Path)>;

#[cfg(test)]
thread_local! {
    // Sees the agent folder just before commit.json is written.
    static COMMIT_PROBE: std::cell::RefCell<Option<CommitProbe>> = const { std::cell::RefCell::new(None) };
}

/// Sets (or clears) the probe that sees the agent folder at the moment commit.json is written.
#[cfg(test)]
fn commit_probe(probe: Option<CommitProbe>) {
    COMMIT_PROBE.with(|slot| *slot.borrow_mut() = probe);
}

/// Made private like the state, agents and mail folders.
fn private_folder(ctx: &Ctx, folder: &Path) -> Res<()> {
    #[cfg(unix)]
    crate::state::private_unix::private_dirs(&ctx.state_root, &[folder])?;
    #[cfg(windows)]
    {
        let _ = ctx;
        crate::state::private_windows::private_dirs(&[folder])?;
    }
    Ok(())
}

/// A value safe unquoted in a multiplexer command string: ASCII letters, digits and `- _ : % @ .` only. Agent ids,
/// names, panes and contexts are always such tokens.
pub(crate) fn plain_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_:%@.".contains(&b))
}

/// The new pane as its creation reply names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    pub pane: String,
    pub window: String,
    // `#{pane_start_command}`, as the multiplexer renders it; may be empty.
    pub command: String,
}

/// `^(%\d+)\|(@\d+)\|(.*)$` after one final LF (and a CR before it) is cut: the start command runs to the end and may
/// hold `|`, but no CR or LF (a reply of more than one line names no pane).
pub(crate) fn created_pane(stdout: &str) -> Option<Created> {
    let line = stdout.strip_suffix('\n').unwrap_or(stdout);
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.contains(['\r', '\n']) {
        return None;
    }
    let mut parts = line.splitn(3, '|');
    let (pane, window, command) = (parts.next()?, parts.next()?, parts.next()?);
    let numbered = |text: &str, mark: char| {
        text.strip_prefix(mark)
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    (numbered(pane, '%') && numbered(window, '@')).then(|| Created {
        pane: pane.into(),
        window: window.into(),
        command: command.into(),
    })
}

pub enum Recipe<'a> {
    First(Option<&'a str>),
    /// Claude forks from `parent` itself. For Copilot, the caller must first copy the parent session into the
    /// `session` passed to `client_args`, as `Launch::run` does, and fall back to `First` if the copy fails.
    Fork {
        parent: &'a str,
        task: Option<&'a str>,
    },
    Resume,
}

/// Arguments after flags: the session selector, then the model, then a nonempty task (today's pane order).
pub fn client_args(
    client: Client,
    recipe: Recipe<'_>,
    session: &str,
    model: Option<&str>,
) -> Vec<String> {
    let (mut args, prompt) = match recipe {
        Recipe::First(task) => (vec!["--session-id".into(), session.into()], task),
        Recipe::Resume => (vec!["--resume".into(), session.into()], None),
        Recipe::Fork { parent, task } => (
            match client {
                Client::Claude => vec![
                    "--resume".into(),
                    parent.into(),
                    "--fork-session".into(),
                    "--session-id".into(),
                    session.into(),
                ],
                Client::Copilot => vec!["--resume".into(), session.into()],
            },
            task,
        ),
    };
    if let Some(model) = model {
        args.extend(["--model".into(), model.into()]);
    }
    if let Some(task) = prompt.filter(|task| !task.is_empty()) {
        if client == Client::Copilot {
            args.push("-i".into());
        }
        args.push(task.into());
    }
    args
}

/// The calling agent's own transcript: Claude's project JSONL or Copilot's events.jsonl; none for a human, a
/// malformed session id or a missing file.
fn parent_transcript(ctx: &Ctx, caller: &Caller) -> Res<Option<PathBuf>> {
    let Some(client) = caller.client.filter(|_| caller.kind == Kind::Agent) else {
        return Ok(None);
    };
    let session = ctx.env.get(session_variable(client)).unwrap_or("");
    if !session_id(session) {
        return Ok(None);
    }
    Ok(match client {
        Client::Claude => crate::checks::claude_transcript(&ctx.home, session)?,
        Client::Copilot => Some(copilot::session_folder(&ctx.home, session).join("events.jsonl"))
            .filter(|events| events.is_file()),
    })
}

/// The five first-turn lines: identity (naming the multiplexer), parent, context, reply and listen. Paths use forward
/// slashes. The binary goes inside double quotes that pwsh and bash expand, so a path outside the entry pattern
/// refuses 1 `entry`.
pub fn header(
    ctx: &Ctx,
    caller: &Caller,
    agent: &Identity,
    plan: &Plan,
    brief: Option<&Path>,
    transcript: Option<&Path>,
) -> Res<Vec<String>> {
    let exe = ctx
        .exe
        .to_str()
        .filter(|exe| entry_ok(exe))
        .ok_or_else(|| Refusal {
            code: 1,
            check: "entry",
            message: ENTRY.into(),
            reason: None,
        })?
        .replace('\\', "/");
    let mux = match ctx.mux.borrow().as_ref().map(|mux| mux.kind()) {
        Some(MuxKind::Tmux) => "tmux",
        Some(MuxKind::Rmux) => "rmux",
        None => return Err(Refusal::internal("the multiplexer is not resolved")),
    };
    let parent = match (caller.kind, caller.client) {
        (Kind::Agent, Some(client)) => format!("{} agent", client_name(client)),
        _ => "root".into(),
    };
    let slashes = |path: Option<&Path>| {
        path.map_or("none".into(), |p| p.to_string_lossy().replace('\\', "/"))
    };
    let context = match plan.context {
        Context::Fork => format!("forked from the parent session ({})", plan.reason),
        Context::Brief => format!(
            "brief-only ({}); brief: {}; parent transcript: {}",
            plan.reason,
            slashes(brief),
            slashes(transcript)
        ),
    };
    Ok(vec![
        format!(
            "[mux-agent] You are {} ({}, id {}), started in this {mux} session.",
            agent.name,
            client_name(agent.client),
            agent.id
        ),
        format!(
            "Your parent is {parent} ({}); report to it with send parent; free text only in files.",
            caller.pane
        ),
        format!("Context: {context}"),
        format!("Reply: \"{exe}\" send parent -BodyFile <file>"),
        format!("Listen: \"{exe}\" read -Wait 600"),
    ])
}

/// `^[\p{L}\p{Nd} _\\/:.()-]+\z`.
fn entry_ok(exe: &str) -> bool {
    !exe.is_empty()
        && exe.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || " _\\/:.()-".contains(c)
                || (!c.is_ascii() && (letter(c) || decimal_digit(c)))
        })
}

// \p{L}: std's Alphabetic is every letter plus the marks, letter numbers and symbols listed below.
fn letter(c: char) -> bool {
    let code = c as u32;
    c.is_alphabetic()
        && !NOT_LETTERS
            .as_chunks::<2>()
            .0
            .iter()
            .any(|range| (range[0]..=range[1]).contains(&code))
}

// \p{Nd}: every decimal digit block is ten code points from its zero.
fn decimal_digit(c: char) -> bool {
    let code = c as u32;
    DIGIT_ZEROS
        .iter()
        .any(|&zero| (zero..zero + 10).contains(&code))
}

/// Mounts the state root into a Unix pane: a pane gets the server's environment, not the caller's, so a root chosen
/// by MUX_AGENTS_STATE or XDG_STATE_HOME in the caller's shell would not reach it. Windows panes need none.
pub fn pane_env(state_root: &str, os: OsKind) -> Vec<String> {
    match os {
        OsKind::Windows => Vec::new(),
        OsKind::Unix => vec!["-e".into(), format!("MUX_AGENTS_STATE={state_root}")],
    }
}

// Unicode 17.0.0 (std's char::UNICODE_VERSION on the toolchain that generated them). An older std knows fewer
// letters, which only refuses more.
const DIGIT_ZEROS: [u32; 76] = [
    0x660, 0x6F0, 0x7C0, 0x966, 0x9E6, 0xA66, 0xAE6, 0xB66, 0xBE6, 0xC66, 0xCE6, 0xD66, 0xDE6,
    0xE50, 0xED0, 0xF20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90, 0x1B50,
    0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0, 0xFF10,
    0x104A0, 0x10D30, 0x10D40, 0x11066, 0x110F0, 0x11136, 0x111D0, 0x112F0, 0x11450, 0x114D0,
    0x11650, 0x116C0, 0x116D0, 0x116DA, 0x11730, 0x118E0, 0x11950, 0x11BF0, 0x11C50, 0x11D50,
    0x11DA0, 0x11DE0, 0x11F50, 0x16130, 0x16A60, 0x16AC0, 0x16B50, 0x16D70, 0x1CCF0, 0x1D7CE,
    0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6, 0x1E140, 0x1E2F0, 0x1E4F0, 0x1E5F1, 0x1E950, 0x1FBF0,
];

/// Inclusive ranges of Alphabetic code points that are not letters (Mn, Mc, Nl, So).
const NOT_LETTERS: [u32; 536] = [
    0x345, 0x345, 0x363, 0x36F, 0x5B0, 0x5BD, 0x5BF, 0x5BF, 0x5C1, 0x5C2, 0x5C4, 0x5C5, 0x5C7,
    0x5C7, 0x610, 0x61A, 0x64B, 0x657, 0x659, 0x65F, 0x670, 0x670, 0x6D6, 0x6DC, 0x6E1, 0x6E4,
    0x6E7, 0x6E8, 0x6ED, 0x6ED, 0x711, 0x711, 0x730, 0x73F, 0x7A6, 0x7B0, 0x816, 0x817, 0x81B,
    0x823, 0x825, 0x827, 0x829, 0x82C, 0x897, 0x897, 0x8D4, 0x8DF, 0x8E3, 0x8E9, 0x8F0, 0x903,
    0x93A, 0x93B, 0x93E, 0x94C, 0x94E, 0x94F, 0x955, 0x957, 0x962, 0x963, 0x981, 0x983, 0x9BE,
    0x9C4, 0x9C7, 0x9C8, 0x9CB, 0x9CC, 0x9D7, 0x9D7, 0x9E2, 0x9E3, 0xA01, 0xA03, 0xA3E, 0xA42,
    0xA47, 0xA48, 0xA4B, 0xA4C, 0xA51, 0xA51, 0xA70, 0xA71, 0xA75, 0xA75, 0xA81, 0xA83, 0xABE,
    0xAC5, 0xAC7, 0xAC9, 0xACB, 0xACC, 0xAE2, 0xAE3, 0xAFA, 0xAFC, 0xB01, 0xB03, 0xB3E, 0xB44,
    0xB47, 0xB48, 0xB4B, 0xB4C, 0xB56, 0xB57, 0xB62, 0xB63, 0xB82, 0xB82, 0xBBE, 0xBC2, 0xBC6,
    0xBC8, 0xBCA, 0xBCC, 0xBD7, 0xBD7, 0xC00, 0xC04, 0xC3E, 0xC44, 0xC46, 0xC48, 0xC4A, 0xC4C,
    0xC55, 0xC56, 0xC62, 0xC63, 0xC81, 0xC83, 0xCBE, 0xCC4, 0xCC6, 0xCC8, 0xCCA, 0xCCC, 0xCD5,
    0xCD6, 0xCE2, 0xCE3, 0xCF3, 0xCF3, 0xD00, 0xD03, 0xD3E, 0xD44, 0xD46, 0xD48, 0xD4A, 0xD4C,
    0xD57, 0xD57, 0xD62, 0xD63, 0xD81, 0xD83, 0xDCF, 0xDD4, 0xDD6, 0xDD6, 0xDD8, 0xDDF, 0xDF2,
    0xDF3, 0xE31, 0xE31, 0xE34, 0xE3A, 0xE4D, 0xE4D, 0xEB1, 0xEB1, 0xEB4, 0xEB9, 0xEBB, 0xEBC,
    0xECD, 0xECD, 0xF71, 0xF83, 0xF8D, 0xF97, 0xF99, 0xFBC, 0x102B, 0x1036, 0x1038, 0x1038, 0x103B,
    0x103E, 0x1056, 0x1059, 0x105E, 0x1060, 0x1062, 0x1064, 0x1067, 0x106D, 0x1071, 0x1074, 0x1082,
    0x108D, 0x108F, 0x108F, 0x109A, 0x109D, 0x16EE, 0x16F0, 0x1712, 0x1713, 0x1732, 0x1733, 0x1752,
    0x1753, 0x1772, 0x1773, 0x17B6, 0x17C8, 0x1885, 0x1886, 0x18A9, 0x18A9, 0x1920, 0x192B, 0x1930,
    0x1938, 0x1A17, 0x1A1B, 0x1A55, 0x1A5E, 0x1A61, 0x1A74, 0x1ABF, 0x1AC0, 0x1ACC, 0x1ACE, 0x1B00,
    0x1B04, 0x1B35, 0x1B43, 0x1B80, 0x1B82, 0x1BA1, 0x1BA9, 0x1BAC, 0x1BAD, 0x1BE7, 0x1BF1, 0x1C24,
    0x1C36, 0x1DD3, 0x1DF4, 0x2160, 0x2182, 0x2185, 0x2188, 0x24B6, 0x24E9, 0x2DE0, 0x2DFF, 0x3007,
    0x3007, 0x3021, 0x3029, 0x3038, 0x303A, 0xA674, 0xA67B, 0xA69E, 0xA69F, 0xA6E6, 0xA6EF, 0xA802,
    0xA802, 0xA80B, 0xA80B, 0xA823, 0xA827, 0xA880, 0xA881, 0xA8B4, 0xA8C3, 0xA8C5, 0xA8C5, 0xA8FF,
    0xA8FF, 0xA926, 0xA92A, 0xA947, 0xA952, 0xA980, 0xA983, 0xA9B4, 0xA9BF, 0xA9E5, 0xA9E5, 0xAA29,
    0xAA36, 0xAA43, 0xAA43, 0xAA4C, 0xAA4D, 0xAA7B, 0xAA7D, 0xAAB0, 0xAAB0, 0xAAB2, 0xAAB4, 0xAAB7,
    0xAAB8, 0xAABE, 0xAABE, 0xAAEB, 0xAAEF, 0xAAF5, 0xAAF5, 0xABE3, 0xABEA, 0xFB1E, 0xFB1E,
    0x10140, 0x10174, 0x10341, 0x10341, 0x1034A, 0x1034A, 0x10376, 0x1037A, 0x103D1, 0x103D5,
    0x10A01, 0x10A03, 0x10A05, 0x10A06, 0x10A0C, 0x10A0F, 0x10D24, 0x10D27, 0x10D69, 0x10D69,
    0x10EAB, 0x10EAC, 0x10EFA, 0x10EFC, 0x11000, 0x11002, 0x11038, 0x11045, 0x11073, 0x11074,
    0x11080, 0x11082, 0x110B0, 0x110B8, 0x110C2, 0x110C2, 0x11100, 0x11102, 0x11127, 0x11132,
    0x11145, 0x11146, 0x11180, 0x11182, 0x111B3, 0x111BF, 0x111CE, 0x111CF, 0x1122C, 0x11234,
    0x11237, 0x11237, 0x1123E, 0x1123E, 0x11241, 0x11241, 0x112DF, 0x112E8, 0x11300, 0x11303,
    0x1133E, 0x11344, 0x11347, 0x11348, 0x1134B, 0x1134C, 0x11357, 0x11357, 0x11362, 0x11363,
    0x113B8, 0x113C0, 0x113C2, 0x113C2, 0x113C5, 0x113C5, 0x113C7, 0x113CA, 0x113CC, 0x113CD,
    0x11435, 0x11441, 0x11443, 0x11445, 0x114B0, 0x114C1, 0x115AF, 0x115B5, 0x115B8, 0x115BE,
    0x115DC, 0x115DD, 0x11630, 0x1163E, 0x11640, 0x11640, 0x116AB, 0x116B5, 0x1171D, 0x1172A,
    0x1182C, 0x11838, 0x11930, 0x11935, 0x11937, 0x11938, 0x1193B, 0x1193C, 0x11940, 0x11940,
    0x11942, 0x11942, 0x119D1, 0x119D7, 0x119DA, 0x119DF, 0x119E4, 0x119E4, 0x11A01, 0x11A0A,
    0x11A35, 0x11A39, 0x11A3B, 0x11A3E, 0x11A51, 0x11A5B, 0x11A8A, 0x11A97, 0x11B60, 0x11B67,
    0x11C2F, 0x11C36, 0x11C38, 0x11C3E, 0x11C92, 0x11CA7, 0x11CA9, 0x11CB6, 0x11D31, 0x11D36,
    0x11D3A, 0x11D3A, 0x11D3C, 0x11D3D, 0x11D3F, 0x11D41, 0x11D43, 0x11D43, 0x11D47, 0x11D47,
    0x11D8A, 0x11D8E, 0x11D90, 0x11D91, 0x11D93, 0x11D96, 0x11EF3, 0x11EF6, 0x11F00, 0x11F01,
    0x11F03, 0x11F03, 0x11F34, 0x11F3A, 0x11F3E, 0x11F40, 0x12400, 0x1246E, 0x1611E, 0x1612E,
    0x16F4F, 0x16F4F, 0x16F51, 0x16F87, 0x16F8F, 0x16F92, 0x16FF0, 0x16FF1, 0x16FF4, 0x16FF6,
    0x1BC9E, 0x1BC9E, 0x1E000, 0x1E006, 0x1E008, 0x1E018, 0x1E01B, 0x1E021, 0x1E023, 0x1E024,
    0x1E026, 0x1E02A, 0x1E08F, 0x1E08F, 0x1E6E3, 0x1E6E3, 0x1E6E6, 0x1E6E6, 0x1E6EE, 0x1E6EF,
    0x1E6F5, 0x1E6F5, 0x1E947, 0x1E947, 0x1F130, 0x1F149, 0x1F150, 0x1F169, 0x1F170, 0x1F189,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::copilot::session_folder;
    use crate::agents::fixture::{CLAUDE, PANE, World};
    use crate::checks::Context;
    use crate::ctx::Env;
    use crate::mux::{Kind as MuxKind, Output};
    use crate::parse::cmdline_length;
    use crate::proc::Row;
    use serde_json::json;
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;
    use std::rc::Rc;

    const OLD: &str = "11111111-2222-4333-8444-555555555555";
    const ENTRY: &str =
        "1 entry mux-agent entry path may hold only letters, digits, spaces and _ \\ / : . ( ) -";

    // The session's bar is off, so a spawn opens none; `bar_on` keeps one.
    fn ready(w: World) -> World {
        w.launches_hold();
        w.fakes
            .mux_rule("new-window", 0, "%9|@5|hc attach t1\n", "");
        w.fakes
            .mux_rule("split-window", 0, "%9|@5|hc attach t1\n", "");
        w.fakes.mux_rule("@mux-agents-sidebar", 0, "off\n", "");
        w
    }

    fn human() -> World {
        ready(World::human(""))
    }

    #[test]
    fn shared_preparation_preserves_pane_metadata() {
        let w = human();
        let caller = w.caller();
        let launch = Launch {
            ctx: &w.ctx,
            caller: &caller,
            client: Client::Claude,
            model: Some("sonnet"),
            no_fork: true,
            task: "task".into(),
            brief: None,
            directory: w.ctx.cwd.clone(),
        };
        let a = launch
            .allocate(&w.state(), "worker".into(), "blue")
            .unwrap();
        let r = launch.record(&a, "%9", "@5", "literal").unwrap();
        assert_eq!(
            (&r["pane"], &r["window"], &r["startCommand"]),
            (&json!("%9"), &json!("@5"), &json!("literal"))
        );
        assert_eq!(r["color"], "blue");
        assert_eq!(r["model"], "sonnet");
        assert_eq!(r["id"], a.id);
        assert_eq!(r["directory"], json!(w.ctx.cwd));
        assert_eq!(r["sessionId"], a.session);
    }

    // An agent caller whose C3, C2 and C5 checks the tests skip, as an admitted pane's are.
    fn agent() -> World {
        let mut w = ready(World::agent(""));
        w.ctx.admitted = true;
        w
    }

    // An agent caller (session OLD) that passes C3, C2 and C5 for real, launched in `<temp>/launch`, not its cwd. A
    // Claude agent runs in bypass mode with a transcript and a session file; a Copilot agent was launched with
    // --allow-all and its session has workspace.yaml (with its id line only when `id_line`) and events.jsonl.
    fn admitted(client: Client, id_line: bool) -> (World, PathBuf) {
        let mut w = ready(World::agent(""));
        let base = w.ctx.cwd.parent().unwrap().to_path_buf();
        let launch = base.join("launch");
        std::fs::create_dir(&launch).unwrap();
        let cwd = launch.to_str().unwrap().to_string();
        w.ctx.home = base.join("home");
        let mut env: Vec<(String, String)> = w
            .ctx
            .env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let argv = match client {
            Client::Claude => {
                env.push(("CLAUDE_CODE_SESSION_ID".into(), OLD.into()));
                env.push(("CLAUDE_CODE_ENTRYPOINT".into(), "cli".into()));
                let project = w.ctx.home.join(".claude").join("projects").join("p");
                std::fs::create_dir_all(&project).unwrap();
                std::fs::write(
                    project.join(format!("{OLD}.jsonl")),
                    "{\"type\":\"permission-mode\",\"permissionMode\":\"bypassPermissions\"}\n",
                )
                .unwrap();
                let sessions = w.ctx.home.join(".claude").join("sessions");
                std::fs::create_dir_all(&sessions).unwrap();
                let session = json!({"cwd": cwd, "procStart": 1500, "sessionId": OLD});
                std::fs::write(sessions.join(format!("{CLAUDE}.json")), session.to_string())
                    .unwrap();
                w.fakes.kernel_cwd(CLAUDE, &cwd);
                // A pane no client has sized: the transcript decides alone.
                w.fakes.mux_rule("#{pane_height}", 0, "2\n", "");
                ["claude", "--dangerously-skip-permissions"]
            }
            Client::Copilot => {
                env.push(("COPILOT_AGENT_SESSION_ID".into(), OLD.into()));
                w.fakes.client(CLAUDE, Client::Copilot);
                let source = session_folder(&w.ctx.home, OLD);
                std::fs::create_dir_all(&source).unwrap();
                let id = if id_line {
                    format!("id: {OLD}\n")
                } else {
                    String::new()
                };
                std::fs::write(source.join("workspace.yaml"), format!("{id}cwd: {cwd}\n")).unwrap();
                std::fs::write(
                    source.join("events.jsonl"),
                    format!(
                        "{{\"type\":\"session.start\",\"data\":{{\"sessionId\":\"{OLD}\"}}}}\n"
                    ),
                )
                .unwrap();
                ["copilot", "--allow-all"]
            }
        };
        w.ctx.env = Env::from_pairs(env);
        w.fakes.process(Row {
            pid: CLAUDE,
            ppid: PANE,
            start: Some(1500),
            path: None,
            argv: Some(argv.map(String::from).to_vec()),
        });
        w.fakes.prefer_last();
        w.file("t.md", b"task");
        w.file("b.md", b"brief");
        (w, launch)
    }

    fn hc_world(w: World) -> World {
        for verb in ["new-window", "split-window"] {
            w.fakes.mux_first(verb, 0, "%9|@5|hc attach t1\n", "");
        }
        w.fakes
            .mux_first("-t %9 #{pane_id}|#{@agent-id}", 0, "|\n", "");
        w
    }
    fn hc_attempt(w: &World, client: Client, model: Option<&str>, rollback: bool) -> Res<Value> {
        use crate::agents::core_fixture as cf;
        let caller = w.caller();
        cf::online(w);
        w.fakes.hc_answer(Ok(crate::spawn::Out {
            stdout: r#"{"address":"t1"}"#.into(),
            ..Default::default()
        }));
        if rollback {
            cf::answer(w, &["kill", "t1", "--json"], "{}");
        }
        let result = launch_hc(
            &w.ctx,
            &caller,
            CoreRequest {
                client,
                task: "hi",
                brief: Some("brief"),
                directory: &w.ctx.cwd,
                model,
                no_fork: false,
                split: None,
            },
            |_| {
                Ok(CoreSlot {
                    name: "hi".into(),
                    color: "blue",
                })
            },
        );
        let folder = agents_dir(w).join(&agent_folders(w)[0]);
        let spec = read_json(&folder.join("spec.json"));
        let id = spec["id"].as_str().unwrap();
        let team = format!("{}:{}", caller.server_key, caller.session);
        let mut run = vec![
            "run".into(),
            "--bg".into(),
            "--keep".into(),
            "--outlive-parent".into(),
            "--no-notify".into(),
            "--name".into(),
            format!("m{}", &id[..8]),
        ];
        for label in [
            "mux.managed=1".into(),
            format!("mux.team={team}"),
            format!("mux.id={id}"),
            "mux.gen=0".into(),
            "mux.name=hi".into(),
        ] {
            run.extend(["--label".into(), label]);
        }
        run.extend([
            "--cwd".into(),
            utf8(&w.ctx.cwd).unwrap().into(),
            "--json".into(),
            "--".into(),
            utf8(&w.ctx.exe).unwrap().into(),
            "launcher".into(),
            utf8(&folder.join("spec.json")).unwrap().into(),
            "--state-root".into(),
            utf8(&w.ctx.state_root).unwrap().into(),
            "--path".into(),
            w.ctx.env.get("PATH").unwrap_or("").into(),
        ]);
        let mut expected = vec![vec!["--version".to_string()], run];
        if rollback {
            expected.push(["kill", "t1", "--json"].map(String::from).to_vec());
        }
        assert_eq!(
            w.fakes
                .hc_calls()
                .iter()
                .map(|(_, a, _)| a.clone())
                .collect::<Vec<_>>(),
            expected
        );
        w.fakes.hc_done().unwrap();
        result
    }

    struct HcCommitReset;
    impl Drop for HcCommitReset {
        fn drop(&mut self) {
            commit_probe(None);
        }
    }

    // Each invocation explicitly queues its one snapshot and, only if admitted, one run.
    fn public_snapshot(w: &World, listed: &[&str], wires: &[Value]) {
        use crate::agents::core_fixture as cf;
        cf::online(w);
        cf::listing(w, &super::super::membership::team(&w.caller()), listed);
        let mut wires = wires.to_vec();
        wires.sort_by_key(|v| v["address"].as_str().unwrap().to_string());
        for wire in wires {
            let address = wire["address"].as_str().unwrap().to_string();
            cf::get(w, &address, wire);
        }
    }

    fn public_launch(w: &World, address: &str) {
        w.fakes.hc_answer(Ok(crate::spawn::Out {
            stdout: json!({"address": address}).to_string(),
            ..Default::default()
        }));
        if address != "t1" {
            for verb in ["new-window", "split-window"] {
                w.fakes
                    .mux_first(verb, 0, &format!("%10|@6|hc attach {address}\n"), "");
            }
        }
    }

    fn empty_launch(w: &World) {
        public_snapshot(w, &[], &[]);
        public_launch(w, "t1");
    }

    fn assert_public_runs(w: &World) {
        for (_, argv, _) in w.fakes.hc_calls().iter().filter(|(_, a, _)| a[0] == "run") {
            let id = argv.iter().find_map(|s| s.strip_prefix("mux.id=")).unwrap();
            let caller = w.caller();
            let folder = w
                .ctx
                .state_root
                .join(&caller.server_key)
                .join("agents")
                .join(id);
            let spec = read_json(&folder.join("spec.json"));
            let record = read_json(&folder.join("record.json"));
            assert_eq!(spec["id"], id);
            let mut expected = vec![
                "run".into(),
                "--bg".into(),
                "--keep".into(),
                "--outlive-parent".into(),
                "--no-notify".into(),
                "--name".into(),
                format!("m{}", &id[..8]),
            ];
            for label in [
                "mux.managed=1".into(),
                format!("mux.team={}:{}", caller.server_key, caller.session),
                format!("mux.id={id}"),
                "mux.gen=0".into(),
                format!("mux.name={}", record["name"].as_str().unwrap()),
            ] {
                expected.extend(["--label".into(), label]);
            }
            expected.extend([
                "--cwd".into(),
                record["directory"].as_str().unwrap().into(),
                "--json".into(),
                "--".into(),
                utf8(&w.ctx.exe).unwrap().into(),
                "launcher".into(),
                utf8(&folder.join("spec.json")).unwrap().into(),
                "--state-root".into(),
                utf8(&w.ctx.state_root).unwrap().into(),
                "--path".into(),
                w.ctx.env.get("PATH").unwrap_or("").into(),
            ]);
            assert_eq!(argv, &expected);
        }
    }

    fn run(w: &World, args: &[&str]) -> String {
        let mut full = vec!["spawn"];
        full.extend(args);
        let result = spawn(&w.ctx, &w.request(&full));
        w.fakes.hc_done().unwrap();
        assert_public_runs(w);
        match result {
            Ok(reply) => format!("ok {}", Value::Object(reply)),
            Err(r) => format!("{} {} {}", r.code, r.check, r.message),
        }
    }

    fn verbs(w: &World) -> Vec<String> {
        w.fakes.mux_calls().iter().map(|c| c[1].clone()).collect()
    }

    fn calls(w: &World, verb: &str) -> Vec<Vec<String>> {
        w.fakes
            .mux_calls()
            .into_iter()
            .filter(|c| c[1] == verb)
            .collect()
    }

    // `<root>/<serverKey>/agents`; asking for the key probes the caller once more.
    fn agents_dir(w: &World) -> PathBuf {
        w.ctx.state_root.join(w.caller().server_key).join("agents")
    }

    fn agent_folders(w: &World) -> Vec<String> {
        match std::fs::read_dir(agents_dir(w)) {
            Ok(entries) => entries
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    fn reply(text: &str) -> Value {
        serde_json::from_str(text.strip_prefix("ok ").expect(text)).unwrap()
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    fn header_text(w: &World, caller: &Caller, plan: &Plan, brief: Option<&Path>) -> String {
        let me = Identity {
            name: "claude-2",
            id: "0123456789ab",
            client: Client::Claude,
        };
        match header(&w.ctx, caller, &me, plan, brief, None) {
            Ok(lines) => lines.join("\n"),
            Err(r) => format!("{} {} {}", r.code, r.check, r.message),
        }
    }

    fn fork() -> Plan {
        Plan {
            context: Context::Fork,
            reason: "same-client",
            parent_session: None,
        }
    }

    #[test]
    fn usage_rules_client_task_split() {
        let usage = format!("2 usage {}", crate::agents::usage("spawn").message);
        for args in [
            &["-Task", "hi"][..],
            &["-Client", "Claude", "-Task", "hi"],
            &["-Client", "../x", "-Task", "hi"],
            &["-Client", "claude"],
            &["-Client", "claude", "-Task", "hi", "-TaskFile", "t.md"],
            &["-Client", "claude", "-Task", "hi", "-Split", "H"],
            &[
                "-Client",
                "claude",
                "-Task",
                "hi",
                "-Split",
                "h;kill-server",
            ],
            &["-Client", "claude", "-Task", "hi", "-Split", ""],
            &["-Client", "claude", "-Task", "hi", "-Tab", "-Split", "v"],
        ] {
            let w = human();
            assert_eq!(run(&w, args), usage, "{args:?}");
            assert!(w.fakes.mux_calls().is_empty(), "{args:?}");
            assert!(!w.ctx.state_root.exists(), "{args:?}");
            assert!(w.fakes.hc_calls().is_empty());
        }
        // Task and brief files: UTF-8, 16 KB and 1 MB, relative to the caller's cwd.
        let w = human();
        w.file("big.md", &[b'x'; 16385]);
        w.file("bad.md", b"a\xc3\x28");
        w.file("empty.md", b"");
        w.file("huge.md", &vec![b'x'; (1 << 20) + 1]);
        for (args, message) in [
            (&["-Task", ""][..], "the task is empty"),
            (&["-TaskFile", "empty.md"], "the task is empty"),
            (
                &["-TaskFile", "missing.md"],
                "a task or brief file cannot be read",
            ),
            (
                &["-TaskFile", "big.md"],
                "a task or brief file is larger than 16384 bytes",
            ),
            (
                &["-TaskFile", "bad.md"],
                "a task or brief file is not UTF-8",
            ),
            (
                &["-Task", "hi", "-BriefFile", "huge.md"],
                "a task or brief file is larger than 1048576 bytes",
            ),
            (
                &["-Task", "hi", "-BriefFile", "missing.md"],
                "a task or brief file cannot be read",
            ),
        ] {
            let mut full = vec!["-Client", "claude"];
            full.extend(args);
            w.fakes.clear_log();
            assert_eq!(run(&w, &full), format!("2 usage {message}"), "{args:?}");
            assert_eq!(verbs(&w), ["display-message", "list-panes"], "{args:?}");
            assert!(!w.ctx.state_root.exists(), "{args:?}");
            assert!(w.fakes.hc_calls().is_empty());
        }
        // At the limits, and with an empty brief, it spawns.
        w.file("max.md", &[b'x'; 16384]);
        w.file("brief.md", &vec![b'x'; 1 << 20]);
        empty_launch(&w);
        let max = run(
            &w,
            &[
                "-Client",
                "claude",
                "-TaskFile",
                "max.md",
                "-BriefFile",
                "brief.md",
            ],
        );
        assert!(max.starts_with("ok "), "{max}");
        let first = reply(&max);
        let record = read_json(&w.folder(first["id"].as_str().unwrap()).join("record.json"));
        public_snapshot(&w, &[], &[crate::agents::core_fixture::wire(&record)]);
        public_launch(&w, "t2");
        let empty = run(
            &w,
            &["-Client", "claude", "-Task", "hi", "-BriefFile", "empty.md"],
        );
        assert!(empty.starts_with("ok "), "{empty}");
    }

    #[test]
    fn recipes_omit_empty_tasks() {
        for client in [Client::Claude, Client::Copilot] {
            assert_eq!(
                client_args(client, Recipe::First(Some("")), "own", None),
                client_args(client, Recipe::First(None), "own", None),
                "empty task is absent"
            );
            assert_eq!(
                client_args(client, Recipe::First(Some(" ")), "own", None)
                    .last()
                    .map(String::as_str),
                Some(" ")
            );
        }
    }

    #[test]
    fn fork_recipe_keeps_parent_only_on_first_launch() {
        for client in [Client::Claude, Client::Copilot] {
            for task in [None, Some(""), Some(" "), Some("task --resume parent")] {
                let mut expected: Vec<&str> = match client {
                    Client::Claude => vec![
                        "--resume",
                        "parent",
                        "--fork-session",
                        "--session-id",
                        "own",
                    ],
                    Client::Copilot => vec!["--resume", "own"],
                };
                expected.extend(["--model", "sonnet"]);
                if let Some(t) = task.filter(|t| !t.is_empty()) {
                    if client == Client::Copilot {
                        expected.push("-i");
                    }
                    expected.push(t);
                }
                assert_eq!(
                    client_args(
                        client,
                        Recipe::Fork {
                            parent: "parent",
                            task,
                        },
                        "own",
                        Some("sonnet")
                    ),
                    expected
                );
                assert_eq!(
                    client_args(client, Recipe::Resume, "own", Some("sonnet")),
                    ["--resume", "own", "--model", "sonnet"]
                );
            }
        }
    }

    #[test]
    fn recipe_matrix() {
        let id = "0f11fc2f-2032-48ef-a920-4af2a8e5af50";
        for c in [Client::Claude, Client::Copilot] {
            for task in [None, Some(""), Some("task --fork-session parent")] {
                let mut expected = vec!["--session-id".to_string(), id.into()];
                if let Some(t) = task.filter(|t| !t.is_empty()) {
                    if c == Client::Copilot {
                        expected.push("-i".into());
                    }
                    expected.push(t.into());
                }
                assert_eq!(client_args(c, Recipe::First(task), id, None), expected);
                assert_eq!(client_args(c, Recipe::Resume, id, None), ["--resume", id]);
                let mut modeled = expected;
                modeled.splice(2..2, ["--model".into(), "sonnet".into()]);
                assert_eq!(
                    client_args(c, Recipe::First(task), id, Some("sonnet")),
                    modeled
                );
            }
            let mut whole = vec![client_name(c).to_string()];
            whole.extend(client_flags(c).iter().map(|s| s.to_string()));
            whole.extend(client_args(c, Recipe::Resume, id, Some("sonnet")));
            let prefix = match c {
                Client::Claude => vec![
                    "claude",
                    "--dangerously-skip-permissions",
                    "--effort",
                    "high",
                    "--resume",
                    id,
                    "--model",
                    "sonnet",
                ],
                Client::Copilot => vec![
                    "copilot",
                    "--no-sandbox",
                    "--allow-all",
                    "--resume",
                    id,
                    "--model",
                    "sonnet",
                ],
            };
            assert_eq!(whole, prefix);
        }
    }

    #[test]
    fn model_value_checked_before_any_call() {
        let usage = format!("2 usage {}", crate::agents::usage("spawn").message);
        let long = "a".repeat(65);
        for model in ["-x", "a b", long.as_str(), "", ".x", "a;b", "a/b", "\u{e9}"] {
            let w = human();
            assert_eq!(
                run(&w, &["-Client", "claude", "-Task", "hi", "-Model", model]),
                usage,
                "{model}"
            );
            assert!(w.fakes.mux_calls().is_empty(), "{model}");
            assert!(w.fakes.hc_calls().is_empty());
            assert!(!w.ctx.state_root.exists(), "{model}");
        }
        let max = "a".repeat(64);
        for model in ["sonnet", "sonnet[1m]", "claude-opus-4.5_x", max.as_str()] {
            let w = human();
            empty_launch(&w);
            let ok = reply(&run(
                &w,
                &["-Client", "claude", "-Task", "hi", "-Model", model],
            ));
            let spec = read_json(
                &agents_dir(&w)
                    .join(ok["id"].as_str().unwrap())
                    .join("spec.json"),
            );
            assert_eq!(
                spec["args"].as_array().unwrap()[..4],
                [
                    json!("--session-id"),
                    ok["sessionId"].clone(),
                    json!("--model"),
                    json!(model)
                ],
                "{model}"
            );
        }
    }

    #[test]
    fn model_reaches_every_client_and_plan() {
        for (client, no_fork) in [
            (Client::Claude, false),
            (Client::Claude, true),
            (Client::Copilot, false),
            (Client::Copilot, true),
        ] {
            let case = format!("{client:?} no-fork {no_fork}");
            let (w, launch) = admitted(client, true);
            let mut args = vec![
                "-Client",
                client_name(client),
                "-TaskFile",
                "t.md",
                "-BriefFile",
                "b.md",
                "-Model",
                "sonnet",
            ];
            if no_fork {
                args.push("-NoFork");
            }
            empty_launch(&w);
            let ok = reply(&run(&w, &args));
            let new = ok["sessionId"].as_str().unwrap();
            let spec = read_json(
                &agents_dir(&w)
                    .join(ok["id"].as_str().unwrap())
                    .join("spec.json"),
            );
            let prompt = spec["prompt"].clone();
            // Before the prompt; for Copilot before -i, which takes the prompt.
            let expected = match (client, no_fork) {
                (Client::Claude, false) => json!([
                    "--resume",
                    OLD,
                    "--fork-session",
                    "--session-id",
                    new,
                    "--model",
                    "sonnet",
                    prompt
                ]),
                (Client::Claude, true) => {
                    json!(["--session-id", new, "--model", "sonnet", prompt])
                }
                (Client::Copilot, false) => {
                    json!(["--resume", new, "--model", "sonnet", "-i", prompt])
                }
                (Client::Copilot, true) => {
                    json!(["--session-id", new, "--model", "sonnet", "-i", prompt])
                }
            };
            assert_eq!(spec["args"], expected, "{case}");
            // The child, a caller in its turn, passes the flags check with its whole command line.
            let mut argv = vec![client_name(client).to_string()];
            argv.extend(client_flags(client).iter().map(|f| f.to_string()));
            argv.extend(
                spec["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| a.as_str().unwrap().to_string()),
            );
            w.fakes.process(Row {
                pid: CLAUDE,
                ppid: PANE,
                start: Some(1500),
                path: None,
                argv: Some(argv),
            });
            w.fakes.prefer_last();
            assert_eq!(
                assert_spawn_allowed(&w.ctx, &w.caller()),
                Ok(launch),
                "{case}"
            );
        }
    }

    #[test]
    fn tagged_caller_refuses() {
        for tag in ["0123456789ab", "x"] {
            let w = ready(World::human(tag));
            assert_eq!(
                run(&w, &["-Client", "claude", "-Task", "hi"]),
                "2 tagged an agent pane cannot spawn agents",
                "{tag}"
            );
            assert_eq!(verbs(&w), ["display-message"], "{tag}");
            assert!(!w.ctx.state_root.exists(), "{tag}");
        }
    }

    #[test]
    fn agent_task_inline_refused() {
        let w = agent();
        assert_eq!(
            run(&w, &["-Client", "claude", "-Task", "x"]),
            "2 usage agents pass free text only in files: use -TaskFile"
        );
        assert_eq!(verbs(&w), ["display-message", "list-panes"]);
        assert!(!w.ctx.state_root.exists());
    }

    fn reservations(w: &World, count: u32, foreign: bool) -> Vec<Value> {
        (1..=count)
            .map(|n| {
                let id = format!("{:012x}", 0xa00 + n);
                let record = crate::agents::core_fixture::member(
                    w,
                    &id,
                    json!({"name": format!("claude-{n}")}),
                );
                if foreign {
                    std::fs::remove_dir_all(w.folder(&id)).unwrap();
                } else {
                    std::fs::remove_file(w.folder(&id).join("commit.json")).unwrap();
                }
                crate::agents::core_fixture::wire(&record)
            })
            .collect()
    }

    fn reservation_snapshot(w: &World, wires: &[Value], foreign: bool) {
        let addresses: Vec<_> = wires
            .iter()
            .map(|v| v["address"].as_str().unwrap())
            .collect();
        public_snapshot(w, if foreign { &addresses } else { &[] }, wires);
    }

    #[test]
    fn cap_6_counts_uncommitted() {
        for foreign in [false, true] {
            let w = human();
            let six = reservations(&w, 6, foreign);
            reservation_snapshot(&w, &six, foreign);
            assert_eq!(
                run(&w, &["-Client", "claude", "-Task", "hi"]),
                "2 cap the session already has 6 live agents"
            );
            assert!(calls(&w, "new-window").is_empty());
            assert!(!w.state().join("server.json").exists());
            assert_eq!(agent_folders(&w).len(), if foreign { 0 } else { 6 });
            for (task, name) in [("do it", "do"), ("just it", "claude-6")] {
                let w = human();
                let five = reservations(&w, 5, foreign);
                reservation_snapshot(&w, &five, foreign);
                public_launch(&w, "t1");
                let ok = reply(&run(&w, &["-Client", "claude", "-Task", task]));
                assert_eq!(ok["name"], name);
                let caller = w.caller();
                assert_eq!(
                    read_json(&w.state().join("server.json")),
                    json!({"socket": caller.socket, "serverPid": caller.server_pid, "serverStart": caller.server_start})
                );
            }
        }
        for name in ["worker6\nhidden", "worker6\r\nhidden", "worker6\rhidden"] {
            let w = human();
            let mut six = reservations(&w, 6, true);
            six[5]["labels"]["mux.name"] = name.into();
            reservation_snapshot(&w, &six, true);
            assert_eq!(
                run(&w, &["-Client", "claude", "-Task", "hi"]),
                "1 internal unexpected error (reservation has no managed name)"
            );
            assert!(calls(&w, "new-window").is_empty());
            assert!(!w.state().join("server.json").exists());
        }
    }

    // The new agent's record.json: the one folder `before` did not have.
    fn new_record(w: &World, before: &[String]) -> Value {
        let new: Vec<_> = agent_folders(w)
            .into_iter()
            .filter(|f| !before.contains(f))
            .collect();
        assert_eq!(new.len(), 1, "{new:?}");
        read_json(&agents_dir(w).join(&new[0]).join("record.json"))
    }

    #[test]
    fn spawn_colors_the_least_used_hue_of_live_agents() {
        use crate::agents::core_fixture as cf;
        let spawn_color = |w: &World, listed: &[&str], wires: &[Value]| {
            let before = agent_folders(w);
            public_snapshot(w, listed, wires);
            public_launch(w, "t1");
            let ok = run(w, &["-Client", "claude", "-Task", "hi", "-Name", "new"]);
            assert!(ok.starts_with("ok "), "{ok}");
            new_record(w, &before)["color"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(spawn_color(&human(), &[], &[]), "blue");
        for (colors, want) in [
            (&["blue"][..], "magenta"),
            (&["blue", "magenta"], "bright-blue"),
            (&["magenta", "blue"], "bright-blue"),
            (
                &["blue", "magenta", "bright-blue", "bright-magenta"],
                "blue",
            ),
            (
                &["blue", "blue", "magenta", "bright-blue", "bright-magenta"],
                "magenta",
            ),
        ] {
            for mode in ["running", "lost", "unknown"] {
                let w = human();
                w.panes(&[]);
                let wires: Vec<_> = colors
                    .iter()
                    .enumerate()
                    .map(|(n, color)| {
                        let r = cf::member(
                            &w,
                            &format!("{:012x}", 0xd00 + n),
                            json!({"name": format!("a{n}"), "color": color}),
                        );
                        let mut wire = cf::wire(&r);
                        if mode == "lost" {
                            wire["argv"] = json!(["/bin/sh", "-l"]);
                        }
                        if mode == "unknown" {
                            wire["labels"]["mux.gen"] = "1".into();
                        }
                        wire
                    })
                    .collect();
                assert_eq!(spawn_color(&w, &[], &wires), want, "{colors:?} {mode}");
            }
        }
        let w = human();
        let a = cf::member(&w, "000000000e01", json!({"name": "a", "color": "blue"}));
        let b = cf::member(&w, "000000000e02", json!({"name": "b", "color": "magenta"}));
        let mut done = cf::wire(&a);
        done["state"] = "finished".into();
        done["reason"] = "exited".into();
        done["exit_code"] = 0.into();
        assert_eq!(spawn_color(&w, &[], &[done, cf::wire(&b)]), "blue");
        let w = human();
        cf::member(
            &w,
            "000000000e03",
            json!({"name": "x", "color": "blue", "session": "$4"}),
        );
        let r = cf::member(&w, "000000000e04", json!({"name": "worker"}));
        let hashed = crate::paint::identity(None, "worker");
        let want = if hashed == "blue" { "magenta" } else { "blue" };
        assert_eq!(spawn_color(&w, &[], &[cf::wire(&r)]), want);
        let w = human();
        let r = cf::member(
            &w,
            "000000000e05",
            json!({"name": "foreign", "color": "blue"}),
        );
        std::fs::remove_dir_all(w.folder("000000000e05")).unwrap();
        assert_eq!(spawn_color(&w, &["t000000000e05"], &[cf::wire(&r)]), "blue");
    }

    #[test]
    fn ended_agents_names_are_released_only_by_hc() {
        use crate::agents::core_fixture as cf;
        for (name, task, requested) in [
            ("add-subtract", "Add subtract", None),
            ("w1", "hi", Some("w1")),
            ("worker", "Worker", Some("worker")),
        ] {
            for mode in ["ended", "running", "lost", "unknown"] {
                let w = human();
                let mut wires = Vec::new();
                // More ended history than a board shows must not hide the oldest reusable name.
                let count = if name == "w1" && mode == "ended" {
                    11
                } else {
                    1
                };
                for n in 1..=count {
                    let r = cf::member(
                        &w,
                        &format!("{:012x}", 0xb00 + n),
                        json!({"name": if n == 1 { name.to_string() } else { format!("w{n}") }}),
                    );
                    let mut wire = cf::wire(&r);
                    if mode == "ended" {
                        wire["state"] = "finished".into();
                        wire["reason"] = "exited".into();
                        wire["exit_code"] = 0.into();
                    }
                    if mode == "lost" {
                        wire["argv"] = json!(["/bin/sh", "-l"]);
                    }
                    if mode == "unknown" {
                        wire["labels"]["mux.gen"] = "1".into();
                    }
                    wires.push(wire);
                }
                // A live suffix never prevents reuse of the verified Ended base name.
                if name == "add-subtract" {
                    let suffix = cf::member(&w, "000000000b20", json!({"name": "add-subtract-2"}));
                    wires.push(cf::wire(&suffix));
                }
                public_snapshot(&w, &[], &wires);
                let mut args = vec!["-Client", "claude", "-Task", task];
                if let Some(requested) = requested {
                    args.extend(["-Name", requested]);
                }
                if mode == "ended" || requested.is_none() {
                    public_launch(&w, "t1");
                    let ok = reply(&run(&w, &args));
                    assert_eq!(
                        ok["name"],
                        if mode == "ended" {
                            name.into()
                        } else {
                            format!("{name}-3")
                        }
                    );
                } else {
                    assert_eq!(
                        run(&w, &args),
                        format!("2 name agent name is taken, try {name}-2")
                    );
                    assert!(calls(&w, "new-window").is_empty());
                }
            }
        }
        // Identity-free legacy history, another session and an empty folder reserve nothing.
        let w = human();
        w.agent_record("000000000b01", json!({"name": "add-subtract"}));
        cf::member(
            &w,
            "000000000b04",
            json!({"name": "add-subtract", "session": "$4"}),
        );
        std::fs::create_dir_all(w.folder("000000000b05")).unwrap();
        empty_launch(&w);
        assert_eq!(
            reply(&run(&w, &["-Client", "claude", "-Task", "add subtract"]))["name"],
            "add-subtract"
        );
    }

    #[test]
    fn an_unreadable_record_refuses_rather_than_reuse_a_name() {
        use crate::agents::core_fixture as cf;
        let id = "000000000b07";
        for case in [
            "not JSON",
            "not an object",
            "unreadable",
            "another id",
            "no name",
            "no session",
            "non-string session",
        ] {
            let w = human();
            let mut record = cf::member(&w, id, json!({"name": "w"}));
            let path = w.folder(id).join("record.json");
            match case {
                "not JSON" => std::fs::write(&path, "{").unwrap(),
                "not an object" => std::fs::write(&path, "[\"w\"]").unwrap(),
                "unreadable" => {
                    std::fs::remove_file(&path).unwrap();
                    std::fs::create_dir(&path).unwrap();
                }
                _ => {
                    match case {
                        "another id" => record["id"] = "000000000b06".into(),
                        "no name" => record["name"] = Value::Null,
                        "no session" => {
                            record.as_object_mut().unwrap().remove("session");
                        }
                        _ => record["session"] = 3.into(),
                    }
                    crate::agents::fixture::write(&path, record);
                }
            }
            for args in [
                &["-Task", "add subtract"][..],
                &["-Task", "hi", "-Name", "w"],
            ] {
                cf::online(&w); // Local validation precedes ls.
                let mut full = vec!["-Client", "claude"];
                full.extend(args);
                let got = run(&w, &full);
                if case == "unreadable" {
                    assert!(got.starts_with("1 internal unexpected error ("), "{got}");
                } else {
                    let message = match case {
                        "not JSON" => "state file is not JSON",
                        "not an object" => "invalid local hc record",
                        _ => "invalid local hc identity",
                    };
                    assert_eq!(
                        got,
                        format!("1 internal unexpected error ({message})"),
                        "{case}"
                    );
                }
                assert!(!w.fakes.hc_calls().iter().any(|(_, a, _)| a[0] == "run"));
            }
            assert!(calls(&w, "new-window").is_empty(), "{case}");
        }
    }

    #[test]
    fn spawn_reaps_before_the_cap_inside_its_lock() {
        for ended in [true, false] {
            let w = human();
            w.clock("2026-10-29T12:00:00.0000000Z");
            let mut six = reservations(&w, 6, false);
            let id = "000000000a06";
            let folder = w.folder(id);
            let mut record = read_json(&folder.join("record.json"));
            record["createdAt"] = "2026-10-29T11:58:00.0000000Z".into();
            crate::agents::fixture::write(&folder.join("record.json"), record);
            if ended {
                six[5]["state"] = "finished".into();
                six[5]["reason"] = "exited".into();
                six[5]["exit_code"] = 7.into();
            }
            reservation_snapshot(&w, &six, false);
            w.fakes.clear_log();
            let c = w.caller();
            w.fakes.clear_log();
            if ended {
                public_launch(&w, "t1");
                let ok = reply(&run(&w, &["-Client", "claude", "-Task", "hi"]));
                assert_eq!(ok["name"], "hi");
                assert_eq!(read_json(&folder.join("notice.json"))["exitCode"], 7);
                let spawn = lock_key("spawn", &c, "$3");
                let member = lock_key("member", &c, id);
                let notice = lock_key("notice", &c, id);
                let mail = lock_key("mail", &c, "pane-1");
                let bar = lock_key("sidebar", &c, "$3");
                assert_eq!(
                    w.fakes.locks(),
                    [
                        format!("+{spawn}"),
                        format!("+{member}"),
                        format!("+{notice}"),
                        format!("+{mail}"),
                        format!("-{mail}"),
                        format!("-{notice}"),
                        format!("-{member}"),
                        format!("-{spawn}"),
                        format!("+{bar}"),
                        format!("-{bar}")
                    ]
                );
            } else {
                assert_eq!(
                    run(&w, &["-Client", "claude", "-Task", "hi"]),
                    "2 cap the session already has 6 live agents"
                );
                assert!(!folder.join("notice.json").exists());
                assert_eq!(
                    w.fakes.locks(),
                    [
                        format!("+{}", lock_key("spawn", &c, "$3")),
                        format!("-{}", lock_key("spawn", &c, "$3"))
                    ]
                );
            }
            // Authentication and membership each list panes once; assertions above do not relist membership.
            assert_eq!(
                w.fakes
                    .hc_calls()
                    .iter()
                    .filter(|(_, a, _)| a[0] == "ls")
                    .count(),
                1
            );
            assert_eq!(
                w.fakes
                    .hc_calls()
                    .iter()
                    .filter(|(_, a, _)| a[0] == "get")
                    .count(),
                6
            );
            assert!(
                calls(&w, "if-shell")
                    .iter()
                    .all(|a| !a.last().unwrap().starts_with("kill-pane"))
            );
        }
    }

    #[test]
    fn brief_required_for_agent_brief_plan() {
        // A Claude agent without a session id starts brief-only, so it must pass a brief.
        let w = agent();
        w.file("t.md", b"task");
        w.file("b.md", b"brief text\n");
        for client in ["claude", "copilot"] {
            public_snapshot(&w, &[], &[]);
            assert_eq!(
                run(&w, &["-Client", client, "-TaskFile", "t.md"]),
                "2 brief this agent starts brief-only, so an agent caller passes -BriefFile",
                "{client}"
            );
            assert!(calls(&w, "new-window").is_empty());
            assert!(agent_folders(&w).is_empty());
        }
        empty_launch(&w);
        let ok = reply(&run(
            &w,
            &[
                "-Client",
                "copilot",
                "-TaskFile",
                "t.md",
                "-BriefFile",
                "b.md",
            ],
        ));
        assert_eq!(
            (&ok["context"], &ok["reason"]),
            (&json!("brief"), &json!("cross-client"))
        );
        let folder = agents_dir(&w).join(ok["id"].as_str().unwrap());
        assert_eq!(
            std::fs::read_to_string(folder.join("brief.md")).unwrap(),
            "brief text\n"
        );
        let record = read_json(&folder.join("record.json"));
        assert_eq!(
            record["parentAgent"],
            json!({"client": "claude", "pid": CLAUDE, "start": 1500, "sessionId": null})
        );
        let prompt = read_json(&folder.join("spec.json"))["prompt"].clone();
        let brief = folder.join("brief.md").to_str().unwrap().replace('\\', "/");
        assert_eq!(
            prompt.as_str().unwrap().split('\n').nth(2).unwrap(),
            format!("Context: brief-only (cross-client); brief: {brief}; parent transcript: none")
        );
        // A human needs no brief.
        let w = human();
        empty_launch(&w);
        assert!(run(&w, &["-Client", "claude", "-Task", "hi"]).starts_with("ok "));
        // Not admitted, an agent caller is checked (C3 first) before the lock.
        let w = ready(World::agent(""));
        w.file("t.md", b"task");
        w.file("b.md", b"brief");
        assert_eq!(
            run(
                &w,
                &[
                    "-Client",
                    "claude",
                    "-TaskFile",
                    "t.md",
                    "-BriefFile",
                    "b.md"
                ]
            ),
            "2 flags a client command line cannot be read"
        );
        assert!(w.fakes.locks().is_empty());
        assert!(!w.ctx.state_root.exists());
    }

    #[test]
    fn header_five_lines_abs_exe_forward_slashes() {
        let mut w = human();
        w.ctx.exe = PathBuf::from(r"C:\Program Files (x86)\mux\mux-agent.exe");
        let human = w.caller();
        let exe = "C:/Program Files (x86)/mux/mux-agent.exe";
        let tail = format!(
            "Reply: \"{exe}\" send parent -BodyFile <file>\nListen: \"{exe}\" read -Wait 600"
        );
        // A fork names neither the brief nor a transcript.
        assert_eq!(
            header_text(&w, &human, &fork(), Some(Path::new(r"C:\b\brief.md"))),
            format!(
                "[mux-agent] You are claude-2 (claude, id 0123456789ab), started in this tmux session.\n\
                 Your parent is root (%1); report to it with send parent; free text only in files.\n\
                 Context: forked from the parent session (same-client)\n{tail}"
            )
        );
        let brief = Plan {
            context: Context::Brief,
            reason: "human",
            parent_session: None,
        };
        let text = header_text(&w, &human, &brief, Some(Path::new(r"C:\b dir\brief.md")));
        assert_eq!(
            text.split('\n').nth(2).unwrap(),
            "Context: brief-only (human); brief: C:/b dir/brief.md; parent transcript: none"
        );
        assert!(!text.contains('\\'));
        assert_eq!(
            header_text(&w, &human, &brief, None)
                .split('\n')
                .nth(2)
                .unwrap(),
            "Context: brief-only (human); brief: none; parent transcript: none"
        );
        let agent = World::agent("").caller();
        assert_eq!(
            header_text(&w, &agent, &fork(), None)
                .split('\n')
                .nth(1)
                .unwrap(),
            "Your parent is claude agent (%1); report to it with send parent; free text only in files."
        );
    }

    #[test]
    fn header_names_tmux_or_rmux() {
        for (kind, name) in [(MuxKind::Tmux, "tmux"), (MuxKind::Rmux, "rmux")] {
            let w = human();
            w.fakes.mux_kind(kind);
            let caller = w.caller();
            let first = header_text(&w, &caller, &fork(), None);
            assert_eq!(
                first.split('\n').next().unwrap(),
                format!(
                    "[mux-agent] You are claude-2 (claude, id 0123456789ab), started in this {name} session."
                )
            );
            empty_launch(&w);
            let ok = reply(&run(&w, &["-Client", "claude", "-Task", "hi"]));
            let spec = read_json(
                &agents_dir(&w)
                    .join(ok["id"].as_str().unwrap())
                    .join("spec.json"),
            );
            assert!(
                spec["prompt"]
                    .as_str()
                    .unwrap()
                    .contains(&format!("started in this {name} session.")),
                "{name}"
            );
        }
    }

    #[test]
    fn entry_pattern_refuses_1_entry() {
        let mut w = human();
        let caller = w.caller();
        let allowed = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789 _\\/:.()-";
        let mut cases: Vec<(char, bool)> = (0x20u8..=0x7e)
            .map(|b| (b as char, allowed.contains(b as char)))
            .collect();
        // Controls; numbers that are not decimal digits; a no-break space; a combining mark; Unicode quotes and dashes.
        for c in [
            '\0', '\u{7}', '\t', '\n', '\r', '\u{7f}', '\u{b2}', '\u{2162}', '\u{a0}', '\u{301}',
            '\u{201c}', '\u{201d}', '\u{201e}', '\u{2018}', '\u{2019}', '\u{2013}', '\u{2014}',
        ] {
            cases.push((c, false));
        }
        // CJK, Cyrillic and Greek letters, Arabic-Indic and fullwidth decimal digits and a fullwidth letter.
        for c in [
            '\u{4e2d}', '\u{416}', '\u{3a9}', '\u{661}', '\u{ff11}', '\u{ff21}',
        ] {
            cases.push((c, true));
        }
        assert_eq!(cases.len(), 118);
        let mut wrong = Vec::new();
        for (c, ok) in cases {
            w.ctx.exe = PathBuf::from(format!("C:\\a{c}b\\mux-agent.exe"));
            let text = header_text(&w, &caller, &fork(), None);
            if text.starts_with(ENTRY) == ok {
                wrong.push(format!("U+{:04X}", c as u32));
            }
        }
        assert_eq!(wrong, Vec::<String>::new());
        for exe in ["", "/bin/mux-agent\n"] {
            w.ctx.exe = PathBuf::from(exe);
            assert_eq!(header_text(&w, &caller, &fork(), None), ENTRY, "{exe:?}");
        }
        // A refused entry launches nothing and leaves no agent folder.
        w.ctx.exe = PathBuf::from("/x\"$(y)/mux-agent");
        w.fakes.clear_log();
        public_snapshot(&w, &[], &[]);
        assert_eq!(
            run(&w, &["-Client", "claude", "-Task", "hi"]),
            format!("{ENTRY}; abort evidence could not be written")
        );
        assert!(calls(&w, "new-window").is_empty());
        assert!(agent_folders(&w).is_empty());
    }

    #[test]
    fn cmdline_over_32767_refuses() {
        for (client, flags) in [
            (
                "claude",
                &["--dangerously-skip-permissions", "--effort", "high"][..],
            ),
            ("copilot", &["--no-sandbox", "--allow-all"]),
        ] {
            assert_eq!(
                client_flags(Client::Claude).len() + client_flags(Client::Copilot).len(),
                5
            );
            // The model's two arguments count toward the limit like every other.
            for model in [&[][..], &["-Model", "sonnet"]] {
                let w = human();
                let mut args = vec!["-Client", client, "-Task", "x", "-Name", "w"];
                args.extend(model);
                empty_launch(&w);
                let ok = reply(&run(&w, &args));
                let spec = read_json(
                    &agents_dir(&w)
                        .join(ok["id"].as_str().unwrap())
                        .join("spec.json"),
                );
                let mut argv: Vec<&str> = flags.to_vec();
                argv.extend(
                    spec["args"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|a| a.as_str().unwrap()),
                );
                assert_eq!(argv.contains(&"--model"), !model.is_empty(), "{client}");
                let base = 1024 + cmdline_length(&argv);
                // Each '"' costs 2 on the command line; the 'x' it replaces cost 1. Exactly 32767 fits.
                let room = 32768 - base;
                let fit = format!("{}{}", "\"".repeat(room / 2), "x".repeat(room % 2));
                for (task, fits) in [(fit.clone(), true), (format!("{fit}x"), false)] {
                    let w = human();
                    let mut args = vec!["-Client", client, "-Task", &task, "-Name", "w"];
                    args.extend(model);
                    public_snapshot(&w, &[], &[]);
                    if fits {
                        public_launch(&w, "t1");
                    }
                    let text = run(&w, &args);
                    if fits {
                        assert!(text.starts_with("ok "), "{client} {model:?} {}", task.len());
                    } else {
                        assert_eq!(
                            text,
                            "2 usage task is too long for the client command line; abort evidence could not be written",
                            "{client} {model:?}"
                        );
                        assert!(calls(&w, "new-window").is_empty());
                        assert!(agent_folders(&w).is_empty());
                    }
                }
            }
        }
    }

    #[test]
    fn a_refused_allow_set_title_still_spawns() {
        // tmux 3.2a has no allow-set-title: its refusal is best effort, so the agent still commits, tab or split.
        for extra in [&[][..], &["-Split", "h"]] {
            let w = human();
            w.fakes
                .mux_first("allow-set-title", 1, "", "invalid option: allow-set-title");
            let mut args = vec!["-Client", "claude", "-Task", "hi"];
            args.extend(extra);
            empty_launch(&w);
            let ok = reply(&run(&w, &args));
            assert_eq!(ok["name"], "hi", "{extra:?}");
            let folder = agents_dir(&w).join(ok["id"].as_str().unwrap());
            assert!(folder.join("commit.json").is_file(), "{extra:?}");
            // Bound like the tags: written only while %9 still runs the command its reply named.
            let titles: Vec<Vec<String>> = calls(&w, "if-shell")
                .into_iter()
                .filter(|c| c.iter().any(|t| t.contains("allow-set-title")))
                .collect();
            assert_eq!(
                titles,
                [[
                    "-N",
                    "if-shell",
                    "-F",
                    "-t",
                    "%9",
                    "#{==:#{pane_start_command},hc attach t1}",
                    "set-option -p -t %9 allow-set-title off"
                ]
                .map(String::from)],
                "{extra:?}"
            );
        }
    }

    #[test]
    fn pane_cmd_is_exe_launcher_spec() {
        let w = human();
        empty_launch(&w);
        let ok = reply(&run(&w, &["-Client", "claude", "-Task", "hi"]));
        let id = ok["id"].as_str().unwrap();
        let spec = agents_dir(&w).join(id).join("spec.json");
        let mut expected: Vec<String> = [
            "-N",
            "new-window",
            "-d",
            "-P",
            "-F",
            "#{pane_id}|#{window_id}|#{pane_start_command}",
            "-t",
            "$3:",
            "-n",
            "hi",
        ]
        .map(String::from)
        .to_vec();
        expected.extend([
            "-c".into(),
            w.ctx.cwd.to_str().unwrap().into(),
            w.fakes.hc_install("hc").to_str().unwrap().into(),
            "attach".into(),
            "t1".into(),
        ]);
        assert_eq!(calls(&w, "new-window"), [expected]);
        assert!(!calls(&w, "new-window")[0].iter().any(|a| a == "-e"));
        assert!(calls(&w, "split-window").is_empty());
        assert_eq!(
            read_json(&spec),
            json!({"schema": 1, "stateDirectory": agents_dir(&w).parent().unwrap().to_str().unwrap(),
                "id": id, "client": "claude", "parentPane": "%1",
                "args": ["--session-id", ok["sessionId"], read_json(&spec)["prompt"]],
                "prompt": read_json(&spec)["prompt"], "generation": "0"})
        );
        let keys: Vec<String> = read_json(&spec)
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            keys,
            [
                "schema",
                "stateDirectory",
                "id",
                "client",
                "parentPane",
                "args",
                "prompt",
                "generation"
            ]
        );
        let prompt = read_json(&spec)["prompt"].as_str().unwrap().to_string();
        assert!(prompt.ends_with("\n\nhi"), "{prompt}");
        assert_eq!(prompt.split('\n').count(), 7);
        for split in ["h", "v"] {
            let w = human();
            empty_launch(&w);
            let ok = reply(&run(
                &w,
                &["-Client", "copilot", "-Task", "hi", "-Split", split],
            ));
            let spec = agents_dir(&w)
                .join(ok["id"].as_str().unwrap())
                .join("spec.json");
            let mut expected: Vec<String> = [
                "-N",
                "split-window",
                &format!("-{split}"),
                "-d",
                "-P",
                "-F",
                "#{pane_id}|#{window_id}|#{pane_start_command}",
                "-t",
                "%1",
            ]
            .map(String::from)
            .to_vec();
            expected.extend([
                "-c".into(),
                w.ctx.cwd.to_str().unwrap().into(),
                w.fakes.hc_install("hc").to_str().unwrap().into(),
                "attach".into(),
                "t1".into(),
            ]);
            assert_eq!(calls(&w, "split-window"), [expected], "{split}");
            assert!(!calls(&w, "split-window")[0].iter().any(|a| a == "-e"));
            assert!(calls(&w, "new-window").is_empty());
            assert_eq!(
                read_json(&spec)["args"].as_array().unwrap()[..2],
                [json!("--session-id"), ok["sessionId"].clone()]
            );
        }
        // An agent caller, admitted by C3, C2 and C5: the launch directory they verified (not its cwd) is the pane's
        // -c and the record's directory, and the client arguments are whole for each client and context.
        for (client, no_fork) in [
            (Client::Claude, false),
            (Client::Claude, true),
            (Client::Copilot, false),
            (Client::Copilot, true),
        ] {
            let (w, launch) = admitted(client, true);
            assert_ne!(launch, w.ctx.cwd);
            let mut args = vec![
                "-Client",
                client_name(client),
                "-TaskFile",
                "t.md",
                "-BriefFile",
                "b.md",
            ];
            if no_fork {
                args.push("-NoFork");
            }
            empty_launch(&w);
            let ok = reply(&run(&w, &args));
            let new = ok["sessionId"].as_str().unwrap();
            let folder = agents_dir(&w).join(ok["id"].as_str().unwrap());
            let spec = folder.join("spec.json");
            let prompt = read_json(&spec)["prompt"].clone();
            let expected = match (client, no_fork) {
                (Client::Claude, false) => {
                    json!([
                        "--resume",
                        OLD,
                        "--fork-session",
                        "--session-id",
                        new,
                        prompt
                    ])
                }
                (Client::Claude, true) => json!(["--session-id", new, prompt]),
                (Client::Copilot, false) => json!(["--resume", new, "-i", prompt]),
                (Client::Copilot, true) => json!(["--session-id", new, "-i", prompt]),
            };
            let case = format!("{client:?} no-fork {no_fork}");
            assert_eq!(read_json(&spec)["args"], expected, "{case}");
            let context = if no_fork { "brief" } else { "fork" };
            assert_eq!(ok["context"], context, "{case}");
            let window = calls(&w, "new-window");
            assert_eq!(window.len(), 1, "{case}");
            let tail: Vec<&str> = window[0][window[0].len() - 5..]
                .iter()
                .map(String::as_str)
                .collect();
            assert_eq!(
                tail,
                [
                    "-c",
                    launch.to_str().unwrap(),
                    w.fakes.hc_install("hc").to_str().unwrap(),
                    "attach",
                    "t1"
                ],
                "{case}"
            );
            assert_eq!(
                read_json(&folder.join("record.json"))["directory"],
                launch.to_str().unwrap(),
                "{case}"
            );
        }
    }

    const PLACEMENT_READ: &str = "-N show-options -qv -t $3 @mux-agents-placement";

    fn placement_reads(w: &World) -> usize {
        placement_reads_in(&w.fakes)
    }

    fn placement_reads_in(fakes: &crate::testing::Fakes) -> usize {
        fakes
            .mux_calls()
            .iter()
            .filter(|c| c.join(" ") == PLACEMENT_READ)
            .count()
    }

    // Where the agent opened, by its launch call: `tab` for a new window, else the split's `h` or `v`.
    fn opened(w: &World) -> String {
        let launch = w
            .fakes
            .mux_calls()
            .into_iter()
            .find(|c| c.iter().any(|a| a == "attach"))
            .expect("a launch call");
        match launch[1].as_str() {
            "new-window" => "tab".into(),
            _ => {
                // A split beside the caller: `split-window -<h|v> -d -P -F <format> -t %1`.
                assert_eq!((launch[3].as_str(), launch[8].as_str()), ("-d", "%1"));
                launch[2].trim_start_matches('-').into()
            }
        }
    }

    #[test]
    fn placement_flag_then_session_then_tab() {
        // (extra args, the stored option's answer, where the agent opens, the source). `throw` is a transport
        // failure, `exit` a nonzero answer.
        for (extra, stored, place, source) in [
            (&[][..], "h\n", "h", "session"),
            (&[], "v\r\n", "v", "session"),
            (&[], "tab\n", "tab", "session"),
            (&[], "", "tab", "default"),
            (&[], "\n", "tab", "default"),
            (&[], "garbage\n", "tab", "default"),
            (&[], "H\n", "tab", "default"),
            (&[], "exit", "tab", "default"),
            (&[], "throw", "tab", "default"),
            (&["-Split", "v"], "h\n", "v", "flag"),
            (&["-Split", "h"], "", "h", "flag"),
            (&["-Tab"], "h\n", "tab", "flag"),
            (&["-tab"], "v\n", "tab", "flag"),
        ] {
            let w = human();
            match stored {
                "exit" => w.fakes.mux_rule("show-options", 1, "", "no server"),
                "throw" => w.fakes.mux_script("show-options", |_| {
                    refuse(1, "endpoint", "the rmux endpoint did not answer")
                }),
                text => w.fakes.mux_rule("show-options", 0, text, ""),
            }
            let fakes = w.fakes.clone();
            let reads = usize::from(extra.is_empty());
            w.fakes.hc_probe(move |a| {
                if a[0] == "--version" {
                    assert_eq!(placement_reads_in(&fakes), reads);
                    assert!(fakes.locks().is_empty());
                }
            });
            let fakes = w.fakes.clone();
            w.fakes.lock_probe(move |event| {
                if event.starts_with("+spawn") {
                    format!("reads {}", placement_reads_in(&fakes))
                } else {
                    String::new()
                }
            });
            let mut args = vec!["-Client", "claude", "-Task", "hi"];
            args.extend(extra);
            let case = format!("{extra:?} {stored:?}");
            empty_launch(&w);
            let ok = reply(&run(&w, &args));
            assert_eq!(
                (&ok["placement"], &ok["placementSource"]),
                (&json!(place), &json!(source)),
                "{case}"
            );
            assert_eq!(opened(&w), place, "{case}");
            // Read only without a flag, once, and before the spawn lock is taken.
            let reads = usize::from(extra.is_empty());
            assert_eq!(placement_reads(&w), reads, "{case}");
            let key = w.caller().server_key;
            assert_eq!(
                w.fakes.locks()[0],
                format!("+spawn|{key}|$3 reads {reads}"),
                "{case}"
            );
        }
    }

    // A human caller whose session keeps its bar: the bar's split answers %13 in the caller's window @1, run by pid
    // 113 (`<exe> watch`, start 1130), and from then on the session's record, the bar's proof and the label list name
    // it. `open` says whether the bar is there.
    fn bar_on() -> (World, Rc<Cell<bool>>) {
        let w = World::human("");
        w.launches_hold();
        w.fakes
            .mux_rule("new-window", 0, "%9|@5|hc attach t1\n", "");
        let exe = w.ctx.exe.display().to_string();
        w.fakes.process(crate::proc::Row {
            pid: 113,
            ppid: 900,
            start: Some(1130),
            path: Some(exe.clone()),
            argv: Some(vec![exe, "watch".into()]),
        });
        let open = Rc::new(Cell::new(false));
        let seen = open.clone();
        w.fakes.mux_script("split-window", move |argv| {
            let bar = argv.last().is_some_and(|a| a == "watch");
            if bar {
                seen.set(true);
            }
            Ok(Output {
                code: 0,
                stdout: if bar {
                    "%13|@1|113\n"
                } else {
                    "%9|@5|hc attach t1\n"
                }
                .into(),
                stderr: String::new(),
            })
        });
        let record = Rc::new(RefCell::new(String::new()));
        w.fakes.mux_script("@mux-agents-sidebar-id", move |argv| {
            let mut record = record.borrow_mut();
            let stdout = match (argv[1].as_str(), argv[2].as_str()) {
                ("show-options", _) if !record.is_empty() => format!("{record}\n"),
                ("set-option", "-u") => {
                    record.clear();
                    String::new()
                }
                ("set-option", _) => {
                    *record = argv[5].clone();
                    String::new()
                }
                _ => String::new(),
            };
            Ok(Output {
                code: 0,
                stdout,
                stderr: String::new(),
            })
        });
        // The label read back after open's guarded label write.
        w.fakes.mux_rule(
            "display-message -p -t %13 #{@agent-role}",
            0,
            "sidebar\n",
            "",
        );
        let seen = open.clone();
        w.fakes.mux_script("#{pane_dead}|#{pane_pid}", move |_| {
            Ok(Output {
                code: 0,
                stdout: if seen.get() {
                    "%13|@1|0|113\n"
                } else {
                    "|||\n"
                }
                .into(),
                stderr: String::new(),
            })
        });
        let seen = open.clone();
        w.fakes
            .mux_script("#{==:#{@agent-role},sidebar}", move |_| {
                Ok(Output {
                    code: 0,
                    stdout: if seen.get() { "%13|1\n" } else { "" }.into(),
                    stderr: String::new(),
                })
            });
        (w, open)
    }

    // The splits that ran the side bar.
    fn bar_splits(w: &World) -> usize {
        calls(w, "split-window")
            .iter()
            .filter(|c| c.last().is_some_and(|a| a == "watch"))
            .count()
    }

    fn keys(reply: &Value) -> Vec<String> {
        reply.as_object().unwrap().keys().cloned().collect()
    }

    #[test]
    fn spawn_keeps_one_bar_after_commit() {
        let (w, _) = bar_on();
        let agents = agents_dir(&w);
        let key = w.caller().server_key;
        w.fakes.lock_probe(move |event| {
            if !event.starts_with("+sidebar") {
                return String::new();
            }
            // The agent is committed, and the spawn lock released, before the bar is looked for.
            let committed = std::fs::read_dir(&agents).map_or(0, |dir| {
                dir.flatten()
                    .filter(|e| e.path().join("commit.json").is_file())
                    .count()
            });
            format!("committed {committed}")
        });
        empty_launch(&w);
        let first = reply(&run(&w, &["-Client", "claude", "-Task", "hi"]));
        assert_eq!(first["sidebar"], "%13");
        assert_eq!(
            keys(&first)[8..],
            [
                "model",
                "placement",
                "placementSource",
                "sidebar",
                "life",
                "view",
                "hcAddress"
            ]
        );
        assert_eq!(
            w.fakes.locks(),
            [
                format!("+spawn|{key}|$3"),
                format!("-spawn|{key}|$3"),
                format!("+sidebar|{key}|$3 committed 1"),
                format!("-sidebar|{key}|$3"),
            ]
        );
        assert_eq!(bar_splits(&w), 1);
        // The bar opens at the right edge of the caller's window, full height.
        let bar = calls(&w, "split-window").pop().unwrap();
        assert_eq!(bar[2..5], ["-d", "-f", "-h"]);
        assert_eq!(bar[9], "%1");
        // A second spawn, a split by the session's placement too, finds the same bar and splits no second one.
        w.fakes.clear_log();
        w.fakes.mux_rule("@mux-agents-placement", 0, "h\n", "");
        let record = read_json(&w.folder(first["id"].as_str().unwrap()).join("record.json"));
        public_snapshot(&w, &[], &[crate::agents::core_fixture::wire(&record)]);
        public_launch(&w, "t2");
        let second = reply(&run(&w, &["-Client", "claude", "-Task", "hi"]));
        assert_eq!(
            (&second["sidebar"], &second["placement"]),
            (&json!("%13"), &json!("h"))
        );
        assert_eq!(bar_splits(&w), 0);
        assert_eq!(
            (&first["hcAddress"], &second["hcAddress"]),
            (&json!("t1"), &json!("t2"))
        );
    }

    #[test]
    fn spawn_with_bar_off_opens_nothing() {
        let (w, _) = bar_on();
        w.fakes.mux_first("@mux-agents-sidebar", 0, "off\n", "");
        empty_launch(&w);
        let ok = reply(&run(&w, &["-Client", "claude", "-Task", "hi"]));
        assert_eq!(ok["sidebar"], Value::Null);
        assert_eq!(&keys(&ok)[11..], ["sidebar", "life", "view", "hcAddress"]);
        assert_eq!(bar_splits(&w), 0);
        assert!(
            calls(&w, "list-panes")
                .iter()
                .all(|c| !c.iter().any(|a| a.contains("#{@agent-role}")))
        );
    }

    #[test]
    fn sidebar_failure_after_commit_still_spawns() {
        let unknown = crate::sidebar::unproven(&["%13".to_string()]);
        for (case, message) in [
            ("lock", "timed out waiting for the agent lock"),
            ("option exit", "tmux call failed"),
            ("option throw", "the rmux endpoint did not answer"),
            ("probe", unknown.as_str()),
            ("open", "tmux call failed"),
        ] {
            let (w, open) = bar_on();
            match case {
                "lock" => w.fakes.lock_timeout("sidebar|"),
                "option exit" => w.fakes.mux_first("@mux-agents-sidebar", 1, "", "no server"),
                "option throw" => w.fakes.mux_script("@mux-agents-sidebar", |_| {
                    refuse(1, "endpoint", "the rmux endpoint did not answer")
                }),
                "probe" => {
                    // A recorded bar whose proof cannot be read: its label refuses a second bar.
                    open.set(true);
                    w.fakes.mux_first(
                        "show-options -qv -t $3 @mux-agents-sidebar-id",
                        0,
                        "%13 113 1130\n",
                        "",
                    );
                    w.fakes
                        .mux_first("#{pane_dead}|#{pane_pid}", 1, "", "no server");
                }
                _ => w.fakes.mux_script("split-window", |argv| {
                    let bar = argv.last().is_some_and(|a| a == "watch");
                    Ok(Output {
                        code: i32::from(bar),
                        stdout: if bar { "" } else { "%9|@5|hc attach t1\n" }.into(),
                        stderr: "no space for new pane\n".into(),
                    })
                }),
            }
            empty_launch(&w);
            let ok = reply(&run(&w, &["-Client", "claude", "-Task", "hi"]));
            assert_eq!(
                (&ok["sidebar"], &ok["sidebarError"]),
                (&Value::Null, &json!(message)),
                "{case}"
            );
            assert_eq!(
                keys(&ok)[8..],
                [
                    "model",
                    "placement",
                    "placementSource",
                    "sidebar",
                    "sidebarError",
                    "life",
                    "view",
                    "hcAddress"
                ],
                "{case}"
            );
            // The agent is live: committed, with its record.
            let folder = agents_dir(&w).join(ok["id"].as_str().unwrap());
            assert!(folder.join("commit.json").is_file(), "{case}");
            assert!(calls(&w, "kill-pane").is_empty(), "{case}");
            // A failed probe or read opens no bar.
            if case != "open" {
                assert_eq!(bar_splits(&w), 0, "{case}");
            }
        }
        // A panic in the bar is caught there too: the committed agent's reply still goes out, so no retry adds one.
        let (w, _) = bar_on();
        w.fakes
            .mux_script("@mux-agents-sidebar", |_| panic!("PANIC-PAYLOAD-bar"));
        empty_launch(&w);
        let ok = reply(&run(&w, &["-Client", "claude", "-Task", "hi"]));
        assert_eq!(
            (&ok["sidebar"], &ok["sidebarError"]),
            (&Value::Null, &json!("unexpected error (panic)"))
        );
        let folder = agents_dir(&w).join(ok["id"].as_str().unwrap());
        assert!(folder.join("commit.json").is_file());
        let (w, _) = bar_on();
        w.fakes.lock_timeout("sidebar|");
        empty_launch(&w);
        assert!(run(&w, &["-Client", "claude", "-Task", "hi"]).starts_with("ok "));
        let key = w.caller().server_key;
        assert_eq!(
            w.fakes.locks(),
            [
                format!("+spawn|{key}|$3"),
                format!("-spawn|{key}|$3"),
                format!("!sidebar|{key}|$3"),
            ]
        );
    }

    #[test]
    fn placement_never_masks_a_refusal() {
        // -Tab with -Split is usage, before any call.
        let w = human();
        assert_eq!(
            run(
                &w,
                &["-Client", "claude", "-Task", "hi", "-Tab", "-Split", "h"]
            ),
            format!("2 usage {}", crate::agents::usage("spawn").message)
        );
        assert!(w.fakes.mux_calls().is_empty());
        // A tagged caller and an agent caller refused by its checks are refused before the option is read.
        let w = ready(World::human("0123456789ab"));
        w.fakes.mux_rule("show-options", 1, "", "no server");
        assert_eq!(
            run(&w, &["-Client", "claude", "-Task", "hi"]),
            "2 tagged an agent pane cannot spawn agents"
        );
        assert_eq!(placement_reads(&w), 0);
        let w = ready(World::agent(""));
        w.file("t.md", b"task");
        w.file("b.md", b"brief");
        let args = [
            "-Client",
            "claude",
            "-TaskFile",
            "t.md",
            "-BriefFile",
            "b.md",
        ];
        assert_eq!(
            run(&w, &args),
            "2 flags a client command line cannot be read"
        );
        assert_eq!(placement_reads(&w), 0);
        // The cap is still the cap when the option cannot be read, or holds a split.
        for (code, stored) in [(1, ""), (0, "h\n")] {
            let w = human();
            w.fakes.mux_rule("show-options", code, stored, "");
            let six = reservations(&w, 6, false);
            reservation_snapshot(&w, &six, false);
            assert_eq!(
                run(&w, &["-Client", "claude", "-Task", "hi"]),
                "2 cap the session already has 6 live agents",
                "{stored:?}"
            );
            assert!(calls(&w, "new-window").is_empty() && calls(&w, "split-window").is_empty());
        }
    }

    #[test]
    fn header_names_parent_transcript() {
        // A cross-client agent starts brief-only; its header names the parent's own transcript: Claude's project
        // JSONL or Copilot's events.jsonl.
        for (parent, child) in [(Client::Claude, "copilot"), (Client::Copilot, "claude")] {
            let (w, _) = admitted(parent, true);
            let transcript = match parent {
                Client::Claude => w
                    .ctx
                    .home
                    .join(".claude")
                    .join("projects")
                    .join("p")
                    .join(format!("{OLD}.jsonl")),
                Client::Copilot => session_folder(&w.ctx.home, OLD).join("events.jsonl"),
            };
            empty_launch(&w);
            let ok = reply(&run(
                &w,
                &["-Client", child, "-TaskFile", "t.md", "-BriefFile", "b.md"],
            ));
            assert_eq!(ok["reason"], "cross-client", "{parent:?}");
            let folder = agents_dir(&w).join(ok["id"].as_str().unwrap());
            let prompt = read_json(&folder.join("spec.json"))["prompt"].clone();
            let slashes = |p: &Path| p.to_str().unwrap().replace('\\', "/");
            assert_eq!(
                prompt.as_str().unwrap().split('\n').nth(2).unwrap(),
                format!(
                    "Context: brief-only (cross-client); brief: {}; parent transcript: {}",
                    slashes(&folder.join("brief.md")),
                    slashes(&transcript)
                ),
                "{parent:?}"
            );
        }
    }

    #[test]
    fn copilot_copy_failed_starts_brief() {
        // The session copy fails its check (workspace.yaml has no id line): the fork turns brief-only, so an agent
        // caller must pass -BriefFile; nothing is launched or copied without it.
        let (w, _) = admitted(Client::Copilot, false);
        let sessions = session_folder(&w.ctx.home, OLD)
            .parent()
            .unwrap()
            .to_path_buf();
        let names = || {
            let mut names: Vec<String> = std::fs::read_dir(&sessions)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };
        public_snapshot(&w, &[], &[]);
        assert_eq!(
            run(&w, &["-Client", "copilot", "-TaskFile", "t.md"]),
            "2 brief this agent starts brief-only, so an agent caller passes -BriefFile"
        );
        assert!(calls(&w, "new-window").is_empty());
        assert!(agent_folders(&w).is_empty());
        assert_eq!(names(), [OLD]);
        // With a brief it starts a fresh session, and says why everywhere.
        empty_launch(&w);
        let ok = reply(&run(
            &w,
            &[
                "-Client",
                "copilot",
                "-TaskFile",
                "t.md",
                "-BriefFile",
                "b.md",
            ],
        ));
        assert_eq!(
            (&ok["context"], &ok["reason"]),
            (&json!("brief"), &json!("copy-verification-failed"))
        );
        assert_eq!(names(), [OLD]);
        let new = ok["sessionId"].as_str().unwrap();
        let folder = agents_dir(&w).join(ok["id"].as_str().unwrap());
        let spec = read_json(&folder.join("spec.json"));
        let prompt = spec["prompt"].clone();
        assert_eq!(spec["args"], json!(["--session-id", new, "-i", prompt]));
        let tags = calls(&w, "if-shell")
            .into_iter()
            .find(|c| c[6].contains("@agent-id"))
            .unwrap();
        assert!(
            tags[6].contains("@agent-context brief:copy-verification-failed"),
            "{tags:?}"
        );
        let slashes = |p: &Path| p.to_str().unwrap().replace('\\', "/");
        assert_eq!(
            prompt.as_str().unwrap().split('\n').nth(2).unwrap(),
            format!(
                "Context: brief-only (copy-verification-failed); brief: {}; parent transcript: {}",
                slashes(&folder.join("brief.md")),
                slashes(&session_folder(&w.ctx.home, OLD).join("events.jsonl"))
            )
        );
        let record = read_json(&folder.join("record.json"));
        assert_eq!(
            (&record["context"], &record["reason"]),
            (&json!("brief"), &json!("copy-verification-failed"))
        );
    }

    #[test]
    fn unix_pane_env_e_state_root() {
        assert_eq!(
            pane_env("/home/u/.local/state/mux agents/state", OsKind::Unix),
            [
                "-e",
                "MUX_AGENTS_STATE=/home/u/.local/state/mux agents/state"
            ]
        );
        assert!(
            pane_env(
                r"C:\Users\u\AppData\Local\mux-agents\state",
                OsKind::Windows
            )
            .is_empty()
        );
    }

    #[test]
    fn failure_after_launch_writes_abort_kills_pane() {
        let kill = [
            "-N",
            "if-shell",
            "-F",
            "-t",
            "%9",
            "#{==:#{pane_start_command},hc attach t1}",
            "kill-pane -t %9",
        ]
        .map(String::from);
        for (gone, closed) in [("|\n", true), ("%9|\n", false), ("", false)] {
            let w = hc_world(human());
            w.fakes.mux_first("@agent-context", 1, "", "no pane");
            w.fakes
                .mux_first("-t %9 #{pane_id}|#{@agent-id}", 0, gone, "");
            let e = hc_attempt(&w, Client::Claude, None, true).unwrap_err();
            assert_eq!(
                (e.code, e.check, e.message.as_str()),
                (1, "rmux", "tmux call failed")
            );
            let folder = agents_dir(&w).join(&agent_folders(&w)[0]);
            let abort = read_json(&folder.join("abort.json"));
            let mut identity = read_json(&folder.join("record.json"));
            for k in ["pane", "window", "startCommand"] {
                identity.as_object_mut().unwrap().remove(k);
            }
            assert_eq!(
                abort,
                json!({"hcAddress":"t1","identity":identity,"pane":"%9","kill":"acknowledged","viewCleanup":{"gone":closed}})
            );
            assert!(folder.join("record.json").is_file());
            assert!(!folder.join("commit.json").exists());
            let kills: Vec<_> = calls(&w, "if-shell")
                .into_iter()
                .filter(|c| c[6].starts_with("kill-pane"))
                .collect();
            assert_eq!(kills, [kill.to_vec()]);
            assert!(calls(&w, "kill-pane").is_empty());
        }
        for (code, out, message) in [
            (0, "%9|@5|\n", "unexpected error (unproven new view)"),
            (1, "%9|@5|hc attach t1\n", "tmux call failed"),
            (0, "", "unexpected error (unproven new view)"),
            (
                0,
                "%9|@5|hc attach t1\n%10|@6\n",
                "unexpected error (unproven new view)",
            ),
            (0, "%9|@5\n", "unexpected error (unproven new view)"),
            (
                0,
                "%9|@5;kill-server\n",
                "unexpected error (unproven new view)",
            ),
            (
                0,
                "#{pane_id}|@5|x\n",
                "unexpected error (unproven new view)",
            ),
            (0, "%9 |@5|x\n", "unexpected error (unproven new view)"),
        ] {
            let w = hc_world(human());
            w.fakes.mux_first("new-window", code, out, "");
            let e = hc_attempt(&w, Client::Claude, None, true).unwrap_err();
            assert_eq!(
                (e.code, e.check, e.message.as_str()),
                (1, if code == 1 { "rmux" } else { "internal" }, message)
            );
            let folder = agents_dir(&w).join(&agent_folders(&w)[0]);
            let a = read_json(&folder.join("abort.json"));
            assert_eq!(
                (&a["hcAddress"], &a["pane"], &a["kill"], &a["viewCleanup"]),
                (
                    &json!("t1"),
                    &Value::Null,
                    &json!("acknowledged"),
                    &json!("unproven")
                )
            );
            assert!(folder.join("record.json").is_file());
            assert!(!folder.join("commit.json").exists());
            assert!(calls(&w, "if-shell").is_empty());
            assert!(calls(&w, "set-option").is_empty());
        }
        let w = hc_world(human());
        w.fakes
            .mux_first("new-window", 0, "%9|@5|/b/hc|binary attach t1\r\n", "");
        let r = hc_attempt(&w, Client::Claude, None, false).unwrap();
        assert_eq!(
            (&r["pane"], &r["window"], &r["startCommand"]),
            (&json!("%9"), &json!("@5"), &json!("/b/hc|binary attach t1"))
        );
        for fail in [true, false] {
            let (w, _) = admitted(Client::Copilot, true);
            let w = hc_world(w);
            let source = session_folder(&w.ctx.home, OLD);
            if fail {
                w.fakes.mux_first("@agent-context", 1, "", "bad");
            }
            let got = hc_attempt(&w, Client::Copilot, None, fail);
            assert_eq!(got.is_err(), fail);
            let folder = agents_dir(&w).join(&agent_folders(&w)[0]);
            let record = read_json(&folder.join("record.json"));
            assert_eq!(
                (&record["context"], &record["reason"]),
                (&json!("fork"), &json!("same-client"))
            );
            let new = record["sessionId"].as_str().unwrap();
            let copy = session_folder(&w.ctx.home, new);
            assert!(copy.join("workspace.yaml").is_file());
            assert!(copy.join("events.jsonl").is_file());
            assert_eq!(
                std::fs::read_dir(source.parent().unwrap()).unwrap().count(),
                2
            );
            assert_eq!(
                read_json(&folder.join("spec.json"))["args"]
                    .as_array()
                    .unwrap()[..3],
                [json!("--resume"), json!(new), json!("-i")]
            );
            assert_eq!(crate::agents::committed(&folder), !fail);
            assert_eq!(folder.join("abort.json").exists(), fail);
        }
    }

    #[test]
    fn record_then_commit_order() {
        let w = hc_world(human());
        let _reset = HcCommitReset;
        let agents = agents_dir(&w);
        let log = Rc::new(RefCell::new(Vec::new()));
        let (seen, dir) = (log.clone(), agents.clone());
        w.fakes.mux_script("new-window", move |_| {
            let folder = std::fs::read_dir(&dir)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            assert!(
                folder.join("record.json").is_file(),
                "hc identity must precede attachment"
            );
            let r = read_json(&folder.join("record.json"));
            assert_eq!(r["hcAddress"], "t1");
            assert!(r.get("pane").is_none());
            assert!(folder.join("spec.json").is_file());
            assert!(!folder.join("commit.json").exists());
            seen.borrow_mut().push("attach");
            Ok(Output {
                stdout: "%9|@5|hc attach t1\n".into(),
                ..Output::default()
            })
        });
        let (seen, dir) = (log.clone(), agents.clone());
        w.fakes.mux_script("@agent-context", move |_| {
            let folder = std::fs::read_dir(&dir)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let r = read_json(&folder.join("record.json"));
            assert_eq!(r["hcAddress"], "t1");
            assert!(r.get("pane").is_none());
            assert!(!folder.join("commit.json").exists());
            seen.borrow_mut().push("tag");
            Ok(Output::default())
        });
        let seen = log.clone();
        w.fakes.mux_script("select-pane -T", move |_| {
            seen.borrow_mut().push("title");
            Ok(Output::default())
        });
        let seen = log.clone();
        w.fakes.mux_script("allow-set-title", move |_| {
            seen.borrow_mut().push("option");
            Ok(Output::default())
        });
        let seen = log.clone();
        commit_probe(Some(Box::new(move |folder| {
            let r = read_json(&folder.join("record.json"));
            assert_eq!(
                (&r["pane"], &r["window"], &r["startCommand"]),
                (&json!("%9"), &json!("@5"), &json!("hc attach t1"))
            );
            assert!(!folder.join("commit.json").exists());
            seen.borrow_mut().push("commit");
        })));
        let r = hc_attempt(&w, Client::Claude, None, false).unwrap();
        assert_eq!(
            *log.borrow(),
            ["attach", "tag", "title", "option", "commit"]
        );
        commit_probe(None);
        let id = r["id"].as_str().unwrap();
        let folder = agents.join(id);
        let caller = w.caller();
        let spec = folder.join("spec.json");
        let argv = json!([
            utf8(&w.ctx.exe).unwrap(),
            "launcher",
            utf8(&spec).unwrap(),
            "--state-root",
            utf8(&w.ctx.state_root).unwrap(),
            "--path",
            w.ctx.env.get("PATH").unwrap_or("")
        ]);
        let expected = json!({"schema":1,"id":id,"name":"hi","client":"claude","sessionId":r["sessionId"],
            "context":"brief","reason":"human","session":"$3","team":format!("{}:$3",caller.server_key),"generation":"0","parentPane":"%1","parentAgent":null,
            "directory":w.ctx.cwd,"taskLine":"hi","createdAt":"1970-01-01T00:00:00.0000000Z","color":"blue",
            "hcAddress":"t1","launcherArgv":argv,"pane":"%9","window":"@5","startCommand":"hc attach t1"});
        assert_eq!(
            std::fs::read_to_string(folder.join("record.json")).unwrap(),
            expected.to_string()
        );
        assert_eq!(r.to_string(), expected.to_string());
        assert_eq!(read_json(&spec)["generation"], "0");
        assert_eq!(
            read_json(&folder.join("commit.json")),
            json!({"committedAt":"1970-01-01T00:00:00.0000000Z"})
        );
        let bound = |command: String| {
            [
                "-N",
                "if-shell",
                "-F",
                "-t",
                "%9",
                "#{==:#{pane_start_command},hc attach t1}",
            ]
            .map(String::from)
            .into_iter()
            .chain([command])
            .collect::<Vec<_>>()
        };
        assert_eq!(
            calls(&w, "if-shell"),
            [
                bound(format!(
                    "set-option -p -t %9 @agent-id {id} ; set-option -p -t %9 @agent-name hi ; set-option -p -t %9 @agent-parent %1 ; set-option -p -t %9 @agent-context brief:human"
                )),
                bound("select-pane -T hi -t %9".into()),
                bound("set-option -p -t %9 allow-set-title off".into())
            ]
        );
        assert!(calls(&w, "set-option").is_empty());
        assert!(calls(&w, "select-pane").is_empty());
        assert!(w.fakes.mux_calls().iter().any(|a| {
            a == &[
                "-N",
                "display-message",
                "-p",
                "-t",
                "%9",
                "#{pane_id}|#{==:#{pane_start_command},hc attach t1}",
            ]
            .map(String::from)
        }));
        assert!(!r.as_object().unwrap().contains_key("model"));
        let w = hc_world(human());
        let r = hc_attempt(&w, Client::Claude, Some("sonnet"), false).unwrap();
        assert_eq!(r["model"], "sonnet");
        let names: Vec<_> = r.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            &names[16..],
            [
                "model",
                "hcAddress",
                "launcherArgv",
                "pane",
                "window",
                "startCommand"
            ]
        );
        let spec = read_json(
            &agents_dir(&w)
                .join(r["id"].as_str().unwrap())
                .join("spec.json"),
        );
        assert_eq!(
            spec["args"].as_array().unwrap()[2..4],
            [json!("--model"), json!("sonnet")]
        );
        for stage in ["before-attach", "after-tags"] {
            let w = hc_world(human());
            let _reset = HcCommitReset;
            let commits = Rc::new(Cell::new(0));
            let seen = commits.clone();
            commit_probe(Some(Box::new(move |_| seen.set(seen.get() + 1))));
            if stage == "before-attach" {
                w.fakes.hc_probe(|a| {
                    if a[0] == "run" {
                        let i = a.iter().position(|v| v == "launcher").unwrap();
                        std::fs::create_dir(
                            Path::new(&a[i + 1]).parent().unwrap().join("record.json"),
                        )
                        .unwrap();
                    }
                });
            } else {
                let agents = agents_dir(&w);
                w.fakes.mux_script("@agent-context", move |_| {
                    let path = std::fs::read_dir(&agents)
                        .unwrap()
                        .next()
                        .unwrap()
                        .unwrap()
                        .path()
                        .join("record.json");
                    std::fs::remove_file(&path).unwrap();
                    std::fs::create_dir(path).unwrap();
                    Ok(Output::default())
                });
            }
            assert!(hc_attempt(&w, Client::Claude, None, true).is_err());
            assert_eq!(commits.get(), 0);
            let folder = agents_dir(&w).join(&agent_folders(&w)[0]);
            assert!(!folder.join("commit.json").exists());
            let a = read_json(&folder.join("abort.json"));
            assert_eq!(a["kill"], "acknowledged");
            assert_eq!(
                a["viewCleanup"],
                if stage == "before-attach" {
                    json!("unproven")
                } else {
                    json!({"gone":true})
                }
            );
            assert_eq!(
                calls(&w, "if-shell")
                    .iter()
                    .filter(|a| a[6] == "kill-pane -t %9")
                    .count(),
                usize::from(stage == "after-tags")
            );
        }
    }

    #[test]
    fn a_launcher_that_gave_up_first_makes_the_spawn_refuse_and_roll_back() {
        for gave_up in [true, false] {
            let _reset = HcCommitReset;
            if gave_up {
                commit_probe(Some(Box::new(|folder| {
                    crate::agents::fixture::write(
                        &folder.join("commit.json"),
                        json!({"aborted":true}),
                    );
                })));
            }
            let w = hc_world(human());
            let result = hc_attempt(&w, Client::Claude, None, gave_up);
            let folder = agents_dir(&w).join(&agent_folders(&w)[0]);
            if gave_up {
                let e = result.unwrap_err();
                assert_eq!(
                    (e.code, e.check, e.message.as_str()),
                    (1, "launch", "the launcher gave up before the commit")
                );
                assert_eq!(
                    read_json(&folder.join("commit.json")),
                    json!({"aborted":true})
                );
                let abort = read_json(&folder.join("abort.json"));
                assert_eq!(
                    (&abort["kill"], &abort["viewCleanup"]),
                    (&json!("acknowledged"), &json!({"gone":true}))
                );
            } else {
                assert_eq!(result.unwrap()["hcAddress"], "t1");
                assert_eq!(
                    read_json(&folder.join("commit.json")),
                    json!({"committedAt":"1970-01-01T00:00:00.0000000Z"})
                );
                assert!(!folder.join("abort.json").exists());
            }
            assert_eq!(crate::agents::committed(&folder), !gave_up);
        }
    }

    #[test]
    fn reservations_count_once_and_their_names_stay_taken() {
        use crate::agents::core_fixture as cf;
        let w = human();
        let wires = reservations(&w, 5, true);
        let mut addresses: Vec<_> = wires
            .iter()
            .map(|v| v["address"].as_str().unwrap())
            .collect();
        addresses.push(addresses[0]);
        public_snapshot(&w, &addresses, &wires);
        assert_eq!(
            run(
                &w,
                &["-Client", "claude", "-Task", "hi", "-Name", "claude-3"]
            ),
            "2 name agent name is taken, try claude-3-2"
        );
        public_snapshot(&w, &addresses, &wires);
        public_launch(&w, "t1");
        assert_eq!(
            reply(&run(&w, &["-Client", "claude", "-Task", "hi"]))["name"],
            "hi"
        );
        assert_eq!(
            w.fakes
                .hc_calls()
                .iter()
                .filter(|(_, a, _)| a[0] == "get")
                .count(),
            10
        );
        let w = human();
        let mut wires = reservations(&w, 5, true);
        let addresses: Vec<_> = wires
            .iter()
            .map(|v| v["address"].as_str().unwrap().to_string())
            .collect();
        let r = cf::member(&w, "000000000d01", json!({"name": "mine", "color": "blue"}));
        wires.push(cf::wire(&r));
        w.panes(&[]);
        public_snapshot(
            &w,
            &addresses.iter().map(String::as_str).collect::<Vec<_>>(),
            &wires,
        );
        assert_eq!(
            run(&w, &["-Client", "claude", "-Task", "hi"]),
            "2 cap the session already has 6 live agents"
        );
        let w = human();
        let mut wires = reservations(&w, 1, true);
        let r = cf::member(&w, "000000000d01", json!({"name": "mine"}));
        wires.push(cf::wire(&r));
        for name in ["mine", "claude-1"] {
            public_snapshot(&w, &["t000000000a01"], &wires);
            assert_eq!(
                run(&w, &["-Client", "claude", "-Task", "hi", "-Name", name]),
                format!("2 name agent name is taken, try {name}-2")
            );
        }
    }

    #[test]
    fn rollback_keeps_a_replacement_and_closes_its_own_launch_live_or_dead() {
        for now in ["hc attach t1", "sh"] {
            let w = hc_world(human());
            let pane = Rc::new(RefCell::new(Some(now.to_string())));
            let state = pane.clone();
            w.fakes.mux_script("if-shell", move |argv| {
                let mut current = state.borrow_mut();
                let holds = current.as_ref().is_some_and(|command| {
                    argv[5]
                        == registry::Launch {
                            command: command.clone(),
                            pid: None,
                        }
                        .launched()
                });
                if argv[6] == "kill-pane -t %9" && (argv[5] == "1" || holds) {
                    *current = None;
                }
                Ok(Output::default())
            });
            w.fakes.mux_first("@agent-context", 1, "", "bad");
            let probe = pane.clone();
            w.fakes
                .mux_script("-t %9 #{pane_id}|#{@agent-id}", move |_| {
                    Ok(Output {
                        stdout: if probe.borrow().is_none() {
                            "|\n"
                        } else {
                            "%9|\n"
                        }
                        .into(),
                        ..Output::default()
                    })
                });
            let e = hc_attempt(&w, Client::Claude, None, true).unwrap_err();
            assert_eq!(
                (e.code, e.check, e.message.as_str()),
                (1, "rmux", "tmux call failed")
            );
            assert_eq!(pane.borrow().is_none(), now == "hc attach t1");
            let a = read_json(
                &agents_dir(&w)
                    .join(&agent_folders(&w)[0])
                    .join("abort.json"),
            );
            assert_eq!(a["kill"], "acknowledged");
            assert_eq!(a["viewCleanup"], json!({"gone":now=="hc attach t1"}));
        }
    }

    #[test]
    fn a_pane_replaced_before_it_is_tagged_is_never_written() {
        let w = hc_world(human());
        let acted = Rc::new(RefCell::new(Vec::new()));
        let log = acted.clone();
        // The exact predicate assertion and set-option/select-pane checks enforce the guard;
        // acted cannot fill after the assertion, so its emptiness alone proves nothing.
        w.fakes.mux_script("if-shell", move |a| {
            assert_eq!(a[5], "#{==:#{pane_start_command},hc attach t1}");
            if a[5] == "#{==:#{pane_start_command},sh}" {
                log.borrow_mut().push(a[6].clone());
            }
            Ok(Output::default())
        });
        w.fakes
            .mux_first("}|#{==:#{pane_start_command},", 0, "%9|0\n", "");
        w.fakes
            .mux_first("-t %9 #{pane_id}|#{@agent-id}", 0, "%9|\n", "");
        let e = hc_attempt(&w, Client::Claude, None, true).unwrap_err();
        assert_eq!(
            (e.code, e.check, e.message.as_str()),
            (1, "internal", "unexpected error (unproven new view)")
        );
        assert!(acted.borrow().is_empty());
        assert!(calls(&w, "set-option").is_empty());
        assert!(calls(&w, "select-pane").is_empty());
        let folder = agents_dir(&w).join(&agent_folders(&w)[0]);
        let r = read_json(&folder.join("record.json"));
        assert_eq!(r["hcAddress"], "t1");
        assert!(r.get("pane").is_none());
        assert!(!folder.join("commit.json").exists());
        let a = read_json(&folder.join("abort.json"));
        assert_eq!(
            (&a["kill"], &a["viewCleanup"]),
            (&json!("acknowledged"), &json!({"gone":false}))
        );
    }

    #[test]
    fn rollback_whose_gone_proof_fails_to_run_claims_nothing() {
        let w = hc_world(human());
        w.fakes.mux_first("@agent-context", 1, "", "bad");
        w.fakes.mux_script("-t %9 #{pane_id}|#{@agent-id}", |_| {
            refuse(1, "endpoint", "rmux endpoint did not answer")
        });
        let e = hc_attempt(&w, Client::Claude, None, true).unwrap_err();
        assert_eq!(
            (e.code, e.check, e.message.as_str()),
            (1, "rmux", "tmux call failed")
        );
        assert_eq!(
            calls(&w, "if-shell")
                .iter()
                .filter(|c| c[6] == "kill-pane -t %9")
                .count(),
            1
        );
        let a = read_json(
            &agents_dir(&w)
                .join(&agent_folders(&w)[0])
                .join("abort.json"),
        );
        assert_eq!(a["kill"], "acknowledged");
        assert_eq!(
            a["viewCleanup"],
            json!({"failed":true,"message":"rmux endpoint did not answer"})
        );
    }
    #[test]
    fn public_hc_spawn_contract() {
        use crate::agents::core_fixture as cf;
        for mode in ["ended", "running", "lost", "unknown", "no-core"] {
            let w = hc_world(human());
            let c = w.caller();
            let r = cf::member(&w, "aaaaaaaaaaaa", json!({"name":"hi","color":"blue"}));
            w.panes(&[]);
            let n = w.folder("aaaaaaaaaaaa").join("notice.json");
            let mut t = cf::wire(&r);
            let stale = br#"{"kind":"stopped","at":"2026-10-29T11:00:00.0000000Z"}"#;
            if matches!(mode, "running" | "lost" | "unknown") {
                std::fs::write(&n, stale).unwrap();
            }
            if mode == "ended" {
                t["state"] = json!("finished");
                t["reason"] = json!("exited");
                t["exit_code"] = json!(7);
            }
            if mode == "lost" {
                t["argv"] = json!(["/bin/sh", "-l"]);
            }
            if mode == "unknown" {
                t["labels"]["mux.gen"] = json!("1");
            }
            if mode != "no-core" {
                cf::online(&w);
                cf::listing(&w, &super::super::membership::team(&c), &[]);
                cf::get(&w, "taaaaaaaaaaaa", t);
            }
            if mode == "ended" {
                w.fakes.hc_answer(Ok(crate::spawn::Out {
                    stdout: r#"{"address":"t1"}"#.into(),
                    ..Default::default()
                }));
            }
            let f = w.fakes.clone();
            w.fakes.hc_probe(move |a| {
                let locks = f.locks();
                if a[0] == "--version" {
                    assert!(locks.is_empty());
                } else {
                    assert!(locks.first().unwrap().starts_with("+spawn|"));
                }
            });
            w.fakes.clear_log();
            let got = spawn(
                &w.ctx,
                &w.request(&["spawn", "-Client", "claude", "-Task", "hi", "-Name", "hi"]),
            );
            if mode == "ended" {
                let out = got.unwrap();
                assert_public_runs(&w);
                assert_eq!(out["model"], "inherited");
                assert_eq!(
                    out.keys().map(String::as_str).collect::<Vec<_>>(),
                    [
                        "name",
                        "id",
                        "pane",
                        "window",
                        "client",
                        "sessionId",
                        "context",
                        "reason",
                        "model",
                        "placement",
                        "placementSource",
                        "sidebar",
                        "life",
                        "view",
                        "hcAddress"
                    ]
                );
                assert_eq!(
                    (&out["life"], &out["view"], &out["hcAddress"]),
                    (&json!("running"), &json!("attached"), &json!("t1"))
                );
                assert_eq!(read_json(&n)["exitCode"], 7);
                let record = read_json(
                    &agents_dir(&w)
                        .join(out["id"].as_str().unwrap())
                        .join("record.json"),
                );
                assert_eq!(record["color"], "blue");
                let spawn = lock_key("spawn", &c, "$3");
                let member = lock_key("member", &c, "aaaaaaaaaaaa");
                let notice = lock_key("notice", &c, "aaaaaaaaaaaa");
                let mail = lock_key("mail", &c, "pane-1");
                let bar = lock_key("sidebar", &c, "$3");
                assert_eq!(
                    w.fakes.locks(),
                    [
                        format!("+{spawn}"),
                        format!("+{member}"),
                        format!("+{notice}"),
                        format!("+{mail}"),
                        format!("-{mail}"),
                        format!("-{notice}"),
                        format!("-{member}"),
                        format!("-{spawn}"),
                        format!("+{bar}"),
                        format!("-{bar}")
                    ]
                );
                assert_eq!(calls(&w, "list-panes").len(), 2);
                assert_eq!(
                    w.fakes
                        .hc_calls()
                        .iter()
                        .map(|(_, a, _)| a[0].clone())
                        .collect::<Vec<_>>(),
                    ["--version", "ls", "get", "run"]
                );
            } else {
                let e = got.unwrap_err();
                if mode == "no-core" {
                    assert!(!n.exists());
                } else {
                    assert_eq!(std::fs::read(&n).unwrap(), stale);
                }
                assert_eq!(
                    (e.code, e.check),
                    if mode == "no-core" {
                        (1, "hc")
                    } else {
                        (2, "name")
                    }
                );
                assert_eq!(
                    e.message,
                    if mode == "no-core" {
                        "install or upgrade herdr-core to >= 0.1.21 and put hc (or herdr-core) on PATH"
                    } else {
                        "agent name is taken, try hi-2"
                    }
                );
                assert!(calls(&w, "new-window").is_empty());
            }
            if mode == "no-core" {
                assert!(w.fakes.locks().is_empty());
            }
            w.fakes.hc_done().unwrap();
        }
    }
    #[test]
    fn public_hc_spawn_counts_detached_and_foreign_reservations() {
        use crate::agents::core_fixture as cf;
        for foreign in [false, true] {
            let w = hc_world(human());
            let c = w.caller();
            w.panes(&[]);
            let mut records = Vec::new();
            for n in 0..6 {
                let id = format!("{n:012x}");
                let r = cf::member(&w, &id, json!({"name":format!("w{n}")}));
                if foreign {
                    std::fs::remove_dir_all(w.folder(&id)).unwrap();
                } else {
                    std::fs::remove_file(w.folder(&id).join("commit.json")).unwrap();
                }
                records.push(r);
            }
            cf::online(&w);
            let addresses: Vec<_> = records
                .iter()
                .map(|r| r["hcAddress"].as_str().unwrap())
                .collect();
            cf::listing(
                &w,
                &super::super::membership::team(&c),
                if foreign { &addresses } else { &[] },
            );
            for r in &records {
                cf::get(&w, r["hcAddress"].as_str().unwrap(), cf::wire(r));
            }
            let e = spawn(
                &w.ctx,
                &w.request(&["spawn", "-Client", "claude", "-Task", "hi"]),
            )
            .unwrap_err();
            assert_eq!(
                (e.code, e.check, e.message.as_str()),
                (2, "cap", "the session already has 6 live agents")
            );
            assert!(!w.state().join("server.json").exists());
            assert!(calls(&w, "new-window").is_empty());
            assert_eq!(w.fakes.hc_calls().len(), 8);
            w.fakes.hc_done().unwrap();
        }
    }
}

#[cfg(test)]
#[path = "spawn_hc_tests.rs"]
mod hc_tests;
pub struct CoreRequest<'a> {
    pub client: Client,
    pub task: &'a str,
    pub brief: Option<&'a str>,
    pub directory: &'a Path,
    pub model: Option<&'a str>,
    pub no_fork: bool,
    pub split: Option<&'a str>,
}
pub struct CoreSlot {
    pub name: String,
    pub color: &'static str,
}
pub fn launch_hc(
    ctx: &Ctx,
    caller: &Caller,
    request: CoreRequest<'_>,
    reserve: impl FnOnce(&crate::hc::Core<'_>) -> Res<CoreSlot>,
) -> Res<Value> {
    let core = crate::hc::connect(ctx)?;
    lock(ctx, &lock_key("spawn", caller, &caller.session), || {
        let slot = reserve(&core)?;
        let launch = Launch {
            ctx,
            caller,
            client: request.client,
            model: request.model,
            no_fork: request.no_fork,
            task: request.task.into(),
            brief: request.brief.map(str::to_owned),
            directory: request.directory.into(),
        };
        super::core_launch::execute_connected(
            &core,
            || launch.prepare_hc(slot),
            |core, p, view| {
                super::attachment::attach(ctx, core, caller, &p.record, request.split, view)
            },
            |folder| commit(ctx, folder),
            |view| super::attachment::clean(ctx, view),
        )
    })
}
impl Launch<'_> {
    fn prepare_hc(&self, slot: CoreSlot) -> Res<super::core_launch::Prepared> {
        let (ctx, caller) = (self.ctx, self.caller);
        let state = state_dir(ctx, caller)?;
        write_json(
            &state.join("server.json"),
            &json!({"socket":caller.socket,"serverPid":caller.server_pid,"serverStart":caller.server_start}),
        )?;
        let agent = self.allocate(&state, slot.name, slot.color)?;
        let prepare = || {
            let spec = self.prepare(&agent)?;
            let mut record = self.record(&agent, "", "", "")?;
            let mut body = crate::state::fs::read_json(&spec)?
                .ok_or_else(|| Refusal::internal("missing launch spec"))?;
            body["generation"] = "0".into();
            write_json(&spec, &body)?;
            record["generation"] = "0".into();
            record["team"] = super::membership::team(caller).into();
            for k in ["pane", "window", "startCommand"] {
                record.as_object_mut().unwrap().remove(k);
            }
            Ok(super::core_launch::Prepared {
                folder: agent.folder.clone(),
                record,
                argv: crate::launcher_scope::argv(ctx, &spec)?,
            })
        };
        prepare().map_err(|mut e: Refusal| {
            if write_json(
                &agent.folder.join("abort.json"),
                &json!({"runUncertain":false,"hcAddress":null,"message":e.message}),
            )
            .is_err()
            {
                e.message.push_str("; abort evidence could not be written");
            }
            e
        })
    }
}
