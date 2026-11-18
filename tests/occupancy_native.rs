//! occupy-3 H1/H4.5 on the real process table: image names, rows kept without path or argv, and gone pids dropped.
mod native;

use mux_agent::agents::occupancy::holds;
use mux_agent::parse::Client;
use mux_agent::state::root::HOST;

const SID: &str = "0f11fc2f-2032-48ef-a920-4af2a8e5af50";

fn scratch(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("occupancy-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// The body copies run: 60 s of nothing, ended by the test's `Reap`.
#[test]
#[ignore]
fn park() {
    std::thread::sleep(std::time::Duration::from_secs(60));
}

// Every OS: a live copy named claude is listed with that image; readable, it is no unbound row.
#[test]
fn a_copy_named_claude_is_listed_with_that_image() {
    let dir = scratch("copy-claude");
    let child = native::copy_as(&dir, "claude", &["--exact", "park", "--ignored"]);
    let pid = child.0.id();
    let os = mux_agent::proc::host();
    let row = native::within_10_s("the copy listed", || {
        os.table().unwrap().into_iter().find(|r| r.pid == pid)
    });
    let want = if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    };
    assert!(
        row.image
            .as_deref()
            .is_some_and(|i| i.eq_ignore_ascii_case(want)),
        "{row:?}"
    );
    assert!(row.start.is_some(), "{row:?}");
    drop(child);
    let _ = std::fs::remove_dir_all(&dir);
}

// Linux: an exited, unreaped copy (a zombie) keeps its start and comm but has no exe link and an empty cmdline, so it
// reads as an unbound row. It runs no more code, so `ended` shows it and it holds nothing, under any image: a stopped
// client its parent has not reaped yet never blocks a resume.
#[cfg(target_os = "linux")]
#[test]
fn a_linux_zombie_is_unbound_and_judged_by_its_image() {
    let os = mux_agent::proc::host();
    for name in ["claude", "worker"] {
        let dir = scratch(&format!("zombie-{name}"));
        let child = native::copy_as(&dir, name, &["--list"]);
        let pid = child.0.id();
        native::within_10_s("a zombie", || {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            (stat.rsplit_once(')')?.1.trim_start().starts_with('Z')).then_some(())
        });
        let row = os
            .table()
            .unwrap()
            .into_iter()
            .find(|r| r.pid == pid)
            .expect("a zombie stays listed");
        assert!(
            row.start.is_some() && row.path.is_none() && row.argv.is_none(),
            "{name}: {row:?}"
        );
        assert_eq!(row.image.as_deref(), Some(name));
        assert!(os.ended(&row), "{name}: a zombie has ended");
        let held = holds(os.as_ref(), &row, SID, Client::Claude, HOST).map(|h| (h.rule, h.pid));
        assert_eq!(held, None, "{name}");
        drop(child);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// macOS: launchd (pid 1, root) lists for a user who cannot read its argv; its image clears it for both kinds.
#[cfg(target_os = "macos")]
#[test]
fn launchd_is_kept_with_its_image_and_holds_nothing() {
    let os = mux_agent::proc::host();
    let row = os
        .table()
        .unwrap()
        .into_iter()
        .find(|r| r.pid == 1)
        .expect("pid 1 is listed (H1)");
    assert_eq!(
        row.argv, None,
        "precondition: the runner cannot read root's argv"
    );
    assert_eq!(row.image.as_deref(), Some("launchd"));
    for kind in [Client::Claude, Client::Copilot] {
        assert_eq!(holds(os.as_ref(), &row, SID, kind, HOST), None, "{kind:?}");
    }
}

// Unix: a reaped pid is ESRCH and leaves the table (`seam::listed`).
#[cfg(unix)]
#[test]
fn a_reaped_pid_leaves_the_table() {
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let pid = child.id();
    child.kill().unwrap();
    child.wait().unwrap();
    let os = mux_agent::proc::host();
    assert!(
        !os.table().unwrap().iter().any(|r| r.pid == pid),
        "a reaped pid is gone (pid reuse within this window is not expected)"
    );
}

// Linux, call 10: this runner's /proc lets occupancy see other users' processes; its real mountinfo parses as Open
// (a hidepid CI host would make every resume refuse, so the suite states it).
#[cfg(target_os = "linux")]
#[test]
fn this_runners_proc_shows_other_users() {
    use mux_agent::agents::occupancy::{ProcView, proc_view};
    let text = mux_agent::proc::host()
        .proc_mounts()
        .expect("Linux has the seam")
        .unwrap();
    assert_eq!(proc_view(&text), ProcView::Open, "{text}");
}

// Windows: copies whose queries are denied (`native::deny_queries`, as report-2b's liveness test) keep Toolhelp's
// pid, ppid and image with no start; claude.exe holds Claude, node.exe holds Copilot only, svchost.exe nothing.
#[cfg(windows)]
#[test]
fn denied_windows_rows_are_judged_by_their_image() {
    let os = mux_agent::proc::host();
    let dir = scratch("denied");
    let copies: Vec<(&str, native::Reap)> = ["claude", "node", "svchost"]
        .into_iter()
        .map(|name| {
            let args = ["--exact", "park", "--ignored"];
            (name, native::copy_as(&dir.join(name), name, &args))
        })
        .collect();
    for (_, child) in &copies {
        native::deny_queries(&child.0);
    }
    for (name, child) in &copies {
        let pid = child.0.id();
        assert!(
            os.row(pid).is_none(),
            "{name}: precondition, the deny took effect: {}",
            native::deny_report(&child.0)
        );
        let row = os
            .table()
            .unwrap()
            .into_iter()
            .find(|r| r.pid == pid)
            .expect("listed by Toolhelp");
        assert_eq!((row.start, row.path.as_deref()), (None, None), "{name}");
        assert!(
            row.image
                .as_deref()
                .is_some_and(|i| i.eq_ignore_ascii_case(&format!("{name}.exe"))),
            "{row:?}"
        );
        for kind in [Client::Claude, Client::Copilot] {
            let want = matches!(
                (*name, kind),
                ("claude", Client::Claude) | ("node", Client::Copilot)
            );
            let held = holds(os.as_ref(), &row, SID, kind, HOST).map(|h| (h.rule, h.pid));
            assert_eq!(
                held,
                want.then_some((mux_agent::agents::occupancy::Rule::Unbound, Some(pid))),
                "{name} {kind:?}"
            );
        }
    }
    drop(copies);
    let _ = std::fs::remove_dir_all(&dir);
}
