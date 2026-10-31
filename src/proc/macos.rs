//! The macOS process table: libproc and KERN_PROCARGS2, no helper processes (the reference ran ps). macOS has no pidfd, so a kill sends
//! nothing and only watches for the exit (Unix.ps1:121-141).

use std::io;
use std::mem::{MaybeUninit, size_of};
use std::ptr::null_mut;

use libc::c_int;
use serde_json::{Map, Value};

use super::seam::{self, ProcSource};
use super::{Handle, Left, Opened, Os, Row, Signals, Token, unix};
use crate::parse::{self, Client};

pub struct MacOs;

// libsystem_sandbox, part of libSystem; the libc crate does not declare it.
unsafe extern "C" {
    fn sandbox_check(pid: libc::pid_t, operation: *const libc::c_char, filter: c_int, ...)
    -> c_int;
}

// The reference's buffer when kern.argmax cannot be read.
const ARGMAX_FALLBACK: usize = 1 << 20;
// Room for pids that start between counting and listing.
const PID_SLACK: usize = 64;

fn bsdinfo(pid: u32) -> Option<libc::proc_bsdinfo> {
    let pid = c_int::try_from(pid).ok()?;
    let size = size_of::<libc::proc_bsdinfo>() as c_int;
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    // SAFETY: the buffer is one proc_bsdinfo of `size` bytes.
    let got = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    // SAFETY: zeroed is a valid proc_bsdinfo, and a full read filled it.
    (got == size).then(|| unsafe { info.assume_init() })
}

// Epoch microseconds, the unit the server key hashes (spec D1).
fn start_of(info: &libc::proc_bsdinfo) -> Option<i64> {
    i64::try_from(info.pbi_start_tvsec)
        .ok()?
        .checked_mul(1_000_000)?
        .checked_add(i64::try_from(info.pbi_start_tvusec).ok()?)
}

fn path(pid: u32) -> Option<String> {
    let pid = c_int::try_from(pid).ok()?;
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer holds `len` writable bytes.
    let got = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    buf.truncate(usize::try_from(got).ok().filter(|&n| n > 0)?);
    String::from_utf8(buf).ok()
}

fn argmax() -> usize {
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    let mut value: c_int = 0;
    let mut size = size_of::<c_int>();
    // SAFETY: a two-name read into one c_int.
    let read = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            2,
            (&raw mut value).cast(),
            &mut size,
            null_mut(),
            0,
        )
    };
    match usize::try_from(value) {
        Ok(n) if read == 0 && n > 0 => n,
        _ => ARGMAX_FALLBACK,
    }
}

// A zeroed kern.argmax buffer: calloc'd pages, touched only as far as an argv reaches. Reused across a table.
fn args_buffer() -> Vec<u8> {
    vec![0u8; argmax()]
}

// KERN_PROCARGS2; another user's process (EINVAL) or any other layout is unreadable.
fn argv(pid: u32, buf: &mut [u8]) -> Option<Vec<String>> {
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROCARGS2,
        c_int::try_from(pid).ok()?,
    ];
    let mut size = buf.len();
    // SAFETY: a three-name read into `buf`, whose length `size` gives.
    let read = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut size,
            null_mut(),
            0,
        )
    };
    if read != 0 {
        return None;
    }
    parse::procargs2(&buf[..size.min(buf.len())])
}

// Start re-read last, so a pid reused between the reads gives no row rather than a mixed one.
fn row_with(pid: u32, buf: &mut [u8]) -> Option<Row> {
    seam::read_row(
        pid,
        |pid| {
            let info = bsdinfo(pid)?;
            Some((info.pbi_ppid, start_of(&info)?))
        },
        path,
        |pid| argv(pid, buf),
    )
}

impl ProcSource for MacOs {
    type H = Handle;
    fn read_start(&self, pid: u32) -> Option<i64> {
        start_of(&bsdinfo(pid)?)
    }
    // No pidfd on macOS: nothing to hold, nothing to signal through.
    fn open_handle(&self, _: u32) -> Option<Handle> {
        None
    }
    fn send_kill(&self, _: &Handle) -> bool {
        false
    }
    fn wait_exit(&self, _: &Handle, _: u32) -> bool {
        false
    }
    fn alive(&self, _: &Handle) -> bool {
        false
    }
    fn probe(&self, pid: u32) -> i32 {
        unix::probe(pid)
    }
}

impl Os for MacOs {
    fn row(&self, pid: u32) -> Option<Row> {
        row_with(pid, &mut args_buffer())
    }

    /// Every process whose start is readable; one that exits mid-read is skipped.
    fn table(&self) -> io::Result<Vec<Row>> {
        // SAFETY: a null buffer asks only for the count.
        let count = unsafe { libc::proc_listallpids(null_mut(), 0) };
        let count = usize::try_from(count).map_err(|_| io::Error::last_os_error())?;
        let mut pids: Vec<c_int> = vec![0; count + PID_SLACK];
        let bytes = (pids.len() * size_of::<c_int>()) as c_int;
        // SAFETY: the buffer holds `bytes` writable bytes.
        let listed = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        let listed = usize::try_from(listed).map_err(|_| io::Error::last_os_error())?;
        pids.truncate(listed.min(pids.len()));
        let mut buf = args_buffer();
        Ok(pids
            .into_iter()
            .filter_map(|pid| u32::try_from(pid).ok())
            .filter_map(|pid| row_with(pid, &mut buf))
            .collect())
    }

    fn open(&self, pid: u32) -> Option<Opened> {
        seam::open_with(self, pid)
    }

    fn kill(&self, opened: Opened) -> bool {
        seam::kill_with(self, opened)
    }

    fn cwd(&self, pid: u32) -> Option<String> {
        let pid = c_int::try_from(pid).ok()?;
        let mut info = vec![0u8; parse::VNODE_INFO_SIZE];
        // SAFETY: the buffer holds VNODE_INFO_SIZE writable bytes.
        let got = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                info.as_mut_ptr().cast(),
                info.len() as c_int,
            )
        };
        parse::vnode_cwd(&info, got)
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
        // SAFETY: the caller's own pid, no operation and SANDBOX_FILTER_NONE, so no variadic arguments follow.
        Signals::SandboxCheck(unsafe { sandbox_check(libc::getpid(), std::ptr::null(), 0) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;
    use std::path::Path;
    use std::process::{Child, Command};
    use std::time::SystemTime;

    fn me() -> u32 {
        std::process::id()
    }

    fn sleeper(dir: &Path) -> Child {
        Command::new("/bin/sleep")
            .arg("60")
            .current_dir(dir)
            .spawn()
            .unwrap()
    }

    fn epoch_micros() -> i64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_micros() as i64
    }

    #[test]
    fn row_of_self_has_parent_and_path() {
        let row = MacOs.row(me()).unwrap();
        assert_eq!(row.pid, me());
        assert_eq!(row.ppid, std::os::unix::process::parent_id());
        let exe = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        assert_eq!(row.path.as_deref(), exe.to_str());
        assert_eq!(MacOs.row(u32::MAX), None);
    }

    #[test]
    fn start_is_epoch_micros_and_stable() {
        let before = epoch_micros();
        let mut child = sleeper(Path::new("/"));
        let after = epoch_micros();
        let first = MacOs.row(child.id()).map(|r| r.start);
        let again = MacOs.read_start(child.id());
        child.kill().unwrap();
        child.wait().unwrap();
        let start = first.unwrap();
        // Same clock, so the fork lands between the two reads, give or take a tick.
        assert!(
            before - 1_000 <= start && start <= after + 1_000,
            "{before} {start} {after}"
        );
        assert_eq!(again, Some(start));
        assert_eq!(MacOs.read_start(child.id()), None);
    }

    #[test]
    fn argv_of_self_matches_env_args() {
        let argv = MacOs.row(me()).unwrap().argv.unwrap();
        assert_eq!(argv, std::env::args().collect::<Vec<_>>());
    }

    #[test]
    fn cwd_of_self() {
        let here = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
        assert_eq!(MacOs.cwd(me()).as_deref(), here.to_str());
        let dir = TempDir::new();
        let mut child = sleeper(dir.path());
        let cwd = MacOs.cwd(child.id());
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(cwd.as_deref(), dir.path().to_str());
    }

    #[test]
    fn table_lists_self_and_parent() {
        let table = MacOs.table().unwrap();
        let mine = table.iter().find(|r| r.pid == me()).unwrap();
        assert_eq!(Some(mine), MacOs.row(me()).as_ref());
        let parent = std::os::unix::process::parent_id();
        assert!(table.iter().any(|r| r.pid == parent));
        assert!(table.len() > 2);
    }

    // The real backend without its 100 ms naps, so 50 probes of a live child take no time.
    struct Quick;

    impl ProcSource for Quick {
        type H = Handle;
        fn read_start(&self, pid: u32) -> Option<i64> {
            MacOs.read_start(pid)
        }
        fn open_handle(&self, pid: u32) -> Option<Handle> {
            MacOs.open_handle(pid)
        }
        fn send_kill(&self, handle: &Handle) -> bool {
            MacOs.send_kill(handle)
        }
        fn wait_exit(&self, handle: &Handle, ms: u32) -> bool {
            MacOs.wait_exit(handle, ms)
        }
        fn alive(&self, handle: &Handle) -> bool {
            MacOs.alive(handle)
        }
        fn probe(&self, pid: u32) -> i32 {
            MacOs.probe(pid)
        }
        fn sleep(&self, _: u64) {}
    }

    #[test]
    fn kill_without_pidfd_sends_nothing_and_reports_gone_only_on_esrch() {
        let mut child = sleeper(Path::new("/"));
        let opened = seam::open_with(&Quick, child.id()).unwrap();
        assert!(opened.handle().is_none());
        assert_eq!(Some(opened.start), MacOs.read_start(child.id()));
        // Alive after all 50 probes: nothing was sent, and alive is not gone.
        assert!(!seam::kill_with(&Quick, opened));
        assert_eq!(child.try_wait().unwrap(), None);

        let opened = seam::open_with(&Quick, child.id()).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(seam::kill_with(&Quick, opened));
        // pid 1 under another user answers EPERM: not proof of an exit.
        // SAFETY: getuid has no preconditions.
        if unsafe { libc::getuid() } != 0 {
            assert!(!seam::kill_with(&Quick, Opened::new(1, 0, None)));
        }
    }

    #[test]
    fn same_start_exact() {
        assert!(MacOs.same_start(1_793_318_401_234_567, 1_793_318_401_234_567));
        assert!(!MacOs.same_start(1_793_318_401_234_567, 1_793_318_401_234_560));
        assert!(!MacOs.same_start(1009, 1000));
    }
}
