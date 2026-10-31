//! The pane launcher, `mux-agent launcher <spec.json>` (mux-agent-launcher.ps1, MuxAgents.ps1:1593-1671): waits
//! for the launch commit, runs the client once in this pane, records its exit code and posts one `exited` notice.

use std::io;
use std::path::{Component, Path};
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::agents::registry::{Notice, add_notice};
use crate::agents::spawn::{CMDLINE_MAX, client_flags, client_named};
use crate::agents::{lock, lock_key};
use crate::caller::{Caller, Kind};
use crate::ctx::Ctx;
use crate::error::{Refusal, Res};
use crate::parse::cmdline_length;
use crate::state::fs::{read_json, write_json};
use crate::state::root::{HOST, OsKind};
use crate::util::utc_o;

/// The parent's client identity, taken out of the client's environment (MuxAgents.ps1:1595-1596).
pub const SCRUB: [&str; 12] = [
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
];

const COMMIT_WAIT: Duration = Duration::from_secs(30);
const COMMIT_POLL: Duration = Duration::from_millis(250);
const OUTSIDE: &str = "mux-agent: the launch spec is outside the state folder";
const LINK: &str = "mux-agent: the launch spec path has a junction or link";

/// Starts the client attached to this pane: its path, its arguments, the variables to take out; its exit code.
pub(crate) type Start<'a> = &'a mut dyn FnMut(&Path, &[String], &[&str]) -> io::Result<i32>;

/// The pane's process: exit 2 for a spec it will not read, else the client's exit code (1 when it never ran).
pub fn launcher(ctx: &Ctx, spec: &str) -> i32 {
    let mut err = |line: &str| {
        use std::io::Write;
        let _ = writeln!(io::stderr(), "{line}");
    };
    run(ctx, spec, &mut err, &mut |path, args, remove| {
        crate::spawn::attached(path, args, remove)
    })
}

pub(crate) fn run(ctx: &Ctx, spec: &str, err: &mut dyn FnMut(&str), start: Start) -> i32 {
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
    let folder = given.parent().unwrap_or(given);
    let code = match launch(ctx, given, folder, err, start) {
        Ok(code) => code,
        Err(refusal) => {
            // Any launcher failure still leaves an exit record, so the parent gets `exited`, never `lost`.
            err(&format!(
                "mux-agent: the launcher failed: {}",
                refusal.message
            ));
            let at = utc_o(ctx.clock.now());
            let _ = write_json(
                &folder.join("launcher.json"),
                &json!({"pid": ctx.pid, "exitCode": 1, "exitedAt": at}),
            );
            1
        }
    };
    // The exit notice for the parent; a stop that already posted one wins.
    let _ = post_exited(ctx, folder, code);
    code
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

fn launch(
    ctx: &Ctx,
    spec: &Path,
    folder: &Path,
    err: &mut dyn FnMut(&str),
    start: Start,
) -> Res<i32> {
    let launch = read_json(spec)?.ok_or_else(|| Refusal::internal("the launch spec is missing"))?;
    let exit_path = folder.join("launcher.json");
    // pid and start name this pane's own process, so stop can tell it from a reused pid.
    let mut record = Map::new();
    record.insert("pid".into(), ctx.pid.into());
    let own_start = ctx.os.open(ctx.pid).map(|opened| opened.start);
    record.insert("start".into(), own_start.map_or(Value::Null, Value::from));
    record.insert("startedAt".into(), utc_o(ctx.clock.now()).into());
    write_json(&exit_path, &Value::Object(record.clone()))?;
    if !wait_commit(ctx, folder) {
        record.insert("aborted".into(), true.into());
        record.insert("exitCode".into(), 1.into());
        write_json(&exit_path, &Value::Object(record))?;
        return Ok(1);
    }
    let mut code = 1;
    match launch
        .get("client")
        .and_then(Value::as_str)
        .and_then(client_named)
    {
        None => err("mux-agent: the launch spec names no known client"),
        Some(client) => {
            let mut args: Vec<String> =
                client_flags(client).iter().map(|f| f.to_string()).collect();
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
            let found = ctx
                .env
                .get("PATH")
                .and_then(|path| crate::mux::find_on_path(&image, path));
            match found {
                None => err(&format!("mux-agent: {image} is not on PATH")),
                Some(executable) => {
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
                    } else {
                        // Attached, so the client owns the pane's console; a start that fails is exit 1.
                        code = start(&executable, &args, &SCRUB).unwrap_or(1);
                    }
                }
            }
        }
    }
    record.insert("exitCode".into(), code.into());
    record.insert("exitedAt".into(), utc_o(ctx.clock.now()).into());
    write_json(&exit_path, &Value::Object(record))?;
    Ok(code)
}

/// True once commit.json exists; false at once for abort.json, or after 30 s with neither.
fn wait_commit(ctx: &Ctx, folder: &Path) -> bool {
    let deadline = ctx.clock.now() + COMMIT_WAIT;
    loop {
        if folder.join("abort.json").is_file() {
            return false;
        }
        if folder.join("commit.json").is_file() {
            return true;
        }
        if ctx.clock.now() >= deadline {
            return false;
        }
        ctx.clock.sleep(COMMIT_POLL);
    }
}

/// Posts `exited` with `code` under the agent's notice lock, for a spec at
/// `<root>/<serverKey>/agents/<id>/spec.json` whose record names that id and whose server.json is there.
fn post_exited(ctx: &Ctx, folder: &Path, code: i32) -> Res<()> {
    let (Some(agents), Some(id)) = (folder.parent(), folder.file_name().and_then(|n| n.to_str()))
    else {
        return Ok(());
    };
    let Some(server) = agents.parent() else {
        return Ok(());
    };
    let Some(server_key) = server.file_name().and_then(|n| n.to_str()) else {
        return Ok(());
    };
    if server.parent() != Some(ctx.state_root.as_path())
        || agents.file_name() != Some("agents".as_ref())
    {
        return Ok(());
    }
    let (Some(record), Some(_)) = (
        read_json(&folder.join("record.json"))?,
        read_json(&server.join("server.json"))?,
    ) else {
        return Ok(());
    };
    if record.get("id").and_then(Value::as_str) != Some(id) {
        return Ok(());
    }
    // Only the server key and session reach the notice: its folder, mailbox and lock.
    let caller = Caller {
        kind: Kind::Human,
        client: None,
        agent: None,
        chain: Vec::new(),
        pane: String::new(),
        window: String::new(),
        session: crate::agents::text(record.get("session")),
        server_pid: 0,
        server_start: 0,
        socket: String::new(),
        server_key: server_key.to_string(),
        agent_tag: String::new(),
    };
    lock(ctx, &lock_key("notice", &caller, id), || {
        add_notice(ctx, &caller, id, &record, Notice::Exited(code.into())).map(|_| ())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::fixture::{CALLER, ID, World, write};
    use crate::ctx::Env;
    use crate::parse::cmdline_length;
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
            &mut |path: &Path, args: &[String], remove: &[&str]| {
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

    fn launcher_json(l: &Launch) -> Value {
        serde_json::from_slice(&std::fs::read(l.folder.join("launcher.json")).unwrap()).unwrap()
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

    fn notice_kind(l: &Launch) -> Value {
        let notice: Value =
            serde_json::from_slice(&std::fs::read(l.folder.join("notice.json")).unwrap()).unwrap();
        notice["kind"].clone()
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

    #[test]
    fn abort_json_ends_1_without_client() {
        let l = setup("claude", json!(["--session-id", "s", "p"]));
        std::fs::remove_file(l.folder.join("commit.json")).unwrap();
        write(
            &l.folder.join("abort.json"),
            json!({"abortedAt": "x", "pane": null}),
        );
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(ran.code, 1);
        assert!(ran.started.is_empty() && ran.err.is_empty());
        assert_eq!(
            launcher_json(&l),
            json!({"pid": CALLER, "start": 2000, "startedAt": "1970-01-01T00:00:00.0000000Z",
                "aborted": true, "exitCode": 1})
        );
        // abort.json wins even beside commit.json, and ends the wait at once.
        let l = setup("claude", json!([]));
        write(&l.folder.join("abort.json"), json!({}));
        assert_eq!(launch(&l, &spec_str(&l), exits_7).code, 1);
        assert!(l.w.fakes.slept().is_empty());
    }

    #[test]
    fn commit_timeout_30s() {
        let l = setup("claude", json!([]));
        std::fs::remove_file(l.folder.join("commit.json")).unwrap();
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(ran.code, 1);
        assert!(ran.started.is_empty());
        assert_eq!(l.w.fakes.slept(), vec![250; 120]);
        assert_eq!(launcher_json(&l)["aborted"], true);
        assert_eq!(launcher_json(&l)["exitCode"], 1);
        // A commit already there needs no wait.
        let l = setup("claude", json!([]));
        assert_eq!(launch(&l, &spec_str(&l), exits_7).code, 7);
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
            ]
        );
        let l = setup("copilot", json!(["-i", "p"]));
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
            let l = setup(client, json!(["--session-id", "s", "two words"]));
            let ran = launch(&l, &spec_str(&l), exits_7);
            assert_eq!(ran.code, 7, "{client}");
            let (path, args, _) = &ran.started[0];
            assert_eq!(path, &l.bin.join(exe_name(client)));
            let mut expected: Vec<String> = flags.iter().map(|f| f.to_string()).collect();
            expected.extend(["--session-id", "s", "two words"].map(String::from));
            assert_eq!(args, &expected);
            let record = launcher_json(&l);
            assert_eq!(
                record,
                json!({"pid": CALLER, "start": 2000, "startedAt": "1970-01-01T00:00:00.0000000Z",
                    "exitCode": 7, "exitedAt": "1970-01-01T00:00:00.0000000Z"})
            );
        }
        // An unknown client starts nothing.
        let l = setup("Claude", json!([]));
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

    #[test]
    fn client_not_on_path_exit_1_launcher_json() {
        let l = setup("claude", json!([]));
        std::fs::remove_file(l.bin.join(exe_name("claude"))).unwrap();
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(ran.code, 1);
        assert_eq!(
            ran.err,
            [format!("mux-agent: {} is not on PATH", exe_name("claude"))]
        );
        assert!(ran.started.is_empty());
        assert_eq!(launcher_json(&l)["exitCode"], 1);
        assert_eq!(notice_kind(&l), "exited");
        assert_eq!(mail(&l)[0]["body"], "worker exited with code 1.");
    }

    #[test]
    fn launcher_start_error_writes_fallback_exit_record() {
        // The client fails to start after PATH resolution.
        let l = setup("claude", json!([]));
        let ran = launch(&l, &spec_str(&l), || Err(io::Error::other("no exec")));
        assert_eq!(ran.code, 1);
        assert_eq!(ran.started.len(), 1);
        assert_eq!(launcher_json(&l)["exitCode"], 1);
        assert_eq!(notice_kind(&l), "exited");
        assert_eq!(mail(&l).len(), 1);
        // A launcher error before the normal completion write: an exit record is still left.
        let l = setup("claude", json!([]));
        std::fs::write(&l.spec, b"{not json").unwrap();
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(ran.code, 1);
        assert!(ran.started.is_empty());
        assert_eq!(
            ran.err,
            ["mux-agent: the launcher failed: unexpected error (state file is not JSON)"]
        );
        assert_eq!(
            launcher_json(&l),
            json!({"pid": CALLER, "exitCode": 1, "exitedAt": "1970-01-01T00:00:00.0000000Z"})
        );
        assert_eq!(notice_kind(&l), "exited");
        let messages = mail(&l);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["body"], "worker exited with code 1.");
        // Never lost afterwards: the pane is gone, and the sweep finds the notice already posted.
        l.w.fakes.mux_first("#{pane_id}|#{@agent-id}", 0, "|\n", "");
        crate::agents::registry::update_notices(&l.w.ctx, &l.w.caller(), Some(&[])).unwrap();
        assert_eq!(mail(&l).len(), 1);
        assert_eq!(notice_kind(&l), "exited");
    }

    #[test]
    fn launcher_cmdline_over_32767_with_resolved_client_path_refuses() {
        let probe = setup("claude", json!([]));
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
        let l = setup("claude", json!(["x".repeat(fit)]));
        assert_eq!(launch(&l, &spec_str(&l), exits_7).started.len(), 1);
        let l = setup("claude", json!(["x".repeat(fit + 1)]));
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(ran.code, 1);
        assert!(ran.started.is_empty());
        assert_eq!(
            ran.err,
            ["mux-agent: the client command line is too long (32768 characters)"]
        );
        assert_eq!(launcher_json(&l)["exitCode"], 1);
    }

    #[test]
    fn exited_notice_unless_stopped() {
        let l = setup("claude", json!([]));
        assert_eq!(launch(&l, &spec_str(&l), exits_7).code, 7);
        assert_eq!(notice_kind(&l), "exited");
        let messages = mail(&l);
        assert_eq!(messages.len(), 1);
        assert_eq!(
            (
                &messages[0]["id"],
                &messages[0]["from"],
                &messages[0]["body"]
            ),
            (
                &json!(format!("exit-{ID}")),
                &json!("worker"),
                &json!("worker exited with code 7.")
            )
        );
        assert_eq!(
            l.w.fakes.locks().first().map(String::as_str),
            Some(format!("+notice|{}|{ID}", l.w.caller().server_key).as_str())
        );
        // A stop that already posted its notice wins.
        let l = setup("claude", json!([]));
        write(
            &l.folder.join("notice.json"),
            json!({"kind": "stopped", "exitCode": null, "at": "x"}),
        );
        assert_eq!(launch(&l, &spec_str(&l), exits_7).code, 7);
        assert_eq!(notice_kind(&l), "stopped");
        assert!(mail(&l).is_empty());
    }
}
