//! The one reader and the one create-once writer of generation evidence and markers (#103 state-0 S6).
use std::path::Path;

use serde_json::Value;

use crate::error::{Refusal, Res};

/// What one read of an evidence file found.
#[derive(Debug, Clone, PartialEq)]
pub enum Evidence {
    Present(Value),
    /// No such file (or no such folder).
    Absent,
    /// An I/O error, a torn write or invalid JSON: never taken for `Absent`.
    Unreadable,
}

/// Whether `publish` made the file or found one there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Published {
    Published,
    Exists,
}

pub fn read(path: &Path) -> Evidence {
    match crate::state::fs::read_json(path) {
        Ok(Some(v)) => Evidence::Present(v),
        Ok(None) => Evidence::Absent,
        Err(_) => Evidence::Unreadable,
    }
}

/// The refusal a caller that cannot name its own gives for `Unreadable`.
pub fn unreadable() -> Refusal {
    Refusal {
        code: 1,
        check: "evidence",
        message: "state evidence cannot be read; retry".into(),
        reason: None,
        leftover: None,
        details: Default::default(),
    }
}

impl Evidence {
    /// The value, `None` for `Absent`, the `evidence` refusal for `Unreadable`.
    pub fn known(self) -> Res<Option<Value>> {
        match self {
            Evidence::Present(v) => Ok(Some(v)),
            Evidence::Absent => Ok(None),
            Evidence::Unreadable => Err(unreadable()),
        }
    }
}

/// Create-once: `path` appears whole with `value` or not at all; of two racers exactly one publishes.
pub fn publish(path: &Path, value: &Value) -> Res<Published> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(if crate::state::fs::claim_json(path, value)? {
        Published::Published
    } else {
        Published::Exists
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reader_answers_present_absent_and_unreadable() {
        let d = crate::testing::TempDir::new();
        let p = d.path().join("gens/1/commit.json");
        assert_eq!(read(&p), Evidence::Absent);
        assert_eq!(
            publish(&p, &json!({"committed": true})).unwrap(),
            Published::Published
        );
        assert_eq!(read(&p), Evidence::Present(json!({"committed": true})));
        std::fs::write(&p, b"{\"commi").unwrap();
        assert_eq!(read(&p), Evidence::Unreadable);
        let e = read(&p).known().unwrap_err();
        assert_eq!((e.code, e.check), (1, "evidence"));
        // An I/O error: the path is a folder.
        let dir = d.path().join("gens/1/ended.json");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read(&dir), Evidence::Unreadable);
    }

    #[test]
    fn racing_publishers_one_publishes_one_finds_it() {
        let d = crate::testing::TempDir::new();
        let p = std::sync::Arc::new(d.path().join("gens/2/commit.json"));
        let racers: Vec<_> = (0..2)
            .map(|i| {
                let p = p.clone();
                std::thread::spawn(move || publish(&p, &json!({"by": i})).unwrap())
            })
            .collect();
        let mut got: Vec<_> = racers.into_iter().map(|t| t.join().unwrap()).collect();
        got.sort_by_key(|g| *g == Published::Exists);
        assert_eq!(got, [Published::Published, Published::Exists]);
        assert!(matches!(read(&p), Evidence::Present(_)));
    }
}
