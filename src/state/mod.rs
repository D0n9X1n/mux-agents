//! The state folder: its root, privacy checks, locks and the files inside it.

pub mod acl;
pub mod fs;
pub mod lock;
#[cfg(unix)]
pub mod private_unix;
pub mod private_windows;
pub mod root;
