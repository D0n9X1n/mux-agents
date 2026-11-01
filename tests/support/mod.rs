//! The live tests' harness: a private multiplexer server per test, never the user's own.
//!
//! - `Dir::new()` makes a short temp dir (`/tmp/mxit-<pid>-<n>`, mode 0700, canonical) with `bin/`, `home/`,
//!   `home/.claude/sessions/`, `work/` and `tmp/`; dropping it removes everything.
//! - `Server::start(mux, &dir)` runs `<mux> -S <dir>/s -f /dev/null new-session -d -x 200 -y 50 -c <dir>/work sh`
//!   from a cleared environment: HOME=<dir>/home, PATH=<dir>/bin:<PATH>, TMPDIR=<dir>/tmp, MUX_IT_DIR=<dir>,
//!   MUX_IT_ENTRY=<the mux-agent binary>, plus USER, LOGNAME and LANG when set. So no TMUX, RMUX, MUX_AGENTS_STATE,
//!   XDG_STATE_HOME or CLAUDE_*/COPILOT_* of the caller's own session reaches a pane. `<dir>/bin` holds `claude` and
//!   `copilot`, copies of the fake-client binary. A mux that is not installed is skipped with a message (None).
//! - Every later call is `<mux> -S <socket> -N …`. Dropping the server (or `kill`) snapshots every descendant of the
//!   server, writes `<dir>/quit`, kills the server, waits for its pid to go (SIGKILL after 5 s), waits for each
//!   descendant to go (SIGKILL, same start only, after 5 s), removes the socket, then removes the lock files whose
//!   keys name this server (`<passwd home>/.local/state/mux-agents/locks/<hex>.lock`; mux-agent never removes one).
//!   Only keys built from this server's own key are touched, so no other lock is. The server's pid is trusted only
//!   while it has the start read at `start`: a reused pid is neither walked nor signalled.
//! - Files the harness waits on are written whole: shell lines write `<name>.tmp` and `mv` it into place, and the fake
//!   client renames its files into place.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

// Generous for loaded CI runners: a wait only runs this long when something is wrong, or behind a 15 s lock wait.
const WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mux {
    /// `tmux` on PATH: the current release (3.8 on the dev host).
    Tmux38,
    /// `$MUX_AGENTS_TMUX32`, the oldest supported tmux, built by `sh scripts/dev/tmux32.sh`.
    Tmux32,
    /// `rmux` on PATH.
    Rmux,
}

impl Mux {
    pub const ALL: [Mux; 3] = [Mux::Tmux38, Mux::Tmux32, Mux::Rmux];

    /// The variable a pane's endpoint is read from: RMUX when set, else TMUX.
    pub fn endpoint_var(self) -> &'static str {
        match self {
            Mux::Rmux => "RMUX",
            Mux::Tmux38 | Mux::Tmux32 => "TMUX",
        }
    }

    /// The binary, canonical, or why this mux is skipped.
    fn binary(self) -> Result<PathBuf, String> {
        let (bin, version) = match self {
            Mux::Tmux38 => (on_path("tmux").ok_or("no tmux on PATH")?, "tmux "),
            Mux::Rmux => (on_path("rmux").ok_or("no rmux on PATH")?, "rmux "),
            Mux::Tmux32 => {
                let set = std::env::var_os("MUX_AGENTS_TMUX32").filter(|v| !v.is_empty());
                let bin = set.ok_or(
                    "MUX_AGENTS_TMUX32 is unset; `sh scripts/dev/tmux32.sh` builds tmux 3.2a and prints it",
                )?;
                (PathBuf::from(bin), "tmux 3.2a")
            }
        };
        let bin = std::fs::canonicalize(&bin).map_err(|e| format!("{}: {e}", bin.display()))?;
        let out = Command::new(&bin).arg("-V").output();
        let reported = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        match reported {
            // A set MUX_AGENTS_TMUX32 that is not tmux 3.2a is a broken setup, not a skip.
            Ok(v) if v.starts_with(version) => Ok(bin),
            other if self == Mux::Tmux32 => panic!("{}: -V answered {other:?}", bin.display()),
            other => Err(format!("{} -V answered {other:?}", bin.display())),
        }
    }
}

fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// Whether a process with this pid exists.
pub fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the pid exists and may be signalled.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// `'text'` for sh, with each `'` as `'\''`.
pub fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Writes `path` whole: a sibling temp file renamed into place.
pub fn write_atomic(path: &Path, bytes: impl AsRef<[u8]>) {
    let tmp = path.with_extension("harness-tmp");
    std::fs::write(&tmp, bytes).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

/// A private temp dir, removed on drop. Short, so `<dir>/s` fits a Unix socket path.
pub struct Dir(PathBuf);

impl Dir {
    pub fn new() -> Dir {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let raw = PathBuf::from(format!("/tmp/mxit-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&raw);
        std::fs::DirBuilder::new().mode(0o700).create(&raw).unwrap();
        // /tmp is a symlink on macOS; panes report the real path.
        let dir = Dir(std::fs::canonicalize(&raw).unwrap());
        for sub in ["bin", "home/.claude/sessions", "work", "tmp"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        dir
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn join(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.0.join(rel)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A running private server; killed on drop, so a failing test leaves none behind.
pub struct Server {
    mux: Mux,
    bin: PathBuf,
    dir: PathBuf,
    socket: PathBuf,
    pid: u32,
    pane: String,
    pane_pid: u32,
    // Counters for the shell lines' output files and the fake clients' command replies.
    runs: u32,
    jobs: u32,
    lines: u32,
    clients_seen: usize,
    replies: HashMap<u64, u32>,
    // The server key mux-agent derives: sha256("<socket>|<pid>|<start>")[..16]; empty when the start is unreadable.
    key: String,
    // The server's start as read at `start`; teardown signals the pid only while it still has this start.
    start: Option<i64>,
    killed: bool,
}

/// A background mux-agent run in the root pane's shell (`Server::bg`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Job(u32);

/// One background run for `Server::bg`: mux-agent's arguments, variables to set and variables to unset.
pub type Run<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)], &'a [&'a str]);

impl Server {
    /// A server whose one pane runs `sh` in `<dir>/work`, or None (with a message) when `mux` is not installed.
    pub fn start(mux: Mux, dir: &Dir) -> Option<Server> {
        let bin = match mux.binary() {
            Ok(bin) => bin,
            Err(why) => {
                eprintln!("skipping {mux:?}: {why}");
                return None;
            }
        };
        let fake = env!("CARGO_BIN_EXE_fake-client");
        for name in ["claude", "copilot"] {
            std::fs::copy(fake, dir.join("bin").join(name)).unwrap();
        }
        let socket = dir.join("s");
        let mut path = OsString::from(dir.join("bin"));
        if let Some(rest) = std::env::var_os("PATH") {
            path.push(":");
            path.push(rest);
        }
        let mut start = Command::new(&bin);
        start
            .arg("-S")
            .arg(&socket)
            .args([
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-x",
                "200",
                "-y",
                "50",
                "-c",
            ])
            .arg(dir.join("work"))
            .arg("sh")
            .env_clear()
            .env("HOME", dir.join("home"))
            .env("PATH", path)
            .env("TMPDIR", dir.join("tmp"))
            .env("MUX_IT_DIR", dir.path())
            .env("MUX_IT_ENTRY", env!("CARGO_BIN_EXE_mux-agent"));
        for keep in ["USER", "LOGNAME", "LANG"] {
            if let Some(value) = std::env::var_os(keep) {
                start.env(keep, value);
            }
        }
        let out = start.output().unwrap();
        let mut server = Server {
            mux,
            bin,
            dir: dir.path().to_path_buf(),
            socket,
            pid: 0,
            pane: String::new(),
            pane_pid: 0,
            runs: 0,
            jobs: 0,
            lines: 0,
            clients_seen: 0,
            replies: HashMap::new(),
            key: String::new(),
            start: None,
            killed: false,
        };
        // Seen once on rmux 0.10.0 under the parallel suite and never reproduced (5 full runs, 2 suites at once,
        // 64 threads): "failed to spawn pane shell: pty syscall failed: Unknown error: -6". Named, not retried.
        let pty =
            mux == Mux::Rmux && String::from_utf8_lossy(&out.stderr).contains("pty syscall failed");
        assert!(
            out.status.success(),
            "{mux:?} new-session: {out:?}{}",
            if pty {
                " (a transient pty-allocation failure inside rmux, not mux-agent; not retried)"
            } else {
                ""
            }
        );
        let panes = server.call(&["list-panes", "-a", "-F", "#{pane_id} #{pane_pid} #{pid}"]);
        let fields: Vec<&str> = panes.split_whitespace().collect();
        assert_eq!(fields.len(), 3, "{mux:?} list-panes: {panes:?}");
        server.pane = fields[0].to_string();
        server.pane_pid = fields[1].parse().unwrap();
        server.pid = fields[2].parse().unwrap();
        if let Some(opened) = mux_agent::proc::host().open(server.pid) {
            let start = opened.start;
            server.start = Some(start);
            let id = format!("{}|{}|{start}", server.socket.display(), server.pid);
            server.key = mux_agent::util::sha256_hex(id)[..16].to_string();
        }
        Some(server)
    }

    pub fn mux(&self) -> Mux {
        self.mux
    }

    /// The canonical binary the server runs.
    pub fn binary(&self) -> &Path {
        &self.bin
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The root pane's id (`%0`) and its process, the `sh`.
    pub fn pane(&self) -> &str {
        &self.pane
    }

    pub fn pane_pid(&self) -> u32 {
        self.pane_pid
    }

    /// The private temp dir (`MUX_IT_DIR`).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The server key mux-agent names this server's state folder and lock keys by.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// `<mux> -S <socket> -N <args>`; panics unless it succeeds, and answers its stdout.
    pub fn call(&self, args: &[&str]) -> String {
        let out = self.try_call(args);
        assert!(out.status.success(), "{:?} {args:?}: {out:?}", self.mux);
        String::from_utf8(out.stdout).unwrap()
    }

    pub fn try_call(&self, args: &[&str]) -> Output {
        Command::new(&self.bin)
            .arg("-S")
            .arg(&self.socket)
            .arg("-N")
            .args(args)
            .output()
            .unwrap()
    }

    /// Types `text` literally into the root pane, then Enter.
    pub fn send_keys(&self, text: &str) {
        self.call(&["send-keys", "-t", &self.pane, "-l", text]);
        self.call(&["send-keys", "-t", &self.pane, "Enter"]);
    }

    /// The root pane's screen.
    pub fn capture(&self) -> String {
        self.call(&["capture-pane", "-p", "-t", &self.pane])
    }

    /// Waits for `<dir>/<name>` and answers its text; on timeout panics with the pane's screen.
    pub fn wait_file(&self, name: &str) -> String {
        let path = self.dir.join(name);
        self.wait(name, || std::fs::read_to_string(&path).ok())
    }

    /// Waits until `<dir>/<name>` has `line` as a whole line.
    pub fn wait_line(&self, name: &str, line: &str) {
        let path = self.dir.join(name);
        self.wait(&format!("{name}: {line}"), || {
            let text = std::fs::read_to_string(&path).ok()?;
            text.lines().any(|l| l == line).then_some(())
        })
    }

    /// Polls `ready` for up to 30 s; on timeout panics naming `what`, with the root pane's screen.
    pub fn wait<T>(&self, what: &str, ready: impl FnMut() -> Option<T>) -> T {
        self.wait_up_to(WAIT, what, ready)
    }

    /// `wait` with its own limit.
    pub fn wait_up_to<T>(
        &self,
        limit: Duration,
        what: &str,
        mut ready: impl FnMut() -> Option<T>,
    ) -> T {
        let end = Instant::now() + limit;
        loop {
            if let Some(found) = ready() {
                return found;
            }
            if Instant::now() > end {
                let screen = self.try_call(&["capture-pane", "-p", "-t", &self.pane]);
                let screen = String::from_utf8_lossy(&screen.stdout);
                panic!(
                    "{:?}: timed out waiting for {what}; pane:\n{screen}",
                    self.mux
                );
            }
            std::thread::sleep(POLL);
        }
    }

    /// Has the root pane's shell run `line` by typing `. '<dir>/line.<n>.sh'`. A long line is never typed: keys that
    /// arrive before the shell reads its first prompt wait in the tty's cooked-mode buffer, which keeps only 1024
    /// bytes on macOS (MAX_CANON) and drops the rest, Enter included. Sourced, the line runs in the shell itself, so
    /// `&`, `$!` and job control are as if typed.
    fn type_line(&mut self, line: &str) {
        self.lines += 1;
        let path = self.dir.join(format!("line.{}.sh", self.lines));
        write_atomic(&path, format!("{line}\n"));
        self.send_keys(&format!(". {}", quote(path.to_str().unwrap())));
    }

    /// Runs `line` in the root pane's shell with stdout to a file renamed into place once the line is done.
    fn shell(&mut self, line: &str) -> (String, String) {
        self.runs += 1;
        let file = |ext: &str| self.dir.join(format!("run.{}.{ext}", self.runs));
        let (out, err, tmp) = (file("out"), file("err"), file("tmp"));
        let (out_q, err_q, tmp_q) = (
            quote(out.to_str().unwrap()),
            quote(err.to_str().unwrap()),
            quote(tmp.to_str().unwrap()),
        );
        self.type_line(&format!(
            "{line} > {tmp_q} 2> {err_q}; echo $? >> {err_q}; mv {tmp_q} {out_q}"
        ));
        let name = out.file_name().unwrap().to_str().unwrap().to_string();
        let stdout = self.wait_file(&name);
        (stdout, std::fs::read_to_string(err).unwrap())
    }

    /// The root pane shell's environment, one `NAME=value` per line.
    pub fn pane_env(&mut self) -> String {
        self.shell("env").0
    }

    /// `env [-u <name>]... <vars> <bin> <args>`, each token quoted for sh.
    fn line(bin: &str, args: &[&str], vars: &[(&str, &str)], unset: &[&str]) -> String {
        let mut line = String::from("env");
        for name in unset {
            line.push_str(" -u ");
            line.push_str(&quote(name));
        }
        for (name, value) in vars {
            line.push(' ');
            line.push_str(&quote(&format!("{name}={value}")));
        }
        line.push(' ');
        line.push_str(&quote(bin));
        for arg in args {
            line.push(' ');
            line.push_str(&quote(arg));
        }
        line
    }

    /// Runs the mux-agent binary in the root pane as `env <vars> <mux-agent> <args>` and answers its exit code and
    /// JSON reply. `vars` reach only this call; the pane's own env is the server's (see `start`).
    pub fn run(&mut self, args: &[&str], vars: &[(&str, &str)]) -> (i32, Value) {
        let (code, stdout, stderr) = self.run_raw(env!("CARGO_BIN_EXE_mux-agent"), args, vars, &[]);
        match serde_json::from_str(&stdout) {
            Ok(reply) => (code, reply),
            Err(_) => panic!(
                "{:?} {args:?}: stdout {stdout:?}, stderr {stderr:?}, code {code}",
                self.mux
            ),
        }
    }

    /// Runs `bin` in the root pane as `env -u <unset>... <vars> <bin> <args>`; answers its exit code, stdout and
    /// stderr exactly.
    pub fn run_raw(
        &mut self,
        bin: &str,
        args: &[&str],
        vars: &[(&str, &str)],
        unset: &[&str],
    ) -> (i32, String, String) {
        let (stdout, err) = self.shell(&Self::line(bin, args, vars, unset));
        // The shell appended `$?` and a LF after whatever the program wrote to stderr.
        let body = err.strip_suffix('\n').unwrap_or(&err);
        let (stderr, code) = match body.rfind('\n') {
            Some(at) => (&body[..=at], &body[at + 1..]),
            None => ("", body),
        };
        let code = code
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{:?} {args:?}: no exit code in {err:?}", self.mux));
        (code, stdout, stderr.to_string())
    }

    /// Starts each `(args, vars, unset)` mux-agent run in the background of the root pane's shell, all from one
    /// line so they start together; returns once every job's pid is recorded. Each job's subshell is its own
    /// process group (the shell has job control), so the caller chain is mux-agent > sh > sh > pane.
    pub fn bg(&mut self, runs: &[Run]) -> Vec<Job> {
        let mut line = String::new();
        let mut jobs = Vec::new();
        for (args, vars, unset) in runs {
            self.jobs += 1;
            let job = Job(self.jobs);
            let file = |ext: &str| quote(self.job_file(job, ext).to_str().unwrap());
            let command = Self::line(env!("CARGO_BIN_EXE_mux-agent"), args, vars, unset);
            line.push_str(&format!(
                "( {command} > {tmp} 2> {err}; echo $? >> {err}; mv {tmp} {out} ) & echo $! > {ptmp}; mv {ptmp} {pid}; ",
                tmp = file("tmp"),
                err = file("err"),
                out = file("out"),
                ptmp = file("ptmp"),
                pid = file("pid"),
            ));
            jobs.push(job);
        }
        self.type_line(line.trim_end());
        for job in &jobs {
            self.job_pid(*job);
        }
        jobs
    }

    fn job_file(&self, job: Job, ext: &str) -> PathBuf {
        self.dir.join(format!("bg.{}.{ext}", job.0))
    }

    /// The background job's subshell pid.
    pub fn job_pid(&self, job: Job) -> u32 {
        let name = format!("bg.{}.pid", job.0);
        self.wait_file(&name).trim().parse().unwrap()
    }

    /// Whether the job has finished (its stdout is in place).
    pub fn job_done(&self, job: Job) -> bool {
        self.job_file(job, "out").exists()
    }

    /// Waits up to `limit` for the job; answers its exit code and JSON reply.
    pub fn job_wait(&self, job: Job, limit: Duration) -> (i32, Value) {
        let out = self.job_file(job, "out");
        let stdout = self.wait_up_to(limit, &format!("background job {}", job.0), || {
            std::fs::read_to_string(&out).ok()
        });
        let err = std::fs::read_to_string(self.job_file(job, "err")).unwrap();
        let code = err.lines().last().and_then(|c| c.trim().parse().ok());
        match (code, serde_json::from_str(&stdout)) {
            (Some(code), Ok(reply)) => (code, reply),
            _ => panic!(
                "{:?} job {}: stdout {stdout:?}, stderr and code {err:?}",
                self.mux, job.0
            ),
        }
    }

    /// The fake client started with `--session-id <sid>`: its `client.<pid>.json` record.
    pub fn client_by_session(&self, sid: &str) -> Value {
        let log = self.dir.join("clients.log");
        self.wait(&format!("client of session {sid}"), || {
            let text = std::fs::read_to_string(&log).ok()?;
            text.lines().find_map(|pid| {
                let path = self.dir.join(format!("client.{}.json", pid.trim()));
                let record: Value =
                    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
                (record["sessionId"] == sid).then_some(record)
            })
        })
    }

    /// The next fake client to start: its `client.<pid>.json` record.
    pub fn wait_client(&mut self) -> Value {
        let log = self.dir.join("clients.log");
        let seen = self.clients_seen;
        let pid = self.wait(&format!("client #{}", seen + 1), || {
            let text = std::fs::read_to_string(&log).ok()?;
            text.lines().nth(seen).map(str::to_string)
        });
        self.clients_seen += 1;
        serde_json::from_str(&self.wait_file(&format!("client.{pid}.json"))).unwrap()
    }

    /// Has fake client `pid` run mux-agent with `args`; answers the exit code and the JSON reply.
    pub fn command(&mut self, pid: u64, args: &[&str]) -> (i32, Value) {
        let n = self.replies.entry(pid).or_default();
        *n += 1;
        let n = *n;
        write_atomic(
            &self.dir.join(format!("cmd.{pid}")),
            format!("{}\n", args.join("\t")),
        );
        let stdout = self.wait_file(&format!("reply.{pid}.{n}.json"));
        let code = std::fs::read_to_string(self.dir.join(format!("reply.{pid}.{n}.code"))).unwrap();
        let reply = serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("{:?} {args:?}: {e}: {stdout:?}", self.mux));
        (code.trim().parse().unwrap(), reply)
    }

    /// Tells every fake client to leave.
    pub fn quit(&self) {
        write_atomic(&self.dir.join("quit"), "");
    }

    /// Ends the fake clients, every process the server started and the server, removes the socket and this server's
    /// lock files. Idempotent.
    pub fn kill(&mut self) {
        if self.killed {
            return;
        }
        self.killed = true;
        let sessions = self.sessions();
        // A pid whose start is not the one recorded at `start` (or was never readable) is another process: its tree
        // is never collected and it is never signalled.
        let descendants = if same_process(self.pid, self.start) {
            descendants(self.pid)
        } else {
            Vec::new()
        };
        let _ = std::fs::write(self.dir.join("quit"), "");
        let _ = self.try_call(&["kill-server"]);
        let end = Instant::now() + Duration::from_secs(5);
        while same_process(self.pid, self.start) && Instant::now() < end {
            std::thread::sleep(POLL);
        }
        // Checked again right before the signal; without a pidfd (macOS) this is the narrowest window there is.
        if same_process(self.pid, self.start) {
            // SAFETY: the pid still has the start this test's private server had.
            unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGKILL) };
        }
        reap(&descendants);
        let _ = std::fs::remove_file(&self.socket);
        // Only once nothing of this server can hold or take a lock.
        let left = self.remove_locks(&sessions);
        if !left.is_empty() && !std::thread::panicking() {
            panic!("{:?}: lock files left: {left:?}", self.mux);
        }
    }

    /// The server's session ids (`$0`, …), plus `$0`-`$3` in case it no longer answers.
    fn sessions(&self) -> Vec<String> {
        let mut ids: Vec<String> = (0..4).map(|n| format!("${n}")).collect();
        let out = self.try_call(&["list-sessions", "-F", "#{session_id}"]);
        for id in String::from_utf8_lossy(&out.stdout).lines() {
            if !ids.iter().any(|known| known == id) {
                ids.push(id.to_string());
            }
        }
        ids
    }

    /// Every lock key mux-agent may have made for this server: spawn and sidebar per session, notice and agent per
    /// agent id, mail per agent id and per `pane-<n>`. The server keys are this server's own and any state folder
    /// key found under the private dir (a folder holding server.json). Removes each lock file that exists and
    /// answers the paths that still exist afterwards.
    fn remove_locks(&self, sessions: &[String]) -> Vec<PathBuf> {
        let Some(root) = lock_root() else {
            return Vec::new();
        };
        let (mut keys, mut ids) = (HashSet::new(), HashSet::new());
        if !self.key.is_empty() {
            keys.insert(self.key.clone());
        }
        scan(&self.dir, 0, &mut keys, &mut ids);
        let mut paths = Vec::new();
        for key in &keys {
            for session in sessions {
                for kind in ["spawn", "sidebar"] {
                    paths.push(lock_file(&root, &format!("{kind}|{key}|{session}")));
                }
            }
            for id in &ids {
                for kind in ["notice", "agent", "mail"] {
                    paths.push(lock_file(&root, &format!("{kind}|{key}|{id}")));
                }
            }
            for pane in 0..256 {
                paths.push(lock_file(&root, &format!("mail|{key}|pane-{pane}")));
            }
        }
        let mut removed = 0;
        for path in &paths {
            if std::fs::remove_file(path).is_ok() {
                removed += 1;
            }
        }
        eprintln!(
            "{:?}: removed {removed} lock file(s) of server key(s) {keys:?}",
            self.mux
        );
        paths.into_iter().filter(|p| p.exists()).collect()
    }
}

/// `<root>/<32 hex of sha256(key)>.lock`, as mux-agent names it.
pub fn lock_file(root: &Path, key: &str) -> PathBuf {
    mux_agent::state::lock::lock_path(root, key)
}

/// `<passwd home, resolved>/.local/state/mux-agents/locks`: the per-user lock root mux-agent uses whatever HOME is.
pub fn lock_root() -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: an all-zero passwd is a valid out value; getpwuid_r fills it from `buf`.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found = std::ptr::null_mut();
    // SAFETY: `pwd`, `buf` (with its length) and `found` are valid for the call.
    let rc = unsafe {
        libc::getpwuid_r(
            libc::geteuid(),
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut found,
        )
    };
    if rc != 0 || found.is_null() || pwd.pw_dir.is_null() {
        return None;
    }
    // SAFETY: pw_dir points at a NUL-terminated string inside `buf`, still live.
    let home = unsafe { CStr::from_ptr(pwd.pw_dir) };
    let home = std::fs::canonicalize(std::ffi::OsStr::from_bytes(home.to_bytes())).ok()?;
    Some(home.join(".local/state/mux-agents/locks"))
}

/// State folder keys (the name of a folder holding server.json) and agent ids (folders in an `agents` folder) under
/// `dir`, links never followed.
fn scan(dir: &Path, depth: u32, keys: &mut HashSet<String>, ids: &mut HashSet<String>) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if kind.is_file() && name == "server.json" {
            if let Some(key) = dir.file_name() {
                keys.insert(key.to_string_lossy().into_owned());
            }
        } else if kind.is_dir() {
            if dir.file_name().is_some_and(|n| n == "agents") {
                ids.insert(name);
            }
            scan(&entry.path(), depth + 1, keys, ids);
        }
    }
}

/// Every descendant of `pid` with its start, from one process table.
fn descendants(pid: u32) -> Vec<(u32, Option<i64>)> {
    let Ok(table) = mux_agent::proc::host().table() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut queue = vec![pid];
    let mut seen = HashSet::from([pid]);
    while let Some(parent) = queue.pop() {
        for row in table.iter().filter(|r| r.ppid == parent) {
            if seen.insert(row.pid) {
                found.push((row.pid, row.start));
                queue.push(row.pid);
            }
        }
    }
    found
}

/// The start of `pid` now, if it is open-able.
pub fn start_of(pid: u32) -> Option<i64> {
    mux_agent::proc::host().open(pid).map(|o| o.start)
}

/// Whether `pid` is still the process that had `start`: alive with that start now. An unknown start is never.
fn same_process(pid: u32, start: Option<i64>) -> bool {
    pid != 0 && start.is_some() && alive(pid) && start_of(pid) == start
}

/// Waits up to 5 s for each process to go, then SIGKILLs any still there with its recorded start (never a reused
/// pid), and waits again.
fn reap(processes: &[(u32, Option<i64>)]) {
    let still = |&&(pid, start): &&(u32, Option<i64>)| same_process(pid, start);
    let end = Instant::now() + Duration::from_secs(5);
    while processes.iter().any(|p| still(&p)) && Instant::now() < end {
        std::thread::sleep(POLL);
    }
    for (pid, _) in processes.iter().filter(still) {
        // SAFETY: a descendant of this test's private server, its start unchanged since the snapshot.
        unsafe { libc::kill(*pid as libc::pid_t, libc::SIGKILL) };
    }
    let end = Instant::now() + Duration::from_secs(5);
    while processes.iter().any(|p| still(&p)) && Instant::now() < end {
        std::thread::sleep(POLL);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.kill();
    }
}

/// SIGKILLs each process still alive with its recorded start, on drop; a test's own sentinels only.
pub struct Sentinels(pub Vec<(u32, Option<i64>)>);

impl Drop for Sentinels {
    fn drop(&mut self) {
        for &(pid, start) in &self.0 {
            if start.is_some() && start_of(pid) == start {
                // SAFETY: a process this test started, its start unchanged.
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            }
        }
    }
}

// The reviewer's case: the private server exited and its pid now names an unrelated process (a `/bin/sh` sentinel
// with a `/bin/sleep` child). Teardown signals neither, whether the recorded start is another process's or unknown.
#[test]
fn teardown_never_signals_a_reused_server_pid() {
    // A real start, then its process gone: the start an exited server recorded.
    let mut gone = Command::new("/bin/sleep").arg("60").spawn().unwrap();
    let gone_start = start_of(gone.id()).expect("the start of a live sleep");
    gone.kill().unwrap();
    gone.wait().unwrap();
    // Linux start times count clock ticks (10 ms): let the clock pass the gone start, so the sentinel's differs.
    std::thread::sleep(Duration::from_millis(50));

    let mut sentinel = Command::new("/bin/sh")
        .args(["-c", "/bin/sleep 60 & wait"])
        .spawn()
        .unwrap();
    let pid = sentinel.id();
    let mut guard = Sentinels(vec![(pid, start_of(pid))]);
    let end = Instant::now() + WAIT;
    let child = loop {
        if let Some(&child) = descendants(pid).first() {
            break child;
        }
        assert!(Instant::now() < end, "the sentinel's sleep never started");
        std::thread::sleep(POLL);
    };
    guard.0.push(child);
    assert_ne!(start_of(pid), Some(gone_start));

    for recorded in [Some(gone_start), None] {
        let dir = Dir::new();
        let mut server = Server {
            mux: Mux::Tmux38,
            // Answers every call with exit 1, as a server that is gone does.
            bin: PathBuf::from("/usr/bin/false"),
            dir: dir.path().to_path_buf(),
            socket: dir.join("s"),
            pid,
            pane: "%0".into(),
            pane_pid: 0,
            runs: 0,
            jobs: 0,
            lines: 0,
            clients_seen: 0,
            replies: HashMap::new(),
            key: String::new(),
            start: recorded,
            killed: false,
        };
        server.kill();
        assert_eq!(
            sentinel.try_wait().unwrap(),
            None,
            "teardown signalled the replacement process (recorded start {recorded:?})"
        );
        assert!(
            alive(child.0) && start_of(child.0) == child.1,
            "teardown signalled the replacement's child (recorded start {recorded:?})"
        );
    }
    drop(guard);
    let _ = sentinel.wait();
}
