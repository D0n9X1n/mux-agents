//! State files: atomic JSON writes and bounded reads (MuxAgents.ps1:487-515, 708-722).

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{Refusal, Res, refuse};

/// A state file read refuses past this.
pub const STATE_MAX: u64 = 1 << 20;
/// -TaskFile, -BriefFile and -BodyFile limits.
pub const TASK_MAX: u64 = 16 << 10;
pub const BRIEF_MAX: u64 = 1 << 20;
pub const BODY_MAX: u64 = 64 << 10;

pub fn write_json(path: &Path, value: &Value) -> Res<()> {
    // std's rename replaces with POSIX semantics on Windows, so a reader that shares delete does not block it.
    write_json_with(path, value, |from, to| std::fs::rename(from, to))
}

/// The JSON goes to `<path>.<32 hex>.tmp` beside it, which then replaces or becomes `path`, so the file is never
/// partial and a failed replace keeps the old one. Callers hold the session lock; this does not lock.
pub fn write_json_with(
    path: &Path,
    value: &Value,
    rename: fn(&Path, &Path) -> io::Result<()>,
) -> Res<()> {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{}.tmp", crate::util::guid()?.replace('-', "")));
    let tmp = PathBuf::from(name);
    let written = write_new(&tmp, value.to_string().as_bytes()).and_then(|()| rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(written?)
}

fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)
}

/// The parsed file, or `None` when it or its folder is missing; over 1 MB is an error. Callers hold the
/// session lock. On Windows the read shares delete, so it never blocks a replace.
pub fn read_json(path: &Path) -> Res<Option<Value>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    share(&mut options, SHARE_READ | SHARE_DELETE);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e)
            if e.kind() == io::ErrorKind::NotFound || e.kind() == io::ErrorKind::NotADirectory =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    };
    let bytes = read_capped(file, STATE_MAX)?
        .ok_or_else(|| Refusal::internal("state file is larger than 1 MB"))?;
    let text = bytes.strip_prefix(BOM).unwrap_or(&bytes);
    serde_json::from_slice(text)
        .map(Some)
        .map_err(|_| Refusal::internal("state file is not JSON"))
}

/// A task, brief or body file as strict UTF-8 without its BOM; a missing, unreadable, oversized or non-UTF-8
/// file refuses 2 `usage`.
pub fn read_text(path: &Path, max: u64) -> Res<String> {
    let unreadable = || Refusal {
        code: 2,
        check: "usage",
        message: "a task or brief file cannot be read".into(),
    };
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    share(&mut options, SHARE_READ | SHARE_WRITE);
    let file = options.open(path).map_err(|_| unreadable())?;
    let Some(bytes) = read_capped(file, max).map_err(|_| unreadable())? else {
        return refuse(
            2,
            "usage",
            format!("a task or brief file is larger than {max} bytes"),
        );
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return refuse(2, "usage", "a task or brief file is not UTF-8");
    };
    Ok(match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_owned(),
        None => text,
    })
}

const BOM: &[u8] = b"\xef\xbb\xbf";

// `None` past `max`. Reads at most one byte more than `max`, so a file that grows while read stays bounded.
// A folder opens on Unix and fails here, as on Windows it fails at open.
fn read_capped(file: File, max: u64) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= max).then_some(bytes))
}

#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    FILE_SHARE_DELETE as SHARE_DELETE, FILE_SHARE_READ as SHARE_READ,
    FILE_SHARE_WRITE as SHARE_WRITE,
};

#[cfg(windows)]
fn share(options: &mut OpenOptions, mode: u32) {
    use std::os::windows::fs::OpenOptionsExt;
    options.share_mode(mode);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;
    use serde_json::json;

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn file(dir: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn write_json_is_atomic_and_leaves_no_tmp() {
        let dir = TempDir::new();
        let path = dir.path().join("record.json");
        write_json(&path, &json!({"n": 1})).unwrap();
        write_json(&path, &json!({"n": 2, "name": "alpha \u{23F5}"})).unwrap();
        // Plain UTF-8 compact JSON, not the stdout reply's ASCII escapes.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"n\":2,\"name\":\"alpha \u{23F5}\"}"
        );
        assert_eq!(names(dir.path()), ["record.json"]);
    }

    #[test]
    fn write_json_failed_replace_keeps_old_bytes() {
        let dir = TempDir::new();
        let path = dir.path().join("record.json");
        write_json(&path, &json!({"n": 1})).unwrap();
        let refused = write_json_with(&path, &json!({"n": 2}), |from, _| {
            // The tmp exists, named <file>.<32 hex>.tmp, when the replace is tried.
            let name = from.file_name().unwrap().to_str().unwrap();
            let hex = name
                .strip_prefix("record.json.")
                .and_then(|n| n.strip_suffix(".tmp"))
                .unwrap();
            assert!(hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(from.is_file());
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "SECRET"))
        })
        .unwrap_err();
        assert_eq!((refused.code, refused.check), (1, "internal"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"n":1}"#);
        assert_eq!(names(dir.path()), ["record.json"]);
    }

    #[test]
    fn read_json_missing_is_none() {
        let dir = TempDir::new();
        assert_eq!(read_json(&dir.path().join("none.json")).unwrap(), None);
        assert_eq!(read_json(&dir.path().join("gone/none.json")).unwrap(), None);
        let path = file(&dir, "r.json", "\u{feff}{\"name\":\"\u{e9}\"}".as_bytes());
        assert_eq!(read_json(&path).unwrap(), Some(json!({"name": "\u{e9}"})));
    }

    #[test]
    fn read_json_over_1mb_is_internal() {
        let dir = TempDir::new();
        let mut big = vec![b' '; STATE_MAX as usize + 1];
        big[0] = b'1';
        let path = file(&dir, "big.json", &big);
        let refused = read_json(&path).unwrap_err();
        assert_eq!((refused.code, refused.check), (1, "internal"));
        assert_eq!(
            refused.message,
            "unexpected error (state file is larger than 1 MB)"
        );
        // Exactly 1 MB still reads.
        big.pop();
        let path = file(&dir, "edge.json", &big);
        assert_eq!(read_json(&path).unwrap(), Some(json!(1)));
    }

    #[test]
    fn read_text_strips_bom() {
        let dir = TempDir::new();
        let path = file(&dir, "task.md", "\u{feff}\u{feff}do it\r\n".as_bytes());
        // One BOM only; the rest is data, line ends included.
        assert_eq!(read_text(&path, TASK_MAX).unwrap(), "\u{feff}do it\r\n");
        let path = file(&dir, "plain.md", b"plain");
        assert_eq!(read_text(&path, TASK_MAX).unwrap(), "plain");
    }

    #[test]
    fn read_text_non_utf8_is_usage_2() {
        let dir = TempDir::new();
        let path = file(&dir, "task.md", b"ok \xff\xfe bad");
        let refused = read_text(&path, TASK_MAX).unwrap_err();
        assert_eq!(
            (refused.code, refused.check, refused.message.as_str()),
            (2, "usage", "a task or brief file is not UTF-8")
        );
        for missing in [dir.path().join("none.md"), dir.path().to_path_buf()] {
            let refused = read_text(&missing, TASK_MAX).unwrap_err();
            assert_eq!(
                (refused.code, refused.check, refused.message.as_str()),
                (2, "usage", "a task or brief file cannot be read")
            );
        }
    }

    #[test]
    fn read_text_oversize_names_limit() {
        let dir = TempDir::new();
        let path = file(&dir, "task.md", &vec![b'a'; TASK_MAX as usize + 1]);
        let refused = read_text(&path, TASK_MAX).unwrap_err();
        assert_eq!(
            (refused.code, refused.check, refused.message.as_str()),
            (
                2,
                "usage",
                "a task or brief file is larger than 16384 bytes"
            )
        );
        let path = file(&dir, "edge.md", &vec![b'a'; TASK_MAX as usize]);
        assert_eq!(read_text(&path, TASK_MAX).unwrap().len(), TASK_MAX as usize);
        let refused = read_text(&path, 10).unwrap_err();
        assert_eq!(
            refused.message,
            "a task or brief file is larger than 10 bytes"
        );
    }

    #[cfg(windows)]
    mod windows {
        use super::*;
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Foundation::GENERIC_READ;
        use windows_sys::Win32::Storage::FileSystem::{
            DELETE, FILE_FLAG_DELETE_ON_CLOSE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        fn reader(path: &Path, share: u32) -> std::fs::File {
            OpenOptions::new()
                .read(true)
                .share_mode(share)
                .open(path)
                .unwrap()
        }

        #[test]
        fn write_json_open_handle_keeps_old() {
            let dir = TempDir::new();
            let path = dir.path().join("record.json");
            write_json(&path, &json!({"n": 1})).unwrap();
            let held = reader(&path, FILE_SHARE_READ);
            let refused = write_json(&path, &json!({"n": 2})).unwrap_err();
            drop(held);
            assert_eq!((refused.code, refused.check), (1, "internal"));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"n":1}"#);
            assert_eq!(names(dir.path()), ["record.json"]);
        }

        #[test]
        fn write_json_replaces_with_share_delete_reader() {
            let dir = TempDir::new();
            let path = dir.path().join("record.json");
            write_json(&path, &json!({"n": 1})).unwrap();
            let held = reader(&path, FILE_SHARE_READ | FILE_SHARE_DELETE);
            write_json(&path, &json!({"n": 2})).unwrap();
            assert_eq!(read_json(&path).unwrap(), Some(json!({"n": 2})));
            drop(held);
            assert_eq!(names(dir.path()), ["record.json"]);
        }

        // Tests.ps1:999: another writer's delete-access handle must not make a read fail.
        #[test]
        fn read_json_while_delete_access_handle_open() {
            let dir = TempDir::new();
            let path = dir.path().join("record.json");
            write_json(&path, &json!({"n": 2})).unwrap();
            let deleter = OpenOptions::new()
                .access_mode(GENERIC_READ | DELETE)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
                .open(&path)
                .unwrap();
            assert_eq!(read_json(&path).unwrap(), Some(json!({"n": 2})));
            drop(deleter);
            assert_eq!(read_json(&path).unwrap(), None);
            assert!(names(dir.path()).is_empty());
        }
    }
}
