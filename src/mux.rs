//! The multiplexer seam: `<mux> -S <socket> <argv>` with the socket and binary already resolved.

use crate::error::Res;

/// One finished multiplexer call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub trait Mux {
    fn run(&self, args: &[&str]) -> Res<Output>;
}
