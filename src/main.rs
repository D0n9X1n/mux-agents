//! mux-agent entry: one verb per call, one JSON object (or -Text) on stdout, exit 0 ok, 1 error, 2 refused or usage.

use std::cell::RefCell;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use mux_agent::cli;
use mux_agent::ctx::{Ctx, Env, SystemClock};
use mux_agent::error::{Refusal, Res, refuse};
use mux_agent::state::root::{HOST, root};

// Console.Out.WriteLine's line end, so callers see the bytes the PowerShell entry wrote.
const NL: &str = if cfg!(windows) { "\r\n" } else { "\n" };

/// PowerShell's $HOME: HOME on Unix, else the passwd entry; the user profile on Windows. A relative answer would
/// resolve against the cwd, where a planted transcript could pass C2, so it refuses.
fn home(env: &Env) -> Res<PathBuf> {
    let set = env.get("HOME").filter(|h| !h.is_empty() && cfg!(unix));
    let home = set.map(PathBuf::from).or_else(std::env::home_dir);
    match home {
        Some(home) if home.is_absolute() => Ok(home),
        _ => refuse(1, "state", "HOME is not an absolute path"),
    }
}

/// This binary's resolved path, the one panes run (spec D4). Windows drops the `\\?\` canonicalize adds: a pane's
/// command line and the spawn header's entry check take a plain drive path.
fn exe() -> Res<PathBuf> {
    let real = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|_| Refusal::internal("the binary path is unreadable"))?;
    #[cfg(windows)]
    if let Some(plain) = real.to_str().and_then(|r| r.strip_prefix(r"\\?\"))
        && !plain.starts_with(r"UNC\")
    {
        return Ok(PathBuf::from(plain));
    }
    Ok(real)
}

// The providers, built before the arguments are parsed, as the reference entry did. The multiplexer connects on
// first use.
fn ctx() -> Res<Ctx> {
    let env = Env::from_os(std::env::vars_os());
    let state_root = root(&env, HOST)?;
    Ok(Ctx {
        pid: std::process::id(),
        cwd: std::env::current_dir().unwrap_or_default(),
        home: home(&env)?,
        state_root,
        exe: exe()?,
        os: mux_agent::proc::host(),
        mux: RefCell::new(None),
        clock: Box::new(SystemClock),
        admitted: false,
        tty: std::io::IsTerminal::is_terminal(&std::io::stdout()),
        env,
    })
}

fn main() -> ExitCode {
    // Silent: a panic's payload or backtrace could carry task text or paths; cli turns it into `internal`.
    std::panic::set_hook(Box::new(|_| {}));
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if let Some(spec) = cli::launcher_spec(&args) {
        let (code, reply) = cli::run_launcher(spec, ctx);
        let mut out = std::io::stdout().lock();
        if let Some(reply) = reply {
            let _ = write!(out, "{reply}{NL}");
        }
        let _ = out.flush();
        // The client's own exit code, which may not fit a u8 on Windows.
        std::process::exit(code);
    }
    let (code, text) = cli::run(&args, ctx);
    // A closed stdout leaves only the exit code to report with.
    let _ = write!(std::io::stdout().lock(), "{text}{NL}");
    ExitCode::from(code)
}
