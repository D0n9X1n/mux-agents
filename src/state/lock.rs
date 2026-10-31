//! Per-user locks (D2): the keys serialize across state roots. Unix uses `File::try_lock` on a private lock
//! file; Windows keeps the named mutex.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ctx::Ctx;
use crate::error::{Res, refuse};

pub const LOCK_WAIT: Duration = Duration::from_secs(15);

fn key_hex(key: &str) -> String {
    crate::util::sha256_hex(key)[..32].to_owned()
}

/// `<root>/<32 hex of sha256(key)>.lock`.
pub fn lock_path(root: &Path, key: &str) -> PathBuf {
    root.join(format!("{}.lock", key_hex(key)))
}

/// `Local\MuxAgents-<32 hex of sha256(key)>`, as the PowerShell entry named it.
pub fn mutex_name(key: &str) -> String {
    format!(r"Local\MuxAgents-{}", key_hex(key))
}

// MUX_AGENTS_TEST_PANIC=lock panics with the lock held, so tests see unwinding release it.
fn test_panic(ctx: &Ctx) {
    #[cfg(feature = "test-panic")]
    if ctx.env.get("MUX_AGENTS_TEST_PANIC") == Some("lock") {
        panic!("PANIC-PAYLOAD-lock");
    }
    let _ = ctx;
}

fn timed_out<T>() -> Res<T> {
    refuse(2, "lock", "timed out waiting for the agent lock")
}

/// `<home of the effective uid>/.local/state/mux-agents/locks`. The home comes from the passwd entry, never
/// from HOME, XDG_STATE_HOME or MUX_AGENTS_STATE, so every state root of one user shares the locks.
#[cfg(unix)]
pub fn lock_root() -> Res<PathBuf> {
    let home = passwd_home()?;
    let Some(home) = home.to_str().filter(|h| h.starts_with('/')) else {
        return refuse(1, "state", "the home folder of the user cannot be read");
    };
    crate::state::root::real_prefix(&format!(
        "{}/.local/state/mux-agents/locks",
        home.trim_end_matches('/')
    ))
}

#[cfg(unix)]
fn passwd_home() -> Res<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;
    let unreadable = || refuse(1, "state", "the home folder of the user cannot be read");
    let mut buf = vec![0 as libc::c_char; 1024];
    loop {
        // SAFETY: an all-zero passwd is a valid out value; the pointers are filled by getpwuid_r.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found = std::ptr::null_mut();
        // SAFETY: `pwd`, `buf` (with its length) and `found` are valid for the call.
        let rc = unsafe {
            libc::getpwuid_r(
                libc::geteuid(),
                &mut pwd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut found,
            )
        };
        if rc == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 4, 0);
            continue;
        }
        if rc != 0 || found.is_null() || pwd.pw_dir.is_null() {
            return unreadable();
        }
        // SAFETY: pw_dir points at a NUL-terminated string inside `buf`, which is still live.
        let dir = unsafe { CStr::from_ptr(pwd.pw_dir) };
        return Ok(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())));
    }
}

/// Runs `f` holding the lock for `key`; waiting longer than 15 s refuses 2 `lock`. Callers take the session
/// lock before the mailbox lock.
#[cfg(unix)]
pub fn with_lock<T>(ctx: &Ctx, key: &str, f: impl FnOnce() -> Res<T>) -> Res<T> {
    with_lock_in(ctx, &lock_root()?, key, LOCK_WAIT, f)
}

/// `with_lock` under an explicit lock root, so tests never touch the real one.
#[cfg(unix)]
pub(crate) fn with_lock_in<T>(
    ctx: &Ctx,
    root: &Path,
    key: &str,
    wait: Duration,
    f: impl FnOnce() -> Res<T>,
) -> Res<T> {
    use std::fs::TryLockError;
    crate::state::private_unix::private_dirs(root, &[root])?;
    let file = open_lock(&lock_path(root, key))?;
    let mut waited = Duration::ZERO;
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if waited < wait => {
                ctx.clock.sleep(BACK_OFF);
                waited += BACK_OFF;
            }
            Err(TryLockError::WouldBlock) => return timed_out(),
            Err(TryLockError::Error(e)) => return Err(e.into()),
        }
    }
    // Closing `file` releases the lock, on return and while unwinding alike.
    test_panic(ctx);
    f()
}

#[cfg(unix)]
const BACK_OFF: Duration = Duration::from_millis(10);

// O_NOFOLLOW: a link at the name is refused, never followed. The file is never removed, so its name stays
// bound to one inode that every holder locks.
#[cfg(unix)]
fn open_lock(path: &Path) -> Res<std::fs::File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let not_private = || refuse(1, "state", "the lock file is not private");
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path);
    let file = match opened {
        Ok(file) => file,
        Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::EISDIR)) => {
            return not_private();
        }
        Err(e) => return Err(e.into()),
    };
    let meta = file.metadata()?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    if !meta.is_file() || meta.uid() != uid || meta.nlink() != 1 || meta.mode() & 0o7777 != 0o600 {
        return not_private();
    }
    Ok(file)
}

/// Runs `f` holding the named mutex for `key`; an abandoned mutex counts as held; waiting longer than 15 s
/// refuses 2 `lock`. Callers take the session lock before the mailbox lock.
#[cfg(windows)]
pub fn with_lock<T>(ctx: &Ctx, key: &str, f: impl FnOnce() -> Res<T>) -> Res<T> {
    with_lock_timeout(ctx, key, LOCK_WAIT, f)
}

#[cfg(windows)]
pub(crate) fn with_lock_timeout<T>(
    ctx: &Ctx,
    key: &str,
    wait: Duration,
    f: impl FnOnce() -> Res<T>,
) -> Res<T> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: an open mutex handle, closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
    struct Held<'a>(&'a Handle);
    impl Drop for Held<'_> {
        fn drop(&mut self) {
            // SAFETY: this thread owns the mutex; released once, before its handle closes.
            unsafe { ReleaseMutex(self.0.0) };
        }
    }
    let name: Vec<u16> = mutex_name(key).encode_utf16().chain([0]).collect();
    // SAFETY: `name` is NUL-terminated; default security, not initially owned.
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if mutex.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let mutex = Handle(mutex);
    // Below INFINITE (u32::MAX), so a long wait never becomes endless.
    let ms = wait.as_millis().min(u128::from(u32::MAX - 1)) as u32;
    // SAFETY: a live mutex handle.
    match unsafe { WaitForSingleObject(mutex.0, ms) } {
        WAIT_OBJECT_0 | WAIT_ABANDONED => {}
        WAIT_TIMEOUT => return timed_out(),
        _ => return Err(std::io::Error::last_os_error().into()),
    }
    let _held = Held(&mutex);
    test_panic(ctx);
    f()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_path_is_32_hex_sha256_of_key() {
        // sha256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a3...
        assert_eq!(
            lock_path(Path::new("/r/locks"), "abc"),
            PathBuf::from("/r/locks/ba7816bf8f01cfea414140de5dae2223.lock")
        );
    }

    #[test]
    fn windows_mutex_name_is_local_muxagents_32hex() {
        assert_eq!(
            mutex_name("abc"),
            r"Local\MuxAgents-ba7816bf8f01cfea414140de5dae2223"
        );
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        use crate::ctx::Env;
        use crate::error::{Refusal, refuse};
        use crate::testing::{Fakes, TempDir};
        use std::cell::Cell;
        use std::fs::File;
        use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
        use std::panic::{AssertUnwindSafe, catch_unwind};

        fn refusal<T: std::fmt::Debug>(result: Res<T>) -> (u8, &'static str, String) {
            let Refusal {
                code,
                check,
                message,
            } = result.unwrap_err();
            (code, check, message)
        }

        fn state(message: &str) -> (u8, &'static str, String) {
            (1, "state", message.into())
        }

        #[test]
        fn lock_root_ignores_home_xdg_and_state() {
            let fakes = Fakes::default();
            let mut ctx = fakes.ctx();
            ctx.env = Env::from_pairs([
                ("HOME", "/tmp/x"),
                ("XDG_STATE_HOME", "/tmp/x/xdg"),
                ("MUX_AGENTS_STATE", "/tmp/x/state"),
            ]);
            ctx.home = "/tmp/x".into();
            // The passwd entry of the effective uid, read here by the non-reentrant call.
            // SAFETY: getpwuid's result is read at once, before any other passwd call on this thread.
            let home = unsafe {
                let pw = libc::getpwuid(libc::geteuid());
                assert!(!pw.is_null());
                std::ffi::CStr::from_ptr((*pw).pw_dir)
                    .to_str()
                    .unwrap()
                    .to_owned()
            };
            let want = crate::state::root::real_prefix(&format!(
                "{}/.local/state/mux-agents/locks",
                home.trim_end_matches('/')
            ))
            .unwrap();
            // Only computed: nothing is created under the real home.
            let got = lock_root().unwrap();
            assert_eq!(got, want);
            assert!(got.ends_with(".local/state/mux-agents/locks"));
            assert!(!got.starts_with("/tmp/x"));
        }

        #[test]
        fn timeout_refuses_lock_2() {
            let dir = TempDir::new();
            let root = dir.path().join("locks");
            let fakes = Fakes::default();
            let ctx = fakes.ctx();
            assert_eq!(with_lock_in(&ctx, &root, "k", LOCK_WAIT, || Ok(7)), Ok(7));
            let path = lock_path(&root, "k");
            let meta = std::fs::symlink_metadata(&path).unwrap();
            assert_eq!((meta.mode() & 0o7777, meta.nlink()), (0o600, 1));
            assert_eq!(
                std::fs::symlink_metadata(&root).unwrap().mode() & 0o7777,
                0o700
            );

            let holder = File::open(&path).unwrap();
            holder.lock().unwrap();
            let ran = Cell::new(false);
            let wait = Duration::from_millis(200);
            let refused = with_lock_in(&ctx, &root, "k", wait, || {
                ran.set(true);
                Ok(())
            });
            assert_eq!(
                refusal(refused),
                (2, "lock", "timed out waiting for the agent lock".into())
            );
            assert!(!ran.get());
            // 10 ms back-off, on the ctx clock.
            assert_eq!(fakes.slept(), [10; 20]);
            // Another key is another file.
            assert_eq!(with_lock_in(&ctx, &root, "other", wait, || Ok(1)), Ok(1));
            drop(holder);
            // A child forked by another test thread (a Command spawn) holds a copy of the holder's open file
            // description, and so its flock, from fork until exec closes it (O_CLOEXEC acts only at exec).
            // The lock is then released a moment after the drop, so the re-lock retries in real time; the
            // ctx clock's sleep does not wait.
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let relocked = loop {
                let got = with_lock_in(&ctx, &root, "k", wait, || Ok(2));
                if got.is_ok() || std::time::Instant::now() >= deadline {
                    break got;
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            assert_eq!(relocked, Ok(2));
            // The file stays.
            assert!(path.is_file());
        }

        #[test]
        fn released_after_panic() {
            let dir = TempDir::new();
            let root = dir.path().join("locks");
            let ctx = Fakes::default().ctx();
            let wait = Duration::from_millis(50);
            let panicked = catch_unwind(AssertUnwindSafe(|| {
                with_lock_in(&ctx, &root, "k", wait, || -> Res<()> { panic!("inside") })
            }));
            assert!(panicked.is_err());
            let probe = File::open(lock_path(&root, "k")).unwrap();
            probe.try_lock().unwrap();
            drop(probe);
            assert_eq!(with_lock_in(&ctx, &root, "k", wait, || Ok(())), Ok(()));
            // A refusal inside the guard releases it too.
            let refused = with_lock_in(&ctx, &root, "k", wait, || -> Res<()> {
                refuse(2, "name", "taken")
            });
            assert_eq!(refusal(refused), (2, "name", "taken".into()));
            assert_eq!(with_lock_in(&ctx, &root, "k", wait, || Ok(())), Ok(()));
        }

        #[cfg(feature = "test-panic")]
        #[test]
        fn test_panic_lock_panics_inside_guard() {
            let dir = TempDir::new();
            let root = dir.path().join("locks");
            let mut ctx = Fakes::default().ctx();
            ctx.env = Env::from_pairs([("MUX_AGENTS_TEST_PANIC", "lock")]);
            let ran = Cell::new(false);
            let panicked = catch_unwind(AssertUnwindSafe(|| {
                with_lock_in(&ctx, &root, "k", LOCK_WAIT, || {
                    ran.set(true);
                    Ok(())
                })
            }));
            assert!(panicked.is_err() && !ran.get());
            let probe = File::open(lock_path(&root, "k")).unwrap();
            probe.try_lock().unwrap();
        }

        #[test]
        fn symlinked_lock_file_refuses_state() {
            let not_private = state("the lock file is not private");
            let dir = TempDir::new();
            let root = dir.path().join("locks");
            let ctx = Fakes::default().ctx();
            let wait = Duration::from_millis(50);
            assert_eq!(with_lock_in(&ctx, &root, "made", wait, || Ok(())), Ok(()));

            let target = dir.path().join("target");
            std::fs::write(&target, b"keep").unwrap();
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
            symlink(&target, lock_path(&root, "link")).unwrap();
            let dangling = dir.path().join("nowhere");
            symlink(&dangling, lock_path(&root, "dangling")).unwrap();
            for key in ["link", "dangling"] {
                let refused = with_lock_in(&ctx, &root, key, wait, || Ok(()));
                assert_eq!(refusal(refused), not_private, "{key}");
            }
            assert_eq!(std::fs::read(&target).unwrap(), b"keep");
            assert!(!dangling.exists());

            // A hard link to another file, and a wider mode.
            std::fs::hard_link(&target, lock_path(&root, "hard")).unwrap();
            let wide = lock_path(&root, "wide");
            std::fs::write(&wide, b"").unwrap();
            std::fs::set_permissions(&wide, std::fs::Permissions::from_mode(0o644)).unwrap();
            std::fs::create_dir(lock_path(&root, "dir")).unwrap();
            for key in ["hard", "wide", "dir"] {
                let refused = with_lock_in(&ctx, &root, key, wait, || Ok(()));
                assert_eq!(refusal(refused), not_private, "{key}");
            }

            // The lock folder itself is checked like a state folder.
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
            let refused = with_lock_in(&ctx, &root, "made", wait, || Ok(()));
            assert_eq!(
                refusal(refused),
                state("the state folder mode is 755, not 700")
            );
        }
    }

    // CI only: the named mutex across threads (a mutex is recursive within its owning thread).
    #[cfg(windows)]
    mod windows {
        use super::*;
        use crate::testing::Fakes;
        use std::panic::{AssertUnwindSafe, catch_unwind};
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};

        fn key() -> String {
            format!("test-{}", crate::util::hex_id().unwrap())
        }

        // Runs `with_lock_timeout` on another thread, with its own fakes (they are not Send).
        fn try_elsewhere(key: &str, wait: Duration) -> Res<u8> {
            let key = key.to_owned();
            std::thread::spawn(move || {
                with_lock_timeout(&Fakes::default().ctx(), &key, wait, || Ok(1))
            })
            .join()
            .unwrap()
        }

        #[test]
        fn windows_mutex_timeout_refuses_lock() {
            let key = key();
            let ctx = Fakes::default().ctx();
            let wait = Duration::from_millis(200);
            let refused =
                with_lock_timeout(&ctx, &key, LOCK_WAIT, || Ok(try_elsewhere(&key, wait)))
                    .unwrap()
                    .unwrap_err();
            assert_eq!(
                (refused.code, refused.check, refused.message.as_str()),
                (2, "lock", "timed out waiting for the agent lock")
            );
            assert_eq!(try_elsewhere(&key, wait), Ok(1));
        }

        #[test]
        fn windows_mutex_abandoned_counts_as_held() {
            let key = key();
            let name: Vec<u16> = mutex_name(&key).encode_utf16().chain([0]).collect();
            // A thread takes the mutex and exits holding it; its handle stays open here.
            let handle = std::thread::spawn(move || {
                // SAFETY: test-only; the handle is returned and closed below.
                let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
                assert!(!mutex.is_null());
                assert_eq!(unsafe { WaitForSingleObject(mutex, 0) }, WAIT_OBJECT_0);
                mutex as usize
            })
            .join()
            .unwrap();
            assert_eq!(try_elsewhere(&key, Duration::from_millis(200)), Ok(1));
            // SAFETY: the handle from the thread above, closed once.
            unsafe { CloseHandle(handle as _) };
        }

        #[test]
        fn windows_mutex_released_after_unwind() {
            let key = key();
            let ctx = Fakes::default().ctx();
            let panicked = catch_unwind(AssertUnwindSafe(|| {
                with_lock_timeout(&ctx, &key, LOCK_WAIT, || -> Res<()> { panic!("inside") })
            }));
            assert!(panicked.is_err());
            assert_eq!(try_elsewhere(&key, Duration::from_millis(200)), Ok(1));
        }
    }
}
