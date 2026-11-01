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

// Answers a matching call from its argv: stateful fakes (a pane table, a queue of probe answers, a failure).
type Script = Rc<dyn Fn(&[String]) -> Res<Output>>;

#[derive(Clone)]
enum Answer {
    Fixed(Output),
    Script(Script),
}

struct Rule {
    contains: String,
    answer: Answer,
}

// Sees each lock event (`+key` or `-key`) and answers text to record with it.
type Probe = Rc<dyn Fn(&str) -> String>;

// Runs after each sleep with how many there have been.
type Wake = Rc<dyn Fn(usize)>;

// What `stop_report` answers for the rows stop left.
type Report = fn(&[Left]) -> serde_json::Map<String, serde_json::Value>;

#[derive(Default)]
struct Log {
    rows: Vec<Row>,
    os_calls: Vec<String>,
    // OS and multiplexer calls in one order: OS calls as recorded, multiplexer calls as `mux <argv>`.
    events: Vec<String>,
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
    wake: Option<Wake>,
    // Lock keys starting with one of these time out, as a lock held elsewhere for 15 s would.
    timeouts: Vec<String>,
    // What `Mux::kind` answers; none is tmux.
    kind: Option<Kind>,
    // Shared with another thread's fakes by `share_locks`: a held key blocks there, as the real lock does.
    gate: Option<std::sync::Arc<Gate>>,
}

/// Locks two fakes on two threads contend for, as two processes contend for the real lock: a key held by one blocks
/// the other until it is given back. Counts each wait, so a test can show the contention happened.
#[derive(Default)]
pub struct Gate {
    state: std::sync::Mutex<GateState>,
    freed: std::sync::Condvar,
}

#[derive(Default)]
struct GateState {
    held: Vec<String>,
    waiting: HashMap<String, usize>,
    waited: HashMap<String, usize>,
}

impl Gate {
    /// Takes `key`, waiting (up to 10 s, then panics) while another thread holds it.
    fn take(&self, key: &str) {
        let mut state = self.state.lock().unwrap();
        if state.held.iter().any(|k| k == key) {
            *state.waiting.entry(key.into()).or_default() += 1;
            *state.waited.entry(key.into()).or_default() += 1;
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while state.held.iter().any(|k| k == key) {
                let left = deadline
                    .checked_duration_since(std::time::Instant::now())
                    .unwrap_or_else(|| panic!("lock {key} held for over 10 s"));
                state = self.freed.wait_timeout(state, left).unwrap().0;
            }
            *state.waiting.entry(key.into()).or_default() -= 1;
        }
        state.held.push(key.into());
    }

    /// Takes `key` when no thread holds it, never waiting; false when it is held.
    fn try_take(&self, key: &str) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.held.iter().any(|k| k == key) {
            return false;
        }
        state.held.push(key.into());
        true
    }

    fn give(&self, key: &str) {
        self.state.lock().unwrap().held.retain(|k| k != key);
        self.freed.notify_all();
    }

    /// Whether `key` is held now.
    pub fn held(&self, key: &str) -> bool {
        self.state.lock().unwrap().held.iter().any(|k| k == key)
    }

    /// How many threads wait for `key` now.
    pub fn waiting(&self, key: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .waiting
            .get(key)
            .copied()
            .unwrap_or(0)
    }

    /// How many times a thread had to wait for `key`.
    pub fn waited(&self, key: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .waited
            .get(key)
            .copied()
            .unwrap_or(0)
    }
}

thread_local! {
    // The fakes whose ctx was made last on this thread; `locked` records into them.
    static CURRENT: RefCell<Option<Fakes>> = const { RefCell::new(None) };
}

/// Stands in for `state::lock::with_lock` in tests: records the hold in the current fakes and runs `f`. A
/// second hold of a held key panics, as the real lock would time out on it; a key set by `lock_timeout` refuses
/// 2 `lock` as the real lock does after its wait. With a shared `Gate`, a key another thread holds is waited for.
pub fn locked<T>(key: &str, f: impl FnOnce() -> Res<T>) -> Res<T> {
    let fakes = CURRENT
        .with(|current| current.borrow().clone())
        .expect("a lock taken before Fakes::ctx on this thread");
    let timed_out = fakes.0.borrow().timeouts.iter().any(|t| key.starts_with(t));
    if timed_out {
        fakes.0.borrow_mut().locks.push(format!("!{key}"));
        return crate::error::refuse(2, "lock", "timed out waiting for the agent lock");
    }
    let gate = fakes.0.borrow().gate.clone();
    if let Some(gate) = &gate {
        gate.take(key);
    }
    fakes.lock_event('+', key);
    let out = f();
    fakes.lock_event('-', key);
    if let Some(gate) = &gate {
        gate.give(key);
    }
    out
}

/// Stands in for `state::lock::try_with_lock`: one attempt, never waiting. A key set by `lock_timeout`, or held by
/// another thread through a shared `Gate`, is busy (recorded as `?key`) and answers `None` without running `f`.
pub fn try_locked<T>(key: &str, f: impl FnOnce() -> Res<T>) -> Res<Option<T>> {
    let fakes = CURRENT
        .with(|current| current.borrow().clone())
        .expect("a lock taken before Fakes::ctx on this thread");
    let busy_here = fakes.0.borrow().timeouts.iter().any(|t| key.starts_with(t));
    let gate = fakes.0.borrow().gate.clone();
    if busy_here || gate.as_ref().is_some_and(|gate| !gate.try_take(key)) {
        fakes.0.borrow_mut().locks.push(format!("?{key}"));
        return Ok(None);
    }
    fakes.lock_event('+', key);
    let out = f();
    fakes.lock_event('-', key);
    if let Some(gate) = &gate {
        gate.give(key);
    }
    out.map(Some)
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
            answer: Answer::Fixed(output),
        });
    }

    /// A rule checked before every other, answered by `script` from the call's argv.
    pub fn mux_script(&self, contains: &str, script: impl Fn(&[String]) -> Res<Output> + 'static) {
        self.0.borrow_mut().rules.insert(
            0,
            Rule {
                contains: contains.into(),
                answer: Answer::Script(Rc::new(script)),
            },
        );
    }

    /// The multiplexer `Mux::kind` names.
    pub fn mux_kind(&self, kind: Kind) {
        self.0.borrow_mut().kind = Some(kind);
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

    /// OS and multiplexer calls in the order they were made.
    pub fn events(&self) -> Vec<String> {
        self.0.borrow().events.clone()
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

    /// Runs after every sleep with the count of sleeps so far, so a test changes what the next read sees.
    pub fn sleep_probe(&self, probe: impl Fn(usize) + 'static) {
        self.0.borrow_mut().wake = Some(Rc::new(probe));
    }

    /// Every later lock whose key starts with `prefix` times out (recorded as `!key`) without running its body.
    pub fn lock_timeout(&self, prefix: &str) {
        self.0.borrow_mut().timeouts.push(prefix.into());
    }

    /// Later locks go through `gate`, which another thread's fakes share: a key one holds blocks the other.
    pub fn share_locks(&self, gate: std::sync::Arc<Gate>) {
        self.0.borrow_mut().gate = Some(gate);
    }

    /// Forgets the recorded OS, multiplexer, sleep and lock calls.
    pub fn clear_log(&self) {
        let mut log = self.0.borrow_mut();
        log.os_calls.clear();
        log.mux_calls.clear();
        log.events.clear();
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
        let mut log = self.0.borrow_mut();
        log.events.push(call.clone());
        log.os_calls.push(call);
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
        let argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let line = args.join(" ");
        let answer = {
            let mut log = self.0.borrow_mut();
            log.mux_calls.push(argv.clone());
            log.events.push(format!("mux {line}"));
            log.rules
                .iter()
                .find(|r| line.contains(&r.contains))
                .map(|r| r.answer.clone())
        };
        // The log is released first, so a script may read the fakes.
        match answer {
            None => Ok(Output::default()),
            Some(Answer::Fixed(output)) => Ok(output),
            Some(Answer::Script(script)) => script(&argv),
        }
    }

    fn kind(&self) -> Kind {
        self.0.borrow().kind.unwrap_or(Kind::Tmux)
    }
}

impl Clock for Fakes {
    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(self.0.borrow().now_ms)
    }

    fn sleep(&self, duration: Duration) {
        let wake = {
            let mut log = self.0.borrow_mut();
            let ms = duration.as_millis() as u64;
            log.slept.push(ms);
            log.now_ms += ms;
            log.wake.clone().map(|wake| (wake, log.slept.len()))
        };
        // The log is released first, so the probe may read the fakes.
        if let Some((wake, count)) = wake {
            wake(count);
        }
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
