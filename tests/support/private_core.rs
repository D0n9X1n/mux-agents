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
    /// 6b ambiguity 14: the core killed outright (SIGKILL; Windows `taskkill /F`, never `/T`, so the terminals it ran
    /// stay as recovery shells), then started again. The pid comes from `server status --json` (r6b call 2).
    pub fn kill_hard(&mut self) {
        let status =
            run(self.command(&["server", "status", "--json"]), &self.dir).expect("core status");
        let pid = status["pid"]
            .as_u64()
            .expect("hc server status --json names the core pid") as u32;
        #[cfg(unix)]
        {
            // SAFETY: a plain signal to the pid the private core reports for itself; no memory is shared.
            let r = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            assert_eq!(r, 0, "kill -9 {pid}: {}", io::Error::last_os_error());
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
            assert!(done.success(), "taskkill /F /PID {pid}: {done:?}");
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
    }
    pub fn start(&mut self) {
        self.owned = true;
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
impl Drop for PrivateCore {
    fn drop(&mut self) {
        let _ = self.stop();
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
