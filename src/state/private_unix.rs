//! Unix state folders private to the user, checked in-process (MuxAgents.Unix.ps1:216-348, D3).

use std::collections::HashSet;
use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::error::{Res, refuse};
use crate::state::acl::Acl;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Missing,
    Link,
    File,
    Dir,
}

/// One lstat of a path: the identity, the full mode (setuid, setgid and sticky included) and the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub kind: Kind,
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub uid: u32,
}

/// What the privacy decision runs on, bound to one folder: lstat, ACL and realpath, then lstat again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub path: PathBuf,
    pub first: Stat,
    pub acl: Acl,
    pub real: PathBuf,
    pub second: Stat,
}

/// The system calls the walk makes; tests override one to fail or race it.
pub trait Steps {
    fn uid(&self) -> u32 {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }

    /// lstat semantics: a final link is reported, not followed. Nothing there is `Kind::Missing`.
    fn lstat(&self, path: &Path) -> io::Result<Stat> {
        use std::os::unix::fs::MetadataExt;
        let meta = match std::fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(Stat {
                    kind: Kind::Missing,
                    dev: 0,
                    ino: 0,
                    mode: 0,
                    uid: 0,
                });
            }
            Err(e) => return Err(e),
        };
        let kind = if meta.file_type().is_symlink() {
            Kind::Link
        } else if meta.is_dir() {
            Kind::Dir
        } else {
            Kind::File
        };
        Ok(Stat {
            kind,
            dev: meta.dev(),
            ino: meta.ino(),
            mode: meta.mode() & 0o7777,
            uid: meta.uid(),
        })
    }

    fn acl(&self, path: &Path) -> io::Result<Acl> {
        crate::state::acl::read(path)
    }

    fn realpath(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }

    /// mkdir(2) at 0700: never created wider, and it fails on anything already there, a link included.
    fn mkdir(&self, path: &Path) -> io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(path)
    }

    /// Restores the bits the umask cleared at mkdir.
    fn chmod(&self, path: &Path) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
}

pub struct Real;

impl Steps for Real {}

const CHECKED: &str = "the state folder cannot be checked";

/// Creates or checks each of `paths` under `root`, walking each from `/` down. Every existing folder above
/// the root must pass `decide_parent`; each missing folder is created at 0700; the root, everything below it
/// and everything this call created must pass `decide`. Each folder is checked once per call, before
/// anything is made inside it. Refuses 1 `state` at the first failure.
pub fn private_dirs(root: &Path, paths: &[&Path]) -> Res<()> {
    private_dirs_with(&Real, root, paths)
}

pub fn private_dirs_with(steps: &dyn Steps, root: &Path, paths: &[&Path]) -> Res<()> {
    let root = root.as_os_str().as_bytes();
    let inside = |path: &[u8]| {
        path == root || (path.starts_with(root) && path.get(root.len()) == Some(&b'/'))
    };
    let mut walks = Vec::with_capacity(paths.len());
    for path in paths {
        let path = path.as_os_str().as_bytes();
        let parts: Vec<&[u8]> = path
            .get(1..)
            .unwrap_or_default()
            .split(|&b| b == b'/')
            .collect();
        let odd = parts.iter().any(|p| matches!(*p, b"" | b"." | b".."));
        if !root.starts_with(b"/") || !path.starts_with(b"/") || !inside(path) || odd {
            return refuse(
                1,
                "state",
                "the state folder path is outside the state root",
            );
        }
        walks.push(parts);
    }
    let uid = steps.uid();
    check_parent(steps, Path::new("/"), uid)?;
    let mut checked: HashSet<Vec<u8>> = HashSet::from([b"/".to_vec()]);
    for parts in walks {
        let mut prefix = Vec::new();
        for part in parts {
            prefix.push(b'/');
            prefix.extend_from_slice(part);
            if checked.contains(&prefix) {
                continue;
            }
            let path = Path::new(OsStr::from_bytes(&prefix));
            if new_dir(steps, path)? || inside(&prefix) {
                decide(&facts(steps, path)?, uid)?;
            } else {
                check_parent(steps, path, uid)?;
            }
            checked.insert(prefix.clone());
        }
    }
    Ok(())
}

// Creates `path` when nothing is there; whether it did. An existing entry, link or not, is left for the
// caller's check. Once mkdir succeeded chmod reaches that folder: its parent was checked first (ours at
// 0700, or sticky), so no other user can rename or replace it.
fn new_dir(steps: &dyn Steps, path: &Path) -> Res<bool> {
    let Ok(stat) = steps.lstat(path) else {
        return refuse(1, "state", CHECKED);
    };
    if stat.kind != Kind::Missing {
        return Ok(false);
    }
    if steps.mkdir(path).and_then(|()| steps.chmod(path)).is_err() {
        return refuse(1, "state", "the state folder cannot be created");
    }
    Ok(true)
}

fn facts(steps: &dyn Steps, path: &Path) -> Res<Facts> {
    let failed = || refuse(1, "state", CHECKED);
    let Ok(first) = steps.lstat(path) else {
        return failed();
    };
    if first.kind != Kind::Dir {
        return Ok(Facts {
            path: path.into(),
            first,
            acl: Acl::default(),
            real: path.into(),
            second: first,
        });
    }
    let (Ok(acl), Ok(real), Ok(second)) =
        (steps.acl(path), steps.realpath(path), steps.lstat(path))
    else {
        return failed();
    };
    if !real.is_absolute() {
        return failed();
    }
    Ok(Facts {
        path: path.into(),
        first,
        acl,
        real,
        second,
    })
}

fn check_parent(steps: &dyn Steps, path: &Path, uid: u32) -> Res<()> {
    let Ok(stat) = steps.lstat(path) else {
        return refuse(1, "state", CHECKED);
    };
    if stat.kind == Kind::Missing {
        return refuse(1, "state", CHECKED);
    }
    // Only macOS ACEs matter above the root: a Linux ACL's write rights show in the group bits (the mask),
    // and the sticky rule ignores ACLs.
    let non_deny = if cfg!(target_os = "macos") {
        let Ok(acl) = steps.acl(path) else {
            return refuse(1, "state", CHECKED);
        };
        acl.non_deny
    } else {
        false
    };
    decide_parent(&stat, non_deny, uid)
}

/// The privacy decision on gathered facts: refuses 1 `state` at the first failed check, in this order.
pub fn decide(facts: &Facts, uid: u32) -> Res<()> {
    let state = |message: String| refuse(1, "state", message);
    match facts.first.kind {
        Kind::Link => return state("the state folder path has a link".into()),
        Kind::File => return state("the state folder path is a file".into()),
        Kind::Missing => return state("the state folder is missing".into()),
        Kind::Dir => {}
    }
    if facts.second != facts.first {
        return state("the state folder changed while it was checked".into());
    }
    if facts.real != facts.path {
        return state("the state folder path has a link".into());
    }
    if facts.first.uid != uid {
        return state("the state folder has another owner".into());
    }
    if facts.first.mode != 0o700 {
        return state(format!(
            "the state folder mode is {:o}, not 700",
            facts.first.mode
        ));
    }
    if facts.acl.extended {
        return state("the state folder has an extended ACL".into());
    }
    Ok(())
}

/// A folder above the root, as OpenSSH StrictModes checks one: owned by root or the user, and not group- or
/// other-writable unless sticky (as /tmp is). On macOS an allow ACE (add_subdirectory, delete_child) outranks
/// both the mode and the sticky bit, so `non_deny` refuses.
pub fn decide_parent(stat: &Stat, non_deny: bool, uid: u32) -> Res<()> {
    if stat.uid != 0 && stat.uid != uid {
        return refuse(
            1,
            "state",
            "a folder above the state folder has another owner",
        );
    }
    if stat.mode & 0o022 != 0 && stat.mode & 0o1000 == 0 {
        return refuse(
            1,
            "state",
            "a folder above the state folder is writable by others",
        );
    }
    if non_deny {
        return refuse(
            1,
            "state",
            "a folder above the state folder has an ACL entry that is not a deny",
        );
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::error::Refusal;
    use crate::testing::TempDir;
    use std::cell::Cell;
    use std::os::unix::fs::{PermissionsExt, symlink};

    const CHECKED: &str = "the state folder cannot be checked";
    const LINK: &str = "the state folder path has a link";

    fn uid() -> u32 {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }

    fn mode(path: &Path) -> Option<u32> {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(path)
            .ok()
            .map(|m| m.mode() & 0o7777)
    }

    fn set_mode(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn mkdir_mode(path: &Path, mode: u32) {
        std::fs::create_dir(path).unwrap();
        set_mode(path, mode);
    }

    fn message(result: Res<()>) -> String {
        match result {
            Ok(()) => "ok".into(),
            Err(Refusal {
                code: 1,
                check: "state",
                message,
            }) => message,
            Err(other) => panic!("{other:?}"),
        }
    }

    fn run(root: &Path, paths: &[&Path]) -> String {
        message(private_dirs(root, paths))
    }

    fn stat(kind: Kind, mode: u32, uid: u32) -> Stat {
        Stat {
            kind,
            dev: 66,
            ino: 12,
            mode,
            uid,
        }
    }

    fn facts(first: Stat) -> Facts {
        Facts {
            path: "/r/k".into(),
            first,
            acl: Acl::default(),
            real: "/r/k".into(),
            second: first,
        }
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn chmod_a(path: &Path, ace: &str) {
        let status = std::process::Command::new("/bin/chmod")
            .arg("+a")
            .arg(ace)
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success(), "chmod +a {ace}");
    }

    #[test]
    fn creates_missing_at_0700() {
        let dir = TempDir::new();
        let root = dir.path().join("state");
        let (agents, mail) = (root.join("k/agents"), root.join("k/mail"));
        assert_eq!(run(&root, &[&root, &agents, &mail]), "ok");
        for path in [&root, &root.join("k"), &agents, &mail] {
            assert_eq!(mode(path), Some(0o700), "{path:?}");
        }
        // A second call finds them private and changes nothing.
        assert_eq!(run(&root, &[&agents, &mail]), "ok");
    }

    // UnixState.Tests.ps1:334: under each umask, mkdir(2) makes the folder no wider than 0700 and chmod then
    // restores the owner bits the umask cleared. The umask is per process, so a child runs `umask_child`.
    #[test]
    fn creates_missing_at_0700_under_umask() {
        for (umask, made) in [
            (0o000, 0o700),
            (0o077, 0o700),
            (0o177, 0o600),
            (0o777, 0o000),
        ] {
            let dir = TempDir::new();
            let root = dir.path().join("state");
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "state::private_unix::tests::umask_child",
                    "--exact",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("MUX_AGENT_TEST_UMASK", format!("{umask:o}"))
                .env("MUX_AGENT_TEST_ROOT", &root)
                .output()
                .unwrap();
            let stdout = String::from_utf8(out.stdout).unwrap();
            let line = stdout
                .lines()
                .find_map(|l| l.split_once("UMASK-RESULT ").map(|(_, r)| r))
                .unwrap_or_else(|| panic!("no result: {stdout}"));
            let step = format!("mkdir {made:o} chmod 700");
            let want = format!("{step}; {step}; {step}; {step}; ok");
            assert_eq!(line, want, "umask {umask:o}");
            for path in [
                &root,
                &root.join("k"),
                &root.join("k/agents"),
                &root.join("k/mail"),
            ] {
                assert_eq!(mode(path), Some(0o700), "umask {umask:o} {path:?}");
            }
        }
    }

    // Run only by `creates_missing_at_0700_under_umask`, in its own process.
    #[test]
    #[ignore = "a child of creates_missing_at_0700_under_umask"]
    fn umask_child() {
        use std::cell::RefCell;
        struct Observe(RefCell<Vec<String>>);
        impl Steps for Observe {
            fn mkdir(&self, path: &Path) -> io::Result<()> {
                Real.mkdir(path)?;
                let made = mode(path).unwrap();
                self.0.borrow_mut().push(format!("mkdir {made:o}"));
                Ok(())
            }
            fn chmod(&self, path: &Path) -> io::Result<()> {
                Real.chmod(path)?;
                let last = self.0.borrow_mut().pop().unwrap();
                let now = mode(path).unwrap();
                self.0.borrow_mut().push(format!("{last} chmod {now:o}"));
                Ok(())
            }
        }
        let (Ok(umask), Some(root)) = (
            std::env::var("MUX_AGENT_TEST_UMASK"),
            std::env::var_os("MUX_AGENT_TEST_ROOT"),
        ) else {
            return;
        };
        let root = PathBuf::from(root);
        // SAFETY: umask has no preconditions; this process runs only this test.
        unsafe { libc::umask(u32::from_str_radix(&umask, 8).unwrap() as libc::mode_t) };
        let steps = Observe(RefCell::new(Vec::new()));
        let (agents, mail) = (root.join("k/agents"), root.join("k/mail"));
        let got = message(private_dirs_with(&steps, &root, &[&root, &agents, &mail]));
        let mut steps = steps.0.into_inner();
        steps.push(got);
        println!("UMASK-RESULT {}", steps.join("; "));
    }

    #[test]
    fn mode_755_refuses_state() {
        let dir = TempDir::new();
        let root = dir.path().join("state");
        mkdir_mode(&root, 0o755);
        let k = root.join("k");
        assert_eq!(run(&root, &[&k]), "the state folder mode is 755, not 700");
        assert_eq!(mode(&k), None);
        set_mode(&root, 0o1700);
        assert_eq!(run(&root, &[&k]), "the state folder mode is 1700, not 700");
    }

    // A folder below the root gets the full check, not the parent rule: a 0755 one or one of another owner
    // refuses, and a later path in the same call is not made.
    #[test]
    fn descendant_not_private_refuses() {
        struct Owner<'a>(&'a Path);
        impl Steps for Owner<'_> {
            fn lstat(&self, path: &Path) -> io::Result<Stat> {
                let mut stat = Real.lstat(path)?;
                if path == self.0 {
                    stat.uid = if self.uid() == 0 { 1 } else { 0 };
                }
                Ok(stat)
            }
        }
        let dir = TempDir::new();
        let root = dir.path().join("state");
        mkdir_mode(&root, 0o700);
        let (open, owned, later) = (root.join("open"), root.join("owned"), root.join("later"));
        mkdir_mode(&open, 0o755);
        mkdir_mode(&owned, 0o700);
        assert_eq!(
            run(&root, &[&open.join("k"), &later]),
            "the state folder mode is 755, not 700"
        );
        let steps = Owner(&owned);
        assert_eq!(
            message(private_dirs_with(
                &steps,
                &root,
                &[&owned.join("k"), &later]
            )),
            "the state folder has another owner"
        );
        for path in [open.join("k"), owned.join("k"), later] {
            assert_eq!(mode(&path), None, "{path:?}");
        }
    }

    #[test]
    fn symlink_component_refuses_state() {
        let dir = TempDir::new();
        let base = dir.path();
        set_mode(base, 0o700);
        mkdir_mode(&base.join("real"), 0o700);
        mkdir_mode(&base.join("real/k"), 0o700);
        symlink(base.join("real"), base.join("link")).unwrap();
        symlink(base.join("nowhere"), base.join("dangling")).unwrap();
        for path in [
            base.join("link"),
            base.join("dangling"),
            base.join("link/k"),
        ] {
            assert_eq!(run(base, &[&path]), LINK, "{path:?}");
        }
        assert_eq!(mode(&base.join("nowhere")), None);
    }

    #[test]
    fn outside_root_refuses() {
        let outside = "the state folder path is outside the state root";
        for (root, path) in [
            ("/r/s", "/r/x"),
            ("/r/s", "/r/s2/k"),
            ("/r/s", "/r/s/../x"),
            ("/r/s", "/r/s//k"),
            ("/r/s", "/r/s/./k"),
            ("/r/s", "/r/s/k/"),
            ("r/s", "r/s/k"),
        ] {
            assert_eq!(
                run(Path::new(root), &[Path::new(path)]),
                outside,
                "{root} {path}"
            );
        }
    }

    #[test]
    fn other_owner_parent_refuses() {
        let me = 501;
        let owner = "a folder above the state folder has another owner";
        let open = "a folder above the state folder is writable by others";
        // Unix.ps1 tests: 0777 another uid, 1777 root, 0755 another uid, 0777 root, 0775 and 0757 ours, 0755
        // ours, 1777 another uid.
        let cases = [
            (502, 0o777, owner),
            (0, 0o1777, "ok"),
            (502, 0o755, owner),
            (0, 0o777, open),
            (me, 0o775, open),
            (me, 0o757, open),
            (me, 0o755, "ok"),
            (502, 0o1777, owner),
        ];
        for (uid, bits, want) in cases {
            let got = message(decide_parent(&stat(Kind::Dir, bits, uid), false, me));
            assert_eq!(got, want, "{uid} {bits:o}");
        }
        assert_eq!(
            message(decide(&facts(stat(Kind::Dir, 0o700, 0)), me)),
            "the state folder has another owner"
        );
        // A real root-owned folder, unless the tests run as root.
        if uid() != 0 {
            let usr = Path::new("/usr");
            assert_eq!(run(usr, &[usr]), "the state folder has another owner");
        }
    }

    // A real walk with one folder above the root reported as another user's: the walk compares each owner
    // with the effective uid, and refuses before making anything.
    #[test]
    fn foreign_ancestor_refuses_before_create() {
        struct Foreign<'a>(&'a Path);
        impl Steps for Foreign<'_> {
            fn lstat(&self, path: &Path) -> io::Result<Stat> {
                let mut stat = Real.lstat(path)?;
                if path == self.0 {
                    stat.uid = self.uid() + 1;
                }
                Ok(stat)
            }
        }
        let dir = TempDir::new();
        let above = dir.path().join("above");
        mkdir_mode(&above, 0o755);
        let root = above.join("state");
        assert_eq!(
            message(private_dirs_with(
                &Foreign(&above),
                &root,
                &[&root.join("k")]
            )),
            "a folder above the state folder has another owner"
        );
        assert_eq!(mode(&root), None);
        assert_eq!(run(&root, &[&root.join("k")]), "ok");
    }

    #[test]
    fn group_writable_parent_without_sticky_refuses() {
        let dir = TempDir::new();
        let (open, sticky) = (dir.path().join("open"), dir.path().join("sticky"));
        mkdir_mode(&open, 0o777);
        mkdir_mode(&sticky, 0o1777);
        assert_eq!(
            run(&open.join("state"), &[&open.join("state/k")]),
            "a folder above the state folder is writable by others"
        );
        assert_eq!(mode(&open.join("state")), None);
        assert_eq!(run(&sticky.join("state"), &[&sticky.join("state/k")]), "ok");
        assert_eq!(mode(&sticky.join("state/k")), Some(0o700));
    }

    #[test]
    fn changed_between_stats_refuses() {
        let changed = "the state folder changed while it was checked";
        let first = stat(Kind::Dir, 0o700, 501);
        assert_eq!(message(decide(&facts(first), 501)), "ok");
        for second in [
            Stat { ino: 13, ..first },
            Stat { dev: 67, ..first },
            Stat {
                mode: 0o777,
                ..first
            },
            Stat { uid: 502, ..first },
            Stat {
                kind: Kind::Link,
                ..first
            },
        ] {
            let f = Facts {
                second,
                ..facts(first)
            };
            assert_eq!(message(decide(&f, 501)), changed, "{second:?}");
        }

        // The decision's order: kind, changed, realpath, owner, mode, ACL.
        let ext = Acl {
            extended: true,
            non_deny: false,
        };
        let table = [
            (facts(stat(Kind::Link, 0o755, 0)), LINK),
            (
                facts(stat(Kind::File, 0o600, 501)),
                "the state folder path is a file",
            ),
            (
                facts(stat(Kind::Missing, 0, 0)),
                "the state folder is missing",
            ),
            (
                Facts {
                    real: "/r/other".into(),
                    ..facts(stat(Kind::Dir, 0o777, 0))
                },
                LINK,
            ),
            (
                facts(stat(Kind::Dir, 0o777, 0)),
                "the state folder has another owner",
            ),
            (
                Facts {
                    acl: ext,
                    ..facts(stat(Kind::Dir, 0o750, 501))
                },
                "the state folder mode is 750, not 700",
            ),
            (
                Facts {
                    acl: ext,
                    ..facts(first)
                },
                "the state folder has an extended ACL",
            ),
        ];
        for (f, want) in table {
            assert_eq!(message(decide(&f, 501)), want, "{f:?}");
        }

        // Gathered for real: the root's inode differs on its last lstat.
        struct Swapped<'a>(&'a Path, Cell<u32>);
        impl Steps for Swapped<'_> {
            fn lstat(&self, path: &Path) -> io::Result<Stat> {
                let mut stat = Real.lstat(path)?;
                if path == self.0 {
                    self.1.set(self.1.get() + 1);
                    if self.1.get() == 3 {
                        stat.ino += 1;
                    }
                }
                Ok(stat)
            }
        }
        let dir = TempDir::new();
        let root = dir.path().join("state");
        mkdir_mode(&root, 0o700);
        let steps = Swapped(&root, Cell::new(0));
        assert_eq!(message(private_dirs_with(&steps, &root, &[&root])), changed);
    }

    #[test]
    fn acl_read_error_refuses_state() {
        // Only the given path's ACL read fails.
        struct NoAcl<'a>(&'a Path);
        impl Steps for NoAcl<'_> {
            fn acl(&self, path: &Path) -> io::Result<Acl> {
                if path == self.0 {
                    return Err(io::Error::from(io::ErrorKind::PermissionDenied));
                }
                Real.acl(path)
            }
        }
        struct NoReal;
        impl Steps for NoReal {
            fn realpath(&self, _: &Path) -> io::Result<PathBuf> {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            }
        }
        let dir = TempDir::new();
        let root = dir.path().join("state");
        mkdir_mode(&root, 0o700);
        let k = root.join("k");
        // The state folder's own ACL.
        assert_eq!(
            message(private_dirs_with(&NoAcl(&root), &root, &[&k])),
            CHECKED
        );
        assert_eq!(mode(&k), None);
        assert_eq!(
            message(private_dirs_with(&NoReal, &root, &[&root])),
            CHECKED
        );
        // A folder above the root: only macOS reads ACLs there.
        let above = message(private_dirs_with(&NoAcl(dir.path()), &root, &[&k]));
        if cfg!(target_os = "macos") {
            assert_eq!(above, CHECKED);
            assert_eq!(mode(&k), None);
        } else {
            assert_eq!(above, "ok");
        }
    }

    // UnixState.Tests.ps1:224, 283: any failed lstat refuses, never passes.
    #[test]
    fn stat_read_error_refuses_state() {
        struct FailNth<'a>(&'a Path, u32, Cell<u32>);
        impl Steps for FailNth<'_> {
            fn lstat(&self, path: &Path) -> io::Result<Stat> {
                if path == self.0 {
                    self.2.set(self.2.get() + 1);
                    if self.2.get() == self.1 {
                        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
                    }
                }
                Real.lstat(path)
            }
        }
        let dir = TempDir::new();
        let root = dir.path().join("state");
        mkdir_mode(&root, 0o700);
        // The entry check, then the two stats of the facts.
        for nth in 1..=3 {
            let steps = FailNth(&root, nth, Cell::new(0));
            let got = message(private_dirs_with(&steps, &root, &[&root]));
            assert_eq!(got, CHECKED, "lstat {nth}");
        }
        // A folder above the root: `/` itself.
        let steps = FailNth(Path::new("/"), 1, Cell::new(0));
        assert_eq!(message(private_dirs_with(&steps, &root, &[&root])), CHECKED);
    }

    // UnixState.Tests.ps1:374: mkdir(2) fails on a link put there after the entry check; nothing follows it.
    #[test]
    fn link_appears_before_mkdir_refuses_no_create() {
        struct PlantLink<'a>(&'a Path);
        impl Steps for PlantLink<'_> {
            fn mkdir(&self, path: &Path) -> io::Result<()> {
                symlink(self.0, path)?;
                Real.mkdir(path)
            }
        }
        let dir = TempDir::new();
        let target = dir.path().join("target");
        mkdir_mode(&target, 0o755);
        let k = dir.path().join("k");
        let steps = PlantLink(&target);
        assert_eq!(
            message(private_dirs_with(&steps, &k, &[&k])),
            "the state folder cannot be created"
        );
        assert_eq!(mode(&target), Some(0o755));
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
        assert_eq!(std::fs::read_link(&k).unwrap(), target);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_allow_ace_refuses() {
        let dir = TempDir::new();
        let root = dir.path().join("state");
        assert_eq!(run(&root, &[&root]), "ok");
        chmod_a(&root, "everyone allow list");
        assert_eq!(run(&root, &[&root]), "the state folder has an extended ACL");
        // On the folder itself any ACE refuses, a deny too.
        let other = dir.path().join("other");
        assert_eq!(run(&other, &[&other]), "ok");
        chmod_a(&other, "everyone deny chown");
        assert_eq!(
            run(&other, &[&other]),
            "the state folder has an extended ACL"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ancestor_allow_ace_refuses_before_create() {
        let dir = TempDir::new();
        let above = dir.path().join("above");
        mkdir_mode(&above, 0o755);
        chmod_a(&above, "everyone allow add_subdirectory,delete_child");
        let root = above.join("state");
        assert_eq!(
            run(&root, &[&root.join("k")]),
            "a folder above the state folder has an ACL entry that is not a deny"
        );
        assert_eq!(mode(&root), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ancestor_deny_only_ace_passes() {
        let dir = TempDir::new();
        let above = dir.path().join("above");
        mkdir_mode(&above, 0o755);
        chmod_a(&above, "everyone deny chown");
        let root = above.join("state");
        assert_eq!(run(&root, &[&root.join("k")]), "ok");
        assert_eq!(mode(&root.join("k")), Some(0o700));
    }

    // UnixState.Tests.ps1:426: a deny ACE that folders inherit passes the parent check, but the new root
    // inherits it and refuses before anything is made in it.
    #[cfg(target_os = "macos")]
    #[test]
    fn inherited_ace_refuses_new_root() {
        let dir = TempDir::new();
        let parent = dir.path().join("parent");
        mkdir_mode(&parent, 0o755);
        chmod_a(&parent, "everyone deny chown,directory_inherit");
        let root = parent.join("state");
        assert_eq!(
            run(&root, &[&root.join("k")]),
            "the state folder has an extended ACL"
        );
        assert_eq!(mode(&root), Some(0o700));
        assert!(crate::state::acl::read(&root).unwrap().extended);
        assert_eq!(mode(&root.join("k")), None);
    }

    // An allow ACE outranks the sticky bit: an owned 1777 folder with one still refuses before creating.
    #[cfg(target_os = "macos")]
    #[test]
    fn sticky_ancestor_allow_ace_refuses() {
        let dir = TempDir::new();
        let above = dir.path().join("above");
        mkdir_mode(&above, 0o1777);
        let root = above.join("state");
        assert_eq!(run(&root, &[&root.join("k")]), "ok");
        std::fs::remove_dir_all(&root).unwrap();
        chmod_a(&above, "everyone allow add_subdirectory,delete_child");
        assert_eq!(
            run(&root, &[&root.join("k")]),
            "a folder above the state folder has an ACL entry that is not a deny"
        );
        assert_eq!(mode(&root), None);
    }

    // CI only: needs setfacl and a file system with POSIX ACLs. Each ACL is checked on its own at mode 0700, so
    // the mode refusal cannot stand in for the ACL one.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_setfacl_refuses_state() {
        let installed = std::process::Command::new("setfacl")
            .arg("--version")
            .output()
            .is_ok();
        if !installed {
            eprintln!("setfacl is not installed; skipped");
            return;
        }
        let setfacl = |args: &[&str], path: &Path| {
            let out = std::process::Command::new("setfacl")
                .args(args)
                .arg(path)
                .output()
                .unwrap();
            assert!(out.status.success(), "setfacl {args:?}: {out:?}");
        };
        let dir = TempDir::new();
        let (access, default) = (dir.path().join("access"), dir.path().join("default"));
        for root in [&access, &default] {
            assert_eq!(run(root, &[root]), "ok");
        }
        // A named user entry recomputes the mask into the group bits; chmod 0700 sets them back, as the
        // PowerShell test does, and leaves the ACL.
        setfacl(&["-m", "u:65534:rx"], &access);
        set_mode(&access, 0o700);
        assert_eq!(mode(&access), Some(0o700));
        assert_eq!(
            run(&access, &[&access]),
            "the state folder has an extended ACL"
        );
        // A default ACL alone leaves the mode at 0700.
        setfacl(&["-d", "-m", "u::rwx"], &default);
        assert_eq!(mode(&default), Some(0o700));
        assert_eq!(
            run(&default, &[&default]),
            "the state folder has an extended ACL"
        );
    }
}
