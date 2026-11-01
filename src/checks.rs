//! Spawn checks: the agent name, the context plan, and for an agent caller C3 (flags), C2 (policy) and C5
//! (directory) (MuxAgents.ps1:548-578, 912-1012; Unix.ps1:142-161).

use std::path::{Path, PathBuf};

use crate::caller::{Agent, Caller, Kind};
use crate::ctx::{Ctx, Env};
use crate::error::{Refusal, Res};
use crate::parse::Client;
use crate::state::root::OsKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    Fork,
    Brief,
}

/// How a new agent starts: from the caller's session (fork, `parent_session`) or from a brief, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub context: Context,
    pub reason: &'static str,
    pub parent_session: Option<String>,
}

const NAME_MAX: usize = 24;
const RESERVED: [&str; 3] = ["parent", "root", "all"];
// The session file and workspace.yaml read at most this, like every state file.
const FILE_MAX: u64 = crate::state::fs::STATE_MAX;

// C3: the flags an agent's client command lines may carry, named by their text before '='.
const CLAUDE_FLAGS: [&str; 13] = [
    "--model",
    "--add-dir",
    "--session-id",
    "--continue",
    "-r",
    "--resume",
    "-n",
    "--name",
    "-c",
    "--dangerously-skip-permissions",
    "--permission-mode",
    "--effort",
    "--fork-session",
];
const COPILOT_FLAGS: [&str; 20] = [
    "--model",
    "--add-dir",
    "--session-id",
    "--continue",
    "-r",
    "--resume",
    "-n",
    "--name",
    "--allow-all",
    "--yolo",
    "--no-sandbox",
    "--allow-all-tools",
    "--allow-all-paths",
    "--allow-all-urls",
    "--reasoning-effort",
    "--autopilot",
    "-i",
    "--interactive",
    "-p",
    "--prompt",
];

fn client_name(client: Client) -> &'static str {
    match client {
        Client::Claude => "claude",
        Client::Copilot => "copilot",
    }
}

fn refusal(code: u8, check: &'static str, message: String) -> Refusal {
    Refusal {
        code,
        check,
        message,
        reason: None,
    }
}

/// A lowercase 8-4-4-4-12 GUID. The reference matched it ignoring case, so upper hex passes too; it only ever
/// names a file or a `--resume` value, and no separator can get through.
pub fn session_id(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.iter().map(|g| g.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|g| g.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The requested name after the pattern, reserved and taken checks, or `<client>-<n>` with the smallest free n
/// when none is requested. Taken names compare without case.
pub fn resolve_name(requested: Option<&str>, client: Client, taken: &[&str]) -> Res<String> {
    let is_taken = |name: &str| {
        let name = name.to_lowercase();
        taken.iter().any(|t| t.to_lowercase() == name)
    };
    let Some(name) = requested else {
        let client = client_name(client);
        let free = (1u64..)
            .map(|n| format!("{client}-{n}"))
            .find(|name| !is_taken(name));
        return Ok(free.expect("fewer taken names than numbers"));
    };
    let mut bytes = name.bytes();
    let well_formed = bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && name.len() <= NAME_MAX;
    if !well_formed {
        return Err(refusal(
            2,
            "name",
            "agent names are 1-24 lowercase letters, digits or hyphens, starting with a letter"
                .into(),
        ));
    }
    if RESERVED.contains(&name) {
        return Err(refusal(2, "name", "agent name is reserved".into()));
    }
    if is_taken(name) {
        return Err(refusal(2, "name", "agent name is taken".into()));
    }
    Ok(name.to_string())
}

/// Fork (the new agent starts from the caller's session) only for a same-client agent with a well-formed session
/// id; a Claude fork also needs the interactive entrypoint `cli`, since a fork of a -p or SDK session fails
/// interactively with API 400.
pub fn context_plan(caller: &Caller, client: Client, env: &Env, no_fork: bool) -> Plan {
    let variable = match client {
        Client::Claude => "CLAUDE_CODE_SESSION_ID",
        Client::Copilot => "COPILOT_AGENT_SESSION_ID",
    };
    let parent = env.get(variable).unwrap_or("");
    let reason = if no_fork {
        Some("no-fork")
    } else if caller.kind == Kind::Human {
        Some("human")
    } else if caller.client != Some(client) {
        Some("cross-client")
    } else if !session_id(parent) {
        Some("no-session-id")
    } else if client == Client::Claude && env.get("CLAUDE_CODE_ENTRYPOINT") != Some("cli") {
        Some("not-interactive")
    } else {
        None
    };
    match reason {
        Some(reason) => Plan {
            context: Context::Brief,
            reason,
            parent_session: None,
        },
        None => Plan {
            context: Context::Fork,
            reason: "same-client",
            parent_session: Some(parent.to_string()),
        },
    }
}

/// C3, C2 and C5 for an agent caller, in that order; returns the launch directory. Refusals name only the check
/// (and for C3 the flag).
pub fn assert_spawn_allowed(ctx: &Ctx, caller: &Caller) -> Res<PathBuf> {
    let (Some(agent), Some(client)) = (&caller.agent, caller.client) else {
        return Err(Refusal::internal("spawn checks need an agent caller"));
    };
    let allowed: &[&str] = match client {
        Client::Claude => &CLAUDE_FLAGS,
        Client::Copilot => &COPILOT_FLAGS,
    };
    let start = caller
        .chain
        .iter()
        .position(|row| row.pid == agent.pid)
        .ok_or_else(|| Refusal::internal("the agent is not in its chain"))?;
    let mut agent_argv: &[String] = &[];
    // The agent and every client row above it up to the pane.
    for (index, row) in caller.chain.iter().enumerate().skip(start) {
        if ctx.os.client_of(row).is_none() {
            continue;
        }
        let argv = row
            .argv
            .as_deref()
            .ok_or_else(|| refusal(2, "flags", "a client command line cannot be read".into()))?;
        if index == start {
            agent_argv = argv;
        }
        for argument in argv.iter().skip(1).filter(|a| a.starts_with('-')) {
            let flag = argument.split('=').next().unwrap_or(argument);
            if !allowed.contains(&flag) {
                return Err(refusal(
                    2,
                    "flags",
                    format!("the flag {flag} is not allowed for a spawning agent"),
                ));
            }
        }
    }
    policy(ctx, caller, agent_argv)?;
    launch_directory(ctx, caller, crate::state::root::HOST)
}

const NOT_BYPASS: &str = "the caller's live policy is not full bypass";

/// C2: refuses 2 `policy` unless the caller's live policy is full bypass. A Claude refusal names the half that
/// failed and why, in its message and as its reason.
fn policy(ctx: &Ctx, caller: &Caller, agent_argv: &[String]) -> Res<()> {
    let reason = match caller.client {
        Some(Client::Claude) => match claude_policy(ctx, caller)? {
            Ok(()) => return Ok(()),
            Err(reason) => reason,
        },
        _ if copilot_policy(ctx, agent_argv)? => return Ok(()),
        _ => return Err(refusal(2, "policy", NOT_BYPASS.into())),
    };
    Err(Refusal {
        reason: Some(reason),
        ..refusal(2, "policy", format!("{NOT_BYPASS} ({reason})"))
    })
}

fn contains_ignoring_case(text: &str, needle: &str) -> bool {
    text.to_ascii_lowercase().contains(needle)
}

/// `^\{"type":"permission-mode","permissionMode":"([A-Za-z]+)"`, ignoring case like the reference's -match.
fn permission_mode_line(line: &str) -> bool {
    const HEAD: &str = r#"{"type":"permission-mode","permissionmode":""#;
    let lower = line.to_ascii_lowercase();
    lower.strip_prefix(HEAD).is_some_and(|rest| {
        let letters = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
        letters > 0 && rest.as_bytes().get(letters) == Some(&b'"')
    })
}

// A read that finds no mode yet (a transcript or a footer being written) is tried this often, this far apart.
const MODE_READS: usize = 3;
const MODE_RETRY: std::time::Duration = std::time::Duration::from_millis(250);

/// `read` until it answers anything but `Err(retry)`, at most `MODE_READS` times; its last answer.
fn retried(
    ctx: &Ctx,
    retry: &'static str,
    read: impl Fn() -> Res<Result<(), &'static str>>,
) -> Res<Result<(), &'static str>> {
    let mut answer = read()?;
    for _ in 1..MODE_READS {
        if answer != Err(retry) {
            break;
        }
        ctx.clock.sleep(MODE_RETRY);
        answer = read()?;
    }
    Ok(answer)
}

/// Claude: the latest transcript permission-mode, then the pane's mode line, which catches a Shift+Tab the
/// transcript has not recorded yet. `Err` names the half that is not bypass and why. Only a missing mode is read
/// again; anything else is final.
fn claude_policy(ctx: &Ctx, caller: &Caller) -> Res<Result<(), &'static str>> {
    let session = ctx.env.get("CLAUDE_CODE_SESSION_ID").unwrap_or("");
    if !session_id(session) {
        return Ok(Err("transcript: no session id"));
    }
    let transcript = retried(ctx, "transcript: no mode", || transcript_mode(ctx, session))?;
    if transcript.is_err() {
        return Ok(transcript);
    }
    let pane = caller.pane.as_str();
    let size = crate::mux::native(
        ctx,
        &["display-message", "-p", "-t", pane, "#{pane_height}"],
        true,
    )?;
    let Some(height) = (size.code == 0)
        .then(|| size.stdout.trim().parse::<i32>().ok())
        .flatten()
    else {
        return Ok(Err("screen: unreadable"));
    };
    // A pane no client has sized shows no mode line, so the transcript decides alone.
    if height < 3 {
        return Ok(Ok(()));
    }
    retried(ctx, "screen: no mode line", || pane_mode(ctx, pane))
}

/// Whether the session's transcript last recorded bypass.
fn transcript_mode(ctx: &Ctx, session: &str) -> Res<Result<(), &'static str>> {
    let Some(transcript) = claude_transcript(&ctx.home, session)? else {
        return Ok(Err("transcript: not found"));
    };
    // A transcript no decoder accepts shows no mode, like one without a permission-mode line.
    let Scan::Found(line) = last_line(&transcript, r#""permission-mode""#, permission_mode_line)?
    else {
        return Ok(Err("transcript: no mode"));
    };
    if !line.starts_with(r#"{"type":"permission-mode","permissionMode":"bypassPermissions""#) {
        return Ok(Err("transcript: other mode"));
    }
    Ok(Ok(()))
}

/// Whether the pane's lowest mode line is bypass.
fn pane_mode(ctx: &Ctx, pane: &str) -> Res<Result<(), &'static str>> {
    // -J gives back a footer the pane's width wrapped as one row.
    let screen = crate::mux::native(ctx, &["capture-pane", "-p", "-J", "-t", pane], true)?;
    if screen.code != 0 {
        return Ok(Err("screen: unreadable"));
    }
    Ok(match screen_mode(&screen.stdout) {
        Some(Mode::Bypass) => Ok(()),
        Some(Mode::Other) => Err("screen: other mode"),
        None => Err("screen: no mode line"),
    })
}

// How many non-blank rows at the bottom of the pane are searched for its mode line: the footer, with a spinner, a
// background task, a queued hint and a status line below it.
const MODE_ROWS: usize = 8;

// Claude Code's footer badges, each the start of a mode line.
const BYPASS_BADGE: &str = "⏵⏵ bypass permissions on";
const OTHER_BADGES: [&str; 5] = [
    "⏵⏵ accept edits on",
    "⏵⏵ auto mode on",
    "⏵⏵ don't ask on",
    "⏸ plan mode on",
    "⏸ manual mode on",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Bypass,
    Other,
}

/// The mode a row shows when, after leading whitespace, it starts with a footer badge (words in any case, anything
/// after it); prompt, quoted or mid-sentence text never does.
fn mode_line(row: &str) -> Option<Mode> {
    let row = row.trim_start().to_lowercase();
    if row.starts_with(BYPASS_BADGE) {
        Some(Mode::Bypass)
    } else if OTHER_BADGES.iter().any(|badge| row.starts_with(badge)) {
        Some(Mode::Other)
    } else {
        None
    }
}

/// The lowest mode line among the screen's last `MODE_ROWS` non-blank rows.
fn screen_mode(screen: &str) -> Option<Mode> {
    let rows: Vec<&str> = screen
        .split('\n')
        .map(|row| row.strip_suffix('\r').unwrap_or(row))
        .filter(|row| !row.trim().is_empty())
        .collect();
    rows[rows.len().saturating_sub(MODE_ROWS)..]
        .iter()
        .rev()
        .find_map(|row| mode_line(row))
}

/// ~/.claude/projects/<folder>/<session>.jsonl when exactly one project folder has it.
pub(crate) fn claude_transcript(home: &Path, session: &str) -> std::io::Result<Option<PathBuf>> {
    let projects = home.join(".claude").join("projects");
    if !projects.is_dir() {
        return Ok(None);
    }
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&projects)? {
        let folder = entry?.path();
        let transcript = folder.join(format!("{session}.jsonl"));
        if folder.is_dir() && transcript.is_file() {
            found.push(transcript);
        }
    }
    Ok(if found.len() == 1 { found.pop() } else { None })
}

/// What a transcript scan found. `Undecodable` is a file no decoder accepts; callers refuse on it and never fall
/// back to anything else.
#[derive(Debug, PartialEq, Eq)]
enum Scan {
    Found(String),
    Missing,
    Undecodable,
}

/// The text of `bytes` as .NET's StreamReader reads it: a UTF-8, UTF-16 or UTF-32 BOM names the encoding (UTF-32 LE
/// is tried before UTF-16 LE, whose BOM it starts with), else UTF-8. `None` when the bytes are not valid in it: a
/// replaced character could hide the very line a check looks for.
fn decode(bytes: &[u8]) -> Option<String> {
    if let Some(rest) = bytes.strip_prefix(b"\xef\xbb\xbf") {
        return String::from_utf8(rest.to_vec()).ok();
    }
    if let Some(rest) = bytes.strip_prefix(b"\xff\xfe\x00\x00") {
        return utf32(rest, u32::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(b"\x00\x00\xfe\xff") {
        return utf32(rest, u32::from_be_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(b"\xff\xfe") {
        return utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(b"\xfe\xff") {
        return utf16(rest, u16::from_be_bytes);
    }
    String::from_utf8(bytes.to_vec()).ok()
}

fn utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| unit([c[0], c[1]])).collect();
    String::from_utf16(&units).ok()
}

fn utf32(bytes: &[u8], scalar: fn([u8; 4]) -> u32) -> Option<String> {
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    bytes
        .chunks_exact(4)
        .map(|c| char::from_u32(scalar([c[0], c[1], c[2], c[3]])))
        .collect()
}

/// The last line holding `needle` that `matches` accepts. Lines end at LF, CR LF or a lone CR, as .NET's ReadLine
/// split them. A file without a UTF-16 or UTF-32 BOM is read a line at a time (transcripts grow large); since LF
/// never falls inside a UTF-8 character, a strict check of every line is a strict check of the file.
fn last_line(path: &Path, needle: &str, matches: impl Fn(&str) -> bool) -> std::io::Result<Scan> {
    use std::io::{BufRead, Read};
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut head = Vec::new();
    (&mut reader).take(4).read_to_end(&mut head)?;
    let wide = head.starts_with(b"\xff\xfe")
        || head.starts_with(b"\xfe\xff")
        || head == b"\x00\x00\xfe\xff";
    let mut last = None;
    let mut keep = |text: &str| {
        for line in text.split('\n').flat_map(|line| line.split('\r')) {
            if line.contains(needle) && matches(line) {
                last = Some(line.to_string());
            }
        }
    };
    if wide {
        let mut bytes = head;
        reader.read_to_end(&mut bytes)?;
        let Some(text) = decode(&bytes) else {
            return Ok(Scan::Undecodable);
        };
        keep(&text);
    } else {
        let mut buf = head.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&head).to_vec();
        loop {
            let ended = reader.read_until(b'\n', &mut buf)? == 0;
            if buf.is_empty() {
                break;
            }
            let complete = ended || buf.ends_with(b"\n");
            if !complete {
                continue;
            }
            let Ok(text) = std::str::from_utf8(&buf) else {
                return Ok(Scan::Undecodable);
            };
            keep(text);
            buf.clear();
            if ended {
                break;
            }
        }
    }
    Ok(last.map_or(Scan::Missing, Scan::Found))
}

/// Copilot: the latest permissions event (with none, an allow-all flag at launch), and never plan mode.
fn copilot_policy(ctx: &Ctx, agent_argv: &[String]) -> std::io::Result<bool> {
    let session = ctx.env.get("COPILOT_AGENT_SESSION_ID").unwrap_or("");
    if !session_id(session) {
        return Ok(false);
    }
    let events = copilot_folder(&ctx.home, session).join("events.jsonl");
    if !events.is_file() {
        return Ok(false);
    }
    let permissions = last_line(&events, "session.permissions_changed", |line| {
        contains_ignoring_case(line, r#""type":"session.permissions_changed""#)
    })?;
    let allow_all = match permissions {
        Scan::Found(line) => contains_ignoring_case(&line, r#""allowallpermissions":true"#),
        // Only a file that decodes and has no permissions event falls back to the launch flags.
        Scan::Undecodable => false,
        Scan::Missing => agent_argv.iter().any(|arg| {
            let flag = arg.split('=').next().unwrap_or(arg);
            flag == "--allow-all" || flag == "--yolo"
        }),
    };
    if !allow_all {
        return Ok(false);
    }
    let mode = last_line(&events, "session.mode_changed", |line| {
        contains_ignoring_case(line, r#""type":"session.mode_changed""#)
    })?;
    Ok(match mode {
        Scan::Found(line) => !contains_ignoring_case(&line, r#""plan""#),
        Scan::Missing => true,
        Scan::Undecodable => false,
    })
}

fn copilot_folder(home: &Path, session: &str) -> PathBuf {
    home.join(".copilot").join("session-state").join(session)
}

/// C5: the agent's launch directory. Anything missing or different refuses 2 `directory`. A relative directory
/// refuses too: it would resolve against our own cwd, not the agent's.
fn launch_directory(ctx: &Ctx, caller: &Caller, os: OsKind) -> Res<PathBuf> {
    let directory = match (caller.client, &caller.agent) {
        (Some(Client::Claude), Some(agent)) => match os {
            OsKind::Windows => windows_claude_directory(ctx, agent),
            OsKind::Unix => unix_claude_directory(ctx, agent),
        },
        (Some(Client::Copilot), _) => copilot_directory(ctx),
        _ => None,
    };
    match directory.map(PathBuf::from) {
        Some(path) if path.is_absolute() && path.is_dir() => Ok(path),
        _ => Err(refusal(
            2,
            "directory",
            "the caller's launch directory cannot be confirmed".into(),
        )),
    }
}

fn claude_session_file(ctx: &Ctx, pid: u32) -> PathBuf {
    ctx.home
        .join(".claude")
        .join("sessions")
        .join(format!("{pid}.json"))
}

/// The file's bytes up to `FILE_MAX`; `Err(true)` when it is proved missing, `Err(false)` for any other failure
/// (unreadable, a folder, oversized).
fn read_small(path: &Path) -> Result<Vec<u8>, bool> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| e.kind() == std::io::ErrorKind::NotFound)?;
    let mut bytes = Vec::new();
    file.take(FILE_MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| false)?;
    if bytes.len() as u64 > FILE_MAX {
        return Err(false);
    }
    Ok(bytes)
}

fn json_object(bytes: &[u8]) -> Option<serde_json::Map<String, serde_json::Value>> {
    let text = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let value: serde_json::Value = serde_json::from_slice(text).ok()?;
    // serde_json keeps the last of two equal keys, so an exact duplicate is found in the text itself.
    if case_collides(&value) || duplicate_key(std::str::from_utf8(text).ok()?) {
        return None;
    }
    match value {
        serde_json::Value::Object(map) => Some(map),
        _ => None,
    }
}

/// Whether an object at any depth names one key twice, escapes decoded. Runs only on text serde_json already
/// parsed, so the scan needs no error handling: it tracks objects and arrays and reads each object's keys.
fn duplicate_key(text: &str) -> bool {
    use std::collections::HashSet;
    // One entry per open object (its keys so far) or array (none).
    let mut open: Vec<Option<HashSet<String>>> = Vec::new();
    // Whether the next string in the innermost object is a key.
    let mut expect_key = false;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                open.push(Some(HashSet::new()));
                expect_key = true;
            }
            b'[' => {
                open.push(None);
                expect_key = false;
            }
            b'}' | b']' => {
                open.pop();
                expect_key = false;
            }
            b',' => expect_key = matches!(open.last(), Some(Some(_))),
            b':' => expect_key = false,
            b'"' => {
                let start = i;
                i += 1;
                while bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                if expect_key {
                    let Ok(key) = serde_json::from_str::<String>(&text[start..=i]) else {
                        // A key serde_json read but String cannot hold (a lone surrogate): refuse.
                        return true;
                    };
                    if let Some(Some(keys)) = open.last_mut()
                        && !keys.insert(key)
                    {
                        return true;
                    }
                    expect_key = false;
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// Whether an object at any depth has two keys equal but for case, which ConvertFrom-Json refuses: `cwd` and `CWD`
/// would otherwise let a second value stand beside the one checked.
fn case_collides(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            let mut seen = std::collections::HashSet::new();
            map.keys().any(|key| !seen.insert(key.to_uppercase()))
                || map.values().any(case_collides)
        }
        serde_json::Value::Array(items) => items.iter().any(case_collides),
        _ => false,
    }
}

/// Windows: ~/.claude/sessions/<pid>.json must carry the agent's start (a FILETIME, compared at whole
/// microseconds like the process table) and this session's id.
fn windows_claude_directory(ctx: &Ctx, agent: &Agent) -> Option<String> {
    let session = json_object(&read_small(&claude_session_file(ctx, agent.pid)).ok()?)?;
    let start = match session.get("procStart")? {
        serde_json::Value::String(text) => text.trim().parse::<i64>().ok()?,
        number => number.as_i64()?,
    };
    if start - start % 10 != agent.start - agent.start % 10 {
        return None;
    }
    let id = session.get("sessionId")?.as_str()?;
    // Both sides present: an unset variable never matches a missing id.
    if ctx.env.get("CLAUDE_CODE_SESSION_ID") != Some(id) {
        return None;
    }
    Some(session.get("cwd")?.as_str()?.to_string())
}

/// Unix: the agent's kernel cwd, read between two starts equal to the agent's. A session file proved missing
/// leaves the kernel cwd; one that is present must be an object whose cwd equals it, and any other failure
/// (permission, a folder, oversized, malformed) refuses (Unix.ps1:142-161). sessionId and procStart stay
/// Windows-only (claude-code#36213).
fn unix_claude_directory(ctx: &Ctx, agent: &Agent) -> Option<String> {
    let same = || {
        ctx.os
            .open(agent.pid)
            .is_some_and(|o| o.start == agent.start)
    };
    if !same() {
        return None;
    }
    let cwd = ctx.os.cwd(agent.pid).filter(|cwd| !cwd.is_empty())?;
    if !same() {
        return None;
    }
    match read_small(&claude_session_file(ctx, agent.pid)) {
        Err(true) => Some(cwd),
        Err(false) => None,
        Ok(bytes) => {
            let session = json_object(&bytes)?;
            (session.get("cwd")?.as_str()? == cwd).then_some(cwd)
        }
    }
}

/// Copilot: the first `cwd: ` line of the session's workspace.yaml, quotes and blanks trimmed.
fn copilot_directory(ctx: &Ctx) -> Option<String> {
    let session = ctx.env.get("COPILOT_AGENT_SESSION_ID").unwrap_or("");
    if !session_id(session) {
        return None;
    }
    let bytes = read_small(&copilot_folder(&ctx.home, session).join("workspace.yaml")).ok()?;
    let text = decode(&bytes)?;
    let line = text
        .split(['\r', '\n'])
        .find(|line| line.starts_with("cwd: "))?;
    let directory = line[5..].trim().trim_matches(['"', '\'']);
    (!directory.is_empty()).then(|| directory.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proc::Row;
    use crate::testing::{self, Fakes, TempDir};

    const SID: &str = "11111111-2222-4333-8444-555555555555";
    const OTHER_SID: &str = "99999999-2222-4333-8444-555555555555";
    const CLAUDE: u32 = 150;
    const UNCONFIRMED: &str = "2 directory the caller's launch directory cannot be confirmed";

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn argv_row(pid: u32, ppid: u32, start: i64, argv: Option<&[&str]>) -> Row {
        Row {
            argv: argv.map(strings),
            ..testing::row(pid, ppid, start)
        }
    }

    // caller 4242 > shell 170 > agent 150 > pane 100.
    fn agent_caller(client: Client, agent_argv: Option<&[&str]>) -> Caller {
        let chain = vec![
            argv_row(4242, 170, 2000, Some(&["mux-agent", "spawn"])),
            argv_row(170, CLAUDE, 1700, Some(&["bash"])),
            argv_row(CLAUDE, 100, 1500, agent_argv),
            argv_row(100, 900, 1000, Some(&["bash"])),
        ];
        Caller {
            kind: Kind::Agent,
            client: Some(client),
            agent: Some(Agent {
                pid: CLAUDE,
                start: 1500,
                path: None,
            }),
            chain,
            pane: "%1".into(),
            window: "@1".into(),
            session: "$3".into(),
            server_pid: 900,
            server_start: 1,
            socket: "/tmp/rmux".into(),
            server_key: "0123456789abcdef".into(),
            agent_tag: String::new(),
        }
    }

    fn human_caller() -> Caller {
        Caller {
            kind: Kind::Human,
            client: None,
            agent: None,
            ..agent_caller(Client::Claude, Some(&["claude"]))
        }
    }

    fn answer<T: std::fmt::Debug>(result: Res<T>) -> String {
        match result {
            Ok(value) => format!("{value:?}"),
            Err(r) => format!("{} {} {}", r.code, r.check, r.message),
        }
    }

    // C2's answer: `true`, the reason a Claude refusal names, `false` for a refusal naming none (Copilot's), else the
    // refusal. A reason is always the message's suffix.
    fn policy_answer(result: Res<()>) -> String {
        match result {
            Ok(()) => "true".into(),
            Err(r) if r.check == "policy" => match r.reason {
                Some(reason) => {
                    assert_eq!(r.message, format!("{POLICY} ({reason})"));
                    reason.into()
                }
                None => {
                    assert_eq!(r.message, POLICY);
                    "false".into()
                }
            },
            Err(r) => format!("{} {} {}", r.code, r.check, r.message),
        }
    }

    // ---- names and context

    #[test]
    fn name_rules_reserved_taken_auto_n() {
        let name = |requested: Option<&str>, client, taken: &[&str]| {
            answer(resolve_name(requested, client, taken))
        };
        assert_eq!(name(None, Client::Claude, &[]), "\"claude-1\"");
        assert_eq!(
            name(
                None,
                Client::Copilot,
                &["copilot-1", "COPILOT-2", "copilot-4"]
            ),
            "\"copilot-3\""
        );
        let ok = "a".repeat(24);
        for good in ["a", "worker", "w-2", "a-", ok.as_str()] {
            assert_eq!(
                name(Some(good), Client::Claude, &[]),
                format!("{good:?}"),
                "{good}"
            );
        }
        let pattern = "2 name agent names are 1-24 lowercase letters, digits or hyphens, starting with a letter";
        let long = "a".repeat(25);
        for bad in [
            "",
            "1a",
            "-a",
            "Worker",
            "a_b",
            "a b",
            "a\n",
            "\u{212a}",
            "é",
            long.as_str(),
        ] {
            assert_eq!(name(Some(bad), Client::Claude, &[]), pattern, "{bad:?}");
        }
        for reserved in ["parent", "root", "all"] {
            assert_eq!(
                name(Some(reserved), Client::Claude, &[]),
                "2 name agent name is reserved"
            );
        }
        // Taken names compare without case.
        assert_eq!(
            name(Some("worker"), Client::Claude, &["other", "WORKER"]),
            "2 name agent name is taken"
        );
    }

    #[test]
    fn context_plan_reasons() {
        let plan = |caller: &Caller, client, pairs: &[(&str, &str)], no_fork| {
            let p = context_plan(
                caller,
                client,
                &Env::from_pairs(pairs.iter().copied()),
                no_fork,
            );
            format!(
                "{:?} {} {}",
                p.context,
                p.reason,
                p.parent_session.as_deref().unwrap_or("-")
            )
        };
        let claude = agent_caller(Client::Claude, Some(&["claude"]));
        let copilot = agent_caller(Client::Copilot, Some(&["copilot"]));
        let cli = [
            ("CLAUDE_CODE_SESSION_ID", SID),
            ("CLAUDE_CODE_ENTRYPOINT", "cli"),
        ];
        assert_eq!(
            plan(&claude, Client::Claude, &cli, false),
            format!("Fork same-client {SID}")
        );
        assert_eq!(plan(&claude, Client::Claude, &cli, true), "Brief no-fork -");
        assert_eq!(
            plan(&human_caller(), Client::Claude, &cli, false),
            "Brief human -"
        );
        assert_eq!(
            plan(&claude, Client::Copilot, &cli, false),
            "Brief cross-client -"
        );
        for bad in [
            "",
            "not-a-guid",
            "11111111-2222-4333-8444-55555555555",
            "x1111111-2222-4333-8444-555555555555",
        ] {
            let env = [
                ("CLAUDE_CODE_SESSION_ID", bad),
                ("CLAUDE_CODE_ENTRYPOINT", "cli"),
            ];
            assert_eq!(
                plan(&claude, Client::Claude, &env, false),
                "Brief no-session-id -",
                "{bad:?}"
            );
        }
        assert_eq!(
            plan(
                &claude,
                Client::Claude,
                &[("CLAUDE_CODE_ENTRYPOINT", "cli")],
                false
            ),
            "Brief no-session-id -"
        );
        // Only the interactive entrypoint, exactly, forks a Claude session.
        for entry in [None, Some("sdk-ts"), Some("CLI")] {
            let mut env = vec![("CLAUDE_CODE_SESSION_ID", SID)];
            env.extend(entry.map(|e| ("CLAUDE_CODE_ENTRYPOINT", e)));
            assert_eq!(
                plan(&claude, Client::Claude, &env, false),
                "Brief not-interactive -",
                "{entry:?}"
            );
        }
        // Copilot needs no entrypoint and reads its own variable.
        assert_eq!(
            plan(
                &copilot,
                Client::Copilot,
                &[("COPILOT_AGENT_SESSION_ID", SID)],
                false
            ),
            format!("Fork same-client {SID}")
        );
        assert_eq!(
            plan(&copilot, Client::Copilot, &cli, false),
            "Brief no-session-id -"
        );
    }

    // ---- C3

    const POLICY: &str = "the caller's live policy is not full bypass";
    // Flags pass when the next check, policy, is the one that refuses (no session id here).
    const NEXT: &str = "2 policy the caller's live policy is not full bypass";
    const NEXT_CLAUDE: &str =
        "2 policy the caller's live policy is not full bypass (transcript: no session id)";

    fn flags(client: Client, agent_argv: Option<&[&str]>, extra: Option<Row>) -> String {
        let fakes = Fakes::default();
        fakes.client(CLAUDE, client);
        let mut caller = agent_caller(client, agent_argv);
        if let Some(row) = extra {
            fakes.client(row.pid, Client::Copilot);
            caller.chain.insert(3, row);
        }
        answer(assert_spawn_allowed(&fakes.ctx(), &caller))
    }

    #[test]
    fn flags_refuses_unlisted_flag_names_it() {
        let refused = flags(
            Client::Claude,
            Some(&[
                "claude",
                "--model",
                "opus",
                "--settings=/secret path/s.json",
            ]),
            None,
        );
        assert_eq!(
            refused,
            "2 flags the flag --settings is not allowed for a spawning agent"
        );
        assert!(!refused.contains("secret"));
        // Case-sensitive, and the caller's own client list decides.
        assert_eq!(
            flags(Client::Claude, Some(&["claude", "--Model"]), None),
            "2 flags the flag --Model is not allowed for a spawning agent"
        );
        assert_eq!(
            flags(Client::Claude, Some(&["claude", "--yolo"]), None),
            "2 flags the flag --yolo is not allowed for a spawning agent"
        );
        assert_eq!(
            flags(
                Client::Copilot,
                Some(&["copilot", "--dangerously-skip-permissions"]),
                None
            ),
            "2 flags the flag --dangerously-skip-permissions is not allowed for a spawning agent"
        );
        // argv[0] and anything not starting with '-' are not flags; every listed flag passes.
        let listed = [
            "-claude",
            "task",
            "--model=opus",
            "--add-dir",
            "/x",
            "--session-id",
            SID,
            "--continue",
            "-r",
            "--resume",
            "-n",
            "--name",
            "-c",
            "--dangerously-skip-permissions",
            "--permission-mode=bypassPermissions",
            "--effort",
            "--fork-session",
        ];
        assert_eq!(flags(Client::Claude, Some(&listed), None), NEXT_CLAUDE);
        let copilot = [
            "copilot",
            "--model",
            "--add-dir",
            "--session-id",
            "--continue",
            "-r",
            "--resume",
            "-n",
            "--name",
            "--allow-all",
            "--yolo",
            "--no-sandbox",
            "--allow-all-tools",
            "--allow-all-paths",
            "--allow-all-urls",
            "--reasoning-effort=high",
            "--autopilot",
            "-i",
            "--interactive",
            "-p",
            "--prompt",
        ];
        assert_eq!(flags(Client::Copilot, Some(&copilot), None), NEXT);
        // A client row above the agent is checked too; a non-client row is not.
        let above = argv_row(120, 100, 1200, Some(&["copilot", "--secret-flag"]));
        assert_eq!(
            flags(Client::Claude, Some(&["claude"]), Some(above)),
            "2 flags the flag --secret-flag is not allowed for a spawning agent"
        );
        let mut caller = agent_caller(Client::Claude, Some(&["claude"]));
        caller.chain[3].argv = Some(strings(&["bash", "--login"]));
        caller.chain[1].argv = None;
        let fakes = Fakes::default();
        fakes.client(CLAUDE, Client::Claude);
        assert_eq!(
            answer(assert_spawn_allowed(&fakes.ctx(), &caller)),
            NEXT_CLAUDE
        );
    }

    #[test]
    fn flags_unreadable_argv_refuses() {
        let unreadable = "2 flags a client command line cannot be read";
        assert_eq!(flags(Client::Claude, None, None), unreadable);
        let above = argv_row(120, 100, 1200, None);
        assert_eq!(
            flags(Client::Claude, Some(&["claude"]), Some(above)),
            unreadable
        );
        // A human caller never reaches these checks.
        let fakes = Fakes::default();
        assert_eq!(
            answer(assert_spawn_allowed(&fakes.ctx(), &human_caller())),
            "1 internal unexpected error (spawn checks need an agent caller)"
        );
    }

    // ---- C2

    struct Claude {
        transcript: Vec<String>,
        // The file's bytes instead of the transcript lines as UTF-8.
        bytes: Option<Vec<u8>>,
        height: Option<&'static str>,
        screen: Option<String>,
        sid: &'static str,
        folders: usize,
    }

    impl Default for Claude {
        fn default() -> Self {
            Claude {
                transcript: vec![mode("bypassPermissions")],
                bytes: None,
                height: Some("40\n"),
                screen: Some(format!("work\n\n{BYPASS}\n\n")),
                sid: SID,
                folders: 1,
            }
        }
    }

    fn mode(name: &str) -> String {
        format!(r#"{{"type":"permission-mode","permissionMode":"{name}","sessionId":"{SID}"}}"#)
    }

    const BYPASS: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";

    // The case's home, fakes and ctx; its answer is `claude_answer`.
    fn claude_setup(case: &Claude) -> (TempDir, Fakes, Ctx) {
        let home = TempDir::new();
        for folder in 0..case.folders {
            let project = home
                .path()
                .join(".claude/projects")
                .join(format!("p{folder}"));
            std::fs::create_dir_all(&project).unwrap();
            let text = case.transcript.join("\n") + "\n";
            let bytes = case.bytes.clone().unwrap_or_else(|| text.into_bytes());
            std::fs::write(project.join(format!("{SID}.jsonl")), bytes).unwrap();
        }
        let fakes = Fakes::default();
        match case.height {
            Some(out) => fakes.mux_rule("#{pane_height}", 0, out, ""),
            None => fakes.mux_rule("#{pane_height}", 1, "", ""),
        }
        match &case.screen {
            Some(out) => fakes.mux_rule("capture-pane", 0, out, ""),
            None => fakes.mux_rule("capture-pane", 1, "", ""),
        }
        let mut ctx = fakes.ctx();
        ctx.home = home.path().to_path_buf();
        ctx.env = Env::from_pairs([("CLAUDE_CODE_SESSION_ID", case.sid)]);
        (home, fakes, ctx)
    }

    fn claude_answer(ctx: &Ctx) -> String {
        let caller = agent_caller(Client::Claude, Some(&["claude"]));
        policy_answer(policy(ctx, &caller, &[]))
    }

    fn claude_policy(case: Claude) -> String {
        let (_home, fakes, ctx) = claude_setup(&case);
        let result = claude_answer(&ctx);
        let calls = fakes.mux_calls();
        if result == "true" && case.screen.is_some() {
            assert_eq!(
                calls,
                [
                    vec!["-N", "display-message", "-p", "-t", "%1", "#{pane_height}"],
                    vec!["-N", "capture-pane", "-p", "-J", "-t", "%1"]
                ]
            );
        }
        result
    }

    #[test]
    fn policy_claude_transcript_and_mode_line() {
        assert_eq!(claude_policy(Claude::default()), "true");
        // The latest permission-mode line decides, exactly.
        for (lines, want) in [
            (vec![mode("default"), mode("bypassPermissions")], "true"),
            (
                vec![mode("bypassPermissions"), mode("default")],
                "transcript: other mode",
            ),
            (
                vec![mode("bypassPermissions"), mode("acceptEdits")],
                "transcript: other mode",
            ),
            (vec![mode("BypassPermissions")], "transcript: other mode"),
            (
                vec![
                    mode("bypassPermissions"),
                    "{\"type\":\"user\",\"text\":\"permission-mode\"}".into(),
                ],
                "true",
            ),
            (
                vec![
                    mode("bypassPermissions"),
                    " {\"type\":\"permission-mode\",\"permissionMode\":\"default\"}".into(),
                ],
                "true",
            ),
            (vec!["{\"type\":\"user\"}".into()], "transcript: no mode"),
            (vec![], "transcript: no mode"),
        ] {
            let case = Claude {
                transcript: lines.clone(),
                ..Claude::default()
            };
            assert_eq!(claude_policy(case), want, "{lines:?}");
        }
        // A pane under 3 rows shows no mode line, so the transcript decides alone.
        for height in ["2\n", " 0 "] {
            let case = Claude {
                height: Some(height),
                screen: None,
                ..Claude::default()
            };
            assert_eq!(claude_policy(case), "true", "{height:?}");
        }
        let small_pane = Claude {
            height: Some("2\n"),
            transcript: vec![mode("default")],
            ..Claude::default()
        };
        assert_eq!(claude_policy(small_pane), "transcript: other mode");
        for height in [None, Some("x\n"), Some("\n")] {
            let case = Claude {
                height,
                ..Claude::default()
            };
            assert_eq!(claude_policy(case), "screen: unreadable", "{height:?}");
        }
        // The lowest mode line in the last 8 non-blank rows must be the bypass badge, any case.
        for (screen, want) in [
            (Some("⏵⏵ BYPASS PERMISSIONS ON\na\n\n \nb\nc\n"), "true"),
            (Some("  ⏵⏵ bypass permissions on\r\nwork\r\n"), "true"),
            (Some("bypass permissions on\na\n"), "screen: no mode line"),
            (Some("> \n  ? for shortcuts\n"), "screen: no mode line"),
            (
                Some("⏵⏵ bypass permissions off\r\n"),
                "screen: no mode line",
            ),
            (None, "screen: unreadable"),
        ] {
            let case = Claude {
                screen: screen.map(String::from),
                ..Claude::default()
            };
            assert_eq!(claude_policy(case), want, "{screen:?}");
        }
        // A malformed session id or a transcript in no project, or in two, is not bypass.
        for sid in ["", "not-a-session"] {
            assert_eq!(
                claude_policy(Claude {
                    sid,
                    ..Claude::default()
                }),
                "transcript: no session id"
            );
        }
        for folders in [0, 2] {
            assert_eq!(
                claude_policy(Claude {
                    folders,
                    ..Claude::default()
                }),
                "transcript: not found"
            );
        }
    }

    fn copilot_policy(events: Option<&[&str]>, argv: &[&str]) -> String {
        let bytes = events.map(|lines| lines.join("\n").into_bytes());
        copilot_policy_bytes(bytes.as_deref(), argv)
    }

    fn copilot_policy_bytes(events: Option<&[u8]>, argv: &[&str]) -> String {
        let home = TempDir::new();
        if let Some(events) = events {
            let folder = home.path().join(".copilot/session-state").join(SID);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("events.jsonl"), events).unwrap();
        }
        let fakes = Fakes::default();
        let mut ctx = fakes.ctx();
        ctx.home = home.path().to_path_buf();
        ctx.env = Env::from_pairs([("COPILOT_AGENT_SESSION_ID", SID)]);
        let caller = agent_caller(Client::Copilot, Some(argv));
        let result = policy_answer(policy(&ctx, &caller, &strings(argv)));
        assert!(fakes.mux_calls().is_empty());
        result
    }

    #[test]
    fn policy_copilot_permissions_and_plan_mode() {
        let allow = |on: bool| {
            format!(
                r#"{{"type":"session.permissions_changed","data":{{"allowAllPermissions":{on}}}}}"#
            )
        };
        let plan = r#"{"type":"session.mode_changed","data":{"newMode":"plan"}}"#;
        let interactive = r#"{"type":"session.mode_changed","data":{"newMode":"interactive"}}"#;
        let start = r#"{"type":"session.start"}"#;
        let (on, off) = (allow(true), allow(false));
        let copilot = ["copilot"];
        // The latest permissions event decides; with none, an allow-all flag at launch.
        assert_eq!(copilot_policy(Some(&[&on]), &copilot), "true");
        assert_eq!(copilot_policy(Some(&[&on, &off]), &copilot), "false");
        assert_eq!(copilot_policy(Some(&[&off, &on]), &copilot), "true");
        assert_eq!(
            copilot_policy(Some(&[&off]), &["copilot", "--yolo"]),
            "false"
        );
        assert_eq!(copilot_policy(Some(&[start]), &copilot), "false");
        for flag in ["--yolo", "--allow-all", "--allow-all=true"] {
            assert_eq!(
                copilot_policy(Some(&[start]), &["copilot", flag]),
                "true",
                "{flag}"
            );
        }
        assert_eq!(
            copilot_policy(Some(&[start]), &["copilot", "--allow-all-tools"]),
            "false"
        );
        // Plan mode is never bypass; a later mode change out of it is.
        assert_eq!(copilot_policy(Some(&[&on, plan]), &copilot), "false");
        assert_eq!(
            copilot_policy(Some(&[plan, &on, interactive]), &copilot),
            "true"
        );
        // No events file is not bypass.
        assert_eq!(copilot_policy(None, &["copilot", "--yolo"]), "false");
    }

    // ---- C5

    fn ctx_at(fakes: &Fakes, home: &Path, env: &[(&str, &str)]) -> Ctx {
        let mut ctx = fakes.ctx();
        ctx.home = home.to_path_buf();
        ctx.env = Env::from_pairs(env.iter().copied());
        ctx
    }

    fn sessions(home: &Path) -> PathBuf {
        let folder = home.join(".claude/sessions");
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    fn found(path: &Path) -> String {
        format!("{path:?}")
    }

    #[test]
    fn directory_windows_session_json_start_and_id() {
        let (home, work) = (TempDir::new(), TempDir::new());
        let caller = agent_caller(Client::Claude, Some(&["claude"]));
        let write = |session: serde_json::Value| {
            std::fs::write(sessions(home.path()).join("150.json"), session.to_string()).unwrap();
        };
        let run = |env: &[(&str, &str)]| {
            let fakes = Fakes::default();
            answer(launch_directory(
                &ctx_at(&fakes, home.path(), env),
                &caller,
                OsKind::Windows,
            ))
        };
        let env = [("CLAUDE_CODE_SESSION_ID", SID)];
        let cwd = work.path().to_str().unwrap();
        // procStart is a FILETIME compared at whole microseconds, as text or a number.
        for start in [
            serde_json::json!("1500"),
            serde_json::json!(1509),
            serde_json::json!(1500),
        ] {
            write(
                serde_json::json!({"pid": 150, "sessionId": SID, "cwd": cwd, "procStart": start}),
            );
            assert_eq!(run(&env), found(work.path()), "{start}");
        }
        for session in [
            serde_json::json!({"sessionId": SID, "cwd": cwd, "procStart": "1510"}),
            serde_json::json!({"sessionId": SID, "cwd": cwd, "procStart": "x"}),
            // Keys equal but for case: ConvertFrom-Json refuses the object.
            serde_json::json!({"sessionId": SID, "cwd": cwd, "CWD": "/different", "procStart": "1500"}),
            serde_json::json!({"sessionId": SID, "cwd": cwd}),
            serde_json::json!({"sessionId": OTHER_SID, "cwd": cwd, "procStart": "1500"}),
            serde_json::json!({"cwd": cwd, "procStart": "1500"}),
            serde_json::json!({"sessionId": SID, "cwd": 7, "procStart": "1500"}),
            serde_json::json!({"sessionId": SID, "procStart": "1500"}),
            serde_json::json!({"sessionId": SID, "cwd": "relative", "procStart": "1500"}),
            serde_json::json!([1]),
        ] {
            write(session.clone());
            assert_eq!(run(&env), UNCONFIRMED, "{session}");
        }
        // No session id in the environment never matches, nor does a missing file.
        write(serde_json::json!({"cwd": cwd, "procStart": "1500"}));
        assert_eq!(run(&[]), UNCONFIRMED);
        std::fs::remove_file(sessions(home.path()).join("150.json")).unwrap();
        assert_eq!(run(&env), UNCONFIRMED);
    }

    // The Unix C5 read for agent 150 whose opens answer `starts` and whose kernel cwd is `cwd`.
    fn unix_directory(
        home: &Path,
        starts: &[Option<i64>],
        cwd: Option<&str>,
    ) -> (String, Vec<String>) {
        let fakes = Fakes::default();
        fakes.opens(CLAUDE, starts);
        if let Some(cwd) = cwd {
            fakes.kernel_cwd(CLAUDE, cwd);
        }
        let caller = agent_caller(Client::Claude, Some(&["claude"]));
        let ctx = ctx_at(&fakes, home, &[]);
        (
            answer(launch_directory(&ctx, &caller, OsKind::Unix)),
            fakes.os_calls(),
        )
    }

    #[test]
    fn directory_unix_cwd_between_two_starts() {
        let (home, work) = (TempDir::new(), TempDir::new());
        let cwd = work.path().to_str().unwrap();
        let (result, calls) = unix_directory(home.path(), &[Some(1500), Some(1500)], Some(cwd));
        assert_eq!(result, found(work.path()));
        assert_eq!(calls, ["open 150", "cwd 150", "open 150"]);
        for starts in [
            [Some(1501), Some(1500)],
            [Some(1500), Some(1501)],
            [None, Some(1500)],
            [Some(1500), None],
        ] {
            assert_eq!(
                unix_directory(home.path(), &starts, Some(cwd)).0,
                UNCONFIRMED,
                "{starts:?}"
            );
        }
        assert_eq!(
            unix_directory(home.path(), &[Some(1500), Some(1500)], None).0,
            UNCONFIRMED
        );
        assert_eq!(
            unix_directory(home.path(), &[Some(1500), Some(1500)], Some("")).0,
            UNCONFIRMED
        );
        // A cwd that is gone is not a launch directory.
        let gone = work.path().join("gone");
        assert_eq!(
            unix_directory(home.path(), &[Some(1500), Some(1500)], gone.to_str()).0,
            UNCONFIRMED
        );
    }

    fn with_session_file(contents: &[u8]) -> (String, TempDir, TempDir) {
        let (home, work) = (TempDir::new(), TempDir::new());
        std::fs::write(sessions(home.path()).join("150.json"), contents).unwrap();
        let cwd = work.path().to_str().unwrap();
        let result = unix_directory(home.path(), &[Some(1500), Some(1500)], Some(cwd)).0;
        (result, home, work)
    }

    #[test]
    fn directory_session_file_cwd_mismatch_refuses() {
        let (result, _home, _work) = with_session_file(br#"{"cwd":"/somewhere/else"}"#);
        assert_eq!(result, UNCONFIRMED);
        let (home, work) = (TempDir::new(), TempDir::new());
        let cwd = work.path().to_str().unwrap();
        let session = serde_json::json!({"pid": 150, "cwd": cwd, "sessionId": OTHER_SID});
        std::fs::write(sessions(home.path()).join("150.json"), session.to_string()).unwrap();
        // sessionId and procStart stay Windows-only (claude-code#36213); the cwd must equal the kernel's.
        assert_eq!(
            unix_directory(home.path(), &[Some(1500), Some(1500)], Some(cwd)).0,
            found(work.path())
        );
    }

    #[test]
    fn directory_session_file_malformed_refuses() {
        for contents in [
            &b""[..],
            b"not json",
            b"[]",
            b"\"cwd\"",
            b"{\"cwd\":7}",
            b"{\"cwd\":\"\"}",
            b"{\"cwd\":null}",
            b"{}",
            b"{\"cwd\":\"/x\"",
            b"\xff\xfe{}",
        ] {
            let (result, _home, _work) = with_session_file(contents);
            assert_eq!(
                result,
                UNCONFIRMED,
                "{:?}",
                String::from_utf8_lossy(contents)
            );
        }
        // Keys equal but for case, at any depth: ConvertFrom-Json refuses the object, so the file is malformed.
        let (home, work) = (TempDir::new(), TempDir::new());
        let cwd = work.path().to_str().unwrap();
        let quoted = serde_json::to_string(cwd).unwrap();
        let file = sessions(home.path()).join("150.json");
        let run = |contents: &str| {
            std::fs::write(&file, contents).unwrap();
            unix_directory(home.path(), &[Some(1500), Some(1500)], Some(cwd)).0
        };
        for contents in [
            format!(r#"{{"cwd":{quoted},"CWD":"/different"}}"#),
            format!(r#"{{"CWD":"/different","cwd":{quoted}}}"#),
            format!(r#"{{"cwd":{quoted},"meta":{{"a":1,"A":2}}}}"#),
            format!(r#"{{"cwd":{quoted},"list":[{{"k":1,"K":2}}]}}"#),
        ] {
            assert_eq!(run(&contents), UNCONFIRMED, "{contents}");
        }
        assert_eq!(
            run(&format!(r#"{{"cwd":{quoted},"meta":{{"a":1,"b":2}}}}"#)),
            found(work.path())
        );
    }

    #[test]
    fn directory_session_file_duplicate_key_refuses() {
        // An exact duplicate key is malformed: a second value must never stand beside the one checked.
        let (home, work) = (TempDir::new(), TempDir::new());
        let cwd = work.path().to_str().unwrap();
        let quoted = serde_json::to_string(cwd).unwrap();
        let file = sessions(home.path()).join("150.json");
        let unix = |contents: &str| {
            std::fs::write(&file, contents).unwrap();
            unix_directory(home.path(), &[Some(1500), Some(1500)], Some(cwd)).0
        };
        let refused = [
            format!(r#"{{"cwd":"/other","cwd":{quoted}}}"#),
            format!(r#"{{"cwd":{quoted},"cwd":"/other"}}"#),
            format!(r#"{{"cwd":{quoted},"cwd":{quoted}}}"#),
            format!(r#"{{"cwd":{quoted},"meta":{{"a":1,"a":2}}}}"#),
            format!(r#"{{"cwd":{quoted},"list":[{{"k":1,"k":2}}]}}"#),
            // An escaped key is the same key.
            format!(r#"{{"cwd":"/other","c\u0077d":{quoted}}}"#),
        ];
        for contents in &refused {
            assert_eq!(unix(contents), UNCONFIRMED, "{contents}");
        }
        // The same key in different objects, or as a value, is no duplicate.
        for contents in [
            format!(r#"{{"cwd":{quoted},"a":{{"cwd":1}},"b":{{"cwd":2}}}}"#),
            format!(r#"{{"cwd":{quoted},"note":"cwd","list":[{{"k":1}},{{"k":2}}]}}"#),
        ] {
            assert_eq!(unix(&contents), found(work.path()), "{contents}");
        }
        // Windows reads the same file for the same decision.
        let caller = agent_caller(Client::Claude, Some(&["claude"]));
        let windows = |contents: &str| {
            std::fs::write(&file, contents).unwrap();
            let fakes = Fakes::default();
            let env = [("CLAUDE_CODE_SESSION_ID", SID)];
            answer(launch_directory(
                &ctx_at(&fakes, home.path(), &env),
                &caller,
                OsKind::Windows,
            ))
        };
        let good = format!(r#"{{"sessionId":"{SID}","cwd":{quoted},"procStart":"1500"}}"#);
        assert_eq!(windows(&good), found(work.path()));
        for contents in [
            format!(r#"{{"sessionId":"{SID}","cwd":"/other","cwd":{quoted},"procStart":"1500"}}"#),
            format!(
                r#"{{"sessionId":"{OTHER_SID}","sessionId":"{SID}","cwd":{quoted},"procStart":"1500"}}"#
            ),
            format!(r#"{{"sessionId":"{SID}","cwd":{quoted},"procStart":"9","procStart":"1500"}}"#),
        ] {
            assert_eq!(windows(&contents), UNCONFIRMED, "{contents}");
        }
    }

    #[test]
    fn directory_session_file_oversize_refuses() {
        let (home, work) = (TempDir::new(), TempDir::new());
        let cwd = work.path().to_str().unwrap();
        let body = serde_json::json!({"cwd": cwd}).to_string();
        let file = sessions(home.path()).join("150.json");
        // Exactly 1 MB is read; one byte more is not.
        let fits = format!("{body}{}", " ".repeat((1 << 20) - body.len()));
        std::fs::write(&file, &fits).unwrap();
        let run = || unix_directory(home.path(), &[Some(1500), Some(1500)], Some(cwd)).0;
        assert_eq!(run(), found(work.path()));
        std::fs::write(&file, format!("{fits} ")).unwrap();
        assert_eq!(run(), UNCONFIRMED);
    }

    #[test]
    fn directory_session_file_is_directory_refuses() {
        let (home, work) = (TempDir::new(), TempDir::new());
        std::fs::create_dir(sessions(home.path()).join("150.json")).unwrap();
        let cwd = work.path().to_str().unwrap();
        assert_eq!(
            unix_directory(home.path(), &[Some(1500), Some(1500)], Some(cwd)).0,
            UNCONFIRMED
        );
    }

    #[test]
    fn directory_session_file_absent_uses_kernel_cwd() {
        let (home, work) = (TempDir::new(), TempDir::new());
        let cwd = work.path().to_str().unwrap();
        let run = || unix_directory(home.path(), &[Some(1500), Some(1500)], Some(cwd)).0;
        // No sessions folder, then a folder without this pid's file.
        assert_eq!(run(), found(work.path()));
        std::fs::write(sessions(home.path()).join("151.json"), "{}").unwrap();
        assert_eq!(run(), found(work.path()));
    }

    #[cfg(unix)]
    #[test]
    fn directory_session_file_unreadable_refuses() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid only reads the caller's id.
        if unsafe { libc::geteuid() } == 0 {
            return; // root reads a 000 file
        }
        let (home, work) = (TempDir::new(), TempDir::new());
        let cwd = work.path().to_str().unwrap();
        let file = sessions(home.path()).join("150.json");
        std::fs::write(&file, serde_json::json!({"cwd": cwd}).to_string()).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = unix_directory(home.path(), &[Some(1500), Some(1500)], Some(cwd)).0;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(result, UNCONFIRMED);
    }

    fn copilot_directory(yaml: Option<&str>, sid: &str) -> String {
        copilot_directory_bytes(yaml.map(str::as_bytes), sid)
    }

    fn copilot_directory_bytes(yaml: Option<&[u8]>, sid: &str) -> String {
        let home = TempDir::new();
        if let Some(yaml) = yaml {
            let folder = home.path().join(".copilot/session-state").join(SID);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("workspace.yaml"), yaml).unwrap();
        }
        let fakes = Fakes::default();
        let ctx = ctx_at(&fakes, home.path(), &[("COPILOT_AGENT_SESSION_ID", sid)]);
        let caller = agent_caller(Client::Copilot, Some(&["copilot"]));
        let result = answer(launch_directory(&ctx, &caller, crate::state::root::HOST));
        assert!(fakes.os_calls().is_empty());
        result
    }

    #[test]
    fn directory_copilot_workspace_yaml_cwd() {
        let work = TempDir::new();
        let cwd = work.path().to_str().unwrap();
        let yaml = format!("id: {SID}\r\ncwd: {cwd}  \r\ncwd: /elsewhere\r\nsummary: x\r\n");
        assert_eq!(copilot_directory(Some(&yaml), SID), found(work.path()));
        // The first line starting exactly `cwd: ` decides.
        let yaml = format!("  cwd: /elsewhere\nCWD: /elsewhere\ncwd: {cwd}\n");
        assert_eq!(copilot_directory(Some(&yaml), SID), found(work.path()));
        let gone = work.path().join("gone");
        let yaml = format!("cwd: {}\n", gone.display());
        assert_eq!(copilot_directory(Some(&yaml), SID), UNCONFIRMED);
        assert_eq!(copilot_directory(Some("cwd: relative\n"), SID), UNCONFIRMED);
    }

    #[test]
    fn directory_copilot_workspace_yaml_quoted_cwd() {
        let work = TempDir::new();
        let cwd = work.path().to_str().unwrap();
        for yaml in [
            format!("cwd: \"{cwd}\"\n"),
            format!("cwd: '{cwd}'\n"),
            format!("cwd:  \"'{cwd}'\" \n"),
            format!("\u{feff}cwd: {cwd}"),
        ] {
            assert_eq!(
                copilot_directory(Some(&yaml), SID),
                found(work.path()),
                "{yaml:?}"
            );
        }
    }

    #[test]
    fn directory_copilot_workspace_yaml_missing_refuses() {
        let work = TempDir::new();
        let yaml = format!("cwd: {}\n", work.path().display());
        assert_eq!(copilot_directory(None, SID), UNCONFIRMED);
        assert_eq!(copilot_directory(Some("id: x\n"), SID), UNCONFIRMED);
        assert_eq!(copilot_directory(Some("cwd: \n"), SID), UNCONFIRMED);
        // A malformed session id never builds a path.
        assert_eq!(copilot_directory(Some(&yaml), "../x"), UNCONFIRMED);
        assert_eq!(copilot_directory(Some(&yaml), ""), UNCONFIRMED);
    }

    #[test]
    fn directory_missing_refuses_2() {
        // A launch directory that names a file, or none at all, refuses 2 directory through the whole check.
        let (home, work) = (TempDir::new(), TempDir::new());
        let file = work.path().join("file");
        std::fs::write(&file, "x").unwrap();
        let yaml_folder = home.path().join(".copilot/session-state").join(SID);
        std::fs::create_dir_all(&yaml_folder).unwrap();
        std::fs::write(
            yaml_folder.join("workspace.yaml"),
            format!("cwd: {}\n", file.display()),
        )
        .unwrap();
        let events =
            r#"{"type":"session.permissions_changed","data":{"allowAllPermissions":true}}"#;
        std::fs::write(yaml_folder.join("events.jsonl"), events).unwrap();
        let fakes = Fakes::default();
        fakes.client(CLAUDE, Client::Copilot);
        let ctx = ctx_at(&fakes, home.path(), &[("COPILOT_AGENT_SESSION_ID", SID)]);
        let caller = agent_caller(Client::Copilot, Some(&["copilot"]));
        assert_eq!(answer(assert_spawn_allowed(&ctx, &caller)), UNCONFIRMED);
        std::fs::write(
            yaml_folder.join("workspace.yaml"),
            format!("cwd: {}\n", work.path().display()),
        )
        .unwrap();
        assert_eq!(
            answer(assert_spawn_allowed(&ctx, &caller)),
            found(work.path())
        );
    }

    // ---- review pins: encodings, boundaries and which rows are checked

    const BOMS: [&str; 5] = ["utf-8", "utf-16le", "utf-16be", "utf-32le", "utf-32be"];

    // `text` behind the BOM that names its encoding.
    fn bom(name: &str, text: &str) -> Vec<u8> {
        let units = |big: bool| -> Vec<u8> {
            text.encode_utf16()
                .flat_map(|u| {
                    if big {
                        u.to_be_bytes()
                    } else {
                        u.to_le_bytes()
                    }
                })
                .collect()
        };
        let scalars = |big: bool| -> Vec<u8> {
            text.chars()
                .flat_map(|c| {
                    let n = c as u32;
                    if big {
                        n.to_be_bytes()
                    } else {
                        n.to_le_bytes()
                    }
                })
                .collect()
        };
        let (head, body): (&[u8], Vec<u8>) = match name {
            "utf-8" => (b"\xef\xbb\xbf", text.as_bytes().to_vec()),
            "utf-16le" => (b"\xff\xfe", units(false)),
            "utf-16be" => (b"\xfe\xff", units(true)),
            "utf-32le" => (b"\xff\xfe\x00\x00", scalars(false)),
            "utf-32be" => (b"\x00\x00\xfe\xff", scalars(true)),
            _ => unreachable!("{name}"),
        };
        [head, &body].concat()
    }

    // Bytes no decoder accepts: a bad UTF-8 byte, an odd UTF-16 length, a lone surrogate, a UTF-32 non-scalar.
    fn undecodable(text: &str) -> Vec<Vec<u8>> {
        let mut bad = text.as_bytes().to_vec();
        bad.extend(b"note \xff\n");
        let mut odd = bom("utf-16le", text);
        odd.push(b'x');
        let mut lone = bom("utf-16le", text);
        lone.extend([0x00, 0xd8]);
        let mut big = bom("utf-32le", text);
        big.extend(0x0011_0000u32.to_le_bytes());
        vec![bad, odd, lone, big]
    }

    #[test]
    fn policy_claude_transcript_bom_decoded_fail_closed() {
        let bypass = mode("bypassPermissions") + "\n";
        let back = format!("{}\n{}\n", mode("bypassPermissions"), mode("default"));
        let case = |bytes: Vec<u8>| Claude {
            bytes: Some(bytes),
            ..Claude::default()
        };
        for name in BOMS {
            assert_eq!(claude_policy(case(bom(name, &bypass))), "true", "{name}");
            assert_eq!(
                claude_policy(case(bom(name, &back))),
                "transcript: other mode",
                "{name}"
            );
        }
        // A transcript no decoder accepts shows no mode.
        for bytes in undecodable(&bypass) {
            assert_eq!(
                claude_policy(case(bytes.clone())),
                "transcript: no mode",
                "{bytes:?}"
            );
        }
    }

    #[test]
    fn policy_copilot_events_bom_decoded_fail_closed() {
        let allow = |on: bool| {
            format!(
                "{{\"type\":\"session.permissions_changed\",\"data\":{{\"allowAllPermissions\":{on}}}}}\n"
            )
        };
        for name in BOMS {
            // The event decides; the launch flag is never the fallback while an event exists.
            assert_eq!(
                copilot_policy_bytes(Some(&bom(name, &allow(false))), &["copilot", "--yolo"]),
                "false",
                "{name}"
            );
            assert_eq!(
                copilot_policy_bytes(Some(&bom(name, &allow(true))), &["copilot"]),
                "true",
                "{name}"
            );
        }
        // An events file that does not decode refuses; it never falls back to --yolo.
        for bytes in undecodable(&allow(false)) {
            assert_eq!(
                copilot_policy_bytes(Some(&bytes), &["copilot", "--yolo"]),
                "false",
                "{bytes:?}"
            );
        }
        let torn = b"{\"type\":\"session.\xffpermissions_changed\",\"data\":{\"allowAllPermissions\":false}}\n";
        assert_eq!(
            copilot_policy_bytes(Some(torn), &["copilot", "--yolo"]),
            "false"
        );
    }

    #[test]
    fn directory_copilot_workspace_yaml_bom_decoded() {
        let work = TempDir::new();
        let yaml = format!("id: x\ncwd: {}\n", work.path().display());
        for name in BOMS {
            assert_eq!(
                copilot_directory_bytes(Some(&bom(name, &yaml)), SID),
                found(work.path()),
                "{name}"
            );
        }
        for bytes in undecodable(&yaml) {
            assert_eq!(
                copilot_directory_bytes(Some(&bytes), SID),
                UNCONFIRMED,
                "{bytes:?}"
            );
        }
    }

    #[test]
    fn flags_caller_allowlist_on_every_client_row_to_the_pane() {
        // Reference :992-1007: rows from the agent to the pane, each client row against the CALLER's list. A Copilot
        // ancestor of a Claude agent may not carry --yolo, a Copilot flag.
        let above = argv_row(120, 100, 1200, Some(&["copilot", "--yolo"]));
        assert_eq!(
            flags(Client::Claude, Some(&["claude"]), Some(above)),
            "2 flags the flag --yolo is not allowed for a spawning agent"
        );
        // The pane's own row, the last of the chain, is checked when it is a client.
        let mut caller = agent_caller(Client::Claude, Some(&["claude"]));
        caller.chain[3].argv = Some(strings(&["claude", "--settings=x"]));
        let fakes = Fakes::default();
        fakes.client(CLAUDE, Client::Claude);
        fakes.client(100, Client::Claude);
        assert_eq!(
            answer(assert_spawn_allowed(&fakes.ctx(), &caller)),
            "2 flags the flag --settings is not allowed for a spawning agent"
        );
    }

    #[test]
    fn policy_claude_height_3_reads_mode_line() {
        // Three rows is a sized pane: the mode line is read and decides, not the transcript alone.
        let off = Claude {
            height: Some("3\n"),
            screen: Some("> \n".into()),
            ..Claude::default()
        };
        assert_eq!(claude_policy(off), "screen: no mode line");
        let on = Claude {
            height: Some("3\n"),
            ..Claude::default()
        };
        assert_eq!(claude_policy(on), "true");
    }

    // `badge` under a work row, then `below` non-blank rows, each after a blank one.
    fn below_badge(badge: &str, below: usize) -> String {
        let mut screen = format!("work\n{badge}\n");
        for n in 1..=below {
            screen.push_str(&format!("\nrow {n}\n"));
        }
        screen
    }

    const OTHER_BADGES: [&str; 5] = [
        "⏵⏵ accept edits on",
        "⏵⏵ auto mode on",
        "⏵⏵ don't ask on",
        "⏸ plan mode on",
        "⏸ manual mode on",
    ];

    // Rows that name bypass but are not a mode line: a prompt, a quote, text before the badge, no glyph.
    const NOT_MODE_LINES: [&str; 4] = [
        "❯ bypass permissions on",
        "\"bypass permissions on\" is quoted",
        "x ⏵⏵ bypass permissions on",
        "bypass permissions on",
    ];

    #[test]
    fn policy_claude_mode_line_in_last_8_rows() {
        // A spinner, a background task, a queued hint and a status line below the footer leave it in view; blank
        // rows between are not counted.
        for below in 4..=7 {
            let case = Claude {
                screen: Some(below_badge(BYPASS, below)),
                ..Claude::default()
            };
            assert_eq!(claude_policy(case), "true", "{below}");
        }
        let case = Claude {
            screen: Some(below_badge(BYPASS, 8)),
            ..Claude::default()
        };
        assert_eq!(claude_policy(case), "screen: no mode line");
    }

    #[test]
    fn policy_claude_wrapped_badge_joined_by_j() {
        // A 60-column pane wraps the footer and four status rows; only -J gives each row back whole.
        let rows: Vec<String> = [format!("{BYPASS} · {}", "x".repeat(80))]
            .into_iter()
            .chain(std::iter::repeat_n("s".repeat(100), 4))
            .collect();
        let (_home, fakes, ctx) = claude_setup(&Claude::default());
        fakes.mux_script("capture-pane", move |argv| {
            let joined = argv.iter().any(|a| a == "-J");
            let mut screen = String::new();
            for row in &rows {
                let chars: Vec<char> = row.chars().collect();
                let width = if joined { chars.len() } else { 60 };
                for part in chars.chunks(width) {
                    screen.extend(part);
                    screen.push('\n');
                }
            }
            Ok(crate::mux::Output {
                code: 0,
                stdout: screen,
                stderr: String::new(),
            })
        });
        assert_eq!(claude_answer(&ctx), "true");
        assert_eq!(
            fakes.mux_calls().last().unwrap(),
            &["-N", "capture-pane", "-p", "-J", "-t", "%1"]
        );
    }

    #[test]
    fn policy_claude_lowest_mode_line_decides() {
        // Any other mode below an older bypass badge refuses, its words in any case.
        for badge in OTHER_BADGES {
            for badge in [badge.to_string(), badge.to_uppercase()] {
                let case = Claude {
                    screen: Some(format!("{BYPASS}\nwork\n  {badge} (shift+tab to cycle)\n")),
                    ..Claude::default()
                };
                assert_eq!(claude_policy(case), "screen: other mode", "{badge}");
            }
        }
        // Text naming bypass is no mode line: alone it shows no mode, and below another mode it hides nothing.
        for row in NOT_MODE_LINES {
            let alone = Claude {
                screen: Some(format!("work\n{row}\n")),
                ..Claude::default()
            };
            assert_eq!(claude_policy(alone), "screen: no mode line", "{row}");
            let below = Claude {
                screen: Some(format!("⏵⏵ accept edits on\n{row}\n")),
                ..Claude::default()
            };
            assert_eq!(claude_policy(below), "screen: other mode", "{row}");
        }
        // Anything may follow a badge.
        let case = Claude {
            screen: Some(format!("{BYPASS}\n")),
            ..Claude::default()
        };
        assert_eq!(claude_policy(case), "true");
    }

    // capture-pane answers `screens` in turn, the last repeating.
    fn screens_in_turn(fakes: &Fakes, screens: &[String]) {
        let screens = screens.to_vec();
        let next = std::cell::Cell::new(0);
        fakes.mux_script("capture-pane", move |_| {
            let n = next.get();
            next.set(n + 1);
            Ok(crate::mux::Output {
                code: 0,
                stdout: screens[n.min(screens.len() - 1)].clone(),
                stderr: String::new(),
            })
        });
    }

    fn captures(fakes: &Fakes) -> usize {
        fakes
            .mux_calls()
            .iter()
            .filter(|c| c[1] == "capture-pane")
            .count()
    }

    #[test]
    fn policy_claude_retries_only_a_missing_mode() {
        let (work, badge) = ("work\n".to_string(), format!("{BYPASS}\n"));
        // No mode row on any read: 3 captures, 250 ms apart.
        let (_home, fakes, ctx) = claude_setup(&Claude::default());
        screens_in_turn(&fakes, std::slice::from_ref(&work));
        assert_eq!(claude_answer(&ctx), "screen: no mode line");
        assert_eq!((captures(&fakes), fakes.slept()), (3, vec![250, 250]));
        // A footer drawn by the second read passes.
        let (_home, fakes, ctx) = claude_setup(&Claude::default());
        screens_in_turn(&fakes, &[work.clone(), badge.clone()]);
        assert_eq!(claude_answer(&ctx), "true");
        assert_eq!((captures(&fakes), fakes.slept()), (2, vec![250]));
        // A transcript with no mode line on any read: 3 reads, 2 sleeps, and the pane is never asked.
        let empty = Claude {
            transcript: vec![],
            ..Claude::default()
        };
        let (_home, fakes, ctx) = claude_setup(&empty);
        assert_eq!(claude_answer(&ctx), "transcript: no mode");
        assert_eq!(fakes.slept(), [250, 250]);
        assert!(fakes.mux_calls().is_empty());
        // A mode line written after the first or the second sleep is read, and the screen decides next.
        for after in [1, 2] {
            let (home, fakes, ctx) = claude_setup(&empty);
            let transcript = home.path().join(format!(".claude/projects/p0/{SID}.jsonl"));
            fakes.sleep_probe(move |count| {
                if count == after {
                    std::fs::write(&transcript, mode("bypassPermissions") + "\n").unwrap();
                }
            });
            assert_eq!(claude_answer(&ctx), "true", "{after}");
            assert_eq!(fakes.slept(), vec![250; after], "{after}");
            assert_eq!(captures(&fakes), 1, "{after}");
        }
        // Every other outcome is final: one read, no sleep.
        for (case, reason) in [
            (
                Claude {
                    sid: "",
                    ..Claude::default()
                },
                "transcript: no session id",
            ),
            (
                Claude {
                    folders: 0,
                    ..Claude::default()
                },
                "transcript: not found",
            ),
            (
                Claude {
                    transcript: vec![mode("default")],
                    ..Claude::default()
                },
                "transcript: other mode",
            ),
            (
                Claude {
                    screen: None,
                    ..Claude::default()
                },
                "screen: unreadable",
            ),
            (
                Claude {
                    screen: Some("⏸ plan mode on\n".into()),
                    ..Claude::default()
                },
                "screen: other mode",
            ),
        ] {
            // A screen reason read the pane once; a transcript reason never reached it.
            let screen = reason.starts_with("screen");
            let (_home, fakes, ctx) = claude_setup(&case);
            assert_eq!(claude_answer(&ctx), reason);
            assert_eq!(fakes.slept(), Vec::<u64>::new(), "{reason}");
            assert_eq!(captures(&fakes), usize::from(screen), "{reason}");
        }
    }

    #[test]
    fn policy_refusal_names_its_reason() {
        let cases = [
            (
                Claude {
                    sid: "",
                    ..Claude::default()
                },
                "transcript: no session id",
            ),
            (
                Claude {
                    folders: 0,
                    ..Claude::default()
                },
                "transcript: not found",
            ),
            (
                Claude {
                    transcript: vec![],
                    ..Claude::default()
                },
                "transcript: no mode",
            ),
            (
                Claude {
                    transcript: vec![mode("plan")],
                    ..Claude::default()
                },
                "transcript: other mode",
            ),
            (
                Claude {
                    screen: None,
                    ..Claude::default()
                },
                "screen: unreadable",
            ),
            (
                Claude {
                    screen: Some("work\n".into()),
                    ..Claude::default()
                },
                "screen: no mode line",
            ),
            (
                Claude {
                    screen: Some("⏸ plan mode on\n".into()),
                    ..Claude::default()
                },
                "screen: other mode",
            ),
        ];
        for (case, reason) in cases {
            let (_home, fakes, ctx) = claude_setup(&case);
            fakes.client(CLAUDE, Client::Claude);
            let caller = agent_caller(Client::Claude, Some(&["claude"]));
            let refused = assert_spawn_allowed(&ctx, &caller).unwrap_err();
            assert_eq!(
                (
                    refused.code,
                    refused.check,
                    refused.message.as_str(),
                    refused.reason
                ),
                (
                    2,
                    "policy",
                    format!("{POLICY} ({reason})").as_str(),
                    Some(reason)
                ),
            );
        }
        // Copilot's refusal is unchanged: no reason.
        let fakes = Fakes::default();
        fakes.client(CLAUDE, Client::Copilot);
        let copilot = agent_caller(Client::Copilot, Some(&["copilot"]));
        let refused = assert_spawn_allowed(&fakes.ctx(), &copilot).unwrap_err();
        assert_eq!(
            (refused.check, refused.message.as_str(), refused.reason),
            ("policy", POLICY, None)
        );
    }

    #[test]
    fn policy_copilot_launch_flag_through_spawn_gate() {
        let (home, work) = (TempDir::new(), TempDir::new());
        let folder = home.path().join(".copilot/session-state").join(SID);
        std::fs::create_dir_all(&folder).unwrap();
        let yaml = format!("cwd: {}\n", work.path().display());
        std::fs::write(folder.join("workspace.yaml"), yaml).unwrap();
        // No permissions event: the agent's own launch flags decide.
        std::fs::write(
            folder.join("events.jsonl"),
            "{\"type\":\"session.start\"}\n",
        )
        .unwrap();
        let run = |argv: &[&str]| {
            let fakes = Fakes::default();
            fakes.client(CLAUDE, Client::Copilot);
            let ctx = ctx_at(&fakes, home.path(), &[("COPILOT_AGENT_SESSION_ID", SID)]);
            answer(assert_spawn_allowed(
                &ctx,
                &agent_caller(Client::Copilot, Some(argv)),
            ))
        };
        for flag in ["--allow-all", "--yolo"] {
            assert_eq!(run(&["copilot", flag]), found(work.path()), "{flag}");
        }
        assert_eq!(run(&["copilot"]), NEXT);
    }

    #[test]
    fn directory_session_cwd_case_differs_refuses() {
        let (home, work) = (TempDir::new(), TempDir::new());
        let cwd = work.path().to_str().unwrap();
        let shouted = cwd.to_uppercase();
        assert_ne!(shouted, cwd);
        let session = serde_json::json!({"cwd": shouted}).to_string();
        std::fs::write(sessions(home.path()).join("150.json"), session).unwrap();
        assert_eq!(
            unix_directory(home.path(), &[Some(1500), Some(1500)], Some(cwd)).0,
            UNCONFIRMED
        );
    }

    #[test]
    fn directory_windows_session_id_case_sensitive() {
        // The reference compares with -ceq; a session id with letters shows the case.
        let (home, work) = (TempDir::new(), TempDir::new());
        let sid = "abcdef12-2222-4333-8444-555555555555";
        let cwd = work.path().to_str().unwrap();
        let caller = agent_caller(Client::Claude, Some(&["claude"]));
        let run = |file_sid: &str| {
            let session =
                serde_json::json!({"sessionId": file_sid, "cwd": cwd, "procStart": "1500"});
            std::fs::write(sessions(home.path()).join("150.json"), session.to_string()).unwrap();
            let fakes = Fakes::default();
            let ctx = ctx_at(&fakes, home.path(), &[("CLAUDE_CODE_SESSION_ID", sid)]);
            answer(launch_directory(&ctx, &caller, OsKind::Windows))
        };
        assert_eq!(run(sid), found(work.path()));
        assert_eq!(run(&sid.to_uppercase()), UNCONFIRMED);
    }
}
