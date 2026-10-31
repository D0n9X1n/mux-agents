//! spawn: checks the request and caller, then under the session's spawn lock: cap, name, context, files, pane,
//! tags, record.json and last commit.json; only a committed agent is live (MuxAgents.ps1:641-833).

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::mail::doing_line;
use super::{copilot, lock, lock_key, registry, state_dir, usage};
use crate::caller::{Caller, Kind, resolve};
use crate::checks::{Context, Plan, assert_spawn_allowed, context_plan, resolve_name, session_id};
use crate::cli::Request;
use crate::ctx::Ctx;
use crate::error::{Refusal, Res, refuse};
use crate::mux::{Kind as MuxKind, native};
use crate::parse::{Client, cmdline_length};
use crate::state::fs::{BRIEF_MAX, TASK_MAX, read_text, write_json};
use crate::state::root::OsKind;
use crate::util::{guid, hex_id, utc_o};

const CLAUDE_FLAGS: [&str; 3] = ["--dangerously-skip-permissions", "--effort", "high"];
const COPILOT_FLAGS: [&str; 2] = ["--no-sandbox", "--allow-all"];
// The client command line holds 32767 UTF-16 units after quoting; 1024 of them are kept for the executable path.
pub(crate) const CMDLINE_MAX: usize = 32767;
const EXE_RESERVE: usize = 1024;
const CAP: usize = 6;
const PANE_FORMAT: &str = "#{pane_id}|#{window_id}";
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
    let one_task = request.value("Task").is_some() != request.value("TaskFile").is_some();
    let Some(client) = request.value("Client").and_then(client_named) else {
        return Err(usage("spawn"));
    };
    if !one_task || !split.is_none_or(|s| s == "h" || s == "v") {
        return Err(usage("spawn"));
    }
    let caller = resolve(ctx)?;
    if !caller.agent_tag.is_empty() {
        return refuse(2, "tagged", "an agent pane cannot spawn agents");
    }
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
    let launch = Launch {
        ctx,
        caller: &caller,
        client,
        requested: request.value("Name"),
        no_fork: request.switch("NoFork"),
        task,
        brief,
        directory,
        split,
    };
    lock(ctx, &lock_key("spawn", &caller, &caller.session), || {
        launch.run()
    })
}

/// A checked request, run under the spawn lock.
struct Launch<'a> {
    ctx: &'a Ctx,
    caller: &'a Caller,
    client: Client,
    requested: Option<&'a str>,
    no_fork: bool,
    task: String,
    brief: Option<String>,
    directory: PathBuf,
    split: Option<&'a str>,
}

/// The agent being started.
struct Agent {
    id: String,
    name: String,
    session: String,
    plan: Plan,
    folder: PathBuf,
    // A Copilot fork's session copy, deleted on any failure.
    copied: Option<PathBuf>,
}

impl Agent {
    fn remove_copy(&self) {
        if let Some(copy) = &self.copied {
            let _ = std::fs::remove_dir_all(copy);
        }
    }
}

impl Launch<'_> {
    fn run(&self) -> Res<Map<String, Value>> {
        let (ctx, caller) = (self.ctx, self.caller);
        let state = state_dir(ctx, caller)?;
        write_json(
            &state.join("server.json"),
            &json!({"socket": caller.socket, "serverPid": caller.server_pid, "serverStart": caller.server_start}),
        )?;
        // Every tagged pane counts, committed or not, so a launch in progress holds its place.
        let live = registry::tagged(ctx, caller)?;
        if live.len() >= CAP {
            return refuse(2, "cap", "the session already has 6 live agents");
        }
        let taken: Vec<&str> = live.iter().map(|p| p.name.as_str()).collect();
        let name = resolve_name(self.requested, self.client, &taken)?;
        let mut plan = context_plan(caller, self.client, &ctx.env, self.no_fork);
        let session = guid()?;
        let id = hex_id()?;
        let mut copied = None;
        if plan.context == Context::Fork && self.client == Client::Copilot {
            let parent = plan.parent_session.clone().unwrap_or_default();
            if copilot::copy_session(&ctx.home, &parent, &session) {
                copied = Some(copilot::session_folder(&ctx.home, &session));
            } else {
                plan = Plan {
                    context: Context::Brief,
                    reason: "copy-verification-failed",
                    parent_session: None,
                };
            }
        }
        // A brief plan never has a copy, so nothing is left behind here.
        if plan.context == Context::Brief && caller.kind == Kind::Agent && self.brief.is_none() {
            return refuse(
                2,
                "brief",
                "this agent starts brief-only, so an agent caller passes -BriefFile",
            );
        }
        let agent = Agent {
            folder: state.join("agents").join(&id),
            id,
            name,
            session,
            plan,
            copied,
        };
        let spec = match self.prepare(&agent) {
            Ok(spec) => spec,
            Err(refusal) => {
                let _ = std::fs::remove_dir_all(&agent.folder);
                agent.remove_copy();
                return Err(refusal);
            }
        };
        let mut pane = None;
        self.start(&agent, &spec, &mut pane).or_else(|_| {
            // Each cleanup step runs on its own: a failing one skips none of the others and never hides the refusal.
            let at = utc_o(ctx.clock.now());
            let _ = write_json(
                &agent.folder.join("abort.json"),
                &json!({"abortedAt": at, "pane": pane}),
            );
            if let Some(pane) = &pane {
                let _ = native(ctx, &["kill-pane", "-t", pane], true);
            }
            agent.remove_copy();
            refuse(1, "launch", "the agent did not start; its pane was closed")
        })
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
        let args = client_args(self.client, &agent.plan, &agent.session, &prompt);
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

    /// From the launch call on a pane may exist, so any failure here is a launch failure; `pane` names it once known.
    fn start(
        &self,
        agent: &Agent,
        spec: &Path,
        pane: &mut Option<String>,
    ) -> Res<Map<String, Value>> {
        let (ctx, caller) = (self.ctx, self.caller);
        let session_target = format!("{}:", caller.session);
        let split_flag = self.split.map(|split| format!("-{split}"));
        let mut argv: Vec<&str> = match &split_flag {
            Some(flag) => vec![
                "split-window",
                "-d",
                flag,
                "-P",
                "-F",
                PANE_FORMAT,
                "-t",
                &caller.pane,
            ],
            None => vec![
                "new-window",
                "-d",
                "-P",
                "-F",
                PANE_FORMAT,
                "-t",
                &session_target,
                "-n",
                &agent.name,
            ],
        };
        let env = pane_env(utf8(&ctx.state_root)?, crate::state::root::HOST);
        argv.extend(env.iter().map(String::as_str));
        argv.extend([
            "-c",
            utf8(&self.directory)?,
            utf8(&ctx.exe)?,
            "launcher",
            utf8(spec)?,
        ]);
        let created = native(ctx, &argv, false)?;
        let (new_pane, window) = pane_and_window(&created.stdout)
            .ok_or_else(|| Refusal::internal("rmux did not name the new pane"))?;
        *pane = Some(new_pane.clone());
        let context = format!("{}:{}", context_name(agent.plan.context), agent.plan.reason);
        let tags = [
            ("@agent-id", agent.id.as_str()),
            ("@agent-name", &agent.name),
            ("@agent-parent", &caller.pane),
            ("@agent-context", &context),
        ];
        let mut tag_argv: Vec<&str> = Vec::new();
        for (key, value) in tags {
            if !tag_argv.is_empty() {
                tag_argv.push(";");
            }
            tag_argv.extend(["set-option", "-p", "-t", &new_pane, key, value]);
        }
        native(ctx, &tag_argv, false)?;
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
        let record = json!({"schema": 1, "id": agent.id, "name": agent.name, "client": client,
            "sessionId": agent.session, "context": context, "reason": agent.plan.reason,
            "session": caller.session, "pane": new_pane, "window": window, "parentPane": caller.pane,
            "parentAgent": parent_agent, "directory": utf8(&self.directory)?,
            "taskLine": doing_line(&self.task), "createdAt": utc_o(ctx.clock.now())});
        write_json(&agent.folder.join("record.json"), &record)?;
        // Last: only a committed agent is live.
        write_json(
            &agent.folder.join("commit.json"),
            &json!({"committedAt": utc_o(ctx.clock.now())}),
        )?;
        let mut reply = Map::new();
        reply.insert("name".into(), agent.name.clone().into());
        reply.insert("id".into(), agent.id.clone().into());
        reply.insert("pane".into(), new_pane.into());
        reply.insert("window".into(), window.into());
        reply.insert("client".into(), client.into());
        reply.insert("sessionId".into(), agent.session.clone().into());
        reply.insert("context".into(), context.into());
        reply.insert("reason".into(), agent.plan.reason.into());
        Ok(reply)
    }
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

/// `^(%\d+)\|(@\d+)$` after trailing CR and LF are cut.
pub(crate) fn pane_and_window(stdout: &str) -> Option<(String, String)> {
    let (pane, window) = stdout.trim_end_matches(['\r', '\n']).split_once('|')?;
    let numbered = |text: &str, mark: char| {
        text.strip_prefix(mark)
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    (numbered(pane, '%') && numbered(window, '@')).then(|| (pane.into(), window.into()))
}

/// The client arguments after its flags. A Claude fork resumes and forks the parent into `session`; a Copilot fork
/// resumes `session`, the copy made for it. A brief starts `session` fresh.
fn client_args(client: Client, plan: &Plan, session: &str, prompt: &str) -> Vec<String> {
    let mut args: Vec<String> = match (plan.context, client) {
        (Context::Fork, Client::Claude) => vec![
            "--resume".into(),
            plan.parent_session.clone().unwrap_or_default(),
            "--fork-session".into(),
            "--session-id".into(),
            session.into(),
        ],
        (Context::Fork, Client::Copilot) => vec!["--resume".into(), session.into()],
        (Context::Brief, _) => vec!["--session-id".into(), session.into()],
    };
    if client == Client::Copilot {
        args.push("-i".into());
    }
    args.push(prompt.into());
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
            .chunks_exact(2)
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
    use crate::agents::fixture::{CLAUDE, World};
    use crate::checks::Context;
    use crate::ctx::Env;
    use crate::mux::{Kind as MuxKind, Output};
    use crate::parse::cmdline_length;
    use serde_json::json;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    const OLD: &str = "11111111-2222-4333-8444-555555555555";
    const ENTRY: &str =
        "1 entry mux-agent entry path may hold only letters, digits, spaces and _ \\ / : . ( ) -";

    fn ready(w: World) -> World {
        w.fakes.mux_rule("new-window", 0, "%9|@5\n", "");
        w.fakes.mux_rule("split-window", 0, "%9|@5\n", "");
        w
    }

    fn human() -> World {
        ready(World::human(""))
    }

    // An agent caller whose C3, C2 and C5 checks the tests skip, as an admitted pane's are.
    fn agent() -> World {
        let mut w = ready(World::agent(""));
        w.ctx.admitted = true;
        w
    }

    fn run(w: &World, args: &[&str]) -> String {
        let mut full = vec!["spawn"];
        full.extend(args);
        match spawn(&w.ctx, &w.request(&full)) {
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
        ] {
            let w = human();
            assert_eq!(run(&w, args), usage, "{args:?}");
            assert!(w.fakes.mux_calls().is_empty(), "{args:?}");
            assert!(!w.ctx.state_root.exists(), "{args:?}");
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
            assert_eq!(verbs(&w), ["display-message"], "{args:?}");
            assert!(!w.ctx.state_root.exists(), "{args:?}");
        }
        // At the limits, and with an empty brief, it spawns.
        w.file("max.md", &[b'x'; 16384]);
        w.file("brief.md", &vec![b'x'; 1 << 20]);
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
        let empty = run(
            &w,
            &["-Client", "claude", "-Task", "hi", "-BriefFile", "empty.md"],
        );
        assert!(empty.starts_with("ok "), "{empty}");
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
        assert_eq!(verbs(&w), ["display-message"]);
        assert!(!w.ctx.state_root.exists());
    }

    fn rows(count: u32) -> Vec<String> {
        (1..=count)
            .map(|n| {
                let p = 20 + n;
                format!("%{p}|@{p}|{}|{:012x}|claude-{n}", 300 + n, 0xa00 + n)
            })
            .collect()
    }

    #[test]
    fn cap_6_counts_uncommitted() {
        // Six tagged panes, none committed: a launch in progress holds its place.
        let w = human();
        let six = rows(6);
        w.panes(&six.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(
            run(&w, &["-Client", "claude", "-Task", "hi"]),
            "2 cap the session already has 6 live agents"
        );
        assert!(calls(&w, "new-window").is_empty());
        assert!(agent_folders(&w).is_empty());
        // server.json is written under the lock before the panes are listed.
        let caller = w.caller();
        let server = std::fs::read_to_string(
            w.ctx
                .state_root
                .join(&caller.server_key)
                .join("server.json"),
        )
        .unwrap();
        let expected = json!({"socket": caller.socket, "serverPid": caller.server_pid, "serverStart": caller.server_start});
        assert_eq!(server, expected.to_string());
        // Five leave room for a sixth, named after the taken ones.
        let w = human();
        let five = rows(5);
        w.panes(&five.iter().map(String::as_str).collect::<Vec<_>>());
        let ok = reply(&run(&w, &["-Client", "claude", "-Task", "hi"]));
        assert_eq!(ok["name"], "claude-6");
    }

    #[test]
    fn brief_required_for_agent_brief_plan() {
        // A Claude agent without a session id starts brief-only, so it must pass a brief.
        let w = agent();
        w.file("t.md", b"task");
        w.file("b.md", b"brief text\n");
        for client in ["claude", "copilot"] {
            assert_eq!(
                run(&w, &["-Client", client, "-TaskFile", "t.md"]),
                "2 brief this agent starts brief-only, so an agent caller passes -BriefFile",
                "{client}"
            );
            assert!(calls(&w, "new-window").is_empty());
            assert!(agent_folders(&w).is_empty());
        }
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
        assert_eq!(run(&w, &["-Client", "claude", "-Task", "hi"]), ENTRY);
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
            let w = human();
            let ok = reply(&run(&w, &["-Client", client, "-Task", "x"]));
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
            let base = 1024 + cmdline_length(&argv);
            // Each '"' costs 2 on the command line; the 'x' it replaces cost 1. Exactly 32767 fits.
            let room = 32768 - base;
            let fit = format!("{}{}", "\"".repeat(room / 2), "x".repeat(room % 2));
            for (task, fits) in [(fit.clone(), true), (format!("{fit}x"), false)] {
                let w = human();
                let text = run(&w, &["-Client", client, "-Task", &task]);
                if fits {
                    assert!(text.starts_with("ok "), "{client} {}", task.len());
                } else {
                    assert_eq!(
                        text, "2 usage task is too long for the client command line",
                        "{client}"
                    );
                    assert!(calls(&w, "new-window").is_empty());
                    assert!(agent_folders(&w).is_empty());
                }
            }
        }
    }

    #[test]
    fn pane_cmd_is_exe_launcher_spec() {
        let host_env =
            |w: &World| pane_env(w.ctx.state_root.to_str().unwrap(), crate::state::root::HOST);
        let w = human();
        let ok = reply(&run(&w, &["-Client", "claude", "-Task", "hi"]));
        let id = ok["id"].as_str().unwrap();
        let spec = agents_dir(&w).join(id).join("spec.json");
        let mut expected: Vec<String> = [
            "-N",
            "new-window",
            "-d",
            "-P",
            "-F",
            "#{pane_id}|#{window_id}",
            "-t",
            "$3:",
            "-n",
            "claude-1",
        ]
        .map(String::from)
        .to_vec();
        expected.extend(host_env(&w));
        expected.extend([
            "-c".into(),
            w.ctx.cwd.to_str().unwrap().into(),
            "/bin/mux-agent".into(),
            "launcher".into(),
            spec.to_str().unwrap().into(),
        ]);
        assert_eq!(calls(&w, "new-window"), [expected]);
        assert!(calls(&w, "split-window").is_empty());
        assert_eq!(
            read_json(&spec),
            json!({"schema": 1, "stateDirectory": agents_dir(&w).parent().unwrap().to_str().unwrap(),
                "id": id, "client": "claude", "parentPane": "%1",
                "args": ["--session-id", ok["sessionId"], read_json(&spec)["prompt"]],
                "prompt": read_json(&spec)["prompt"]})
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
                "prompt"
            ]
        );
        let prompt = read_json(&spec)["prompt"].as_str().unwrap().to_string();
        assert!(prompt.ends_with("\n\nhi"), "{prompt}");
        assert_eq!(prompt.split('\n').count(), 7);
        for split in ["h", "v"] {
            let w = human();
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
                "-d",
                &format!("-{split}"),
                "-P",
                "-F",
                "#{pane_id}|#{window_id}",
                "-t",
                "%1",
            ]
            .map(String::from)
            .to_vec();
            expected.extend(host_env(&w));
            expected.extend([
                "-c".into(),
                w.ctx.cwd.to_str().unwrap().into(),
                "/bin/mux-agent".into(),
                "launcher".into(),
                spec.to_str().unwrap().into(),
            ]);
            assert_eq!(calls(&w, "split-window"), [expected], "{split}");
            assert!(calls(&w, "new-window").is_empty());
            assert_eq!(
                read_json(&spec)["args"].as_array().unwrap()[..2],
                [json!("--session-id"), ok["sessionId"].clone()]
            );
        }
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
        for kill in [0, 1] {
            let w = human();
            w.fakes.mux_first("set-option", 1, "", "no pane");
            w.fakes.mux_first("kill-pane", kill, "", "");
            assert_eq!(
                run(&w, &["-Client", "claude", "-Task", "hi"]),
                "1 launch the agent did not start; its pane was closed",
                "{kill}"
            );
            let folder = agents_dir(&w).join(&agent_folders(&w)[0]);
            assert_eq!(
                std::fs::read_to_string(folder.join("abort.json")).unwrap(),
                r#"{"abortedAt":"1970-01-01T00:00:00.0000000Z","pane":"%9"}"#
            );
            assert!(!folder.join("record.json").exists() && !folder.join("commit.json").exists());
            assert_eq!(
                calls(&w, "kill-pane"),
                [["-N", "kill-pane", "-t", "%9"].map(String::from).to_vec()]
            );
        }
        // A failed or garbled launch names no pane: abort.json says null and nothing is killed or tagged.
        for (code, out) in [
            (1, "%9|@5\n"),
            (0, ""),
            (0, "%9|@5\n%10|@6\n"),
            (0, "%9|@5;kill-server\n"),
            (0, "#{pane_id}|@5\n"),
            (0, "%9 |@5\n"),
        ] {
            let w = human();
            w.fakes.mux_first("new-window", code, out, "");
            assert_eq!(
                run(&w, &["-Client", "claude", "-Task", "hi"]),
                "1 launch the agent did not start; its pane was closed",
                "{out:?}"
            );
            let folder = agents_dir(&w).join(&agent_folders(&w)[0]);
            assert_eq!(
                std::fs::read_to_string(folder.join("abort.json")).unwrap(),
                r#"{"abortedAt":"1970-01-01T00:00:00.0000000Z","pane":null}"#
            );
            assert!(calls(&w, "kill-pane").is_empty() && calls(&w, "set-option").is_empty());
        }
        // A CRLF line end is not garbled.
        let w = human();
        w.fakes.mux_first("new-window", 0, "%9|@5\r\n", "");
        let ok = reply(&run(&w, &["-Client", "claude", "-Task", "hi"]));
        assert_eq!((&ok["pane"], &ok["window"]), (&json!("%9"), &json!("@5")));
        // A Copilot fork's copy is deleted when the launch fails, and kept when it commits.
        for fail in [true, false] {
            let mut w = agent();
            w.fakes.client(CLAUDE, Client::Copilot);
            let mut pairs: Vec<(String, String)> = w
                .ctx
                .env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            pairs.push(("COPILOT_AGENT_SESSION_ID".into(), OLD.into()));
            w.ctx.env = Env::from_pairs(pairs);
            w.ctx.home = w.ctx.cwd.parent().unwrap().join("home");
            let source = session_folder(&w.ctx.home, OLD);
            std::fs::create_dir_all(&source).unwrap();
            std::fs::write(source.join("workspace.yaml"), format!("id: {OLD}\n")).unwrap();
            std::fs::write(
                source.join("events.jsonl"),
                format!("{{\"sessionId\":\"{OLD}\"}}\n"),
            )
            .unwrap();
            if fail {
                w.fakes.mux_first("set-option", 1, "", "");
            }
            w.file("t.md", b"task");
            let text = run(&w, &["-Client", "copilot", "-TaskFile", "t.md"]);
            let mut sessions: Vec<String> = std::fs::read_dir(source.parent().unwrap())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            sessions.sort();
            if fail {
                assert_eq!(
                    text,
                    "1 launch the agent did not start; its pane was closed"
                );
                assert_eq!(sessions, [OLD]);
            } else {
                let ok = reply(&text);
                assert_eq!(
                    (&ok["context"], &ok["reason"]),
                    (&json!("fork"), &json!("same-client"))
                );
                let new = ok["sessionId"].as_str().unwrap();
                assert!(sessions.contains(&new.to_string()) && sessions.len() == 2);
                let spec = read_json(
                    &agents_dir(&w)
                        .join(ok["id"].as_str().unwrap())
                        .join("spec.json"),
                );
                assert_eq!(
                    spec["args"].as_array().unwrap()[..3],
                    [json!("--resume"), json!(new), json!("-i")]
                );
            }
        }
    }

    #[test]
    fn record_then_commit_order() {
        let w = human();
        let agents = agents_dir(&w);
        let log = Rc::new(RefCell::new(Vec::new()));
        let (seen, dir) = (log.clone(), agents.clone());
        w.fakes.mux_script("new-window", move |argv| {
            let spec = PathBuf::from(argv.last().unwrap());
            seen.borrow_mut()
                .push(format!("launch spec {}", spec.is_file()));
            let _ = &dir;
            Ok(Output {
                code: 0,
                stdout: "%9|@5\n".into(),
                stderr: String::new(),
            })
        });
        let (seen, dir) = (log.clone(), agents.clone());
        w.fakes.mux_script("set-option", move |argv| {
            let folder = dir.join(&argv[6]);
            seen.borrow_mut().push(format!(
                "tag record {} commit {}",
                folder.join("record.json").exists(),
                folder.join("commit.json").exists()
            ));
            Ok(Output::default())
        });
        w.fakes.clear_log();
        let ok = reply(&run(&w, &["-Client", "claude", "-Task", "hi"]));
        let id = ok["id"].as_str().unwrap();
        assert_eq!(
            *log.borrow(),
            ["launch spec true", "tag record false commit false"]
        );
        let caller = w.caller();
        assert_eq!(
            w.fakes.locks()[0],
            format!("+spawn|{}|$3", caller.server_key)
        );
        let tags: Vec<String> = [
            "-N",
            "set-option",
            "-p",
            "-t",
            "%9",
            "@agent-id",
            id,
            ";",
            "set-option",
            "-p",
            "-t",
            "%9",
            "@agent-name",
            "claude-1",
            ";",
            "set-option",
            "-p",
            "-t",
            "%9",
            "@agent-parent",
            "%1",
            ";",
            "set-option",
            "-p",
            "-t",
            "%9",
            "@agent-context",
            "brief:human",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(calls(&w, "set-option"), [tags]);
        let folder = agents.join(id);
        let record = json!({"schema": 1, "id": id, "name": "claude-1", "client": "claude",
            "sessionId": ok["sessionId"], "context": "brief", "reason": "human", "session": "$3",
            "pane": "%9", "window": "@5", "parentPane": "%1", "parentAgent": null,
            "directory": w.ctx.cwd.to_str().unwrap(), "taskLine": "hi",
            "createdAt": "1970-01-01T00:00:00.0000000Z"});
        assert_eq!(
            std::fs::read_to_string(folder.join("record.json")).unwrap(),
            record.to_string()
        );
        assert_eq!(
            std::fs::read_to_string(folder.join("commit.json")).unwrap(),
            r#"{"committedAt":"1970-01-01T00:00:00.0000000Z"}"#
        );
        let keys: Vec<String> = ok.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            [
                "name",
                "id",
                "pane",
                "window",
                "client",
                "sessionId",
                "context",
                "reason"
            ]
        );
        // A record that cannot be written is never committed: the launch aborts and the pane closes.
        let w = human();
        let dir = agents_dir(&w);
        w.fakes.mux_script("set-option", move |argv| {
            std::fs::create_dir(dir.join(&argv[6]).join("record.json")).unwrap();
            Ok(Output::default())
        });
        assert_eq!(
            run(&w, &["-Client", "claude", "-Task", "hi"]),
            "1 launch the agent did not start; its pane was closed"
        );
        let folder = agents_dir(&w).join(&agent_folders(&w)[0]);
        assert!(folder.join("abort.json").is_file() && !folder.join("commit.json").exists());
        assert_eq!(calls(&w, "kill-pane").len(), 1);
    }
}
