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
            "RMUX_ALLOW_INTERNAL_DAEMON_IN_CALLER_JOB",
        ]
        .into_iter()
        .filter_map(|k| std::env::var_os(k).map(|v| (k, v)))
        .collect();
        for (k, sub) in [
            ("HOME", "home"),
            ("USERPROFILE", "home"),
            ("APPDATA", "appdata"),
            ("LOCALAPPDATA", "localappdata"),
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
