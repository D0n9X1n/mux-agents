//! The pane launcher, `mux-agent launcher <spec.json>` (mux-agent-launcher.ps1, MuxAgents.ps1:1593-1671): waits
//! for the launch commit, runs the client once in this pane, records its exit code; only the legacy form posts an `exited` notice.

use std::io;
use std::path::{Component, Path};
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::agents::spawn::{CMDLINE_MAX, client_named};
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

/// Starts the client attached to this pane: its path, its arguments, the variables to take out, and `started`, told the
/// client's pid from the child handle before the wait (#103 D1); its exit code.
pub(crate) type Start<'a> =
    &'a mut dyn FnMut(&Path, &[String], &[&str], &mut dyn FnMut(u32)) -> io::Result<i32>;

/// The pane's process: exit 2 for a spec it will not read, else the client's exit code (1 when it never ran).
pub fn launcher(ctx: &Ctx, spec: &str) -> i32 {
    let mut err = |line: &str| {
        use std::io::Write;
        let _ = writeln!(io::stderr(), "{line}");
    };
    run(ctx, spec, &mut err, &mut |path, args, remove, started| {
        let overrides = launch_overrides(ctx)?;
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

/// The client's environment overrides: the launch names its state root and PATH.
fn launch_overrides(ctx: &Ctx) -> io::Result<Vec<(&'static str, String)>> {
    let root = ctx
        .state_root
        .to_str()
        .ok_or_else(|| io::Error::other("state root is not UTF-8"))?;
    Ok(vec![
        ("MUX_AGENTS_STATE", root.to_string()),
        ("PATH", ctx.env.get("PATH").unwrap_or("").to_string()),
    ])
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
/// finished (#103). Any failed flush stops the launch. Above the state root a folder this user cannot add an entry to
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

/// The exit record a launcher that claimed its generation leaves when it fails before or instead of the client, so
/// the parent gets `exited`, never `lost`.
fn failed_exit(ctx: &Ctx, folder: &Path) {
    let at = utc_o(ctx.clock.now());
    let _ = write_json(
        &folder.join("launcher.json"),
        &json!({"pid": ctx.pid, "exitCode": 1, "exitedAt": at}),
    );
}

fn run_with(
    ctx: &Ctx,
    spec: &str,
    err: &mut dyn FnMut(&str),
    start: Start,
    sync: &mut dyn FnMut(&Path, bool) -> io::Result<()>,
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
    let folder = given.parent().unwrap_or(given);
    let spec_value = match claim_spec(given, folder) {
        Ok(Some(v)) => v,
        Ok(None) => return 0,
        Err(refusal) => {
            err(&format!(
                "mux-agent: the launcher failed: {}",
                refusal.message
            ));
            return 1;
        }
    };
    // A won claim is on disk before the client may run (#103): a failed flush leaves the claim `run` and an exit
    // record, and the client never runs.
    if let Err(e) = flush_claim(&ctx.state_root, folder, sync) {
        err(&format!(
            "mux-agent: the launcher failed: the generation claim could not be flushed to disk: {e}"
        ));
        failed_exit(ctx, folder);
        return 1;
    }
    let code = match launch(ctx, &spec_value, folder, err, start) {
        Ok(code) => code,
        Err(refusal) => {
            err(&format!(
                "mux-agent: the launcher failed: {}",
                refusal.message
            ));
            failed_exit(ctx, folder);
            1
        }
    };
    // The exit notice is reconciliation's, from hc facts, once (#104): never the launcher's.
    code
}

/// The client flags the launch spec records, the exact ones its spawn chose (#103: a person's `up` records its own).
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

/// Claims the spec's generation (`gens/<gen>.claim` = `run`): the spec when this launcher won it, none when another
/// claim (or an abandonment) was there first.
fn claim_spec(spec: &Path, folder: &Path) -> Res<Option<Value>> {
    let v = read_json(spec)?.ok_or_else(|| Refusal::internal("the launch spec is missing"))?;
    let gen_id = match v.get("generation") {
        None => "0",
        Some(Value::String(g)) if crate::agents::core_identity::generation(g) => g,
        _ => return Err(Refusal::internal("invalid launch generation")),
    };
    let gens = folder.join("gens");
    std::fs::create_dir_all(&gens)?;
    let plain = gens
        .to_str()
        .ok_or_else(|| Refusal::internal("invalid generation folder"))?;
    if !same_path(&std::fs::canonicalize(&gens)?, plain, HOST) {
        return Err(Refusal::internal("invalid generation folder"));
    }
    Ok(
        crate::state::fs::claim_json(&gens.join(format!("{gen_id}.claim")), &json!("run"))?
            .then_some(v),
    )
}

fn launch(
    ctx: &Ctx,
    launch: &Value,
    folder: &Path,
    err: &mut dyn FnMut(&str),
    start: Start,
) -> Res<i32> {
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
    let client = launch
        .get("client")
        .and_then(Value::as_str)
        .and_then(client_named);
    match (client, spec_flags(launch)) {
        (None, _) => err("mux-agent: the launch spec names no known client"),
        (Some(_), None) => err("mux-agent: the launch spec names no client flags"),
        (Some(client), Some(mut args)) => {
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
                        let mut client = Value::Null;
                        code = start(&executable, &args, &SCRUB, &mut |pid| {
                            client =
                                json!({"pid": pid, "start": ctx.os.open(pid).map(|o| o.start)});
                            let mut now = record.clone();
                            now.insert("client".into(), client.clone());
                            // A record that cannot be written proves no member: D1 fails closed.
                            let _ = write_json(&exit_path, &Value::Object(now));
                        })
                        .unwrap_or(1);
                        if !client.is_null() {
                            record.insert("client".into(), client);
                        }
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

/// True once the agent is committed; false at once for abort.json. At 30 s the launcher makes commit.json itself as
/// its give-up (`"aborted": true`), so a spawn can no longer commit; if the spawn's commit was there first, that is
/// the answer (#97). commit.json is made once, whole, by whichever comes first.
fn wait_commit(ctx: &Ctx, folder: &Path) -> bool {
    let deadline = ctx.clock.now() + COMMIT_WAIT;
    loop {
        if folder.join("abort.json").is_file() {
            return false;
        }
        if folder.join("commit.json").is_file() {
            return crate::agents::committed(folder);
        }
        if ctx.clock.now() >= deadline {
            let gave_up = crate::state::fs::claim_json(
                &folder.join("commit.json"),
                &json!({"aborted": true, "at": utc_o(ctx.clock.now())}),
            );
            return gave_up.is_ok_and(|made| !made) && crate::agents::committed(folder);
        }
        ctx.clock.sleep(COMMIT_POLL);
    }
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

    #[test]
    fn a_launch_names_its_state_root_and_path() {
        let mut w = World::human("");
        w.ctx.env = Env::from_pairs([("PATH", "client-path")]);
        assert_eq!(
            launch_overrides(&w.ctx).unwrap(),
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

    #[test]
    fn generation_fence() {
        let l = setup("claude", json!([]));
        assert_eq!(launch(&l, &spec_str(&l), exits_7).started.len(), 1);
        let record = std::fs::read(l.folder.join("launcher.json")).unwrap();
        let messages = mail(&l);
        let second = launch(&l, &spec_str(&l), exits_7);
        assert!(second.started.is_empty());
        assert_eq!(second.code, 0);
        assert_eq!(
            std::fs::read(l.folder.join("launcher.json")).unwrap(),
            record
        );
        assert_eq!(mail(&l), messages);
        assert_eq!(
            read_json(&l.folder.join("gens/0.claim")).unwrap(),
            Some(json!("run"))
        );
        for value in [b"\"abandoned\"".as_slice(), b"\"run\"", b"corrupt", b""] {
            let l = setup("claude", json!([]));
            std::fs::create_dir(l.folder.join("gens")).unwrap();
            std::fs::write(l.folder.join("gens/0.claim"), value).unwrap();
            let r = launch(&l, &spec_str(&l), exits_7);
            assert_eq!(r.code, 0);
            assert!(r.started.is_empty());
            assert!(!l.folder.join("launcher.json").exists());
            assert!(mail(&l).is_empty());
            assert_eq!(std::fs::read(l.folder.join("gens/0.claim")).unwrap(), value);
        }
    }

    // #103: a won claim and every folder above it are flushed to disk before the client runs. Any failed flush, at
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
        let l = setup("claude", json!([]));
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
        let l = setup("claude", json!([]));
        let (code, synced, started, err) = run_flushing(&l, true);
        assert_eq!((code, synced.len(), started), (1, 1, 0));
        assert!(err[0].contains("could not be flushed"), "{err:?}");
        assert_eq!(
            read_json(&l.folder.join("gens/0.claim")).unwrap(),
            Some(json!("run"))
        );
        assert_eq!(launcher_json(&l)["exitCode"], 1);
        // A failed flush above the root stops the launch too; the sync it gets for those folders is the one that
        // skips folders the user cannot add to.
        let l = setup("claude", json!([]));
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
        assert_eq!(launcher_json(&l)["exitCode"], 1);
    }

    #[cfg(unix)]
    #[test]
    fn generation_fence_dangling_claim_blocks_without_following_link() {
        let l = setup("claude", json!([]));
        let outside = crate::testing::TempDir::new();
        let target = outside.path().join("claim.json");
        let claim = l.folder.join("gens/0.claim");
        std::fs::create_dir(l.folder.join("gens")).unwrap();
        std::os::unix::fs::symlink(&target, &claim).unwrap();
        assert!(!claim.exists());
        assert!(std::fs::symlink_metadata(&claim).unwrap().is_symlink());
        let r = launch(&l, &spec_str(&l), exits_7);
        assert!(!target.exists());
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
        assert_eq!(r.code, 0);
        assert!(r.started.is_empty());
        assert!(r.err.is_empty());
        assert!(!l.folder.join("launcher.json").exists());
        assert!(mail(&l).is_empty());
        assert_eq!(std::fs::read_link(&claim).unwrap(), target);
    }

    #[test]
    fn generation_adversarial_active() {
        let l = setup("claude", json!([]));
        let mut seen = false;
        let code = run(&l.w.ctx, &spec_str(&l), &mut |_| {}, &mut |_, _, _, _| {
            seen = true;
            let before = std::fs::read(l.folder.join("launcher.json")).unwrap();
            let duplicate = launch(&l, &spec_str(&l), exits_7);
            assert!(duplicate.started.is_empty());
            assert_eq!(
                std::fs::read(l.folder.join("launcher.json")).unwrap(),
                before
            );
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
            let l = setup("claude", json!([]));
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
            let r = launch(&l, &spec_str(&l), exits_7);
            assert_eq!(r.code, 1, "{gen_id}");
            assert!(r.started.is_empty(), "{gen_id}");
            assert_eq!(
                r.err,
                ["mux-agent: the launcher failed: unexpected error (invalid launch generation)"],
                "{gen_id}"
            );
            assert!(!l.folder.join("launcher.json").exists(), "{gen_id}");
            assert!(mail(&l).is_empty(), "{gen_id}");
            assert!(!claim.exists(), "{gen_id}");
            assert!(!l.folder.join("gens").exists(), "{gen_id}");
            #[cfg(windows)]
            assert!(!escaped.exists(), "{gen_id}");
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
            let l = setup("claude", json!([]));
            let mut v = read_json(&l.spec).unwrap().unwrap();
            v["generation"] = gen_id;
            write(&l.spec, v);
            let r = launch(&l, &spec_str(&l), exits_7);
            assert_eq!(r.code, 1);
            assert!(r.started.is_empty());
            assert!(!l.folder.join("launcher.json").exists());
            assert!(mail(&l).is_empty());
            assert!(!l.folder.join("gens").exists());
        }
        let l = setup("claude", json!([]));
        std::fs::write(l.folder.join("gens"), b"not a directory").unwrap();
        assert_eq!(launch(&l, &spec_str(&l), exits_7).code, 1);
        assert!(!l.folder.join("launcher.json").exists());
        assert!(mail(&l).is_empty());
        // Unix-only: creating Windows symlinks requires privileges.
        #[cfg(unix)]
        {
            let l = setup("claude", json!([]));
            let outside = crate::testing::TempDir::new();
            std::os::unix::fs::symlink(outside.path(), l.folder.join("gens")).unwrap();
            let r = launch(&l, &spec_str(&l), exits_7);
            assert_eq!(r.code, 1);
            assert!(r.started.is_empty());
            assert!(!l.folder.join("launcher.json").exists());
            assert!(mail(&l).is_empty());
            assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
        }
    }

    #[test]
    fn generation_adversarial_failed_start() {
        let l = setup("claude", json!([]));
        let mut v = read_json(&l.spec).unwrap().unwrap();
        v["generation"] = json!("g-1");
        write(&l.spec, v);
        assert_eq!(
            launch(&l, &spec_str(&l), || Err(io::Error::other("uncertain"))).code,
            1
        );
        assert_eq!(
            read_json(&l.folder.join("gens/g-1.claim")).unwrap(),
            Some(json!("run"))
        );
        let record = std::fs::read(l.folder.join("launcher.json")).unwrap();
        let messages = mail(&l);
        let duplicate = launch(&l, &spec_str(&l), exits_7);
        assert!(duplicate.started.is_empty());
        assert_eq!(duplicate.code, 0);
        assert_eq!(
            std::fs::read(l.folder.join("launcher.json")).unwrap(),
            record
        );
        assert_eq!(mail(&l), messages);
    }

    #[test]
    fn generation_adversarial_valid() {
        for gen_id in ["a_z-09".to_string(), "x".repeat(64), "0".to_string()] {
            let l = setup("claude", json!([]));
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
        let l = setup("claude", json!([]));
        std::fs::remove_file(l.folder.join("commit.json")).unwrap();
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(ran.code, 1);
        assert!(ran.started.is_empty());
        assert_eq!(l.w.fakes.slept(), vec![250; 120]);
        assert_eq!(launcher_json(&l)["aborted"], true);
        assert!(!crate::agents::committed(&l.folder));
        assert_eq!(
            read_json(&l.folder.join("gens/0.claim")).unwrap(),
            Some(json!("run"))
        );
        let record = std::fs::read(l.folder.join("launcher.json")).unwrap();
        let messages = mail(&l);
        let second = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(second.code, 0);
        assert!(second.started.is_empty());
        assert_eq!(
            std::fs::read(l.folder.join("launcher.json")).unwrap(),
            record
        );
        assert_eq!(mail(&l), messages);
        assert_eq!(
            read_json(&l.folder.join("gens/0.claim")).unwrap(),
            Some(json!("run"))
        );
    }

    #[test]
    fn generation_claim_survives_abort_exit() {
        let l = setup("claude", json!([]));
        std::fs::remove_file(l.folder.join("commit.json")).unwrap();
        write(&l.folder.join("abort.json"), json!({}));
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(ran.code, 1);
        assert!(ran.started.is_empty());
        assert!(l.w.fakes.slept().is_empty());
        assert_eq!(launcher_json(&l)["aborted"], true);
        assert_eq!(
            read_json(&l.folder.join("gens/0.claim")).unwrap(),
            Some(json!("run"))
        );
        let record = std::fs::read(l.folder.join("launcher.json")).unwrap();
        let messages = mail(&l);
        let second = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(second.code, 0);
        assert!(second.started.is_empty());
        assert_eq!(
            std::fs::read(l.folder.join("launcher.json")).unwrap(),
            record
        );
        assert_eq!(mail(&l), messages);
        assert_eq!(
            read_json(&l.folder.join("gens/0.claim")).unwrap(),
            Some(json!("run"))
        );
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
    fn a_timed_out_launcher_makes_commit_json_as_its_give_up_unless_the_spawn_did() {
        // Timed out with no commit: it makes commit.json as its give-up, so a late spawn can no longer commit (#97).
        let l = setup("claude", json!([]));
        std::fs::remove_file(l.folder.join("commit.json")).unwrap();
        assert_eq!(launch(&l, &spec_str(&l), exits_7).code, 1);
        assert_eq!(launcher_json(&l)["aborted"], true);
        assert!(!crate::agents::committed(&l.folder));
        assert!(!crate::state::fs::claim_json(&l.folder.join("commit.json"), &json!({})).unwrap());
        // The spawn commits after the launcher's last look but before its give-up claim: the launcher loses the
        // claim, the spawn's commit stands, and the client starts once.
        let l = setup("claude", json!([]));
        std::fs::remove_file(l.folder.join("commit.json")).unwrap();
        let folder = l.folder.clone();
        let start_ms =
            l.w.ctx
                .clock
                .now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
        l.w.fakes.clock_probe(move |ms| {
            let commit = folder.join("commit.json");
            if ms >= start_ms + 30_000 && !commit.exists() {
                crate::agents::fixture::write(&commit, json!({"committedAt": "x"}));
            }
        });
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!((ran.code, ran.started.len()), (7, 1));
        assert!(crate::agents::committed(&l.folder));
        assert_eq!(
            crate::state::fs::read_json(&l.folder.join("commit.json")).unwrap(),
            Some(json!({"committedAt": "x"}))
        );
        // A give-up already there (another launcher's): no client.
        let l = setup("claude", json!([]));
        write(&l.folder.join("commit.json"), json!({"aborted": true}));
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

    // #103 phase 2a-i: the client runs with the flags its spec records, byte for byte, then its args; a spec with no
    // recorded flags starts nothing rather than fall back to a fixed list.
    #[test]
    fn the_launch_runs_the_flags_its_spec_records() {
        let l = setup("claude", json!(["a"]));
        let spec = l.folder.join("spec.json");
        let mut v: Value = read_json(&spec).unwrap().unwrap();
        v["flags"] = json!(["--model", "x y", "\u{e9}"]);
        write(&spec, v);
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(ran.started[0].1, ["--model", "x y", "\u{e9}", "a"]);
        for flags in [None, Some(json!("--x")), Some(json!(["--x", 1]))] {
            let l = setup("claude", json!(["a"]));
            let spec = l.folder.join("spec.json");
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
        assert!(!l.folder.join("notice.json").exists());
        assert!(mail(&l).is_empty());
    }

    #[test]
    fn launcher_start_error_writes_fallback_exit_record() {
        // The client fails to start after PATH resolution.
        let l = setup("claude", json!([]));
        let ran = launch(&l, &spec_str(&l), || Err(io::Error::other("no exec")));
        assert_eq!(ran.code, 1);
        assert_eq!(ran.started.len(), 1);
        assert_eq!(launcher_json(&l)["exitCode"], 1);
        assert!(!l.folder.join("notice.json").exists());
        assert!(mail(&l).is_empty());
        // A failure after the claim (no args) still leaves the fallback exit record, and no notice.
        let l = setup("claude", json!(null));
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(ran.code, 1);
        assert!(ran.started.is_empty());
        assert_eq!(launcher_json(&l)["exitCode"], 1);
        assert!(!l.folder.join("notice.json").exists());
        assert!(mail(&l).is_empty());
        assert_eq!(
            read_json(&l.folder.join("gens/0.claim")).unwrap(),
            Some(json!("run"))
        );
        // A failure before the claim (malformed spec) writes no record and no notice.
        let l = setup("claude", json!([]));
        std::fs::write(&l.spec, b"{not json").unwrap();
        let ran = launch(&l, &spec_str(&l), exits_7);
        assert_eq!(ran.code, 1);
        assert!(ran.started.is_empty());
        assert_eq!(
            ran.err,
            ["mux-agent: the launcher failed: unexpected error (state file is not JSON)"]
        );
        assert!(!l.folder.join("launcher.json").exists());
        assert!(mail(&l).is_empty());
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
                let l = setup(
                    "claude",
                    if case == "no-args" {
                        Value::Null
                    } else {
                        json!([])
                    },
                );
                if case == "timeout" {
                    std::fs::remove_file(l.folder.join("commit.json")).unwrap();
                }
                if case == "abort" {
                    write(&l.folder.join("abort.json"), json!({}));
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
                assert_eq!(launcher_json(&l)["exitCode"], code);
                assert_eq!(starts, usize::from(["exit", "start-error"].contains(&case)));
                assert!(!l.folder.join("notice.json").exists());
                assert!(mail(&l).is_empty());
                assert_eq!(
                    read_json(&l.folder.join("gens/0.claim")).unwrap(),
                    Some(json!("run"))
                );
                let before = std::fs::read(l.folder.join("launcher.json")).unwrap();
                assert_eq!(
                    run(
                        &l.w.ctx,
                        &spec_str(&l),
                        &mut |_| {},
                        &mut |_, _, _, _| panic!("duplicate")
                    ),
                    0
                );
                assert_eq!(
                    std::fs::read(l.folder.join("launcher.json")).unwrap(),
                    before
                );
                l.w.fakes.hc_done().unwrap();
            }
        }
    }

    #[test]
    fn the_launcher_records_its_client_before_it_waits() {
        let l = setup("claude", json!([]));
        l.w.fakes
            .process(crate::testing::row(4300, l.w.ctx.pid, 1600));
        let mut seen = Value::Null;
        let code = run(
            &l.w.ctx,
            &spec_str(&l),
            &mut |_| {},
            &mut |_, _, _, started| {
                started(4300);
                seen = launcher_json(&l)["client"].clone();
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
            launcher_json(&l)["client"],
            json!({"pid": 4300, "start": 1600}),
            "kept after the exit"
        );
    }
}
