//! Recording fakes for the `Ctx` seams; clones share one log, so a test keeps a handle after boxing them.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use crate::ctx::{Clock, Ctx, Env};
use crate::error::Res;
use crate::mux::{Kind, Mux, Output};
use crate::parse::Client;
use crate::proc::{Left, Opened, Os, Row, Signals, Token};

struct Rule {
    contains: String,
    output: Output,
}

#[derive(Default)]
struct Log {
    rows: Vec<Row>,
    os_calls: Vec<String>,
    rules: Vec<Rule>,
    mux_calls: Vec<Vec<String>>,
    slept: Vec<u64>,
    // What sandbox() answers; none is an unsandboxed macOS.
    signals: Option<Signals>,
    // Milliseconds since the epoch; sleeping advances it.
    now_ms: u64,
    clients: HashMap<u32, Client>,
    shells: Vec<u32>,
    cwds: HashMap<u32, String>,
    // Starts `open` answers in turn (none: unreadable) before it falls back to the row's own.
    opens: HashMap<u32, VecDeque<Option<i64>>>,
}

#[derive(Clone, Default)]
pub struct Fakes(Rc<RefCell<Log>>);

impl Fakes {
    /// A process the fake table lists, opens and answers `row` for.
    pub fn process(&self, row: Row) {
        self.0.borrow_mut().rows.push(row);
    }

    /// The first rule whose text appears in the space-joined argv answers; anything else exits 0 with no output.
    pub fn mux_rule(&self, contains: &str, code: i32, stdout: &str, stderr: &str) {
        let output = Output {
            code,
            stdout: stdout.into(),
            stderr: stderr.into(),
        };
        self.0.borrow_mut().rules.push(Rule {
            contains: contains.into(),
            output,
        });
    }

    /// The client `client_of` names for this pid.
    pub fn client(&self, pid: u32, client: Client) {
        self.0.borrow_mut().clients.insert(pid, client);
    }

    /// A pid `is_shell` answers true for.
    pub fn shell(&self, pid: u32) {
        self.0.borrow_mut().shells.push(pid);
    }

    /// The kernel cwd `Os::cwd` answers for this pid.
    pub fn kernel_cwd(&self, pid: u32, cwd: &str) {
        self.0.borrow_mut().cwds.insert(pid, cwd.into());
    }

    /// The starts the next `open` calls of this pid answer, in order; none is an unreadable start.
    pub fn opens(&self, pid: u32, starts: &[Option<i64>]) {
        self.0
            .borrow_mut()
            .opens
            .entry(pid)
            .or_default()
            .extend(starts.iter().copied());
    }

    /// What the fake OS says about a sandbox.
    pub fn signals(&self, signals: Signals) {
        self.0.borrow_mut().signals = Some(signals);
    }

    pub fn os_calls(&self) -> Vec<String> {
        self.0.borrow().os_calls.clone()
    }

    pub fn mux_calls(&self) -> Vec<Vec<String>> {
        self.0.borrow().mux_calls.clone()
    }

    pub fn slept(&self) -> Vec<u64> {
        self.0.borrow().slept.clone()
    }

    pub fn ctx(&self) -> Ctx {
        Ctx {
            env: Env::default(),
            pid: 4242,
            cwd: PathBuf::from("/work"),
            home: PathBuf::from("/home/user"),
            state_root: PathBuf::from("/state"),
            exe: PathBuf::from("/bin/mux-agent"),
            os: Box::new(self.clone()),
            mux: RefCell::new(Some(Box::new(self.clone()))),
            clock: Box::new(self.clone()),
            admitted: false,
        }
    }
}

impl Fakes {
    fn os_call(&self, call: String) {
        self.0.borrow_mut().os_calls.push(call);
    }

    fn listed(&self, pid: u32) -> Option<Row> {
        self.0.borrow().rows.iter().find(|r| r.pid == pid).cloned()
    }
}

// Every answer comes from the registered rows and per-pid answers; nothing is ever killed.
impl Os for Fakes {
    fn row(&self, pid: u32) -> Option<Row> {
        self.os_call(format!("row {pid}"));
        self.listed(pid)
    }

    fn table(&self) -> std::io::Result<Vec<Row>> {
        self.os_call("table".into());
        Ok(self.0.borrow().rows.clone())
    }

    fn open(&self, pid: u32) -> Option<Opened> {
        self.os_call(format!("open {pid}"));
        let queued = self
            .0
            .borrow_mut()
            .opens
            .get_mut(&pid)
            .and_then(VecDeque::pop_front);
        match queued {
            Some(start) => start.map(|start| Opened::new(pid, start, None)),
            None => self.listed(pid).map(|r| Opened::new(r.pid, r.start, None)),
        }
    }

    fn kill(&self, opened: Opened) -> bool {
        self.os_call(format!("kill {}", opened.pid));
        false
    }

    fn cwd(&self, pid: u32) -> Option<String> {
        self.os_call(format!("cwd {pid}"));
        self.0.borrow().cwds.get(&pid).cloned()
    }

    fn client_of(&self, row: &Row) -> Option<Client> {
        self.os_call(format!("client_of {}", row.pid));
        self.0.borrow().clients.get(&row.pid).copied()
    }

    fn is_shell(&self, row: &Row) -> bool {
        self.os_call(format!("is_shell {}", row.pid));
        self.0.borrow().shells.contains(&row.pid)
    }

    fn same_start(&self, opened: i64, listed: i64) -> bool {
        opened == listed
    }

    fn stop_report(&self, _: &[Left]) -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::new()
    }

    fn token(&self) -> Option<Token> {
        None
    }

    fn sandbox(&self) -> Signals {
        self.os_call("sandbox".into());
        let signals = self.0.borrow().signals.clone();
        signals.unwrap_or(Signals::SandboxCheck(0))
    }
}

/// A row with no path or argv.
pub fn row(pid: u32, ppid: u32, start: i64) -> Row {
    Row {
        pid,
        ppid,
        start,
        path: None,
        argv: None,
    }
}

impl Mux for Fakes {
    fn run(&self, args: &[&str]) -> Res<Output> {
        let mut log = self.0.borrow_mut();
        log.mux_calls
            .push(args.iter().map(|a| a.to_string()).collect());
        let line = args.join(" ");
        Ok(log
            .rules
            .iter()
            .find(|r| line.contains(&r.contains))
            .map(|r| r.output.clone())
            .unwrap_or_default())
    }

    fn kind(&self) -> Kind {
        Kind::Tmux
    }
}

impl Clock for Fakes {
    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(self.0.borrow().now_ms)
    }

    fn sleep(&self, duration: Duration) {
        let mut log = self.0.borrow_mut();
        let ms = duration.as_millis() as u64;
        log.slept.push(ms);
        log.now_ms += ms;
    }
}

/// A fresh folder under the canonical temp dir (macOS /var is a link to /private/var), removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> TempDir {
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = base.join(format!("mux-agent-test-{}", crate::util::hex_id().unwrap()));
        std::fs::create_dir(&path).unwrap();
        TempDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
