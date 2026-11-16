//! The live tests' stand-in for claude and copilot: the harness copies this binary to `<dir>/bin/claude` and
//! `<dir>/bin/copilot`, first on PATH, so a pane that starts a client starts this.
//!
//! Environment:
//! - `MUX_IT_DIR` (required): where every file below goes. Without it the client exits 2 at once.
//! - `MUX_IT_ENTRY`: the mux-agent binary command lines run (default `mux-agent` on PATH).
//! - `MUX_IT_EXIT`: the exit code on quit (default 0).
//! - `<MUX_IT_DIR>/resume-gone` (outcome-2 O5): `--resume <id>` prints the client's own "no such conversation" line
//!   on stderr (by this file's stem: `claude` or `copilot`) and exits 1 before anything is recorded.
//!
//! Protocol, all under `$MUX_IT_DIR`; every file the client writes appears whole (written aside, then renamed):
//! - On start: `client.<pid>.json` = `{"pid", "ppid", "exe", "argv", "sessionId", "worker", "cwd", "tty": {"stdin",
//!   "stdout"}, "env": {…}}`, then `<pid>` appended to `clients.log`. `exe` is this file's name (`claude`), `argv`
//!   the arguments after it, `sessionId` the `--session-id` value or null, `worker` whether one was given (a spawned
//!   agent) and `env` the variables in `ENV` (null when unset). `ppid` is null on Windows. Then, with a session id
//!   (`--session-id` or `--resume`), `report -Session <sid> -Source startup` through `MUX_IT_ENTRY` on a thread, its
//!   stdout written whole to `session.<pid>.json` (report-2b K3, as a client's hook).
//! - Worker mode: a worker runs each line of `worker.cmd`, if present at start, as `worker.<pid>.<n>.{json,err,code}`
//!   (n from 1), e.g. `send\tparent\t-BodyFile\t{dir}/reply.md` or `status\t-Body\tbusy`.
//! - Commands: the client polls `cmd.<pid>`; it takes the file (renames it to `cmd.<pid>.taken.<n>`) and runs each
//!   line, writing `reply.<pid>.<n>.err`, `reply.<pid>.<n>.code`, then `reply.<pid>.<n>.json` (stdout) last, so the
//!   json's appearance means all three are there. n counts taken files from 1; a file of several lines answers in
//!   the files of its own n, the last line's output winning.
//! - A command line is mux-agent's arguments split on tabs; `{pid}`, `{sid}` and `{dir}` in them are replaced by
//!   this client's pid, session id and `$MUX_IT_DIR`. It runs with stdin closed, in the client's cwd and env.
//! - The command line `@detach` (Unix) runs no mux-agent: it starts `/bin/sleep 600` as this client's child in a session
//!   of its own with SIGHUP ignored, so a pane's hangup never reaches it, and answers `{"pid": <its pid>}`. It is a
//!   descendant that outlives its pane; whoever starts one kills it.
//! - The command line `@say\t<text>` runs no mux-agent: it prints `<text>` and a newline on the client's own stdout (its
//!   terminal, which hc keeps) and answers `{"ok": true}`. The client prints nothing else, at start or after.
//! - The command line `@sleep\t<ms>` runs no mux-agent: it sleeps `<ms>` and answers `{"slept": <ms>}`. A line
//!   `@bg <line>` runs `<line>` on a thread beside the loop, answering in the files of its own n (#103).
//! - Each line read on stdin (what `send` types into the pane) is appended to `input.<pid>.log`.
//! - Quit: `quit` (all clients) or `quit.<pid>` appearing makes the client write `exited.<pid>` (its exit code) and
//!   exit with numeric `quit.<pid>` contents overriding `MUX_IT_EXIT`; empty, invalid or missing contents keep it.

use std::ffi::OsString;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{Map, Value, json};

const POLL: Duration = Duration::from_millis(50);

// outcome-2 call 23: this client's own children are bounded as the live harness's are.
#[path = "../../tests/support/bounded.rs"]
mod bounded;

// The variables a client record carries; never the whole environment.
const ENV: [&str; 11] = [
    "MUX_AGENTS_STATE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_ENTRYPOINT",
    "COPILOT_AGENT_SESSION_ID",
    "TMUX",
    "TMUX_PANE",
    "RMUX",
    "HOME",
    "MUX_IT_EXIT",
    "HC_TERMINAL_ID",
    "MUX_AGENTS_GEN",
];

/// O5 (verified 2026-11-14): what the real client prints for `--resume` of a conversation it does not have.
fn gone_message(stem: &str, id: &str) -> String {
    if stem == "copilot" {
        format!("Error: No session, task, or name matched '{id}'.")
    } else {
        format!("No conversation found with session ID: {id}")
    }
}

fn main() {
    let Some(dir) = std::env::var_os("MUX_IT_DIR").map(PathBuf::from) else {
        eprintln!("fake-client: MUX_IT_DIR is not set");
        std::process::exit(2);
    };
    let pid = std::process::id();
    let argv: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if dir.join("resume-gone").exists()
        && let Some(id) = argv
            .iter()
            .position(|a| a == "--resume")
            .and_then(|i| argv.get(i + 1))
    {
        let me = std::env::current_exe().ok();
        let stem = me
            .as_deref()
            .and_then(Path::file_stem)
            .and_then(|s| s.to_str())
            .unwrap_or("claude");
        eprintln!("{}", gone_message(stem, id));
        std::process::exit(1);
    }
    let sid = argv
        .iter()
        .position(|a| a == "--session-id")
        .and_then(|i| argv.get(i + 1))
        .cloned();
    let entry = std::env::var_os("MUX_IT_ENTRY").unwrap_or_else(|| OsString::from("mux-agent"));
    let code: i32 = std::env::var("MUX_IT_EXIT")
        .ok()
        .and_then(|c| c.trim().parse().ok())
        .unwrap_or(0);

    let record = json!({
        "pid": pid,
        "ppid": ppid(),
        "exe": exe_name(),
        "argv": argv,
        "sessionId": sid,
        "worker": sid.is_some(),
        "cwd": std::env::current_dir().ok().map(|d| d.display().to_string()),
        "tty": {"stdin": std::io::stdin().is_terminal(), "stdout": std::io::stdout().is_terminal()},
        "env": ENV.iter().map(|n| (n.to_string(), std::env::var(n).map_or(Value::Null, Value::from))).collect::<Map<_, _>>(),
    });
    write_whole(&dir.join(format!("client.{pid}.json")), record.to_string());
    append(&dir.join("clients.log"), &format!("{pid}\n"));

    // report-2b K3: a client's session-start hook. On a thread: the verb waits up to 10 s for client.json.
    let resumed = argv
        .iter()
        .position(|a| a == "--resume")
        .and_then(|i| argv.get(i + 1))
        .cloned();
    if let Some(sid) = sid.clone().or(resumed) {
        let (entry, dir) = (entry.clone(), dir.clone());
        std::thread::spawn(move || {
            // o2 call 23: this client's own children are bounded (the subprocess pin).
            let out = bounded::output(
                Command::new(&entry)
                    .args(["report", "-Session", &sid, "-Source", "startup"])
                    .stdin(Stdio::null()),
                bounded::LIMIT,
            );
            if let Ok(out) = out {
                write_whole(&dir.join(format!("session.{pid}.json")), out.stdout);
            }
        });
    }

    let runner = Runner {
        dir: dir.clone(),
        pid,
        sid: sid.unwrap_or_default(),
        entry,
    };
    if record["worker"] == true
        && let Ok(text) = std::fs::read_to_string(dir.join("worker.cmd"))
    {
        for (n, line) in text.lines().filter(|l| !l.is_empty()).enumerate() {
            runner.run(line, &format!("worker.{pid}.{}", n + 1));
        }
    }

    let input = dir.join(format!("input.{pid}.log"));
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            append(&input, &format!("{}\n", line.trim_end_matches('\r')));
        }
    });

    let mut taken = 0;
    loop {
        if dir.join("quit").exists() || dir.join(format!("quit.{pid}")).exists() {
            let code = quit_code(&dir, pid, code);
            write_whole(&dir.join(format!("exited.{pid}")), format!("{code}\n"));
            std::process::exit(code);
        }
        let cmd = dir.join(format!("cmd.{pid}"));
        let held = dir.join(format!("cmd.{pid}.taken.{}", taken + 1));
        if std::fs::rename(&cmd, &held).is_ok() {
            taken += 1;
            let text = std::fs::read_to_string(&held).unwrap_or_default();
            for line in text.lines().filter(|l| !l.is_empty()) {
                let base = format!("reply.{pid}.{taken}");
                // `@bg <line>`: run beside the loop, so a long read -Wait does not block the next command (#103).
                if let Some(rest) = line.strip_prefix("@bg ") {
                    let (runner, rest) = (runner.clone(), rest.to_string());
                    std::thread::spawn(move || runner.run(&rest, &base));
                } else {
                    runner.run(line, &base);
                }
            }
        }
        std::thread::sleep(POLL);
    }
}

fn quit_code(dir: &Path, pid: u32, fallback: i32) -> i32 {
    std::fs::read_to_string(dir.join(format!("quit.{pid}")))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(fallback)
}

#[derive(Clone)]
struct Runner {
    dir: PathBuf,
    pid: u32,
    sid: String,
    entry: OsString,
}

impl Runner {
    /// Runs one command line and writes `<base>.err`, `<base>.code`, then `<base>.json`.
    fn run(&self, line: &str, base: &str) {
        let dir = self.dir.display().to_string();
        let args: Vec<String> = line
            .split('\t')
            .map(|a| {
                a.replace("{pid}", &self.pid.to_string())
                    .replace("{sid}", &self.sid)
                    .replace("{dir}", &dir)
            })
            .collect();
        let (stdout, stderr, code) = if let [say_cmd, text] = args.as_slice()
            && say_cmd == "@say"
        {
            match say(&mut std::io::stdout().lock(), text) {
                Ok(()) => (json!({"ok": true}).to_string().into_bytes(), Vec::new(), 0),
                Err(e) => (Vec::new(), format!("fake-client: {e}\n").into_bytes(), -1),
            }
        } else if args.len() == 2 && args[0] == "@sleep" {
            // Live harness support (#103): a slow command with no mux-agent, for background-line tests.
            let ms: u64 = args[1].parse().unwrap_or(0);
            std::thread::sleep(std::time::Duration::from_millis(ms));
            (json!({"slept": ms}).to_string().into_bytes(), Vec::new(), 0)
        } else if args == ["@detach"] {
            match detach() {
                Ok(pid) => (json!({"pid": pid}).to_string().into_bytes(), Vec::new(), 0),
                Err(e) => (Vec::new(), format!("fake-client: {e}\n").into_bytes(), -1),
            }
        } else {
            let out = bounded::output(
                Command::new(&self.entry).args(&args).stdin(Stdio::null()),
                bounded::LIMIT,
            );
            match out {
                Ok(o) => (o.stdout, o.stderr, o.status.code().unwrap_or(-1)),
                Err(e) => (Vec::new(), format!("fake-client: {e}\n").into_bytes(), -1),
            }
        };
        let file = |ext: &str| self.dir.join(format!("{base}.{ext}"));
        write_whole(&file("err"), stderr);
        write_whole(&file("code"), format!("{code}\n"));
        write_whole(&file("json"), stdout);
    }
}

/// `/bin/sleep 600` as this client's child, in its own session (no controlling terminal, so no hangup from the pane)
/// with SIGHUP ignored besides; its pid. Never waited for: it outlives this client.
#[cfg(unix)]
fn detach() -> std::io::Result<u32> {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new("/bin/sleep");
    command
        .arg("600")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid and signal are async-signal-safe, and nothing else runs between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            Ok(())
        });
    }
    // Its own `sleep 600` is its end; the watchdog bounds it to the same while this client runs.
    Ok(bounded::spawn(&mut command, Duration::from_secs(600))?.id())
}

#[cfg(not(unix))]
fn detach() -> std::io::Result<u32> {
    Err(std::io::Error::other("@detach is Unix only"))
}

/// `@say`: `text` and a newline on `out` (the client's own stdout, its terminal), flushed, so the terminal keeps it.
fn say(out: &mut impl Write, text: &str) -> std::io::Result<()> {
    writeln!(out, "{text}")?;
    out.flush()
}

/// This program's file name without extension, as started (`claude`, `copilot`).
fn exe_name() -> String {
    let path = std::env::current_exe()
        .ok()
        .or_else(|| std::env::args_os().next().map(PathBuf::from));
    path.as_deref()
        .and_then(Path::file_stem)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(unix)]
fn ppid() -> Option<u32> {
    Some(std::os::unix::process::parent_id())
}

#[cfg(not(unix))]
fn ppid() -> Option<u32> {
    None
}

/// Writes `path` whole: a sibling file renamed into place.
fn write_whole(path: &Path, bytes: impl AsRef<[u8]>) {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.part", std::process::id()));
    let tmp = PathBuf::from(tmp);
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// One write per call, so lines from several clients do not interleave.
fn append(path: &Path, text: &str) {
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(text.as_bytes());
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn client_record_includes_core_terminal_address() {
        assert!(super::ENV.contains(&"HC_TERMINAL_ID"));
    }

    #[test]
    fn quit_file_owns_exit_code() {
        let d = std::env::temp_dir().join(format!("mux-quit-{}", std::process::id()));
        std::fs::create_dir(&d).unwrap();
        assert_eq!(super::quit_code(&d, 7, 3), 3);
        for (text, want) in [("7\n", 7), ("", 3), ("invalid", 3)] {
            std::fs::write(d.join("quit.7"), text).unwrap();
            assert_eq!(super::quit_code(&d, 7, 3), want);
        }
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn say_prints_its_text_and_a_newline() {
        let mut out = Vec::new();
        super::say(&mut out, "grand-marker-1").unwrap();
        super::say(&mut out, "second line").unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "grand-marker-1\nsecond line\n"
        );
    }

    // O5 (verified 2026-11-14): each client's own words for a conversation it does not have.
    #[test]
    fn a_gone_conversation_has_each_clients_message() {
        let id = "7d1c0e5a-9b1f-4d0e-8f7a-3c2b1a0e9d8c";
        assert_eq!(
            super::gone_message("claude", id),
            format!("No conversation found with session ID: {id}")
        );
        assert_eq!(
            super::gone_message("copilot", id),
            format!("Error: No session, task, or name matched '{id}'.")
        );
    }
}
