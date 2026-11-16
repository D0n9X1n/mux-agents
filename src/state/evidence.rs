//! The one reader and the one create-once writer of generation evidence and markers (#103 state-0 S6).
use std::io;
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

/// Point 8: opens only what `symlink_metadata` (which never follows a link) reports as a regular file. Anything else
/// at the final name (a FIFO, a socket, a folder, any link, dangling or not) is `Unreadable`, unopened. A missing
/// final name keeps the ancestor rule (`absent`).
pub fn read(path: &Path) -> Evidence {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => {}
        Ok(_) => return Evidence::Unreadable,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return if absent(path, |d| std::fs::metadata(d).map(|m| m.is_dir())) {
                Evidence::Absent
            } else {
                Evidence::Unreadable
            };
        }
        Err(_) => return Evidence::Unreadable,
    }
    match crate::state::fs::read_json(path) {
        Ok(Some(v)) => Evidence::Present(v),
        Ok(None) | Err(_) => Evidence::Unreadable,
    }
}

/// Whether `read_json`'s "no such file" at `path` is a genuine absence: walking up from its parent, the first ancestor
/// that exists is a folder. A file in a folder's place (Unix `NotADirectory`, Windows `NotFound`) is not.
fn absent(path: &Path, is_dir: impl Fn(&Path) -> io::Result<bool>) -> bool {
    let mut at = path.parent();
    while let Some(d) = at.filter(|d| !d.as_os_str().is_empty()) {
        match is_dir(d) {
            Ok(dir) => return dir,
            Err(e) if e.kind() == io::ErrorKind::NotFound => at = d.parent(),
            Err(_) => return false,
        }
    }
    true
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

/// A crash test kills the process right after `step` (launch-1 crash guarantee): exit 86, no unwinding, no Drop.
/// Only in test builds and under the test-only `live` feature (b-call 39); release builds use no features.
pub fn crash_point(step: &str) {
    #[cfg(any(test, feature = "live"))]
    if std::env::var("MUX_AGENTS_CRASH").as_deref() == Ok(step) {
        use std::io::Write;
        let _ = std::io::stdout().flush();
        std::process::exit(86);
    }
    #[cfg(not(any(test, feature = "live")))]
    let _ = step;
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

    // Point 8: only a regular file is opened. A folder reads Unreadable unopened; a missing name keeps the ancestor
    // rule; a file reads Present.
    #[test]
    fn only_a_regular_file_is_opened() {
        let d = crate::testing::TempDir::new();
        let (dir, file) = (d.path().join("dir.json"), d.path().join("real.json"));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(&file, br#"{"a":1}"#).unwrap();
        let gone = d.path().join("gone.json");
        let (got, reads) =
            crate::state::fs::record_reads(|| (read(&dir), read(&gone), read(&file)));
        assert_eq!(
            got,
            (
                Evidence::Unreadable,
                Evidence::Absent,
                Evidence::Present(json!({"a": 1}))
            )
        );
        assert_eq!(reads, [file], "only the regular file is opened");
    }

    // Point 8: a link, dangling or not, and a FIFO read Unreadable and are never opened. Bounded: an opened FIFO would
    // block, so the reads run on a thread with a 5 s limit and the test fails instead of hanging. Unix only.
    #[cfg(unix)]
    #[test]
    fn a_link_or_a_fifo_reads_unreadable_unopened() {
        let d = crate::testing::TempDir::new();
        let file = d.path().join("real.json");
        std::fs::write(&file, br#"{"a":1}"#).unwrap();
        let (dangling, live, fifo) = (
            d.path().join("d.json"),
            d.path().join("l.json"),
            d.path().join("f.json"),
        );
        std::os::unix::fs::symlink(d.path().join("gone.json"), &dangling).unwrap();
        std::os::unix::fs::symlink(&file, &live).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::state::fs::record_reads(|| {
                [read(&dangling), read(&live), read(&fifo)]
            }));
        });
        let (got, reads) = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("a read blocked: the FIFO was opened");
        assert_eq!(
            got,
            [
                Evidence::Unreadable,
                Evidence::Unreadable,
                Evidence::Unreadable
            ]
        );
        assert!(reads.is_empty(), "opened {reads:?}");
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

    // S6: a file where a folder belongs is Unreadable on every OS; only a missing folder or file is Absent.
    #[test]
    fn a_file_where_a_folder_belongs_is_unreadable() {
        let d = crate::testing::TempDir::new();
        std::fs::write(d.path().join("gens"), b"").unwrap();
        assert_eq!(
            read(&d.path().join("gens/commit.json")),
            Evidence::Unreadable
        );
        assert_eq!(
            read(&d.path().join("gens/0/commit.json")),
            Evidence::Unreadable
        );
        assert_eq!(read(&d.path().join("none/0/commit.json")), Evidence::Absent);
    }

    // The two error shapes a path through a file gives: Unix fails `NotADirectory` at once; Windows fails `NotFound`
    // up to the file, which is no folder.
    #[test]
    fn absence_reads_the_unix_and_windows_error_shapes() {
        use std::io::{Error, ErrorKind};
        let p = Path::new("m/gens/0/x/commit.json");
        let file = Path::new("m/gens/0");
        let shape = |kind: ErrorKind| {
            move |d: &Path| {
                if d == file {
                    Ok(false)
                } else if d.starts_with(file) {
                    Err(Error::from(kind))
                } else {
                    Ok(true)
                }
            }
        };
        assert!(!absent(p, shape(ErrorKind::NotADirectory)), "unix");
        assert!(!absent(p, shape(ErrorKind::NotFound)), "windows");
        let missing = |d: &Path| {
            if d.starts_with(file) {
                Err(Error::from(ErrorKind::NotFound))
            } else {
                Ok(true)
            }
        };
        assert!(absent(p, missing), "a missing folder under a folder");
        let denied = |d: &Path| {
            if d.starts_with(file) {
                Err(Error::from(ErrorKind::PermissionDenied))
            } else {
                Ok(true)
            }
        };
        assert!(!absent(p, denied), "an unreadable folder");
    }
}
