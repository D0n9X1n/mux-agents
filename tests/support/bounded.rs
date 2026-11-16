//! outcome-2 call 14 (build checklist 4, o2 call 23): every child a live harness or the fake client starts finishes
//! within a bound or is killed. `output` bounds one call (refused `TimedOut`, naming the command); `spawn` bounds a
//! child a test keeps (a watchdog kills it at its bound). Included by the live harnesses and, through `#[path]`, the
//! fake client.
#![allow(dead_code)] // each crate that includes it uses part of it
use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant};

/// The bound of one multiplexer, core or client call (the harness's own reply wait).
pub const LIMIT: Duration = Duration::from_secs(60);
/// The bound of a child a test keeps for its whole run, or of a release build.
pub const HELD: Duration = Duration::from_secs(30 * 60);
const TICK: Duration = Duration::from_millis(20);
/// How long a pipe a grandchild still holds is read after the child's exit.
const GRACE: Duration = Duration::from_secs(5);

/// `command.output()`, bounded: stdin null (as `output()`), both pipes drained on threads (a full pipe never stalls
/// the child), the child killed and reaped at `limit` and refused `TimedOut`; a pipe still held by a grandchild
/// `GRACE` after the exit is dropped.
pub fn output(command: &mut Command, limit: Duration) -> io::Result<Output> {
    let what = format!("{command:?}");
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let end = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < end => std::thread::sleep(TICK),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{what} ran past {limit:?}"),
                ));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
    };
    Ok(Output {
        status,
        stdout: out.recv_timeout(GRACE).unwrap_or_default(),
        stderr: err.recv_timeout(GRACE).unwrap_or_default(),
    })
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut bytes);
        }
        let _ = tx.send(bytes);
    });
    rx
}

/// A child a test keeps, with a watchdog that kills and reaps it at its bound. Dropping it neither kills nor waits
/// (a detached child outlives its handle); the watchdog still bounds it while this process runs.
pub struct Watched {
    child: Arc<Mutex<Child>>,
    id: u32,
}

/// `command.spawn()`, its stdio as the caller set it, with a watchdog that kills the child at `limit`.
pub fn spawn(command: &mut Command, limit: Duration) -> io::Result<Watched> {
    let child = command.spawn()?;
    let id = child.id();
    let child = Arc::new(Mutex::new(child));
    let dog = child.clone();
    let end = Instant::now() + limit;
    std::thread::spawn(move || {
        loop {
            {
                let mut c = guard(&dog);
                match c.try_wait() {
                    Ok(None) if Instant::now() < end => {}
                    Ok(None) => {
                        let _ = c.kill();
                        let _ = c.wait();
                        return;
                    }
                    _ => return,
                }
            }
            std::thread::sleep(TICK);
        }
    });
    Ok(Watched { child, id })
}

fn guard(child: &Mutex<Child>) -> MutexGuard<'_, Child> {
    child
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Watched {
    pub fn id(&self) -> u32 {
        self.id
    }
    pub fn kill(&mut self) -> io::Result<()> {
        guard(&self.child).kill()
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        guard(&self.child).try_wait()
    }
    /// Polls without holding the child, so the watchdog can still kill it: bounded by the watchdog.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            std::thread::sleep(TICK);
        }
    }
}
