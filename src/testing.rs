//! Recording fakes for the `Ctx` seams; clones share one log, so a test keeps a handle after boxing them.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use crate::ctx::{Clock, Ctx, Env};
use crate::error::Res;
use crate::mux::{Mux, Output};
use crate::proc::Os;

struct Rule {
    contains: String,
    output: Output,
}

#[derive(Default)]
struct Log {
    alive: Vec<u32>,
    os_calls: Vec<String>,
    rules: Vec<Rule>,
    mux_calls: Vec<Vec<String>>,
    slept: Vec<u64>,
    // Milliseconds since the epoch; sleeping advances it.
    now_ms: u64,
}

#[derive(Clone, Default)]
pub struct Fakes(Rc<RefCell<Log>>);

impl Fakes {
    pub fn alive(&self, pid: u32) {
        self.0.borrow_mut().alive.push(pid);
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

impl Os for Fakes {
    fn is_alive(&self, pid: u32) -> bool {
        let mut log = self.0.borrow_mut();
        log.os_calls.push(format!("is_alive {pid}"));
        log.alive.contains(&pid)
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
