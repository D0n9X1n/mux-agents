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

// Sees each lock event (`+key` or `-key`) and answers text to record with it.
type Probe = Rc<dyn Fn(&str) -> String>;

// What `stop_report` answers for the rows stop left.
type Report = fn(&[Left]) -> serde_json::Map<String, serde_json::Value>;

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
    // `+key` on taking a lock and `-key` on releasing it, each followed by the probe's answer when one is set.
    locks: Vec<String>,
    held: Vec<String>,
    // Pids whose kill shows an exit; any other kill fails.
    kills: Vec<u32>,
    report: Option<Report>,
    probe: Option<Probe>,
}

thread_local! {
    // The fakes whose ctx was made last on this thread; `locked` records into them.
    static CURRENT: RefCell<Option<Fakes>> = const { RefCell::new(None) };
}

/// Stands in for `state::lock::with_lock` in tests: records the hold in the current fakes and runs `f`. A
/// second hold of a held key panics, as the real lock would time out on it.
pub fn locked<T>(key: &str, f: impl FnOnce() -> Res<T>) -> Res<T> {
    let fakes = CURRENT
        .with(|current| current.borrow().clone())
        .expect("a lock taken before Fakes::ctx on this thread");
    fakes.lock_event('+', key);
    let out = f();
    fakes.lock_event('-', key);
    out
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

    /// A rule checked before every earlier one, so a test can change an answer mid-way.
    pub fn mux_first(&self, contains: &str, code: i32, stdout: &str, stderr: &str) {
        self.mux_rule(contains, code, stdout, stderr);
        let mut log = self.0.borrow_mut();
        let rule = log.rules.pop().expect("pushed above");
        log.rules.insert(0, rule);
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

    /// A kill of this pid shows the exit.
    pub fn kill_ok(&self, pid: u32) {
        self.0.borrow_mut().kills.push(pid);
    }

    /// What `stop_report` answers; none answers an empty map.
    pub fn stop_report_with(
        &self,
        report: fn(&[Left]) -> serde_json::Map<String, serde_json::Value>,
    ) {
        self.0.borrow_mut().report = Some(report);
    }

    /// Moves the last listed row to the front, so `row` answers it for its pid.
    pub fn prefer_last(&self) {
        let mut log = self.0.borrow_mut();
        let last = log.rows.pop().expect("a listed row");
        log.rows.insert(0, last);
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

    /// The lock events `locked` recorded, in order.
    pub fn locks(&self) -> Vec<String> {
        self.0.borrow().locks.clone()
    }

    /// Runs at every lock event with the event (`+key` or `-key`); a non-empty answer is added to the event, so a
    /// test sees, or changes, state while a lock is held.
    pub fn lock_probe(&self, probe: impl Fn(&str) -> String + 'static) {
        self.0.borrow_mut().probe = Some(Rc::new(probe));
    }

    /// Forgets the recorded OS, multiplexer, sleep and lock calls.
    pub fn clear_log(&self) {
        let mut log = self.0.borrow_mut();
        log.os_calls.clear();
        log.mux_calls.clear();
        log.slept.clear();
        log.locks.clear();
    }

    /// Moves the clock on without a sleep.
    pub fn advance(&self, by: Duration) {
        self.0.borrow_mut().now_ms += by.as_millis() as u64;
    }

    pub fn ctx(&self) -> Ctx {
        CURRENT.with(|current| *current.borrow_mut() = Some(self.clone()));
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

    fn lock_event(&self, sign: char, key: &str) {
        if sign == '+' {
            assert!(
                !self.0.borrow().held.iter().any(|k| k == key),
                "lock {key} taken while held"
            );
        }
        let probe = self.0.borrow().probe.clone();
        let event = format!("{sign}{key}");
        let seen = probe
            .map(|probe| probe(&event))
            .filter(|seen| !seen.is_empty());
        let mut log = self.0.borrow_mut();
        if sign == '+' {
            log.held.push(key.into());
        } else {
            log.held.retain(|k| k != key);
        }
        log.locks.push(match seen {
            Some(seen) => format!("{event} {seen}"),
            None => event,
        });
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
            None => self
                .listed(pid)
                .and_then(|r| r.start.map(|start| Opened::new(r.pid, start, None))),
        }
    }

    fn kill(&self, opened: Opened) -> bool {
        self.os_call(format!("kill {}", opened.pid));
        self.0.borrow().kills.contains(&opened.pid)
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

    fn stop_report(&self, left: &[Left]) -> serde_json::Map<String, serde_json::Value> {
        let report = self.0.borrow().report;
        report.map_or_else(serde_json::Map::new, |report| report(left))
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

/// A row whose start is unknown (Windows: listed by Toolhelp but not openable).
pub fn unknown_start(pid: u32, ppid: u32) -> Row {
    Row {
        start: None,
        ..row(pid, ppid, 0)
    }
}

/// A row with no path or argv.
pub fn row(pid: u32, ppid: u32, start: i64) -> Row {
    Row {
        pid,
        ppid,
        start: Some(start),
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
