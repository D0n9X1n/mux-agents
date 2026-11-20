//! The pane launcher, `mux-agent launcher <spec.json>` (mux-agent-launcher.ps1, MuxAgents.ps1:1593-1671): waits
//! for the launch commit, runs the client once in this pane, records its exit code; only the legacy form posts an `exited` notice.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::agents::gens::{self, Decision};
use crate::agents::spawn::{CMDLINE_MAX, client_named};
use crate::ctx::Ctx;
use crate::error::{Refusal, Res};
use crate::parse::cmdline_length;
use crate::state::evidence::{self, Evidence, Published};
use crate::state::root::{HOST, OsKind};
use crate::util::utc_o;

/// Taken out of the client's environment: the parent's client identity (MuxAgents.ps1:1595-1596), and the
/// multiplexer of whoever started the launch. A member runs in its own hc terminal and finds its multiplexer from its
/// record, never from `TMUX` or `RMUX`; a client that kept them (an `up` run from a tmux pane) would read as a person
/// in a pane, so its session hook would skip the readiness handshake and the member would never become ready.
pub const SCRUB: [&str; 15] = [
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "COPILOT_AGENT_SESSION_ID",
    "COPILOT_CLI",
    "TMUX",
    "TMUX_PANE",
    "RMUX",
];

const COMMIT_WAIT: Duration = Duration::from_secs(30);
const COMMIT_POLL: Duration = Duration::from_millis(250);
const OUTSIDE: &str = "mux-agent: the launch spec is outside the state folder";
const LINK: &str = "mux-agent: the launch spec path has a junction or link";
const GEN_PATH: &str = "mux-agent: the launch spec path names no generation of a member";
const FOREIGN: &str = "mux-agent: the launch spec is not this member's launch";
/// PM call 7, all-calls 14: a folder-level spec is a pane an older build started and has not yet claimed; it names
/// the remedy that works (`stop` ends a 0.2.x pane; a pre-lifecycle member never resumes, G10d).
const OLD_SPEC: &str = "mux-agent: this pane was started by an older mux-agent and cannot launch; \
                        run `mux-agent stop <name>` to end it, then start a new agent";

/// The create-once publisher (`evidence::publish` in production): the seam for every publication of the generation
/// path, so each publication failure is testable.
pub(crate) type Publish<'a> = &'a mut dyn FnMut(&Path, &Value) -> Res<Published>;

/// What a spec path names (launch-1 G7).
#[derive(Debug, PartialEq, Eq)]
enum Spec {
    /// No `gens` below the state root: a pane an older build started; refused with `OLD_SPEC`.
    Folder,
    /// `<root>/<key>/agents/<id>/gens/<n>/spec.json`: generation n of the member folder.
    Gen(PathBuf, u64),
    /// Names `gens` in any other way: refused, never run as a folder.
    Malformed,
}

/// `spec` is under `root` (checked by `run_seamed` first; otherwise the path is Malformed). A spec path with no
/// component equal to `gens` (ignoring ASCII case) below the root is a Folder. One that does name `gens` is a Gen only
/// when it is exactly `<key>/agents/<id>/gens/<n>/spec.json` and `gens::plain_key`, `gens::plain_id` and
/// `gens::plain_gen` accept its three names (redesign 3); the member is then joined from the root and those checked
/// names only.
fn classify_spec(root: &Path, spec: &Path) -> Spec {
    if !lexically_under(root, spec) {
        return Spec::Malformed;
    }
    let below: Vec<String> = spec
        .components()
        .skip(root.components().count())
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if !below.iter().any(|c| c.eq_ignore_ascii_case("gens")) {
        return Spec::Folder;
    }
    match below
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [key, "agents", id, "gens", n, "spec.json"] => match gens::plain_gen(n) {
            Some(n) if gens::plain_key(key) && gens::plain_id(id) => {
                Spec::Gen(root.join(key).join("agents").join(id), n)
            }
            _ => Spec::Malformed,
        },
        _ => Spec::Malformed,
    }
}

/// Starts the client attached to this pane: its path, its arguments, the variables to take out, and `started`, told the
/// client's pid from the child handle before the wait (D1); its exit code.
pub(crate) type Start<'a> =
    &'a mut dyn FnMut(&Path, &[String], &[&str], &mut dyn FnMut(u32)) -> io::Result<i32>;

/// The pane's process: exit 2 for a spec it will not read, else the client's exit code (1 when it never ran).
pub fn launcher(ctx: &Ctx, spec: &str) -> i32 {
    let mut err = |line: &str| {
        use std::io::Write;
        let _ = writeln!(io::stderr(), "{line}");
    };
    run(ctx, spec, &mut err, &mut |path, args, remove, started| {
        let overrides = launch_overrides(ctx, Path::new(spec))?;
        crate::spawn::attached_env(
            path,
            args,
            remove,
            &overrides
                .iter()
                .map(|(k, v)| (*k, v.as_str()))
                .collect::<Vec<_>>(),
            started,
        )
    })
}

/// The client's environment overrides: the launch names its state root and PATH; a generation spec also names its
/// generation (launch-1 G7, `MUX_AGENTS_GEN`).
fn launch_overrides(ctx: &Ctx, spec: &Path) -> io::Result<Vec<(&'static str, String)>> {
    let root = ctx
        .state_root
        .to_str()
        .ok_or_else(|| io::Error::other("state root is not UTF-8"))?;
    let mut all = vec![
        ("MUX_AGENTS_STATE", root.to_string()),
        ("PATH", ctx.env.get("PATH").unwrap_or("").to_string()),
    ];
    if let Spec::Gen(_, n) = classify_spec(&ctx.state_root, spec) {
        all.push(("MUX_AGENTS_GEN", n.to_string()));
    }
    Ok(all)
}

pub(crate) fn run(ctx: &Ctx, spec: &str, err: &mut dyn FnMut(&str), start: Start) -> i32 {
    run_with(ctx, spec, err, start, &mut |d: &Path, above_root: bool| {
        if above_root {
            crate::state::fs::sync_ancestor(d)
        } else {
            crate::state::fs::sync_dir(d)
        }
    })
}

/// Flushes the claim's folder and every folder above it, up to the filesystem root, before the client may run, so no
/// folder entry on the claim's path can vanish in a crash or power loss, whoever made it and whether or not its maker
/// finished. Any failed flush stops the launch. Above the state root a folder this user cannot add an entry to
/// (`C:\Users`, `/home`) is skipped by `sync`: nothing on the path can have been made in it by this user.
fn flush_claim(
    root: &Path,
    folder: &Path,
    sync: &mut dyn FnMut(&Path, bool) -> io::Result<()>,
) -> io::Result<()> {
    let gens = folder.join("gens");
    let depth = root.components().count();
    let mut dirs = gens.ancestors();
    for dir in dirs.by_ref() {
        // Every folder below the root is lexically under it; the first one at the root's depth is the root itself.
        let at_root = dir.components().count() == depth;
        if !at_root && !lexically_under(root, dir) {
            return Err(io::Error::other("the claim is not under the state root"));
        }
        sync(dir, false)?;
        if at_root {
            break;
        }
    }
    for dir in dirs.filter(|d| !d.as_os_str().is_empty()) {
        sync(dir, true)?;
    }
    Ok(())
}

fn run_with(
    ctx: &Ctx,
    spec: &str,
    err: &mut dyn FnMut(&str),
    start: Start,
    sync: &mut dyn FnMut(&Path, bool) -> io::Result<()>,
) -> i32 {
    run_seamed(ctx, spec, err, start, sync, &mut |at: &Path, v: &Value| {
        evidence::publish(at, v)
    })
}

fn run_seamed(
    ctx: &Ctx,
    spec: &str,
    err: &mut dyn FnMut(&str),
    start: Start,
    sync: &mut dyn FnMut(&Path, bool) -> io::Result<()>,
    publish: Publish,
) -> i32 {
    let given = Path::new(spec);
    // Only a spec.json inside the state root, its path already canonical (no link on the way), is read.
    if !given.is_absolute()
        || !lexically_under(&ctx.state_root, given)
        || given.file_name().and_then(|n| n.to_str()) != Some("spec.json")
    {
        err(OUTSIDE);
        return 2;
    }
    if !std::fs::canonicalize(given).is_ok_and(|real| same_path(&real, spec, HOST)) {
        err(LINK);
        return 2;
    }
    // On a canonical path: a link cannot make a Gen of a folder-level spec, or the reverse.
    // launch-1 G7: only gens/<n>/spec.json launches; anything else is refused before anything is claimed.
    match classify_spec(&ctx.state_root, given) {
        Spec::Gen(member, n) => run_gen(ctx, &member, n, err, start, sync, publish),
        Spec::Folder => {
            err(OLD_SPEC);
            2
        }
        Spec::Malformed => {
            err(GEN_PATH);
            2
        }
    }
}

/// The client flags the launch spec records, the exact ones its spawn chose (a person's `up` records its own).
/// None when they are missing or not all text: a spec never falls back to flags it does not name.
fn spec_flags(launch: &Value) -> Option<Vec<String>> {
    launch
        .get("flags")?
        .as_array()?
        .iter()
        .map(|f| f.as_str().map(String::from))
        .collect()
}

/// `path` below `root` by whole components, with no `.` or `..` (Windows: ignoring case).
fn lexically_under(root: &Path, path: &Path) -> bool {
    let plain = |c: &Component| !matches!(c, Component::CurDir | Component::ParentDir);
    let text = |c: Component| {
        let text = c.as_os_str().to_string_lossy().into_owned();
        if cfg!(windows) {
            text.to_lowercase()
        } else {
            text
        }
    };
    if !path.components().all(|c| plain(&c)) || !root.components().all(|c| plain(&c)) {
        return false;
    }
    let (mut root, mut path) = (root.components().map(text), path.components().map(text));
    for part in root.by_ref() {
        if path.next().as_ref() != Some(&part) {
            return false;
        }
    }
    path.next().is_some()
}

/// Whether `given` is already the canonical path `resolved`, so no link or junction lies on the way. Windows:
/// canonicalize answers `\\?\C:\...` whatever the caller typed, and NTFS names ignore case, so both sides drop a
/// drive's `\\?\` and compare ignoring case; an 8.3 short name still differs and refuses. Unix: exact bytes.
fn same_path(resolved: &Path, given: &str, os: OsKind) -> bool {
    let Some(resolved) = resolved.to_str() else {
        return false;
    };
    match os {
        OsKind::Unix => resolved == given,
        OsKind::Windows => {
            // A spec on a share never runs: either side UNC refuses before any comparison.
            if unc(resolved) || unc(given) {
                return false;
            }
            let plain = |p: &str| p.strip_prefix(r"\\?\").unwrap_or(p).to_lowercase();
            plain(resolved) == plain(given)
        }
    }
}

/// A Windows path that starts with two separators (`\\srv\share`, `\\?\UNC\...`, `//srv/share`, `\\.\...`) other
/// than a drive's `\\?\C:`.
fn unc(path: &str) -> bool {
    let b = path.as_bytes();
    let sep = |i: usize| matches!(b.get(i), Some(b'\\' | b'/'));
    if !(sep(0) && sep(1)) {
        return false;
    }
    let drive =
        b.len() >= 6 && b[2] == b'?' && sep(3) && b[4].is_ascii_alphabetic() && b[5] == b':';
    !drive
}

/// The client a spec names, found on PATH, with its full argument list.
struct ClientLaunch {
    executable: PathBuf,
    args: Vec<String>,
}

/// The one lookup of a spec's client: its recorded flags then its args, the executable on PATH, the command line
/// within `CMDLINE_MAX`. A reason it cannot start goes to `err` and answers `Ok(None)`; no `args` array refuses.
fn client_command(
    ctx: &Ctx,
    launch: &Value,
    err: &mut dyn FnMut(&str),
) -> Res<Option<ClientLaunch>> {
    let client = launch
        .get("client")
        .and_then(Value::as_str)
        .and_then(client_named);
    let (client, mut args) = match (client, spec_flags(launch)) {
        (None, _) => {
            err("mux-agent: the launch spec names no known client");
            return Ok(None);
        }
        (Some(_), None) => {
            err("mux-agent: the launch spec names no client flags");
            return Ok(None);
        }
        (Some(client), Some(args)) => (client, args),
    };
    for arg in launch
        .get("args")
        .and_then(Value::as_array)
        .ok_or_else(|| Refusal::internal("the launch spec has no args"))?
    {
        args.push(crate::agents::text(Some(arg)));
    }
    let name = crate::agents::spawn::client_name(client);
    let image = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let Some(executable) = ctx
        .env
        .get("PATH")
        .and_then(|path| crate::mux::find_on_path(&image, path))
    else {
        err(&format!("mux-agent: {image} is not on PATH"));
        return Ok(None);
    };
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let length = executable
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .count()
        + 3
        + cmdline_length(&argv);
    if length > CMDLINE_MAX {
        err(&format!(
            "mux-agent: the client command line is too long ({length} characters)"
        ));
        return Ok(None);
    }
    Ok(Some(ClientLaunch { executable, args }))
}

/// launch-1 G1.2, G12: the spec of generation `n`, only when it is this member's launch n. The record must be a
/// lifecycle record (`lifecycle: 1`) whose `id` is the folder's name, and it must name n either as `pending.gen` or as
/// the active `generation`. That entry supplies the expected `hcName` and `sessionId`. Only an absent `hcName` at
/// generation 0 derives m and the first 8 of the id (G2); `null` or any non-text value is refused. The name must have
/// the hc shape. `launch.json` must name that hcName and session, and `spec.json` this id, `generation` n as text, and
/// that session. Anything else, a torn, missing or linked file included, is None, and nothing has been created.
pub(crate) fn own_spec(member: &Path, n: u64) -> Option<Value> {
    let id = member.file_name()?.to_str()?;
    let Evidence::Present(record) = gens::record(member) else {
        return None;
    };
    if !gens::lifecycle(&record) || record.get("id")?.as_str()? != id {
        return None;
    }
    let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    let (hc_name, session) = if gens::pending(&record) == Some(n) {
        let pending = record.get("pending")?;
        (text(pending, "hcName")?, text(pending, "sessionId")?)
    } else if gens::active(&record) == Some(n) {
        // A launcher can read the record only after promotion: the active generation is still its own.
        let name = match record.get("hcName") {
            Some(Value::String(s)) => s.clone(),
            None if n == 0 => format!("m{}", id.get(..8)?),
            _ => return None,
        };
        (name, text(&record, "sessionId")?)
    } else {
        return None;
    };
    if !crate::agents::core_identity::is_hc_name(&hc_name) {
        return None;
    }
    let Evidence::Present(launch) = gens::read(member, n, "launch") else {
        return None;
    };
    let Evidence::Present(spec) = gens::read(member, n, "spec") else {
        return None;
    };
    let owned = text(&launch, "hcName").as_deref() == Some(hc_name.as_str())
        && text(&launch, "sessionId").as_deref() == Some(session.as_str())
        && text(&spec, "id").as_deref() == Some(id)
        && text(&spec, "generation") == Some(n.to_string())
        && text(&spec, "sessionId").as_deref() == Some(session.as_str());
    owned.then_some(spec)
}

/// launch-1 G4, G7: the launcher of generation `n` of `member`. Order: ownership (`own_spec`, nothing created on
/// refusal, exit 1 FOREIGN); claim `gens/<n>.claim` = `run` (`Exists`, whatever is there: exit 0, nothing else
/// touched); flush the claim's folders; `launcher.json` once; the commit wait; the client; `exit.json` once. Every
/// failure after the won claim leaves `exit.json` with its reason, and exit 1.
fn run_gen(
    ctx: &Ctx,
    member: &Path,
    n: u64,
    err: &mut dyn FnMut(&str),
    start: Start,
    sync: &mut dyn FnMut(&Path, bool) -> io::Result<()>,
    publish: Publish,
) -> i32 {
    use crate::state::evidence::crash_point;
    let Some(spec) = own_spec(member, n) else {
        err(FOREIGN);
        return 1;
    };
    match publish(&gens::claim_path(member, n), &json!("run")) {
        Ok(Published::Published) => {}
        Ok(Published::Exists) => return 0,
        Err(refusal) => {
            err(&format!(
                "mux-agent: the launcher failed: {}",
                refusal.message
            ));
            return 1;
        }
    }
    crash_point("claim");
    // Final audit (PM): a won claim means no launcher ran this generation. Earlier launcher, client or exit evidence
    // (anything not Absent by the read rule) is corrupt: nothing starts and nothing more is published. Checked right
    // after the claim is won, before the claim is flushed and before launcher.json is published.
    for file in ["launcher", "client", "exit"] {
        if gens::read(member, n, file) != Evidence::Absent {
            err(&format!(
                "mux-agent: the launcher failed: gens/{n}/{file}.json was already there"
            ));
            return 1;
        }
    }
    if let Err(e) = flush_claim(&ctx.state_root, member, sync) {
        err(&format!(
            "mux-agent: the launcher failed: the generation claim could not be flushed to disk: {e}"
        ));
        gen_exit(ctx, member, n, &mut *publish, Some(1), "claim not flushed");
        return 1;
    }
    let own = json!({
        "pid": ctx.pid,
        "start": ctx.os.open(ctx.pid).map(|o| o.start),
        "startedAt": utc_o(ctx.clock.now()),
    });
    if !matches!(
        publish(&gens::path(member, n, "launcher"), &own),
        Ok(Published::Published)
    ) {
        err("mux-agent: the launcher failed: launcher.json could not be published");
        gen_exit(
            ctx,
            member,
            n,
            &mut *publish,
            Some(1),
            "launcher not published",
        );
        return 1;
    }
    crash_point("launcher");
    let stop = match wait_gen_commit(ctx, member, n, &mut *publish) {
        // Call 34: a Run decision acts only once the commit is durable; else it is an uncommitted launch.
        GenCommit::Decided(Decision::Run) => match commit_durable(member, n) {
            Ok(()) => None,
            Err(_) => Some("commit not flushed"),
        },
        GenCommit::Decided(Decision::Aborted) => Some("aborted"),
        GenCommit::Decided(Decision::Invalid) => Some("commit invalid"),
        GenCommit::GaveUp => Some("gave up"),
        GenCommit::Unreadable => Some("commit unreadable"),
        GenCommit::Unpublished => Some("give-up not published"),
    };
    if let Some(reason) = stop {
        gen_exit(ctx, member, n, &mut *publish, Some(1), reason);
        return 1;
    }
    let client = client_command(ctx, &spec, err).unwrap_or_else(|refusal| {
        err(&format!(
            "mux-agent: the launcher failed: {}",
            refusal.message
        ));
        None
    });
    let Some(client) = client else {
        gen_exit(ctx, member, n, &mut *publish, Some(1), "client not started");
        return 1;
    };
    let (mut conflicted, mut told_pid) = (false, false);
    let started = start(&client.executable, &client.args, &SCRUB, &mut |pid| {
        #[cfg(feature = "live")]
        live_client_barrier(ctx, member, n);
        told_pid = true;
        match publish_client(ctx, member, n, pid, &mut *publish) {
            ClientEvidence::Recorded => {}
            ClientEvidence::Conflict => {
                conflicted = true;
                err(
                    "mux-agent: client.json names another process: client-conflict; the client runs on",
                );
                if !mark_conflict(ctx, member, n, &mut *publish) {
                    err(
                        "mux-agent: the client-conflict could not be marked; the client runs on, unproven",
                    );
                }
            }
            ClientEvidence::Unrecorded => {
                err("mux-agent: client.json could not be published; the client runs on, unproven");
            }
        }
        crash_point("client");
    });
    let (code, reason) = match started {
        Ok(code) if conflicted => (code, "client-conflict"),
        Ok(code) => (code, "exited"),
        Err(_) if conflicted => (1, "client-conflict"),
        Err(_) if told_pid => (1, "client wait failed"),
        Err(_) => (1, "client not started"),
    };
    gen_exit(ctx, member, n, &mut *publish, Some(code), reason);
    crash_point("exit");
    code
}

/// What the commit wait of a generation ends with.
#[derive(Debug, PartialEq, Eq)]
enum GenCommit {
    /// A whole commit, decided by `gens::decide`.
    Decided(Decision),
    /// Absent at 30 s; this launcher published its own `{aborted}`.
    GaveUp,
    /// Unreadable at 30 s: nothing published, never overwritten (fix cycle 2).
    Unreadable,
    /// Absent at 30 s, the give-up publication failed, and the reread found no decision: nothing created.
    Unpublished,
}

/// Call 34: flushes the folders of generation n's commit exactly as `gens::publish_once` does, innermost first:
/// `gens/<n>`, `gens`, the member folder, `agents`. A publisher that crashed or failed its flush after the commit
/// became visible leaves it unflushed; no client runs on it until these flushes succeed.
fn commit_durable(member: &Path, n: u64) -> io::Result<()> {
    let generation = gens::dir(member, n);
    let all = member.join("gens");
    let agents = member
        .parent()
        .ok_or_else(|| io::Error::other("the member has no agents folder"))?;
    for dir in [generation.as_path(), all.as_path(), member, agents] {
        crate::state::fs::sync_dir(dir)?;
    }
    Ok(())
}

/// launch-1 G4, state-0 S6: polls `gens::read(commit)` (the walk of Task 1a step 0) every 250 ms. A whole commit
/// decides at once. `Absent` and `Unreadable` keep waiting. At 30 s only `Absent` publishes the launcher's own
/// `{aborted}`, create-once; `Unreadable` publishes nothing and ends `Unreadable` (fix cycle 2). A give-up that lost
/// the race (`Exists`) or failed (`Err`) rereads: a whole commit acts as decided; else `Unreadable` after `Exists`,
/// `Unpublished` after `Err`.
fn wait_gen_commit(ctx: &Ctx, member: &Path, n: u64, publish: Publish) -> GenCommit {
    let deadline = ctx.clock.now() + COMMIT_WAIT;
    loop {
        let seen = gens::read(member, n, "commit");
        if let Evidence::Present(c) = &seen {
            return GenCommit::Decided(gens::decide(c));
        }
        if ctx.clock.now() >= deadline {
            if seen == Evidence::Unreadable {
                return GenCommit::Unreadable;
            }
            let give_up = json!({"aborted": true, "at": utc_o(ctx.clock.now())});
            let failed = match publish(&gens::path(member, n, "commit"), &give_up) {
                Ok(Published::Published) => return GenCommit::GaveUp,
                Ok(Published::Exists) => false,
                Err(_) => true,
            };
            return match gens::read(member, n, "commit") {
                Evidence::Present(c) => GenCommit::Decided(gens::decide(&c)),
                _ if failed => GenCommit::Unpublished,
                _ => GenCommit::Unreadable,
            };
        }
        ctx.clock.sleep(COMMIT_POLL);
    }
}

/// launch-1 G4: generation n's `exit.json` = `{exitCode, reason, exitedAt}`, create-once through `publish`. An exit
/// already there is kept (`Exists`): the first reason stands, `client-conflict` included (Task 1c). A failed
/// publication is not retried: the launcher exits either way, and a generation without an exit stays reserved
/// (settle-1b). `code` is None only for `client-conflict`, published while the client still runs.
fn gen_exit(
    ctx: &Ctx,
    member: &Path,
    n: u64,
    publish: Publish,
    code: Option<i32>,
    reason: &str,
) -> Option<Published> {
    let exit = json!({"exitCode": code, "reason": reason, "exitedAt": utc_o(ctx.clock.now())});
    publish(&gens::path(member, n, "exit"), &exit).ok()
}

/// proof-0b K1: a failed publication of `client.json`, or of the `client-conflict` mark, is retried once.
const CLIENT_ATTEMPTS: usize = 2;

/// What publishing the started client ends with.
#[derive(Debug, PartialEq, Eq)]
enum ClientEvidence {
    /// `client.json` names this client: published now, or found naming this pid and start.
    Recorded,
    /// `client.json` names another process, or this pid with another start, or is torn.
    Conflict,
    /// Every attempt failed: no `client.json`; the client runs on, unproven and reserved.
    Unrecorded,
}

/// The identity K1 compares: `pid` and `start`, never other fields (K1 allows `loader`).
fn pid_start(v: &Value) -> (Option<&Value>, Option<&Value>) {
    (v.get("pid"), v.get("start"))
}

/// proof-0b K1: publishes `gens/<n>/client.json` = `{pid, start}` of the client just started, create-once through
/// `publish`, at most `CLIENT_ATTEMPTS` times. `Exists` is read back through `gens::read` (the walk): the same pid and
/// start is `Recorded`; anything else is `Conflict`.
fn publish_client(ctx: &Ctx, member: &Path, n: u64, pid: u32, publish: Publish) -> ClientEvidence {
    let ours = json!({"pid": pid, "start": ctx.os.open(pid).map(|o| o.start)});
    for _ in 0..CLIENT_ATTEMPTS {
        match publish(&gens::path(member, n, "client"), &ours) {
            Ok(Published::Published) => return ClientEvidence::Recorded,
            Ok(Published::Exists) => {
                let same = matches!(gens::read(member, n, "client"),
                    Evidence::Present(c) if pid_start(&c) == pid_start(&ours));
                return if same {
                    ClientEvidence::Recorded
                } else {
                    ClientEvidence::Conflict
                };
            }
            Err(_) => {}
        }
    }
    ClientEvidence::Unrecorded
}

/// Live tests only (report-2b, PM call 18): with `<state root>/test-client-barrier`, hold client.json until generation
/// `n`'s report.json exists (at most 600 polls of 100 ms), then mark the order in `test-barrier-<this pid>`:
/// `report-first` (client.json absent as the report appeared), `client-first`, or `timeout`. Not in release builds.
#[cfg(feature = "live")]
fn live_client_barrier(ctx: &Ctx, member: &Path, n: u64) {
    if !ctx.state_root.join("test-client-barrier").exists() {
        return;
    }
    let mut mark = "timeout";
    for poll in 0..=600 {
        if matches!(gens::read(member, n, "report"), Evidence::Present(_)) {
            // r2b call 33: report-first only when client.json was still absent as the report appeared.
            mark = if gens::path(member, n, "client").exists() {
                "client-first"
            } else {
                "report-first"
            };
            break;
        }
        if poll < 600 {
            ctx.clock.sleep(Duration::from_millis(100));
        }
    }
    let _ = std::fs::write(
        ctx.state_root.join(format!("test-barrier-{}", ctx.pid)),
        mark,
    );
}

/// Fix cycle 2: the `client-conflict` mark, `exitCode` null while the client runs, retried once. True only when this
/// launcher published it; an `exit.json` already there (any reason) is not a mark.
fn mark_conflict(ctx: &Ctx, member: &Path, n: u64, publish: Publish) -> bool {
    for _ in 0..CLIENT_ATTEMPTS {
        match gen_exit(ctx, member, n, &mut *publish, None, "client-conflict") {
            Some(Published::Published) => return true,
            Some(Published::Exists) => return false,
            None => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::fixture::{CALLER, ID, World, write};
    use crate::agents::gens;
    use crate::ctx::Env;
    use crate::parse::cmdline_length;
    use crate::state::evidence::{self, Evidence};
    use crate::state::fs::read_json;
    use serde_json::{Value, json};
    use std::path::PathBuf;

    const OUTSIDE: &str = "mux-agent: the launch spec is outside the state folder";
    const LINK: &str = "mux-agent: the launch spec path has a junction or link";

    struct Launch {
        w: World,
        folder: PathBuf,
        spec: PathBuf,
        bin: PathBuf,
    }

    struct Ran {
        code: i32,
        err: Vec<String>,
        started: Vec<(PathBuf, Vec<String>, Vec<String>)>,
    }

    fn exe_name(client: &str) -> String {
        if cfg!(windows) {
            format!("{client}.exe")
        } else {
            client.to_string()
        }
    }

    // A committed agent of session $3 whose spec names `client` with `args`, and a PATH holding both clients.
    fn setup(client: &str, args: Value) -> Launch {
        let mut w = World::human("");
        // A real root comes from %LOCALAPPDATA% as `C:\...`; the temp dir's `\\?\C:\...` would fold `..` away on join,
        // so the dotted spec below would name the real one.
        #[cfg(windows)]
        if let Some(plain) = w
            .ctx
            .state_root
            .to_str()
            .and_then(|r| r.strip_prefix(r"\\?\"))
        {
            w.ctx.state_root = PathBuf::from(plain);
        }
        let folder = w.agent_record(ID, json!({}));
        let state = w.state();
        write(
            &state.join("server.json"),
            json!({"socket": "/tmp/rmux-501/main", "serverPid": 900, "serverStart": 1}),
        );
        let spec = folder.join("spec.json");
        write(
            &spec,
            json!({"schema": 1, "stateDirectory": state.to_str().unwrap(), "id": ID, "client": client,
                "flags": client_named(client).map(|c| crate::agents::spawn::client_flags(c).to_vec())
                    .unwrap_or_default(),
                "parentPane": "%1", "args": args, "prompt": "p"}),
        );
        let bin = w.ctx.cwd.parent().unwrap().join("bin");
        std::fs::create_dir(&bin).unwrap();
        for name in ["claude", "copilot"] {
            let path = bin.join(exe_name(name));
            std::fs::write(&path, b"").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let mut pairs: Vec<(String, String)> = w
            .ctx
            .env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        pairs.push(("PATH".into(), bin.to_str().unwrap().into()));
        w.ctx.env = Env::from_pairs(pairs);
        Launch {
            w,
            folder,
            spec,
            bin,
        }
    }

    fn launch(l: &Launch, spec: &str, answer: fn() -> io::Result<i32>) -> Ran {
        let mut err = Vec::new();
        let mut started = Vec::new();
        let code = run(
            &l.w.ctx,
            spec,
            &mut |line: &str| err.push(line.to_string()),
            &mut |path: &Path, args: &[String], remove: &[&str], _| {
                started.push((
                    path.to_path_buf(),
                    args.to_vec(),
                    remove.iter().map(|r| r.to_string()).collect(),
                ));
                answer()
            },
        );
        Ran { code, err, started }
    }

    fn spec_str(l: &Launch) -> String {
        l.spec.to_str().unwrap().to_string()
    }

    fn exits_7() -> io::Result<i32> {
        Ok(7)
    }

    // The parent's mailbox lines.
    fn mail(l: &Launch) -> Vec<Value> {
        let path = l.w.state().join("mail").join("pane-1.jsonl");
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn a_launch_names_its_state_root_and_path() {
        let mut w = World::human("");
        w.ctx.env = Env::from_pairs([("PATH", "client-path")]);
        assert_eq!(
            launch_overrides(
                &w.ctx,
                &w.ctx
                    .state_root
                    .join("k")
                    .join("agents")
                    .join(ID)
                    .join("spec.json")
            )
            .unwrap(),
            vec![
                (
                    "MUX_AGENTS_STATE",
                    w.ctx.state_root.to_str().unwrap().to_string()
                ),
                ("PATH", "client-path".to_string()),
            ]
        );
    }

    // The link check's decision, as text, so the Windows rules run on every host.
    #[test]
    fn windows_spec_path_comparison() {
        use crate::state::root::OsKind::{Unix, Windows};
        let same = |resolved: &str, given: &str, os| same_path(Path::new(resolved), given, os);
        let spec = r"C:\Users\me\AppData\Local\mux-agents\state\k\agents\a\spec.json";
        // canonicalize answers `\\?\C:\...`; the given path has no prefix, or has it too.
        let resolved = format!(r"\\?\{spec}");
        assert!(same(&resolved, spec, Windows));
        assert!(same(&resolved, &resolved, Windows));
        assert!(same(spec, spec, Windows));
        // NTFS names ignore case: a drive letter or folder typed in another case is the same file.
        assert!(same(&resolved, &spec.to_lowercase(), Windows));
        assert!(same(&resolved, &spec.replace("C:", "c:"), Windows));
        // A junction or link resolves elsewhere; an 8.3 short name resolves to its long name. Both refuse.
        let elsewhere = r"\\?\D:\elsewhere\agents\a\spec.json";
        assert!(!same(elsewhere, spec, Windows));
        assert!(!same(
            &resolved,
            r"C:\Users\RUNNER~1\AppData\Local\mux-agents\state\k\agents\a\spec.json",
            Windows
        ));
        // A UNC spec always refuses, whatever form either side takes, even when both sides agree.
        let unc = r"\\?\UNC\srv\share\spec.json";
        let plain_unc = r"\\srv\share\spec.json";
        assert!(!same(unc, plain_unc, Windows));
        assert!(!same(unc, unc, Windows));
        assert!(!same(plain_unc, plain_unc, Windows));
        assert!(!same(r"\\?\unc\srv\share\spec.json", unc, Windows));
        assert!(!same(
            "//srv/share/spec.json",
            "//srv/share/spec.json",
            Windows
        ));
        assert!(!same(&resolved, plain_unc, Windows));
        // Unix: exact bytes, no prefix and no case folding.
        assert!(same(
            "/s/k/agents/a/spec.json",
            "/s/k/agents/a/spec.json",
            Unix
        ));
        assert!(!same(
            "/s/k/agents/a/spec.json",
            "/S/k/agents/a/spec.json",
            Unix
        ));
        assert!(!same(r"\\?\/s/spec.json", "/s/spec.json", Unix));
    }

    #[test]
    fn spec_outside_root_exit_2() {
        let l = setup("claude", json!([]));
        let other = crate::testing::TempDir::new();
        let outside = other.path().join("spec.json");
        std::fs::copy(&l.spec, &outside).unwrap();
        let renamed = l.folder.join("other.json");
        std::fs::copy(&l.spec, &renamed).unwrap();
        let dotted = l.folder.join("..").join(ID).join("spec.json");
        for spec in [
            outside.to_str().unwrap().to_string(),
            renamed.to_str().unwrap().to_string(),
            dotted.to_str().unwrap().to_string(),
            "spec.json".to_string(),
            l.w.ctx.state_root.to_str().unwrap().to_string(),
        ] {
            let ran = launch(&l, &spec, exits_7);
            assert_eq!(
                (ran.code, ran.err.as_slice()),
                (2, &[OUTSIDE.to_string()][..]),
                "{spec}"
            );
            assert!(ran.started.is_empty());
            assert!(!l.folder.join("launcher.json").exists(), "{spec}");
        }
    }

    #[test]
    fn spec_link_exit_2() {
        let l = setup("claude", json!([]));
        // A spec that is not there cannot be shown free of links.
        let missing = l
            .folder
            .parent()
            .unwrap()
            .join("bbbbbbbbbbbb")
            .join("spec.json");
        let ran = launch(&l, missing.to_str().unwrap(), exits_7);
        assert_eq!((ran.code, ran.err.as_slice()), (2, &[LINK.to_string()][..]));
        #[cfg(unix)]
        {
            // A linked agent folder inside the root, and a linked spec.json.
            let elsewhere = crate::testing::TempDir::new();
            std::fs::copy(&l.spec, elsewhere.path().join("spec.json")).unwrap();
            let linked = l.folder.parent().unwrap().join("cccccccccccc");
            std::os::unix::fs::symlink(elsewhere.path(), &linked).unwrap();
            let file = l.folder.parent().unwrap().join("dddddddddddd");
            std::fs::create_dir(&file).unwrap();
            std::os::unix::fs::symlink(&l.spec, file.join("spec.json")).unwrap();
            for spec in [linked.join("spec.json"), file.join("spec.json")] {
                let ran = launch(&l, spec.to_str().unwrap(), exits_7);
                assert_eq!(
                    (ran.code, ran.err.as_slice()),
                    (2, &[LINK.to_string()][..]),
                    "{spec:?}"
                );
                assert!(ran.started.is_empty());
            }
        }
        assert!(!l.folder.join("launcher.json").exists());
    }

    // launch-1 G7, PM call 7: a folder-level spec (a pane an older build started, not yet claimed) is refused exit 2
    // with the remedy, before anything is claimed, written or started.
    #[test]
    fn a_folder_level_spec_names_its_remedy() {
        let l = setup("claude", json!([]));
        let before = crate::testing::tree(&l.w.ctx.state_root);
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(
            (ran.code, ran.err.as_slice()),
            (2, &[OLD_SPEC.to_string()][..])
        );
        assert!(ran.started.is_empty());
        assert_eq!(
            crate::testing::tree(&l.w.ctx.state_root),
            before,
            "nothing claimed or written"
        );
    }

    // PM call 7, all-calls 14: the wording is pinned; it names the remedy that works (a pre-lifecycle member never
    // resumes, G10d).
    #[test]
    fn the_old_spec_message_names_the_remedy() {
        assert_eq!(
            OLD_SPEC,
            "mux-agent: this pane was started by an older mux-agent and cannot launch; \
             run `mux-agent stop <name>` to end it, then start a new agent"
        );
    }

    // N2 on every OS: `gens` a regular file, or a link to a folder outside the member: refused before any read
    // (LINK), nothing written through it, nothing started.
    #[test]
    fn a_gens_that_is_a_file_or_a_link_runs_nothing() {
        for case in ["file", "link"] {
            let l = committed_spec("claude", json!([]));
            let outside = crate::testing::TempDir::new();
            let moved = outside.path().join("gens");
            std::fs::rename(l.folder.join("gens"), &moved).unwrap();
            if case == "file" {
                std::fs::write(l.folder.join("gens"), b"not a directory").unwrap();
            } else {
                crate::testing::link_dir(&moved, &l.folder.join("gens"));
            }
            let before = crate::testing::tree(outside.path());
            let r = launch(&l, &spec_str(&l), exits_7);
            assert_eq!(
                (r.code, r.err.as_slice()),
                (2, &[LINK.to_string()][..]),
                "{case}"
            );
            assert!(r.started.is_empty() && mail(&l).is_empty(), "{case}");
            assert_eq!(
                crate::testing::tree(outside.path()),
                before,
                "{case}: nothing written through it"
            );
        }
    }

    // On every OS (main's `a_dangling_commit_during_the_wait_is_never_absent` is Unix-only): a commit.json that is a
    // dangling link is never absent; the wait ends at its deadline `commit unreadable`; nothing is followed or run.
    #[test]
    fn a_dangling_commit_link_is_never_absent() {
        let (l, at) = gen_setup(0);
        let outside = crate::testing::TempDir::new();
        let target = outside.path().join("commit");
        crate::testing::link_dir(&target, &gens::path(&l.folder, 0, "commit"));
        let ran = launch(&l, at.to_str().unwrap(), exits_7);
        assert_eq!((ran.code, ran.started.len()), (1, 0));
        assert_eq!(l.w.fakes.slept(), vec![250; 120]);
        let exit = read_json(&gens::path(&l.folder, 0, "exit"))
            .unwrap()
            .unwrap();
        assert_eq!(exit["reason"], "commit unreadable");
        assert!(
            !target.exists() && std::fs::read_dir(outside.path()).unwrap().next().is_none(),
            "never followed"
        );
    }

    // On every OS (`spec_link_exit_2`'s linked folder is Unix-only): a spec reached through a linked member folder
    // is refused LINK before anything is claimed.
    #[test]
    fn a_spec_through_a_linked_member_folder_is_refused() {
        let (l, _) = gen_setup(0);
        let alias = l.folder.parent().unwrap().join("cccccccccccc");
        crate::testing::link_dir(&l.folder, &alias);
        let spec = alias.join("gens").join("0").join("spec.json");
        let r = launch(&l, spec.to_str().unwrap(), exits_7);
        assert_eq!((r.code, r.err.as_slice()), (2, &[LINK.to_string()][..]));
        assert!(r.started.is_empty() && !gens::claim_path(&l.folder, 0).exists());
    }

    #[test]
    fn generation_fence() {
        let l = committed_spec("claude", json!([]));
        assert_eq!(launch(&l, &spec_str(&l), exits_7).started.len(), 1);
        let record = launcher_files(&l).bytes;
        let messages = mail(&l);
        let second = launch(&l, &spec_str(&l), exits_7);
        assert!(second.started.is_empty());
        assert_eq!(second.code, 0);
        assert_eq!(launcher_files(&l).bytes, record);
        assert_eq!(mail(&l), messages);
        assert_eq!(
            read_json(&l.folder.join("gens/0.claim")).unwrap(),
            Some(json!("run"))
        );
        for value in [b"\"abandoned\"".as_slice(), b"\"run\"", b"corrupt", b""] {
            let l = committed_spec("claude", json!([]));
            std::fs::write(l.folder.join("gens/0.claim"), value).unwrap();
            let r = launch(&l, &spec_str(&l), exits_7);
            assert_eq!(r.code, 0);
            assert!(r.started.is_empty());
            assert!(launcher_files(&l).absent);
            assert!(mail(&l).is_empty());
            assert_eq!(std::fs::read(l.folder.join("gens/0.claim")).unwrap(), value);
        }
    }

    // a won claim and every folder above it are flushed to disk before the client runs. Any failed flush, at
    // `gens` or above the state root, leaves the claim `run` and an exit record, and the client never runs.
    #[test]
    fn generation_claim_is_flushed_before_the_client_runs() {
        let run_flushing = |l: &Launch, fail: bool| {
            let mut synced = Vec::new();
            let mut err = Vec::new();
            let mut started = 0;
            let code = run_with(
                &l.w.ctx,
                &spec_str(l),
                &mut |line: &str| err.push(line.to_string()),
                &mut |_: &Path, _: &[String], _: &[&str], _| {
                    started += 1;
                    Ok(0)
                },
                &mut |d: &Path, _: bool| {
                    synced.push(d.to_path_buf());
                    if fail {
                        Err(io::Error::other("flush failed"))
                    } else {
                        Ok(())
                    }
                },
            );
            (code, synced, started, err)
        };
        let l = committed_spec("claude", json!([]));
        let (code, synced, started, _) = run_flushing(&l, false);
        assert_eq!(code, 0);
        assert_eq!(started, 1);
        let root = l.w.ctx.state_root.clone();
        // gens, the agent folder, agents, the server folder, the root, then every folder above it.
        let want: Vec<PathBuf> = l
            .folder
            .join("gens")
            .ancestors()
            .filter(|d| !d.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .collect();
        assert_eq!(synced, want);
        assert!(synced.contains(&root));
        assert!(synced.len() > 5);
        let l = committed_spec("claude", json!([]));
        let (code, synced, started, err) = run_flushing(&l, true);
        assert_eq!((code, synced.len(), started), (1, 1, 0));
        assert!(err[0].contains("could not be flushed"), "{err:?}");
        assert_eq!(
            read_json(&l.folder.join("gens/0.claim")).unwrap(),
            Some(json!("run"))
        );
        assert_eq!(launcher_files(&l).record["exitCode"], 1);
        // A failed flush above the root stops the launch too; the sync it gets for those folders is the one that
        // skips folders the user cannot add to.
        let l = committed_spec("claude", json!([]));
        let depth = l.w.ctx.state_root.components().count();
        let mut err = Vec::new();
        let mut started = 0;
        let code = run_with(
            &l.w.ctx,
            &spec_str(&l),
            &mut |line: &str| err.push(line.to_string()),
            &mut |_: &Path, _: &[String], _: &[&str], _| {
                started += 1;
                Ok(0)
            },
            &mut |d: &Path, above_root: bool| {
                assert_eq!(
                    above_root,
                    d.components().count() < depth,
                    "{}",
                    d.display()
                );
                if above_root {
                    Err(io::Error::other("no write access"))
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!((code, started), (1, 0), "{err:?}");
        assert_eq!(launcher_files(&l).record["exitCode"], 1);
    }

    // S12: every OS through `testing::link_dir`; on Windows also through `testing::junction`, the fallback a user
    // without symlink rights gets (checklist 3). The claim path is joined per component, so no `/` reaches
    // `mklink /J`; the link target is compared in canonical form (call 34, checklist 2).
    #[test]
    fn generation_fence_dangling_claim_blocks_without_following_link() {
        #[cfg(unix)]
        let makers: [fn(&Path, &Path); 1] = [crate::testing::link_dir];
        #[cfg(windows)]
        let makers: [fn(&Path, &Path); 2] = [crate::testing::link_dir, crate::testing::junction];
        for make in makers {
            let l = committed_spec("claude", json!([]));
            let outside = crate::testing::TempDir::new();
            let target = outside.path().join("claim.json");
            let claim = l.folder.join("gens").join("0.claim");
            make(&target, &claim);
            assert!(!claim.exists());
            assert!(std::fs::symlink_metadata(&claim).unwrap().is_symlink());
            let r = launch(&l, &spec_str(&l), exits_7);
            assert!(!target.exists());
            assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
            assert_eq!(r.code, 0);
            assert!(r.started.is_empty());
            assert!(r.err.is_empty());
            assert!(launcher_files(&l).absent);
            assert!(mail(&l).is_empty());
            assert_eq!(
                unprefixed(&std::fs::read_link(&claim).unwrap()),
                unprefixed(&target)
            );
        }
    }

    #[test]
    fn generation_adversarial_active() {
        let l = committed_spec("claude", json!([]));
        let mut seen = false;
        let code = run(&l.w.ctx, &spec_str(&l), &mut |_| {}, &mut |_, _, _, _| {
            seen = true;
            let before = launcher_files(&l).bytes;
            let duplicate = launch(&l, &spec_str(&l), exits_7);
            assert!(duplicate.started.is_empty());
            assert_eq!(launcher_files(&l).bytes, before);
            assert!(mail(&l).is_empty());
            Ok(7)
        });
        assert!(seen);
        assert_eq!(code, 7);
        assert!(mail(&l).is_empty());
    }

    #[test]
    fn generation_adversarial_path_syntax_refusal() {
        for gen_id in [r"..\..\x", "C:x", "a.b", r"a\b"] {
            let l = committed_spec("claude", json!([]));
            // A drive-relative name can replace the folder prefix on Windows.
            let claim = l.folder.join("gens").join(format!("{gen_id}.claim"));
            assert!(!claim.exists(), "{gen_id}");
            // Backslash traversal would escape gens and the agent folder on Windows.
            #[cfg(windows)]
            let escaped = l.folder.parent().unwrap().join("x.claim");
            #[cfg(windows)]
            assert!(!escaped.exists());
            let mut v = read_json(&l.spec).unwrap().unwrap();
            v["generation"] = json!(gen_id);
            write(&l.spec, v);
            let seeded = crate::testing::tree(&l.folder.join("gens"));
            let r = launch(&l, &spec_str(&l), exits_7);
            assert_eq!(r.code, 1, "{gen_id}");
            assert!(r.started.is_empty(), "{gen_id}");
            assert!(launcher_files(&l).absent, "{gen_id}");
            assert!(mail(&l).is_empty(), "{gen_id}");
            assert!(!claim.exists(), "{gen_id}");
            assert!(launcher_files(&l).made.is_empty(), "{gen_id}");
            #[cfg(windows)]
            assert!(!escaped.exists(), "{gen_id}");
            // Checklist 5: the seeded generation is byte-for-byte as it was (nothing overwritten).
            assert_eq!(
                crate::testing::tree(&l.folder.join("gens")),
                seeded,
                "{gen_id}"
            );
        }
    }

    #[test]
    fn generation_adversarial_refusal() {
        for gen_id in [
            json!("A_z-09"),
            json!("A"),
            json!("../x"),
            json!(""),
            json!(2),
            json!(null),
            json!("a/b"),
            json!("x".repeat(65)),
            json!("中"),
        ] {
            let l = committed_spec("claude", json!([]));
            let mut v = read_json(&l.spec).unwrap().unwrap();
            v["generation"] = gen_id;
            write(&l.spec, v);
            let seeded = crate::testing::tree(&l.folder.join("gens"));
            let r = launch(&l, &spec_str(&l), exits_7);
            assert_eq!(r.code, 1);
            assert!(r.started.is_empty());
            assert!(launcher_files(&l).absent);
            assert!(mail(&l).is_empty());
            assert!(launcher_files(&l).made.is_empty());
            // Checklist 5: the seeded generation is byte-for-byte as it was (nothing overwritten).
            assert_eq!(crate::testing::tree(&l.folder.join("gens")), seeded);
        }
    }

    #[test]
    fn generation_adversarial_failed_start() {
        let l = committed_spec("claude", json!([]));
        let mut v = read_json(&l.spec).unwrap().unwrap();
        v["generation"] = json!("0");
        write(&l.spec, v);
        assert_eq!(
            launch(&l, &spec_str(&l), || Err(io::Error::other("uncertain"))).code,
            1
        );
        assert_eq!(
            read_json(&l.folder.join("gens/0.claim")).unwrap(),
            Some(json!("run"))
        );
        let record = launcher_files(&l).bytes;
        let messages = mail(&l);
        let duplicate = launch(&l, &spec_str(&l), exits_7);
        assert!(duplicate.started.is_empty());
        assert_eq!(duplicate.code, 0);
        assert_eq!(launcher_files(&l).bytes, record);
        assert_eq!(mail(&l), messages);
    }

    #[test]
    fn generation_adversarial_valid() {
        {
            let gen_id = "0".to_string();
            let l = committed_spec("claude", json!([]));
            let mut v = read_json(&l.spec).unwrap().unwrap();
            v["generation"] = json!(gen_id);
            write(&l.spec, v);
            let ran = launch(&l, &spec_str(&l), exits_7);
            assert_eq!(ran.code, 7);
            assert_eq!(ran.started.len(), 1);
            assert!(ran.err.is_empty());
            assert_eq!(
                read_json(&l.folder.join("gens").join(format!("{gen_id}.claim"))).unwrap(),
                Some(json!("run"))
            );
            let duplicate = launch(&l, &spec_str(&l), exits_7);
            assert_eq!(duplicate.code, 0);
            assert!(duplicate.started.is_empty());
        }
    }

    #[test]
    fn generation_claim_survives_commit_timeout_exit() {
        let l = committed_spec("claude", json!([]));
        std::fs::remove_file(gens::path(&l.folder, 0, "commit")).unwrap();
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(ran.code, 1);
        assert!(ran.started.is_empty());
        assert_eq!(l.w.fakes.slept(), vec![250; 120]);
        assert_eq!(launcher_files(&l).record["aborted"], true);
        assert!(!gens::committed(&l.folder, 0).unwrap());
        assert_eq!(
            read_json(&l.folder.join("gens/0.claim")).unwrap(),
            Some(json!("run"))
        );
        let record = launcher_files(&l).bytes;
        let messages = mail(&l);
        let second = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(second.code, 0);
        assert!(second.started.is_empty());
        assert_eq!(launcher_files(&l).bytes, record);
        assert_eq!(mail(&l), messages);
        assert_eq!(
            read_json(&l.folder.join("gens/0.claim")).unwrap(),
            Some(json!("run"))
        );
    }

    #[test]
    fn generation_claim_survives_abort_exit() {
        let l = committed_spec("claude", json!([]));
        std::fs::remove_file(gens::path(&l.folder, 0, "commit")).unwrap();
        write(
            &gens::path(&l.folder, 0, "commit"),
            json!({"aborted": true}),
        );
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(ran.code, 1);
        assert!(ran.started.is_empty());
        assert!(l.w.fakes.slept().is_empty());
        assert_eq!(launcher_files(&l).record["aborted"], true);
        assert_eq!(
            read_json(&l.folder.join("gens/0.claim")).unwrap(),
            Some(json!("run"))
        );
        let record = launcher_files(&l).bytes;
        let messages = mail(&l);
        let second = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(second.code, 0);
        assert!(second.started.is_empty());
        assert_eq!(launcher_files(&l).bytes, record);
        assert_eq!(mail(&l), messages);
        assert_eq!(
            read_json(&l.folder.join("gens/0.claim")).unwrap(),
            Some(json!("run"))
        );
    }

    #[test]
    fn abort_json_ends_1_without_client() {
        let l = committed_spec("claude", json!(["--session-id", "s", "p"]));
        std::fs::remove_file(gens::path(&l.folder, 0, "commit")).unwrap();
        write(
            &gens::path(&l.folder, 0, "commit"),
            json!({"aborted": true}),
        );
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(ran.code, 1);
        assert!(ran.started.is_empty() && ran.err.is_empty());
        assert_eq!(
            launcher_files(&l).record,
            json!({"pid": CALLER, "start": 2000, "startedAt": "1970-01-01T00:00:00.0000000Z",
                "aborted": true, "exitCode": 1})
        );
        // An abort wins even beside a commit (`gens::decide` reads `aborted` first), and ends the wait at once.
        let l = committed_spec("claude", json!([]));
        write(
            &gens::path(&l.folder, 0, "commit"),
            json!({"aborted": true, "committed": true}),
        );
        assert_eq!(launch(&l, &spec_str(&l), exits_7).code, 1);
        assert!(l.w.fakes.slept().is_empty());
    }

    #[test]
    fn commit_timeout_30s() {
        let l = committed_spec("claude", json!([]));
        std::fs::remove_file(gens::path(&l.folder, 0, "commit")).unwrap();
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(ran.code, 1);
        assert!(ran.started.is_empty());
        assert_eq!(l.w.fakes.slept(), vec![250; 120]);
        assert_eq!(launcher_files(&l).record["aborted"], true);
        assert_eq!(launcher_files(&l).record["exitCode"], 1);
        // A commit already there needs no wait.
        let l = committed_spec("claude", json!([]));
        assert_eq!(launch(&l, &spec_str(&l), exits_7).code, 7);
        assert!(l.w.fakes.slept().is_empty());
    }

    #[test]
    fn a_timed_out_launcher_makes_commit_json_as_its_give_up_unless_the_spawn_did() {
        // Timed out with no commit: it makes commit.json as its give-up, so a late spawn can no longer commit.
        let l = committed_spec("claude", json!([]));
        std::fs::remove_file(gens::path(&l.folder, 0, "commit")).unwrap();
        assert_eq!(launch(&l, &spec_str(&l), exits_7).code, 1);
        assert_eq!(launcher_files(&l).record["aborted"], true);
        assert!(!gens::committed(&l.folder, 0).unwrap());
        assert!(
            !crate::state::fs::claim_json(&gens::path(&l.folder, 0, "commit"), &json!({})).unwrap()
        );
        // A give-up already there (another launcher's): no client.
        let l = committed_spec("claude", json!([]));
        write(
            &gens::path(&l.folder, 0, "commit"),
            json!({"aborted": true}),
        );
        assert_eq!(launch(&l, &spec_str(&l), exits_7).code, 1);
        assert!(l.w.fakes.slept().is_empty());
    }

    #[test]
    fn scrub_list_exact() {
        assert_eq!(
            SCRUB,
            [
                "CLAUDECODE",
                "CLAUDE_CODE_SESSION_ID",
                "CLAUDE_CODE_MESSAGING_SOCKET",
                "CLAUDE_CODE_MESSAGING_TOKEN",
                "CLAUDE_CODE_CHILD_SESSION",
                "CLAUDE_CODE_SESSION_ATTENDED",
                "CLAUDE_CODE_ENTRYPOINT",
                "CLAUDE_CODE_EXECPATH",
                "CLAUDE_PID",
                "CLAUDE_EFFORT",
                "COPILOT_AGENT_SESSION_ID",
                "COPILOT_CLI",
                "TMUX",
                "TMUX_PANE",
                "RMUX",
            ]
        );
        let l = committed_spec("copilot", json!(["-i", "p"]));
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(ran.started.len(), 1);
        assert_eq!(ran.started[0].2, SCRUB);
    }

    #[test]
    fn client_flags_then_args() {
        for (client, flags) in [
            (
                "claude",
                &["--dangerously-skip-permissions", "--effort", "high"][..],
            ),
            ("copilot", &["--no-sandbox", "--allow-all"]),
        ] {
            let l = committed_spec(client, json!(["--session-id", "s", "two words"]));
            let ran = launch(&l, &spec_str(&l), exits_7);
            assert_eq!(ran.code, 7, "{client}");
            let (path, args, _) = &ran.started[0];
            assert_eq!(path, &l.bin.join(exe_name(client)));
            let mut expected: Vec<String> = flags.iter().map(|f| f.to_string()).collect();
            expected.extend(["--session-id", "s", "two words"].map(String::from));
            assert_eq!(args, &expected);
            let record = launcher_files(&l).record;
            assert_eq!(
                record,
                json!({"pid": CALLER, "start": 2000, "startedAt": "1970-01-01T00:00:00.0000000Z",
                    "exitCode": 7, "exitedAt": "1970-01-01T00:00:00.0000000Z"})
            );
        }
        // An unknown client starts nothing.
        let l = committed_spec("Claude", json!([]));
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(
            (ran.code, ran.err.as_slice()),
            (
                1,
                &["mux-agent: the launch spec names no known client".to_string()][..]
            )
        );
        assert!(ran.started.is_empty());
    }

    // phase 2a-i: the client runs with the flags its spec records, byte for byte, then its args; a spec with no
    // recorded flags starts nothing rather than fall back to a fixed list.
    #[test]
    fn the_launch_runs_the_flags_its_spec_records() {
        let l = committed_spec("claude", json!(["a"]));
        let spec = l.spec.clone();
        let mut v: Value = read_json(&spec).unwrap().unwrap();
        v["flags"] = json!(["--model", "x y", "\u{e9}"]);
        write(&spec, v);
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(ran.started[0].1, ["--model", "x y", "\u{e9}", "a"]);
        for flags in [None, Some(json!("--x")), Some(json!(["--x", 1]))] {
            let l = committed_spec("claude", json!(["a"]));
            let spec = l.spec.clone();
            let mut v: Value = read_json(&spec).unwrap().unwrap();
            match flags {
                None => {
                    v.as_object_mut().unwrap().remove("flags");
                }
                Some(f) => v["flags"] = f,
            }
            write(&spec, v);
            let ran = launch(&l, &spec_str(&l), exits_7);
            assert_eq!(ran.code, 1);
            assert!(ran.started.is_empty());
            assert_eq!(
                ran.err,
                ["mux-agent: the launch spec names no client flags"]
            );
        }
    }

    #[test]
    fn client_not_on_path_exit_1_launcher_json() {
        let l = committed_spec("claude", json!([]));
        std::fs::remove_file(l.bin.join(exe_name("claude"))).unwrap();
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(ran.code, 1);
        assert_eq!(
            ran.err,
            [format!("mux-agent: {} is not on PATH", exe_name("claude"))]
        );
        assert!(ran.started.is_empty());
        assert_eq!(launcher_files(&l).record["exitCode"], 1);
        assert!(!gens::path(&l.folder, 0, "notice").exists());
        assert!(mail(&l).is_empty());
    }

    #[test]
    fn launcher_start_error_writes_fallback_exit_record() {
        // The client fails to start after PATH resolution.
        let l = committed_spec("claude", json!([]));
        let ran = launch(&l, &spec_str(&l), || Err(io::Error::other("no exec")));
        assert_eq!(ran.code, 1);
        assert_eq!(ran.started.len(), 1);
        assert_eq!(launcher_files(&l).record["exitCode"], 1);
        assert!(!gens::path(&l.folder, 0, "notice").exists());
        assert!(mail(&l).is_empty());
        // A failure after the claim (no args) still leaves the fallback exit record, and no notice.
        let l = committed_spec("claude", json!(null));
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(ran.code, 1);
        assert!(ran.started.is_empty());
        assert_eq!(launcher_files(&l).record["exitCode"], 1);
        assert!(!gens::path(&l.folder, 0, "notice").exists());
        assert!(mail(&l).is_empty());
        assert_eq!(
            read_json(&l.folder.join("gens/0.claim")).unwrap(),
            Some(json!("run"))
        );
        // A failure before the claim (malformed spec) writes no record and no notice.
        let l = committed_spec("claude", json!([]));
        std::fs::write(&l.spec, b"{not json").unwrap();
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(ran.code, 1);
        assert!(ran.started.is_empty());
        assert!(launcher_files(&l).absent);
        assert!(mail(&l).is_empty());
    }

    #[test]
    fn launcher_cmdline_over_32767_with_resolved_client_path_refuses() {
        let probe = committed_spec("claude", json!([]));
        let exe = probe.bin.join(exe_name("claude"));
        let exe_len = exe.to_str().unwrap().encode_utf16().count();
        let total = |n: usize| {
            let arg = "x".repeat(n);
            let mut argv = vec!["--dangerously-skip-permissions", "--effort", "high"];
            argv.push(&arg);
            exe_len + 3 + cmdline_length(&argv)
        };
        let fit = 32767 - total(1) + 1;
        assert_eq!(total(fit), 32767);
        let l = committed_spec("claude", json!(["x".repeat(fit)]));
        assert_eq!(launch(&l, &spec_str(&l), exits_7).started.len(), 1);
        let l = committed_spec("claude", json!(["x".repeat(fit + 1)]));
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(ran.code, 1);
        assert!(ran.started.is_empty());
        assert_eq!(
            ran.err,
            ["mux-agent: the client command line is too long (32768 characters)"]
        );
        assert_eq!(launcher_files(&l).record["exitCode"], 1);
    }

    #[test]
    fn launcher_leaves_notices_to_reap() {
        {
            for case in [
                "exit",
                "start-error",
                "no-args",
                "timeout",
                "abort",
                "missing-client",
            ] {
                let l = committed_spec(
                    "claude",
                    if case == "no-args" {
                        Value::Null
                    } else {
                        json!([])
                    },
                );
                if case == "timeout" {
                    std::fs::remove_file(gens::path(&l.folder, 0, "commit")).unwrap();
                }
                if case == "abort" {
                    write(
                        &gens::path(&l.folder, 0, "commit"),
                        json!({"aborted": true, "committed": true}),
                    );
                }
                if case == "missing-client" {
                    std::fs::remove_file(l.bin.join(exe_name("claude"))).unwrap();
                }
                let mut starts = 0;
                let code = run(&l.w.ctx, &spec_str(&l), &mut |_| {}, &mut |_, _, _, _| {
                    starts += 1;
                    if case == "start-error" {
                        Err(io::Error::other("no exec"))
                    } else {
                        Ok(7)
                    }
                });
                assert_eq!(code, if case == "exit" { 7 } else { 1 });
                assert_eq!(
                    l.w.fakes.slept(),
                    if case == "timeout" {
                        vec![250; 120]
                    } else {
                        vec![]
                    }
                );
                assert_eq!(launcher_files(&l).record["exitCode"], code);
                assert_eq!(starts, usize::from(["exit", "start-error"].contains(&case)));
                assert!(!gens::path(&l.folder, 0, "notice").exists());
                assert!(mail(&l).is_empty());
                assert_eq!(
                    read_json(&l.folder.join("gens/0.claim")).unwrap(),
                    Some(json!("run"))
                );
                let before = launcher_files(&l).bytes;
                assert_eq!(
                    run(
                        &l.w.ctx,
                        &spec_str(&l),
                        &mut |_| {},
                        &mut |_, _, _, _| panic!("duplicate")
                    ),
                    0
                );
                assert_eq!(launcher_files(&l).bytes, before);
                l.w.fakes.hc_done().unwrap();
            }
        }
    }

    #[test]
    fn the_launcher_records_its_client_before_it_waits() {
        let l = committed_spec("claude", json!([]));
        l.w.fakes
            .process(crate::testing::row(4300, l.w.ctx.pid, 1600));
        let mut seen = Value::Null;
        let code = run(
            &l.w.ctx,
            &spec_str(&l),
            &mut |_| {},
            &mut |_, _, _, started| {
                started(4300);
                seen = launcher_files(&l).record["client"].clone();
                Ok(7)
            },
        );
        assert_eq!(code, 7);
        assert_eq!(
            seen,
            json!({"pid": 4300, "start": 1600}),
            "on disk while the client runs"
        );
        assert_eq!(
            launcher_files(&l).record["client"],
            json!({"pid": 4300, "start": 1600}),
            "kept after the exit"
        );
    }

    const HC: &str = "m0123abcd";
    // Generation n of the setup member, pending (launch-1 G1.1): record `lifecycle: 1`, `pending {gen, hcName, sessionId}`;
    // gens/<n>/spec.json (the folder spec plus id, generation, sessionId) and launch.json (G12). No gens/<n>/commit.json:
    // the folder-level commit.json of `setup` must never count.
    fn gen_setup_of(client: &str, args: Value, n: u64) -> (Launch, PathBuf) {
        let l = setup(client, args);
        edit(&l.folder.join("record.json"), |r| {
            r["lifecycle"] = json!(1);
            r["pending"] = json!({"gen": n, "kind": "spawn", "hcName": HC, "sessionId": "s1"});
        });
        let mut spec = read_json(&l.spec).unwrap().unwrap();
        for (k, v) in [
            ("id", json!(ID)),
            ("generation", json!(n.to_string())),
            ("sessionId", json!("s1")),
        ] {
            spec[k] = v;
        }
        let at = gens::path(&l.folder, n, "spec");
        evidence::publish(&at, &spec).unwrap();
        let launch = json!({"hcName": HC, "launcherArgv": [], "sessionId": "s1"});
        evidence::publish(&gens::path(&l.folder, n, "launch"), &launch).unwrap();
        (l, at)
    }
    fn gen_setup(n: u64) -> (Launch, PathBuf) {
        gen_setup_of("claude", json!(["--session-id", "s1"]), n)
    }
    /// PM call 22, placement: `setup(client, args)`'s member as committed generation 0. Its `spec` names
    /// `gens/0/spec.json`, so a moved test's `spec_str(&l)`, `l.spec` reads and edits all follow it.
    fn committed_spec(client: &str, args: Value) -> Launch {
        let (mut l, at) = gen_setup_of(client, args, 0);
        commit(&l, 0, json!({"committed": true}));
        l.spec = at;
        l
    }
    /// PM call 22, reading: generation 0's evidence, where a moved test read the folder's.
    struct Files {
        /// The folder `launcher.json` the test read: `gens/0/launcher.json`, plus `exit.json`'s `exitCode`, plus
        /// `exitedAt` when the client exited or `aborted: true` when the launch gave up or was aborted, plus
        /// `client.json` as `client`. `Null` when none of the three exists.
        record: Value,
        /// The raw bytes of `launcher.json`, `exit.json`, `client.json` (each `None` when absent): the test's
        /// byte-for-byte `launcher.json` comparison.
        bytes: [Option<Vec<u8>>; 3],
        /// Checklist 4: none of `launcher.json`, `exit.json`, `client.json` exists, not even as a link (the test's
        /// "no `launcher.json`" is nonexistence, never a null value).
        absent: bool,
        /// Everything under `gens` (links kept as links, never followed) except the spec, launch and commit
        /// `committed_spec` placed: the test's "no `gens`" is this list empty.
        made: Vec<PathBuf>,
    }
    fn launcher_files(l: &Launch) -> Files {
        let files = ["launcher", "exit", "client"].map(|f| gens::path(&l.folder, 0, f));
        let absent = files.iter().all(|p| std::fs::symlink_metadata(p).is_err());
        let bytes = files.clone().map(|p| std::fs::read(p).ok());
        let parse = |b: &Option<Vec<u8>>| {
            b.as_ref()
                .map(|b| serde_json::from_slice::<Value>(b).unwrap())
        };
        let (launcher, exit, client) = (parse(&bytes[0]), parse(&bytes[1]), parse(&bytes[2]));
        let mut record = launcher.unwrap_or(Value::Null);
        if let Some(e) = exit {
            record["exitCode"] = e["exitCode"].clone();
            match e["reason"].as_str() {
                Some("exited" | "client-conflict") => record["exitedAt"] = e["exitedAt"].clone(),
                Some("gave up" | "aborted") => record["aborted"] = json!(true),
                _ => {}
            }
        }
        if let Some(c) = client {
            record["client"] = c;
        }
        let placed = ["spec", "launch", "commit"].map(|f| gens::path(&l.folder, 0, f));
        let mut made = Vec::new();
        under(&l.folder.join("gens"), &mut made);
        made.retain(|p| !placed.contains(p) && *p != gens::dir(&l.folder, 0));
        Files {
            record,
            bytes,
            absent,
            made,
        }
    }
    /// Every path under `dir`, never following a link (a link or file is listed, a real folder listed and entered).
    fn under(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries {
            let p = e.unwrap().path();
            let real_dir = std::fs::symlink_metadata(&p).unwrap().is_dir();
            out.push(p.clone());
            if real_dir {
                under(&p, out);
            }
        }
    }
    /// Call 34: a path in canonical form for a link-target comparison: a drive's `\\?\` prefix dropped (Windows
    /// `read_link` and `TempDir` may spell the same target with and without it).
    fn unprefixed(p: &Path) -> PathBuf {
        let s = p.to_str().unwrap();
        PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(s))
    }
    fn edit(at: &Path, change: impl FnOnce(&mut Value)) {
        let mut v = read_json(at).unwrap().unwrap();
        change(&mut v);
        std::fs::write(at, serde_json::to_vec(&v).unwrap()).unwrap();
    }
    fn commit(l: &Launch, n: u64, body: Value) {
        evidence::publish(&gens::path(&l.folder, n, "commit"), &body).unwrap();
    }
    fn gen_file(l: &Launch, file: &str) -> Value {
        match gens::read(&l.folder, 1, file) {
            Evidence::Present(v) => v,
            other => panic!("{file}: {other:?}"),
        }
    }
    // Writes `body` as gens/1/commit.json right after the nth sleep.
    fn commit_on_sleep(l: &Launch, nth: usize, body: &'static [u8]) {
        let at = gens::path(&l.folder, 1, "commit");
        l.w.fakes.sleep_probe(move |k| {
            if k == nth {
                std::fs::write(&at, body).unwrap();
            }
        });
    }
    // The test's stand-in for the client: told the client pid, answers the exit code.
    type ClientStart<'a> = &'a mut dyn FnMut(&mut dyn FnMut(u32)) -> io::Result<i32>;
    struct Seamed {
        code: i32,
        err: Vec<String>,
        started: usize,
        offered: Vec<PathBuf>,
    }
    // `run_seamed` with every flush answering `flushed`, and a publisher that fails the first k offers of each listed
    // path (Err, nothing written) before it publishes for real; every offered path is recorded.
    fn seamed(
        l: &Launch,
        spec: &Path,
        flushed: bool,
        fails: &[(PathBuf, usize)],
        start: ClientStart,
    ) -> Seamed {
        let (mut left, mut offered, mut err, mut started) =
            (fails.to_vec(), Vec::new(), Vec::new(), 0);
        let code = run_seamed(
            &l.w.ctx,
            spec.to_str().unwrap(),
            &mut |line: &str| err.push(line.to_string()),
            &mut |_: &Path, _: &[String], _: &[&str], told: &mut dyn FnMut(u32)| {
                started += 1;
                start(told)
            },
            &mut |_: &Path, _: bool| {
                if flushed {
                    Ok(())
                } else {
                    Err(io::Error::other("flush failed"))
                }
            },
            &mut |at: &Path, v: &Value| {
                offered.push(at.to_path_buf());
                if let Some((_, k)) = left.iter_mut().find(|(p, k)| p.as_path() == at && *k > 0) {
                    *k -= 1;
                    return Err(Refusal::internal("injected publication failure"));
                }
                evidence::publish(at, v)
            },
        );
        Seamed {
            code,
            err,
            started,
            offered,
        }
    }

    #[test]
    fn a_spec_path_is_a_folder_a_generation_or_malformed() {
        let root = PathBuf::from("state");
        let m = root.join("0123456789abcdef").join("agents").join(ID);
        let at = |parts: &[&str]| parts.iter().fold(m.clone(), |p, s| p.join(s));
        assert_eq!(classify_spec(&root, &at(&["spec.json"])), Spec::Folder);
        assert_eq!(
            classify_spec(&root, &at(&["gens", "1", "spec.json"])),
            Spec::Gen(m.clone(), 1)
        );
        assert_eq!(
            classify_spec(&root, &at(&["gens", "0", "spec.json"])),
            Spec::Gen(m.clone(), 0)
        );
        let other = root
            .join("0123456789abcdef")
            .join("notagents")
            .join(ID)
            .join("gens")
            .join("1")
            .join("spec.json");
        for bad in [
            at(&["gens", "01", "spec.json"]),
            at(&["gens", "+1", "spec.json"]),
            at(&["gens", "-1", "spec.json"]),
            at(&["gens", "x", "spec.json"]),
            at(&["gens", "18446744073709551616", "spec.json"]),
            at(&["gens", "spec.json"]),
            at(&["gens", "1", "x", "spec.json"]),
            at(&["x", "gens", "1", "spec.json"]),
            at(&["GENS", "1", "spec.json"]),
            // Redesign 3: an id that is not 12 lowercase hex, or a name that is not plain, never becomes a member.
            root.join("0123456789abcdef")
                .join("agents")
                .join("AAAAAAAAAAAA")
                .join("gens")
                .join("1")
                .join("spec.json"),
            root.join("0123456789abcdef")
                .join("agents")
                .join("..")
                .join("gens")
                .join("1")
                .join("spec.json"),
            root.join("k")
                .join("agents")
                .join(ID)
                .join("gens")
                .join("1")
                .join("spec.json"),
            root.join("gens").join("1").join("spec.json"),
            other,
        ] {
            assert_eq!(
                classify_spec(&root, &bad),
                Spec::Malformed,
                "{}",
                bad.display()
            );
        }
    }

    // G7: a path that names `gens` is a generation's or refused; it never reaches the folder-level launch, which would
    // claim gens/<x>/gens/<n>.claim and decide through agents::committed.
    #[test]
    fn a_malformed_generation_path_is_refused_never_run_as_a_folder() {
        for name in ["01", "x"] {
            let (l, _) = gen_setup(1);
            let dir = l.folder.join("gens").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::copy(gens::path(&l.folder, 1, "spec"), dir.join("spec.json")).unwrap();
            std::fs::write(dir.join("commit.json"), br#"{"x":1}"#).unwrap();
            let ran = launch(&l, dir.join("spec.json").to_str().unwrap(), exits_7);
            assert_eq!((ran.code, ran.started.len()), (2, 0), "{name}");
            assert_eq!(ran.err, [GEN_PATH], "{name}");
            for left in [
                dir.join("gens"),
                dir.join("launcher.json"),
                gens::claim_path(&l.folder, 1),
            ] {
                assert!(!left.exists(), "{name}: {}", left.display());
            }
        }
    }

    // outcome-2 O5 (; finding 5to8-14): a restore whose conversation is gone, through the launcher entry. The
    // stand-in client knows only conversation `s1`: `--resume` of any other prints the verified claude rejection
    // (`No conversation found with session ID: <uuid>`) on its stderr and exits 1 before readiness. The launcher records
    // exit.json; the generation is failed and needs recovery; no task is resent (no prompt in the spec, no hc call).
    #[test]
    fn a_resume_of_a_gone_conversation_fails_and_needs_recovery() {
        use crate::agents::outcome::{EXITED, Recovery};
        const GONE: &str = "7d1c0e5a-9b1f-4d0e-8f7a-3c2b1a0e9d8c";
        let (l, spec) = gen_setup(1);
        edit(&spec, |s| {
            s["args"] = json!(["--resume", GONE]);
            s.as_object_mut().unwrap().remove("prompt");
        });
        commit(&l, 1, json!({"committed": true}));
        l.w.fakes
            .process(crate::testing::row(4300, l.w.ctx.pid, 1600));
        let (mut argv, mut said): (Vec<String>, Option<String>) = (Vec::new(), None);
        let code = run(
            &l.w.ctx,
            spec.to_str().unwrap(),
            &mut |_| {},
            &mut |_: &Path, args: &[String], _: &[&str], told: &mut dyn FnMut(u32)| {
                argv = args.to_vec();
                told(4300);
                let asked = args
                    .iter()
                    .position(|a| a == "--resume")
                    .and_then(|i| args.get(i + 1));
                match asked {
                    Some(id) if id != "s1" => {
                        said = Some(format!("No conversation found with session ID: {id}"));
                        Ok(1)
                    }
                    _ => Ok(0),
                }
            },
        );
        assert_eq!(code, 1);
        let rejected = format!("No conversation found with session ID: {GONE}");
        assert_eq!(
            said.as_deref(),
            Some(rejected.as_str()),
            "the verified rejection of the unknown conversation"
        );
        assert_eq!(
            argv[argv.len() - 2..],
            ["--resume".to_string(), GONE.to_string()],
            "{argv:?}"
        );
        let exit = gen_file(&l, "exit");
        assert_eq!(
            (&exit["exitCode"], &exit["reason"]),
            (&json!(1), &json!("exited"))
        );
        // Promoted, as forward does after the commit: generation 1 is active.
        let record = json!({"lifecycle": 1, "id": ID, "generation": "1"});
        let key = l.w.caller().server_key;
        let got =
            crate::agents::gens::needs_recovery(&l.w.ctx, &key, ID, &record, Some(false), None);
        assert_eq!(
            got,
            Ok(Some(Recovery {
                generation: 1,
                reason: EXITED.into(),
                running: true
            }))
        );
        assert_eq!(gen_file(&l, "outcome")["outcome"], "failed");
        assert_eq!(gens::read(&l.folder, 1, "ready"), Evidence::Absent);
        assert!(l.w.fakes.hc_calls().is_empty(), "no task resent");
    }

    #[test]
    fn a_gens_spec_claims_its_generation_and_names_it_to_the_client() {
        let (l, spec) = gen_setup(1);
        commit(&l, 1, json!({"committed": true}));
        let ran = launch(&l, spec.to_str().unwrap(), exits_7);
        assert_eq!((ran.code, ran.started.len()), (7, 1), "{:?}", ran.err);
        assert_eq!(gens::claim(&l.folder, 1), Evidence::Present(json!("run")));
        assert!(
            !gens::dir(&l.folder, 1).join("gens").exists(),
            "no nested claim"
        );
        assert!(!gens::claim_path(&l.folder, 0).exists());
        let exit = gen_file(&l, "exit");
        assert_eq!(
            (&exit["exitCode"], &exit["reason"]),
            (&json!(7), &json!("exited"))
        );
        // G7: the client of a generation spec gets MUX_AGENTS_GEN=<n>; a folder-level launch gets none.
        let var = |at: &Path| {
            let all = launch_overrides(&l.w.ctx, at).unwrap();
            all.into_iter().find(|(k, _)| *k == "MUX_AGENTS_GEN")
        };
        assert_eq!(var(&spec), Some(("MUX_AGENTS_GEN", "1".to_string())));
        assert_eq!(var(&l.spec), None);
    }

    #[test]
    fn commit_wait_absent_then_committed_executes_once() {
        let (l, spec) = gen_setup(1);
        commit_on_sleep(&l, 3, br#"{"committed":true}"#);
        let ran = launch(&l, spec.to_str().unwrap(), exits_7);
        assert_eq!(
            (ran.code, ran.started.len(), l.w.fakes.slept().len()),
            (7, 1, 3)
        );
        assert_eq!(gen_file(&l, "exit")["reason"], "exited");
    }

    #[test]
    fn commit_wait_torn_waits_for_a_whole_read() {
        let (l, spec) = gen_setup(1);
        std::fs::write(gens::path(&l.folder, 1, "commit"), br#"{"comm"#).unwrap();
        commit_on_sleep(&l, 5, br#"{"committed":true}"#);
        let ran = launch(&l, spec.to_str().unwrap(), exits_7);
        assert_eq!(
            (ran.code, ran.started.len()),
            (7, 1),
            "a torn commit is not a decision"
        );
        assert_eq!(l.w.fakes.slept().len(), 5);
    }

    #[test]
    fn commit_wait_unreadable_at_30s_exits_commit_unreadable() {
        let (l, spec) = gen_setup(1);
        let at = gens::path(&l.folder, 1, "commit");
        std::fs::write(&at, b"{").unwrap();
        let ran = launch(&l, spec.to_str().unwrap(), exits_7);
        assert_eq!((ran.code, ran.started.len()), (1, 0));
        assert_eq!(l.w.fakes.slept(), vec![250; 120]);
        assert_eq!(gen_file(&l, "exit")["reason"], "commit unreadable");
        assert_eq!(std::fs::read(&at).unwrap(), b"{", "never overwritten");
    }

    // The commit lands on the clock read at the deadline, after the last Absent: the give-up loses, the commit wins.
    #[test]
    fn a_commit_landing_at_the_deadline_wins_over_the_give_up() {
        let (l, spec) = gen_setup(1);
        let at = gens::path(&l.folder, 1, "commit");
        let base = std::cell::Cell::new(None);
        l.w.fakes.clock_probe(move |ms| {
            let start = base.get().unwrap_or(ms);
            base.set(Some(start));
            if ms >= start + 30_000 && !at.exists() {
                std::fs::write(&at, br#"{"committed":true}"#).unwrap();
            }
        });
        let ran = launch(&l, spec.to_str().unwrap(), exits_7);
        assert_eq!((ran.code, ran.started.len()), (7, 1));
        assert_eq!(gen_file(&l, "commit"), json!({"committed": true}));
        assert_eq!(gen_file(&l, "exit")["reason"], "exited");
    }

    #[test]
    fn an_aborted_contradictory_or_invalid_commit_runs_nothing() {
        for (body, reason) in [
            (json!({"aborted": true}), "aborted"),
            (json!({"committed": true, "aborted": true}), "aborted"),
            (json!({"x": 1}), "commit invalid"),
            (json!({"committed": "true"}), "commit invalid"),
        ] {
            let (l, spec) = gen_setup(1);
            commit(&l, 1, body.clone());
            let ran = launch(&l, spec.to_str().unwrap(), exits_7);
            assert_eq!((ran.code, ran.started.len()), (1, 0), "{body}");
            assert_eq!(gen_file(&l, "exit")["reason"], reason, "{body}");
            assert!(
                l.w.fakes.slept().is_empty(),
                "{body}: a whole commit decides at once"
            );
        }
    }

    #[test]
    fn launcher_json_is_never_rewritten_after_a_give_up() {
        let (l, spec) = gen_setup(1);
        let ran = launch(&l, spec.to_str().unwrap(), exits_7);
        assert_eq!(
            (ran.code, ran.started.len()),
            (1, 0),
            "absent at 30 s: its own aborted commit"
        );
        assert_eq!(gen_file(&l, "commit")["aborted"], true);
        let launcher = gen_file(&l, "launcher");
        let keys: Vec<&str> = launcher
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["pid", "start", "startedAt"]);
        // Fix cycle 2: the identity values, not only the keys: the fake launcher (World CALLER 4242, start 2000).
        assert_eq!(
            (&launcher["pid"], &launcher["start"]),
            (&json!(4242), &json!(2000))
        );
        assert_eq!(gen_file(&l, "exit")["reason"], "gave up");
    }

    #[test]
    fn launcher_json_is_unchanged_after_the_client_exits() {
        let (l, spec) = gen_setup(1);
        commit(&l, 1, json!({"committed": true}));
        let at = gens::path(&l.folder, 1, "launcher");
        let mut during = Vec::new();
        let code = run(
            &l.w.ctx,
            spec.to_str().unwrap(),
            &mut |_| {},
            &mut |_, _, _, _| {
                during = std::fs::read(&at).unwrap();
                Ok(0)
            },
        );
        assert_eq!(code, 0);
        assert_eq!(
            std::fs::read(&at).unwrap(),
            during,
            "the exit leaves launcher.json as the client saw it"
        );
        assert_eq!(gen_file(&l, "exit")["exitCode"], 0);
    }

    #[test]
    fn a_claim_that_cannot_be_flushed_runs_nothing() {
        let (l, spec) = gen_setup(1);
        commit(&l, 1, json!({"committed": true}));
        let r = seamed(&l, &spec, false, &[], &mut |_| Ok(7));
        assert_eq!((r.code, r.started), (1, 0));
        assert!(r.err[0].contains("could not be flushed"), "{:?}", r.err);
        assert_eq!(gens::claim(&l.folder, 1), Evidence::Present(json!("run")));
        assert_eq!(gen_file(&l, "exit")["reason"], "claim not flushed");
        assert!(
            !gens::path(&l.folder, 1, "launcher").exists(),
            "nothing after a failed flush"
        );
    }

    // A failed publication of launcher.json (injected) stops the launch. (A launcher.json already there is the
    // earlier-evidence refusal below.)
    #[test]
    fn a_launcher_json_that_cannot_be_published_runs_nothing() {
        let (l, spec) = gen_setup(1);
        commit(&l, 1, json!({"committed": true}));
        let at = gens::path(&l.folder, 1, "launcher");
        let r = seamed(&l, &spec, true, &[(at.clone(), 1)], &mut |_| Ok(7));
        assert_eq!((r.code, r.started), (1, 0));
        assert!(!at.exists(), "nothing written");
        assert_eq!(gen_file(&l, "exit")["reason"], "launcher not published");
    }

    // Final audit (PM): a won claim means no launcher ran this generation. launcher.json, client.json or exit.json
    // already there (anything not Absent) is corrupt evidence: nothing starts, the error names the file, nothing more
    // is published, and the file is unchanged. The exit case is the audit repro.
    #[test]
    fn a_generation_with_earlier_evidence_runs_nothing() {
        // Anything but Absent refuses: whole JSON (Present) and torn bytes (Unreadable), for each file.
        let whole = br#"{"exitCode":1,"reason":"client not started","exitedAt":"x"}"#.as_slice();
        for (file, body) in ["exit", "launcher", "client"]
            .into_iter()
            .flat_map(|f| [(f, whole), (f, b"{".as_slice())])
        {
            let (l, spec) = gen_setup(1);
            commit(&l, 1, json!({"committed": true}));
            let at = gens::path(&l.folder, 1, file);
            std::fs::write(&at, body).unwrap();
            let before = std::fs::read(&at).unwrap();
            let r = seamed(&l, &spec, true, &[], &mut |told: &mut dyn FnMut(u32)| {
                told(4300);
                Ok(7)
            });
            assert_eq!((r.code, r.started), (1, 0), "{file}");
            assert!(
                r.err[0].contains(&format!("{file}.json")),
                "{file}: {:?}",
                r.err
            );
            assert_eq!(
                r.offered,
                [gens::claim_path(&l.folder, 1)],
                "{file}: nothing after the claim"
            );
            assert_eq!(std::fs::read(&at).unwrap(), before, "{file}: unchanged");
        }
    }

    // Final audit: the exit.json the launcher publishes (nothing pre-seeded) carries an exitedAt timestamp.
    #[test]
    fn the_launcher_exit_json_has_an_exited_at() {
        let (l, spec) = gen_setup(1);
        commit(&l, 1, json!({"committed": true}));
        let at = gens::path(&l.folder, 1, "exit");
        let r = seamed(&l, &spec, true, &[], &mut |_| Ok(7));
        assert_eq!((r.code, r.started), (7, 1));
        assert!(
            r.offered.contains(&at),
            "the exit goes through publish: {:?}",
            r.offered
        );
        let exit = gen_file(&l, "exit");
        assert!(
            exit["exitedAt"].as_str().is_some_and(|s| !s.is_empty()),
            "exitedAt: {exit}"
        );
        assert_eq!(
            (&exit["exitCode"], &exit["reason"]),
            (&json!(7), &json!("exited"))
        );
    }

    // G4 fence on the generation path: a claim already there, whatever it holds, ends the launch before any other
    // file is offered; a dangling link counts as a claim and is never followed.
    #[test]
    fn a_claimed_generation_never_runs_again() {
        for body in [br#""run""#.as_slice(), br#""abandoned""#, b"corrupt", b""] {
            let (l, spec) = gen_setup(1);
            commit(&l, 1, json!({"committed": true}));
            let claim = gens::claim_path(&l.folder, 1);
            std::fs::write(&claim, body).unwrap();
            let r = seamed(&l, &spec, true, &[], &mut |_| Ok(7));
            assert_eq!((r.code, r.started), (0, 0), "{body:?}");
            assert_eq!(
                r.offered,
                std::slice::from_ref(&claim),
                "{body:?}: nothing after the lost claim"
            );
            assert_eq!(std::fs::read(&claim).unwrap(), body);
        }
        #[cfg(unix)]
        {
            let (l, spec) = gen_setup(1);
            commit(&l, 1, json!({"committed": true}));
            let outside = crate::testing::TempDir::new();
            let target = outside.path().join("claim");
            std::os::unix::fs::symlink(&target, gens::claim_path(&l.folder, 1)).unwrap();
            let r = seamed(&l, &spec, true, &[], &mut |_| Ok(7));
            assert_eq!((r.code, r.started), (0, 0), "a dangling link is a claim");
            assert!(!target.exists(), "never followed");
        }
    }

    // A competing launcher while the client runs, and the same spec launched again after it exited: neither starts
    // anything or touches a file.
    #[test]
    fn the_same_generation_runs_once_while_it_runs_and_after() {
        let (l, spec) = gen_setup(1);
        commit(&l, 1, json!({"committed": true}));
        let s = spec.to_str().unwrap();
        let mut inner = None;
        let code = run(&l.w.ctx, s, &mut |_| {}, &mut |_, _, _, _| {
            inner = Some(launch(&l, s, exits_7));
            Ok(7)
        });
        let inner = inner.unwrap();
        assert_eq!(
            (code, inner.code, inner.started.len()),
            (7, 0, 0),
            "the competitor stops at the claim"
        );
        assert_eq!(gens::claim(&l.folder, 1), Evidence::Present(json!("run")));
        let read = |f: &str| std::fs::read(gens::path(&l.folder, 1, f)).unwrap();
        let (exit, launcher) = (read("exit"), read("launcher"));
        let again = launch(&l, s, exits_7);
        assert_eq!(
            (again.code, again.started.len()),
            (0, 0),
            "launched again after the exit"
        );
        assert_eq!((read("exit"), read("launcher")), (exit, launcher));
        assert_eq!(gen_file(&l, "exit")["reason"], "exited");
    }

    // launch-1 G1.2, G12: a spec in gens/<n>/ whose generation matches but which is not this member's launch n is
    // refused before the claim; nothing is created.
    #[test]
    fn a_spec_that_is_not_this_members_launch_is_refused_unclaimed() {
        for case in [
            "spec id",
            "spec generation",
            "spec session",
            "launch hcName",
            "launch session",
            "no launch",
            "torn launch",
            "pending other gen",
            "pre-lifecycle",
            "record id",
            "gen-0 name at gen 1",
            "stale active",
            "torn record",
        ] {
            let (l, spec) = gen_setup(1);
            commit(&l, 1, json!({"committed": true}));
            let (record, at) = (
                l.folder.join("record.json"),
                gens::path(&l.folder, 1, "launch"),
            );
            match case {
                // Another member's valid spec copied here: its own id.
                "spec id" => edit(&spec, |v| v["id"] = json!("bbbbbbbbbbbb")),
                "spec generation" => edit(&spec, |v| v["generation"] = json!("2")),
                "spec session" => edit(&spec, |v| v["sessionId"] = json!("s2")),
                "launch hcName" => edit(&at, |v| v["hcName"] = json!("m00000000")),
                "launch session" => edit(&at, |v| v["sessionId"] = json!("s2")),
                "no launch" => std::fs::remove_file(&at).unwrap(),
                "torn launch" => std::fs::write(&at, b"{").unwrap(),
                "pending other gen" => edit(&record, |r| r["pending"]["gen"] = json!(2)),
                "pre-lifecycle" => edit(&record, |r| {
                    r.as_object_mut().unwrap().remove("lifecycle");
                }),
                "record id" => edit(&record, |r| r["id"] = json!("bbbbbbbbbbbb")),
                // G2: m<id[..8]> is generation 0's name only.
                "gen-0 name at gen 1" => {
                    edit(&record, |r| {
                        r.as_object_mut().unwrap().remove("pending");
                        r["generation"] = json!("1");
                        r["sessionId"] = json!("s1");
                    });
                    edit(&at, |v| v["hcName"] = json!("maaaaaaaa"));
                }
                // Fix cycle 2: no pending, active generation 2 with this name and session: launch 1 is stale.
                "stale active" => edit(&record, |r| {
                    r.as_object_mut().unwrap().remove("pending");
                    r["generation"] = json!("2");
                    r["hcName"] = json!(HC);
                    r["sessionId"] = json!("s1");
                }),
                _ => std::fs::write(&record, b"{").unwrap(),
            }
            let ran = launch(&l, spec.to_str().unwrap(), exits_7);
            assert_eq!((ran.code, ran.started.len()), (1, 0), "{case}");
            assert_eq!(ran.err, [FOREIGN], "{case}");
            let made = [
                gens::claim_path(&l.folder, 1),
                gens::path(&l.folder, 1, "launcher"),
                gens::path(&l.folder, 1, "exit"),
            ];
            for f in made {
                assert!(!f.exists(), "{case}: {}", f.display());
            }
        }
    }

    #[test]
    fn a_promoted_or_generation_zero_launch_is_its_members() {
        // Promoted before the launcher read the record: generation 1 active, with its hcName and session.
        let (l, spec) = gen_setup(1);
        edit(&l.folder.join("record.json"), |r| {
            r.as_object_mut().unwrap().remove("pending");
            r["generation"] = json!("1");
            r["hcName"] = json!(HC);
            r["sessionId"] = json!("s1");
        });
        commit(&l, 1, json!({"committed": true}));
        let ran = launch(&l, spec.to_str().unwrap(), exits_7);
        assert_eq!(
            (ran.code, ran.started.len()),
            (7, 1),
            "promoted: {:?}",
            ran.err
        );
        assert_eq!(gens::claim(&l.folder, 1), Evidence::Present(json!("run")));
        // Generation 0 of an existing member: no hcName; launch.json names m<id[..8]> (G2).
        let (l, spec) = gen_setup(0);
        edit(&l.folder.join("record.json"), |r| {
            r.as_object_mut().unwrap().remove("pending");
            r.as_object_mut().unwrap().remove("hcName");
            r["generation"] = json!("0");
            r["sessionId"] = json!("s1");
        });
        edit(&gens::path(&l.folder, 0, "launch"), |v| {
            v["hcName"] = json!("maaaaaaaa")
        });
        commit(&l, 0, json!({"committed": true}));
        let ran = launch(&l, spec.to_str().unwrap(), exits_7);
        assert_eq!(
            (ran.code, ran.started.len()),
            (7, 1),
            "generation 0: {:?}",
            ran.err
        );
        assert_eq!(gens::claim(&l.folder, 0), Evidence::Present(json!("run")));
    }

    // state-0 S5: under the member folder the generation path reads its record and, through gens::read, only its
    // own spec, launch and commit; never a folder-level file, never another generation's.
    #[test]
    fn a_generation_reads_only_its_own_evidence() {
        let (l, spec) = gen_setup(1);
        commit(&l, 1, json!({"committed": true}));
        let (ran, reads) =
            crate::state::fs::record_reads(|| launch(&l, spec.to_str().unwrap(), exits_7));
        assert_eq!(ran.code, 7, "{:?}", ran.err);
        let mut under: Vec<PathBuf> = reads
            .into_iter()
            .filter(|p| p.starts_with(&l.folder))
            .collect();
        under.sort();
        under.dedup();
        let mut want = vec![l.folder.join("record.json")];
        want.extend(["spec", "launch", "commit"].map(|f| gens::path(&l.folder, 1, f)));
        want.sort();
        assert_eq!(under, want);
    }

    // G2: a name that is not `m` and 8 lowercase hex digits belongs to no launch, even where the record and launch.json agree.
    #[test]
    fn a_launch_whose_hc_name_is_malformed_is_refused_unclaimed() {
        let (l, spec) = gen_setup(1);
        commit(&l, 1, json!({"committed": true}));
        edit(&l.folder.join("record.json"), |r| {
            r["pending"]["hcName"] = json!("m0123abcg")
        });
        edit(&gens::path(&l.folder, 1, "launch"), |v| {
            v["hcName"] = json!("m0123abcg")
        });
        let ran = launch(&l, spec.to_str().unwrap(), exits_7);
        assert_eq!((ran.code, ran.started.len()), (1, 0));
        assert_eq!(ran.err, [FOREIGN]);
        assert!(!gens::claim_path(&l.folder, 1).exists());
    }

    // G4: a client that is not on PATH, or whose start fails, leaves exit.json `client not started`, code 1.
    #[test]
    fn a_generation_whose_client_cannot_start_records_why() {
        for case in ["not on PATH", "start error"] {
            let (l, spec) = gen_setup(1);
            commit(&l, 1, json!({"committed": true}));
            if case == "not on PATH" {
                std::fs::remove_file(l.bin.join(exe_name("claude"))).unwrap();
            }
            let ran = launch(&l, spec.to_str().unwrap(), || {
                Err(io::Error::other("no exec"))
            });
            assert_eq!(
                (ran.code, ran.started.len()),
                (1, usize::from(case == "start error")),
                "{case}"
            );
            let exit = gen_file(&l, "exit");
            assert_eq!(
                (&exit["exitCode"], &exit["reason"]),
                (&json!(1), &json!("client not started")),
                "{case}"
            );
        }
    }

    // Fix cycle 2: a claim whose publication fails (Err, not Exists) runs nothing and offers nothing more.
    #[test]
    fn a_claim_that_cannot_be_published_runs_nothing() {
        let (l, spec) = gen_setup(1);
        commit(&l, 1, json!({"committed": true}));
        let claim = gens::claim_path(&l.folder, 1);
        let r = seamed(&l, &spec, true, &[(claim.clone(), 1)], &mut |_| Ok(7));
        assert_eq!((r.code, r.started), (1, 0));
        assert_eq!(
            r.offered,
            std::slice::from_ref(&claim),
            "nothing after the failed claim"
        );
        assert!(
            r.err[0].contains("injected publication failure"),
            "{:?}",
            r.err
        );
        assert!(!claim.exists());
    }

    // Fix cycle 2, G2: only an absent hcName derives m<id[..8]> at generation 0; null or any non-text value is refused.
    #[test]
    fn a_generation_zero_name_that_is_null_or_not_text_is_refused() {
        for bad in [json!(null), json!(1), json!(["maaaaaaaa"])] {
            let (l, spec) = gen_setup(0);
            edit(&l.folder.join("record.json"), |r| {
                r.as_object_mut().unwrap().remove("pending");
                r["hcName"] = bad.clone();
                r["generation"] = json!("0");
                r["sessionId"] = json!("s1");
            });
            edit(&gens::path(&l.folder, 0, "launch"), |v| {
                v["hcName"] = json!("maaaaaaaa")
            });
            commit(&l, 0, json!({"committed": true}));
            let ran = launch(&l, spec.to_str().unwrap(), exits_7);
            assert_eq!((ran.code, ran.started.len()), (1, 0), "{bad}");
            assert_eq!(ran.err, [FOREIGN], "{bad}");
            assert!(!gens::claim_path(&l.folder, 0).exists(), "{bad}");
        }
    }

    // Point 8: a link at an evidence file is refused unopened: the spec by the canonical-path check of run_seamed
    // (exit 2, LINK); commit, record and launch by the walk of Task 1a step 0 (gens::read, gens::record). One case
    // per file. Unix only.
    #[cfg(unix)]
    #[test]
    fn linked_generation_evidence_runs_nothing() {
        for case in ["spec", "commit", "record", "launch"] {
            let (l, spec) = gen_setup(1);
            commit(&l, 2, json!({"committed": true}));
            let out = crate::testing::TempDir::new();
            let at = match case {
                "spec" => spec.clone(),
                "commit" => gens::path(&l.folder, 1, "commit"),
                "record" => l.folder.join("record.json"),
                _ => gens::path(&l.folder, 1, "launch"),
            };
            let target = if case == "commit" {
                gens::path(&l.folder, 2, "commit")
            } else {
                let copy = out.path().join(format!("{case}.json"));
                std::fs::rename(&at, &copy).unwrap();
                copy
            };
            std::os::unix::fs::symlink(&target, &at).unwrap();
            let (r, reads) =
                crate::state::fs::record_reads(|| seamed(&l, &spec, true, &[], &mut |_| Ok(7)));
            assert_eq!(r.started, 0, "{case}: {:?}", r.err);
            assert!(
                !reads.contains(&target) && !reads.contains(&at),
                "{case}: opened {reads:?}"
            );
            assert!(
                !r.offered.contains(&gens::path(&l.folder, 1, "commit")),
                "{case}: no give-up offered"
            );
            match case {
                "spec" => assert_eq!((r.code, r.err.clone()), (2, vec![LINK.to_string()])),
                "commit" => assert_eq!(
                    (r.code, gen_file(&l, "exit")["reason"].clone()),
                    (1, json!("commit unreadable"))
                ),
                _ => assert_eq!(
                    (r.code, r.err.clone()),
                    (1, vec![FOREIGN.to_string()]),
                    "{case}"
                ),
            }
        }
    }

    // Point 8: a dangling commit.json during the wait reads Unreadable, never Absent: no give-up is offered over it.
    // Unix only.
    #[cfg(unix)]
    #[test]
    fn a_dangling_commit_during_the_wait_is_never_absent() {
        let (l, spec) = gen_setup(1);
        let at = gens::path(&l.folder, 1, "commit");
        std::os::unix::fs::symlink(gens::path(&l.folder, 2, "commit"), &at).unwrap();
        let r = seamed(&l, &spec, true, &[], &mut |_| Ok(7));
        assert_eq!((r.code, r.started), (1, 0));
        assert!(
            !r.offered.contains(&at),
            "no give-up offered over a dangling link: {:?}",
            r.offered
        );
        assert_eq!(gen_file(&l, "exit")["reason"], "commit unreadable");
    }

    // Point 8: a FIFO at commit.json is never opened: the wait reads Unreadable until the deadline and ends
    // `commit unreadable`. Bounded: the launch runs on a thread with a 30 s limit (the fake clock never sleeps for
    // real), so an opened FIFO fails the test instead of hanging it. Unix only.
    #[cfg(unix)]
    #[test]
    fn a_fifo_commit_ends_commit_unreadable_within_the_deadline() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (l, spec) = gen_setup(1);
            let at = gens::path(&l.folder, 1, "commit");
            assert!(
                std::process::Command::new("mkfifo")
                    .arg(&at)
                    .status()
                    .unwrap()
                    .success()
            );
            let r = seamed(&l, &spec, true, &[], &mut |_| Ok(7));
            let _ = tx.send((
                r.code,
                r.started,
                gen_file(&l, "exit")["reason"].clone(),
                r.offered.contains(&at),
            ));
        });
        let got = rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the wait blocked: the FIFO was opened");
        assert_eq!(
            got,
            (1, 0, json!("commit unreadable"), false),
            "no give-up offered over the FIFO"
        );
    }

    // Fix cycle 2, G4: Unreadable at the deadline is `commit unreadable` and publishes nothing, even when the torn file
    // is gone by the time a give-up would be written (removed on the clock read at the deadline).
    #[test]
    fn a_commit_torn_until_the_deadline_is_never_given_up() {
        let (l, spec) = gen_setup(1);
        let at = gens::path(&l.folder, 1, "commit");
        std::fs::write(&at, b"{").unwrap();
        let (base, gone) = (std::cell::Cell::new(None), at.clone());
        l.w.fakes.clock_probe(move |ms| {
            let start = base.get().unwrap_or(ms);
            base.set(Some(start));
            if ms >= start + 30_000 && gone.exists() {
                std::fs::remove_file(&gone).unwrap();
            }
        });
        let r = seamed(&l, &spec, true, &[], &mut |_| Ok(7));
        assert_eq!((r.code, r.started), (1, 0));
        assert_eq!(gen_file(&l, "exit")["reason"], "commit unreadable");
        assert!(
            !r.offered.contains(&at),
            "no give-up offered: {:?}",
            r.offered
        );
        assert!(!at.exists(), "no decision created");
    }

    // Fix cycle 2, G4: Absent at the deadline, and the give-up publication fails (Err): nothing runs, no decision.
    #[test]
    fn a_give_up_that_cannot_be_published_runs_nothing() {
        let (l, spec) = gen_setup(1);
        let at = gens::path(&l.folder, 1, "commit");
        let r = seamed(&l, &spec, true, &[(at.clone(), 1)], &mut |_| Ok(7));
        assert_eq!((r.code, r.started), (1, 0));
        assert_eq!(
            r.offered.iter().filter(|p| **p == at).count(),
            1,
            "one give-up offered"
        );
        assert!(!at.exists(), "no decision created");
        assert_eq!(gen_file(&l, "exit")["reason"], "give-up not published");
    }

    // Fix cycle 2, G4: the give-up publication fails, and a whole commit landed meanwhile: the reread decides.
    #[test]
    fn a_failed_give_up_reads_the_decision_that_won() {
        let (l, spec) = gen_setup(1);
        let at = gens::path(&l.folder, 1, "commit");
        let (base, land) = (std::cell::Cell::new(None), at.clone());
        l.w.fakes.clock_probe(move |ms| {
            let start = base.get().unwrap_or(ms);
            base.set(Some(start));
            if ms >= start + 30_000 && !land.exists() {
                std::fs::write(&land, br#"{"committed":true}"#).unwrap();
            }
        });
        let r = seamed(&l, &spec, true, &[(at.clone(), 1)], &mut |_| Ok(7));
        assert_eq!((r.code, r.started), (7, 1), "{:?}", r.err);
        assert_eq!(
            r.offered.iter().filter(|p| **p == at).count(),
            1,
            "one give-up offered, and it failed"
        );
        assert_eq!(gen_file(&l, "exit")["reason"], "exited");
    }

    // Generation 1, committed; its client 4300 (start 2600) is the one child of the fake launcher 4242 (start 2000):
    // a child starts after its parent (r2b call 26).
    fn gen_client() -> (Launch, PathBuf) {
        let (l, spec) = gen_setup(1);
        commit(&l, 1, json!({"committed": true}));
        l.w.fakes
            .process(crate::testing::row(4300, l.w.ctx.pid, 2600));
        (l, spec)
    }
    fn offers(r: &Seamed, at: &Path) -> usize {
        r.offered.iter().filter(|p| p.as_path() == at).count()
    }
    fn killed(l: &Launch) -> Vec<String> {
        l.w.fakes
            .os_calls()
            .into_iter()
            .filter(|c| c.starts_with("kill"))
            .collect()
    }
    fn reason(e: &Evidence) -> Value {
        match e {
            Evidence::Present(v) => v["reason"].clone(),
            _ => Value::Null,
        }
    }

    // report-2b (PM call 18): with the barrier file, client.json waits for report.json (at most 600 polls of 100 ms)
    // and the launcher marks the order; without it, nothing waits and nothing is marked.
    #[cfg(feature = "live")]
    #[test]
    fn the_live_client_barrier_waits_for_report_json_and_records_the_order() {
        let f = crate::testing::Fakes::default();
        let mut ctx = f.ctx();
        let d = crate::testing::TempDir::new();
        ctx.state_root = d.path().to_path_buf();
        let m = d
            .path()
            .join("0123456789abcdef")
            .join("agents")
            .join("aaaaaaaaaaaa");
        let mark = d.path().join(format!("test-barrier-{}", ctx.pid));
        live_client_barrier(&ctx, &m, 1);
        assert!(
            f.slept().is_empty() && !mark.exists(),
            "no barrier file: no wait, no mark"
        );
        std::fs::write(d.path().join("test-client-barrier"), "").unwrap();
        let m2 = m.clone();
        f.sleep_probe(move |count| {
            if count == 3 {
                crate::state::evidence::publish(&gens::path(&m2, 1, "report"), &json!({})).unwrap();
            }
        });
        live_client_barrier(&ctx, &m, 1);
        let read = || std::fs::read_to_string(&mark).ok();
        assert_eq!(
            (f.slept().len(), read()),
            (3, Some("report-first".to_string()))
        );
        // r2b call 33: client.json already there when the report appears is not report-first.
        crate::state::evidence::publish(&gens::path(&m, 1, "client"), &json!({})).unwrap();
        live_client_barrier(&ctx, &m, 1);
        assert_eq!(
            (f.slept().len(), read()),
            (3, Some("client-first".to_string())),
            "client.json before report.json"
        );
        std::fs::remove_file(gens::path(&m, 1, "client")).unwrap();
        std::fs::remove_file(gens::path(&m, 1, "report")).unwrap();
        live_client_barrier(&ctx, &m, 1);
        assert_eq!(
            (f.slept().len(), read()),
            (603, Some("timeout".to_string())),
            "at most 60 s"
        );
        // r2b call 34: every poll sleeps 100 ms, so 600 polls are 60 s.
        assert_eq!(f.slept(), vec![100; 603], "each barrier sleep is 100 ms");
    }

    // r2b call 33 (review F1): through run_gen's start callback, with the barrier file, client.json stays
    // unpublished while the launcher waits for report.json, and is published once it exists; the mark is report-first.
    #[cfg(feature = "live")]
    #[test]
    fn the_launcher_holds_client_json_until_report_json_exists() {
        let (l, spec) = gen_client();
        let root = l.w.ctx.state_root.clone();
        std::fs::write(root.join("test-client-barrier"), "").unwrap();
        let client = gens::path(&l.folder, 1, "client");
        let report = gens::path(&l.folder, 1, "report");
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let (at, seen2) = (client.clone(), seen.clone());
        l.w.fakes.sleep_probe(move |k| {
            seen2.borrow_mut().push(at.exists());
            if k == 3 {
                evidence::publish(&report, &json!({})).unwrap();
            }
        });
        let mut after = None;
        let r = seamed(&l, &spec, true, &[], &mut |told: &mut dyn FnMut(u32)| {
            told(4300);
            after = Some(gen_file(&l, "client"));
            Ok(7)
        });
        assert_eq!((r.code, r.started), (7, 1), "{:?}", r.err);
        assert_eq!(
            *seen.borrow(),
            [false, false, false],
            "client.json while the launcher waited for report.json"
        );
        assert_eq!(
            after,
            Some(json!({"pid": 4300, "start": 2600})),
            "client.json once report.json exists"
        );
        let mark = std::fs::read_to_string(root.join(format!("test-barrier-{}", l.w.ctx.pid)));
        assert_eq!(mark.ok().as_deref(), Some("report-first"));
    }

    // K1: client.json = {pid, start} of the started client, published after the start; while the client runs,
    // gens::client proves it; launcher.json gains no client field.
    #[test]
    fn the_launcher_publishes_client_json_once_and_no_client_field() {
        let (l, spec) = gen_client();
        let at = gens::path(&l.folder, 1, "client");
        let (mut during, mut proven) = (None, None);
        let r = seamed(&l, &spec, true, &[], &mut |told: &mut dyn FnMut(u32)| {
            assert!(!at.exists(), "client.json before the client started");
            told(4300);
            during = Some(gen_file(&l, "client"));
            proven = gens::client(&l.w.ctx, &l.folder, 1);
            Ok(7)
        });
        assert_eq!((r.code, r.started), (7, 1), "{:?}", r.err);
        assert_eq!(
            during,
            Some(json!({"pid": 4300, "start": 2600})),
            "while the client runs"
        );
        assert_eq!(
            proven,
            Some(gens::Proven {
                launcher: (4242, 2000),
                client: (4300, 2600)
            })
        );
        assert_eq!(offers(&r, &at), 1);
        let launcher = gen_file(&l, "launcher");
        let keys: Vec<&str> = launcher
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["pid", "start", "startedAt"]);
        assert_eq!(gen_file(&l, "exit")["reason"], "exited");
        assert!(matches!(
            gens::read(&l.folder, 1, "client"),
            Evidence::Present(_)
        ));
    }

    // K1, fix cycle 2 (finding 4): client.json already names another process. The client is not stopped and the
    // launcher waits for it; the mark is retried once; the final exit says client-conflict; and while the client runs,
    // gens::client never proves a marked or mismatched foreign file (A2a's rule; redesign 4).
    #[test]
    fn a_client_json_naming_another_process_records_client_conflict() {
        for (case, theirs, mark_fails, earlier) in [
            ("other pid", (4301, 1700), 0, false),
            ("other start", (4300, 1599), 0, false),
            ("mark retried", (4301, 1700), 1, false),
            ("mark fails", (4301, 1700), 2, false),
            ("earlier exit", (4301, 1700), 0, true),
        ] {
            let (l, spec) = gen_client();
            l.w.fakes
                .process(crate::testing::row(4301, l.w.ctx.pid, 1700));
            let at = gens::path(&l.folder, 1, "client");
            let exit = gens::path(&l.folder, 1, "exit");
            let foreign = json!({"pid": theirs.0, "start": theirs.1});
            let fails = if mark_fails > 0 {
                vec![(exit.clone(), mark_fails)]
            } else {
                vec![]
            };
            let (mut mark, mut proven) = (Evidence::Absent, None);
            let r = seamed(&l, &spec, true, &fails, &mut |told: &mut dyn FnMut(u32)| {
                evidence::publish(&at, &foreign).unwrap();
                if earlier {
                    evidence::publish(
                        &exit,
                        &json!({"exitCode": 3, "reason": "exited", "exitedAt": "x"}),
                    )
                    .unwrap();
                }
                told(4300);
                mark = gens::read(&l.folder, 1, "exit");
                proven = gens::client(&l.w.ctx, &l.folder, 1);
                Ok(7)
            });
            assert_eq!(
                (r.code, r.started),
                (7, 1),
                "{case}: the launcher waited for its client"
            );
            assert!(
                killed(&l).is_empty(),
                "{case}: the client is not stopped: {:?}",
                killed(&l)
            );
            let want = match case {
                "mark fails" => Value::Null,
                "earlier exit" => json!("exited"),
                _ => json!("client-conflict"),
            };
            assert_eq!(
                reason(&mark),
                want,
                "{case}: exit.json while the client runs"
            );
            // A2a's rule vetoes through the mark or the start. A foreign live child of the launcher with no mark (mark
            // fails, earlier exit) is outside the threat model: only this user's own processes run there.
            if !matches!(case, "mark fails" | "earlier exit") {
                assert_eq!(
                    proven, None,
                    "{case}: a marked or mismatched client.json is never proven"
                );
            }
            assert_eq!(gen_file(&l, "client"), foreign, "{case}: never replaced");
            let last = if earlier { "exited" } else { "client-conflict" };
            assert_eq!(
                gen_file(&l, "exit")["reason"],
                last,
                "{case}: the final reason"
            );
            if case == "other pid" {
                assert_eq!(
                    gen_file(&l, "exit")["exitCode"],
                    Value::Null,
                    "the mark is written while the client runs"
                );
            }
            assert!(
                r.err.iter().any(|e| e.contains("client-conflict")),
                "{case}: {:?}",
                r.err
            );
            let unmarked = r.err.iter().any(|e| e.contains("could not be marked"));
            assert_eq!(
                unmarked,
                matches!(case, "mark fails" | "earlier exit"),
                "{case}: {:?}",
                r.err
            );
        }
    }

    // K1: one failed publication is retried once; the second attempt records the real client.
    #[test]
    fn a_failed_client_publication_is_retried() {
        let (l, spec) = gen_client();
        let at = gens::path(&l.folder, 1, "client");
        let r = seamed(
            &l,
            &spec,
            true,
            &[(at.clone(), 1)],
            &mut |told: &mut dyn FnMut(u32)| {
                told(4300);
                Ok(7)
            },
        );
        assert_eq!((r.code, r.started), (7, 1), "{:?}", r.err);
        assert_eq!(offers(&r, &at), 2);
        assert_eq!(gen_file(&l, "client"), json!({"pid": 4300, "start": 2600}));
        assert_eq!(gen_file(&l, "exit")["reason"], "exited");
    }

    // K1: every attempt fails: at most CLIENT_ATTEMPTS offers and an error line. The client is not stopped and runs
    // on, unproven and reserved (no client.json); its real exit is recorded, never "never ran".
    #[test]
    fn a_client_json_that_cannot_be_published_records_why() {
        let (l, spec) = gen_client();
        let at = gens::path(&l.folder, 1, "client");
        let r = seamed(
            &l,
            &spec,
            true,
            &[(at.clone(), 5)],
            &mut |told: &mut dyn FnMut(u32)| {
                told(4300);
                Ok(7)
            },
        );
        assert_eq!((r.code, r.started), (7, 1));
        assert_eq!(offers(&r, &at), 2, "bounded: CLIENT_ATTEMPTS");
        assert!(!at.exists(), "unproven: no client.json");
        assert!(
            r.err
                .iter()
                .any(|e| e.contains("client.json could not be published")),
            "{:?}",
            r.err
        );
        let exit = gen_file(&l, "exit");
        assert_eq!(
            (&exit["exitCode"], &exit["reason"]),
            (&json!(7), &json!("exited"))
        );
        assert!(killed(&l).is_empty());
    }

    // K1, fix cycle 2 (finding 5): Exists naming this pid and start is this launcher's own client, whatever other
    // field it carries (K1 allows `loader`): success, not a conflict.
    #[test]
    fn the_same_client_publishing_twice_is_not_a_conflict() {
        for ours in [
            json!({"pid": 4300, "start": 2600}),
            json!({"pid": 4300, "start": 2600, "loader": true}),
        ] {
            let (l, spec) = gen_client();
            let at = gens::path(&l.folder, 1, "client");
            let r = seamed(&l, &spec, true, &[], &mut |told: &mut dyn FnMut(u32)| {
                evidence::publish(&at, &ours).unwrap();
                told(4300);
                Ok(7)
            });
            assert_eq!((r.code, r.started), (7, 1), "{ours}: {:?}", r.err);
            assert_eq!(offers(&r, &at), 1, "{ours}");
            assert_eq!(gen_file(&l, "client"), ours);
            let exit = gen_file(&l, "exit");
            assert_eq!(
                (&exit["exitCode"], &exit["reason"]),
                (&json!(7), &json!("exited")),
                "{ours}"
            );
            assert!(r.err.is_empty(), "{ours}: {:?}", r.err);
        }
    }

    // Redesign 5: once a conflict is seen, a wait error after the start keeps the reason `client-conflict` (both mark
    // attempts fail here); with no conflict it is `client wait failed`. A started client is never `client not started`.
    #[test]
    fn a_wait_error_after_the_start_keeps_the_reason_of_a_started_client() {
        for (conflict, want) in [(true, "client-conflict"), (false, "client wait failed")] {
            let (l, spec) = gen_client();
            let exit = gens::path(&l.folder, 1, "exit");
            if conflict {
                l.w.fakes
                    .process(crate::testing::row(4301, l.w.ctx.pid, 1700));
            }
            let fails = if conflict {
                vec![(exit.clone(), 2)]
            } else {
                vec![]
            };
            let r = seamed(&l, &spec, true, &fails, &mut |told: &mut dyn FnMut(u32)| {
                if conflict {
                    evidence::publish(
                        &gens::path(&l.folder, 1, "client"),
                        &json!({"pid": 4301, "start": 1700}),
                    )
                    .unwrap();
                }
                told(4300);
                Err(io::Error::other("wait failed"))
            });
            assert_eq!((r.code, r.started), (1, 1), "{want}");
            assert_eq!(
                offers(&r, &exit),
                if conflict { 3 } else { 1 },
                "{want}: two failed marks, then the exit"
            );
            assert_eq!(gen_file(&l, "exit")["reason"], want);
        }
    }

    // Point 8: client.json is read back through gens::read, the walk. A link there, even to a copy naming this pid and start,
    // is Unreadable, so a conflict, and is never opened. Unix only.
    #[cfg(unix)]
    #[test]
    fn a_linked_client_json_is_a_conflict() {
        let (l, spec) = gen_client();
        let at = gens::path(&l.folder, 1, "client");
        let out = crate::testing::TempDir::new();
        let copy = out.path().join("client.json");
        std::fs::write(&copy, br#"{"pid":4300,"start":2600}"#).unwrap();
        let (r, reads) = crate::state::fs::record_reads(|| {
            seamed(&l, &spec, true, &[], &mut |told: &mut dyn FnMut(u32)| {
                std::os::unix::fs::symlink(&copy, &at).unwrap();
                told(4300);
                Ok(7)
            })
        });
        assert_eq!((r.code, r.started), (7, 1), "{:?}", r.err);
        assert!(
            !reads.contains(&copy) && !reads.contains(&at),
            "opened {reads:?}"
        );
        assert_eq!(gen_file(&l, "exit")["reason"], "client-conflict");
    }

    #[test]
    #[ignore = "a child of a_kill_after_each_launcher_step_starts_no_second_client"]
    fn child_launcher() {
        let (l, spec) = gen_setup(1);
        // Finding 4-4: A3a `Identity::held` needs a valid session id, and `gen_setup` seeds "s1": one id everywhere.
        let sid = crate::agents::fixture::SESSION_ID;
        edit(&l.folder.join("record.json"), |r| {
            r["pending"]["sessionId"] = json!(sid)
        });
        edit(&gens::path(&l.folder, 1, "spec"), |v| {
            v["sessionId"] = json!(sid)
        });
        // Checklist item 1: a launch identity whose argv `classify` can match (Running needs a nonempty one).
        edit(&gens::path(&l.folder, 1, "launch"), |v| {
            v["sessionId"] = json!(sid);
            v["launcherArgv"] = json!(["/bin/mux-agent", "launcher", spec.to_str().unwrap()]);
        });
        commit(&l, 1, json!({"committed": true}));
        l.w.fakes
            .process(crate::testing::row(4300, l.w.ctx.pid, 1600));
        println!("CRASH-ROOT {}", l.w.ctx.state_root.display());
        println!("CRASH-SPEC {}", spec.display());
        run(
            &l.w.ctx,
            spec.to_str().unwrap(),
            &mut |_| {},
            &mut |_, _, _, started| {
                started(4300);
                Ok(0)
            },
        );
        std::mem::forget(l);
    }

    // settle-1b "Forward crash, replayed", launcher side: the real launcher killed after its claim, launcher.json,
    // client.json or exit.json. A Normal settle promotes generation 1 once from launch.json (Running; Ended after
    // exit.json, whose end it records); a replay asks hc nothing; a second launcher starts no client.
    #[test]
    fn a_launcher_crash_after_each_step_is_settled_once_and_starts_nothing_twice() {
        use crate::agents::core_fixture as cf;
        use crate::agents::core_launch::crash::{killed_at, printed};
        use crate::agents::settle::{Mode, Outcome};
        use crate::agents::team_state::{Scope, Tx};
        for step in ["claim", "launcher", "client", "exit"] {
            let kv = killed_at("launcher::tests::child_launcher", step);
            let (root, spec) = (printed(&kv, "ROOT"), printed(&kv, "SPEC"));
            let member = spec
                .parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .unwrap()
                .to_path_buf();
            let id = member.file_name().unwrap().to_str().unwrap().to_string();
            let mut w = World::human("");
            w.ctx.state_root = root.clone();
            let launch = gens::read(&member, 1, "launch").known().unwrap().unwrap();
            let team = crate::agents::membership::team(&w.caller());
            let ended = step == "exit";
            let (state, reason, code) = if ended {
                ("finished", json!("exited"), json!(0))
            } else {
                ("live", json!(null), json!(null))
            };
            let wire = json!({"address": "t1", "name": launch["hcName"], "argv": launch["launcherArgv"],
                "state": state, "reason": reason, "exit_code": code,
                "labels": {"mux.managed": "1", "mux.team": team, "mux.id": id, "mux.gen": "1", "mux.name": "worker"}});
            let label = format!("mux.id={id}");
            cf::online(&w);
            let terminals = json!({"terminals": [{"address": "t1"}]}).to_string();
            cf::answer(
                &w,
                &[
                    "ls",
                    "--all",
                    "--label",
                    &label,
                    "--label",
                    "mux.gen=1",
                    "--json",
                ],
                &terminals,
            );
            cf::answer(&w, &["get", "t1", "--json"], &wire.to_string());
            let settled = |w: &World| {
                Tx {
                    ctx: &w.ctx,
                    scope: Scope::of(&w.caller()),
                }
                .settle(&id, Mode::Normal)
            };
            assert_eq!(settled(&w).unwrap(), Outcome::Promoted, "{step}");
            let r = crate::state::fs::read_json(&member.join("record.json"))
                .unwrap()
                .unwrap();
            assert_eq!(
                (&r["generation"], r.get("pending"), r.get("completing")),
                (&json!("1"), None, None),
                "{step}"
            );
            assert_eq!(
                gens::read(&member, 1, "ended").known().unwrap().is_some(),
                ended,
                "{step}: ended.json"
            );
            w.fakes.hc_done().unwrap();
            let asked = w.fakes.hc_calls().len();
            assert_eq!(settled(&w).unwrap(), Outcome::Done, "{step}: replay");
            assert_eq!(
                w.fakes.hc_calls().len(),
                asked,
                "{step}: the replay asked hc"
            );
            let mut started = 0;
            let code = run(
                &w.ctx,
                spec.to_str().unwrap(),
                &mut |_| {},
                &mut |_, _, _, _| {
                    started += 1;
                    Ok(0)
                },
            );
            assert_eq!(started, 0, "{step}: exit {code}, a second client");
            let _ = std::fs::remove_dir_all(root.parent().unwrap());
        }
    }

    // Killed after the claim, launcher.json, client.json or exit.json: a second launcher never starts a client.
    #[test]
    fn a_kill_after_each_launcher_step_starts_no_second_client() {
        use crate::agents::core_launch::crash::{killed_at, printed};
        for (i, step) in ["claim", "launcher", "client", "exit"].iter().enumerate() {
            let kv = killed_at("launcher::tests::child_launcher", step);
            let (root, spec) = (printed(&kv, "ROOT"), printed(&kv, "SPEC"));
            let member = spec
                .parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .unwrap()
                .to_path_buf();
            assert_eq!(
                gens::claim(&member, 1),
                Evidence::Present(json!("run")),
                "{step}"
            );
            for (j, file) in ["launcher", "client", "exit"].iter().enumerate() {
                assert_eq!(
                    gens::path(&member, 1, file).is_file(),
                    i > j,
                    "{step}: {file}"
                );
            }
            let mut w = World::human("");
            w.ctx.state_root = root.clone();
            // Finding 4-3: the readers classify what each kill left: pending 1 held, committed, never finished,
            // and the member keeps its slot (exit.json is not a completion marker).
            let gens::Member::Life(g) = gens::member_of(&member) else {
                panic!("{step}: not a lifecycle member")
            };
            let st = g.status().unwrap();
            assert_eq!(
                (st.subject, st.held, st.committed, st.finished),
                (Some(1), true, true, false),
                "{step}"
            );
            let locals = crate::agents::membership::locals(&w.ctx, &w.caller()).unwrap();
            assert_eq!(
                locals.iter().filter(|l| l.held).count(),
                1,
                "{step}: the slot is held"
            );
            let mut started = 0;
            let code = run(
                &w.ctx,
                spec.to_str().unwrap(),
                &mut |_| {},
                &mut |_, _, _, _| {
                    started += 1;
                    Ok(0)
                },
            );
            assert_eq!(
                (code, started),
                (0, 0),
                "{step}: the claim is taken, no second client"
            );
            let _ = std::fs::remove_dir_all(root.parent().unwrap());
        }
    }
}
