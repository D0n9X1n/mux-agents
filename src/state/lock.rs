//! Per-user locks (D2): the keys serialize across state roots. Unix uses `File::try_lock` on a private lock
//! file; Windows keeps the named mutex.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ctx::Ctx;
use crate::error::{Res, refuse};
#[cfg(unix)]
use crate::state::private_unix::{Real, Steps};

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
///
/// Only the home is resolved: the passwd entry is trusted and may be a link (/home on macOS). Below it each
/// existing component is read with lstat, never followed: a link, a file, another owner or group or other
/// write refuses 1 `state`, so no other user can repoint a component and split one key across two lock
/// files. The walk stops at the first missing component; `with_lock_in` makes the rest at 0700 and checks
/// `locks` as a state folder.
#[cfg(unix)]
pub(crate) fn lock_root(steps: &dyn Steps, home: &Path) -> Res<PathBuf> {
    let unreadable = || refuse(1, "state", "the home folder of the user cannot be read");
    if !home.is_absolute() {
        return unreadable();
    }
    let Ok(mut path) = steps.realpath(home) else {
        return unreadable();
    };
    if !path.is_absolute() {
        return unreadable();
    }
    let uid = steps.uid();
    // `locks` itself is left to `with_lock_in`, which refuses a link there and checks it as a state folder.
    let above = [".local", "state", "mux-agents"];
    for (index, part) in above.iter().enumerate() {
        path.push(part);
        if !owned_dir(steps, &path, uid)? {
            path.extend(&above[index + 1..]);
            break;
        }
    }
    path.push("locks");
    Ok(path)
}

// Whether `path` is an existing folder of `uid` that no one else can write; `false` when nothing is there.
#[cfg(unix)]
fn owned_dir(steps: &dyn Steps, path: &Path, uid: u32) -> Res<bool> {
    use crate::state::private_unix::Kind;
    let state = |message: &str| refuse(1, "state", message.to_owned());
    let Ok(stat) = steps.lstat(path) else {
        return state("the state folder cannot be checked");
    };
    match stat.kind {
        Kind::Missing => return Ok(false),
        Kind::Link => return state("the state folder path has a link"),
        Kind::File => return state("the state folder path is a file"),
        Kind::Dir => {}
    }
    if stat.uid != uid {
        return state("a folder above the state folder has another owner");
    }
    // No sticky exception: another user could still make a missing component below it.
    if stat.mode & 0o022 != 0 {
        return state("a folder above the state folder is writable by others");
    }
    Ok(true)
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
    with_lock_under(ctx, &passwd_home()?, key, f)
}

/// `with_lock` for a given passwd home, so tests never touch the real one.
#[cfg(unix)]
pub(crate) fn with_lock_under<T>(
    ctx: &Ctx,
    home: &Path,
    key: &str,
    f: impl FnOnce() -> Res<T>,
) -> Res<T> {
    with_lock_in(ctx, &lock_root(&Real, home)?, key, LOCK_WAIT, f)
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
    // A flock belongs to the open file description, which a child forked while it is held shares until it
    // execs (O_CLOEXEC acts only at exec), so closing our descriptor alone can leave the lock held. Unlocking
    // releases it for every holder of the description; Drop runs on return, on `Err` and while unwinding.
    struct Held<'a>(&'a std::fs::File);
    impl Drop for Held<'_> {
        fn drop(&mut self) {
            // Nothing to report it to; the close that follows releases what it can.
            let _ = self.0.unlock();
        }
    }
    crate::state::private_unix::private_dirs(root, &[root])?;
    let file = open_lock(&lock_path(root, key))?;
    // The wait is clock time, as .NET's WaitOne(15000) is: on a loaded machine a 10 ms sleep can take far
    // longer. Each back-off counts what the clock moved, never less than BACK_OFF, so a clock that stands
    // still or is set back still refuses after wait / BACK_OFF back-offs.
    let mut waited = Duration::ZERO;
    let mut last = ctx.clock.now();
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if waited < wait => {
                ctx.clock.sleep(BACK_OFF);
                let now = ctx.clock.now();
                waited += now.duration_since(last).unwrap_or_default().max(BACK_OFF);
                last = now;
            }
            Err(TryLockError::WouldBlock) => return timed_out(),
            Err(TryLockError::Error(e)) => return Err(e.into()),
        }
    }
    // Unlocked before `file` closes, as the Windows guard releases the mutex before its handle closes.
    let _held = Held(&file);
    test_panic(ctx);
    f()
}

#[cfg(unix)]
const BACK_OFF: Duration = Duration::from_millis(10);

/// Runs `f` holding the lock for `key` when one attempt takes it, never waiting: `None` when another holder has
/// it, so the caller can defer its work. A refusal of `f`'s own is passed on.
#[cfg(unix)]
pub fn try_with_lock<T>(ctx: &Ctx, key: &str, f: impl FnOnce() -> Res<T>) -> Res<Option<T>> {
    try_with_lock_in(ctx, &lock_root(&Real, &passwd_home()?)?, key, f)
}

/// `try_with_lock` under an explicit lock root, so tests never touch the real one.
#[cfg(unix)]
pub(crate) fn try_with_lock_in<T>(
    ctx: &Ctx,
    root: &Path,
    key: &str,
    f: impl FnOnce() -> Res<T>,
) -> Res<Option<T>> {
    once(|f| with_lock_in(ctx, root, key, Duration::ZERO, f), f)
}

/// Runs `f` holding the named mutex for `key` when one zero wait takes it (an abandoned mutex counts as held, as
/// in `with_lock`): `None` when another holder has it. A refusal of `f`'s own is passed on.
#[cfg(windows)]
pub fn try_with_lock<T>(ctx: &Ctx, key: &str, f: impl FnOnce() -> Res<T>) -> Res<Option<T>> {
    once(|f| with_lock_timeout(ctx, key, Duration::ZERO, f), f)
}

// A zero-wait lock call: its `lock` refusal before `f` ran is "busy", anything else is `f`'s own.
fn once<T>(
    take: impl FnOnce(&mut dyn FnMut() -> Res<T>) -> Res<T>,
    f: impl FnOnce() -> Res<T>,
) -> Res<Option<T>> {
    let mut f = Some(f);
    let ran = std::cell::Cell::new(false);
    let got = take(&mut || {
        ran.set(true);
        (f.take().expect("run once"))()
    });
    match got {
        Ok(value) => Ok(Some(value)),
        Err(refusal) if refusal.check == "lock" && !ran.get() => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

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
    // Same order as Unix: released on return, on `Err` and while unwinding, before the handle closes.
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
    fn lock_wait_is_15_seconds() {
        assert_eq!(LOCK_WAIT, Duration::from_secs(15));
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
                ..
            } = result.unwrap_err();
            (code, check, message)
        }

        fn state(message: &str) -> (u8, &'static str, String) {
            (1, "state", message.into())
        }

        // Whether a fresh descriptor can take the lock right now; it unlocks what it took.
        fn free_now(path: &Path) -> bool {
            let probe = File::open(path).unwrap();
            let got = probe.try_lock().is_ok();
            if got {
                probe.unlock().unwrap();
            }
            got
        }

        const PARTS: [&str; 4] = [".local", "state", "mux-agents", "locks"];

        fn mkdir_mode(path: &Path, mode: u32) {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }

        // A private home with the first `depth` lock-root components made at 0700; the deepest one made.
        fn home_with(dir: &TempDir, depth: usize) -> (PathBuf, PathBuf) {
            let home = dir.path().join("home");
            mkdir_mode(&home, 0o700);
            let mut path = home.clone();
            for part in &PARTS[..depth] {
                path.push(part);
                mkdir_mode(&path, 0o700);
            }
            (home, path)
        }

        // The root comes from the passwd home alone: HOME, XDG_STATE_HOME, MUX_AGENTS_STATE and the ctx's own
        // home and state root all point at a decoy, which stays empty.
        #[test]
        fn lock_root_ignores_home_xdg_and_state() {
            let dir = TempDir::new();
            let (home, _) = home_with(&dir, 0);
            let decoy = dir.path().join("decoy");
            mkdir_mode(&decoy, 0o700);
            let d = decoy.to_str().unwrap();
            let mut ctx = Fakes::default().ctx();
            ctx.env = Env::from_pairs([
                ("HOME", d.to_owned()),
                ("XDG_STATE_HOME", format!("{d}/xdg")),
                ("MUX_AGENTS_STATE", format!("{d}/state")),
            ]);
            ctx.home = decoy.clone();
            ctx.state_root = decoy.join("state");
            assert_eq!(with_lock_under(&ctx, &home, "k", || Ok(5)), Ok(5));
            assert!(lock_path(&home.join(PARTS.join("/")), "k").is_file());
            assert_eq!(std::fs::read_dir(&decoy).unwrap().count(), 0);

            // `with_lock` passes the home of the effective uid's passwd entry, read here by the non-reentrant
            // call. Resolving it only reads: nothing is made under the real home.
            // SAFETY: getpwuid's result is read at once, before any other passwd call on this thread.
            let passwd = unsafe {
                let pw = libc::getpwuid(libc::geteuid());
                assert!(!pw.is_null());
                std::ffi::CStr::from_ptr((*pw).pw_dir)
                    .to_str()
                    .unwrap()
                    .to_owned()
            };
            assert_eq!(passwd_home(), Ok(PathBuf::from(&passwd)));
            let want = std::fs::canonicalize(&passwd)
                .unwrap()
                .join(".local/state/mux-agents/locks");
            assert_eq!(lock_root(&Real, &passwd_home().unwrap()), Ok(want));
        }

        // Moving `.local/state` (or any component below the home) to a link would split one key across two
        // lock files between calls, so a link anywhere below the home refuses and nothing is made past it.
        #[test]
        fn lock_root_symlinked_component_refuses() {
            for (depth, part) in PARTS.iter().enumerate() {
                let dir = TempDir::new();
                let (home, above) = home_with(&dir, depth);
                let elsewhere = dir.path().join("elsewhere");
                mkdir_mode(&elsewhere, 0o700);
                symlink(&elsewhere, above.join(part)).unwrap();
                let ctx = Fakes::default().ctx();
                let ran = Cell::new(false);
                let refused = with_lock_under(&ctx, &home, "k", || {
                    ran.set(true);
                    Ok(())
                });
                assert_eq!(
                    refusal(refused),
                    state("the state folder path has a link"),
                    "{part}"
                );
                assert!(!ran.get());
                assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
            }
        }

        // The sticky bit does not help here: another user could still make a missing component.
        #[test]
        fn lock_root_writable_component_refuses() {
            let open = state("a folder above the state folder is writable by others");
            for depth in 1..PARTS.len() {
                for bits in [0o1777, 0o1770, 0o775, 0o757] {
                    let dir = TempDir::new();
                    let (home, deepest) = home_with(&dir, depth);
                    std::fs::set_permissions(&deepest, std::fs::Permissions::from_mode(bits))
                        .unwrap();
                    let refused = lock_root(&Real, &home);
                    assert_eq!(refusal(refused), open, "{deepest:?} {bits:o}");
                    let locks = home.join(PARTS.join("/"));
                    assert!(!locks.exists(), "{deepest:?} {bits:o}");
                }
            }
        }

        // A component of another owner, injected: the walk compares the owner with the effective uid.
        #[test]
        fn lock_root_foreign_component_refuses() {
            struct Foreign<'a>(&'a Path);
            impl Steps for Foreign<'_> {
                fn lstat(&self, path: &Path) -> std::io::Result<crate::state::private_unix::Stat> {
                    let mut stat = Real.lstat(path)?;
                    if path == self.0 {
                        stat.uid = self.uid() + 1;
                    }
                    Ok(stat)
                }
            }
            for depth in 1..PARTS.len() {
                let dir = TempDir::new();
                let (home, deepest) = home_with(&dir, depth);
                let refused = lock_root(&Foreign(&deepest), &home);
                assert_eq!(
                    refusal(refused),
                    state("a folder above the state folder has another owner"),
                    "{deepest:?}"
                );
                assert!(!home.join(PARTS.join("/")).exists());
            }
        }

        // The passwd home may itself be a link (/home -> /System/Volumes/Data/home); it is resolved once.
        #[test]
        fn lock_root_follows_symlinked_home() {
            let dir = TempDir::new();
            let real = dir.path().join("real");
            mkdir_mode(&real, 0o700);
            let home = dir.path().join("home");
            symlink(&real, &home).unwrap();
            let want = real.join(PARTS.join("/"));
            let ctx = Fakes::default().ctx();
            assert_eq!(with_lock_under(&ctx, &home, "k", || Ok(3)), Ok(3));
            assert!(lock_path(&want, "k").is_file());
            assert_eq!(lock_root(&Real, &home), Ok(want.clone()));
            for path in [real.join(".local"), real.join(".local/state"), want] {
                assert_eq!(
                    std::fs::symlink_metadata(&path).unwrap().mode() & 0o7777,
                    0o700,
                    "{path:?}"
                );
            }
        }

        // A home that is missing or not absolute refuses; nothing is made in its place.
        #[test]
        fn lock_root_missing_home_refuses() {
            let unreadable = state("the home folder of the user cannot be read");
            let dir = TempDir::new();
            let missing = dir.path().join("gone");
            assert_eq!(refusal(lock_root(&Real, &missing)), unreadable);
            assert!(!missing.exists());
            assert_eq!(refusal(lock_root(&Real, Path::new("rel/home"))), unreadable);
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
            holder.unlock().unwrap();
            drop(holder);
            assert_eq!(with_lock_in(&ctx, &root, "k", wait, || Ok(2)), Ok(2));
            // The file stays.
            assert!(path.is_file());
        }

        // One attempt, no wait: a held lock answers None at once (no back-off on the clock) and the action never
        // runs; a free one runs it. A refusal of the action's own is passed on, never read as busy.
        #[test]
        fn try_takes_once_without_waiting() {
            let dir = TempDir::new();
            let root = dir.path().join("locks");
            let fakes = Fakes::default();
            let ctx = fakes.ctx();
            assert_eq!(try_with_lock_in(&ctx, &root, "k", || Ok(7)), Ok(Some(7)));
            let holder = File::open(lock_path(&root, "k")).unwrap();
            holder.lock().unwrap();
            let ran = Cell::new(false);
            let busy = try_with_lock_in(&ctx, &root, "k", || {
                ran.set(true);
                Ok(())
            });
            assert_eq!(busy, Ok(None));
            assert!(!ran.get());
            assert!(fakes.slept().is_empty());
            holder.unlock().unwrap();
            drop(holder);
            let inner = try_with_lock_in(&ctx, &root, "k", || -> Res<()> {
                refuse(2, "lock", "timed out waiting for the agent lock")
            });
            assert_eq!(
                refusal(inner),
                (2, "lock", "timed out waiting for the agent lock".into())
            );
            assert!(free_now(&lock_path(&root, "k")));
            assert_eq!(try_with_lock_in(&ctx, &root, "k", || Ok(2)), Ok(Some(2)));
        }

        // The lock is held while the action runs: another descriptor cannot take it then.
        #[test]
        fn held_while_action_runs() {
            let dir = TempDir::new();
            let root = dir.path().join("locks");
            let ctx = Fakes::default().ctx();
            let path = lock_path(&root, "k");
            let inside = with_lock_in(&ctx, &root, "k", LOCK_WAIT, || {
                let probe = File::open(&path).unwrap();
                Ok(matches!(
                    probe.try_lock(),
                    Err(std::fs::TryLockError::WouldBlock)
                ))
            });
            assert_eq!(inside, Ok(true));
        }

        // `with_lock`'s own wait: 1500 back-offs of 10 ms on the ctx clock, then the refusal; the action never
        // runs.
        #[test]
        fn default_wait_is_1500_back_offs() {
            let dir = TempDir::new();
            let (home, _) = home_with(&dir, 0);
            let fakes = Fakes::default();
            let ctx = fakes.ctx();
            assert_eq!(with_lock_under(&ctx, &home, "k", || Ok(())), Ok(()));
            let root = home.join(PARTS.join("/"));
            let holder = File::open(lock_path(&root, "k")).unwrap();
            holder.lock().unwrap();
            let ran = Cell::new(false);
            let refused = with_lock_under(&ctx, &home, "k", || {
                ran.set(true);
                Ok(())
            });
            assert_eq!(
                refusal(refused),
                (2, "lock", "timed out waiting for the agent lock".into())
            );
            assert!(!ran.get());
            assert_eq!(fakes.slept(), vec![10; 1500]);
            holder.unlock().unwrap();
        }

        // A clock whose every back-off moves `now` by `step_ms` (negative: backwards), whatever was asked: a
        // loaded machine where each 10 ms sleep overruns, or a clock that is set back. It counts its sleeps.
        #[derive(Clone)]
        struct Skewed(std::rc::Rc<(Cell<i64>, Cell<u32>)>, i64);

        impl crate::ctx::Clock for Skewed {
            fn now(&self) -> std::time::SystemTime {
                let base = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000);
                let ms = self.0.0.get();
                if ms >= 0 {
                    base + Duration::from_millis(ms as u64)
                } else {
                    base - Duration::from_millis(ms.unsigned_abs())
                }
            }

            fn sleep(&self, _: Duration) {
                self.0.1.set(self.0.1.get() + 1);
                self.0.0.set(self.0.0.get() + self.1);
            }
        }

        // The wait is 15 s of clock time, as .NET's WaitOne(15000) is, not 1500 sleeps: when each 10 ms
        // sleep takes 40 ms it refuses after 375. A clock that stands still or goes back never makes it
        // endless: each back-off counts at least 10 ms, so it refuses after 1500 at most.
        #[test]
        fn wait_counts_clock_time_not_back_offs() {
            let dir = TempDir::new();
            let root = dir.path().join("locks");
            let fakes = Fakes::default();
            assert_eq!(
                with_lock_in(&fakes.ctx(), &root, "k", LOCK_WAIT, || Ok(())),
                Ok(())
            );
            let holder = File::open(lock_path(&root, "k")).unwrap();
            holder.lock().unwrap();
            for (step_ms, sleeps) in [(40, 375), (10, 1500), (0, 1500), (-1000, 1500)] {
                let clock = Skewed(Default::default(), step_ms);
                let mut ctx = fakes.ctx();
                ctx.clock = Box::new(clock.clone());
                let ran = Cell::new(false);
                let refused = with_lock_in(&ctx, &root, "k", LOCK_WAIT, || {
                    ran.set(true);
                    Ok(())
                });
                assert_eq!(
                    refusal(refused),
                    (2, "lock", "timed out waiting for the agent lock".into()),
                    "{step_ms}"
                );
                assert!(!ran.get());
                assert_eq!(clock.0.1.get(), sleeps, "{step_ms}");
            }
            holder.unlock().unwrap();
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
            assert!(free_now(&lock_path(&root, "k")));
            assert_eq!(with_lock_in(&ctx, &root, "k", wait, || Ok(())), Ok(()));
            // A refusal inside the guard releases it too.
            let refused = with_lock_in(&ctx, &root, "k", wait, || -> Res<()> {
                refuse(2, "name", "taken")
            });
            assert_eq!(refusal(refused), (2, "name", "taken".into()));
            assert_eq!(with_lock_in(&ctx, &root, "k", wait, || Ok(())), Ok(()));
        }

        // A child forked while the lock is held, parked before exec: it shares the lock file's open file
        // description, and so its flock, until it exits. Dropping releases it (and waits), even mid-assert.
        struct ParkedChild {
            pid: libc::pid_t,
            release: libc::c_int,
        }

        impl ParkedChild {
            fn fork() -> Self {
                let mut fds = [0 as libc::c_int; 2];
                // SAFETY: `fds` has room for the two descriptors pipe writes.
                assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
                let [wait_on, release] = fds;
                // SAFETY: the child makes only async-signal-safe calls (close, read, _exit): no allocation,
                // no std I/O, no unwinding.
                let pid = unsafe { libc::fork() };
                if pid == 0 {
                    // SAFETY: as above; the child never returns from this block.
                    unsafe {
                        libc::close(release);
                        let mut byte = 0u8;
                        while libc::read(wait_on, (&raw mut byte).cast(), 1) < 0 {}
                        libc::_exit(0);
                    }
                }
                // SAFETY: the parent's copy of the read end, closed once.
                unsafe { libc::close(wait_on) };
                assert!(pid > 0, "fork failed");
                Self { pid, release }
            }

            // Whether the child has not exited yet.
            fn alive(&self) -> bool {
                let mut status = 0;
                // SAFETY: our own child; WNOHANG returns 0 while it runs and does not reap it then.
                unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) == 0 }
            }
        }

        impl Drop for ParkedChild {
            fn drop(&mut self) {
                let mut status = 0;
                // SAFETY: the write end, closed once (EOF releases the child too); then our own child is reaped.
                unsafe {
                    libc::write(self.release, [1u8].as_ptr().cast(), 1);
                    libc::close(self.release);
                    libc::waitpid(self.pid, &mut status, 0);
                }
            }
        }

        // With the child still parked, the lock is free at once: a fresh descriptor takes it without
        // blocking, and the next `with_lock_in` runs its action without a single back-off.
        fn assert_free_while_parked(root: &Path, child: &ParkedChild) {
            assert!(
                free_now(&lock_path(root, "k")),
                "lock still held after release"
            );
            let fakes = Fakes::default();
            let again = with_lock_in(&fakes.ctx(), root, "k", LOCK_WAIT, || Ok(9));
            assert_eq!((again, fakes.slept()), (Ok(9), vec![]));
            assert!(child.alive(), "the child exited before the checks");
        }

        #[test]
        fn release_while_forked_child_holds_fd() {
            let dir = TempDir::new();
            let root = dir.path().join("locks");
            let ctx = Fakes::default().ctx();
            let child =
                with_lock_in(&ctx, &root, "k", LOCK_WAIT, || Ok(ParkedChild::fork())).unwrap();
            assert_free_while_parked(&root, &child);
        }

        #[test]
        fn release_after_panic_while_forked_child_holds_fd() {
            let dir = TempDir::new();
            let root = dir.path().join("locks");
            let ctx = Fakes::default().ctx();
            let child = std::cell::RefCell::new(None);
            let panicked = catch_unwind(AssertUnwindSafe(|| {
                with_lock_in(&ctx, &root, "k", LOCK_WAIT, || -> Res<()> {
                    *child.borrow_mut() = Some(ParkedChild::fork());
                    panic!("inside")
                })
            }));
            assert!(panicked.is_err());
            let child = child.into_inner().unwrap();
            assert_free_while_parked(&root, &child);
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
            assert!(free_now(&lock_path(&root, "k")));
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
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use windows_sys::Win32::Foundation::{
            CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
        };
        use windows_sys::Win32::System::Threading::{
            CreateMutexW, ReleaseMutex, WaitForSingleObject,
        };

        fn key() -> String {
            format!("test-{}", crate::util::hex_id().unwrap())
        }

        fn wide_name(key: &str) -> Vec<u16> {
            mutex_name(key).encode_utf16().chain([0]).collect()
        }

        // An independent handle to the key's mutex, not owned; it keeps the named object alive.
        fn open(key: &str) -> usize {
            let name = wide_name(key);
            // SAFETY: `name` is NUL-terminated; the handle is closed by the caller.
            let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
            assert!(!mutex.is_null());
            mutex as usize
        }

        // What another thread's zero wait on the key's mutex returns; it releases what it took.
        fn wait_elsewhere(key: &str) -> u32 {
            let key = key.to_owned();
            std::thread::spawn(move || {
                let mutex = open(&key) as _;
                // SAFETY: a live mutex handle, released only if taken, closed once.
                unsafe {
                    let got = WaitForSingleObject(mutex, 0);
                    if got == WAIT_OBJECT_0 || got == WAIT_ABANDONED {
                        ReleaseMutex(mutex);
                    }
                    CloseHandle(mutex);
                    got
                }
            })
            .join()
            .unwrap()
        }

        // Runs `with_lock_timeout` on another thread, with its own fakes (they are not Send); whether its
        // action ran, and the result.
        fn try_elsewhere(key: &str, wait: Duration) -> (bool, Res<u8>) {
            let key = key.to_owned();
            let ran = Arc::new(AtomicBool::new(false));
            let seen = ran.clone();
            let got = std::thread::spawn(move || {
                with_lock_timeout(&Fakes::default().ctx(), &key, wait, || {
                    seen.store(true, Ordering::SeqCst);
                    Ok(1)
                })
            })
            .join()
            .unwrap();
            (ran.load(Ordering::SeqCst), got)
        }

        #[test]
        fn windows_mutex_timeout_refuses_lock() {
            let key = key();
            let ctx = Fakes::default().ctx();
            let wait = Duration::from_millis(200);
            let (inside, (ran, refused)) = with_lock_timeout(&ctx, &key, LOCK_WAIT, || {
                Ok((wait_elsewhere(&key), try_elsewhere(&key, wait)))
            })
            .unwrap();
            // Held while the action runs: another thread's wait times out.
            assert_eq!(inside, WAIT_TIMEOUT);
            let refused = refused.unwrap_err();
            assert_eq!(
                (refused.code, refused.check, refused.message.as_str()),
                (2, "lock", "timed out waiting for the agent lock")
            );
            assert!(!ran);
            assert_eq!(try_elsewhere(&key, wait), (true, Ok(1)));
        }

        // One attempt, no wait: another thread's try while the mutex is held answers None at once; once it is
        // released, the try runs its action.
        #[test]
        fn windows_try_takes_once_without_waiting() {
            let key = key();
            let attempt = |key: &str| {
                let key = key.to_owned();
                std::thread::spawn(move || {
                    let ran = Arc::new(AtomicBool::new(false));
                    let seen = ran.clone();
                    let at = std::time::Instant::now();
                    let got = try_with_lock(&Fakes::default().ctx(), &key, || {
                        seen.store(true, Ordering::SeqCst);
                        Ok(1u8)
                    });
                    (ran.load(Ordering::SeqCst), got, at.elapsed())
                })
                .join()
                .unwrap()
            };
            let ctx = Fakes::default().ctx();
            let (ran, busy, took) =
                with_lock_timeout(&ctx, &key, LOCK_WAIT, || Ok(attempt(&key))).unwrap();
            assert_eq!((ran, busy), (false, Ok(None)));
            assert!(took < Duration::from_secs(1), "{took:?}");
            let (ran, free, _) = attempt(&key);
            assert_eq!((ran, free), (true, Ok(Some(1))));
        }

        #[test]
        fn windows_mutex_abandoned_counts_as_held() {
            let key = key();
            let keep = open(&key);
            let name = wide_name(&key);
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
            assert_eq!(
                try_elsewhere(&key, Duration::from_millis(200)),
                (true, Ok(1))
            );
            // Recovered and released: the next wait gets it outright, not abandoned.
            assert_eq!(wait_elsewhere(&key), WAIT_OBJECT_0);
            // SAFETY: the handles from above, closed once.
            unsafe {
                CloseHandle(handle as _);
                CloseHandle(keep as _);
            }
        }

        #[test]
        fn windows_mutex_released_after_unwind() {
            let key = key();
            // Kept open across the unwind, so the object outlives the guard's handle and the wait below sees
            // whether the guard released it (WAIT_OBJECT_0) or left it held or abandoned.
            let keep = open(&key);
            let ctx = Fakes::default().ctx();
            let panicked = catch_unwind(AssertUnwindSafe(|| {
                with_lock_timeout(&ctx, &key, LOCK_WAIT, || -> Res<()> { panic!("inside") })
            }));
            assert!(panicked.is_err());
            assert_eq!(wait_elsewhere(&key), WAIT_OBJECT_0);
            // SAFETY: the handle from above, closed once.
            unsafe { CloseHandle(keep as _) };
        }
    }
}
