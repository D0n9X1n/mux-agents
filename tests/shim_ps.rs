//! Runs tests/shim.ps1, the PowerShell entry shim's tests, under every PowerShell this host has: `pwsh` everywhere and
//! Windows PowerShell 5.1 (`powershell`) on Windows. A PowerShell that is missing is skipped locally and fails in CI
//! (`CI` set), where every runner has `pwsh` and Windows runners also have `powershell`. The binary itself needs no
//! PowerShell; scripts/mux-agent.ps1 is an optional entry point.

use std::path::Path;
use std::process::Command;

fn shells() -> Vec<&'static str> {
    if cfg!(windows) {
        vec!["pwsh", "powershell"]
    } else {
        vec!["pwsh"]
    }
}

#[test]
fn powershell_shim_tests_pass_under_every_powershell() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/shim.ps1");
    let ci = std::env::var_os("CI").is_some();
    for shell in shells() {
        let out = match Command::new(shell)
            .args(["-NoProfile", "-NonInteractive", "-File"])
            .arg(&script)
            .env_remove("MUX_AGENTS_BIN")
            .output()
        {
            Ok(out) => out,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !ci => {
                eprintln!("skip {shell}: not installed");
                continue;
            }
            Err(e) => panic!("{shell}: {e}"),
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let last = stdout.lines().last().unwrap_or("").trim();
        assert!(
            out.status.success() && last == "0 failed",
            "{shell}: {}\n{stdout}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
