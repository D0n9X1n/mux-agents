//! The process table seam; each OS supplies its own `Os`.

use std::io;

use crate::parse::Client;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod seam;
#[cfg(unix)]
pub mod unix;
pub mod windows;

/// One process. `start` is the kernel's own unit (spec D1), none when the process cannot be opened (a Windows
/// Toolhelp row whose parent information remains available); path is the real executable; argv is optional.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub pid: u32,
    pub ppid: u32,
    pub start: Option<i64>,
    pub path: Option<String>,
    pub argv: Option<Vec<String>>,
    /// The name the OS lists without opening the process (Linux `comm`, macOS `proc_name`, Windows Toolhelp
    /// `szExeFile`); none when unreadable, and always none from Windows `Os::row`, which has no snapshot (H1).
    pub image: Option<String>,
}

#[cfg(unix)]
pub type Handle = std::os::fd::OwnedFd;
#[cfg(windows)]
pub type Handle = std::os::windows::io::OwnedHandle;

/// A process held for a kill: its start read after the handle opened, so both name the same process.
pub type Opened = seam::Opened<Handle>;

/// The caller's Windows token (T14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub app_container: bool,
    pub restricted: bool,
    pub integrity_rid: u32,
}

/// What the OS says about a sandbox around the caller; `sandbox::decide` judges it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signals {
    /// macOS: sandbox_check(getpid(), NULL, 0), non-zero inside a sandbox.
    SandboxCheck(i32),
    /// Linux: /proc/self/status, none when unreadable.
    Status(Option<String>),
    /// Windows: the caller's token, none when unreadable.
    Token(Option<Token>),
}

/// Which boot a process start belongs to (r2b call 27). Linux starts are clock ticks since boot, so one (pid, start)
/// can come back in a later boot; macOS (µs since the epoch) and Windows (FILETIME) starts are absolute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Boot {
    /// Starts are absolute: a (pid, start) never repeats across boots, and no boot is recorded.
    Absolute,
    /// Starts are boot-relative; this boot's identity (Linux: /proc/sys/kernel/random/boot_id).
    Id(String),
    /// Starts are boot-relative and this boot's identity cannot be read.
    Unreadable,
}

pub trait Os {
    fn row(&self, pid: u32) -> Option<Row>;
    fn table(&self) -> io::Result<Vec<Row>>;
    fn open(&self, pid: u32) -> Option<Opened>;
    /// True only when the opened process is shown to have exited.
    fn kill(&self, opened: Opened) -> bool;
    fn cwd(&self, pid: u32) -> Option<String>;
    fn client_of(&self, row: &Row) -> Option<Client>;
    /// o3 call 34 N2: `client_of` with "unknown" (None: its identity cannot be resolved) told apart from "no client"
    /// (Some(None)).
    fn client_known(&self, row: &Row) -> Option<Option<Client>> {
        Some(self.client_of(row))
    }
    fn is_shell(&self, row: &Row) -> bool;
    fn same_start(&self, opened: i64, listed: i64) -> bool;
    fn token(&self) -> Option<Token>;
    fn sandbox(&self) -> Signals;
    fn boot(&self) -> Boot;
    /// Linux: `/proc/self/mountinfo` (`Some(Err)` when it cannot be read); None where there is no /proc.
    fn proc_mounts(&self) -> Option<io::Result<String>> {
        None
    }
    /// True only when the OS shows `row`'s process (same pid and start) has ended: a zombie its parent has not reaped
    /// yet, or a process already in its exit. Such a process runs no more code, but until it is reaped it is still
    /// listed, with its argv and path gone. False when that cannot be read, and where an exited process leaves the table.
    fn ended(&self, _row: &Row) -> bool {
        false
    }
}

#[cfg(target_os = "macos")]
pub fn host() -> Box<dyn Os> {
    Box::new(macos::MacOs)
}

#[cfg(target_os = "linux")]
pub fn host() -> Box<dyn Os> {
    Box::new(linux::Linux)
}

#[cfg(windows)]
pub fn host() -> Box<dyn Os> {
    Box::new(windows::Windows)
}
