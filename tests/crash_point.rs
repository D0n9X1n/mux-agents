//! Crash exits exist only in test builds (the launch-1 crash model): the library that ships never exits on
//! MUX_AGENTS_CRASH. The check runs in a child process, so this process keeps its own environment.
#![cfg(not(feature = "live"))]

use std::process::Command;

#[test]
fn the_library_never_exits_on_a_crash_variable() {
    if std::env::var_os("MUX_AGENTS_CRASH_CHILD").is_some() {
        mux_agent::state::evidence::crash_point("pending");
        println!("CRASH-POINT-RETURNED");
        return;
    }
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "the_library_never_exits_on_a_crash_variable",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("MUX_AGENTS_CRASH_CHILD", "1")
        .env("MUX_AGENTS_CRASH", "pending")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("CRASH-POINT-RETURNED"),
        "the child returned"
    );
}
