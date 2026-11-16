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

// O5: with `<MUX_IT_DIR>/resume-gone`, `--resume <id>` prints the client's own message (by its file stem, so
// `claude.exe` on Windows) and exits 1 before it records itself. Bounded: a client that never exits fails in 20 s.
#[test]
fn a_resume_of_a_gone_conversation_exits_1_with_the_clients_message() {
    let id = "7d1c0e5a-9b1f-4d0e-8f7a-3c2b1a0e9d8c";
    for (name, want) in [
        (
            "claude",
            format!("No conversation found with session ID: {id}"),
        ),
        (
            "copilot",
            format!("Error: No session, task, or name matched '{id}'."),
        ),
    ] {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("fake-gone-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let exe = dir
            .join("bin")
            .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        std::fs::copy(env!("CARGO_BIN_EXE_fake-client"), &exe).unwrap();
        std::fs::write(dir.join("resume-gone"), "").unwrap();
        let mut child = start(&exe, &dir, id);
        let end = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            if Instant::now() > end {
                child.kill().unwrap();
                let _ = child.wait();
                let _ = std::fs::remove_dir_all(&dir);
                panic!("{name}: a resume of a gone conversation never exited");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let mut err = String::new();
        std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut err).unwrap();
        let recorded = std::fs::read_dir(&dir).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("client.")
        });
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            (status.code(), err.trim(), recorded),
            (Some(1), want.as_str(), false),
            "{name}"
        );
    }
}

/// Starts the copy; a copy just written can be refused "text file busy" on Linux (#76): retried 20 ms apart.
fn start(exe: &std::path::Path, dir: &std::path::Path, id: &str) -> std::process::Child {
    for _ in 0..50 {
        let spawned = std::process::Command::new(exe)
            .args(["--resume", id])
            .env("MUX_IT_DIR", dir)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn();
        match spawned {
            Err(e) if e.raw_os_error() == Some(26) => std::thread::sleep(Duration::from_millis(20)),
            other => return other.unwrap(),
        }
    }
    panic!("{} stayed busy", exe.display());
}
