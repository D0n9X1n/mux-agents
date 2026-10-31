//! The live tests' harness: a private multiplexer server per test, never the user's own.
//!
//! - `Dir::new()` makes a short temp dir (`/tmp/mxit-<pid>-<n>`, mode 0700, canonical) with `bin/`, `home/`,
//!   `home/.claude/sessions/`, `work/` and `tmp/`; dropping it removes everything.
//! - `Server::start(mux, &dir)` runs `<mux> -S <dir>/s -f /dev/null new-session -d -x 200 -y 50 -c <dir>/work sh`
//!   from a cleared environment: HOME=<dir>/home, PATH=<dir>/bin:<PATH>, TMPDIR=<dir>/tmp, MUX_IT_DIR=<dir>,
//!   MUX_IT_ENTRY=<the mux-agent binary>, plus USER, LOGNAME and LANG when set. So no TMUX, RMUX, MUX_AGENTS_STATE,
//!   XDG_STATE_HOME or CLAUDE_*/COPILOT_* of the caller's own session reaches a pane. `<dir>/bin` holds `claude` and
//!   `copilot`, copies of the fake-client binary. A mux that is not installed is skipped with a message (None).
//! - Every later call is `<mux> -S <socket> -N …`. Dropping the server (or `kill`) writes `<dir>/quit`, kills the
//!   server, waits for its pid to go (SIGKILL after 5 s) and removes the socket.
//! - Files the harness waits on are written whole: shell lines write `<name>.tmp` and `mv` it into place, and the fake
//!   client renames its files into place.
#![allow(dead_code)]

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

const WAIT: Duration = Duration::from_secs(30);
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
    clients_seen: usize,
    replies: HashMap<u64, u32>,
    killed: bool,
}

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
            clients_seen: 0,
            replies: HashMap::new(),
            killed: false,
        };
        assert!(out.status.success(), "{mux:?} new-session: {out:?}");
        let panes = server.call(&["list-panes", "-a", "-F", "#{pane_id} #{pane_pid} #{pid}"]);
        let fields: Vec<&str> = panes.split_whitespace().collect();
        assert_eq!(fields.len(), 3, "{mux:?} list-panes: {panes:?}");
        server.pane = fields[0].to_string();
        server.pane_pid = fields[1].parse().unwrap();
        server.pid = fields[2].parse().unwrap();
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

    fn wait<T>(&self, what: &str, mut ready: impl FnMut() -> Option<T>) -> T {
        let end = Instant::now() + WAIT;
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
        self.send_keys(&format!(
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

    /// Runs the mux-agent binary in the root pane as `env <vars> <mux-agent> <args>` and answers its exit code and
    /// JSON reply. `vars` reach only this call; the pane's own env is the server's (see `start`).
    pub fn run(&mut self, args: &[&str], vars: &[(&str, &str)]) -> (i32, Value) {
        let mut line = String::from("env");
        for (name, value) in vars {
            line.push(' ');
            line.push_str(&quote(&format!("{name}={value}")));
        }
        line.push(' ');
        line.push_str(&quote(env!("CARGO_BIN_EXE_mux-agent")));
        for arg in args {
            line.push(' ');
            line.push_str(&quote(arg));
        }
        let (stdout, err) = self.shell(&line);
        let code = err.lines().last().and_then(|c| c.trim().parse().ok());
        let reply = serde_json::from_str(&stdout);
        match (code, reply) {
            (Some(code), Ok(reply)) => (code, reply),
            _ => panic!(
                "{:?} {args:?}: stdout {stdout:?}, stderr and code {err:?}",
                self.mux
            ),
        }
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

    /// Ends the fake clients and the server, and removes the socket. Idempotent.
    pub fn kill(&mut self) {
        if self.killed {
            return;
        }
        self.killed = true;
        let _ = std::fs::write(self.dir.join("quit"), "");
        let _ = self.try_call(&["kill-server"]);
        if self.pid != 0 {
            let end = Instant::now() + Duration::from_secs(5);
            while alive(self.pid) && Instant::now() < end {
                std::thread::sleep(POLL);
            }
            if alive(self.pid) {
                // SAFETY: the pid is this test's own private server.
                unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGKILL) };
            }
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.kill();
    }
}
