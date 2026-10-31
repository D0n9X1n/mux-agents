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
        RealMux::connect_within(ctx, VERSION_TIMEOUT)
    }

    /// `connect` with `-V` bounded by `version_timeout`; a `-V` that fails to start or times out is no version.
    fn connect_within(ctx: &Ctx, version_timeout: Duration) -> Res<RealMux> {
        let Some(endpoint) = endpoint(&ctx.env, crate::state::root::HOST) else {
            return refuse(1, "endpoint", "not inside an rmux pane");
        };
        let image = ctx.os.row(endpoint.server_pid).and_then(|row| row.path);
        let rmux_set = ctx.env.get("RMUX").is_some_and(|v| !v.is_empty());
        let (bin, kind) = binary(image.as_deref(), rmux_set, ctx.env.get("PATH"))?;
        let version = crate::spawn::captured(&bin, &["-V"], version_timeout).ok();
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
    use crate::testing::{Fakes, TempDir};
    use std::time::Instant;

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

    const EXE: &str = if cfg!(windows) { ".exe" } else { "" };

    // An empty file named `name` (`name.exe` on Windows), executable on Unix.
    fn tool_file(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(format!("{name}{EXE}"));
        std::fs::write(&path, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    // A PATH value in the host's syntax: `:` apart on Unix, `;` on Windows.
    fn path_of(dirs: &[&Path]) -> String {
        std::env::join_paths(dirs).unwrap().into_string().unwrap()
    }

    #[test]
    fn binary_rmux_on_path_when_rmux_set_else_tmux() {
        let (a, b) = (TempDir::new(), TempDir::new());
        let rmux = tool_file(a.path(), "rmux");
        let tmux = tool_file(b.path(), "tmux");
        tool_file(b.path(), "rmux");
        let path = path_of(&[a.path(), b.path()]);
        assert_eq!(binary(None, true, Some(&path)), Ok((rmux, Kind::Rmux)));
        let other = if cfg!(windows) {
            "C:\\bin\\bash.exe"
        } else {
            "/usr/bin/bash"
        };
        assert_eq!(
            binary(Some(other), false, Some(&path)),
            Ok((tmux, Kind::Tmux))
        );
        std::fs::remove_file(b.path().join(format!("rmux{EXE}"))).unwrap();
        let only_tmux = path_of(&[b.path()]);
        assert_eq!(
            refusal(binary(None, true, Some(&only_tmux))),
            (1, "endpoint", format!("rmux{EXE} was not found"))
        );
        assert_eq!(
            refusal(binary(None, false, None)),
            (1, "endpoint", format!("tmux{EXE} was not found"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn path_lookup_skips_files_that_are_not_executable() {
        let (a, b) = (TempDir::new(), TempDir::new());
        std::fs::write(a.path().join("tmux"), "").unwrap();
        let tmux = tool_file(b.path(), "tmux");
        let path = path_of(&[a.path(), b.path()]);
        assert_eq!(find_on_path("tmux", &path), Some(tmux));
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_takes_only_the_exe_name() {
        let (a, b) = (TempDir::new(), TempDir::new());
        std::fs::write(a.path().join("tmux"), "").unwrap();
        std::fs::write(a.path().join("tmux.cmd"), "").unwrap();
        let tmux = tool_file(b.path(), "tmux");
        let path = path_of(&[a.path(), b.path()]);
        assert!(path.contains(';'), "{path}");
        assert_eq!(binary(None, false, Some(&path)), Ok((tmux, Kind::Tmux)));
    }

    // `target` relative to the cwd; none when they share no drive and root (Windows).
    fn relative_to_cwd(target: &Path) -> Option<PathBuf> {
        use std::path::Component;
        let cwd = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
        let cwd: Vec<Component> = cwd.components().collect();
        let target: Vec<Component> = target.components().collect();
        let common = cwd.iter().zip(&target).take_while(|(a, b)| a == b).count();
        let lead = target
            .iter()
            .take_while(|c| !matches!(c, Component::Normal(_)))
            .count();
        if common < lead {
            return None;
        }
        let mut relative = PathBuf::new();
        for _ in common..cwd.len() {
            relative.push("..");
        }
        for component in &target[common..] {
            relative.push(component);
        }
        Some(relative)
    }

    #[test]
    fn path_lookup_skips_relative_entries() {
        let dir = TempDir::new();
        let tmux = tool_file(dir.path(), "tmux");
        let name = format!("tmux{EXE}");
        let Some(relative) = relative_to_cwd(dir.path()) else {
            eprintln!("skipped: the temp folder is on another drive than the cwd");
            return;
        };
        // The same folder by a relative path from the test's cwd.
        assert!(relative.is_relative() && relative.join(&name).is_file());
        let skipped = path_of(&[&relative, Path::new(""), Path::new(".")]);
        assert_eq!(find_on_path(&name, &skipped), None);
        let found = path_of(&[&relative, dir.path()]);
        assert_eq!(find_on_path(&name, &found), Some(tmux));
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

    // The caller's socket, with a space in it, so an argument split at whitespace shows.
    const SOCKET: &str = if cfg!(windows) {
        r"\\.\pipe\my mux"
    } else {
        "/tmp/my mux"
    };

    // A ctx whose server pid 900 runs `image` (none: no such row) with RMUX or TMUX set.
    fn ctx_for(fakes: &Fakes, image: Option<&Path>, var: &str, path: &str) -> Ctx {
        let mut row = crate::testing::row(900, 1, 9);
        row.path = image.map(|p| p.display().to_string());
        fakes.process(row);
        let mut ctx = fakes.ctx();
        ctx.env = Env::from_pairs([(var, format!("{SOCKET},900,0")), ("PATH", path.to_string())]);
        *ctx.mux.borrow_mut() = None;
        ctx
    }

    // The stand-in for rmux and tmux, one program on every host. `-V` answers the `<stem>.version` file beside it
    // ("fail" exits 1, "hang" sleeps 30 s); any other call appends its argv to `calls` beside it, each argument
    // NUL-terminated and the call ended by a newline, echoes the same to stdout, and exits 3 when an argument holds
    // "fail" or sleeps 5 s first when one holds "slow".
    const FAKE_MUX: &str = r#"
use std::io::Write;
use std::time::Duration;
fn main() {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("-V") {
        let stem = exe.file_stem().unwrap().to_str().unwrap();
        let version = std::fs::read_to_string(dir.join(format!("{stem}.version"))).unwrap_or_default();
        match version.trim() {
            "fail" => {
                println!("tmux 3.4");
                std::process::exit(1);
            }
            "hang" => std::thread::sleep(Duration::from_secs(30)),
            version => println!("{version}"),
        }
        return;
    }
    let mut call = String::new();
    for arg in &args {
        call.push_str(arg);
        call.push('\0');
    }
    call.push('\n');
    let mut log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("calls")).unwrap();
    log.write_all(call.as_bytes()).unwrap();
    if args.iter().any(|a| a.contains("fail")) {
        std::process::exit(3);
    }
    if args.iter().any(|a| a.contains("slow")) {
        std::thread::sleep(Duration::from_secs(5));
    }
    std::io::stdout().write_all(call.as_bytes()).unwrap();
}
"#;

    // Built once per source with this toolchain's rustc, beside the test binary.
    fn fake_mux_program() -> &'static Path {
        static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        BUILT.get_or_init(|| {
            let deps = std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .to_path_buf();
            let hash = &crate::util::sha256_hex(FAKE_MUX)[..16];
            let built = deps.join(format!("mux-agent-fake-mux-{hash}{EXE}"));
            if built.is_file() {
                return built;
            }
            let work = deps.join(format!("mux-agent-fake-mux-{hash}-{}", std::process::id()));
            std::fs::create_dir_all(&work).unwrap();
            let source = work.join("fake.rs");
            std::fs::write(&source, FAKE_MUX).unwrap();
            let out = work.join(format!("fake{EXE}"));
            let rustc = Path::new(env!("CARGO")).with_file_name(format!("rustc{EXE}"));
            let rustc = if rustc.is_file() {
                rustc
            } else {
                PathBuf::from("rustc")
            };
            let status = std::process::Command::new(rustc)
                .args(["--edition=2021", "-o"])
                .arg(&out)
                .arg(&source)
                .status()
                .unwrap();
            assert!(status.success(), "the fake mux did not build");
            // Another test process may have won the race; both built the same program.
            let _ = std::fs::rename(&out, &built);
            let _ = std::fs::remove_dir_all(&work);
            assert!(built.is_file());
            built
        })
    }

    fn fake_mux(dir: &Path, name: &str, version: &str) -> PathBuf {
        let path = dir.join(format!("{name}{EXE}"));
        std::fs::copy(fake_mux_program(), &path).unwrap();
        std::fs::write(dir.join(format!("{name}.version")), version).unwrap();
        path
    }

    // Calls as the fake records them, one argv each.
    fn split(text: &str) -> Vec<Vec<String>> {
        text.lines()
            .map(|line| line.split_terminator('\0').map(String::from).collect())
            .collect()
    }

    fn calls(dir: &Path) -> Vec<Vec<String>> {
        split(&std::fs::read_to_string(dir.join("calls")).unwrap_or_default())
    }

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn real_mux_prefixes_s_socket_n() {
        let dir = TempDir::new();
        let bin = fake_mux(dir.path(), "tmux", "tmux 3.4");
        let fakes = Fakes::default();
        let ctx = ctx_for(&fakes, Some(&bin), "TMUX", "");
        let mux = RealMux::connect(&ctx).unwrap();
        // Each argument arrives whole, spaces and all.
        let out = mux
            .run(&["-N", "send-keys", "-t", "%1", "echo a b"])
            .unwrap();
        let sent = argv(&["-S", SOCKET, "-N", "send-keys", "-t", "%1", "echo a b"]);
        assert_eq!(out.code, 0);
        assert_eq!(split(&out.stdout), std::slice::from_ref(&sent));
        // Without a leading -N nothing starts.
        for bad in [&["list-panes"][..], &[], &["-S", "x", "-N"]] {
            assert_eq!(refusal(mux.run(bad)).1, "internal");
        }
        assert_eq!(calls(dir.path()), [sent]);

        // native connects on first use, adds -N and refuses a failed call without echoing it.
        let listed = native(&ctx, &["display-message", "-p", "x"], false).unwrap();
        assert_eq!(
            split(&listed.stdout),
            [argv(&["-S", SOCKET, "-N", "display-message", "-p", "x"])]
        );
        assert_eq!(
            refusal(native(&ctx, &["fail"], false)),
            (1, "rmux", "rmux call failed".into())
        );
        assert_eq!(native(&ctx, &["fail"], true).unwrap().code, 3);
        assert!(ctx.mux.borrow().is_some());
    }

    #[test]
    fn failed_or_timed_out_version_refuses_endpoint() {
        let dir = TempDir::new();
        let fakes = Fakes::default();
        let required = (1, "endpoint", OLD_TMUX.to_string());
        let bin = fake_mux(dir.path(), "tmux", "fail");
        let ctx = ctx_for(&fakes, Some(&bin), "TMUX", "");
        let kind = |within: Duration| RealMux::connect_within(&ctx, within).map(|m| m.kind());
        // -V exits 1, though its text would pass.
        assert_eq!(refusal(kind(VERSION_TIMEOUT)), required);
        // -V past its timeout is killed and refused, not a panic.
        std::fs::write(dir.path().join("tmux.version"), "hang").unwrap();
        let started = Instant::now();
        assert_eq!(refusal(kind(Duration::from_millis(500))), required);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        // A server image that is gone cannot start.
        std::fs::remove_file(&bin).unwrap();
        assert_eq!(refusal(kind(VERSION_TIMEOUT)), required);
        assert!(calls(dir.path()).is_empty());
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

    #[test]
    fn timeout_refuses_endpoint_did_not_answer() {
        let dir = TempDir::new();
        let bin = fake_mux(dir.path(), "tmux", "tmux 3.4");
        let fakes = Fakes::default();
        let ctx = ctx_for(&fakes, Some(&bin), "TMUX", "");
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

    #[test]
    fn kind_is_rmux_or_tmux() {
        let dir = TempDir::new();
        let rmux = fake_mux(dir.path(), "rmux", "rmux 0.10.0");
        let tmux = fake_mux(dir.path(), "tmux", "tmux 3.8");
        let path = path_of(&[dir.path()]);
        let kind = |image: Option<&Path>, var: &str| {
            let fakes = Fakes::default();
            RealMux::connect(&ctx_for(&fakes, image, var, &path)).map(|m| m.kind())
        };
        assert_eq!(kind(Some(&rmux), "TMUX"), Ok(Kind::Rmux));
        assert_eq!(kind(Some(&tmux), "RMUX"), Ok(Kind::Tmux));
        // No server row: RMUX picks rmux on PATH, TMUX tmux.
        assert_eq!(kind(None, "RMUX"), Ok(Kind::Rmux));
        assert_eq!(kind(None, "TMUX"), Ok(Kind::Tmux));
        // A tmux binary that answers as rmux is refused.
        let liar = TempDir::new();
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
