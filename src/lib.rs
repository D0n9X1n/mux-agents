//! mux-agent: one verb per call, one JSON object (or -Text) on stdout, exit 0 ok, 1 error, 2 refused or usage.

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
compile_error!("mux-agent runs on Windows, Linux and macOS only");

pub mod ctx;
pub mod error;
pub mod json;
pub mod mux;
pub mod proc;
#[cfg(test)]
pub(crate) mod testing;
