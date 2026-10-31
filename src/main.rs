//! mux-agent entry: one verb per call, one JSON object (or -Text) on stdout, exit 0 ok, 1 error, 2 refused or usage.

use std::io::Write;
use std::process::ExitCode;

use mux_agent::cli;
use mux_agent::ctx::Env;

// Console.Out.WriteLine's line end, so callers see the bytes the PowerShell entry wrote.
const NL: &str = if cfg!(windows) { "\r\n" } else { "\n" };

fn main() -> ExitCode {
    // Silent: a panic's payload or backtrace could carry task text or paths; cli::run turns it into `internal`.
    std::panic::set_hook(Box::new(|_| {}));
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let (code, text) = cli::run(&args, &Env::from_os(std::env::vars_os()));
    // A closed stdout leaves only the exit code to report with.
    let _ = write!(std::io::stdout().lock(), "{text}{NL}");
    ExitCode::from(code)
}
