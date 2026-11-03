//! The side bar's own terminal: raw mode and mouse reporting while `watch` runs, restored on every way it ends.
#![allow(dead_code)] // Until T9 gives every item a caller (#61).

use std::io;
use std::time::Duration;

use crate::ctx::{Clock, Env};
use crate::paint;

/// What one wait for input saw: bytes, nothing before the timeout, or a terminal that can no longer be read.
#[derive(Debug, PartialEq, Eq)]
pub enum Waited {
    Bytes(Vec<u8>),
    Timeout,
    Closed,
}

/// Where `watch` waits between ticks: it answers early with bytes the user typed or clicked.
pub trait Input {
    fn wait(&mut self, clock: &dyn Clock, timeout: Duration) -> Waited;
}

/// No terminal to read: each wait sleeps its timeout and times out. A zero timeout does not sleep at all.
pub struct NoInput;

impl Input for NoInput {
    fn wait(&mut self, clock: &dyn Clock, timeout: Duration) -> Waited {
        if !timeout.is_zero() {
            clock.sleep(timeout);
        }
        Waited::Timeout
    }
}

/// Whether to read the side bar's terminal at all: only when stdin and stdout are both terminals and `TERM` is not
/// `dumb`. A pipe at either end, or a terminal that takes no mode controls, keeps the bar draw-only.
pub fn wanted(stdin_tty: bool, stdout_tty: bool, dumb: bool) -> bool {
    stdin_tty && stdout_tty && !dumb
}

/// Calls `setup` only when `wanted`; `None` from it, or no call, is `NoInput`.
pub(crate) fn open_with(
    env: &Env,
    stdin_tty: bool,
    stdout_tty: bool,
    setup: impl FnOnce() -> Option<Box<dyn Input>>,
) -> Box<dyn Input> {
    if wanted(stdin_tty, stdout_tty, paint::dumb(env))
        && let Some(input) = setup()
    {
        return input;
    }
    Box::new(NoInput)
}

/// The side bar's input: its own terminal when stdin and stdout are both terminals and `TERM` is not `dumb`, else
/// `NoInput`. A terminal that fails to set up is `NoInput` too.
pub fn open(env: &Env) -> Box<dyn Input> {
    use std::io::IsTerminal;
    open_with(
        env,
        io::stdin().is_terminal(),
        io::stdout().is_terminal(),
        platform_setup,
    )
}

/// Unix: the terminal on stdin and stdout, set up.
#[cfg(unix)]
fn platform_setup() -> Option<Box<dyn Input>> {
    TermInput::setup(Tty::stdio()).map(|input| Box::new(input) as Box<dyn Input>)
}

/// Windows: draw-only, never reading the console (#70).
#[cfg(windows)]
fn platform_setup() -> Option<Box<dyn Input>> {
    None
}

#[cfg(unix)]
pub use unix::Tty;

#[cfg(unix)]
mod unix {
    use std::cell::UnsafeCell;
    use std::io;
    use std::mem::MaybeUninit;
    use std::os::fd::RawFd;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
    use std::time::{Duration, Instant};

    use super::{DISABLE, Term, Waited};
    use crate::ctx::Clock;

    // The signals that end `watch` from outside; each restores the terminal before it ends the process.
    const SIGNALS: [libc::c_int; 4] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT];

    // The terminal mode `save` read, which `raw` changes a copy of and `restore` and the signal handler put back.
    struct Saved(UnsafeCell<MaybeUninit<libc::termios>>);

    // SAFETY: only `save` writes it, while it holds `OWNED` and before `ARMED` is set; every read comes after that
    // write, from the same `Tty` or from the handler once it sees `ARMED` (Acquire against `arm`'s Release).
    unsafe impl Sync for Saved {}

    static SAVED: Saved = Saved(UnsafeCell::new(MaybeUninit::uninit()));
    // Whether the signal handler restores the terminal: set by `arm`, cleared by `disarm`.
    static ARMED: AtomicBool = AtomicBool::new(false);
    // Whether a `Tty` holds the globals: one per process, from `save` to `disarm`.
    static OWNED: AtomicBool = AtomicBool::new(false);
    static IN_FD: AtomicI32 = AtomicI32::new(-1);
    static OUT_FD: AtomicI32 = AtomicI32::new(-1);

    /// The handler for `SIGNALS`: puts the saved mode back and turns mouse reports off, then ends the process by
    /// the same signal under its default action. Only async-signal-safe calls (tcsetattr, write, signal, raise).
    extern "C" fn on_signal(signal: libc::c_int) {
        if ARMED.load(Ordering::Acquire) {
            let input = IN_FD.load(Ordering::Relaxed);
            let output = OUT_FD.load(Ordering::Relaxed);
            // SAFETY: `ARMED` means `save` wrote `SAVED` and the descriptors first; nothing writes them while armed.
            // A failed call is ignored: the process ends either way.
            unsafe {
                libc::tcsetattr(input, libc::TCSANOW, (*SAVED.0.get()).as_ptr());
                libc::write(output, DISABLE.as_ptr().cast(), DISABLE.len());
            }
        }
        // SAFETY: back to the default action, then raised again: the signal stays blocked until this handler
        // returns, and then ends the process.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        }
    }

    /// One terminal read from `input` and written to `output`: the real `Term`. Only one may hold the terminal per
    /// process, since the signal handler restores it from globals. It does not own or close the descriptors.
    pub struct Tty {
        input: RawFd,
        output: RawFd,
        // The actions `arm` replaced, one per `SIGNALS`, which `disarm` puts back.
        old: [Option<libc::sigaction>; 4],
    }

    impl Tty {
        /// The process's own terminal: stdin and stdout.
        pub fn stdio() -> Tty {
            Tty::on(libc::STDIN_FILENO, libc::STDOUT_FILENO)
        }

        pub(crate) fn on(input: RawFd, output: RawFd) -> Tty {
            Tty {
                input,
                output,
                old: [None; 4],
            }
        }

        // A copy of the mode `save` read.
        fn saved(&self) -> libc::termios {
            // SAFETY: `TermInput` calls `raw` and `restore`, the only callers, after a `save` that wrote it.
            unsafe { (*SAVED.0.get()).assume_init() }
        }

        fn set(&self, mode: &libc::termios) -> io::Result<()> {
            // SAFETY: `mode` is a valid termios for the call.
            if unsafe { libc::tcsetattr(self.input, libc::TCSANOW, mode) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Term for Tty {
        /// Claims the globals, then reads the mode to restore; a second `Tty`, or a failed read, is an error.
        fn save(&mut self) -> io::Result<()> {
            if OWNED
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return Err(io::Error::other("a terminal is already set up"));
            }
            let mut mode = MaybeUninit::<libc::termios>::uninit();
            // SAFETY: `mode` is valid for tcgetattr to fill in; it is read only when that succeeds.
            if unsafe { libc::tcgetattr(self.input, mode.as_mut_ptr()) } != 0 {
                let error = io::Error::last_os_error();
                OWNED.store(false, Ordering::Release);
                return Err(error);
            }
            // SAFETY: `OWNED` is ours and `ARMED` is not set, so nothing else reads or writes `SAVED` now.
            unsafe { (*SAVED.0.get()).write(mode.assume_init()) };
            IN_FD.store(self.input, Ordering::Relaxed);
            OUT_FD.store(self.output, Ordering::Relaxed);
            Ok(())
        }

        /// Arms the handler, then installs it for each of `SIGNALS`, keeping the actions it replaces. A signal ignored
        /// when `watch` started (SIGHUP under nohup) is left ignored: it would not have ended the process either.
        fn arm(&mut self) -> io::Result<()> {
            ARMED.store(true, Ordering::Release);
            for (signal, old) in SIGNALS.into_iter().zip(&mut self.old) {
                // SAFETY: a zeroed sigaction is a valid value (empty mask, no flags) to fill in.
                let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
                // SAFETY: a null new action only reads the current one into `current`.
                if unsafe { libc::sigaction(signal, std::ptr::null(), &mut current) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                if current.sa_sigaction == libc::SIG_IGN {
                    continue;
                }
                // SAFETY: as above, for the action to install.
                let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
                action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
                // SAFETY: as above, for the old action sigaction fills in.
                let mut replaced: libc::sigaction = unsafe { std::mem::zeroed() };
                // SAFETY: both pointers are valid for the call; the handler makes only async-signal-safe calls.
                if unsafe { libc::sigaction(signal, &action, &mut replaced) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                *old = Some(replaced);
            }
            Ok(())
        }

        /// The saved mode without line editing, echo or signal keys: each byte is read as it comes.
        fn raw(&mut self) -> io::Result<()> {
            let mut mode = self.saved();
            mode.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
            mode.c_cc[libc::VMIN] = 1;
            mode.c_cc[libc::VTIME] = 0;
            self.set(&mode)
        }

        /// Writes every byte, retrying when a signal interrupts the write.
        fn write(&mut self, mut bytes: &[u8]) -> io::Result<()> {
            while !bytes.is_empty() {
                // SAFETY: `bytes` is valid for its length.
                let n = unsafe { libc::write(self.output, bytes.as_ptr().cast(), bytes.len()) };
                if n < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                if n == 0 {
                    return Err(io::ErrorKind::WriteZero.into());
                }
                bytes = &bytes[n as usize..];
            }
            Ok(())
        }

        /// Puts the saved mode back; a failure is ignored, as there is nothing left to try.
        fn restore(&mut self) {
            let _ = self.set(&self.saved());
        }

        /// Disarms the handler, puts back the actions `arm` replaced, then gives up the globals.
        fn disarm(&mut self) {
            ARMED.store(false, Ordering::Release);
            for (signal, old) in SIGNALS.into_iter().zip(&mut self.old) {
                if let Some(old) = old.take() {
                    // SAFETY: `old` is the action sigaction answered for this signal.
                    unsafe { libc::sigaction(signal, &old, std::ptr::null_mut()) };
                }
            }
            OWNED.store(false, Ordering::Release);
        }

        /// Waits up to `timeout`, by the real time and not `clock`, for bytes: a signal mid-wait keeps waiting to the
        /// same deadline. A zero timeout still polls once, so bytes already waiting are read. EOF, or a hang-up or
        /// error without bytes, is `Closed`.
        fn poll_read(&mut self, _clock: &dyn Clock, timeout: Duration) -> io::Result<Waited> {
            let deadline = Instant::now() + timeout;
            let mut polled = false;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() && polled {
                    return Ok(Waited::Timeout);
                }
                polled = true;
                // Rounded up, so poll never wakes early and spins on a deadline less than a millisecond away.
                let ms = left
                    .as_nanos()
                    .div_ceil(1_000_000)
                    .min(libc::c_int::MAX as u128);
                let mut fd = libc::pollfd {
                    fd: self.input,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one pollfd, valid for the call.
                let ready = unsafe { libc::poll(&mut fd, 1, ms as libc::c_int) };
                if ready < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                if ready == 0 {
                    continue;
                }
                if fd.revents & libc::POLLIN == 0 {
                    if fd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                        return Ok(Waited::Closed);
                    }
                    continue;
                }
                let mut buf = [0u8; 256];
                // SAFETY: `buf` has room for the `buf.len()` bytes read asks for.
                let n = unsafe { libc::read(self.input, buf.as_mut_ptr().cast(), buf.len()) };
                if n < 0 {
                    let error = io::Error::last_os_error();
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) {
                        continue;
                    }
                    return Err(error);
                }
                if n == 0 {
                    return Ok(Waited::Closed);
                }
                return Ok(Waited::Bytes(buf[..n as usize].to_vec()));
            }
        }
    }
}

/// Turns on mouse press and release reports in SGR form; `DISABLE` turns them off in reverse.
pub const ENABLE: &[u8] = b"\x1b[?1000h\x1b[?1006h";
pub const DISABLE: &[u8] = b"\x1b[?1006l\x1b[?1000l";

/// The terminal calls `TermInput` makes, one per setup step and one per undo, so a test can fail any of them.
pub trait Term {
    fn save(&mut self) -> io::Result<()>;
    fn arm(&mut self) -> io::Result<()>;
    fn raw(&mut self) -> io::Result<()>;
    fn write(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn restore(&mut self);
    fn disarm(&mut self);
    fn poll_read(&mut self, clock: &dyn Clock, timeout: Duration) -> io::Result<Waited>;
}

// The setup steps begun so far; each is set before its call, so one that fails partway is still undone.
#[derive(Default)]
struct Steps {
    armed: bool,
    raw: bool,
    mouse: bool,
}

/// The side bar's terminal while `watch` reads it: set up step by step, and undone in reverse when dropped.
pub struct TermInput<T: Term> {
    term: T,
    steps: Steps,
}

impl<T: Term> TermInput<T> {
    /// Saves the terminal, then arms it, makes it raw and turns on mouse reports, in that order. A failed save
    /// answers `None` with nothing to undo; any later failure answers `None` once the steps begun are undone.
    pub fn setup(mut term: T) -> Option<Self> {
        term.save().ok()?;
        // Built before the first step, so every early return below drops it and undoes what was begun.
        let mut input = TermInput {
            term,
            steps: Steps::default(),
        };
        input.steps.armed = true;
        input.term.arm().ok()?;
        input.steps.raw = true;
        input.term.raw().ok()?;
        input.steps.mouse = true;
        input.term.write(ENABLE).ok()?;
        Some(input)
    }
}

impl<T: Term> Input for TermInput<T> {
    /// A terminal that cannot be read is `Closed`: `watch` stops reading it, never retries it.
    fn wait(&mut self, clock: &dyn Clock, timeout: Duration) -> Waited {
        self.term
            .poll_read(clock, timeout)
            .unwrap_or(Waited::Closed)
    }
}

impl<T: Term> Drop for TermInput<T> {
    /// Undoes the steps begun in reverse: mouse reports off (a failed write is ignored), then the saved mode back,
    /// then disarmed.
    fn drop(&mut self) {
        if self.steps.mouse {
            let _ = self.term.write(DISABLE);
        }
        if self.steps.raw {
            self.term.restore();
        }
        if self.steps.armed {
            self.term.disarm();
        }
    }
}

/// A terminal that logs each call (`save`, `arm`, `raw`, `write <escaped bytes>`, `restore`, `disarm`) into a shared
/// `log`, so a test keeps a handle after `setup` takes the terminal. `fail` names the one step that logs and then errors; with `partial`, the `ENABLE` write logs
/// its first 4 bytes and then errors. `poll_read` answers `reads` in turn, then sleeps its timeout and times out.
#[cfg(test)]
pub(crate) struct FakeTerm {
    pub(crate) log: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    pub(crate) fail: Option<&'static str>,
    pub(crate) partial: bool,
    /// The `DISABLE` write fails (on the way out, after the mode was in use).
    pub(crate) fail_disable: bool,
    pub(crate) reads: std::collections::VecDeque<io::Result<Waited>>,
}

#[cfg(test)]
impl FakeTerm {
    /// A terminal whose every step succeeds and which has nothing to read.
    pub(crate) fn new() -> Self {
        FakeTerm {
            log: Default::default(),
            fail: None,
            partial: false,
            fail_disable: false,
            reads: Default::default(),
        }
    }

    fn step(&mut self, name: &'static str, entry: String) -> io::Result<()> {
        self.log.borrow_mut().push(entry);
        if self.fail == Some(name) {
            return Err(io::Error::other(format!("{name} failed")));
        }
        Ok(())
    }
}

#[cfg(test)]
impl Term for FakeTerm {
    fn save(&mut self) -> io::Result<()> {
        self.step("save", "save".into())
    }

    fn arm(&mut self) -> io::Result<()> {
        self.step("arm", "arm".into())
    }

    fn raw(&mut self) -> io::Result<()> {
        self.step("raw", "raw".into())
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.fail_disable && bytes == DISABLE {
            self.log.borrow_mut().push("write failed".into());
            return Err(io::Error::other("write failed"));
        }
        if self.partial && bytes == ENABLE {
            let entry = format!("write {}", bytes[..4].escape_ascii());
            self.log.borrow_mut().push(entry);
            return Err(io::Error::other("write failed partway"));
        }
        self.step("write", format!("write {}", bytes.escape_ascii()))
    }

    fn restore(&mut self) {
        self.log.borrow_mut().push("restore".into());
    }

    fn disarm(&mut self) {
        self.log.borrow_mut().push("disarm".into());
    }

    fn poll_read(&mut self, clock: &dyn Clock, timeout: Duration) -> io::Result<Waited> {
        self.reads.pop_front().unwrap_or_else(|| {
            clock.sleep(timeout);
            Ok(Waited::Timeout)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Fakes;

    fn write(bytes: &[u8]) -> String {
        format!("write {}", bytes.escape_ascii())
    }

    #[test]
    fn setup_rolls_back_each_failed_step_in_reverse() {
        // The mode bytes as the terminal must see them, written out rather than taken from the constants.
        let enable = write(b"\x1b[?1000h\x1b[?1006h");
        let partway = write(b"\x1b[?1");
        let disable = write(b"\x1b[?1006l\x1b[?1000l");
        let cases: [(Option<&'static str>, bool, Vec<&str>); 4] = [
            (Some("save"), false, vec!["save"]),
            (Some("arm"), false, vec!["save", "arm", "disarm"]),
            (
                Some("raw"),
                false,
                vec!["save", "arm", "raw", "restore", "disarm"],
            ),
            (
                None,
                true,
                vec![
                    "save", "arm", "raw", &partway, &disable, "restore", "disarm",
                ],
            ),
        ];
        for (fail, partial, want) in cases {
            let mut term = FakeTerm::new();
            term.fail = fail;
            term.partial = partial;
            let log = term.log.clone();
            assert!(
                TermInput::setup(term).is_none(),
                "{fail:?} partial {partial}"
            );
            assert_eq!(*log.borrow(), want, "{fail:?} partial {partial}");
        }

        let term = FakeTerm::new();
        let log = term.log.clone();
        let input = TermInput::setup(term).expect("every step succeeds");
        assert_eq!(*log.borrow(), ["save", "arm", "raw", &enable]);
        drop(input);
        assert_eq!(
            *log.borrow(),
            ["save", "arm", "raw", &enable, &disable, "restore", "disarm"]
        );

        // A disable write that fails on the way out still restores the mode and disarms, without panicking.
        let mut term = FakeTerm::new();
        term.fail_disable = true;
        let log = term.log.clone();
        drop(TermInput::setup(term).expect("every step succeeds"));
        assert_eq!(
            *log.borrow(),
            [
                "save",
                "arm",
                "raw",
                &enable,
                "write failed",
                "restore",
                "disarm"
            ]
        );
    }

    #[test]
    fn no_input_sleeps_the_timeout_but_never_zero() {
        let fakes = Fakes::default();
        assert_eq!(
            NoInput.wait(&fakes, Duration::from_millis(2000)),
            Waited::Timeout
        );
        assert_eq!(NoInput.wait(&fakes, Duration::ZERO), Waited::Timeout);
        assert_eq!(fakes.slept(), [2000]);
    }

    #[test]
    fn input_only_for_two_terminals_and_not_dumb() {
        for stdin_tty in [false, true] {
            for stdout_tty in [false, true] {
                for dumb in [false, true] {
                    let want = stdin_tty && stdout_tty && !dumb;
                    assert_eq!(
                        wanted(stdin_tty, stdout_tty, dumb),
                        want,
                        "stdin {stdin_tty} stdout {stdout_tty} dumb {dumb}"
                    );
                }
            }
        }
    }

    // An input whose every wait answers `setup`, so a test can tell it from `NoInput`.
    struct Answers;

    impl Input for Answers {
        fn wait(&mut self, _clock: &dyn Clock, _timeout: Duration) -> Waited {
            Waited::Bytes(b"setup".to_vec())
        }
    }

    #[test]
    fn open_calls_setup_only_when_wanted() {
        let xterm = Env::from_pairs([("TERM", "xterm-256color")]);
        let dumb = Env::from_pairs([("TERM", "dumb")]);
        // (env, stdin_tty, stdout_tty, calls, setup's input reaches the caller)
        let cases = [
            (&dumb, true, true, 0, false),
            (&xterm, false, true, 0, false),
            (&xterm, true, false, 0, false),
            (&xterm, false, false, 0, false),
            (&xterm, true, true, 1, true),
        ];
        for (env, stdin_tty, stdout_tty, want_calls, answered) in cases {
            let fakes = Fakes::default();
            let mut calls = 0;
            let mut input = open_with(env, stdin_tty, stdout_tty, || {
                calls += 1;
                Some(Box::new(Answers) as Box<dyn Input>)
            });
            let want = if answered {
                Waited::Bytes(b"setup".to_vec())
            } else {
                Waited::Timeout
            };
            let case = format!(
                "{:?} stdin {stdin_tty} stdout {stdout_tty}",
                env.get("TERM")
            );
            assert_eq!(calls, want_calls, "{case}");
            assert_eq!(input.wait(&fakes, Duration::ZERO), want, "{case}");
            assert_eq!(fakes.slept(), Vec::<u64>::new(), "{case}");
        }

        // A setup that fails is `NoInput`: a zero wait times out without sleeping.
        let fakes = Fakes::default();
        let mut calls = 0;
        let mut input = open_with(&xterm, true, true, || {
            calls += 1;
            None
        });
        assert_eq!(calls, 1);
        assert_eq!(input.wait(&fakes, Duration::ZERO), Waited::Timeout);
        assert_eq!(fakes.slept(), Vec::<u64>::new());
    }

    // Draw-only on Windows (#70): two terminals still get `NoInput`, and the console is never touched.
    #[cfg(windows)]
    #[test]
    fn windows_setup_is_none() {
        assert!(platform_setup().is_none());
        let fakes = Fakes::default();
        let env = Env::from_pairs([("TERM", "xterm-256color")]);
        let mut input = open_with(&env, true, true, platform_setup);
        assert_eq!(input.wait(&fakes, Duration::ZERO), Waited::Timeout);
        assert_eq!(fakes.slept(), Vec::<u64>::new());
    }

    #[test]
    fn term_input_errors_are_closed() {
        let fakes = Fakes::default();
        let mut term = FakeTerm::new();
        term.reads.push_back(Ok(Waited::Bytes(b"j".to_vec())));
        term.reads.push_back(Ok(Waited::Timeout));
        term.reads.push_back(Err(io::Error::from_raw_os_error(5))); // EIO
        let mut input = TermInput::setup(term).expect("every step succeeds");
        let wait = Duration::from_millis(2000);
        // What the terminal answers passes through; only an error is Closed.
        assert_eq!(input.wait(&fakes, wait), Waited::Bytes(b"j".to_vec()));
        assert_eq!(input.wait(&fakes, wait), Waited::Timeout);
        assert_eq!(input.wait(&fakes, wait), Waited::Closed);
        assert!(fakes.slept().is_empty(), "answered reads take no time");
        // With nothing left to answer, the fake terminal waits out the timeout on the fake clock.
        assert_eq!(input.wait(&fakes, wait), Waited::Timeout);
        assert_eq!(fakes.slept(), [2000]);
    }

    #[cfg(unix)]
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    #[cfg(unix)]
    use std::sync::Mutex;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[cfg(unix)]
    use std::time::Instant;

    // Every pty test runs alone: it also keeps them from racing for the one `Tty` a process may hold.
    #[cfg(unix)]
    static PTY: Mutex<()> = Mutex::new(());

    // The local modes raw clears; a fresh pty has all three.
    #[cfg(unix)]
    const COOKED: libc::tcflag_t = libc::ICANON | libc::ECHO | libc::ISIG;

    // The variable that makes `signal_child` run: `<signal number>:<after-setup|before-enable>`.
    #[cfg(unix)]
    const SIGNAL_CHILD: &str = "MUX_AGENTS_SIGNAL_CHILD";

    // A fresh pty, (master, slave), both close-on-exec so no other test's child inherits them.
    #[cfg(unix)]
    fn pty() -> (OwnedFd, OwnedFd) {
        // SAFETY: each call takes the descriptor `posix_openpt` just returned, or the name `ptsname` returned for it;
        // `ptsname` uses a static buffer, read before any other call, and only under `PTY`.
        unsafe {
            let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            assert!(master >= 0, "posix_openpt: {}", io::Error::last_os_error());
            let master = OwnedFd::from_raw_fd(master);
            let fd = master.as_raw_fd();
            assert_eq!(libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC), 0);
            assert_eq!(libc::grantpt(fd), 0, "grantpt");
            assert_eq!(libc::unlockpt(fd), 0, "unlockpt");
            let name = libc::ptsname(fd);
            assert!(!name.is_null(), "ptsname");
            let slave = libc::open(name, libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
            assert!(slave >= 0, "open slave: {}", io::Error::last_os_error());
            (master, OwnedFd::from_raw_fd(slave))
        }
    }

    // What the master has to read until `quiet` passes with nothing new; EOF or an error (Linux answers EIO once no
    // slave is open) ends it too.
    #[cfg(unix)]
    fn drain(master: &OwnedFd, quiet: Duration) -> Vec<u8> {
        let mut seen = Vec::new();
        loop {
            let mut fd = libc::pollfd {
                fd: master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one pollfd, valid for the call.
            let ready = unsafe { libc::poll(&mut fd, 1, quiet.as_millis() as libc::c_int) };
            if ready <= 0 || fd.revents & libc::POLLIN == 0 {
                return seen;
            }
            let mut buf = [0u8; 4096];
            // SAFETY: `buf` has room for the `buf.len()` bytes read asks for.
            let n = unsafe { libc::read(fd.fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                return seen;
            }
            seen.extend_from_slice(&buf[..n as usize]);
        }
    }

    #[cfg(unix)]
    fn find(seen: &[u8], bytes: &[u8]) -> Option<usize> {
        seen.windows(bytes.len()).position(|w| w == bytes)
    }

    // Which of ICANON, ECHO and ISIG the terminal on `fd` has on.
    #[cfg(unix)]
    fn modes(fd: &OwnedFd) -> libc::tcflag_t {
        // SAFETY: a zeroed termios is a valid value (all integers) for tcgetattr to fill in.
        let mut mode: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `mode` is valid for the call.
        assert_eq!(unsafe { libc::tcgetattr(fd.as_raw_fd(), &mut mode) }, 0);
        mode.c_lflag & COOKED
    }

    #[cfg(unix)]
    static WINCHED: AtomicUsize = AtomicUsize::new(0);

    #[cfg(unix)]
    extern "C" fn on_winch(_: libc::c_int) {
        WINCHED.fetch_add(1, Ordering::Relaxed);
    }

    // The test thread, for another thread to interrupt.
    #[cfg(unix)]
    struct Target(libc::pthread_t);

    // SAFETY: a pthread_t only names a thread; signalling it from another thread is what pthread_kill is for.
    #[cfg(unix)]
    unsafe impl Send for Target {}

    #[cfg(unix)]
    impl Target {
        fn kill(&self, signal: libc::c_int) {
            // SAFETY: the target is the test thread, alive until it joins the thread calling this.
            assert_eq!(unsafe { libc::pthread_kill(self.0, signal) }, 0);
        }
    }

    // A signal mid-wait (EINTR) keeps the wait going to its first deadline; the next byte still arrives.
    #[cfg(unix)]
    #[test]
    fn eintr_during_wait_is_not_a_failure() {
        let _pty = PTY.lock().unwrap_or_else(|e| e.into_inner());
        let (master, slave) = pty();
        let fd = slave.as_raw_fd();
        assert_eq!(modes(&slave), COOKED);
        let mut input = TermInput::setup(Tty::on(fd, fd)).expect("a pty sets up");
        assert_eq!(modes(&slave), 0, "raw: ICANON, ECHO and ISIG off");
        assert!(
            TermInput::setup(Tty::on(fd, fd)).is_none(),
            "a second Tty while the first is held"
        );

        // No SA_RESTART, so the signal interrupts poll with EINTR.
        // SAFETY: a zeroed sigaction is a valid value (empty mask, no flags) for sigaction to fill in.
        let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: as above; the flags stay empty, so no SA_RESTART.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = on_winch as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // SAFETY: both pointers are valid for the call; the handler only bumps an atomic, and the old action is put
        // back below.
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGWINCH, &action, &mut old) },
            0
        );
        WINCHED.store(0, Ordering::Relaxed);
        // SAFETY: pthread_self has no preconditions.
        let target = Target(unsafe { libc::pthread_self() });
        let kicker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            target.kill(libc::SIGWINCH);
        });
        let fakes = Fakes::default();
        let start = Instant::now();
        let waited = input.wait(&fakes, Duration::from_millis(300));
        let took = start.elapsed();
        kicker.join().unwrap();
        // SAFETY: puts back the action saved above.
        unsafe { libc::sigaction(libc::SIGWINCH, &old, std::ptr::null_mut()) };

        assert_eq!(waited, Waited::Timeout);
        assert_eq!(
            WINCHED.load(Ordering::Relaxed),
            1,
            "the signal came mid-wait"
        );
        assert!(
            took >= Duration::from_millis(300) && took < Duration::from_millis(340),
            "{took:?}"
        );
        assert_eq!(
            fakes.slept(),
            Vec::<u64>::new(),
            "a tty never sleeps the clock"
        );

        // SAFETY: one byte from a valid buffer.
        assert_eq!(
            unsafe { libc::write(master.as_raw_fd(), b"j".as_ptr().cast(), 1) },
            1
        );
        assert_eq!(
            input.wait(&fakes, Duration::from_secs(1)),
            Waited::Bytes(b"j".to_vec())
        );
        let seen = drain(&master, Duration::from_millis(100));
        assert!(find(&seen, ENABLE).is_some(), "{}", seen.escape_ascii());

        drop(input);
        let seen = drain(&master, Duration::from_millis(100));
        assert!(find(&seen, DISABLE).is_some(), "{}", seen.escape_ascii());
        assert_eq!(modes(&slave), COOKED, "restored");
    }

    // A zero wait still reads bytes already waiting (the loop drains input before each frame), and a signal that was
    // ignored when `watch` started (SIGHUP under nohup) stays ignored while it runs and after.
    #[cfg(unix)]
    #[test]
    fn zero_wait_reads_waiting_bytes_and_ignored_signals_stay_ignored() {
        let _pty = PTY.lock().unwrap_or_else(|e| e.into_inner());
        let (master, slave) = pty();
        let fd = slave.as_raw_fd();
        let ignored = |signal| {
            // SAFETY: a zeroed sigaction is a valid value for sigaction to fill in.
            let mut now: libc::sigaction = unsafe { std::mem::zeroed() };
            // SAFETY: a null new action only reads the current one into `now`.
            unsafe { libc::sigaction(signal, std::ptr::null(), &mut now) };
            now.sa_sigaction == libc::SIG_IGN
        };
        // SAFETY: SIG_IGN for SIGHUP is put back to the default below, under `PTY`.
        unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
        let mut input = TermInput::setup(Tty::on(fd, fd)).expect("a pty sets up");
        assert!(ignored(libc::SIGHUP), "an ignored SIGHUP is left ignored");
        assert!(!ignored(libc::SIGTERM), "SIGTERM gets the handler");
        let fakes = Fakes::default();
        assert_eq!(input.wait(&fakes, Duration::ZERO), Waited::Timeout);
        // SAFETY: one byte from a valid buffer to the open master.
        assert_eq!(
            unsafe { libc::write(master.as_raw_fd(), b"k".as_ptr().cast(), 1) },
            1
        );
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            input.wait(&fakes, Duration::ZERO),
            Waited::Bytes(b"k".to_vec())
        );
        drop(input);
        assert!(ignored(libc::SIGHUP), "still ignored after");
        // SAFETY: back to the default action, as before this test.
        unsafe { libc::signal(libc::SIGHUP, libc::SIG_DFL) };
    }

    // The current action for `signal`.
    #[cfg(unix)]
    fn action(signal: libc::c_int) -> libc::sighandler_t {
        // SAFETY: a zeroed sigaction is a valid value for sigaction to fill in.
        let mut now: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: a null new action only reads the current one into `now`.
        assert_eq!(
            unsafe { libc::sigaction(signal, std::ptr::null(), &mut now) },
            0
        );
        now.sa_sigaction
    }

    // Installs `handler` for `signal`, with no SA_RESTART.
    #[cfg(unix)]
    fn install(signal: libc::c_int, handler: libc::sighandler_t) {
        // SAFETY: a zeroed sigaction is a valid value (empty mask, no flags).
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = handler;
        // SAFETY: the pointer is valid for the call; each test puts the default back under `PTY`.
        assert_eq!(
            unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) },
            0
        );
    }

    // VMIN and VTIME on `fd`.
    #[cfg(unix)]
    fn vmin_vtime(fd: &OwnedFd) -> (libc::cc_t, libc::cc_t) {
        // SAFETY: a zeroed termios is a valid value for tcgetattr to fill in.
        let mut mode: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `mode` is valid for the call.
        assert_eq!(unsafe { libc::tcgetattr(fd.as_raw_fd(), &mut mode) }, 0);
        (mode.c_cc[libc::VMIN], mode.c_cc[libc::VTIME])
    }

    // A read end with nothing to read yet, and its write end.
    #[cfg(unix)]
    fn pipe() -> (OwnedFd, OwnedFd) {
        let mut fds = [0; 2];
        // SAFETY: `fds` has room for the two descriptors pipe returns, each taken once below.
        unsafe {
            assert_eq!(libc::pipe(fds.as_mut_ptr()), 0);
            (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1]))
        }
    }

    // A save that fails (no terminal) gives the globals back, so the next setup works; a handler someone else had
    // comes back after, an ignored signal is never touched, and raw mode sets VMIN 1, VTIME 0 whatever came before.
    #[cfg(unix)]
    #[test]
    fn setup_gives_back_what_it_took() {
        let _pty = PTY.lock().unwrap_or_else(|e| e.into_inner());
        let (read, write) = pipe();
        assert!(
            TermInput::setup(Tty::on(read.as_raw_fd(), write.as_raw_fd())).is_none(),
            "a pipe is no terminal"
        );

        let (_master, slave) = pty();
        let fd = slave.as_raw_fd();
        // SAFETY: a zeroed termios is a valid value for tcgetattr to fill in.
        let mut mode: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `mode` is valid for both calls on the open slave.
        unsafe {
            assert_eq!(libc::tcgetattr(fd, &mut mode), 0);
            mode.c_cc[libc::VMIN] = 0;
            mode.c_cc[libc::VTIME] = 5;
            assert_eq!(libc::tcsetattr(fd, libc::TCSANOW, &mode), 0);
        }
        let winch = on_winch as extern "C" fn(libc::c_int) as libc::sighandler_t;
        install(libc::SIGTERM, winch);
        install(libc::SIGHUP, libc::SIG_IGN);

        let input = TermInput::setup(Tty::on(fd, fd)).expect("the globals were given back");
        assert_eq!(
            vmin_vtime(&slave),
            (1, 0),
            "a read waits for one byte, no timer"
        );
        assert_ne!(
            action(libc::SIGTERM),
            winch,
            "the bar's handler while it runs"
        );
        // Changed while the bar runs: the bar never armed it, so it leaves it as it finds it.
        install(libc::SIGHUP, winch);
        drop(input);
        assert_eq!(vmin_vtime(&slave), (0, 5), "restored");
        assert_eq!(action(libc::SIGTERM), winch, "the replaced handler is back");
        assert_eq!(
            action(libc::SIGHUP),
            winch,
            "an ignored signal is never disarmed"
        );

        install(libc::SIGTERM, libc::SIG_DFL);
        install(libc::SIGHUP, libc::SIG_DFL);
    }

    // A write the terminal takes in pieces, or a signal interrupts, still writes every byte.
    #[cfg(unix)]
    #[test]
    fn write_finishes_through_short_and_interrupted_writes() {
        let _pty = PTY.lock().unwrap_or_else(|e| e.into_inner());
        let (master, slave) = pty();
        let fd = slave.as_raw_fd();
        // SAFETY: a zeroed termios is a valid value for tcgetattr to fill in.
        let mut mode: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `mode` is valid for both calls; the output is passed through as is, so the bytes count exactly.
        unsafe {
            assert_eq!(libc::tcgetattr(fd, &mut mode), 0);
            libc::cfmakeraw(&mut mode);
            assert_eq!(libc::tcsetattr(fd, libc::TCSANOW, &mode), 0);
        }
        install(
            libc::SIGWINCH,
            on_winch as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        WINCHED.store(0, Ordering::Relaxed);
        // Fill the pty while nothing reads it, so the next write blocks before its first byte.
        // SAFETY: fcntl on the open slave; it is made blocking again before `write`.
        unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) };
        let mut full = 0;
        loop {
            let chunk = [b'f'; 1024];
            // SAFETY: `chunk` is valid for its length.
            let n = unsafe { libc::write(fd, chunk.as_ptr().cast(), chunk.len()) };
            if n <= 0 {
                break;
            }
            full += n as usize;
        }
        // SAFETY: as above.
        unsafe { libc::fcntl(fd, libc::F_SETFL, 0) };
        assert!(full > 0, "the pty took nothing");

        const LEN: usize = 256 * 1024;
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        // SAFETY: pthread_self has no preconditions.
        let target = Target(unsafe { libc::pthread_self() });
        // Signals the writer the whole time: first while it is blocked with nothing written (EINTR), then while
        // the reader drains, between pieces (a short count).
        let kicker = {
            let done = done.clone();
            std::thread::spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    target.kill(libc::SIGWINCH);
                    std::thread::sleep(Duration::from_millis(2));
                }
            })
        };
        // Reads nothing for 200 ms, then slowly, until every byte came or none came for 2 s.
        let reader = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let mut seen = 0;
            while seen < full + LEN {
                let mut poll = libc::pollfd {
                    fd: master.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one pollfd, valid for the call.
                if unsafe { libc::poll(&mut poll, 1, 2000) } <= 0 {
                    break;
                }
                let mut buf = [0u8; 4096];
                // SAFETY: `buf` has room for the bytes read asks for.
                let n =
                    unsafe { libc::read(master.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 {
                    break;
                }
                seen += n as usize;
                std::thread::sleep(Duration::from_millis(1));
            }
            seen
        });
        WINCHED.store(0, Ordering::Relaxed);
        let wrote = Tty::on(fd, fd).write(&vec![b'x'; LEN]);
        let during = WINCHED.load(Ordering::Relaxed);
        done.store(true, Ordering::Relaxed);
        kicker.join().unwrap();
        let seen = reader.join().unwrap();
        install(libc::SIGWINCH, libc::SIG_DFL);
        assert!(wrote.is_ok(), "{wrote:?}");
        assert_eq!(seen - full, LEN, "every byte, none twice");
        assert!(during > 10, "signals came mid-write: {during}");
    }

    // EOF, a hang-up, a descriptor that is not open, and a read error each end the wait at once.
    #[cfg(unix)]
    #[test]
    fn wait_ends_on_eof_hang_up_and_errors() {
        let fakes = Fakes::default();
        let long = Duration::from_secs(5);
        let poll = |input: RawFd| {
            let start = Instant::now();
            let waited = Tty::on(input, -1).poll_read(&fakes, long);
            assert!(start.elapsed() < Duration::from_secs(1), "{waited:?}");
            waited
        };
        // A read of no bytes is EOF.
        let null = std::fs::File::open("/dev/null").unwrap();
        assert_eq!(poll(null.as_raw_fd()).unwrap(), Waited::Closed);
        // Nothing left to read and no writer: a hang-up (or EOF).
        let (read, write) = pipe();
        drop(write);
        assert_eq!(poll(read.as_raw_fd()).unwrap(), Waited::Closed);
        // Past the descriptor limit, never open.
        assert_eq!(poll(libc::c_int::MAX - 1).unwrap(), Waited::Closed);
        // A regular file always polls readable, but one opened only to write fails its read (EBADF): an error, which
        // `TermInput::wait` makes Closed.
        let path = std::env::temp_dir().join(format!("mux-agents-wait-{}", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        let waited = poll(file.as_raw_fd());
        let _ = std::fs::remove_file(&path);
        assert!(waited.is_err(), "{waited:?}");
    }

    // Each signal, mid-setup or after it, restores the terminal and still ends the process by that signal. The child
    // is this test binary running `signal_child` with its stdio on a pty.
    #[cfg(unix)]
    #[test]
    fn signal_restores_the_terminal_and_ends_by_that_signal() {
        use std::os::unix::process::ExitStatusExt;
        use std::process::{Command, Stdio};

        let _pty = PTY.lock().unwrap_or_else(|e| e.into_inner());
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
            for when in ["after-setup", "before-enable"] {
                let case = format!("{signal}:{when}");
                // The parent keeps `slave` open, so the master stays readable and the mode can be read after.
                let (master, slave) = pty();
                assert_eq!(modes(&slave), COOKED, "{case}");
                let stdio = || Stdio::from(slave.try_clone().unwrap());
                let mut child = Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "sidebar::input::tests::signal_child",
                        "--include-ignored",
                        "--test-threads=1",
                    ])
                    .env(SIGNAL_CHILD, &case)
                    .stdin(stdio())
                    .stdout(stdio())
                    .stderr(stdio())
                    .spawn()
                    .unwrap();
                // The child sleeps a minute if the signal does not end it: give up long before that.
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut seen = Vec::new();
                let status = loop {
                    seen.extend(drain(&master, Duration::from_millis(20)));
                    if let Some(status) = child.try_wait().unwrap() {
                        break status;
                    }
                    if Instant::now() > deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("{case}: the child did not end: {}", seen.escape_ascii());
                    }
                };
                seen.extend(drain(&master, Duration::from_millis(100)));
                let seen_text = seen.escape_ascii().to_string();

                assert_eq!(
                    status.signal(),
                    Some(signal),
                    "{case}: {status:?} {seen_text}"
                );
                let disable = find(&seen, DISABLE)
                    .unwrap_or_else(|| panic!("{case}: no DISABLE in {seen_text}"));
                let enable = find(&seen, ENABLE);
                match when {
                    "after-setup" => assert!(
                        enable.is_some_and(|enable| enable < disable),
                        "{case}: {seen_text}"
                    ),
                    _ => assert_eq!(enable, None, "{case}: {seen_text}"),
                }
                assert_eq!(modes(&slave), COOKED, "{case}: restored");
            }
        }
    }

    // A `Tty` that raises `signal` inside its first write, before writing a byte.
    #[cfg(unix)]
    struct Raising {
        tty: Tty,
        signal: Option<libc::c_int>,
    }

    #[cfg(unix)]
    impl Term for Raising {
        fn save(&mut self) -> io::Result<()> {
            self.tty.save()
        }

        fn arm(&mut self) -> io::Result<()> {
            self.tty.arm()
        }

        fn raw(&mut self) -> io::Result<()> {
            self.tty.raw()
        }

        fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
            if let Some(signal) = self.signal.take() {
                // SAFETY: raise has no preconditions; the armed handler restores the terminal and ends the process.
                unsafe { libc::raise(signal) };
            }
            self.tty.write(bytes)
        }

        fn restore(&mut self) {
            self.tty.restore();
        }

        fn disarm(&mut self) {
            self.tty.disarm();
        }

        fn poll_read(&mut self, clock: &dyn Clock, timeout: Duration) -> io::Result<Waited> {
            self.tty.poll_read(clock, timeout)
        }
    }

    // Run only by `signal_restores_the_terminal_and_ends_by_that_signal`, in its own process with its stdio on a pty;
    // without `SIGNAL_CHILD` it never touches the terminal.
    #[cfg(unix)]
    #[test]
    #[ignore = "a child of signal_restores_the_terminal_and_ends_by_that_signal"]
    fn signal_child() {
        let Ok(case) = std::env::var(SIGNAL_CHILD) else {
            return;
        };
        let (signal, when) = case.split_once(':').expect("<signal>:<when>");
        let signal: libc::c_int = signal.parse().expect("a signal number");
        let none = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: a valid rlimit for this process alone: SIGQUIT then ends it without a core file.
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_CORE, &none) }, 0);
        let tty = Raising {
            tty: Tty::stdio(),
            signal: (when == "before-enable").then_some(signal),
        };
        let input = TermInput::setup(tty);
        if when == "after-setup" {
            assert!(input.is_some(), "set up");
            // SAFETY: as in `Raising::write`.
            unsafe { libc::raise(signal) };
        }
        std::thread::sleep(Duration::from_secs(60));
        drop(input);
    }
}
