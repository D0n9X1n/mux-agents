//! Open and kill over a handle-or-probe source (Unix.ps1:125-141); no OS types here, so it is tested on every host.

use std::time::Duration;

pub trait ProcSource {
    type H;
    fn read_start(&self, pid: u32) -> Option<i64>;
    fn open_handle(&self, pid: u32) -> Option<Self::H>;
    fn send_kill(&self, handle: &Self::H) -> bool;
    fn wait_exit(&self, handle: &Self::H, ms: u32) -> bool;
    /// kill(pid, 0): 0 when the process exists, else errno.
    fn probe(&self, pid: u32) -> i32;
    fn sleep(&self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// A process held for a kill. It owns its handle, so dropping it closes the handle.
#[derive(Debug)]
pub struct Opened<H> {
    pub pid: u32,
    pub start: i64,
    handle: Option<H>,
}

impl<H> Opened<H> {
    pub fn new(pid: u32, start: i64, handle: Option<H>) -> Self {
        Opened { pid, start, handle }
    }

    pub fn handle(&self) -> Option<&H> {
        self.handle.as_ref()
    }
}

// errno ESRCH (3 on Linux and macOS): the only proof a process without a handle is gone.
const ESRCH: i32 = 3;
const KILL_WAIT_MS: u32 = 5000;
const POLL_ROUNDS: u32 = 50;
const POLL_MS: u64 = 100;

/// The handle first, then the start, so the start belongs to the process the handle holds; an unreadable start
/// is no process, and the handle closes with it.
pub fn open_with<S: ProcSource>(src: &S, pid: u32) -> Option<Opened<S::H>> {
    let handle = src.open_handle(pid);
    let start = src.read_start(pid)?;
    Some(Opened::new(pid, start, handle))
}

/// Keeps an opened process only when its start is the listed one: a pid reused between listing and opening is
/// another process, left as `start`, and its handle closes here without a signal.
pub fn hold<H>(
    opened: Option<Opened<H>>,
    listed: i64,
    same: impl Fn(i64, i64) -> bool,
) -> Result<Opened<H>, super::Reason> {
    let opened = opened.ok_or(super::Reason::Open)?;
    if same(opened.start, listed) {
        Ok(opened)
    } else {
        Err(super::Reason::Start)
    }
}

/// True when the opened process exited within 5 s. With a handle, SIGKILL goes through it, which cannot reach a
/// reused pid. Without one nothing is sent: only ESRCH shows the exit, since a pid is never signalled.
pub fn kill_with<S: ProcSource>(src: &S, opened: Opened<S::H>) -> bool {
    if let Some(handle) = opened.handle() {
        // The send result is no proof; the wait is.
        let _ = src.send_kill(handle);
        return src.wait_exit(handle, KILL_WAIT_MS);
    }
    (0..POLL_ROUNDS).any(|_| {
        src.sleep(POLL_MS);
        src.probe(opened.pid) == ESRCH
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proc::Reason;
    use std::cell::RefCell;
    use std::rc::Rc;

    type Log = Rc<RefCell<Vec<String>>>;

    struct FakeHandle(u32, Log);

    impl Drop for FakeHandle {
        fn drop(&mut self) {
            self.1.borrow_mut().push(format!("close {}", self.0));
        }
    }

    #[derive(Default)]
    struct Fake {
        log: Log,
        handle: bool,
        start: Option<i64>,
        exits: bool,
        // Probe answers in turn; the last one repeats.
        probes: Vec<i32>,
    }

    impl Fake {
        fn calls(&self) -> Vec<String> {
            self.log.borrow().clone()
        }
    }

    impl ProcSource for Fake {
        type H = FakeHandle;
        fn read_start(&self, pid: u32) -> Option<i64> {
            self.log.borrow_mut().push(format!("start {pid}"));
            self.start
        }
        fn open_handle(&self, pid: u32) -> Option<FakeHandle> {
            self.log.borrow_mut().push(format!("handle {pid}"));
            self.handle.then(|| FakeHandle(pid, self.log.clone()))
        }
        fn send_kill(&self, handle: &FakeHandle) -> bool {
            self.log.borrow_mut().push(format!("kill {}", handle.0));
            false
        }
        fn wait_exit(&self, handle: &FakeHandle, ms: u32) -> bool {
            self.log
                .borrow_mut()
                .push(format!("wait {} {ms}", handle.0));
            self.exits
        }
        fn probe(&self, pid: u32) -> i32 {
            let mut log = self.log.borrow_mut();
            let round = log.iter().filter(|c| c.starts_with("probe")).count();
            log.push(format!("probe {pid}"));
            self.probes[round.min(self.probes.len() - 1)]
        }
        fn sleep(&self, ms: u64) {
            self.log.borrow_mut().push(format!("sleep {ms}"));
        }
    }

    #[test]
    fn opened_drop_closes() {
        // Handle first, then start (Unix.ps1:129-132); an unreadable start closes the handle at once.
        let src = Fake {
            handle: true,
            ..Fake::default()
        };
        assert!(open_with(&src, 7).is_none());
        assert_eq!(src.calls(), ["handle 7", "start 7", "close 7"]);

        let src = Fake {
            handle: true,
            start: Some(1500),
            ..Fake::default()
        };
        let opened = open_with(&src, 7).unwrap();
        assert_eq!(
            (opened.pid, opened.start, opened.handle().is_some()),
            (7, 1500, true)
        );
        assert_eq!(src.calls(), ["handle 7", "start 7"]);
        drop(opened);
        assert_eq!(src.calls(), ["handle 7", "start 7", "close 7"]);

        // No handle (macOS): the start alone is enough to open.
        let src = Fake {
            start: Some(1500),
            ..Fake::default()
        };
        assert!(open_with(&src, 7).unwrap().handle().is_none());
    }

    #[test]
    fn kill_with_handle_signals_waits_5s_and_closes() {
        // The send result is not proof; only the wait is, and the handle closes either way.
        for exits in [true, false] {
            let src = Fake {
                exits,
                ..Fake::default()
            };
            let opened = Opened::new(9, 1, Some(FakeHandle(9, src.log.clone())));
            assert_eq!(kill_with(&src, opened), exits);
            assert_eq!(src.calls(), ["kill 9", "wait 9 5000", "close 9"]);
        }
    }

    #[test]
    fn kill_without_handle_polls_esrch_50_times() {
        // EPERM, success or any other errno is not gone; only ESRCH is.
        let src = Fake {
            probes: vec![0, 1, 22, 3],
            ..Fake::default()
        };
        assert!(kill_with(&src, Opened::new(9, 1, None)));
        let calls = src.calls();
        assert_eq!(calls.iter().filter(|c| c.starts_with("probe")).count(), 4);
        assert_eq!(calls[..2], ["sleep 100", "probe 9"]);
        assert!(!calls.iter().any(|c| c.starts_with("kill")));

        let src = Fake {
            probes: vec![1],
            ..Fake::default()
        };
        assert!(!kill_with(&src, Opened::new(9, 1, None)));
        let calls = src.calls();
        assert_eq!(calls.len(), 100);
        assert_eq!(calls.iter().filter(|c| *c == "sleep 100").count(), 50);
    }

    #[test]
    fn open_rejects_start_change_between_read_and_pidfd() {
        // Listed at start A; the pid exited and was reused before the pidfd opened, so the fd holds start B.
        let listed = 1000;
        let src = Fake {
            handle: true,
            start: Some(2000),
            ..Fake::default()
        };
        let opened = open_with(&src, 7);
        assert_eq!(opened.as_ref().map(|o| o.start), Some(2000));
        assert!(matches!(
            hold(opened, listed, |a, b| a == b),
            Err(Reason::Start)
        ));
        // Rejected, the fd closed at once and nothing was signalled.
        assert_eq!(src.calls(), ["handle 7", "start 7", "close 7"]);

        // No process at all is `open`; the same start is held with its fd.
        let none = hold::<FakeHandle>(None, listed, |a, b| a == b);
        assert!(matches!(none, Err(Reason::Open)));
        let src = Fake {
            handle: true,
            start: Some(listed),
            ..Fake::default()
        };
        let held = hold(open_with(&src, 7), listed, |a, b| a == b).unwrap();
        assert!(held.handle().is_some());
        assert_eq!(src.calls(), ["handle 7", "start 7"]);
    }

    #[test]
    fn kill_after_reuse_never_signals_replacement() {
        // The opened process exited and its pid now names another; whether the wait shows the exit or not, the
        // signal goes only through the pidfd and the pid is never probed or signalled.
        for exits in [true, false] {
            let src = Fake {
                exits,
                probes: vec![0],
                ..Fake::default()
            };
            let opened = Opened::new(9, 1000, Some(FakeHandle(9, src.log.clone())));
            assert_eq!(kill_with(&src, opened), exits);
            assert_eq!(src.calls(), ["kill 9", "wait 9 5000", "close 9"]);
        }
    }
}
