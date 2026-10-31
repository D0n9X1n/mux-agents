//! The process table seam; each OS supplies its own `Os`.

use std::io;

use serde_json::{Map, Value};

use crate::parse::Client;

#[cfg(target_os = "macos")]
pub mod macos;
pub mod seam;
#[cfg(unix)]
pub mod unix;

/// One process. `start` is the kernel's own unit (spec D1); `path` the real executable; `argv` none when unreadable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub pid: u32,
    pub ppid: u32,
    pub start: i64,
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

/// What the OS says about a sandbox around the caller (T15 fills it in).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Signals {}

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

#[cfg(not(target_os = "macos"))]
pub fn host() -> Box<dyn Os> {
    Box::new(Pending)
}

// Temporary by design: the Linux (T13) and Windows (T14) tables replace it. Any question leaves as 1 `internal`
// rather than act on a made-up answer.
#[cfg(not(target_os = "macos"))]
struct Pending;

#[cfg(not(target_os = "macos"))]
impl Os for Pending {
    fn row(&self, _: u32) -> Option<Row> {
        unimplemented!("no process table yet")
    }
    fn table(&self) -> io::Result<Vec<Row>> {
        unimplemented!("no process table yet")
    }
    fn open(&self, _: u32) -> Option<Opened> {
        unimplemented!("no process table yet")
    }
    fn kill(&self, _: Opened) -> bool {
        unimplemented!("no process table yet")
    }
    fn cwd(&self, _: u32) -> Option<String> {
        unimplemented!("no process table yet")
    }
    fn client_of(&self, _: &Row) -> Option<Client> {
        unimplemented!("no process table yet")
    }
    fn is_shell(&self, _: &Row) -> bool {
        unimplemented!("no process table yet")
    }
    fn same_start(&self, _: i64, _: i64) -> bool {
        unimplemented!("no process table yet")
    }
    fn stop_report(&self, _: &[Left]) -> Map<String, Value> {
        unimplemented!("no process table yet")
    }
    fn token(&self) -> Option<Token> {
        unimplemented!("no process table yet")
    }
    fn sandbox(&self) -> Signals {
        unimplemented!("no process table yet")
    }
}
