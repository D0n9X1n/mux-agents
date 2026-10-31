//! The Linux process table: /proc reads and the pidfd, no helper processes (Unix.ps1:49-75, 84-101, 125-141).

use std::io;

use serde_json::{Map, Value};

use super::seam;
use super::unix::{self, PidfdSource};
use super::{Left, Opened, Os, Row, Signals, Token};
use crate::parse::{self, Client};

pub struct Linux;

// A /proc link's target as text; a pid that is gone, another user's or not UTF-8 is none.
fn link(pid: u32, name: &str) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/{name}"))
        .ok()?
        .into_os_string()
        .into_string()
        .ok()
}

// An empty cmdline (a kernel thread or a zombie) has no final NUL, so it is unreadable too.
fn argv(pid: u32) -> Option<Vec<String>> {
    parse::linux_cmdline(&std::fs::read(format!("/proc/{pid}/cmdline")).ok()?)
}

impl Os for Linux {
    fn row(&self, pid: u32) -> Option<Row> {
        let stat = parse::linux_stat(&std::fs::read(format!("/proc/{pid}/stat")).ok()?)?;
        // The stat file names its own pid; anything else is not this row.
        if stat.pid != pid {
            return None;
        }
        Some(Row {
            pid,
            ppid: stat.ppid,
            start: stat.start,
            path: link(pid, "exe"),
            argv: argv(pid),
        })
    }

    /// Every numeric /proc entry whose stat reads; one that exits mid-read is skipped.
    fn table(&self) -> io::Result<Vec<Row>> {
        let mut rows = Vec::new();
        for entry in std::fs::read_dir("/proc")? {
            let name = entry?.file_name();
            let pid = name.to_str().and_then(|n| n.parse::<u32>().ok());
            if let Some(row) = pid.and_then(|pid| self.row(pid)) {
                rows.push(row);
            }
        }
        Ok(rows)
    }

    fn open(&self, pid: u32) -> Option<Opened> {
        seam::open_with(&PidfdSource, pid)
    }

    fn kill(&self, opened: Opened) -> bool {
        seam::kill_with(&PidfdSource, opened)
    }

    fn cwd(&self, pid: u32) -> Option<String> {
        link(pid, "cwd")
    }

    fn client_of(&self, row: &Row) -> Option<Client> {
        parse::unix_client(
            row.path.as_deref(),
            row.argv.as_deref(),
            || self.cwd(row.pid),
            // realpath(3): every link in every component resolved.
            |path| {
                std::fs::canonicalize(path)
                    .ok()?
                    .into_os_string()
                    .into_string()
                    .ok()
            },
        )
    }

    fn is_shell(&self, row: &Row) -> bool {
        unix::is_shell(row)
    }

    // Raw starttime ticks on both sides, so exact.
    fn same_start(&self, opened: i64, listed: i64) -> bool {
        opened == listed
    }

    fn stop_report(&self, left: &[Left]) -> Map<String, Value> {
        unix::stop_report(left)
    }

    // Unix has no token (Unix.ps1 binds Token to $null).
    fn token(&self) -> Option<Token> {
        None
    }

    fn sandbox(&self) -> Signals {
        Signals::Status(std::fs::read_to_string("/proc/self/status").ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proc::Handle;
    use crate::proc::seam::{self, ProcSource};
    use crate::proc::unix::PidfdSource;
    use std::process::{Child, Command};

    fn me() -> u32 {
        std::process::id()
    }

    fn sleeper() -> Child {
        Command::new("/bin/sleep").arg("60").spawn().unwrap()
    }

    // Seconds since boot, as /proc/uptime gives them, in clock ticks.
    fn uptime_ticks() -> i64 {
        let text = std::fs::read_to_string("/proc/uptime").unwrap();
        let secs: f64 = text.split(' ').next().unwrap().parse().unwrap();
        // SAFETY: sysconf has no preconditions.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
        (secs * hz) as i64
    }

    #[test]
    fn row_of_self_from_proc() {
        let row = Linux.row(me()).unwrap();
        assert_eq!(row.pid, me());
        assert_eq!(row.ppid, std::os::unix::process::parent_id());
        let exe = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        assert_eq!(row.path.as_deref(), exe.to_str());
        assert_eq!(row.argv.unwrap(), std::env::args().collect::<Vec<_>>());
        let here = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
        assert_eq!(Linux.cwd(me()).as_deref(), here.to_str());
        assert_eq!(Linux.row(u32::MAX), None);
        let table = Linux.table().unwrap();
        assert!(table.iter().any(|r| r.pid == me()));
        assert!(table.iter().any(|r| r.pid == row.ppid));
    }

    #[test]
    fn start_is_raw_ticks() {
        // starttime counts clock ticks since boot, so a fresh child's falls between two uptime reads (one
        // tick of slack each side for /proc/uptime's centiseconds).
        let before = uptime_ticks();
        let mut child = sleeper();
        let after = uptime_ticks();
        let start = Linux.row(child.id()).map(|r| r.start);
        let opened = Linux.open(child.id()).map(|o| o.start);
        child.kill().unwrap();
        child.wait().unwrap();
        let start = start.unwrap();
        assert!(
            before - 1 <= start && start <= after + 1,
            "{before} {start} {after}"
        );
        assert_eq!(opened, Some(start));
        assert!(Linux.same_start(start, start));
        assert!(!Linux.same_start(start, start + 1));
    }

    #[test]
    fn pidfd_kill_child_true() {
        let mut child = sleeper();
        let opened = Linux.open(child.id()).unwrap();
        assert!(opened.handle().is_some(), "no pidfd on this kernel");
        assert!(Linux.kill(opened));
        // Exited, so a zombie until reaped; SIGKILL is what ended it.
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGKILL));
    }

    // The real backend as a kernel without pidfd sees it, without the 100 ms naps.
    struct NoPidfd;

    impl ProcSource for NoPidfd {
        type H = Handle;
        fn read_start(&self, pid: u32) -> Option<i64> {
            PidfdSource.read_start(pid)
        }
        fn open_handle(&self, _: u32) -> Option<Handle> {
            None
        }
        fn send_kill(&self, handle: &Handle) -> bool {
            PidfdSource.send_kill(handle)
        }
        fn wait_exit(&self, handle: &Handle, ms: u32) -> bool {
            PidfdSource.wait_exit(handle, ms)
        }
        fn probe(&self, pid: u32) -> i32 {
            PidfdSource.probe(pid)
        }
        fn sleep(&self, _: u64) {}
    }

    #[test]
    fn no_pidfd_falls_back_to_esrch_poll() {
        let mut child = sleeper();
        let opened = seam::open_with(&NoPidfd, child.id()).unwrap();
        assert!(opened.handle().is_none());
        // Nothing is sent by pid: the child outlives all 50 probes, and alive is not gone.
        assert!(!seam::kill_with(&NoPidfd, opened));
        assert_eq!(child.try_wait().unwrap(), None);

        let opened = seam::open_with(&NoPidfd, child.id()).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(seam::kill_with(&NoPidfd, opened));
    }
}
