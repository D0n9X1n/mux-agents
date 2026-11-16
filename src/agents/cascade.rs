//! cascade-3d: a stop records `stopped` for a member's whole sub-tree, closes it, then ends it bottom-up; a journal
//! under `teams/<sha16(session)>.cascade/` lets the next stop, `up` or bar tick finish a cascade cut short.
use std::path::Path;

use serde_json::Value;

use super::gens;
use crate::error::{Refusal, Res};

/// state-0 S2: `desired.json` `{desired, at}`; absent reads `running` (C1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desired {
    Running,
    Stopped,
}

/// C1, R2, R5: through the walk from the state root. Absent: Running. A word other than the two: `internal`.
/// Unreadable (torn, a folder, a link on the way): `evidence`, never Running.
pub fn desired_of(member: &Path) -> Res<Desired> {
    match gens::walk(member, &["desired.json"]).known()? {
        None => Ok(Desired::Running),
        Some(v) => match v.get("desired").and_then(Value::as_str) {
            Some("running") => Ok(Desired::Running),
            Some("stopped") => Ok(Desired::Stopped),
            _ => Err(Refusal::internal("desired.json names no desired state")),
        },
    }
}

#[cfg(test)]
#[path = "cascade_tests.rs"]
mod tests;
