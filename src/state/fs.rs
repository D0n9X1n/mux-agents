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

/// Makes `path` hold `value` only if nothing is there yet: true when this call made it, false when it already
/// existed. The JSON is written whole beside `path`, then hard-linked to it, so `path` appears with its content or
/// not at all, and of two racers exactly one makes it (#97: spawn's commit against the launcher's give-up).
pub fn claim_json(path: &Path, value: &Value) -> Res<bool> {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{}.tmp", crate::util::guid()?.replace('-', "")));
    let tmp = PathBuf::from(name);
    // The content is on disk before the name appears, so a claim seen after a crash is never empty.
    write_new(&tmp, value.to_string().as_bytes())
        .and_then(|()| OpenOptions::new().write(true).open(&tmp)?.sync_data())?;
    let linked = std::fs::hard_link(&tmp, path);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Flushes `dir`'s entries to disk, so an entry made in it survives a crash or power loss. Windows opens the folder
/// with the right to add a subfolder (FILE_APPEND_DATA) and backup semantics: the least access FlushFileBuffers
/// takes, and the right a folder made in it needed; a read-only open cannot flush there.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_APPEND_DATA: u32 = 0x0004;
        OpenOptions::new()
            .access_mode(FILE_APPEND_DATA)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(dir)?
            .sync_all()
    }
    #[cfg(not(windows))]
    {
        File::open(dir)?.sync_all()
    }
}

/// Flushes a folder above the state root, which may not be this user's. A folder whose flush is refused is skipped
/// only when this user cannot add an entry to it (`C:\Users`, `/home`, a 0100 folder): then no folder on the user's
/// path can have been made in it by this user. Any other failure is an error, so an entry this user may have made is
/// never left unflushed (#103). This assumes the permissions of the folders above the state root do not change
/// while a launch runs: a folder whose rights are taken away after the user made an entry in it can no longer be
/// opened, by this or any program, and cannot be flushed.
pub fn sync_ancestor(dir: &Path) -> io::Result<()> {
    match sync_dir(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied && !may_add_entry(dir) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Whether this user may add an entry to `dir`, after sync_dir's open was refused; true when unsure. Windows: a
/// refused open does not prove a refused create (an ACL can deny only SYNCHRONIZE), so a probe folder is made and
/// removed: only a create refused for permission answers no. Unix: write and search permission, as the kernel
/// checks them for this user.
fn may_add_entry(dir: &Path) -> bool {
    #[cfg(windows)]
    {
        let Ok(guid) = crate::util::guid() else {
            return true;
        };
        let probe = dir.join(format!(".mux-agents-probe-{guid}"));
        match std::fs::create_dir(&probe) {
            // Made, so the user can add here. Removed again; if it cannot be, the refused flush is still the error
            // the launch reports, and the probe's name says what it is.
            Ok(()) => {
                let _ = std::fs::remove_dir(&probe);
                true
            }
            Err(e) => e.kind() != io::ErrorKind::PermissionDenied,
        }
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
            return true;
        };
        // SAFETY: `c` is a NUL-terminated path that outlives the call.
        if unsafe { libc::access(c.as_ptr(), libc::W_OK | libc::X_OK) } == 0 {
            return true;
        }
        !refused(io::Error::last_os_error().raw_os_error())
    }
}

/// Whether access(2) failed because this user may not add an entry: permission (EACCES, EPERM) or a read-only file
/// system (EROFS). Any other failure (EIO, ENOMEM, ...) proves nothing, so the refused flush stays an error.
#[cfg(not(windows))]
fn refused(errno: Option<i32>) -> bool {
    matches!(errno, Some(libc::EACCES | libc::EPERM | libc::EROFS))
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
        reason: None,
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
    fn claim_json_makes_a_file_once_whole_and_leaves_no_tmp() {
        let dir = TempDir::new();
        let path = dir.path().join("commit.json");
        assert!(claim_json(&path, &serde_json::json!({"a": 1})).unwrap());
        assert!(!claim_json(&path, &serde_json::json!({"b": 2})).unwrap());
        assert_eq!(
            read_json(&path).unwrap().unwrap(),
            serde_json::json!({"a": 1})
        );
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["commit.json"]);
        // Many racers: exactly one makes it, and the file is that one's whole JSON.
        let path = dir.path().join("race.json");
        let made: Vec<(usize, bool)> = std::thread::scope(|scope| {
            let racers: Vec<_> = (0..16)
                .map(|n| {
                    let path = &path;
                    scope
                        .spawn(move || (n, claim_json(path, &serde_json::json!({"n": n})).unwrap()))
                })
                .collect();
            racers.into_iter().map(|r| r.join().unwrap()).collect()
        });
        let winners: Vec<usize> = made.iter().filter(|m| m.1).map(|m| m.0).collect();
        assert_eq!(winners.len(), 1, "{made:?}");
        assert_eq!(
            read_json(&path).unwrap().unwrap(),
            serde_json::json!({"n": winners[0]})
        );
    }

    #[test]
    fn sync_dir_flushes_a_folder_and_refuses_a_missing_one() {
        let dir = TempDir::new();
        assert!(claim_json(&dir.path().join("c.json"), &json!(1)).unwrap());
        sync_dir(dir.path()).unwrap();
        assert!(sync_dir(&dir.path().join("missing")).is_err());
        assert!(sync_ancestor(&dir.path().join("missing")).is_err());
        sync_ancestor(dir.path()).unwrap();
        // Every folder above a user's own one either flushes or is one the user could not add to.
        for above in dir.path().ancestors() {
            sync_ancestor(above).unwrap_or_else(|e| panic!("{}: {e}", above.display()));
        }
    }

    // #103 (review of 67de719): on Windows an ACL that denies only SYNCHRONIZE refuses the flush open but still lets
    // the user make a folder there; that folder is an error, never skipped.
    #[cfg(windows)]
    #[test]
    fn sync_ancestor_refuses_a_folder_the_user_can_add_to_but_not_flush() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
            SetNamedSecurityInfoW,
        };
        use windows_sys::Win32::Security::{
            DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        };
        use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
        let wide = |s: &std::ffi::OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
        let dir = TempDir::new();
        let odd = dir.path().join("nosync");
        let name = wide(odd.as_os_str());
        // Deny SYNCHRONIZE to everyone, allow everything else; children inherit only the allow.
        let sddl = wide("D:P(D;;0x00100000;;;WD)(A;OICI;FA;;;WD)".as_ref());
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated; `sd` receives a LocalAlloc descriptor, freed below.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(ok, 0);
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        };
        // SAFETY: `name` is NUL-terminated and `attributes` points at a live descriptor.
        let made = unsafe { CreateDirectoryW(name.as_ptr(), &attributes) };
        // SAFETY: `sd` came from ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(sd) };
        assert_ne!(made, 0);
        let flush = sync_dir(&odd);
        let ancestor = sync_ancestor(&odd);
        let child = std::fs::create_dir(odd.join("child"));
        // A NULL DACL again, so the temp folder can be removed whatever the asserts say.
        // SAFETY: `name` is NUL-terminated; the null pointers mean no owner, group or SACL change and a NULL DACL.
        let reset = unsafe {
            SetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        assert_eq!(reset, 0);
        assert!(child.is_ok(), "{child:?}");
        if flush.is_ok() {
            // An elevated user (a CI runner) opens it anyway with backup semantics: the folder is flushed, which is
            // the safe outcome, and nothing is skipped.
            assert!(ancestor.is_ok(), "{ancestor:?}");
        } else {
            assert!(
                ancestor.is_err(),
                "a folder the user can add to was skipped"
            );
        }
        let left: Vec<_> = std::fs::read_dir(&odd)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, ["child"], "the probe folder was left behind");
    }

    // #103 (review of 5dc2ff5): only a permission or read-only refusal from access(2) means the user cannot add an
    // entry; an inconclusive failure keeps the flush error.
    #[cfg(unix)]
    #[test]
    fn only_a_permission_refusal_means_the_user_cannot_add() {
        for errno in [libc::EACCES, libc::EPERM, libc::EROFS] {
            assert!(refused(Some(errno)), "{errno}");
        }
        for errno in [
            libc::EIO,
            libc::ENOMEM,
            libc::ENOENT,
            libc::ELOOP,
            libc::ENAMETOOLONG,
        ] {
            assert!(!refused(Some(errno)), "{errno}");
        }
        assert!(!refused(None));
    }

    // #103: a folder the user can add to but not open for a flush (Unix mode 0300) is an error, never skipped; one
    // the user cannot add to (0100) is skipped.
    #[cfg(unix)]
    #[test]
    fn sync_ancestor_skips_only_folders_the_user_cannot_add_to() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid has no preconditions and cannot fail.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skip: root may add to any folder");
            return;
        }
        let dir = TempDir::new();
        let probe = dir.path().join("p");
        std::fs::create_dir(&probe).unwrap();
        let set =
            |mode| std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(mode)).unwrap();
        set(0o300);
        assert!(sync_ancestor(&probe).is_err());
        set(0o100);
        sync_ancestor(&probe).unwrap();
        set(0o500);
        sync_ancestor(&probe).unwrap();
        set(0o700);
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

    // The new bytes go to a new inode that replaces the name: a reader holding the old file still reads the old
    // bytes, and the old inode is never truncated or rewritten in place.
    #[cfg(unix)]
    #[test]
    fn write_json_replaces_never_truncates() {
        use std::io::{Seek, SeekFrom};
        use std::os::unix::fs::MetadataExt;
        let dir = TempDir::new();
        let path = dir.path().join("record.json");
        write_json(&path, &json!({"n": 1, "pad": "old old old"})).unwrap();
        let mut reader = File::open(&path).unwrap();
        let before = reader.metadata().unwrap().ino();
        write_json(&path, &json!({"n": 2})).unwrap();
        let mut old = String::new();
        reader.read_to_string(&mut old).unwrap();
        assert_eq!(old, r#"{"n":1,"pad":"old old old"}"#);
        reader.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(reader.metadata().unwrap().len(), old.len() as u64);
        assert_ne!(std::fs::metadata(&path).unwrap().ino(), before);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"n":2}"#);
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

    // Only a missing file or folder is `None`; a folder at the name or bytes that are not JSON are errors.
    #[test]
    fn read_json_folder_or_malformed_is_internal() {
        let dir = TempDir::new();
        std::fs::create_dir(dir.path().join("folder.json")).unwrap();
        let refused = read_json(&dir.path().join("folder.json")).unwrap_err();
        assert_eq!((refused.code, refused.check), (1, "internal"));
        for bad in [&b"{\"n\":"[..], b"", b"\xff{}", b"{} {}"] {
            let path = file(&dir, "bad.json", bad);
            let refused = read_json(&path).unwrap_err();
            assert_eq!(
                (refused.code, refused.check, refused.message.as_str()),
                (1, "internal", "unexpected error (state file is not JSON)"),
                "{bad:?}"
            );
        }
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

    // -TaskFile 16 KB, -BriefFile 1 MB, -BodyFile 64 KB: exactly the limit reads, one byte more refuses and
    // names the limit.
    #[test]
    fn read_text_limits_are_exact() {
        assert_eq!((TASK_MAX, BRIEF_MAX, BODY_MAX), (16_384, 1_048_576, 65_536));
        let dir = TempDir::new();
        for (max, name) in [
            (TASK_MAX, "16384"),
            (BRIEF_MAX, "1048576"),
            (BODY_MAX, "65536"),
        ] {
            let at = file(&dir, "at.md", &vec![b'a'; max as usize]);
            assert_eq!(read_text(&at, max).unwrap().len(), max as usize, "{max}");
            let over = file(&dir, "over.md", &vec![b'a'; max as usize + 1]);
            let refused = read_text(&over, max).unwrap_err();
            assert_eq!(
                (refused.code, refused.check, refused.message),
                (
                    2,
                    "usage",
                    format!("a task or brief file is larger than {name} bytes")
                )
            );
        }
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
