//! D6: a panic leaves as the internal refusal, with no payload or backtrace on either stream.
#![cfg(feature = "test-panic")]

use std::process::Command;

// Console.Out.WriteLine's line end: exactly one, as the entry writes it.
const NL: &str = if cfg!(windows) { "\r\n" } else { "\n" };

#[test]
fn dispatch_panic_prints_internal_and_hides_payload() {
    let out = Command::new(env!("CARGO_BIN_EXE_mux-agent"))
        .args(["list", "-All"])
        .env("MUX_AGENTS_TEST_PANIC", "dispatch")
        .env("RUST_BACKTRACE", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert_eq!(
        stdout,
        format!(
            r#"{{"ok":false,"verb":"list","code":1,"check":"internal","message":"unexpected error (panic)"}}{NL}"#
        )
    );
    assert_eq!(stderr, "");
    assert_eq!(out.status.code(), Some(1));
    assert!(!stdout.contains("PANIC-PAYLOAD-7f3a") && !stderr.contains("PANIC-PAYLOAD-7f3a"));
}
