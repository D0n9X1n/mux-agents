//! The side bar's own terminal: raw mode and mouse reporting while `watch` runs, restored on every way it ends.
#![allow(dead_code)] // Until T9 gives every item a caller (#61).

use std::io;
use std::time::Duration;

use crate::ctx::Clock;

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
        let enable = write(ENABLE);
        let partway = write(&ENABLE[..4]);
        let disable = write(DISABLE);
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
    fn term_input_errors_are_closed() {
        let fakes = Fakes::default();
        let mut term = FakeTerm::new();
        term.reads.push_back(Err(io::Error::from_raw_os_error(5))); // EIO
        let mut input = TermInput::setup(term).expect("every step succeeds");
        assert_eq!(
            input.wait(&fakes, Duration::from_millis(2000)),
            Waited::Closed
        );
    }
}
