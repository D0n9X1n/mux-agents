//! outcome-2 call 14 (build checklist 4, o2 call 23): every child a live harness or the fake client starts finishes
//! within a bound or is killed. `output` bounds one call (refused `TimedOut`, naming the command); `spawn` bounds a
//! child a test keeps (a watchdog kills it at its bound). Included by the live harnesses and, through `#[path]`, the
//! fake client.
#![allow(dead_code)] // each crate that includes it uses part of it
use std::fs::File;
use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant};

/// The bound of one multiplexer, core or client call (the harness's own reply wait).
pub const LIMIT: Duration = Duration::from_secs(60);
/// The bound of a child a test keeps for its whole run, or of a release build.
pub const HELD: Duration = Duration::from_secs(30 * 60);
const TICK: Duration = Duration::from_millis(20);
/// How long a pipe a grandchild still holds is read after the child's exit.
const GRACE: Duration = Duration::from_secs(5);

/// How long a stopped reader has to drop its pipe (o2 call 29).
const RELEASE: Duration = Duration::from_secs(1);

/// `command.output()`, bounded: stdin null (as `output()`), both pipes drained on threads (a full pipe never stalls
/// the child), the child killed and reaped at `limit` and refused `TimedOut`. o2 call 29: never success with output it
/// did not finish reading: a pipe a descendant still holds open `GRACE` after the child's exit is refused `TimedOut`
/// naming that pipe, and its reader is stopped and drops its read handle.
pub fn output(command: &mut Command, limit: Duration) -> io::Result<Output> {
    let what = format!("{command:?}");
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut out = Drain::start(child.stdout.take().map(file_of));
    let mut err = Drain::start(child.stderr.take().map(file_of));
    let end = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < end => std::thread::sleep(TICK),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                out.release();
                err.release();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{what} ran past {limit:?}"),
                ));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                out.release();
                err.release();
                return Err(e);
            }
        }
    };
    let grace = Instant::now() + GRACE;
    let stdout = out.finish(grace);
    let stderr = err.finish(grace);
    match (stdout, stderr) {
        (Ok(stdout), Ok(stderr)) => Ok(Output {
            status,
            stdout,
            stderr,
        }),
        (Err(released), _) => Err(held(&what, "stdout", released)),
        (_, Err(released)) => Err(held(&what, "stderr", released)),
    }
}

/// o2 call 29: the refusal for a pipe still open `GRACE` after the child exited.
fn held(what: &str, pipe: &str, released: bool) -> io::Error {
    let stuck = if released {
        ""
    } else {
        " (its reader could not be stopped)"
    };
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "{what}: {pipe} still held open {GRACE:?} after the child exited; its output was not all read{stuck}"
        ),
    )
}

/// One pipe read to its end on a thread, which can be stopped so it drops its read handle.
struct Drain {
    bytes: mpsc::Receiver<Vec<u8>>,
    stop: Arc<AtomicBool>,
    reader: Option<std::thread::JoinHandle<()>>,
    /// Windows: the pipe, so a blocked read on it can be cancelled.
    #[cfg(windows)]
    pipe: Option<Arc<File>>,
}

impl Drain {
    fn start(pipe: Option<File>) -> Drain {
        let (tx, bytes) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let pipe = pipe.map(Arc::new);
        #[cfg(windows)]
        let (read, kept) = (pipe.clone(), pipe);
        #[cfg(unix)]
        let read = pipe;
        let halt = stop.clone();
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(p) = read {
                read_until_stopped(&p, &halt, &mut out);
            }
            let _ = tx.send(out);
        });
        Drain {
            bytes,
            stop,
            reader: Some(reader),
            #[cfg(windows)]
            pipe: kept,
        }
    }

    /// The whole output, or, when the pipe is still open at `deadline`, `Err(released)`: the reader is stopped and
    /// `released` says whether it dropped its read handle within `RELEASE`.
    fn finish(mut self, deadline: Instant) -> Result<Vec<u8>, bool> {
        let left = deadline.saturating_duration_since(Instant::now());
        match self.bytes.recv_timeout(left) {
            Ok(bytes) => Ok(bytes),
            Err(_) => Err(self.release()),
        }
    }

    /// Stops the reader and waits `RELEASE` for it to end (dropping its read handle); true when it did.
    fn release(&mut self) -> bool {
        self.stop.store(true, Ordering::SeqCst);
        let Some(reader) = self.reader.take() else {
            return true;
        };
        let end = Instant::now() + RELEASE;
        while !reader.is_finished() {
            if Instant::now() >= end {
                return false;
            }
            #[cfg(windows)]
            cancel(self.pipe.as_deref(), &reader);
            std::thread::sleep(TICK);
        }
        let _ = reader.join();
        true
    }
}

#[cfg(unix)]
fn file_of(pipe: impl Into<std::os::fd::OwnedFd>) -> File {
    File::from(pipe.into())
}

#[cfg(windows)]
fn file_of(pipe: impl Into<std::os::windows::io::OwnedHandle>) -> File {
    File::from(pipe.into())
}

/// Unix: poll in 100 ms steps, so a stop is seen while a descendant holds the pipe open and silent.
#[cfg(unix)]
fn read_until_stopped(pipe: &File, stop: &AtomicBool, out: &mut Vec<u8>) {
    use std::os::fd::AsRawFd;
    let mut buf = [0u8; 8192];
    while !stop.load(Ordering::SeqCst) {
        let mut fd = libc::pollfd {
            fd: pipe.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd for the call.
        let ready = unsafe { libc::poll(&mut fd, 1, 100) };
        if ready < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return;
        }
        if ready <= 0 {
            continue;
        }
        match (&*pipe).read(&mut buf) {
            Ok(0) => return,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

/// Windows: blocking reads; `cancel` ends a blocked one once the drain is stopped.
#[cfg(windows)]
fn read_until_stopped(pipe: &File, stop: &AtomicBool, out: &mut Vec<u8>) {
    let mut buf = [0u8; 8192];
    while !stop.load(Ordering::SeqCst) {
        match (&*pipe).read(&mut buf) {
            Ok(0) => return,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

/// Windows: cancels the reader's blocked read on the pipe (overlapped or synchronous).
#[cfg(windows)]
fn cancel(pipe: Option<&File>, reader: &std::thread::JoinHandle<()>) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::{CancelIoEx, CancelSynchronousIo};
    // SAFETY: both handles are live: the pipe is held by `Drain`, the thread by its JoinHandle.
    unsafe {
        if let Some(p) = pipe {
            CancelIoEx(p.as_raw_handle(), std::ptr::null());
        }
        CancelSynchronousIo(reader.as_raw_handle());
    }
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
