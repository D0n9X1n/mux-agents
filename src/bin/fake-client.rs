//! The live tests' stand-in for claude and copilot: the harness copies this binary to `<dir>/bin/claude` and
//! `<dir>/bin/copilot`, first on PATH, so a pane that starts a client starts this.
//!
//! Environment:
//! - `MUX_IT_DIR` (required): where every file below goes. Without it the client exits 2 at once.
//! - `MUX_IT_ENTRY`: the mux-agent binary command lines run (default `mux-agent` on PATH).
//! - `MUX_IT_EXIT`: the exit code on quit (default 0).
//!
//! Protocol, all under `$MUX_IT_DIR`; every file the client writes appears whole (written aside, then renamed):
//! - On start: `client.<pid>.json` = `{"pid", "ppid", "exe", "argv", "sessionId", "worker", "cwd", "tty": {"stdin",
//!   "stdout"}, "env": {…}}`, then `<pid>` appended to `clients.log`. `exe` is this file's name (`claude`), `argv`
//!   the arguments after it, `sessionId` the `--session-id` value or null, `worker` whether one was given (a spawned
//!   agent) and `env` the variables in `ENV` (null when unset). `ppid` is null on Windows.
//! - Worker mode: a worker runs each line of `worker.cmd`, if present at start, as `worker.<pid>.<n>.{json,err,code}`
//!   (n from 1), e.g. `send\tparent\t-BodyFile\t{dir}/reply.md` or `status\t-Body\tbusy`.
//! - Commands: the client polls `cmd.<pid>`; it takes the file (renames it to `cmd.<pid>.taken.<n>`) and runs each
//!   line, writing `reply.<pid>.<n>.err`, `reply.<pid>.<n>.code`, then `reply.<pid>.<n>.json` (stdout) last, so the
//!   json's appearance means all three are there. n counts taken files from 1; a file of several lines answers in
//!   the files of its own n, the last line's output winning.
//! - A command line is mux-agent's arguments split on tabs; `{pid}`, `{sid}` and `{dir}` in them are replaced by
//!   this client's pid, session id and `$MUX_IT_DIR`. It runs with stdin closed, in the client's cwd and env.
//! - Each line read on stdin (what `send` types into the pane) is appended to `input.<pid>.log`.
//! - Quit: `quit` (all clients) or `quit.<pid>` appearing makes the client write `exited.<pid>` (its exit code) and
//!   exit with `MUX_IT_EXIT`.

use std::ffi::OsString;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{Map, Value, json};

const POLL: Duration = Duration::from_millis(50);

// The variables a client record carries; never the whole environment.
const ENV: [&str; 9] = [
    "MUX_AGENTS_STATE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_ENTRYPOINT",
    "COPILOT_AGENT_SESSION_ID",
    "TMUX",
    "TMUX_PANE",
    "RMUX",
    "HOME",
    "MUX_IT_EXIT",
];

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
            write_whole(&dir.join(format!("exited.{pid}")), format!("{code}\n"));
            std::process::exit(code);
        }
        let cmd = dir.join(format!("cmd.{pid}"));
        let held = dir.join(format!("cmd.{pid}.taken.{}", taken + 1));
        if std::fs::rename(&cmd, &held).is_ok() {
            taken += 1;
            let text = std::fs::read_to_string(&held).unwrap_or_default();
            for line in text.lines().filter(|l| !l.is_empty()) {
                runner.run(line, &format!("reply.{pid}.{taken}"));
            }
        }
        std::thread::sleep(POLL);
    }
}

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
        let out = Command::new(&self.entry)
            .args(&args)
            .stdin(Stdio::null())
            .output();
        let (stdout, stderr, code) = match out {
            Ok(o) => (o.stdout, o.stderr, o.status.code().unwrap_or(-1)),
            Err(e) => (Vec::new(), format!("fake-client: {e}\n").into_bytes(), -1),
        };
        let file = |ext: &str| self.dir.join(format!("{base}.{ext}"));
        write_whole(&file("err"), stderr);
        write_whole(&file("code"), format!("{code}\n"));
        write_whole(&file("json"), stdout);
    }
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
