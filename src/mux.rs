//! The multiplexer seam: `<mux> -S <socket> -N <argv>` with the socket and binary already resolved (spec D1).

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ctx::{Ctx, Env};
use crate::error::{Refusal, Res, refuse};
use crate::state::root::OsKind;

/// One finished multiplexer call.
pub type Output = crate::spawn::Out;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Rmux,
    Tmux,
}

pub trait Mux {
    fn run(&self, args: &[&str]) -> Res<Output>;
    fn kind(&self) -> Kind;
}

/// The caller's server: its socket (or pipe) and pid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub socket: String,
    pub server_pid: u32,
}

const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const CALL_TIMEOUT: Duration = Duration::from_secs(15);
const PIPE_PREFIX: &str = r"\\.\pipe\";

fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// `RMUX` if set, else `TMUX`: `<socket>,<server pid>,<n>` with no CR or LF anywhere. Unix sockets are absolute
/// paths (Unix.ps1:5-6, n may be negative); Windows endpoints are `\\.\pipe\` names, any case (MuxAgents.ps1:194-198).
pub fn endpoint(env: &Env, os: OsKind) -> Option<Endpoint> {
    // An empty variable counts as unset, as PowerShell's truthiness test did.
    let value = env
        .get("RMUX")
        .filter(|v| !v.is_empty())
        .or_else(|| env.get("TMUX"))?;
    let mut parts = value.rsplitn(3, ',');
    let (n, pid, socket) = (parts.next()?, parts.next()?, parts.next()?);
    let n_ok = match os {
        OsKind::Unix => digits(n.strip_prefix('-').unwrap_or(n)),
        OsKind::Windows => digits(n),
    };
    if !n_ok || !digits(pid) || socket.contains([',', '\r', '\n']) {
        return None;
    }
    let named = match os {
        OsKind::Unix => socket.len() > 1 && socket.starts_with('/'),
        OsKind::Windows => {
            socket.len() > PIPE_PREFIX.len()
                && socket.as_bytes()[..PIPE_PREFIX.len()]
                    .eq_ignore_ascii_case(PIPE_PREFIX.as_bytes())
        }
    };
    if !named {
        return None;
    }
    Some(Endpoint {
        socket: socket.to_string(),
        server_pid: pid.parse().ok()?,
    })
}

fn file_name(kind: Kind) -> &'static str {
    match (kind, cfg!(windows)) {
        (Kind::Rmux, false) => "rmux",
        (Kind::Tmux, false) => "tmux",
        (Kind::Rmux, true) => "rmux.exe",
        (Kind::Tmux, true) => "tmux.exe",
    }
}

/// The multiplexer a server image is by its file name: exact on Unix, `.exe` and any case on Windows.
pub fn image_kind(path: &str, os: OsKind) -> Option<Kind> {
    let (file, names) = match os {
        OsKind::Unix => (path.rsplit('/').next()?, ["rmux", "tmux"]),
        OsKind::Windows => (path.rsplit(['/', '\\']).next()?, ["rmux.exe", "tmux.exe"]),
    };
    let same = |name: &str| match os {
        OsKind::Unix => file == name,
        OsKind::Windows => file.eq_ignore_ascii_case(name),
    };
    [Kind::Rmux, Kind::Tmux]
        .into_iter()
        .zip(names)
        .find_map(|(kind, name)| same(name).then_some(kind))
}

/// The first `name` in an absolute PATH entry that is an executable file. Relative and empty entries are skipped:
/// they would resolve against the caller's cwd.
pub fn find_on_path(name: &str, path_var: &str) -> Option<PathBuf> {
    std::env::split_paths(path_var)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| executable(candidate))
}

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
fn executable(path: &Path) -> bool {
    path.is_file()
}

/// The server's image when it is named rmux or tmux, else rmux on PATH when RMUX is set, else tmux on PATH.
pub fn binary(
    server_image: Option<&str>,
    rmux_set: bool,
    path_var: Option<&str>,
) -> Res<(PathBuf, Kind)> {
    if let Some(image) = server_image
        && let Some(kind) = image_kind(image, crate::state::root::HOST)
    {
        return Ok((PathBuf::from(image), kind));
    }
    let kind = if rmux_set { Kind::Rmux } else { Kind::Tmux };
    let name = file_name(kind);
    match path_var.and_then(|path| find_on_path(name, path)) {
        Some(found) => Ok((found, kind)),
        None => refuse(1, "endpoint", format!("{name} was not found")),
    }
}

/// `<kind> -V` must exit 0 and read `tmux [next-]X.Y` at least 3.2 (older has no -N; Unix.ps1:359-361) or
/// `rmux X.Y` at least 0.10. No answer, another answer or an older one refuses.
pub fn gate(kind: Kind, version: Option<&Output>) -> Res<()> {
    let (prefix, minimum, message) = match kind {
        Kind::Tmux => ("tmux ", (3, 2), "tmux 3.2 or later is required"),
        Kind::Rmux => ("rmux ", (0, 10), "rmux 0.10 or later is required"),
    };
    let read = version.filter(|v| v.code == 0).and_then(|v| {
        let rest = v.stdout.trim().strip_prefix(prefix)?;
        let rest = match kind {
            Kind::Tmux => rest.strip_prefix("next-").unwrap_or(rest),
            Kind::Rmux => rest,
        };
        let (major, rest) = rest.split_once('.')?;
        let minor = &rest[..rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len())];
        if !digits(major) || !digits(minor) {
            return None;
        }
        Some((major.parse::<u64>().ok()?, minor.parse::<u64>().ok()?))
    });
    match read {
        Some(found) if found >= minimum => Ok(()),
        _ => refuse(1, "endpoint", message),
    }
}

/// The caller's own server through its own binary.
pub struct RealMux {
    bin: PathBuf,
    socket: String,
    kind: Kind,
    timeout: Duration,
}

impl RealMux {
    /// The endpoint, then the binary (one process row, not the table), then its version gate.
    pub fn connect(ctx: &Ctx) -> Res<RealMux> {
        let Some(endpoint) = endpoint(&ctx.env, crate::state::root::HOST) else {
            return refuse(1, "endpoint", "not inside an rmux pane");
        };
        let image = ctx.os.row(endpoint.server_pid).and_then(|row| row.path);
        let rmux_set = ctx.env.get("RMUX").is_some_and(|v| !v.is_empty());
        let (bin, kind) = binary(image.as_deref(), rmux_set, ctx.env.get("PATH"))?;
        let version = crate::spawn::captured(&bin, &["-V"], VERSION_TIMEOUT).ok();
        gate(kind, version.as_ref())?;
        Ok(RealMux {
            bin,
            socket: endpoint.socket,
            kind,
            timeout: CALL_TIMEOUT,
        })
    }
}

impl Mux for RealMux {
    /// `<bin> -S <socket> <args>`. A call without a leading -N refuses before any process starts, so nothing here
    /// can start a server (MuxAgents.ps1:290-301).
    fn run(&self, args: &[&str]) -> Res<Output> {
        if args.first() != Some(&"-N") {
            // A bug, not a caller's mistake: the reference threw InvalidOperationException here.
            return Err(Refusal::internal("every rmux call must start with -N"));
        }
        let mut argv = vec!["-S", self.socket.as_str()];
        argv.extend_from_slice(args);
        crate::spawn::captured(&self.bin, &argv, self.timeout)
            .or_else(|_| refuse(1, "endpoint", "the rmux endpoint did not answer"))
    }

    fn kind(&self) -> Kind {
        self.kind
    }
}

/// The only multiplexer path (MuxAgents.ps1:323-329): -N first, the multiplexer resolved on first use; a failed call
/// refuses without echoing its stderr unless `allow_failure`.
pub fn native(ctx: &Ctx, args: &[&str], allow_failure: bool) -> Res<Output> {
    if ctx.mux.borrow().is_none() {
        let mux = RealMux::connect(ctx)?;
        *ctx.mux.borrow_mut() = Some(Box::new(mux));
    }
    let mut argv = vec!["-N"];
    argv.extend_from_slice(args);
    let mux = ctx.mux.borrow();
    let out = mux.as_ref().expect("connected above").run(&argv)?;
    if out.code != 0 && !allow_failure {
        return refuse(1, "rmux", "rmux call failed");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Fakes;

    fn refusal<T: std::fmt::Debug>(result: Res<T>) -> (u8, &'static str, String) {
        let refusal = result.unwrap_err();
        (refusal.code, refusal.check, refusal.message)
    }

    fn parsed(pairs: &[(&str, &str)], os: OsKind) -> Option<(String, u32)> {
        endpoint(&Env::from_pairs(pairs.iter().copied()), os).map(|e| (e.socket, e.server_pid))
    }

    fn some(socket: &str, pid: u32) -> Option<(String, u32)> {
        Some((socket.to_string(), pid))
    }

    #[test]
    fn endpoint_rmux_before_tmux() {
        // rmux 0.10 on macOS sets both to the same value; RMUX wins when both are set and differ.
        let both = [
            ("TMUX", "/tmp/tmux-501/default,7,0"),
            ("RMUX", "/private/tmp/rmux-501/main,123,0"),
        ];
        assert_eq!(
            parsed(&both, OsKind::Unix),
            some("/private/tmp/rmux-501/main", 123)
        );
        let empty_rmux = [("RMUX", ""), ("TMUX", "/tmp/tmux-501/default,7,0")];
        assert_eq!(
            parsed(&empty_rmux, OsKind::Unix),
            some("/tmp/tmux-501/default", 7)
        );
        // A bad RMUX does not fall back to TMUX.
        let bad_rmux = [("RMUX", "relative,1,0"), ("TMUX", "/tmp/t,7,0")];
        assert_eq!(parsed(&bad_rmux, OsKind::Unix), None);
        let windows = [
            ("RMUX", "\\\\.\\pipe\\a,0900,1"),
            ("TMUX", "\\\\.\\pipe\\b,7,0"),
        ];
        assert_eq!(
            parsed(&windows, OsKind::Windows),
            some("\\\\.\\pipe\\a", 900)
        );
        assert_eq!(parsed(&[], OsKind::Unix), None);
    }

    #[test]
    fn unix_endpoint_needs_absolute_socket() {
        let one = |value: &str| parsed(&[("TMUX", value)], OsKind::Unix);
        assert_eq!(
            one("/tmp/tmux-1000/default,0900,-1"),
            some("/tmp/tmux-1000/default", 900)
        );
        assert_eq!(one("/tmp/my socket,7,0"), some("/tmp/my socket", 7));
        for bad in [
            "\\\\.\\pipe\\rmux,7,0",
            "tmp/x,7,0",
            "/,7,0",
            "/a,b,7,0",
            "/a\n/b,7,0",
            "/a\r/b,7,0",
            "/tmp/t,7,0\r",
            "/tmp/t,7,0\n",
            "/a,7,x",
            "/a,-7,0",
            "/a,,0",
            "/a,7,",
            "/a,7",
            "/a,99999999999,0",
            "",
        ] {
            assert_eq!(one(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn windows_endpoint_needs_pipe_name() {
        let one = |value: &str| parsed(&[("RMUX", value)], OsKind::Windows);
        assert_eq!(
            one("\\\\.\\pipe\\rmux-me,7,0"),
            some("\\\\.\\pipe\\rmux-me", 7)
        );
        // PowerShell's -match ignores case.
        assert_eq!(one("\\\\.\\PIPE\\x,7,0"), some("\\\\.\\PIPE\\x", 7));
        for bad in [
            "/tmp/tmux-1000/default,7,0",
            "\\\\.\\pipe\\a,7,-1",
            "\\\\.\\pipe\\,7,0",
            "\\\\.\\pipes\\a,7,0",
            "C:\\pipe\\a,7,0",
            "\\\\.\\pipe\\a\nb,7,0",
            "\\\\.\\pipe\\a,7,0\n",
            "\\\\.\\pipe\\a,b,7,0",
        ] {
            assert_eq!(one(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn binary_from_server_image_named_rmux_or_tmux() {
        use Kind::{Rmux, Tmux};
        let unix: Vec<_> = [
            "/opt/rmux/bin/rmux",
            "/usr/bin/tmux",
            "/usr/bin/Tmux",
            "/usr/bin/tmux.exe",
            "/usr/bin/bash",
            "",
        ]
        .iter()
        .map(|p| image_kind(p, OsKind::Unix))
        .collect();
        assert_eq!(unix, [Some(Rmux), Some(Tmux), None, None, None, None]);
        let windows: Vec<_> = [
            "C:\\rmux\\rmux.exe",
            "C:\\bin\\TMUX.EXE",
            "/srv/rmux.exe",
            "C:\\bin\\rmux",
            "C:\\bin\\pwsh.exe",
        ]
        .iter()
        .map(|p| image_kind(p, OsKind::Windows))
        .collect();
        assert_eq!(windows, [Some(Rmux), Some(Tmux), Some(Rmux), None, None]);
        // A server image named for either wins over PATH and over which variable is set.
        let image = if cfg!(windows) {
            "C:\\srv\\tmux.exe"
        } else {
            "/srv/tmux"
        };
        assert_eq!(
            binary(Some(image), true, None),
            Ok((PathBuf::from(image), Tmux))
        );
    }

    #[cfg(unix)]
    fn tool(dir: &Path, name: &str, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn binary_rmux_on_path_when_rmux_set_else_tmux() {
        let (a, b) = (
            crate::testing::TempDir::new(),
            crate::testing::TempDir::new(),
        );
        let rmux = tool(a.path(), "rmux", "");
        let tmux = tool(b.path(), "tmux", "");
        tool(b.path(), "rmux", "");
        // Not executable: skipped.
        std::fs::write(a.path().join("tmux"), "").unwrap();
        let path = format!("{}:{}", a.path().display(), b.path().display());
        assert_eq!(binary(None, true, Some(&path)), Ok((rmux, Kind::Rmux)));
        assert_eq!(
            binary(Some("/usr/bin/bash"), false, Some(&path)),
            Ok((tmux, Kind::Tmux))
        );
        let only_tmux = b.path().display().to_string();
        std::fs::remove_file(b.path().join("rmux")).unwrap();
        assert_eq!(
            refusal(binary(None, true, Some(&only_tmux))),
            (1, "endpoint", "rmux was not found".into())
        );
        assert_eq!(
            refusal(binary(None, false, None)),
            (1, "endpoint", "tmux was not found".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn path_lookup_skips_relative_entries() {
        let dir = crate::testing::TempDir::new();
        let tmux = tool(dir.path(), "tmux", "");
        // The same folder by a relative path from the test's cwd.
        let cwd = std::env::current_dir().unwrap();
        let up = "../".repeat(cwd.components().count() - 1);
        let relative = format!(
            "{up}{}",
            dir.path().display().to_string().trim_start_matches('/')
        );
        assert!(Path::new(&relative).join("tmux").is_file());
        assert_eq!(find_on_path("tmux", &format!("{relative}::.")), None);
        assert_eq!(
            find_on_path("tmux", &format!("{relative}:{}", dir.path().display())),
            Some(tmux)
        );
    }

    fn version(code: i32, stdout: &str) -> Output {
        Output {
            code,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    const OLD_TMUX: &str = "tmux 3.2 or later is required";
    const OLD_RMUX: &str = "rmux 0.10 or later is required";

    #[test]
    fn gate_tmux_3_2_ok_3_1_refuses_endpoint() {
        for ok in [
            "tmux 3.2\n",
            "tmux 3.2a",
            "tmux 3.8",
            "tmux 3.10",
            "tmux next-3.6",
            "tmux 4.0",
            "  tmux 3.4  ",
        ] {
            assert_eq!(gate(Kind::Tmux, Some(&version(0, ok))), Ok(()), "{ok:?}");
        }
        for old in ["tmux 3.1c", "tmux 3.1", "tmux 2.9a", "tmux next-3.1"] {
            assert_eq!(
                refusal(gate(Kind::Tmux, Some(&version(0, old)))),
                (1, "endpoint", OLD_TMUX.into()),
                "{old:?}"
            );
        }
    }

    #[test]
    fn gate_rmux_0_10_ok_0_9_refuses() {
        for ok in ["rmux 0.10.0", "rmux 0.10.0\n", "rmux 0.11.2", "rmux 1.0.0"] {
            assert_eq!(gate(Kind::Rmux, Some(&version(0, ok))), Ok(()), "{ok:?}");
        }
        for old in ["rmux 0.9.9", "rmux 0.1.0"] {
            assert_eq!(
                refusal(gate(Kind::Rmux, Some(&version(0, old)))),
                (1, "endpoint", OLD_RMUX.into()),
                "{old:?}"
            );
        }
    }

    #[test]
    fn gate_unreadable_version_refuses() {
        for (kind, old) in [(Kind::Tmux, OLD_TMUX), (Kind::Rmux, OLD_RMUX)] {
            // A timeout or a binary that cannot start gives no output at all.
            assert_eq!(refusal(gate(kind, None)), (1, "endpoint", old.into()));
            for (code, text) in [
                (0, ""),
                (0, "tmux master"),
                (0, "tmux 3."),
                (0, "tmux x.y"),
                (0, "version tmux 3.4"),
                (0, "rmux"),
                (1, "tmux 3.4"),
                (1, "rmux 0.10.0"),
            ] {
                assert_eq!(
                    refusal(gate(kind, Some(&version(code, text)))),
                    (1, "endpoint", old.into()),
                    "{kind:?} {code} {text:?}"
                );
            }
        }
        // The other multiplexer's answer is not this one's version.
        assert!(gate(Kind::Tmux, Some(&version(0, "rmux 3.4"))).is_err());
        assert!(gate(Kind::Rmux, Some(&version(0, "tmux 3.8"))).is_err());
    }

    // A ctx whose server pid 900 runs `image` (none: no such row) with RMUX or TMUX set.
    #[cfg(unix)]
    fn ctx_for(fakes: &Fakes, image: Option<&Path>, var: &str, path: &Path) -> Ctx {
        let mut row = crate::testing::row(900, 1, 9);
        row.path = image.map(|p| p.display().to_string());
        fakes.process(row);
        let mut ctx = fakes.ctx();
        ctx.env = Env::from_pairs([
            (var, "/tmp/s,900,0".to_string()),
            ("PATH", path.display().to_string()),
        ]);
        *ctx.mux.borrow_mut() = None;
        ctx
    }

    // A "tmux" or "rmux" that answers -V with `version`, logs each other call's argv and echoes it; `fail` exits 3.
    #[cfg(unix)]
    fn fake_mux(dir: &Path, name: &str, version: &str) -> PathBuf {
        let log = dir.join("calls");
        let script = format!(
            "if [ \"$1\" = -V ]; then echo '{version}'; exit 0; fi\n\
             echo \"$*\" >> '{log}'\n\
             case \"$*\" in *fail*) exit 3;; *slow*) sleep 5;; esac\n\
             echo \"$*\"",
            log = log.display()
        );
        tool(dir, name, &script)
    }

    #[cfg(unix)]
    fn calls(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("calls")).unwrap_or_default()
    }

    #[cfg(unix)]
    #[test]
    fn real_mux_prefixes_s_socket_n() {
        let dir = crate::testing::TempDir::new();
        let bin = fake_mux(dir.path(), "tmux", "tmux 3.4");
        let fakes = Fakes::default();
        let ctx = ctx_for(&fakes, Some(&bin), "TMUX", Path::new("/nonexistent"));
        let mux = RealMux::connect(&ctx).unwrap();
        let out = mux.run(&["-N", "list-panes", "-a"]).unwrap();
        assert_eq!(
            (out.code, out.stdout.as_str()),
            (0, "-S /tmp/s -N list-panes -a\n")
        );
        // Without a leading -N nothing starts.
        for bad in [&["list-panes"][..], &[], &["-S", "x", "-N"]] {
            assert_eq!(refusal(mux.run(bad)).1, "internal");
        }
        assert_eq!(calls(dir.path()), "-S /tmp/s -N list-panes -a\n");

        // native connects on first use, adds -N and refuses a failed call without echoing it.
        let listed = native(&ctx, &["display-message", "-p", "x"], false).unwrap();
        assert_eq!(listed.stdout, "-S /tmp/s -N display-message -p x\n");
        assert_eq!(
            refusal(native(&ctx, &["fail"], false)),
            (1, "rmux", "rmux call failed".into())
        );
        assert_eq!(native(&ctx, &["fail"], true).unwrap().code, 3);
        assert!(ctx.mux.borrow().is_some());
    }

    #[test]
    fn native_on_a_fake_mux_records_n_first() {
        let fakes = Fakes::default();
        fakes.mux_rule("kill-pane", 1, "", "SECRET");
        let ctx = fakes.ctx();
        let refused = refusal(native(&ctx, &["kill-pane", "-t", "%9"], false));
        assert_eq!(refused, (1, "rmux", "rmux call failed".into()));
        assert_eq!(
            native(&ctx, &["kill-pane", "-t", "%9"], true)
                .unwrap()
                .stderr,
            "SECRET"
        );
        assert_eq!(fakes.mux_calls()[0], ["-N", "kill-pane", "-t", "%9"]);
    }

    #[test]
    fn native_outside_a_pane_refuses_endpoint() {
        let fakes = Fakes::default();
        let ctx = fakes.ctx();
        *ctx.mux.borrow_mut() = None;
        assert_eq!(
            refusal(native(&ctx, &["list-panes"], false)),
            (1, "endpoint", "not inside an rmux pane".into())
        );
        assert!(fakes.os_calls().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn timeout_refuses_endpoint_did_not_answer() {
        let dir = crate::testing::TempDir::new();
        let bin = fake_mux(dir.path(), "tmux", "tmux 3.4");
        let fakes = Fakes::default();
        let ctx = ctx_for(&fakes, Some(&bin), "TMUX", Path::new("/nonexistent"));
        let mut mux = RealMux::connect(&ctx).unwrap();
        mux.timeout = Duration::from_millis(200);
        let did_not_answer = (
            1,
            "endpoint",
            "the rmux endpoint did not answer".to_string(),
        );
        assert_eq!(refusal(mux.run(&["-N", "slow"])), did_not_answer);
        // A binary that is gone by the call answers nothing either.
        std::fs::remove_file(&bin).unwrap();
        assert_eq!(refusal(mux.run(&["-N", "list-panes"])), did_not_answer);
    }

    #[cfg(unix)]
    #[test]
    fn kind_is_rmux_or_tmux() {
        let dir = crate::testing::TempDir::new();
        let rmux = fake_mux(dir.path(), "rmux", "rmux 0.10.0");
        let tmux = fake_mux(dir.path(), "tmux", "tmux 3.8");
        let kind = |image: Option<&Path>, var: &str| {
            let fakes = Fakes::default();
            RealMux::connect(&ctx_for(&fakes, image, var, dir.path())).map(|m| m.kind())
        };
        assert_eq!(kind(Some(&rmux), "TMUX"), Ok(Kind::Rmux));
        assert_eq!(kind(Some(&tmux), "RMUX"), Ok(Kind::Tmux));
        // No server row: RMUX picks rmux on PATH, TMUX tmux.
        assert_eq!(kind(None, "RMUX"), Ok(Kind::Rmux));
        assert_eq!(kind(None, "TMUX"), Ok(Kind::Tmux));
        // A tmux binary that answers as rmux is refused.
        let liar = crate::testing::TempDir::new();
        let fake = fake_mux(liar.path(), "tmux", "rmux 0.10.0");
        assert_eq!(
            kind(Some(&fake), "TMUX"),
            Err(Refusal {
                code: 1,
                check: "endpoint",
                message: OLD_TMUX.into()
            })
        );
        assert_eq!(Fakes::default().kind(), Kind::Tmux);
    }
}
