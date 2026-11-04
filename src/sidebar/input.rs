//! The side bar's own terminal: raw mode and mouse reporting while `watch` runs, restored on every way it ends.

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

/// Windows: the console on stdin and stdout, set up (#70).
#[cfg(windows)]
fn platform_setup() -> Option<Box<dyn Input>> {
    TermInput::setup(Console::stdio()).map(|input| Box::new(input) as Box<dyn Input>)
}

/// Turns the characters of console key records into UTF-8, one read at a time (#70). Under VT input the console
/// reports each character of a sequence as a key-down record of its own; key-up records and records with no character
/// are dropped. The first half of a surrogate pair is held until its second half, even across reads; a lone half is
/// U+FFFD.
#[cfg(any(windows, test))]
#[derive(Default)]
pub(crate) struct KeyUnits {
    high: Option<u16>,
}

#[cfg(any(windows, test))]
impl KeyUnits {
    /// One record: `down` its key state, `repeat` its repeat count (0 counts once), `unit` its UTF-16 character.
    pub(crate) fn push(&mut self, down: bool, repeat: u16, unit: u16, out: &mut Vec<u8>) {
        if !down || unit == 0 {
            return;
        }
        for _ in 0..repeat.max(1) {
            self.unit(unit, out);
        }
    }

    fn unit(&mut self, unit: u16, out: &mut Vec<u8>) {
        let held = self.high.take();
        if let Some(high) = held
            && (0xdc00..=0xdfff).contains(&unit)
        {
            let pair = char::decode_utf16([high, unit]).next().and_then(Result::ok);
            return put(pair.unwrap_or(char::REPLACEMENT_CHARACTER), out);
        }
        if held.is_some() {
            put(char::REPLACEMENT_CHARACTER, out);
        }
        if (0xd800..=0xdbff).contains(&unit) {
            self.high = Some(unit);
        } else {
            put(
                char::from_u32(u32::from(unit)).unwrap_or(char::REPLACEMENT_CHARACTER),
                out,
            );
        }
    }
}

#[cfg(any(windows, test))]
fn put(c: char, out: &mut Vec<u8>) {
    out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
}

#[cfg(windows)]
pub use windows::Console;

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::io;
    use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};
    use std::sync::{Mutex, MutexGuard};
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{
        FALSE, HANDLE, INVALID_HANDLE_VALUE, TRUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Storage::FileSystem::WriteFile;
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS,
        ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_QUICK_EDIT_MODE,
        ENABLE_VIRTUAL_TERMINAL_INPUT, GetConsoleMode, GetNumberOfConsoleInputEvents, GetStdHandle,
        INPUT_RECORD, KEY_EVENT, ReadConsoleInputW, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
        SetConsoleCtrlHandler, SetConsoleMode,
    };
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    use windows_sys::core::BOOL;

    use super::{DISABLE, KeyUnits, Term, Waited};
    use crate::ctx::Clock;

    // The input mode `save` read, which `raw` changes and `restore` and the handler put back.
    static SAVED: AtomicU32 = AtomicU32::new(0);
    // Whether the handler restores the console: set by `arm`, cleared by `disarm`.
    static ARMED: AtomicBool = AtomicBool::new(false);
    // Whether a `Console` holds the globals: one per process, from `save` to `disarm`.
    static OWNED: AtomicBool = AtomicBool::new(false);
    static IN: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
    static OUT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
    // Held by the handler and by every change to the console (`save`, `raw`, `write`, `restore`, and `arm` and
    // `disarm` setting `ARMED`). The handler runs on a thread of its own, so without it a setup step could run just
    // after the handler restored the mode and leave the console raw when the process ends. True once the handler
    // ran while armed: the console is ending, and `raw`, `write` and `restore` then change nothing.
    static GATE: Mutex<bool> = Mutex::new(false);

    fn gate() -> MutexGuard<'static, bool> {
        GATE.lock().unwrap_or_else(|e| e.into_inner())
    }

    // The error a setup step answers once the handler ran.
    fn ended() -> io::Error {
        io::Error::other("the console is ending")
    }

    /// The saved input mode without line editing, echo, Ctrl-C processing or quick edit, and with VT input: each
    /// key, arrow and mouse report arrives as its characters, and Ctrl-C as byte 0x03.
    pub(crate) fn raw_mode(saved: u32) -> u32 {
        (saved
            & !(ENABLE_LINE_INPUT
                | ENABLE_ECHO_INPUT
                | ENABLE_PROCESSED_INPUT
                | ENABLE_QUICK_EDIT_MODE))
            | ENABLE_VIRTUAL_TERMINAL_INPUT
            | ENABLE_EXTENDED_FLAGS
    }

    fn valid(handle: HANDLE) -> bool {
        !handle.is_null() && handle != INVALID_HANDLE_VALUE
    }

    /// Writes every byte to `output`.
    fn write_all(output: HANDLE, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let len = bytes.len().min(u32::MAX as usize) as u32;
            let mut n = 0u32;
            // SAFETY: `bytes` is valid for `len` bytes and `n` for the count written; no overlapped I/O.
            if unsafe { WriteFile(output, bytes.as_ptr(), len, &mut n, std::ptr::null_mut()) } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            bytes = &bytes[n as usize..];
        }
        Ok(())
    }

    /// The console control handler, run by the system on a thread of its own: while armed it marks the console
    /// ending, puts the saved input mode back and, on Ctrl-C or Ctrl-Break, turns mouse reports off, all under
    /// `GATE`, so no setup step undoes it. It returns FALSE, so the next handler, and at last the default one, still
    /// ends the process. On close, logoff and shutdown the console is going away and console calls are unreliable:
    /// it writes nothing then.
    unsafe extern "system" fn on_ctrl(event: u32) -> BOOL {
        let mut ending = gate();
        if ARMED.load(Ordering::Acquire) {
            *ending = true;
            // SAFETY: `ARMED` means `save` stored `SAVED` and the handles first, under `GATE`, which is held here.
            // Console never closes the handles. A failed call is ignored: the process ends either way.
            unsafe { SetConsoleMode(IN.load(Ordering::Relaxed), SAVED.load(Ordering::Relaxed)) };
            if matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT) {
                let _ = write_all(OUT.load(Ordering::Relaxed), DISABLE);
            }
        }
        FALSE
    }

    /// The console read from `input` and written to `output`: the real `Term` on Windows. Only one may hold the
    /// console per process, since the control handler restores it from globals. It borrows the handles: it never
    /// closes them, so the handler never sees a closed one.
    pub struct Console {
        input: HANDLE,
        output: HANDLE,
        installed: bool,
        keys: KeyUnits,
    }

    impl Console {
        /// The process's own console: its standard input and output handles.
        pub fn stdio() -> Console {
            // SAFETY: GetStdHandle takes a constant; a handle that is missing or not a console fails `save`.
            unsafe {
                Console::on(
                    GetStdHandle(STD_INPUT_HANDLE),
                    GetStdHandle(STD_OUTPUT_HANDLE),
                )
            }
        }

        pub(crate) fn on(input: HANDLE, output: HANDLE) -> Console {
            Console {
                input,
                output,
                installed: false,
                keys: KeyUnits::default(),
            }
        }

        fn set(&self, mode: u32) -> io::Result<()> {
            // SAFETY: takes the handle and a mode value only.
            if unsafe { SetConsoleMode(self.input, mode) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        /// Reads the records waiting, never more, so the read does not block; answers the bytes of their keys.
        fn read_keys(&mut self) -> io::Result<Vec<u8>> {
            let mut bytes = Vec::new();
            let mut waiting = 0u32;
            // SAFETY: `waiting` is valid for the call.
            if unsafe { GetNumberOfConsoleInputEvents(self.input, &mut waiting) } == 0 {
                return Err(io::Error::last_os_error());
            }
            if waiting == 0 {
                return Ok(bytes);
            }
            // SAFETY: INPUT_RECORD is plain data: all zeroes is a valid value for ReadConsoleInputW to fill in.
            let mut records: [INPUT_RECORD; 64] = unsafe { std::mem::zeroed() };
            let want = waiting.min(records.len() as u32);
            let mut read = 0u32;
            // SAFETY: `records` has room for `want` records and `read` is valid for the count read.
            if unsafe { ReadConsoleInputW(self.input, records.as_mut_ptr(), want, &mut read) } == 0
            {
                return Err(io::Error::last_os_error());
            }
            for record in &records[..read as usize] {
                if u32::from(record.EventType) != KEY_EVENT {
                    continue;
                }
                // SAFETY: a KEY_EVENT record's event is its KeyEvent member, and a key record's character is its
                // UnicodeChar member when read with ReadConsoleInputW.
                let (key, unit) = unsafe {
                    (
                        record.Event.KeyEvent,
                        record.Event.KeyEvent.uChar.UnicodeChar,
                    )
                };
                self.keys
                    .push(key.bKeyDown != 0, key.wRepeatCount, unit, &mut bytes);
            }
            Ok(bytes)
        }
    }

    impl Term for Console {
        /// Claims the globals, then reads the mode to restore; a second `Console`, or a handle that is not a
        /// console, is an error.
        fn save(&mut self) -> io::Result<()> {
            let mut ending = gate();
            if OWNED
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return Err(io::Error::other("a console is already set up"));
            }
            *ending = false;
            let mut mode = 0u32;
            let read = if !valid(self.input) {
                Err(io::Error::other("no console input"))
            // SAFETY: `mode` is valid for the call; a handle that is not a console fails it.
            } else if unsafe { GetConsoleMode(self.input, &mut mode) } == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            };
            if let Err(error) = read {
                OWNED.store(false, Ordering::Release);
                return Err(error);
            }
            SAVED.store(mode, Ordering::Relaxed);
            IN.store(self.input, Ordering::Relaxed);
            OUT.store(self.output, Ordering::Relaxed);
            Ok(())
        }

        /// Arms the handler, then installs it. The handler list is changed outside `GATE`, so the system never waits
        /// on it while a handler waits on `GATE`.
        fn arm(&mut self) -> io::Result<()> {
            {
                let _gate = gate();
                ARMED.store(true, Ordering::Release);
            }
            // SAFETY: `on_ctrl` only reads the globals `save` stored and makes console calls on their handles.
            if unsafe { SetConsoleCtrlHandler(Some(on_ctrl), TRUE) } == 0 {
                return Err(io::Error::last_os_error());
            }
            self.installed = true;
            Ok(())
        }

        /// The raw mode, unless the handler already restored the console.
        fn raw(&mut self) -> io::Result<()> {
            let ending = gate();
            if *ending {
                return Err(ended());
            }
            self.set(raw_mode(SAVED.load(Ordering::Relaxed)))
        }

        /// Writes every byte, unless the handler already restored the console: no ENABLE after its DISABLE.
        fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
            let ending = gate();
            if *ending {
                return Err(ended());
            }
            write_all(self.output, bytes)
        }

        /// Puts the saved mode back, unless the handler already did; a failure is ignored, as there is nothing left
        /// to try.
        fn restore(&mut self) {
            let ending = gate();
            if !*ending {
                let _ = self.set(SAVED.load(Ordering::Relaxed));
            }
        }

        /// Disarms the handler under `GATE`, so a handler already running finishes first and any later one does
        /// nothing; then removes it and gives up the globals.
        fn disarm(&mut self) {
            {
                let _gate = gate();
                ARMED.store(false, Ordering::Release);
            }
            if std::mem::take(&mut self.installed) {
                // SAFETY: removes the handler `arm` added; a failure leaves a disarmed handler that does nothing.
                unsafe { SetConsoleCtrlHandler(Some(on_ctrl), FALSE) };
            }
            OWNED.store(false, Ordering::Release);
        }

        /// Waits up to `timeout`, by the real time and not `clock`, for key records: records with no key (a resize,
        /// focus) are read and dropped, and the wait goes on to the same deadline. A zero timeout still looks once,
        /// so keys already waiting are read. Never ReadFile, which would block on a buffer holding only such
        /// records. A failed wait or read is an error.
        fn poll_read(&mut self, _clock: &dyn Clock, timeout: Duration) -> io::Result<Waited> {
            let deadline = Instant::now() + timeout;
            let mut looked = false;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() && looked {
                    return Ok(Waited::Timeout);
                }
                looked = true;
                // Rounded up, so the wait never wakes early and spins on a deadline less than a millisecond away;
                // below INFINITE.
                let ms = left
                    .as_nanos()
                    .div_ceil(1_000_000)
                    .min(u128::from(u32::MAX - 1)) as u32;
                // SAFETY: waits on the input handle `save` checked; waiting consumes nothing.
                match unsafe { WaitForSingleObject(self.input, ms) } {
                    WAIT_OBJECT_0 => {}
                    WAIT_TIMEOUT => continue,
                    _ => return Err(io::Error::last_os_error()),
                }
                let bytes = self.read_keys()?;
                if !bytes.is_empty() {
                    return Ok(Waited::Bytes(bytes));
                }
            }
        }
    }
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
    /// A hang-up writes nothing: the terminal is going away, and rmux, closing the pane, no longer reaps a process
    /// that writes to it then (it is left a zombie).
    extern "C" fn on_signal(signal: libc::c_int) {
        if ARMED.load(Ordering::Acquire) {
            let input = IN_FD.load(Ordering::Relaxed);
            let output = OUT_FD.load(Ordering::Relaxed);
            // SAFETY: `ARMED` means `save` wrote `SAVED` and the descriptors first; nothing writes them while armed.
            // A failed call is ignored: the process ends either way.
            unsafe {
                libc::tcsetattr(input, libc::TCSANOW, (*SAVED.0.get()).as_ptr());
                if signal != libc::SIGHUP {
                    libc::write(output, DISABLE.as_ptr().cast(), DISABLE.len());
                }
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
                let ready = if ready < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(ready)
                };
                match after_poll(ready, fd.revents) {
                    Polled::Again => continue,
                    Polled::Done(done) => return done,
                    Polled::Read => {}
                }
                let mut buf = [0u8; 256];
                // SAFETY: `buf` has room for the `buf.len()` bytes read asks for.
                let n = unsafe { libc::read(self.input, buf.as_mut_ptr().cast(), buf.len()) };
                let read = if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(&buf[..n as usize])
                };
                if let Some(done) = after_read(read) {
                    return done;
                }
            }
        }
    }

    /// What one `poll` answer means for the wait.
    #[derive(Debug)]
    pub(super) enum Polled {
        /// Nothing yet, or a signal: poll again to the same deadline.
        Again,
        /// Bytes are ready: read them.
        Read,
        Done(io::Result<Waited>),
    }

    /// `ready` is what poll returned (or its error), `revents` what it set. Bytes ready come first, even beside a
    /// hang-up; a hang-up, error or bad descriptor without them is `Closed`; a failed poll other than EINTR is an
    /// error.
    pub(super) fn after_poll(ready: io::Result<libc::c_int>, revents: libc::c_short) -> Polled {
        match ready {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Polled::Again,
            Err(error) => Polled::Done(Err(error)),
            Ok(0) => Polled::Again,
            Ok(_) if revents & libc::POLLIN != 0 => Polled::Read,
            Ok(_) if revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 => {
                Polled::Done(Ok(Waited::Closed))
            }
            Ok(_) => Polled::Again,
        }
    }

    /// What one `read` answer means: bytes, `Closed` at EOF, an error, or `None` to wait again on EINTR or EAGAIN
    /// (another reader took the bytes poll saw).
    pub(super) fn after_read(read: io::Result<&[u8]>) -> Option<io::Result<Waited>> {
        match read {
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) =>
            {
                None
            }
            Err(error) => Some(Err(error)),
            Ok([]) => Some(Ok(Waited::Closed)),
            Ok(bytes) => Some(Ok(Waited::Bytes(bytes.to_vec()))),
        }
    }
}

/// Turns on mouse press and release reports in SGR form; `DISABLE` turns them off in reverse.
#[cfg(any(unix, windows, test))]
pub const ENABLE: &[u8] = b"\x1b[?1000h\x1b[?1006h";
#[cfg(any(unix, windows, test))]
pub const DISABLE: &[u8] = b"\x1b[?1006l\x1b[?1000l";

/// The terminal calls `TermInput` makes, one per setup step and one per undo, so a test can fail any of them.
#[cfg(any(unix, windows, test))]
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
#[cfg(any(unix, windows, test))]
#[derive(Default)]
struct Steps {
    armed: bool,
    raw: bool,
    mouse: bool,
}

/// The side bar's terminal while `watch` reads it: set up step by step, and undone in reverse when dropped.
#[cfg(any(unix, windows, test))]
pub struct TermInput<T: Term> {
    term: T,
    steps: Steps,
}

#[cfg(any(unix, windows, test))]
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

#[cfg(any(unix, windows, test))]
impl<T: Term> Input for TermInput<T> {
    /// A terminal that cannot be read is `Closed`: `watch` stops reading it, never retries it.
    fn wait(&mut self, clock: &dyn Clock, timeout: Duration) -> Waited {
        self.term
            .poll_read(clock, timeout)
            .unwrap_or(Waited::Closed)
    }
}

#[cfg(any(unix, windows, test))]
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

    // What `KeyUnits` turns `(down, repeat, unit)` records into, one read at a time.
    fn units(keys: &mut KeyUnits, records: &[(bool, u16, u16)]) -> Vec<u8> {
        let mut out = Vec::new();
        for &(down, repeat, unit) in records {
            keys.push(down, repeat, unit, &mut out);
        }
        out
    }

    #[test]
    fn key_records_become_utf8_bytes() {
        let mut keys = KeyUnits::default();
        // Key-up records and records with no character are dropped; a repeat count repeats the character, and a
        // count of 0 still counts once.
        let read = units(
            &mut keys,
            &[
                (true, 1, 0x6a),
                (false, 1, 0x6a),
                (true, 1, 0),
                (true, 3, 0x6b),
                (true, 0, 0x1b),
            ],
        );
        assert_eq!(read, b"jkkk\x1b");
        // An SGR press and Ctrl-C come through byte for byte; é is two UTF-8 bytes.
        let sgr: Vec<(bool, u16, u16)> = "\x1b[<0;3;5M\x03é"
            .encode_utf16()
            .map(|u| (true, 1, u))
            .collect();
        assert_eq!(units(&mut keys, &sgr), "\x1b[<0;3;5M\x03é".as_bytes());
        // A surrogate pair split across two reads is held until its second half arrives.
        assert_eq!(units(&mut keys, &[(true, 1, 0xd83d)]), b"");
        assert_eq!(units(&mut keys, &[(false, 1, 0xde00)]), b"");
        assert_eq!(units(&mut keys, &[(true, 1, 0xde00)]), "😀".as_bytes());
        // A lone half is U+FFFD, and the character after a lone high half still counts.
        assert_eq!(
            units(&mut keys, &[(true, 1, 0xd83d), (true, 1, 0x6a)]),
            "\u{fffd}j".as_bytes()
        );
        assert_eq!(
            units(&mut keys, &[(true, 1, 0xde00)]),
            "\u{fffd}".as_bytes()
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

    // The Windows console tests (#70): each runs this test binary again as a child with CREATE_NO_WINDOW, which
    // gives the child a hidden console of its own, as the unix tests use a pty. The child opens CONIN$, writes
    // records into it as a terminal's keys would arrive, and checks what `Console` reads and leaves behind.
    #[cfg(windows)]
    mod console {
        use std::ffi::c_void;
        use std::io::Read;
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};
        use std::time::Instant;

        use windows_sys::Win32::Foundation::{
            FALSE, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, TRUE,
        };
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        };
        use windows_sys::Win32::System::Console::{
            CTRL_BREAK_EVENT, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
            ENABLE_QUICK_EDIT_MODE, ENABLE_VIRTUAL_TERMINAL_INPUT, FOCUS_EVENT,
            GenerateConsoleCtrlEvent, GetConsoleMode, GetStdHandle, INPUT_RECORD, KEY_EVENT,
            STD_OUTPUT_HANDLE, SetConsoleCtrlHandler, WINDOW_BUFFER_SIZE_EVENT, WriteConsoleInputW,
        };
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
        use windows_sys::core::BOOL;

        use super::super::windows::raw_mode;
        use super::*;

        // The variable that makes `child` run: `setup`, `break` or `during`.
        const CHILD: &str = "MUX_AGENTS_CONSOLE_CHILD";
        // Where `observe` writes the mode it saw.
        const SEEN: &str = "MUX_AGENTS_CONSOLE_SEEN";
        // How a process ends by Ctrl-Break under the default handler: STATUS_CONTROL_C_EXIT.
        const CTRL_EXIT: u32 = 0xC000_013A;

        // Runs `child` for `case` with a hidden console of its own; answers its exit code and its stdout, where
        // `Console` writes ENABLE and DISABLE.
        fn run(case: &str, seen: Option<&std::path::Path>) -> (u32, Vec<u8>) {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "sidebar::input::tests::console::child",
                    "--include-ignored",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(CHILD, case)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .creation_flags(CREATE_NO_WINDOW);
            if let Some(seen) = seen {
                command.env(SEEN, seen);
            }
            let mut child = command.spawn().unwrap();
            let drain = |mut pipe: Box<dyn Read + Send>| {
                std::thread::spawn(move || {
                    let mut bytes = Vec::new();
                    let _ = pipe.read_to_end(&mut bytes);
                    bytes
                })
            };
            let out = drain(Box::new(child.stdout.take().unwrap()));
            let err = drain(Box::new(child.stderr.take().unwrap()));
            // The child sleeps 20 s if Ctrl-Break does not end it: give up after that.
            let deadline = Instant::now() + Duration::from_secs(30);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("{case}: the child did not end");
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            let (out, err) = (out.join().unwrap(), err.join().unwrap());
            let code = status.code().unwrap() as u32;
            assert!(
                code == 0 || code == CTRL_EXIT,
                "{case}: exit {code:#x}\n{}\n{}",
                out.escape_ascii(),
                err.escape_ascii()
            );
            (code, out)
        }

        // Setup takes the raw mode and turns mouse reports on; keys come back as their bytes, records with no key
        // never end a wait early, and Drop puts the console back and gives it up.
        #[test]
        fn console_sets_up_reads_keys_and_restores() {
            let (code, out) = run("setup", None);
            let text = out.escape_ascii().to_string();
            assert_eq!(code, 0, "{text}");
            let enable = find(&out, ENABLE).unwrap_or_else(|| panic!("no ENABLE: {text}"));
            let disable = find(&out, DISABLE).unwrap_or_else(|| panic!("no DISABLE: {text}"));
            assert!(enable < disable, "{text}");
        }

        // Ctrl-Break after setup: the handler puts the mode back and turns mouse reports off, then the process still
        // ends by Ctrl-Break. The next handler, run after ours, sees the mode as it was before setup.
        #[test]
        fn ctrl_break_restores_the_console_before_the_process_ends() {
            let seen = std::env::temp_dir().join(format!(
                "mux-agents-console-break-{}.txt",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&seen);
            let (code, out) = run("break", Some(&seen));
            let observed = std::fs::read_to_string(&seen).unwrap_or_default();
            let _ = std::fs::remove_file(&seen);
            let text = out.escape_ascii().to_string();
            assert_eq!(code, CTRL_EXIT, "{text}");
            let (before, after) = observed
                .split_once(' ')
                .unwrap_or_else(|| panic!("no mode seen: {observed:?} {text}"));
            assert_eq!(before, after, "the mode after the handler");
            let enable = find(&out, ENABLE).unwrap_or_else(|| panic!("no ENABLE: {text}"));
            let disable = find(&out, DISABLE).unwrap_or_else(|| panic!("no DISABLE: {text}"));
            assert!(enable < disable, "{text}");
        }

        // Ctrl-Break in the middle of setup, its handler running on its own thread before the ENABLE write: the
        // write is refused, so setup fails and the handler's restore stands. A later setup takes the console again.
        #[test]
        fn ctrl_break_during_setup_never_leaves_the_console_raw() {
            let (code, out) = run("during", None);
            let text = out.escape_ascii().to_string();
            assert_eq!(code, 0, "{text}");
            // The handler's DISABLE comes first; the only ENABLE is the later setup's.
            let disable = find(&out, DISABLE).unwrap_or_else(|| panic!("no DISABLE: {text}"));
            let enable = find(&out, ENABLE).unwrap_or_else(|| panic!("no ENABLE: {text}"));
            assert!(disable < enable, "{text}");
        }

        // CONIN$ for `observe`, and the mode before setup.
        static CONIN: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
        static BEFORE: AtomicU32 = AtomicU32::new(0);
        // Set by `swallow` once it ran, which is after our handler finished.
        static SWALLOWED: AtomicBool = AtomicBool::new(false);

        // Registered before setup, so it runs after `Console`'s handler: writes the mode before setup and the mode
        // now to `SEEN`, and lets the process end.
        unsafe extern "system" fn observe(_event: u32) -> BOOL {
            let mut now = 0;
            // SAFETY: `now` is valid for the call; CONIN$ stays open for the whole process.
            unsafe { GetConsoleMode(CONIN.load(Ordering::Relaxed), &mut now) };
            if let Ok(path) = std::env::var(SEEN) {
                let before = BEFORE.load(Ordering::Relaxed);
                let _ = std::fs::write(path, format!("{before:#x} {now:#x}"));
            }
            FALSE
        }

        // Registered before setup, so it runs after `Console`'s handler: keeps the process alive.
        unsafe extern "system" fn swallow(_event: u32) -> BOOL {
            SWALLOWED.store(true, Ordering::Release);
            TRUE
        }

        // The child's own console input, open for the whole process.
        fn conin() -> HANDLE {
            let name: Vec<u16> = "CONIN$".encode_utf16().chain([0]).collect();
            // SAFETY: `name` is a NUL-terminated wide string; the handle is never closed.
            let handle = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(
                handle,
                INVALID_HANDLE_VALUE,
                "CONIN$: {}",
                io::Error::last_os_error()
            );
            handle
        }

        fn stdout() -> HANDLE {
            // SAFETY: takes a constant.
            unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }
        }

        fn mode(handle: HANDLE) -> u32 {
            let mut mode = 0;
            // SAFETY: `mode` is valid for the call.
            assert_ne!(unsafe { GetConsoleMode(handle, &mut mode) }, 0);
            mode
        }

        // Writes `records` into the console's input, as a terminal's keys would arrive.
        fn type_in(handle: HANDLE, records: &[INPUT_RECORD]) {
            let mut n = 0;
            // SAFETY: `records` is valid for its length and `n` for the count written.
            let written = unsafe {
                WriteConsoleInputW(handle, records.as_ptr(), records.len() as u32, &mut n)
            };
            assert_ne!(written, 0, "{}", io::Error::last_os_error());
            assert_eq!(n as usize, records.len());
        }

        fn record(kind: u32) -> INPUT_RECORD {
            // SAFETY: INPUT_RECORD is plain data: all zeroes is a valid value.
            let mut record: INPUT_RECORD = unsafe { std::mem::zeroed() };
            record.EventType = kind as u16;
            record
        }

        fn key(down: bool, unit: u16) -> INPUT_RECORD {
            let mut key = record(KEY_EVENT);
            key.Event.KeyEvent.bKeyDown = i32::from(down);
            key.Event.KeyEvent.wRepeatCount = 1;
            key.Event.KeyEvent.uChar.UnicodeChar = unit;
            key
        }

        // Each character of `text` as a key-down record, as the console reports VT input.
        fn keys(text: &str) -> Vec<INPUT_RECORD> {
            text.encode_utf16().map(|unit| key(true, unit)).collect()
        }

        // A `Console` that sends Ctrl-Break inside its first write, before writing, and waits for the handlers to
        // finish: the handler then runs between `raw` and the ENABLE write.
        struct Breaking {
            console: Console,
            sent: bool,
        }

        impl Term for Breaking {
            fn save(&mut self) -> io::Result<()> {
                self.console.save()
            }

            fn arm(&mut self) -> io::Result<()> {
                self.console.arm()
            }

            fn raw(&mut self) -> io::Result<()> {
                self.console.raw()
            }

            fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
                if !std::mem::replace(&mut self.sent, true) {
                    // SAFETY: Ctrl-Break to the processes on this console: this child alone.
                    assert_ne!(unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, 0) }, 0);
                    let end = Instant::now() + Duration::from_secs(10);
                    while !SWALLOWED.load(Ordering::Acquire) {
                        assert!(Instant::now() < end, "Ctrl-Break never arrived");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                self.console.write(bytes)
            }

            fn restore(&mut self) {
                self.console.restore();
            }

            fn disarm(&mut self) {
                self.console.disarm();
            }

            fn poll_read(&mut self, clock: &dyn Clock, timeout: Duration) -> io::Result<Waited> {
                self.console.poll_read(clock, timeout)
            }
        }

        fn setup(input: HANDLE, before: u32) {
            let fakes = Fakes::default();
            // A handle that is not console input fails `save` and takes nothing: setup still works after.
            assert!(TermInput::setup(Console::on(INVALID_HANDLE_VALUE, stdout())).is_none());
            assert!(TermInput::setup(Console::on(stdout(), stdout())).is_none());
            let mut term =
                TermInput::setup(Console::on(input, stdout())).expect("the console sets up");
            let now = mode(input);
            assert_eq!(now, raw_mode(before), "{before:#x} -> {now:#x}");
            let cleared = ENABLE_LINE_INPUT
                | ENABLE_ECHO_INPUT
                | ENABLE_PROCESSED_INPUT
                | ENABLE_QUICK_EDIT_MODE;
            assert_eq!(now & cleared, 0, "{now:#x}");
            assert_ne!(now & ENABLE_VIRTUAL_TERMINAL_INPUT, 0, "{now:#x}");
            assert!(
                TermInput::setup(Console::on(input, stdout())).is_none(),
                "a second Console while the first is held"
            );

            // A focus record and a key-up are dropped; each key-down character comes back, in one read.
            let sent = "\x1b[<0;3;5M\x1b[<0;3;5m\x1b[Aé\r\x03";
            let mut records = vec![
                record(FOCUS_EVENT),
                key(true, 'j' as u16),
                key(false, 'j' as u16),
            ];
            records.extend(keys(sent));
            type_in(input, &records);
            let want = format!("j{sent}").into_bytes();
            assert_eq!(
                term.wait(&fakes, Duration::from_secs(5)),
                Waited::Bytes(want)
            );

            // A resize alone wakes the wait but holds no key: the wait goes on to its deadline.
            type_in(input, &[record(WINDOW_BUFFER_SIZE_EVENT)]);
            let start = Instant::now();
            assert_eq!(
                term.wait(&fakes, Duration::from_millis(300)),
                Waited::Timeout
            );
            assert!(
                start.elapsed() >= Duration::from_millis(290),
                "{:?}",
                start.elapsed()
            );

            // A zero wait reads keys already waiting, and times out when there are none.
            type_in(input, &keys("k"));
            assert_eq!(
                term.wait(&fakes, Duration::ZERO),
                Waited::Bytes(b"k".to_vec())
            );
            assert_eq!(term.wait(&fakes, Duration::ZERO), Waited::Timeout);
            assert!(fakes.slept().is_empty(), "waits run on the real time");

            drop(term);
            assert_eq!(mode(input), before, "Drop restores the mode");
            // Given back: the next Console takes the console again.
            drop(TermInput::setup(Console::on(input, stdout())).expect("set up again"));
            assert_eq!(mode(input), before);
        }

        fn ctrl_break(input: HANDLE, before: u32) {
            // SAFETY: `observe` only reads the console mode and writes a file.
            assert_ne!(unsafe { SetConsoleCtrlHandler(Some(observe), TRUE) }, 0);
            let term = TermInput::setup(Console::on(input, stdout())).expect("the console sets up");
            assert_eq!(mode(input), raw_mode(before));
            // SAFETY: Ctrl-Break to the processes on this console: this child alone.
            assert_ne!(unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, 0) }, 0);
            std::thread::sleep(Duration::from_secs(20));
            drop(term);
            panic!("Ctrl-Break did not end the process");
        }

        fn during(input: HANDLE, before: u32) {
            // SAFETY: `swallow` only stores a flag.
            assert_ne!(unsafe { SetConsoleCtrlHandler(Some(swallow), TRUE) }, 0);
            let breaking = Breaking {
                console: Console::on(input, stdout()),
                sent: false,
            };
            assert!(
                TermInput::setup(breaking).is_none(),
                "the ENABLE write after the handler is refused"
            );
            assert_eq!(mode(input), before, "the handler's restore stands");
            drop(TermInput::setup(Console::on(input, stdout())).expect("set up again"));
            assert_eq!(mode(input), before);
        }

        // Run only by the tests above, in its own process with a hidden console; without `CHILD` it never touches
        // the console.
        #[test]
        #[ignore = "a child of the console tests"]
        fn child() {
            let Ok(case) = std::env::var(CHILD) else {
                return;
            };
            let input = conin();
            CONIN.store(input, Ordering::Relaxed);
            let before = mode(input);
            BEFORE.store(before, Ordering::Relaxed);
            let cooked = ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT;
            assert_eq!(
                before & cooked,
                cooked,
                "a new console is cooked: {before:#x}"
            );
            match case.as_str() {
                "setup" => setup(input, before),
                "break" => ctrl_break(input, before),
                "during" => during(input, before),
                other => panic!("unknown case {other}"),
            }
        }
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

    #[cfg(any(unix, windows))]
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

    // The current action for `signal`, whole.
    #[cfg(unix)]
    fn current(signal: libc::c_int) -> libc::sigaction {
        // SAFETY: a zeroed sigaction is a valid value for sigaction to fill in.
        let mut now: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: a null new action only reads the current one into `now`.
        assert_eq!(
            unsafe { libc::sigaction(signal, std::ptr::null(), &mut now) },
            0
        );
        now
    }

    // The current handler for `signal`.
    #[cfg(unix)]
    fn action(signal: libc::c_int) -> libc::sighandler_t {
        current(signal).sa_sigaction
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

    // Each poll answer and each read answer, including those a real terminal will not give on demand: EINTR or
    // EAGAIN on a read poll called ready, an error event, a failed poll, bytes beside a hang-up.
    #[cfg(unix)]
    #[test]
    fn poll_and_read_answers() {
        use super::unix::{Polled, after_poll, after_read};
        let os = io::Error::from_raw_os_error;
        let polled = |ready, revents| format!("{:?}", after_poll(ready, revents));
        let closed = format!("{:?}", Polled::Done(Ok(Waited::Closed)));
        assert_eq!(polled(Err(os(libc::EINTR)), 0), "Again");
        assert!(
            matches!(after_poll(Err(os(libc::EINVAL)), 0), Polled::Done(Err(e)) if e.raw_os_error() == Some(libc::EINVAL))
        );
        assert_eq!(polled(Ok(0), 0), "Again");
        assert_eq!(polled(Ok(1), libc::POLLIN), "Read");
        assert_eq!(
            polled(Ok(1), libc::POLLIN | libc::POLLHUP),
            "Read",
            "bytes before the hang-up"
        );
        assert_eq!(polled(Ok(1), libc::POLLIN | libc::POLLERR), "Read");
        for event in [libc::POLLHUP, libc::POLLERR, libc::POLLNVAL] {
            assert_eq!(polled(Ok(1), event), closed, "{event}");
        }
        assert_eq!(polled(Ok(1), libc::POLLOUT), "Again");

        assert!(after_read(Err(os(libc::EINTR))).is_none());
        assert!(after_read(Err(os(libc::EAGAIN))).is_none());
        assert!(
            matches!(after_read(Err(os(libc::EIO))), Some(Err(e)) if e.raw_os_error() == Some(libc::EIO))
        );
        assert_eq!(after_read(Ok(b"")).unwrap().unwrap(), Waited::Closed);
        assert_eq!(
            after_read(Ok(b"jk")).unwrap().unwrap(),
            Waited::Bytes(b"jk".to_vec())
        );
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
        // Someone else's SIGTERM handler, with its own flags and mask, all of which come back.
        // SAFETY: a zeroed sigaction is a valid value; the mask and flags are set before it is installed, and the
        // default goes back at the end of this test, under `PTY`.
        unsafe {
            let mut theirs: libc::sigaction = std::mem::zeroed();
            theirs.sa_sigaction = winch;
            theirs.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut theirs.sa_mask);
            libc::sigaddset(&mut theirs.sa_mask, libc::SIGUSR1);
            assert_eq!(
                libc::sigaction(libc::SIGTERM, &theirs, std::ptr::null_mut()),
                0
            );
        }
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
        let back = current(libc::SIGTERM);
        assert_eq!(back.sa_sigaction, winch, "the replaced handler is back");
        assert_ne!(back.sa_flags & libc::SA_RESTART, 0, "with its flags");
        // SAFETY: `back.sa_mask` is a valid signal set sigaction filled in.
        assert_eq!(
            unsafe { libc::sigismember(&back.sa_mask, libc::SIGUSR1) },
            1,
            "and its mask"
        );
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
            let mut seen = Vec::new();
            while seen.len() < full + LEN {
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
                seen.extend_from_slice(&buf[..n as usize]);
                std::thread::sleep(Duration::from_millis(1));
            }
            seen
        });
        WINCHED.store(0, Ordering::Relaxed);
        // No two neighbouring KiB alike, so a piece written twice or skipped shows.
        let bytes: Vec<u8> = (0..LEN).map(|i| (i % 251) as u8).collect();
        let wrote = Tty::on(fd, fd).write(&bytes);
        let during = WINCHED.load(Ordering::Relaxed);
        done.store(true, Ordering::Relaxed);
        kicker.join().unwrap();
        let seen = reader.join().unwrap();
        install(libc::SIGWINCH, libc::SIG_DFL);
        assert!(wrote.is_ok(), "{wrote:?}");
        assert_eq!(seen.len(), full + LEN, "every byte, none twice");
        assert!(seen[..full].iter().all(|&b| b == b'f'));
        assert!(seen[full..] == bytes[..], "the bytes in order");
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
                let enable = find(&seen, ENABLE);
                match when {
                    "after-setup" => assert!(enable.is_some(), "{case}: {seen_text}"),
                    _ => assert_eq!(enable, None, "{case}: {seen_text}"),
                }
                // A hang-up means the terminal is gone: nothing is written to it.
                let disable = find(&seen, DISABLE);
                if signal == libc::SIGHUP {
                    assert_eq!(disable, None, "{case}: {seen_text}");
                } else {
                    let disable =
                        disable.unwrap_or_else(|| panic!("{case}: no DISABLE in {seen_text}"));
                    assert!(
                        enable.is_none_or(|enable| enable < disable),
                        "{case}: {seen_text}"
                    );
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
