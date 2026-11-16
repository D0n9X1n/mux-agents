#![cfg(windows)]
//! report-2b PM call 10 on the real Toolhelp table: a live process this user cannot open or query is Unknown and
//! keeps its registry entry; after it exits it is Dead and swept.
mod native;

use std::process::{Command, Stdio};

use mux_agent::parse::Client;
use mux_agent::state::live::{self, Alive};

const SID: &str = "0f11fc2f-2032-48ef-a920-4af2a8e5af50";

#[test]
fn a_live_process_that_denies_queries_is_unknown_and_kept() {
    let dir = std::env::temp_dir().join(format!("liveness-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ctx = native::ctx(&dir.join("state"));
    let mut child = Command::new("ping")
        .args(["-n", "60", "127.0.0.1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let start = ctx.os.open(pid).expect("open before the deny").start;
    assert_eq!(
        live::report(&ctx, Client::Claude, pid, start, SID, "startup").map_err(|r| r.check),
        Ok(1)
    );
    native::deny_queries(&child);
    let report = native::deny_report(&child);
    eprintln!("deny: {report}");
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert!(
            ctx.os.row(pid).is_none(),
            "precondition: the deny took effect (no row): {report}"
        );
        let listed = ctx.os.table().unwrap().into_iter().find(|r| r.pid == pid);
        assert_eq!(
            listed.map(|r| r.start),
            Some(None),
            "Toolhelp lists it with no start"
        );
        assert_eq!(live::alive(&ctx, pid, start), Alive::Unknown);
        assert_eq!(live::sweep(&ctx).map_err(|r| r.check), Ok(0), "kept");
        assert!(live::path(&ctx, Client::Claude, pid, start).is_file());
    }));
    child.kill().unwrap();
    child.wait().unwrap();
    drop(child);
    outcome.unwrap();
    // PM call 17: Dead once the pid has left the Toolhelp table; bounded wait, never an immediate assumption.
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while live::alive(&ctx, pid, start) != Alive::Dead {
        assert!(
            std::time::Instant::now() < end,
            "still listed 10 s after exit"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(live::sweep(&ctx).map_err(|r| r.check), Ok(1));

    let _ = std::fs::remove_dir_all(&dir);
}
