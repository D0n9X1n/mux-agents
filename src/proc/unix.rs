//! What Linux and macOS share: the shells, the stop report, the pid probe and the Linux pidfd source.

use std::io;

use serde_json::{Map, Value};

use super::{Left, Row};

/// The shells allowed between the caller and its agent or pane, by real-path basename, case-sensitive (Unix.ps1:8).
pub const SHELLS: [&str; 8] = ["sh", "bash", "zsh", "fish", "dash", "ksh", "tcsh", "pwsh"];

pub fn is_shell(row: &Row) -> bool {
    row.path
        .as_deref()
        .and_then(|path| path.rsplit('/').next())
        .is_some_and(|file| SHELLS.contains(&file))
}

/// Every tree row stop did not kill, whatever the reason, is a survivor (Unix.ps1:17-19).
pub fn stop_report(left: &[Left]) -> Map<String, Value> {
    let pids: Vec<Value> = left.iter().map(|l| l.pid.into()).collect();
    let mut report = Map::new();
    report.insert("survivors".into(), pids.len().into());
    report.insert("survivorPids".into(), Value::Array(pids));
    report
}

/// kill(pid, 0), which sends nothing: 0 when the process exists, else errno (ESRCH gone, EPERM another user's).
/// A pid that is not positive would name a group, so it is EINVAL without a call.
pub fn probe(pid: u32) -> i32 {
    let Some(pid) = libc::pid_t::try_from(pid).ok().filter(|&p| p > 0) else {
        return libc::EINVAL;
    };
    // SAFETY: signal 0 only checks the pid.
    if unsafe { libc::kill(pid, 0) } == 0 {
        0
    } else {
        io::Error::last_os_error().raw_os_error().unwrap_or(-1)
    }
}

/// Linux: the pidfd, which pins the process it opened, so a kill through it cannot reach a reused pid.
#[cfg(target_os = "linux")]
pub struct PidfdSource;

#[cfg(target_os = "linux")]
impl super::seam::ProcSource for PidfdSource {
    type H = std::os::fd::OwnedFd;

    fn read_start(&self, pid: u32) -> Option<i64> {
        let text = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
        crate::parse::linux_stat(&text)
            .filter(|stat| stat.pid == pid)
            .map(|stat| stat.start)
    }

    fn open_handle(&self, pid: u32) -> Option<Self::H> {
        use std::os::fd::FromRawFd;
        let pid = libc::pid_t::try_from(pid).ok().filter(|&p| p > 0)?;
        // SAFETY: pidfd_open takes a pid and flags and returns a new fd or -1.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        // SAFETY: a non-negative result is a fresh fd nothing else owns.
        (fd >= 0).then(|| unsafe { std::os::fd::OwnedFd::from_raw_fd(fd as std::os::fd::RawFd) })
    }

    fn send_kill(&self, handle: &Self::H) -> bool {
        use std::os::fd::AsRawFd;
        // SAFETY: the fd is a live pidfd; a null siginfo asks for a plain SIGKILL.
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                handle.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            ) == 0
        }
    }

    fn wait_exit(&self, handle: &Self::H, ms: u32) -> bool {
        use std::os::fd::AsRawFd;
        // A pidfd polls readable once its process exited.
        let mut item = libc::pollfd {
            fd: handle.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout = ms.min(i32::MAX as u32) as libc::c_int;
        // SAFETY: one live pollfd.
        let count = unsafe { libc::poll(&mut item, 1, timeout) };
        crate::parse::poll_shows_exit(count, item.revents)
    }

    fn alive(&self, handle: &Self::H) -> bool {
        use std::os::fd::AsRawFd;
        let mut item = libc::pollfd {
            fd: handle.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one live pollfd, no wait.
        let count = unsafe { libc::poll(&mut item, 1, 0) };
        // Only "nothing ready" is alive; an exit, an error or a bad fd is not.
        count == 0
    }

    fn probe(&self, pid: u32) -> i32 {
        probe(pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proc::Reason;

    fn row(path: Option<&str>) -> Row {
        Row {
            pid: 1,
            ppid: 0,
            start: 0,
            path: path.map(str::to_string),
            argv: None,
        }
    }

    #[test]
    fn shells_by_basename_case_sensitive() {
        let answers: Vec<bool> = [
            Some("/bin/sh"),
            Some("/usr/bin/bash"),
            Some("/bin/zsh"),
            Some("/usr/local/bin/fish"),
            Some("/bin/dash"),
            Some("/bin/ksh"),
            Some("/bin/tcsh"),
            Some("/opt/microsoft/powershell/7/pwsh"),
            Some("/bin/Bash"),
            Some("/bin/pwsh.exe"),
            Some("/usr/bin/node"),
            Some("/bin/csh"),
            None,
        ]
        .iter()
        .map(|p| is_shell(&row(*p)))
        .collect();
        let mut want = vec![true; 8];
        want.extend([false; 5]);
        assert_eq!(answers, want);
    }

    #[test]
    fn stop_report_lists_every_survivor_pid() {
        let left = [
            (310, Reason::Kill),
            (320, Reason::Start),
            (330, Reason::Open),
        ]
        .map(|(pid, reason)| Left { pid, reason });
        assert_eq!(
            Value::Object(stop_report(&left)).to_string(),
            r#"{"survivors":3,"survivorPids":[310,320,330]}"#
        );
        assert_eq!(
            Value::Object(stop_report(&[])).to_string(),
            r#"{"survivors":0,"survivorPids":[]}"#
        );
    }

    #[test]
    fn probe_errno() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let pid = child.id();
        assert_eq!(probe(pid), 0);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(probe(pid), libc::ESRCH);
        // A pid that names a group, or none, is EINVAL here, never a signal to a group.
        assert_eq!(probe(0), libc::EINVAL);
        assert_eq!(probe(u32::MAX), libc::EINVAL);
        // SAFETY: getuid has no preconditions.
        if unsafe { libc::getuid() } != 0 {
            assert_eq!(probe(1), libc::EPERM);
        }
    }
}
