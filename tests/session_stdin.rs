//! The Copilot `sessionStart` hook exactly as hooks.json states it, with Copilot's JSON on stdin (report-2b K3),
//! through each shell that may run it. Outside tmux, rmux and hc no client is above the test process (the walk stops
//! at this test binary, neither a shell nor a client), so the reply echoes the session and nothing is written.
use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const SID: &str = "0f11fc2f-2032-48ef-a920-4af2a8e5af50";

/// hooks.json's `sessionStart` command for `field` (`bash` or `command`), without `-Quiet` so the reply can be read.
/// An invalid manifest (a bad escape) fails here.
fn hook(field: &str) -> String {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/hooks.json")).unwrap();
    let v: Value = serde_json::from_str(&text).expect("hooks.json is valid JSON");
    let cmd = v["hooks"]["sessionStart"][0][field]
        .as_str()
        .unwrap()
        .to_string();
    assert!(cmd.ends_with(" -Stdin -Quiet"), "{cmd}");
    cmd.trim_end_matches(" -Quiet").to_string()
}

/// Runs `command` from the plugin root with `body` on stdin; the exit code, the reply, whether `live/` exists.
fn run(mut command: Command, label: &str, body: &[u8]) -> (Option<i32>, Value, bool) {
    let dir = std::env::temp_dir().join(format!("session-stdin-{}-{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for v in [
        "TMUX",
        "RMUX",
        "TMUX_PANE",
        "HC_TERMINAL_ID",
        "SANDBOX_RUNTIME",
    ] {
        command.env_remove(v);
    }
    command
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("MUX_AGENTS_BIN", env!("CARGO_BIN_EXE_mux-agent"))
        .env("MUX_AGENTS_STATE", dir.join("state"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    // A child that stops reading past its cap may close the pipe first: that write error is expected.
    let _ = child.stdin.take().unwrap().write_all(body);
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let live = dir.join("state").join("live").exists();
    let _ = std::fs::remove_dir_all(&dir);
    let reply = serde_json::from_str(text.trim())
        .unwrap_or_else(|_| panic!("{label}: {text} {}", String::from_utf8_lossy(&out.stderr)));
    (out.status.code(), reply, live)
}

fn body() -> Vec<u8> {
    json!({"sessionId": SID, "timestamp": 1, "cwd": "/w", "source": "startup"})
        .to_string()
        .into_bytes()
}

/// (fix 1, audit 5-9 item 11) A `{sessionId, pad}` body of exactly `n` bytes.
fn sized(n: usize) -> Vec<u8> {
    let tail = r#""}"#;
    let mut b = format!(r#"{{"sessionId":"{SID}","pad":""#).into_bytes();
    b.resize(n - tail.len(), b'x');
    b.extend_from_slice(tail.as_bytes());
    assert_eq!(b.len(), n);
    b
}

fn check(label: &str, (code, reply, live): (Option<i32>, Value, bool)) {
    assert_eq!(code, Some(0), "{label}: {reply}");
    assert_eq!(
        (&reply["ok"], &reply["sessionId"]),
        (&json!(true), &json!(SID)),
        "{label}"
    );
    assert_eq!(
        (&reply["client"], &reply["seq"]),
        (&Value::Null, &Value::Null),
        "{label}: no client above the test"
    );
    assert!(!live, "{label}: nothing written");
}

fn binary() -> Command {
    let mut bin = Command::new(env!("CARGO_BIN_EXE_mux-agent"));
    bin.args(["report", "-Session", "-Stdin"]);
    bin
}

#[cfg(unix)]
#[test]
fn the_unix_hook_reads_its_session_from_stdin() {
    let mut sh = Command::new("sh");
    sh.arg("-c").arg(hook("bash"));
    check("sh", run(sh, "sh", &body()));
}

// Copilot runs `command` under its Windows shell (PowerShell, per the hooks reference); cmd.exe, powershell.exe,
// pwsh and the .ps1 entry each pass the JSON to the binary.
#[cfg(windows)]
#[test]
fn the_windows_hook_reads_its_session_from_stdin_through_each_shell() {
    use std::os::windows::process::CommandExt;
    let line = hook("command");
    let mut cmd = Command::new("cmd.exe");
    cmd.args(["/d", "/c"]).raw_arg(&line);
    check("cmd", run(cmd, "cmd", &body()));
    for shell in ["powershell.exe", "pwsh.exe"] {
        let mut ps = Command::new(shell);
        ps.args(["-NoProfile", "-NonInteractive", "-Command", &line]);
        check(shell, run(ps, shell, &body()));
    }
    let mut ps1 = Command::new("pwsh.exe");
    ps1.args([
        "-NoProfile",
        "-NonInteractive",
        "-File",
        r"scripts\mux-agent.ps1",
        "report",
        "-Session",
        "-Stdin",
    ]);
    check("mux-agent.ps1", run(ps1, "ps1", &body()));
}

// (fix 1, audit 5-9 item 11) Ambiguity 17: at most 64 KiB is accepted; 64 KiB + 1 and an empty body are usage.
#[test]
fn stdin_of_exactly_64_kib_is_read_and_one_byte_more_is_usage() {
    let (code, reply, _) = run(binary(), "at-cap", &sized(65_536));
    assert_eq!(
        (code, &reply["sessionId"]),
        (Some(0), &json!(SID)),
        "exactly 64 KiB: {reply}"
    );
    for (label, input) in [("over-cap", sized(65_537)), ("empty", Vec::new())] {
        let (code, reply, live) = run(binary(), label, &input);
        assert_eq!(
            (code, &reply["check"]),
            (Some(2), &json!("usage")),
            "{label}"
        );
        assert!(!live, "{label}");
    }
}

// (fix 1, audit 5-9 item 12) The production `IsTerminal` branch on a real terminal: a pty's slave as stdin is refused at
// once (usage), never read. Windows: CI has no console to give a child; the manual check is in Step 4.
#[cfg(unix)]
#[test]
fn a_terminal_stdin_is_refused_without_reading() {
    use std::os::fd::FromRawFd;
    let (mut master, mut slave) = (0, 0);
    // SAFETY: two valid out pointers; the name, termios and window size are not asked for.
    let made = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(made, 0, "openpty");
    // SAFETY: openpty returned these descriptors and nothing else owns them.
    let (master, slave) = unsafe {
        (
            std::fs::File::from_raw_fd(master),
            std::fs::File::from_raw_fd(slave),
        )
    };
    let dir = std::env::temp_dir().join(format!("session-tty-{}", std::process::id()));
    let mut child = binary()
        .env("MUX_AGENTS_STATE", dir.join("state"))
        .env_remove("TMUX")
        .env_remove("RMUX")
        .env_remove("HC_TERMINAL_ID")
        .stdin(Stdio::from(slave))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if std::time::Instant::now() >= end {
            let _ = child.kill();
            panic!("blocked reading a terminal stdin");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    drop(master);
    assert_eq!(status.code(), Some(2), "a terminal stdin is usage");
    assert!(!dir.join("state").join("live").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

// The second `sessionStart` hook runs the side bar by default from the same JSON. Outside tmux and rmux the caller
// resolves to no pane and the verb refuses `endpoint` (nothing opened); a `compact` start is skipped before any lookup.
#[cfg(unix)]
#[test]
fn the_unix_sidebar_hook_reads_its_source_from_stdin() {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/hooks.json")).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    let cmd = v["hooks"]["sessionStart"][1]["bash"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(cmd, "sh scripts/mux-agent sidebar -Auto -Stdin -Quiet");
    let line = cmd.trim_end_matches(" -Quiet").to_string();
    let compact = json!({"sessionId": SID, "source": "compact"}).to_string();
    let mut sh = Command::new("sh");
    sh.arg("-c").arg(&line);
    let (code, reply, _) = run(sh, "sidebar-compact", compact.as_bytes());
    assert_eq!(
        (code, &reply["skipped"]),
        (Some(0), &json!("source")),
        "{reply}"
    );
    let mut sh = Command::new("sh");
    sh.arg("-c").arg(&line);
    let (code, reply, _) = run(sh, "sidebar-startup", &body());
    assert_eq!(
        (code, &reply["check"]),
        (Some(1), &json!("endpoint")),
        "{reply}"
    );
    // As Copilot runs it: quiet, and success whatever happens.
    let mut sh = Command::new("sh");
    sh.arg("-c").arg(&cmd);
    let dir = std::env::temp_dir().join(format!("session-stdin-{}-quiet", std::process::id()));
    let out = sh
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("MUX_AGENTS_BIN", env!("CARGO_BIN_EXE_mux-agent"))
        .env("MUX_AGENTS_STATE", dir.join("state"))
        .env_remove("TMUX")
        .env_remove("RMUX")
        .env_remove("HC_TERMINAL_ID")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!((out.status.code(), out.stdout.len()), (Some(0), 0));
}
