//! Crash exits (launch-1's crash model). The library that ships never exits on MUX_AGENTS_CRASH (A3b); a
//! `test-panic` build stops only when MUX_AGENTS_CRASH names the step (A3c, PM call 8). Children run the steps, so
//! this process keeps its own environment. A `live` build exits on purpose too (b-call 39), so this file checks the
//! builds without it.
#![cfg(not(feature = "live"))]

use std::process::Command;

// a3c call 48: the one bounded runner (o2 call 14). Its `output` drains both pipes with a deadline even after the
// child exits, so a descendant holding them cannot stretch the bound.
#[path = "support/bounded.rs"]
mod bounded;

/// Runs `c` with its output captured through `bounded::output`; past `bounded::LIMIT` it is killed and this fails.
fn output_within(mut c: Command) -> std::process::Output {
    bounded::output(&mut c, bounded::LIMIT)
        .unwrap_or_else(|e| panic!("the crash child did not finish within its bound: {e}"))
}

#[cfg(not(feature = "test-panic"))]
#[test]
fn the_library_never_exits_on_a_crash_variable() {
    if std::env::var_os("MUX_AGENTS_CRASH_CHILD").is_some() {
        mux_agent::state::evidence::crash_point("pending");
        println!("CRASH-POINT-RETURNED");
        return;
    }
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args([
        "--exact",
        "the_library_never_exits_on_a_crash_variable",
        "--nocapture",
        "--test-threads=1",
    ])
    .env("MUX_AGENTS_CRASH_CHILD", "1")
    .env("MUX_AGENTS_CRASH", "pending");
    let out = output_within(c);
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

#[cfg(feature = "test-panic")]
const STEPS: [&str; 8] = [
    "allocated",
    "pending",
    "spec",
    "launch",
    "run",
    "address",
    "commit",
    "promote",
];

/// The child: passes every step, printing each, unless MUX_AGENTS_CRASH stops it.
#[cfg(feature = "test-panic")]
#[test]
#[ignore = "run by crash_point_runs_only_when_named"]
fn child_passes_every_step() {
    if std::env::var_os("MUX_AGENTS_CRASH_CHILD").is_none() {
        return;
    }
    for step in STEPS {
        mux_agent::state::evidence::crash_point(step);
        println!("passed {step}");
    }
}

#[cfg(feature = "test-panic")]
fn child(crash: Option<&str>) -> (i32, String) {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args([
        "child_passes_every_step",
        "--exact",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ]);
    c.env("MUX_AGENTS_CRASH_CHILD", "1")
        .env_remove("MUX_AGENTS_CRASH");
    if let Some(step) = crash {
        c.env("MUX_AGENTS_CRASH", step);
    }
    let out = output_within(c);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

// PM call 8 (4to6 #6): in a test-panic build only a named step stops the process; with the variable removed, or
// naming another step, every step passes.
#[cfg(feature = "test-panic")]
#[test]
fn crash_point_runs_only_when_named() {
    let (code, out) = child(None);
    assert_eq!(code, 0, "no variable: {out}");
    assert!(
        STEPS.iter().all(|s| out.contains(&format!("passed {s}"))),
        "no variable: every step passes: {out}"
    );
    let (code, out) = child(Some("run"));
    assert!(
        out.contains("passed launch") && !out.contains("passed run"),
        "named: stops at run: {out}"
    );
    assert_eq!(code, 86, "named: {out}");
    let (code, out) = child(Some("nowhere"));
    assert_eq!(code, 0, "another name: {out}");
    assert!(
        out.contains("passed promote"),
        "another name: every step passes: {out}"
    );
}
