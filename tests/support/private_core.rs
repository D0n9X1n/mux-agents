#![allow(dead_code)]
use serde_json::Value;
use std::{
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};
pub struct PrivateCore {
    bin: PathBuf,
    dir: PathBuf,
    pub env: Vec<(&'static str, OsString)>,
    owned: bool,
    started: bool,
}
impl PrivateCore {
    pub fn installed() -> PathBuf {
        let c = mux_agent::ctx::Ctx {
            env: mux_agent::ctx::Env::from_os(std::env::vars_os()),
            pid: std::process::id(),
            cwd: std::env::current_dir().unwrap(),
            home: std::env::temp_dir(),
            state_root: std::env::temp_dir(),
            exe: std::env::current_exe().unwrap(),
            os: mux_agent::proc::host(),
            mux: std::cell::RefCell::new(None),
            member_mux: std::cell::RefCell::new(mux_agent::mux::MemberMux::Caller),
            hc: Box::new(mux_agent::hc::NativeHc),
            clock: Box::new(mux_agent::ctx::SystemClock),
            admitted: false,
            tty: false,
        };
        mux_agent::hc::connect(&c)
            .expect("live tests require hc >=0.1.21")
            .binary()
            .to_path_buf()
    }
    pub fn new(bin: PathBuf, dir: &Path, path: &OsStr) -> Self {
        let session = format!(
            "mux-test-{}-{}",
            std::process::id(),
            mux_agent::util::hex_id().unwrap()
        );
        let mut env: Vec<_> = [
            "USER",
            "USERNAME",
            "LOGNAME",
            "LANG",
            "SystemRoot",
            "ComSpec",
            "SystemDrive",
            "windir",
            "ProgramData",
            "ALLUSERSPROFILE",
            "PUBLIC",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "CommonProgramFiles",
            "PATHEXT",
            "OS",
            "PROCESSOR_ARCHITECTURE",
            "NUMBER_OF_PROCESSORS",
            "COMPUTERNAME",
            "USERDOMAIN",
            "RMUX_ALLOW_INTERNAL_DAEMON_IN_CALLER_JOB",
        ]
        .into_iter()
        .filter_map(|k| std::env::var_os(k).map(|v| (k, v)))
        .collect();
        for (k, sub) in [
            ("HOME", "home"),
            ("USERPROFILE", "home"),
            ("APPDATA", "home/AppData/Roaming"),
            ("LOCALAPPDATA", "home/AppData/Local"),
            ("XDG_STATE_HOME", "core-state"),
            ("XDG_CONFIG_HOME", "core-config"),
            ("XDG_RUNTIME_DIR", "run"),
            ("TMPDIR", "tmp"),
            ("TEMP", "tmp"),
            ("TMP", "tmp"),
        ] {
            fs::create_dir_all(dir.join(sub)).unwrap();
            env.push((k, dir.join(sub).into_os_string()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.join("run"), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let socket = if cfg!(windows) {
            OsString::from(format!(r"\\.\pipe\{session}"))
        } else {
            dir.join("h.sock").into_os_string()
        };
        env.extend([
            ("PATH", path.to_owned()),
            ("HC_SESSION", session.into()),
            ("HC_SOCKET", socket),
            ("MUX_IT_DIR", dir.as_os_str().to_owned()),
            ("MUX_IT_ENTRY", env!("CARGO_BIN_EXE_mux-agent").into()),
        ]);
        Self {
            bin,
            dir: dir.into(),
            env,
            owned: false,
            started: false,
        }
    }
    pub fn apply(&self, c: &mut Command) {
        c.env_clear().envs(self.env.iter().cloned());
    }
    pub fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(&self.bin);
        self.apply(&mut c);
        c.args(args).current_dir(&self.dir);
        c
    }
    /// 6b ambiguity 14: the core killed outright (SIGKILL; Windows `taskkill /F`, never `/T`), then started again. The
    /// pid comes from `server status --json` (r6b call 2). The terminals it ran survive (hc's `hold`) and the restarted
    /// core adopts them as live: a core kill alone loses no member (r6b-calls "how a live test produces a Lost member").
    pub fn kill_hard(&mut self) {
        self.kill_hard_with(&[]);
    }

    /// The processes of the member terminal running client `client`, read before a kill: the client and its nearest
    /// ancestor running this core's binary (its `hold`), each with its start, so a reused pid is never signalled.
    pub fn terminal_of(&self, client: u32) -> Vec<(u32, Option<i64>)> {
        let table = mux_agent::proc::host().table().expect("process table");
        let name = |r: &mux_agent::proc::Row| {
            let p = r
                .path
                .clone()
                .or_else(|| r.argv.as_ref().and_then(|a| a.first().cloned()))?;
            Some(p.rsplit(['/', '\\']).next()?.to_ascii_lowercase())
        };
        // The core runs as the file the install's links resolve to: `hc` is often a link to `herdr-core`.
        let real = std::fs::canonicalize(&self.bin).unwrap_or_else(|_| self.bin.clone());
        let core = real
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_ascii_lowercase();
        let row = |pid: u32| table.iter().find(|r| r.pid == pid);
        let me = row(client).unwrap_or_else(|| panic!("client {client} is not running"));
        let mut found = vec![(me.pid, me.start)];
        let mut at = me.ppid;
        for _ in 0..8 {
            let Some(r) = row(at) else { break };
            if name(r).as_deref() == Some(core.as_str()) {
                found.push((r.pid, r.start));
                return found;
            }
            at = r.ppid;
        }
        panic!("client {client} has no {core} ancestor (its hold)");
    }

    /// `kill_hard`, killing each of `terminals` (from `terminal_of`) while the core is down, so the restarted core finds
    /// those members' terminals gone: a Lost member (r6b-calls). Only a pid whose start is unchanged is signalled; one
    /// that is gone by the time the signal lands (its holder ended it) is already what the kill wanted.
    pub fn kill_hard_with(&mut self, terminals: &[(u32, Option<i64>)]) {
        let status =
            run(self.command(&["server", "status", "--json"]), &self.dir).expect("core status");
        let pid = status["pid"]
            .as_u64()
            .expect("hc server status --json names the core pid") as u32;
        assert!(kill_pid(pid), "kill of the core {pid} failed");
        let same = |pid: u32, start| mux_agent::proc::host().open(pid).map(|o| o.start) == start;
        for (pid, start) in kill_order(terminals) {
            if start.is_some() && same(pid, start) && !kill_pid(pid) {
                let end = Instant::now() + Duration::from_secs(5);
                while same(pid, start) && Instant::now() < end {
                    std::thread::sleep(Duration::from_millis(50));
                }
                assert!(!same(pid, start), "kill of {pid} failed and it still runs");
            }
        }
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            match run(self.command(&["server", "start", "--json"]), &self.dir) {
                Ok(_) => break,
                Err(e) if Instant::now() < end => {
                    eprintln!("core restart: {e}");
                    std::thread::sleep(Duration::from_millis(250));
                }
                Err(e) => panic!("the private core did not start again: {e}"),
            }
        }
        self.owned = true;
        self.started = true;
    }
    pub fn start(&mut self) {
        self.owned = true;
        self.started = true;
        run(self.command(&["server", "start", "--json"]), &self.dir).expect("private core start");
    }
    pub fn stop(&mut self) -> bool {
        if !self.owned {
            return true;
        }
        match run(self.command(&["server", "stop", "--json"]), &self.dir) {
            Ok(_) => {
                self.owned = false;
                true
            }
            Err(e) => {
                let _ = fs::write(self.dir.join("core-stop-failed"), e.to_string());
                eprintln!("private core retained: {e}");
                false
            }
        }
    }
}
impl PrivateCore {
    /// The holder-socket prefixes of this fixture's core: its socket as given and, on Unix, under the dir's real path
    /// (macOS `/tmp` is `/private/tmp`).
    fn holder_marks(&self) -> Vec<String> {
        let socket = self.env.iter().find(|(k, _)| *k == "HC_SOCKET");
        let socket = socket.map(|(_, v)| PathBuf::from(v)).unwrap_or_default();
        let mut marks = vec![holder_mark(&socket.to_string_lossy())];
        if let (Ok(real), Some(name)) = (fs::canonicalize(&self.dir), socket.file_name()) {
            let real = holder_mark(&real.join(name).to_string_lossy());
            if !marks.contains(&real) {
                marks.push(real);
            }
        }
        marks
    }

    /// Ends what this fixture's core left running. `server stop` reaches only a core that answers: a holder whose
    /// core was killed keeps its kept shells, and any hc call after the stop (a member's launcher, a client still
    /// reporting) starts a new core on this socket. Each round stops such a core again, then kills every holder of
    /// this fixture's socket and its descendants, checked by pid and start, and waits for them to go. A process
    /// still running after the rounds is returned, so a failed kill is reported rather than leaked.
    fn sweep(&mut self) -> Vec<(u32, Option<i64>)> {
        let host = mux_agent::proc::host();
        let marks = self.holder_marks();
        let mut left = Vec::new();
        for _ in 0..3 {
            if self.started {
                let _ = run(self.command(&["server", "stop", "--json"]), &self.dir);
            }
            let Ok(table) = host.table() else {
                return left;
            };
            left = leftovers(&table, &marks);
            if left.is_empty() {
                return left;
            }
            for &(pid, start) in &left {
                if let Some(opened) = host.open(pid).filter(|o| Some(o.start) == start) {
                    host.kill(opened);
                }
            }
            let gone =
                |&(pid, start): &(u32, Option<i64>)| host.open(pid).map(|o| o.start) != start;
            let end = Instant::now() + Duration::from_secs(10);
            while !left.iter().all(gone) && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(50));
            }
            left.retain(|p| !gone(p));
        }
        left
    }
}
impl Drop for PrivateCore {
    fn drop(&mut self) {
        let _ = self.stop();
        let left = self.sweep();
        if !left.is_empty() {
            let message = format!("private core processes still running: {left:?}");
            let _ = fs::write(self.dir.join("core-stop-failed"), &message);
            if std::thread::panicking() {
                eprintln!("{message}");
            } else {
                panic!("{message}");
            }
        }
    }
}
pub fn run(mut c: Command, dir: &Path) -> io::Result<Value> {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let out = dir.join(format!("core-{n}.out"));
    let err = dir.join(format!("core-{n}.err"));
    let mut child = c
        .stdin(Stdio::null())
        .stdout(fs::File::create(&out)?)
        .stderr(fs::File::create(&err)?)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "private core command",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if !status.success() {
        return Err(io::Error::other(fs::read_to_string(err)?));
    }
    serde_json::from_slice(&fs::read(out)?).map_err(io::Error::other)
}

/// (CI 85d70b5): `terminals` as `terminal_of` gives them, (client, hold) per member, in kill order: every hold
/// once, then every client. One hc holder can carry every member's terminal; a client killed while its holder lives
/// is an exit the holder records (Ended), never Lost, and a shared holder listed twice is signalled twice.
pub fn kill_order(terminals: &[(u32, Option<i64>)]) -> Vec<(u32, Option<i64>)> {
    assert!(
        terminals.len().is_multiple_of(2),
        "(client, hold) pairs: {terminals:?}"
    );
    let mut order = Vec::new();
    let holds = terminals.chunks(2).map(|p| p[1]);
    let clients = terminals.chunks(2).map(|p| p[0]);
    for t in holds.chain(clients) {
        if !order.contains(&t) {
            order.push(t);
        }
    }
    order
}

/// SIGKILL (Windows `taskkill /F /PID`, never `/T`) of one pid this test owns, bounded to 30 s; whether it succeeded.
pub fn kill_pid(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: a plain signal to a pid this test read from its own private core; no memory is shared.
        let r = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        if r != 0 {
            eprintln!("kill -9 {pid}: {}", io::Error::last_os_error());
        }
        r == 0
    }
    #[cfg(windows)]
    {
        let mut child = Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("taskkill");
        let deadline = Instant::now() + Duration::from_secs(30);
        let done = loop {
            if let Some(s) = child.try_wait().expect("taskkill wait") {
                break s;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("taskkill /F /PID {pid} did not finish");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if !done.success() {
            eprintln!("taskkill /F /PID {pid}: {done:?}");
        }
        done.success()
    }
}

/// The prefix of every holder socket of the core at `socket`: the core names each holder's socket after its own,
/// `<socket stem>.holder.<id>.sock` (`<dir>/h.holder.<id>.sock` beside `<dir>/h.sock`; on Windows
/// `\\.\pipe\<session>.holder.<id>.sock`). The stem ends in this fixture's unique dir or session, so no other
/// fixture's holder, and no core of the developer's own session, carries it.
pub fn holder_mark(socket: &str) -> String {
    format!("{}.holder.", socket.strip_suffix(".sock").unwrap_or(socket))
}

/// The processes this fixture's core left running, from one process table: each holder whose argv names a socket
/// starting with one of `marks`, and every descendant of one. Each comes with its start, so a pid reused before the
/// kill is never signalled. A row without a start is never chosen (it could not be checked again before the signal),
/// nor is a child that started before its parent: its parent link names an earlier process that had the pid.
pub fn leftovers(table: &[mux_agent::proc::Row], marks: &[String]) -> Vec<(u32, Option<i64>)> {
    let holder = |r: &mux_agent::proc::Row| {
        r.argv
            .iter()
            .flatten()
            .any(|a| marks.iter().any(|m| a.starts_with(m.as_str())))
    };
    let mut found: Vec<(u32, Option<i64>)> = table
        .iter()
        .filter(|r| r.start.is_some() && holder(r))
        .map(|r| (r.pid, r.start))
        .collect();
    let mut at = 0;
    while at < found.len() {
        let (parent, born) = found[at];
        for r in table.iter().filter(|r| r.ppid == parent) {
            let child = (r.pid, r.start);
            if r.start.is_some() && r.start >= born && !found.contains(&child) {
                found.push(child);
            }
        }
        at += 1;
    }
    found
}
