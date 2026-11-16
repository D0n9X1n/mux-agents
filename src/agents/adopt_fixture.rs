//! adopt-3e fixtures over cascade's (`cx`) and group B's (`lf`). Each helper arrives with its first user.
use serde_json::{Value, json};

use super::adopt::{Change, Journal};
use super::cascade_fixture::{A, B};
use super::life_fixture as lf;

pub const E: &str = "eeeeeeeeeee1";

/// The journal that moves B from E under A at depth 2.
pub fn to_a(seq: u64) -> Journal {
    let parent_agent =
        json!({"client": "claude", "pid": null, "start": null, "sessionId": lf::SID, "id": A});
    Journal {
        seq,
        from: E.into(),
        to: A.into(),
        changes: vec![Change {
            id: B.into(),
            parent_agent,
            parent_pane: Value::Null,
            depth: 2,
        }],
    }
}
