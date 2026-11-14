//! The shell shortcuts end to end: the real scripts/shell/mux-agents.{sh,ps1}, through the real entry shim, to the
//! built binary's `up`. Outside a multiplexer `up` refuses with its own message, so a refusal naming the multiplexer
//! (not a usage or parse error) proves cc and gg reached `up -Client <client> -- <args>`. bash runs on Unix, pwsh
//! everywhere; a shell that is missing is skipped locally and fails in CI (`CI` set).

use std::path::Path;
use std::process::{Command, Output};

const OUTSIDE: &str = "up needs tmux or rmux for now";

/// Runs `script` in `shell` with no multiplexer, no herdr terminal and the built binary as MUX_AGENTS_BIN.
fn run(shell: &str, args: &[&str]) -> Option<Output> {
    let mut command = Command::new(shell);
    command
        .args(args)
        .env("MUX_AGENTS_BIN", env!("CARGO_BIN_EXE_mux-agent"))
        .env(
            "MUX_AGENTS_STATE",
            std::env::temp_dir().join(format!("mux-shortcuts-{}", std::process::id())),
        );
    for name in [
        "TMUX",
        "RMUX",
        "TMUX_PANE",
        "HC_TERMINAL_ID",
        "MUX_AGENTS_HOME",
        // bash runs $BASH_ENV even with --norc --noprofile: its output would break the JSON, and it could set the
        // variables removed here again.
        "BASH_ENV",
        "ENV",
    ] {
        command.env_remove(name);
    }
    match command.output() {
        Ok(out) => Some(out),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && std::env::var_os("CI").is_none() => {
            eprintln!("skip {shell}: not installed");
            None
        }
        Err(e) => panic!("{shell}: {e}"),
    }
}

fn assert_up_refused(shell: &str, function: &str, out: &Output) {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let reply: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "{shell} {function}: {e}\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert_eq!(
        (
            out.status.code(),
            &reply["verb"],
            &reply["check"],
            &reply["message"]
        ),
        (Some(1), &"up".into(), &"endpoint".into(), &OUTSIDE.into()),
        "{shell} {function}: {reply}"
    );
}

#[cfg(unix)]
#[test]
fn bash_cc_and_gg_reach_up_through_the_real_entry() {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/shell/mux-agents.sh");
    for function in ["cc", "gg"] {
        let line = format!(". \"$1\" && {function} --model 'a b' -Text");
        let Some(out) = run(
            "bash",
            &[
                "--norc",
                "--noprofile",
                "-c",
                &line,
                "_",
                file.to_str().unwrap(),
            ],
        ) else {
            return;
        };
        assert_up_refused("bash", function, &out);
    }
}

#[test]
fn pwsh_cc_and_gg_reach_up_through_the_real_entry() {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/shell/mux-agents.ps1");
    for function in ["cc", "gg"] {
        let line = format!(
            ". '{}'; {function} --model 'a b' -Text; exit $LASTEXITCODE",
            file.display()
        );
        let Some(out) = run(
            "pwsh",
            &["-NoProfile", "-NonInteractive", "-Command", &line],
        ) else {
            return;
        };
        assert_up_refused("pwsh", function, &out);
    }
}
