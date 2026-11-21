#![cfg(feature = "live")]
#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/private_core.rs"]
mod private_core;
use private_core::PrivateCore;
fn root() -> std::path::PathBuf {
    let base = if cfg!(unix) {
        std::path::PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    base.join(format!("mxc-{}", mux_agent::util::hex_id().unwrap()))
}
#[test]
fn core_environment_is_private() {
    let d = root();
    let a = PrivateCore::new("fake".into(), &d.join("a"), "private-path".as_ref());
    let b = PrivateCore::new("fake".into(), &d.join("b"), "private-path".as_ref());
    let mut c = a.command(&[]);
    c.env("HC_TERMINAL_ID", "ambient")
        .env("HC_SOCKET", "ambient")
        .env("TMUX", "ambient");
    a.apply(&mut c);
    let vars: std::collections::BTreeMap<_, _> = c.get_envs().collect();
    assert_eq!(
        vars[std::ffi::OsStr::new("HOME")],
        Some(d.join("a").join("home").as_os_str())
    );
    for home in ["HOME", "USERPROFILE"] {
        let home = std::path::Path::new(vars[std::ffi::OsStr::new(home)].unwrap());
        for (key, sub) in [("APPDATA", "Roaming"), ("LOCALAPPDATA", "Local")] {
            let folder = std::path::Path::new(vars[std::ffi::OsStr::new(key)].unwrap());
            assert_eq!(folder, home.join("AppData").join(sub), "{key}");
            assert!(folder.is_dir(), "{key} must exist");
        }
    }
    assert!(!vars.contains_key(std::ffi::OsStr::new("HC_TERMINAL_ID")));
    assert!(!vars.contains_key(std::ffi::OsStr::new("TMUX")));
    assert_eq!(
        vars[std::ffi::OsStr::new("HC_SOCKET")],
        a.env
            .iter()
            .find(|(k, _)| *k == "HC_SOCKET")
            .map(|(_, v)| v.as_os_str())
    );
    assert_ne!(
        a.env.iter().find(|(k, _)| *k == "HC_SESSION"),
        b.env.iter().find(|(k, _)| *k == "HC_SESSION")
    );
    assert_ne!(
        a.env.iter().find(|(k, _)| *k == "HC_SOCKET"),
        b.env.iter().find(|(k, _)| *k == "HC_SOCKET")
    );
    drop((a, b));
    std::fs::remove_dir_all(d).unwrap();
}
#[test]
fn core_environment_keys_are_allowlisted() {
    use std::collections::BTreeSet;

    let d = root();
    let core = PrivateCore::new("fake".into(), &d, "private-path".as_ref());
    let keys: BTreeSet<_> = core.env.iter().map(|(key, _)| *key).collect();
    let expected: BTreeSet<_> = [
        "USER",
        "USERNAME",
        "LOGNAME",
        "LANG",
        "SystemRoot",
        "ComSpec",
        "SystemDrive",
        "windir",
        "ProgramData",
        "ALLUSERSPROFILE",
        "PUBLIC",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "CommonProgramFiles",
        "PATHEXT",
        "OS",
        "PROCESSOR_ARCHITECTURE",
        "NUMBER_OF_PROCESSORS",
        "COMPUTERNAME",
        "USERDOMAIN",
        "RMUX_ALLOW_INTERNAL_DAEMON_IN_CALLER_JOB",
    ]
    .into_iter()
    .filter(|key| std::env::var_os(key).is_some())
    .chain([
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_RUNTIME_DIR",
        "TMPDIR",
        "TEMP",
        "TMP",
        "PATH",
        "HC_SESSION",
        "HC_SOCKET",
        "MUX_IT_DIR",
        "MUX_IT_ENTRY",
    ])
    .collect();
    drop(core);
    std::fs::remove_dir_all(d).unwrap();
    assert_eq!(keys, expected);
}

#[test]
fn core_drop_spares_another_fixture() {
    let d = root();
    let bin = PrivateCore::installed();
    let path = std::env::var_os("PATH").unwrap();
    let mut sentinel = PrivateCore::new(bin.clone(), &d.join("s"), &path);
    sentinel.start();
    let mut victim = PrivateCore::new(bin, &d.join("v"), &path);
    victim.start();
    let probe = victim.command(&["server", "stop", "--json"]);
    assert!(
        std::panic::catch_unwind(move || {
            let _guard = victim;
            panic!("fixture failure");
        })
        .is_err()
    );
    assert_eq!(
        private_core::run(probe, &d.join("v")).unwrap()["stopped"],
        false
    );
    assert_eq!(
        private_core::run(
            sentinel.command(&["server", "stop", "--json"]),
            &d.join("s")
        )
        .unwrap()["stopped"],
        true
    );
    assert!(sentinel.stop());
    drop(sentinel);
    std::fs::remove_dir_all(d).unwrap();
}

// A holder is found by its socket: the core names each holder's socket after its own, `<socket stem>.holder.<id>.sock`
// (`<dir>/h.holder.<id>.sock` beside `<dir>/h.sock`; `\.\pipe\<session>.holder.<id>.sock` on Windows). Only a holder
// of this fixture's socket, and what descends from it, is chosen; each with a start no earlier than its parent's.
#[test]
fn leftovers_are_this_fixtures_holders_and_their_descendants() {
    use mux_agent::proc::Row;
    let row = |pid, ppid, start, argv: &[&str]| Row {
        pid,
        ppid,
        start,
        path: None,
        argv: Some(argv.iter().map(|a| a.to_string()).collect()),
        image: None,
    };
    let hold = |socket: &str| ["/opt/hc/herdr-core", "hold", "--socket", socket].map(String::from);
    let hold = |pid, start, socket| {
        let argv = hold(socket);
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        row(pid, 1, start, &argv)
    };
    let table = vec![
        hold(100, Some(50), "/tmp/mxit-7-1/h.holder.ab12.sock"),
        row(101, 100, Some(51), &["/bin/sh", "-l"]),
        row(102, 101, Some(52), &["/bin/sleep", "60"]),
        // A pid reused by a process older than the holder: its parent link names another process.
        row(103, 100, Some(40), &["/bin/sh"]),
        // No start: it could never be checked again before a signal.
        row(104, 100, None, &["/bin/sh"]),
        hold(110, None, "/tmp/mxit-7-1/h.holder.cd34.sock"),
        // Another fixture's holders: a longer dir of the same prefix, and another run's dir.
        hold(105, Some(60), "/tmp/mxit-7-10/h.holder.ef56.sock"),
        row(106, 105, Some(61), &["/bin/sh", "-l"]),
        hold(107, Some(62), "/tmp/mxit-8-1/h.holder.ab12.sock"),
        // The developer's own core and its holder.
        row(108, 1, Some(10), &[r"C:\hc\herdr-core.exe", "serve"]),
        hold(
            109,
            Some(11),
            r"\.\pipe\herdr-core-dotan-default.holder.76c79df3.sock",
        ),
        // This fixture's multiplexer: the harness ends it, not the core's teardown.
        row(
            111,
            1,
            Some(70),
            &["tmux", "-S", "/tmp/mxit-7-1/s", "new-session"],
        ),
    ];
    let marks = [private_core::holder_mark("/tmp/mxit-7-1/h.sock")];
    assert_eq!(
        private_core::leftovers(&table, &marks),
        [(100, Some(50)), (101, Some(51)), (102, Some(52))]
    );
    assert_eq!(marks[0], "/tmp/mxit-7-1/h.holder.");
    assert_eq!(
        private_core::holder_mark(r"\.\pipe\mux-test-9-0123456789ab"),
        r"\.\pipe\mux-test-9-0123456789ab.holder."
    );
}

// A core killed outright leaves its holder and the kept shell running, and `server stop` then finds no core to ask:
// only the fixture's own sweep ends them. Dropping the fixture must leave no process of it running.
#[test]
fn core_drop_ends_a_holder_whose_core_is_gone() {
    let d = root();
    let path = std::env::var_os("PATH").unwrap();
    let mut core = PrivateCore::new(PrivateCore::installed(), &d, &path);
    core.start();
    let shell = if cfg!(windows) { "cmd.exe" } else { "/bin/sh" };
    private_core::run(
        core.command(&["run", "--bg", "--keep", "--json", "--", shell]),
        &d,
    )
    .expect("a kept shell");
    let status = private_core::run(core.command(&["server", "status", "--json"]), &d).unwrap();
    let pid = status["pid"].as_u64().expect("the core pid") as u32;
    let host = mux_agent::proc::host();
    let start = host.open(pid).map(|o| o.start);
    assert!(private_core::kill_pid(pid), "kill of the core {pid}");
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while host.open(pid).map(|o| o.start) == start {
        assert!(
            std::time::Instant::now() < end,
            "the core {pid} survived its kill"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let socket = core
        .env
        .iter()
        .find(|(k, _)| *k == "HC_SOCKET")
        .unwrap()
        .1
        .clone();
    let socket = socket.to_string_lossy().into_owned();
    let mut needles = vec![d.to_string_lossy().into_owned(), socket];
    if let Ok(real) = std::fs::canonicalize(&d) {
        needles.push(real.to_string_lossy().into_owned());
    }
    let names = |r: &mux_agent::proc::Row| {
        r.argv.iter().flatten().any(|a| {
            needles
                .iter()
                .any(|n| a.contains(n.trim_end_matches(".sock")))
        })
    };
    let table = host.table().unwrap();
    let ours: Vec<_> = table
        .iter()
        .filter(|r| names(r))
        .map(|r| (r.pid, r.start))
        .collect();
    let kept: Vec<_> = table
        .iter()
        .filter(|r| ours.iter().any(|&(p, _)| p == r.ppid))
        .map(|r| (r.pid, r.start))
        .collect();
    assert!(!ours.is_empty(), "the holder outlived its core");
    assert!(!kept.is_empty(), "the kept shell outlived its core");
    drop(core);
    let alive: Vec<_> = ours
        .iter()
        .chain(&kept)
        .filter(|&&(p, s)| s.is_some() && host.open(p).map(|o| o.start) == s)
        .collect();
    let named: Vec<_> = host
        .table()
        .unwrap()
        .into_iter()
        .filter(|r| names(r))
        .map(|r| (r.pid, r.argv))
        .collect();
    let _ = std::fs::remove_dir_all(&d);
    assert!(alive.is_empty(), "still running after drop: {alive:?}");
    assert!(
        named.is_empty(),
        "processes naming the fixture remain: {named:?}"
    );
}

// `still_listed` decides by identity, (pid, start) as read before the kill: the same pid and start is still listed, and
// so is that pid without a start (macOS lists a zombie not yet reaped without one); the pid with another start is
// another process that took the pid, and an absent pid is gone.
#[test]
fn still_listed_decides_by_pid_and_start() {
    use mux_agent::proc::Row;
    let row = |pid, start| Row {
        pid,
        ppid: 1,
        start,
        path: None,
        argv: None,
        image: None,
    };
    let table = [row(10, Some(100)), row(11, None), row(12, Some(999))];
    let killed = [
        (10, Some(100)),
        (11, Some(110)),
        (12, Some(120)),
        (13, Some(130)),
    ];
    assert_eq!(
        private_core::still_listed(&table, &killed),
        [(10, Some(100)), (11, Some(110))]
    );
    assert!(private_core::still_listed(&[], &killed).is_empty());
}

// `wait_gone_in` reads the table again until it lists none of the processes, and returns at its bound with those it
// still lists: never sooner, and never long after.
#[test]
fn wait_gone_in_polls_until_gone_or_its_bound() {
    use mux_agent::proc::Row;
    use std::time::{Duration, Instant};
    let me = Row {
        pid: 10,
        ppid: 1,
        start: Some(100),
        path: None,
        argv: None,
        image: None,
    };
    let mut reads = 0;
    let left = private_core::wait_gone_in(
        || {
            reads += 1;
            if reads < 3 {
                vec![me.clone()]
            } else {
                Vec::new()
            }
        },
        &[(10, Some(100))],
        Duration::from_secs(30),
    );
    assert_eq!((left, reads), (Vec::new(), 3));
    let begun = Instant::now();
    let left = private_core::wait_gone_in(
        || vec![me.clone()],
        &[(10, Some(100))],
        Duration::from_millis(300),
    );
    let took = begun.elapsed();
    assert_eq!(left, [(10, Some(100))]);
    assert!(
        took >= Duration::from_millis(300) && took < Duration::from_secs(5),
        "{took:?}"
    );
}

// A child that exited is not listed, even while this test still holds it (on Windows a `Child` keeps its process
// handle, and a pid still opens while any handle to its process is held): `listed`, which a kill's recovery uses, reads
// the process table, not whether the pid opens.
#[test]
fn an_exited_child_still_held_is_not_listed() {
    use std::time::{Duration, Instant};
    let d = root();
    std::fs::create_dir_all(&d).unwrap();
    let release = d.join("release-held.json");
    let mut hold = std::process::Command::new(env!("CARGO_BIN_EXE_mux-agent"));
    hold.arg("hold")
        .arg(&release)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = bounded::spawn(&mut hold, Duration::from_secs(120)).unwrap();
    let me = mux_agent::proc::host()
        .table()
        .unwrap()
        .iter()
        .find(|r| r.pid == child.id())
        .map(|r| (r.pid, r.start))
        .expect("the child is listed");
    assert!(
        private_core::listed(me.0, me.1),
        "a running child is listed"
    );
    std::fs::write(&release, "{}").unwrap();
    let end = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < end, "the child did not exit");
        std::thread::sleep(Duration::from_millis(50));
    }
    // The table may list an exited process for a moment after its exit is seen: waited for, with the child held.
    let end = Instant::now() + Duration::from_secs(10);
    while private_core::listed(me.0, me.1) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !private_core::listed(me.0, me.1),
        "an exited child is not listed while it is still held"
    );
    drop(child);
}

// On the real process table, on every OS: a child running this binary's `hold` is listed until it exits (its
// release file appears), then `wait_gone` returns, not before.
#[test]
fn wait_gone_returns_once_a_real_child_left_the_table() {
    use std::time::{Duration, Instant};
    let d = root();
    std::fs::create_dir_all(&d).unwrap();
    let release = d.join("release.json");
    let mut hold = std::process::Command::new(env!("CARGO_BIN_EXE_mux-agent"));
    hold.arg("hold")
        .arg(&release)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = bounded::spawn(&mut hold, Duration::from_secs(120)).unwrap();
    let host = mux_agent::proc::host();
    let table = host.table().unwrap();
    let me = table
        .iter()
        .find(|r| r.pid == child.id())
        .map(|r| (r.pid, r.start))
        .expect("the child is listed");
    assert!(me.1.is_some(), "the child's start: {me:?}");
    let running =
        private_core::wait_gone_in(|| host.table().unwrap(), &[me], Duration::from_millis(300));
    assert_eq!(running, [me], "a running child is not gone");
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(&release, "{}").unwrap();
    });
    let begun = Instant::now();
    private_core::wait_gone(&[me], Duration::from_secs(30));
    assert!(
        begun.elapsed() >= Duration::from_millis(300),
        "returned before the child exited"
    );
    releaser.join().unwrap();
    assert!(
        child.wait().unwrap().success(),
        "hold exits 0 once released"
    );
    std::fs::remove_dir_all(d).unwrap();
}
