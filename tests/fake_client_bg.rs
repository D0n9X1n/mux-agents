#![cfg(feature = "live")]
//! The fake client runs a background line beside its loop (live harness support, #103).
use std::time::{Duration, Instant};

fn wait_for(p: &std::path::Path) {
    let end = Instant::now() + Duration::from_secs(20);
    while !p.exists() {
        assert!(Instant::now() < end, "{}", p.display());
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn the_fake_client_runs_a_bg_line_without_blocking() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("fake-bg-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_fake-client"))
        .env("MUX_IT_DIR", &dir)
        .stdin(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let cmd = dir.join(format!("cmd.{pid}"));
    std::fs::write(&cmd, "@bg @sleep\t5000\n").unwrap();
    wait_for(&dir.join(format!("cmd.{pid}.taken.1")));
    std::fs::write(&cmd, "@sleep\t1\n").unwrap();
    wait_for(&dir.join(format!("reply.{pid}.2.json")));
    let still = !dir.join(format!("reply.{pid}.1.json")).exists();
    if still {
        wait_for(&dir.join(format!("reply.{pid}.1.json")));
    }
    // Stop the client and remove its folder before asserting, so a failure leaves nothing running.
    std::fs::write(dir.join(format!("quit.{pid}")), "0").unwrap();
    child.wait().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(still, "the background line is still running");
}
