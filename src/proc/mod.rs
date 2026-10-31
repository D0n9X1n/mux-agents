//! The process table seam; each OS supplies its own `Os`.

use std::io;

use serde_json::{Map, Value};

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
/// Toolhelp row kept so stop's walk reaches its children); `path` the real executable; `argv` none when unreadable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub pid: u32,
    pub ppid: u32,
    pub start: Option<i64>,
    pub path: Option<String>,
    pub argv: Option<Vec<String>>,
}

#[cfg(unix)]
pub type Handle = std::os::fd::OwnedFd;
#[cfg(windows)]
pub type Handle = std::os::windows::io::OwnedHandle;

/// A process held for a kill: its start read after the handle opened, so both name the same process.
pub type Opened = seam::Opened<Handle>;

/// Why stop left a tree row alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Open,
    Start,
    Kill,
    /// The pane's process is not the launcher launcher.json recorded, so its tree was never opened or killed.
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Left {
    pub pid: u32,
    pub reason: Reason,
}

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

pub trait Os {
    fn row(&self, pid: u32) -> Option<Row>;
    fn table(&self) -> io::Result<Vec<Row>>;
    fn open(&self, pid: u32) -> Option<Opened>;
    /// True only when the opened process is shown to have exited.
    fn kill(&self, opened: Opened) -> bool;
    fn cwd(&self, pid: u32) -> Option<String>;
    fn client_of(&self, row: &Row) -> Option<Client>;
    fn is_shell(&self, row: &Row) -> bool;
    fn same_start(&self, opened: i64, listed: i64) -> bool;
    fn stop_report(&self, left: &[Left]) -> Map<String, Value>;
    fn token(&self) -> Option<Token>;
    fn sandbox(&self) -> Signals;
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
