#![cfg(all(windows, feature = "live"))]
//! Windows live tests for the side bar's clicks and keys (#70): a private rmux server per test (`rmux -L <name>`,
//! never the user's own) whose root pane is `cmd.exe`, with `claude.exe` on PATH as the fake client. Each test
//! drives the bar as a terminal would and checks which pane the multiplexer then has active.
//!
//! Run with `cargo test --features live --test live_windows -- --nocapture`. Without rmux on PATH each test is
//! skipped, unless MUX_AGENTS_LIVE_REQUIRE names `rmux`: then it fails. In CI, rmux 0.10.0 runs from a debug build
//! with RMUX_ALLOW_INTERNAL_DAEMON_IN_CALLER_JOB=1, since a release rmux 0.10.0 cannot start its daemon inside a
//! runner's job object (rmux#238).

#[path = "support/private_core.rs"]
mod private_core;

// outcome-2 call 14: every child this harness starts is bounded.
#[path = "support/bounded.rs"]
mod bounded;

// A3c Task 7: readiness barriers (PM calls 12, 26) and the fake client's own guards (PM call 27).
#[path = "support/ready.rs"]
mod ready;

// report-2b: the shared integration fixtures (private root, 150 s wait, deny_queries); occupy-3 uses them here.
mod native;

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

// outcome-2 call 14 (build checklist 4): a child past its bound is killed and refused `TimedOut` well before its own
// end (`cmd.exe /C ping`, about 29 s); a watched child past its bound is killed by its watchdog. Needs no rmux.
#[test]
fn a_child_past_its_bound_is_killed_and_refused() {
    let begun = Instant::now();
    let mut pinger = Command::new("cmd.exe");
    pinger.args(["/C", "ping", "-n", "30", "127.0.0.1"]);
    let got = bounded::output(&mut pinger, Duration::from_secs(1));
    assert_eq!(
        got.map(|_| ()).map_err(|e| e.kind()),
        Err(std::io::ErrorKind::TimedOut)
    );
    assert!(
        begun.elapsed() < Duration::from_secs(10),
        "{:?}",
        begun.elapsed()
    );
    let mut held = bounded::spawn(
        Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null()),
        Duration::from_secs(1),
    )
    .unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    while held.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < end,
            "the watchdog never killed the watched child"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

// o3 call 34 N4: a slow diagnostic run on a bounded child's handle never holds the watchdog off: the child, bounded at
// 1 s, is killed while a 4 s diagnostic still runs. Needs no rmux.
#[test]
fn a_diagnostic_on_a_bounded_child_never_blocks_its_watchdog() {
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    let mut ping = Command::new("ping");
    ping.args(["-n", "60", "127.0.0.1"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    let mut child = bounded::spawn(&mut ping, Duration::from_secs(1)).unwrap();
    let handle = child.handle();
    std::thread::sleep(Duration::from_secs(4)); // the slow diagnostic
    // SAFETY: the child's own process handle, alive while `child` is; a zero timeout only polls.
    let exited_during = unsafe { WaitForSingleObject(handle, 0) == WAIT_OBJECT_0 };
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        exited_during,
        "the watchdog killed the child while the diagnostic ran"
    );
}

// o2 call 29 (QA #146 F3): `bounded::output` never answers success with output it did not finish reading. The child
// prints and exits while a descendant keeps its stdout open about 8 s: refused `TimedOut` naming stdout, within the
// 5 s grace and the 1 s release (so the read handle was dropped, not left blocked until the descendant ends). A
// normal child keeps its whole output, past one pipe buffer. Needs no rmux.
#[test]
fn bounded_output_never_answers_with_unread_output() {
    let begun = Instant::now();
    let mut held = Command::new("cmd.exe");
    // The descendant (an inner cmd) holds stdout and writes nothing: only a cancelled read can drop the handle.
    held.args([
        "/C",
        "echo",
        "expected",
        "output",
        "&",
        "start",
        "/b",
        "cmd",
        "/c",
        "ping -n 9 127.0.0.1 >nul",
    ]);
    let got = bounded::output(&mut held, bounded::LIMIT);
    let took = begun.elapsed();
    let err = got.map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    let err = match err {
        Ok(stdout) => {
            panic!("answered success with stdout {stdout:?} while a descendant held the pipe")
        }
        Err(e) => e,
    };
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{err}");
    assert!(err.to_string().contains("stdout"), "{err}");
    assert!(
        !err.to_string().contains("could not be stopped"),
        "the read handle was not dropped: {err}"
    );
    assert!(
        took < Duration::from_millis(7500),
        "the read handle was not dropped: {took:?}"
    );
    let mut full = Command::new("cmd.exe");
    full.args([
        "/C",
        "for",
        "/L",
        "%i",
        "in",
        "(1,1,3000)",
        "do",
        "@echo",
        "line-%i",
    ]);
    let out = bounded::output(&mut full, bounded::LIMIT).unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(text.lines().count(), 3000, "the whole output");
    assert_eq!(text.lines().last(), Some("line-3000"));
}

// Generous for loaded runners: a wait only runs this long when something is wrong.
const WAIT: Duration = Duration::from_secs(60);
const MUX_AGENT: &str = env!("CARGO_BIN_EXE_mux-agent");
// The pane's mouse reporting, as the multiplexer tracks it: `11` once the bar wrote ENABLE, `00` after DISABLE.
const MOUSE: &str = "#{mouse_any_flag}#{mouse_sgr_flag}";
/// One test's server and temp dir; dropping it ends the fake clients, kills the server and removes the dir.
struct Live {
    core: private_core::PrivateCore,
    socket: String,
    dir: PathBuf,
    root: String,
}

impl Live {
    /// A server whose one pane runs `cmd.exe`, or None (with a message) when rmux is not installed and not
    /// required.
    fn start() -> Option<Live> {
        let required = std::env::var("MUX_AGENTS_LIVE_REQUIRE")
            .unwrap_or_default()
            .split(',')
            .any(|m| m.trim() == "rmux");
        let path = std::env::var("PATH").unwrap_or_default();
        if mux_agent::mux::find_on_path("rmux.exe", &path).is_none() {
            assert!(!required, "MUX_AGENTS_LIVE_REQUIRE names rmux: not on PATH");
            eprintln!("skipped: no rmux on PATH");
            return None;
        }
        static N: AtomicU32 = AtomicU32::new(0);
        let socket = format!(
            "mxit-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        );
        let dir = std::env::temp_dir().join(&socket);
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["bin", "state"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        for alias in ["claude.exe", "copilot.exe"] {
            std::fs::copy(
                env!("CARGO_BIN_EXE_fake-client"),
                dir.join("bin").join(alias),
            )
            .unwrap();
            assert!(
                dir.join("bin").join(alias).is_file(),
                "{alias} alias installed"
            );
        }
        std::fs::write(dir.join("task.txt"), "say hi").unwrap();
        let mut path = dir.join("bin").into_os_string();
        path.push(";");
        path.push(Path::new(MUX_AGENT).parent().unwrap());
        if let Some(rest) = std::env::var_os("PATH") {
            path.push(";");
            path.push(rest);
        }
        let core =
            private_core::PrivateCore::new(private_core::PrivateCore::installed(), &dir, &path);
        // Built before any server starts, so a failed start still kills whatever began.
        let mut live = Live {
            core,
            socket,
            dir,
            root: String::new(),
        };
        match crate::bounded::output(live.rmux().arg("-V"), crate::bounded::LIMIT) {
            Ok(out) if out.status.success() => {
                eprintln!("rmux: {}", String::from_utf8_lossy(&out.stdout).trim());
            }
            other => {
                assert!(!required, "MUX_AGENTS_LIVE_REQUIRE names rmux: {other:?}");
                eprintln!("skipped: no rmux on PATH");
                return None;
            }
        }
        live.core.start();
        live.root = live.new_session("it");
        Some(live)
    }

    /// A detached session `name` whose one pane runs `cmd.exe` with the private core's environment and this test's
    /// state root (`MUX_AGENTS_STATE`); its pane id, once the prompt shows. Every session of a test is made here, so
    /// each gets the same environment as the first.
    fn new_session(&self, name: &str) -> String {
        let mut start = self.rmux();
        start.args(["new-session", "-d", "-s", name, "-x", "200", "-y", "50"]);
        for (key, value) in &self.core.env {
            start.arg("-e").arg(assign(key, value));
        }
        start.arg("-e").arg(assign(
            "MUX_AGENTS_STATE",
            self.dir.join("state").as_os_str(),
        ));
        let out = crate::bounded::output(start.arg("cmd.exe"), crate::bounded::LIMIT).unwrap();
        assert!(out.status.success(), "new-session {name}: {out:?}");
        let pane = self
            .call(&["list-panes", "-t", name, "-F", "#{pane_id}"])
            .trim()
            .to_string();
        self.prompt(&pane);
        pane
    }

    /// The server and every client need the same environment to resolve the named pipe.
    fn rmux(&self) -> Command {
        let mut command = Command::new("rmux");
        self.core.apply(&mut command);
        command.arg("-L").arg(&self.socket);
        command
    }

    /// `rmux -L <socket> <args>`; panics unless it succeeds, and answers its stdout.
    fn call(&self, args: &[&str]) -> String {
        let out = crate::bounded::output(self.rmux().args(args), crate::bounded::LIMIT).unwrap();
        assert!(out.status.success(), "rmux {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Types `text` literally into `pane`.
    fn keys(&self, pane: &str, text: &str) {
        self.call(&["send-keys", "-t", pane, "-l", text]);
    }

    /// Types `text` into `pane`, then Enter.
    fn line(&self, pane: &str, text: &str) {
        self.keys(pane, text);
        self.call(&["send-keys", "-t", pane, "Enter"]);
    }

    fn field(&self, pane: &str, format: &str) -> String {
        self.call(&["display-message", "-p", "-t", pane, format])
            .trim()
            .to_string()
    }

    /// The pane's screen, wrapped lines joined.
    fn screen(&self, pane: &str) -> String {
        self.call(&["capture-pane", "-p", "-J", "-t", pane])
    }

    /// Waits for `cmd.exe` in `pane` to show its prompt.
    fn prompt(&self, pane: &str) {
        self.wait("the cmd.exe prompt", || {
            self.screen(pane)
                .lines()
                .any(|l| l.trim_end().ends_with('>'))
                .then_some(())
        });
    }

    /// The active pane of the active window.
    fn focused(&self) -> String {
        self.call(&[
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}|#{window_active}|#{pane_active}",
        ])
        .lines()
        .filter(|r| r.ends_with("|1|1"))
        .map(|r| r.split('|').next().unwrap().to_string())
        .collect::<Vec<_>>()
        .join(",")
    }

    /// Makes the root pane the active one again.
    fn focus_root(&self) {
        let window = self.field(&self.root, "#{window_id}");
        self.call(&["select-window", "-t", &window]);
        self.call(&["select-pane", "-t", &self.root]);
    }

    /// Polls `ready` until it answers, for up to `WAIT`; panics naming `what` when it never does.
    fn wait<T>(&self, what: &str, ready: impl FnMut() -> Option<T>) -> T {
        self.wait_or(what, ready, String::new)
    }

    /// `wait`, whose panic also carries what `diagnose` says at the timeout.
    fn wait_or<T>(
        &self,
        what: &str,
        mut ready: impl FnMut() -> Option<T>,
        diagnose: impl FnOnce() -> String,
    ) -> T {
        let end = Instant::now() + WAIT;
        loop {
            if let Some(value) = ready() {
                return value;
            }
            if Instant::now() >= end {
                panic!("timed out waiting for {what}\n{}", diagnose());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// mux-agent typed into the root pane; its JSON reply, whatever it says.
    fn run_any(&self, args: &[&str]) -> Value {
        self.run_any_in(&self.root, args)
    }

    /// `run_any` typed into `pane` (cascade-3d call 26: a person's command from a pane in the team's session).
    fn run_any_in(&self, pane: &str, args: &[&str]) -> Value {
        let done = self.dir.join("call.done");
        let out = self.dir.join("call.json");
        let _ = std::fs::remove_file(&done);
        let args = args
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(" ");
        self.line(
            pane,
            &format!(
                "\"{MUX_AGENT}\" {args} > \"{}\" & echo done> \"{}\"",
                out.display(),
                done.display()
            ),
        );
        self.wait_or(
            &format!("command reply to {args}"),
            || done.exists().then_some(()),
            || format!("pane {pane}:\n{}", self.screen(pane)),
        );
        serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap()
    }

    /// `run_any`, whose reply must be `ok`.
    fn run(&self, args: &[&str]) -> Value {
        let v = self.run_any(args);
        assert_eq!(v["ok"], true, "{v}");
        v
    }

    fn spawn(&self, name: &str) -> Value {
        self.spawn_with(name, "")
    }

    /// `spawn -Client claude -TaskFile <dir>\task.txt -Name <name> <extra>` typed into the root pane; its JSON reply,
    /// which must be `ok`.
    fn spawn_with(&self, name: &str, extra: &str) -> Value {
        let file = |ext: &str| self.dir.join(format!("{name}.{ext}"));
        let line = format!(
            "\"{MUX_AGENT}\" spawn -Client claude -TaskFile \"{}\" -Name {name} {extra} > \"{}\" 2> \"{}\" & echo done> \"{}\"",
            self.dir.join("task.txt").display(),
            file("json").display(),
            file("err").display(),
            file("done").display(),
        );
        self.line(&self.root, &line);
        self.wait(&format!("spawn {name}"), || {
            file("done").exists().then_some(())
        });
        let text = std::fs::read_to_string(file("json")).unwrap();
        let err = std::fs::read_to_string(file("err")).unwrap_or_default();
        let reply: Value = serde_json::from_str(&text)
            .unwrap_or_else(|_| panic!("spawn {name}: stdout {text:?}, stderr {err:?}"));
        assert_eq!(reply["ok"], true, "spawn {name}: {reply} {err}");
        reply
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let core_stopped = self.core.stop();
        // The fake clients quit when this file appears; then the server and its panes go.
        let _ = std::fs::write(self.dir.join("quit"), "");
        std::thread::sleep(Duration::from_millis(500));
        let _ = crate::bounded::output(self.rmux().arg("kill-server"), crate::bounded::LIMIT);
        if !core_stopped {
            let message = format!(
                "private core stop failed; fixture kept at {}",
                self.dir.display()
            );
            if std::thread::panicking() {
                eprintln!("{message}");
                return;
            }
            panic!("{message}");
        }
        if self.dir.join("core-stop-failed").exists() {
            return;
        }
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.dir).is_ok() || !self.dir.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

/// `NAME=value` for `new-session -e`.
fn assign(name: &str, value: &OsStr) -> OsString {
    let mut text = OsString::from(format!("{name}="));
    text.push(value);
    text
}

fn s(value: &Value) -> String {
    value
        .as_str()
        .unwrap_or_else(|| panic!("not a string: {value}"))
        .to_string()
}

/// The 1-based line of `screen` whose second token is `name`: that agent's name line (`◉`/`◌`, then the name).
fn row_at(screen: &str, name: &str) -> Option<usize> {
    screen
        .lines()
        .position(|l| l.split_whitespace().nth(1) == Some(name))
        .map(|i| i + 1)
}

/// Polls `ready` up to `limit`; false when it never held.
fn eventually(limit: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + limit;
    loop {
        if ready() {
            return true;
        }
        if Instant::now() > end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn the_harness_installs_both_client_aliases() {
    let Some(live) = Live::start() else { return };
    for alias in ["claude.exe", "copilot.exe"] {
        assert!(
            live.dir.join("bin").join(alias).is_file(),
            "{alias} alias installed"
        );
    }
}

#[test]
fn hc_worker_and_view_close_survives_focus_stop() {
    let Some(live) = Live::start() else {
        return;
    };
    live.run(&["sidebar", "-Close"]);
    let w = live.spawn("w1");
    let pid: u32 = live.wait("worker session", || {
        std::fs::read_to_string(live.dir.join("clients.log"))
            .ok()?
            .lines()
            .find_map(|p| {
                let r: Value = serde_json::from_str(
                    &std::fs::read_to_string(live.dir.join(format!("client.{p}.json"))).ok()?,
                )
                .ok()?;
                (r["sessionId"] == w["sessionId"])
                    .then(|| p.parse().ok())
                    .flatten()
            })
    });
    let worker: Value = serde_json::from_str(
        &std::fs::read_to_string(live.dir.join(format!("client.{pid}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(worker["env"]["HC_TERMINAL_ID"], w["hcAddress"]);
    assert_eq!(worker["env"]["MUX_AGENTS_GEN"], "0");
    assert!(worker["env"]["TMUX"].is_null() && worker["env"]["RMUX"].is_null());
    std::fs::write(live.dir.join("status.txt"), "checking core").unwrap();
    for (n, (command, key, want)) in [
        (
            "status\t-BodyFile\t{dir}/status.txt",
            "text",
            serde_json::json!("checking core"),
        ),
        (
            "report\tblocked\t-Kind\tquestion",
            "state",
            serde_json::json!("blocked"),
        ),
        (
            "send\tparent\t-BodyFile\t{dir}/status.txt",
            "from",
            serde_json::json!("w1"),
        ),
        ("read", "ids", serde_json::json!([])),
    ]
    .into_iter()
    .enumerate()
    {
        let held = live.dir.join(format!("cmd.{pid}.part"));
        std::fs::write(&held, format!("{command}\n")).unwrap();
        std::fs::rename(held, live.dir.join(format!("cmd.{pid}"))).unwrap();
        let text = live.wait("worker reply", || {
            std::fs::read_to_string(live.dir.join(format!("reply.{pid}.{}.json", n + 1))).ok()
        });
        let r: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r[key], want);
    }
    let first = live.run(&["read"]);
    assert_eq!(first["ids"].as_array().unwrap().len(), 1, "{first}");
    let address = s(&w["hcAddress"]);
    let get =
        || private_core::run(live.core.command(&["get", &address, "--json"]), &live.dir).unwrap();
    let before = get();
    live.call(&["kill-pane", "-t", &s(&w["pane"])]);
    let list = live.run(&["list", "-All"]);
    let row = &list["agents"][0];
    assert_eq!(row["life"], "running");
    assert_eq!(row["view"], "detached");
    assert!(row["pane"].is_null());
    // Nothing is consumed without an ack (#103 4b-i): acking the first batch leaves nothing new.
    let batch = s(&first["batch"]);
    assert_eq!(
        live.run(&["read", "-Deliver", "-Ack", &batch])["ids"],
        serde_json::json!([])
    );
    assert_eq!(get()["state"], "live");
    let focus = live.run(&["focus", "w1"]);
    assert_ne!(focus["pane"], w["pane"]);
    assert_eq!(live.focused(), s(&focus["pane"]));
    let list = live.run(&["list"]);
    assert_eq!(list["agents"][0]["sessionId"], w["sessionId"]);
    assert_eq!(get()["argv"], before["argv"]);
    assert_eq!(
        live.run(&["stop", "w1"]),
        serde_json::json!({"ok":true,"verb":"stop","name":"w1","hcAddress":address,"life":"ended","stopped":true,"unresolved":[]})
    );
    live.wait("finished core terminal", || {
        (get()["state"] == "finished").then_some(())
    });
}

// A worker whose view is closed still reads its own mail (#104 phase 1c): the person sends to it after the view pane
// is gone, and `read` run by the worker in its hc terminal, which has no TMUX or RMUX, returns that message.
#[test]
fn hc_worker_reads_after_its_view_closes() {
    let Some(live) = Live::start() else {
        return;
    };
    live.run(&["sidebar", "-Close"]);
    let w = live.spawn("w1");
    let pid: u32 = live.wait("worker session", || {
        std::fs::read_to_string(live.dir.join("clients.log"))
            .ok()?
            .lines()
            .find_map(|p| {
                let r: Value = serde_json::from_str(
                    &std::fs::read_to_string(live.dir.join(format!("client.{p}.json"))).ok()?,
                )
                .ok()?;
                (r["sessionId"] == w["sessionId"])
                    .then(|| p.parse().ok())
                    .flatten()
            })
    });
    let worker: Value = serde_json::from_str(
        &std::fs::read_to_string(live.dir.join(format!("client.{pid}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(worker["env"]["HC_TERMINAL_ID"], w["hcAddress"]);
    assert_eq!(worker["env"]["MUX_AGENTS_GEN"], "0");
    assert!(worker["env"]["TMUX"].is_null() && worker["env"]["RMUX"].is_null());
    live.call(&["kill-pane", "-t", &s(&w["pane"])]);
    live.wait("the view pane to close", || {
        (!live
            .call(&["list-panes", "-a", "-F", "#{pane_id}"])
            .lines()
            .any(|p| p == s(&w["pane"])))
        .then_some(())
    });
    live.run(&["send", "w1", "-Body", "after the view closed"]);
    // The worker runs `read` in its hc terminal, twice: the message, then (acking it) nothing left.
    let read = |n: u32, cmd: &str| -> Value {
        let held = live.dir.join(format!("cmd.{pid}.part"));
        std::fs::write(&held, format!("{cmd}\n")).unwrap();
        std::fs::rename(held, live.dir.join(format!("cmd.{pid}"))).unwrap();
        let text = live.wait("worker read", || {
            std::fs::read_to_string(live.dir.join(format!("reply.{pid}.{n}.json"))).ok()
        });
        serde_json::from_str(&text).unwrap()
    };
    let first = read(1, "read");
    assert_eq!(first["ok"], true, "{first}");
    // A served batch (#103 4b-i) is one `text`: each entry starts at its header and ends in LF.
    let text = s(&first["text"]);
    let bodies: Vec<&str> = text
        .split("[mux-agent from ")
        .skip(1)
        .map(|e| {
            e.trim_end_matches('\n')
                .split_once('\n')
                .map_or("", |b| b.1)
        })
        .collect();
    assert_eq!(bodies, ["after the view closed"], "{first}");
    // Nothing is consumed without an ack: the second read acks the first batch and finds nothing new.
    let again = read(2, &format!("read\t-Deliver\t-Ack\t{}", s(&first["batch"])));
    assert_eq!(again["ok"], true, "{again}");
    assert_eq!(again["ids"], serde_json::json!([]), "{again}");
}

// Stop an agent, then resume it (#103): the reply names X as `from` and the successor Y (same name, a new hc
// terminal); the client in Y's hc terminal runs `--resume <X's session>` as the last arguments, with no task after
// them; Y's record names X in `resumedFrom`; and the reply's `sidebar` is a live bar in the window of Y's view
// (#122: a bar per window).
#[test]
fn hc_stop_then_resume_returns_with_its_conversation() {
    let Some(live) = Live::start() else {
        return;
    };
    let w = live.spawn("w1");
    let sid = s(&w["sessionId"]);
    ready::wait_conversation(&live.dir, &sid); // PM call 12
    let address = s(&w["hcAddress"]);
    let get = |address: &str| {
        private_core::run(live.core.command(&["get", address, "--json"]), &live.dir).unwrap()
    };
    assert_eq!(
        live.run(&["stop", "w1"]),
        serde_json::json!({"ok":true,"verb":"stop","name":"w1","hcAddress":address,"life":"ended","stopped":true,"unresolved":[]})
    );
    live.wait("finished core terminal", || {
        (get(&address)["state"] == "finished").then_some(())
    });
    let resumed = live.run(&["resume", "w1"]);
    assert_eq!(resumed["from"], w["id"], "{resumed}");
    let y = &resumed["agent"];
    assert_eq!(y["name"], "w1", "{resumed}");
    assert_ne!(y["id"], w["id"], "{resumed}");
    let y_address = s(&y["hcAddress"]);
    assert_ne!(y_address, address, "{resumed}");
    assert_eq!(get(&y_address)["state"], "live");
    // The client hc started in Y's terminal: the fake client's record whose HC_TERMINAL_ID is Y's address.
    let client: Value = live.wait("the resumed client", || {
        std::fs::read_to_string(live.dir.join("clients.log"))
            .ok()?
            .lines()
            .find_map(|p| {
                let r: Value = serde_json::from_str(
                    &std::fs::read_to_string(live.dir.join(format!("client.{}.json", p.trim())))
                        .ok()?,
                )
                .ok()?;
                (r["env"]["HC_TERMINAL_ID"] == y["hcAddress"]).then_some(r)
            })
    });
    let argv: Vec<String> = client["argv"].as_array().unwrap().iter().map(s).collect();
    // notebook-7b V1: the resumed turn is the notebook block.
    assert!(argv.len() >= 3, "{argv:?}");
    assert_eq!(
        argv[argv.len() - 3..],
        [
            "--resume".to_string(),
            sid.clone(),
            mux_agent::agents::notebook::news::HEAD.to_string()
        ],
        "{argv:?}"
    );
    assert!(!argv.iter().any(|a| a == "--fork-session"), "{argv:?}");
    // Y's record, in its server's folder of the state root.
    let record: Value = std::fs::read_dir(live.dir.join("state"))
        .unwrap()
        .find_map(|server| {
            let path = server
                .ok()?
                .path()
                .join("agents")
                .join(s(&y["id"]))
                .join("record.json");
            serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
        })
        .expect("Y's record");
    // V1: Y's view is in view.json, never in its record.
    let view = member_file(&live, &s(&y["id"]), "view.json");
    assert_eq!(record["resumedFrom"], w["id"]);
    assert_eq!(record["sessionId"], w["sessionId"]);
    // The bar kept by the resume: a live pane carrying the side-bar label, in the window of Y's view.
    let bar = s(&resumed["sidebar"]);
    assert!(resumed.get("sidebarError").is_none(), "{resumed}");
    assert_eq!(live.field(&bar, "#{pane_dead}"), "0", "{resumed}");
    assert_eq!(live.field(&bar, "#{@agent-role}"), "sidebar", "{resumed}");
    assert_eq!(
        live.field(&bar, "#{window_id}"),
        s(&view["window"]),
        "{resumed}"
    );
    assert_eq!(
        live.field(&s(&view["pane"]), "#{window_id}"),
        s(&view["window"])
    );
}

// A client whose running image an update renamed (claude.exe.old.<ms>.<pid>) is still the caller's client: `list` run
// by it answers ok instead of refusing `caller`.
#[test]
fn a_client_renamed_by_an_update_still_calls() {
    let Some(live) = Live::start() else {
        return;
    };
    live.line(&live.root, "claude");
    let log = live.dir.join("clients.log");
    let pid: u32 = live.wait("the client to start", || {
        std::fs::read_to_string(&log)
            .ok()?
            .lines()
            .next()?
            .trim()
            .parse()
            .ok()
    });
    let bin = live.dir.join("bin");
    std::fs::rename(
        bin.join("claude.exe"),
        bin.join(format!("claude.exe.old.1794521471990.{pid}")),
    )
    .unwrap();
    let held = live.dir.join(format!("cmd.{pid}.part"));
    std::fs::write(&held, "list\n").unwrap();
    std::fs::rename(&held, live.dir.join(format!("cmd.{pid}"))).unwrap();
    let reply = live.dir.join(format!("reply.{pid}.1.json"));
    let text = live.wait("the list reply", || std::fs::read_to_string(&reply).ok());
    let err =
        std::fs::read_to_string(live.dir.join(format!("reply.{pid}.1.err"))).unwrap_or_default();
    let answer: Value = serde_json::from_str(&text)
        .unwrap_or_else(|_| panic!("list: stdout {text:?}, stderr {err:?}"));
    assert_eq!(answer["ok"], true, "list: {answer} {err}");
}

// #70: on Windows the bar reads its console: it turns mouse reports on, `j` then Enter focuses the first agent, and
// a left click on an agent's name line focuses that agent.
#[test]
fn bar_keys_and_a_click_focus_an_agent() {
    let Some(live) = Live::start() else {
        return;
    };
    let w1 = live.spawn("w1");
    let w2 = live.spawn("w2");
    let bar = s(&w1["sidebar"]);
    let (p1, p2) = (s(&w1["pane"]), s(&w2["pane"]));
    live.wait("both agents on the bar", || {
        let screen = live.screen(&bar);
        (row_at(&screen, "w1").is_some() && row_at(&screen, "w2").is_some()).then_some(())
    });
    assert_eq!(
        live.field(&bar, MOUSE),
        "11",
        "the bar turned mouse reports on"
    );

    live.focus_root();
    live.keys(&bar, "j");
    live.call(&["send-keys", "-t", &bar, "Enter"]);
    live.wait("j then Enter to focus w1", || {
        (live.focused() == p1).then_some(())
    });

    // A press read within 50 ms of a frame is dropped, as the user may have aimed at the frame before it (sidebar.rs
    // PRESS_GAP). So each try presses just after a redraw, when the next frame is about 1.9 s away: the bar's text
    // changes with every frame while its agents' ages count seconds. A bar busy between frames can still read a press
    // late, so the tries go on until WAIT (#103: the tree tests now run beside this one).
    let mut clicked = false;
    let deadline = Instant::now() + WAIT;
    while !clicked && Instant::now() < deadline {
        live.focus_root();
        let before = live.screen(&bar);
        eventually(Duration::from_secs(3), || live.screen(&bar) != before);
        std::thread::sleep(Duration::from_millis(100));
        let screen = live.screen(&bar);
        let y = row_at(&screen, "w2").unwrap_or_else(|| panic!("no w2 row:\n{screen}"));
        for end in ['M', 'm'] {
            live.keys(&bar, &format!("\x1b[<0;3;{y}{end}"));
        }
        clicked = eventually(Duration::from_secs(3), || live.focused() == p2);
    }
    assert!(
        clicked,
        "a click on w2's row never focused {p2}: {}",
        live.focused()
    );
    assert_eq!(live.field(&bar, MOUSE), "11", "the bar still reads clicks");
}

// #70: Ctrl-C typed into a `watch` run by hand ends it `interrupted`, turns mouse reports off, and leaves the shell
// taking typed lines again.
#[test]
fn ctrl_c_ends_a_hand_run_watch_and_gives_the_console_back() {
    let Some(live) = Live::start() else {
        return;
    };
    let pane = live
        .call(&[
            "split-window",
            "-v",
            "-t",
            &live.root,
            "-P",
            "-F",
            "#{pane_id}",
            "cmd.exe",
        ])
        .trim()
        .to_string();
    live.prompt(&pane);
    // A split does not get the session's `-e` variables in rmux 0.10: the state root is set in the pane itself.
    live.line(
        &pane,
        &format!("set MUX_AGENTS_STATE={}", live.dir.join("state").display()),
    );
    live.line(&pane, &format!("\"{MUX_AGENT}\" watch"));
    live.wait("watch to turn mouse reports on", || {
        (live.field(&pane, MOUSE) == "11").then_some(())
    });

    live.call(&["send-keys", "-t", &pane, "C-c"]);
    live.wait("watch to end interrupted", || {
        live.screen(&pane)
            .contains(r#""ended":"interrupted""#)
            .then_some(())
    });
    assert_eq!(live.field(&pane, MOUSE), "00", "mouse reports off again");

    live.line(&pane, "echo typed-after-watch");
    live.wait("the shell to take a typed line", || {
        live.screen(&pane)
            .lines()
            .any(|l| l.trim() == "typed-after-watch")
            .then_some(())
    });
}

#[test]
fn private_core_windows() {
    let Some(live) = Live::start() else {
        return;
    };
    let out = live.dir.join("scope.txt");
    let done = live.dir.join("scope.done");
    live.line(
        &live.root,
        &format!(
            "set > \"{}\" & echo done> \"{}\"",
            out.display(),
            done.display()
        ),
    );
    live.wait("private core env", || done.exists().then_some(()));
    let env = std::fs::read_to_string(out).unwrap();
    for (key, value) in live
        .core
        .env
        .iter()
        .filter(|(k, _)| ["HC_SOCKET", "HC_SESSION", "HOME", "XDG_CONFIG_HOME"].contains(k))
    {
        assert!(
            env.lines()
                .any(|l| l.eq_ignore_ascii_case(&format!("{key}={}", value.to_string_lossy())))
        );
    }
    let d = live.dir.clone();
    let probe = live.core.command(&["server", "stop", "--json"]);
    drop(live);
    std::fs::create_dir_all(&d).unwrap();
    assert_eq!(private_core::run(probe, &d).unwrap()["stopped"], false);
    std::fs::remove_dir_all(d).unwrap();
}

/// The hostile settings: renumbered windows, kept dead panes, and a default command that leaves a marker.
fn hostile(live: &Live) -> PathBuf {
    let marker = live.dir.join("marker");
    live.call(&["set-option", "-g", "renumber-windows", "on"]);
    live.call(&["set-option", "-g", "remain-on-exit", "on"]);
    live.call(&[
        "set-option",
        "-g",
        "default-command",
        &format!("cmd /c type nul > \"{}\" & claude", marker.display()),
    ]);
    marker
}

/// The first fake client whose argv `matches`: its `client.<pid>.json` record.
fn client_where(live: &Live, what: &str, matches: impl Fn(&[String]) -> bool) -> Value {
    live.wait(what, || {
        std::fs::read_to_string(live.dir.join("clients.log"))
            .ok()?
            .lines()
            .find_map(|p| {
                let path = live.dir.join(format!("client.{}.json", p.trim()));
                let r: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
                let argv: Vec<String> = r["argv"].as_array()?.iter().map(s).collect();
                matches(&argv).then_some(r)
            })
    })
}

// options-6e E1 on rmux for Windows: an unknown flag makes no session; options, the recipe's session, then prompt.
#[test]
fn up_runs_the_leader_with_options_then_session_then_prompt() {
    let Some(live) = Live::start() else {
        return;
    };
    let sessions = || live.call(&["list-sessions", "-F", "#{session_id}"]);
    let before = sessions();
    let refused = live.run_any(&["up", "-Client", "claude", "--", "fix", "--frobnicate"]);
    assert_eq!(refused["check"], "usage", "{refused}");
    assert_eq!(sessions(), before, "a refused up makes no session");
    let up = live.run(&[
        "up",
        "-Client",
        "claude",
        "--",
        "fix",
        "--resume",
        "SEL9",
        "--model",
        "x",
        "--session-id=SEL8",
    ]);
    assert!(up["leader"]["id"].is_string(), "{up}");
    let leader = client_where(&live, "the leader", |a| a.iter().any(|t| t == "fix"));
    let sid = s(&leader["sessionId"]);
    assert_eq!(
        leader["argv"],
        serde_json::json!(["--model", "x", "--session-id", sid, "fix"])
    );
}

// occupy-3 H4.5 live: a claude.exe this user cannot open (an elevated client; here a fake-client copy whose queries
// are denied, the same Toolhelp-only row) blocks `mux-agent resume` of an ended Claude member, naming its pid; once
// it has left the Toolhelp table the resume is admitted. The deny comes before the server starts: `deny_queries`
// drops this process's SeDebugPrivilege, and the rmux server and the pane's mux-agent copy the token as it is then.
#[test]
fn an_unreadable_claude_exe_blocks_resume() {
    let elevated = std::env::temp_dir().join(format!("elevated-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&elevated);
    std::fs::create_dir_all(elevated.join("it")).unwrap();
    let exe = elevated.join("claude.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_fake-client"), &exe).unwrap();
    let mut copy = std::process::Command::new(&exe);
    copy.env("MUX_IT_DIR", elevated.join("it"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // outcome-2 call 14: bounded; the watchdog kills it at 300 s if the test never does.
    let mut child = bounded::spawn(&mut copy, Duration::from_secs(300)).unwrap();
    let pid = child.id();
    native::deny_queries_on(child.handle());
    assert!(
        mux_agent::proc::host().row(pid).is_none(),
        "precondition, the deny took effect: {}",
        native::deny_report_on(child.handle(), pid)
    );
    let Some(live) = Live::start() else {
        return;
    };
    let w = live.spawn("w1");
    let address = s(&w["hcAddress"]);
    live.run(&["stop", "w1"]);
    live.wait("finished core terminal", || {
        let got = private_core::run(live.core.command(&["get", &address, "--json"]), &live.dir);
        (got.unwrap()["state"] == "finished").then_some(())
    });
    let refused = live.run_any(&["resume", "w1"]);
    assert_eq!(refused["check"], "session", "{refused}");
    // PM call 2: this copy's pid and image, and the unbound rule.
    let message = s(&refused["message"]);
    assert!(
        message.starts_with(&format!("process {pid} (claude.exe)")),
        "{message}"
    );
    assert!(
        message.contains(mux_agent::agents::occupancy::Rule::Unbound.phrase()),
        "{message}"
    );
    let _ = child.kill();
    let _ = child.wait();
    native::within_10_s("the copy leaves Toolhelp", || {
        (!mux_agent::proc::host()
            .table()
            .unwrap()
            .iter()
            .any(|r| r.pid == pid))
        .then_some(())
    });
    live.run(&["resume", "w1"]);
    let _ = std::fs::remove_dir_all(&elevated);
}

// occupy-3 H2 live on Windows: managed w2's newest report naming w1's ended conversation holds it. Copilot is not
// spawned by this harness (`spawn_with` passes `-Client claude`), so the twin of the Unix Copilot move uses a Claude
// member; the rule (the newest report of a managed client decides) is kind-independent. w1's terminal is finished
// first, so w1's own client is not the holder named.
#[test]
fn a_managed_member_resumed_into_a_conversation_holds_it() {
    let Some(live) = Live::start() else {
        return;
    };
    let (w1, w2) = (live.spawn("w1"), live.spawn("w2"));
    let (sid1, sid2) = (s(&w1["sessionId"]), s(&w2["sessionId"]));
    let address = s(&w1["hcAddress"]);
    live.run(&["stop", "w1"]);
    live.wait("finished core terminal", || {
        let got = private_core::run(live.core.command(&["get", &address, "--json"]), &live.dir);
        (got.unwrap()["state"] == "finished").then_some(())
    });
    let pid = native::client_at(&live.dir, &s(&w2["hcAddress"]));
    native::startup_report(&live.dir, pid);
    let line = |sid: &str| format!("report\t-Session\t{sid}\t-Source\tresume");
    let r = native::client_cmd(&live.dir, pid, &line(&sid1));
    assert_eq!(
        r["client"]["pid"], pid,
        "the walk found w2's own client: {r}"
    );
    let refused = live.run_any(&["resume", "w1"]);
    assert_eq!(refused["check"], "session", "{refused}");
    // PM call 2: w2's client, its image, and the managed rule.
    let message = s(&refused["message"]);
    assert!(
        message.starts_with(&format!("process {pid} (claude.exe)")),
        "{message}"
    );
    assert!(
        message.contains(mux_agent::agents::occupancy::Rule::Managed.phrase()),
        "{message}"
    );
    native::client_cmd(&live.dir, pid, &line(&sid2));
    live.run(&["resume", "w1"]);
}

// options-6e live on rmux for Windows: started with `--model x` and a task, stopped, its record seeded as in the
// task text, resumed: argv `--model x --resume <sid>`, no task.
#[test]
fn hc_stopped_member_resumes_with_its_stored_options_only() {
    let Some(live) = Live::start() else {
        return;
    };
    let w = live.spawn_with("w1", "-Model x");
    let (sid, address) = (s(&w["sessionId"]), s(&w["hcAddress"]));
    let first = client_where(&live, "the first client", |a| a.contains(&sid));
    let first: Vec<String> = first["argv"].as_array().unwrap().iter().map(s).collect();
    assert!(
        first.windows(2).any(|p| p[0] == "--model" && p[1] == "x"),
        "started with --model x: {first:?}"
    );
    ready::wait_conversation(&live.dir, &sid); // PM call 12
    let conv = live.dir.join(format!("conversation.{sid}.log"));
    let r = member_file(&live, &s(&w["id"]), "record.json");
    let options: Vec<String> = r["options"].as_array().unwrap().iter().map(s).collect();
    assert_eq!(r["lifecycle"], 1, "written by forward, not seeded");
    assert!(r["prompt"].is_array(), "{r}");
    assert!(
        options.windows(2).any(|p| p[0] == "--model" && p[1] == "x"),
        "--model x stored: {options:?}"
    );
    let get =
        |a: &str| private_core::run(live.core.command(&["get", a, "--json"]), &live.dir).unwrap();
    assert_eq!(live.run(&["stop", "w1"])["life"], "ended");
    live.wait("finished core terminal", || {
        (get(&address)["state"] == "finished").then_some(())
    });
    let resumed = live.run(&["resume", "w1"]);
    let resumed_client = client_where(&live, "the resumed client", |a| {
        a.windows(2).any(|p| p[0] == "--resume" && p[1] == sid)
    });
    let y = member_file(&live, &s(&resumed["agent"]["id"]), "record.json");
    assert_eq!(
        s(&y["sessionId"]),
        sid,
        "the successor keeps the conversation"
    );
    let mut want = options.clone();
    // notebook-7b: a resumed client's first text is the notebook news block.
    want.extend([
        "--resume".to_string(),
        sid.clone(),
        mux_agent::agents::notebook::news::HEAD.to_string(),
    ]);
    assert_eq!(
        resumed_client["argv"],
        serde_json::json!(want),
        "the stored options only"
    );
    let lines: Vec<String> = std::fs::read_to_string(&conv)
        .unwrap()
        .lines()
        .map(|l| l.split(' ').nth(1).unwrap_or("").to_string())
        .collect();
    assert_eq!(
        lines,
        ["start", "resume"],
        "reloaded, not refused: an unknown resume publishes no client"
    );
    assert_ne!(y["hcName"], r["hcName"], "a fresh hc name");
}

/// The member folders under every server key of the state root.
fn agents_of(live: &Live) -> Vec<PathBuf> {
    let state = live.dir.join("state");
    let keys = std::fs::read_dir(&state).into_iter().flatten().flatten();
    keys.flat_map(|k| {
        std::fs::read_dir(k.path().join("agents"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
    })
    .collect()
}

// launch-1 live on rmux for Windows (ambiguity 14): a dropped `hc run` reply. One child cmd sets MUX_AGENTS_CRASH=run
// and runs spawn, which dies after `hc run` returns. Nothing runs; the launcher gives up; a retry in the same console
// (no variable: it never leaked) runs exactly one client.
#[test]
fn a_dropped_run_reply_runs_nothing_then_a_retry_runs_once() {
    let Some(live) = Live::start() else {
        return;
    };
    std::fs::write(live.dir.join("task.txt"), "say hi").unwrap();
    let log = live.dir.join("clients.log");
    let clients = || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .count()
    };
    let before = clients();
    let (script, out, done) = (
        live.dir.join("crash.cmd"),
        live.dir.join("crash.out"),
        live.dir.join("crash.done"),
    );
    let body = format!(
        "@set MUX_AGENTS_CRASH=run\r\n@\"{MUX_AGENT}\" spawn -Client claude -TaskFile \"{}\" -Name w1 > \"{}\" 2>&1\r\n",
        live.dir.join("task.txt").display(),
        out.display()
    );
    std::fs::write(&script, body).unwrap();
    live.line(
        &live.root,
        &format!(
            "cmd /c \"{}\" & echo done> \"{}\"",
            script.display(),
            done.display()
        ),
    );
    live.wait("the crashed spawn", || done.exists().then_some(()));
    let text = std::fs::read_to_string(&out).unwrap_or_default();
    assert!(
        serde_json::from_str::<Value>(&text).is_err(),
        "no reply: the spawn died after hc run: {text:?}"
    );
    let ids = agents_of(&live);
    assert_eq!(ids.len(), 1, "one member folder: {ids:?}");
    let commit = ids[0].join("gens").join("0").join("commit.json");
    live.wait("the launcher's give-up", || {
        let c: Value = serde_json::from_str(&std::fs::read_to_string(&commit).ok()?).ok()?;
        (c["aborted"] == true).then_some(())
    });
    assert_eq!(clients(), before, "nothing ran");
    live.spawn("w1");
    live.wait("the retry's client", || {
        (clients() == before + 1).then_some(())
    });
    assert_eq!(clients(), before + 1, "the retry ran exactly one client");
}

// proof-0b K2 on rmux for Windows (4to6 #1, #3, #4, #19): a team, its leader spawns w2 with a brief; proof waited
// (PM call 26); with the server killed (RMUX unset in hc clients, only rmux on PATH) the leader sends a file, w2 reads
// and reports, focus is refused `views`; list and stop run with views down.
#[test]
fn members_mail_without_the_multiplexer() {
    let Some(live) = Live::start() else {
        return;
    };
    let file = |name: &str, text: &str| {
        let p = live.dir.join(name);
        std::fs::write(&p, text).unwrap();
        p.display().to_string()
    };
    let (task, brief, note) = (
        file("task.txt", "say hi"),
        file("brief.md", "brief"),
        file("note.md", "without the views"),
    );
    let lead = s(&live.run(&["up", "-Client", "claude"])["leader"]["id"]);
    let mut sent = HashMap::new();
    let args = [
        "spawn",
        "-Client",
        "claude",
        "-TaskFile",
        &task,
        "-BriefFile",
        &brief,
        "-Context",
        "brief",
        "-Name",
        "w2",
    ];
    let w2 = by(&live, &mut sent, &lead, &args);
    assert_eq!(w2["ok"], true, "{w2}");
    let w2 = s(&w2["id"]);
    for id in [&lead, &w2] {
        let dir = member_dir(&live, id);
        let client = ready::wait_proof(&dir, 0);
        assert!(client["pid"].is_u64(), "{id}: {client}");
        let launcher: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("gens").join("0").join("launcher.json")).unwrap(),
        )
        .unwrap();
        assert!(launcher.get("client").is_none(), "{id}: no client field");
    }
    let sid = |id: &str| s(&member_file(&live, id, "record.json")["sessionId"]);
    let (p1, p2) = (
        client_pid(&live, &sid(&lead), 0),
        client_pid(&live, &sid(&w2), 0),
    );
    // Checklist 12: every child here is bounded (o2 call 14's runner).
    let _ = bounded::output(live.rmux().arg("kill-server"), bounded::LIMIT);
    live.wait("the server gone", || {
        let out = bounded::output(live.rmux().arg("list-sessions"), bounded::LIMIT).ok()?;
        (!out.status.success()).then_some(())
    });
    let reply = by_pid(&live, &mut sent, p1, &["send", "w2", "-BodyFile", &note]);
    assert_eq!(reply["ok"], true, "{reply}");
    let read = by_pid(&live, &mut sent, p2, &["read"]);
    assert!(read.to_string().contains("without the views"), "{read}");
    assert_eq!(
        by_pid(
            &live,
            &mut sent,
            p2,
            &["report", "blocked", "-Kind", "question"]
        )["state"],
        "blocked"
    );
    assert_eq!(
        by_pid(&live, &mut sent, p1, &["focus", "w2"])["check"],
        "views"
    );
    let list = by_pid(&live, &mut sent, p1, &["list"]);
    let row = list["agents"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["name"] == "w2"))
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        list["ok"], true,
        "list runs with views down (PM call 18): {list}"
    );
    assert!(
        !row.is_null() && row["pane"].is_null() && row["window"].is_null(),
        "w2 listed without pane fields: {list}"
    );
    assert_eq!(
        by_pid(&live, &mut sent, p1, &["stop", "w2"])["life"],
        "ended",
        "stop needs no view (4to7re #15)"
    );
}

#[test]
fn up_creates_a_team_in_a_new_session() {
    let Some(live) = Live::start() else {
        return;
    };
    let marker = hostile(&live);
    let up = live.run(&["up", "-Client", "claude"]);
    let get = private_core::run(
        live.core
            .command(&["get", &s(&up["leader"]["hcAddress"]), "--json"]),
        &live.dir,
    )
    .unwrap();
    assert!(get["state"] == "live" && up["sidebar"].is_string(), "{up}");
    // No client is attached here (the Windows harness has none), so only switch-client may warn: the bar and the
    // team record must not. The switch itself is covered by the unit tests and the Unix live test on tmux.
    assert!(
        up.get("sidebarError").is_none() && up.get("teamRecordError").is_none(),
        "{up}"
    );
    assert!(eventually(Duration::from_secs(10), || live
        .call(&[
            "list-windows",
            "-t",
            &s(&up["session"]),
            "-F",
            "#{window_name}|#{window_active}"
        ])
        .trim()
        == "leader|1"));
    assert!(!marker.exists());
    // 6b ambiguity 8 (r6b call 7): a second `up` reconciles the team; nothing is lost, nothing restored.
    let again = live.run_any(&["up", "-Client", "claude"]);
    assert_eq!(
        (&again["created"], &again["restored"]),
        (&serde_json::json!(false), &serde_json::json!([])),
        "{again}"
    );
}

#[cfg(feature = "test-panic")]
#[test]
fn up_whose_view_panics_keeps_the_leader_under_hostile_settings() {
    let Some(live) = Live::start() else {
        return;
    };
    let marker = hostile(&live);
    live.line(&live.root, "set MUX_AGENTS_TEST_PANIC=launch");
    live.prompt(&live.root);
    let reply = live.run_any(&["up", "-Client", "claude"]);
    // The panic fires inside the view step, after the commit: up succeeds with viewError (PM calls 1, 10).
    assert_eq!(reply["ok"], serde_json::json!(true), "{reply}");
    assert!(
        reply["viewError"]
            .as_str()
            .is_some_and(|e| e.contains("panicked")),
        "{reply}"
    );
    assert!(eventually(Duration::from_secs(10), || live
        .call(&["list-sessions", "-F", "#{session_name}"])
        .lines()
        .count()
        == 2));
    assert!(!marker.exists());
}

/// `file` of member `id`, under whichever server folder of the state root holds it.
fn member_file(live: &Live, id: &str, file: &str) -> Value {
    let state = live.dir.join("state");
    let path = std::fs::read_dir(&state)
        .unwrap()
        .map(|e| e.unwrap().path().join("agents").join(id).join(file))
        .find(|p| p.is_file())
        .unwrap();
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The folder of member `id`, under whichever server key of the state root holds it.
fn member_dir(live: &Live, id: &str) -> PathBuf {
    let state = live.dir.join("state");
    live.wait("member folder", || {
        let mut keys = std::fs::read_dir(&state).ok()?.flatten();
        keys.find_map(|k| {
            Some(k.path().join("agents").join(id)).filter(|p| p.join("record.json").is_file())
        })
    })
}

/// PM call 26: waits for member `id`'s proof evidence before it is commanded.
fn proof_of(live: &Live, id: &str) {
    ready::wait_member(&member_dir(live, id));
}

/// mux-agent run by member `id`'s fake client (found by its record's session); its reply.
fn by(live: &Live, sent: &mut HashMap<u32, usize>, id: &str, args: &[&str]) -> Value {
    proof_of(live, id);
    let session = s(&member_file(live, id, "record.json")["sessionId"]);
    by_pid(live, sent, client_pid(live, &session, 0), args)
}

/// The pid of the newest fake client of `session` (by `sessionId` or argv) other than `skip` (`msg-plan-4b-i-3.md`
/// F20): a resumed member's client answers, never its dead predecessor.
fn client_pid(live: &Live, session: &str, skip: u32) -> u32 {
    live.wait("member client", || {
        let log = std::fs::read_to_string(live.dir.join("clients.log")).ok()?;
        log.lines().rev().find_map(|p| {
            let text = std::fs::read_to_string(live.dir.join(format!("client.{p}.json"))).ok()?;
            let r: Value = serde_json::from_str(&text).ok()?;
            let mine = r["sessionId"] == session
                || r["argv"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|x| x == session));
            let pid: u32 = p.parse().ok()?;
            (mine && pid != skip).then_some(pid)
        })
    })
}

/// `by` for a known client pid.
fn by_pid(live: &Live, sent: &mut HashMap<u32, usize>, pid: u32, args: &[&str]) -> Value {
    let n = sent.entry(pid).or_default();
    *n += 1;
    let held = live.dir.join(format!("cmd.{pid}.part"));
    std::fs::write(&held, format!("{}\n", args.join("\t"))).unwrap();
    std::fs::rename(held, live.dir.join(format!("cmd.{pid}"))).unwrap();
    let reply = live.dir.join(format!("reply.{pid}.{n}.json"));
    serde_json::from_str(&live.wait("member reply", || std::fs::read_to_string(&reply).ok()))
        .unwrap()
}
#[test]
fn members_spawn_and_act_only_down_the_tree() {
    let Some(live) = Live::start() else { return };
    let (task, brief) = (live.dir.join("task.txt"), live.dir.join("brief.md"));
    std::fs::write(&task, "say hi").unwrap();
    std::fs::write(&brief, "brief").unwrap();
    let (task, brief) = (task.display().to_string(), brief.display().to_string());
    let leader = s(&live.run(&["up", "-Client", "claude"])["leader"]["id"]);
    let mut sent = HashMap::new();
    let spawn = |sent: &mut HashMap<u32, usize>, who: &str, name: &str| {
        by(
            &live,
            sent,
            who,
            &[
                "spawn",
                "-Client",
                "claude",
                "-TaskFile",
                &task,
                "-BriefFile",
                &brief,
                "-Context",
                "brief",
                "-Name",
                name,
            ],
        )
    };
    let ok = |sent: &mut HashMap<u32, usize>, who: &str, name: &str| {
        let r = spawn(sent, who, name);
        assert_eq!(r["ok"], true, "{name}: {r}");
        s(&r["id"])
    };
    let c1 = ok(&mut sent, &leader, "c1");
    let flags = member_file(&live, &c1, "gens/0/spec.json")["flags"].clone();
    assert!(
        !flags
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "--dangerously-skip-permissions"),
        "no bypass: {flags}"
    );
    let c2 = ok(&mut sent, &leader, "c2");
    let g1 = ok(&mut sent, &c1, "g1");
    let h1 = ok(&mut sent, &g1, "h1");
    assert_eq!(member_file(&live, &h1, "record.json")["depth"], 3);
    assert_eq!(spawn(&mut sent, &h1, "x")["check"], "depth");
    for verb in ["stop", "read"] {
        assert_eq!(
            by(&live, &mut sent, &c2, &[verb, "c1"])["check"],
            "authority",
            "a sibling's {verb}"
        );
    }
    assert_eq!(
        by(&live, &mut sent, &leader, &["stop", "g1"])["ok"],
        true,
        "the leader stops a grandchild"
    );
    // cascade-3d: stopping g1 stops its sub-tree; h1 is ended and noticed too.
    let notice = member_file(&live, &h1, "gens/0/notice.json");
    assert_eq!(notice["kind"], "stopped", "h1 stopped with g1");
    // Live besides the leader: c1, c2; four more make six, a 7th refuses.
    for name in ["c3", "c4", "c5", "c6"] {
        ok(&mut sent, &leader, name);
    }
    assert_eq!(spawn(&mut sent, &leader, "c7")["check"], "limit");
}

/// Each named row on a bar screen: (line, column-0 mark, glyph column).
fn tree_rows(screen: &str, names: &[&str]) -> Option<Vec<(usize, char, usize)>> {
    names
        .iter()
        .map(|name| {
            screen.lines().enumerate().find_map(|(at, l)| {
                let c: Vec<char> = l.chars().collect();
                let glyph = (1..c.len()).find(|&i| c[i] != ' ' && c[i] != '^')?;
                let word: String = c
                    .get(glyph + 2..)?
                    .iter()
                    .take_while(|x| **x != ' ')
                    .collect();
                (word == *name).then_some((at, c[0], glyph))
            })
        })
        .collect()
}

#[test]
fn team_bar_draws_the_tree_and_opens_any_row() {
    let Some(live) = Live::start() else { return };
    let up = live.run(&["up", "-Client", "claude"]);
    std::fs::write(live.dir.join("brief.md"), "brief").unwrap();
    // Harness prerequisite (rmux 0.10.0): the bar's own popup command, run as the bar runs it (`run-shell -b -C`) with no client,
    // answers 0 and leaves its peek file; else the bar deletes the file unseen and the output check below fails for another reason.
    let file = live.dir.join("peek-probe.txt");
    std::fs::write(&file, "probe").unwrap();
    let command = mux_agent::sidebar::popup_command(&s(&up["sidebar"]), file.to_str().unwrap());
    let probe = crate::bounded::output(
        live.rmux().args(["run-shell", "-b", "-C", &command]),
        crate::bounded::LIMIT,
    )
    .unwrap();
    std::thread::sleep(Duration::from_secs(1));
    assert!(
        probe.status.success() && file.exists(),
        "the popup command without a client: {probe:?}"
    );
    std::fs::remove_file(&file).unwrap();
    let record = |id: &str| -> Value {
        let f = std::fs::read_dir(live.dir.join("state"))
            .unwrap()
            .flatten()
            .map(|d| d.path().join("agents").join(id).join("record.json"))
            .find(|p| p.exists())
            .unwrap();
        serde_json::from_str(&std::fs::read_to_string(f).unwrap()).unwrap()
    };
    let pid_of = |sid: &Value| -> u64 {
        live.wait("the client", || {
            std::fs::read_to_string(live.dir.join("clients.log"))
                .ok()?
                .lines()
                .find_map(|p| {
                    let r: Value = serde_json::from_str(
                        &std::fs::read_to_string(
                            live.dir.join(format!("client.{}.json", p.trim())),
                        )
                        .ok()?,
                    )
                    .ok()?;
                    (&r["sessionId"] == sid)
                        .then(|| p.trim().parse().ok())
                        .flatten()
                })
        })
    };
    let mut replies = std::collections::HashMap::new();
    let mut agent = |pid: u64, args: &[&str]| -> Value {
        let n = {
            let n = replies.entry(pid).or_insert(0);
            *n += 1;
            *n
        };
        let held = live.dir.join(format!("cmd.{pid}.part"));
        std::fs::write(&held, format!("{}\n", args.join("\t"))).unwrap();
        std::fs::rename(&held, live.dir.join(format!("cmd.{pid}"))).unwrap();
        let v: Value = serde_json::from_str(&live.wait("the reply", || {
            std::fs::read_to_string(live.dir.join(format!("reply.{pid}.{n}.json"))).ok()
        }))
        .unwrap();
        assert_eq!(v["ok"], true, "{args:?}: {v}");
        v
    };
    let (task, brief, session) = (
        live.dir.join("task.txt").display().to_string(),
        live.dir.join("brief.md").display().to_string(),
        s(&up["session"]),
    );
    let child = agent(
        pid_of(&record(&s(&up["leader"]["id"]))["sessionId"]),
        &[
            "spawn",
            "-Client",
            "claude",
            "-TaskFile",
            &task,
            "-BriefFile",
            &brief,
            "-Context",
            "brief",
            "-Name",
            "child",
        ],
    );
    let child_pid = pid_of(&child["sessionId"]);
    let grand = agent(
        child_pid,
        &[
            "spawn",
            "-Client",
            "claude",
            "-TaskFile",
            &task,
            "-BriefFile",
            &brief,
            "-Context",
            "brief",
            "-Name",
            "grand",
        ],
    );
    assert_eq!(record(&s(&grand["id"]))["depth"], 2);
    let bars = [
        s(&up["sidebar"]),
        s(&child["sidebar"]),
        s(&grand["sidebar"]),
    ];
    for bar in &bars {
        live.wait("the tree indented, no mark", || {
            let r = tree_rows(&live.screen(bar), &["leader", "child", "grand"])?;
            (r[0].0 < r[1].0
                && r[1].0 < r[2].0
                && r.iter().map(|x| x.2).eq([1, 3, 5])
                && r.iter().all(|x| x.1 != '\u{25cf}'))
            .then_some(())
        });
    }
    live.keys(&bars[0], "jjj");
    live.call(&["send-keys", "-t", &bars[0], "Enter"]);
    live.wait("j to grand, Enter", || {
        (live.field(&session, "#{pane_id}") == s(&grand["pane"])).then_some(())
    });
    // Grand prints a marker on its own terminal (the fake client prints nothing else), and hc keeps it before grand
    // stops: the output the peek file must hold.
    let marker = format!("grand-said-{}", std::process::id());
    let grand_pid = pid_of(&grand["sessionId"]);
    agent(grand_pid, &["@say", &marker]);
    let read = || {
        String::from_utf8(
            crate::bounded::output(
                &mut live
                    .core
                    .command(&["read", &s(&grand["hcAddress"]), "--lines", "200"]),
                crate::bounded::LIMIT,
            )
            .unwrap()
            .stdout,
        )
        .unwrap()
    };
    live.wait("hc keeps grand's marker", || {
        read().contains(&marker).then_some(())
    });
    // o2 call 35: grand is ready before its stop, so the bar shows `stopped`, not `recover`.
    wait_ready(&live, &s(&grand["id"]));
    agent(child_pid, &["stop", "grand"]);
    let line = live.wait("grand stays, stopped", || {
        live.call(&["capture-pane", "-e", "-p", "-t", &bars[0]])
            .lines()
            .find(|l| l.contains("grand") && l.contains("stopped"))
            .map(str::to_string)
    });
    let dim = line.split("\u{1b}[").skip(1).any(|p| {
        p.split("m")
            .next()
            .unwrap_or("")
            .split(";")
            .any(|n| n == "2")
    });
    assert!(
        dim || !line.contains("\u{1b}"),
        "the ended row is dimmed where SGR is drawn: {line:?}"
    );
    let kept = read();
    assert!(
        kept.contains(&marker),
        "the stopped grand keeps its marker: {kept:?}"
    );
    live.keys(&bars[0], "jjj");
    live.call(&["send-keys", "-t", &bars[0], "Enter"]);
    let text = live.wait("the peek file", || {
        std::fs::read_dir(live.dir.join("state"))
            .ok()?
            .flatten()
            .flat_map(|d| std::fs::read_dir(d.path()).into_iter().flatten().flatten())
            .find(|f| f.file_name().to_string_lossy().starts_with("peek-"))
            .and_then(|f| std::fs::read_to_string(f.path()).ok())
    });
    assert!(
        text.contains(&marker),
        "the peek file holds grand's marker: {text:?}"
    );
    assert_eq!(text, format!("{kept}\n-- peek: grand (q closes) --\n"));
    assert!(
        !live.screen(&bars[0]).contains("focus failed"),
        "no popup error"
    );
    assert_eq!(
        private_core::run(
            live.core
                .command(&["get", &s(&grand["hcAddress"]), "--json"]),
            &live.dir
        )
        .unwrap()["state"],
        "finished"
    );
}

impl Live {
    /// mux-agent typed into `pane` after `cd /d <dir>`; its JSON reply.
    fn run_at(&self, pane: &str, dir: &Path, args: &[&str]) -> Value {
        std::fs::create_dir_all(dir).unwrap();
        let (done, out) = (self.dir.join("at.done"), self.dir.join("at.json"));
        let _ = std::fs::remove_file(&done);
        self.line(pane, &self.at_line(dir, args));
        self.wait("reply in pane", || done.exists().then_some(()));
        serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap()
    }

    /// The line `run_at` types: a test comparing a pane's screen expects exactly it.
    fn at_line(&self, dir: &Path, args: &[&str]) -> String {
        let (done, out) = (self.dir.join("at.done"), self.dir.join("at.json"));
        let args = args
            .iter()
            .map(|a| format!("\"{a}\""))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "cd /d \"{}\" & \"{MUX_AGENT}\" {args} > \"{}\" & echo done> \"{}\"",
            dir.display(),
            out.display(),
            done.display()
        )
    }

    /// A `cmd.exe` pane split into `target` (focus kept) with the core's environment and this test's state root, as
    /// `new_session` gives a session; its id, once the prompt shows.
    fn pane_in(&self, target: &str) -> String {
        let mut split = self.rmux();
        split.args([
            "split-window",
            "-d",
            "-v",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            target,
        ]);
        for (key, value) in &self.core.env {
            split.arg("-e").arg(assign(key, value));
        }
        split.arg("-e").arg(assign(
            "MUX_AGENTS_STATE",
            self.dir.join("state").as_os_str(),
        ));
        let out = crate::bounded::output(split.arg("cmd.exe"), crate::bounded::LIMIT).unwrap();
        assert!(out.status.success(), "split-window in {target}: {out:?}");
        let pane = String::from_utf8_lossy(&out.stdout).trim().to_string();
        self.prompt(&pane);
        pane
    }
}

/// Phase 7a on rmux for Windows: as `live.rs`'s scenario of the same name. No client attaches here, so client
/// selection is observed as `list-clients` answers it (checklist 14); every pane is inventoried after the bars
/// close (9), each member's client is proven raw by a canary byte (10) and then receives no byte (8).
#[test]
fn notebook_and_peek_stay_in_the_team() {
    let Some(live) = Live::start() else { return };
    let (task, brief, body) = (
        live.dir.join("task.txt"),
        live.dir.join("brief.md"),
        live.dir.join("note.md"),
    );
    for (p, t) in [(&task, "say hi"), (&brief, "brief"), (&body, "the plan")] {
        std::fs::write(p, t).unwrap();
    }
    let (task, brief, body) = (
        task.display().to_string(),
        brief.display().to_string(),
        body.display().to_string(),
    );
    // PM call 10: every fake client of this scenario keeps the bytes it receives in `input.<pid>.raw`.
    std::fs::write(live.dir.join("raw-input"), "").unwrap();
    let up = live.run(&["up", "-Client", "claude"]);
    let (leader, session) = (s(&up["leader"]["id"]), s(&up["session"]));
    let mut sent = HashMap::new();
    let spawn = |sent: &mut HashMap<u32, usize>, who: &str, name: &str| {
        let r = by(
            &live,
            sent,
            who,
            &[
                "spawn",
                "-Client",
                "claude",
                "-TaskFile",
                &task,
                "-BriefFile",
                &brief,
                "-Context",
                "brief",
                "-Name",
                name,
            ],
        );
        assert_eq!(r["ok"], json!(true), "{name}: {r}");
        s(&r["id"])
    };
    let a = spawn(&mut sent, &leader, "a");
    let b = spawn(&mut sent, &a, "b");
    let c = spawn(&mut sent, &leader, "c");
    let mut notices = Vec::new();
    for title in ["first", "second"] {
        let r = by(
            &live,
            &mut sent,
            &a,
            &["note", "-Title", title, "-BodyFile", &body],
        );
        assert_eq!(r["ok"], json!(true), "{r}");
        notices.push(format!("note-{}", s(&r["id"])));
    }
    // 7b V2: b's next read serves each note's notice once.
    let got = by(&live, &mut sent, &b, &["read"]);
    let ids: Vec<String> = got["ids"].as_array().unwrap().iter().map(s).collect();
    assert_eq!(ids, notices, "the notices: {got}");
    let r = by(&live, &mut sent, &b, &["notes"]);
    let titles: Vec<String> = r["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| s(&n["title"]))
        .collect();
    assert_eq!(titles, ["second", "first"], "newest first: {r}");
    let person = live.pane_in(&session);
    let t2 = live.new_session("t2");
    let up2 = live.run_at(&t2, &live.dir.join("t2"), &["up", "-Client", "claude"]);
    assert_eq!(up2["ok"], json!(true), "{up2}");
    let t2_leader = s(&up2["leader"]["id"]);
    let t2_person = live.pane_in(&s(&up2["session"]));
    // o2 call 35: the spawned members are ready before anything is sent to them (the canary proves each client).
    for id in [&a, &b, &c] {
        wait_ready(&live, id);
    }
    let panes = |format: &str| -> Vec<String> {
        live.call(&["list-panes", "-a", "-F", format])
            .lines()
            .map(str::to_string)
            .collect()
    };
    // Checklist 9: every bar closes through the public command, from a shell pane of its own session.
    let shells = [
        live.root.clone(),
        t2.clone(),
        person.clone(),
        t2_person.clone(),
    ];
    let bar_sessions: std::collections::BTreeSet<String> = panes("#{session_id}|#{@agent-role}")
        .into_iter()
        .filter_map(|r| r.strip_suffix("|sidebar").map(str::to_string))
        .collect();
    for bar_session in bar_sessions {
        let from = shells
            .iter()
            .find(|p| live.field(p, "#{session_id}") == bar_session)
            .unwrap_or_else(|| panic!("no shell pane in {bar_session}"));
        let close = live.run_at(from, &live.dir, &["sidebar", "-Close"]);
        assert_eq!(close["ok"], json!(true), "{close}");
        for bar in close["panes"].as_array().into_iter().flatten() {
            let bar = s(bar);
            live.wait(&format!("bar {bar} to close"), || {
                (!panes("#{pane_id}").contains(&bar)).then_some(())
            });
        }
    }
    // b's pane, then another pane of b's window made active: a peek that selects b would show.
    let seen = by(&live, &mut sent, &leader, &["peek", "b"]);
    assert_eq!(
        seen["ok"],
        json!(true),
        "the leader peeks its grandchild: {seen}"
    );
    let pane = s(&seen["pane"]);
    let other = live
        .call(&[
            "split-window",
            "-d",
            "-h",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            &pane,
            "cmd.exe",
        ])
        .trim()
        .to_string();
    live.call(&["select-pane", "-t", &other]);
    live.prompt(&other);
    // Checklist 9: every pane is a member of either team or one of the five known shells.
    let (quiet, commands) = (
        [live.root.clone(), other, t2],
        [person.clone(), t2_person.clone()],
    );
    let mut tagged: Vec<(String, String)> = Vec::new();
    for row in panes("#{pane_id}|#{@agent-id}") {
        let (p, agent) = row.split_once('|').unwrap();
        if agent.is_empty() {
            assert!(
                quiet.iter().chain(&commands).any(|q| q == p),
                "pane {p} is neither a member nor a known shell"
            );
        } else {
            tagged.push((agent.to_string(), p.to_string()));
        }
    }
    let mut got: Vec<String> = tagged.iter().map(|(i, _)| i.clone()).collect();
    let mut want: Vec<String> = [&leader, &a, &b, &c, &t2_leader]
        .iter()
        .map(|i| i.to_string())
        .collect();
    got.sort();
    want.sort();
    assert_eq!(got, want, "the tagged panes are the members of both teams");
    let raw = |id: &str| {
        let pid = client_pid(
            &live,
            &s(&member_file(&live, id, "record.json")["sessionId"]),
            0,
        );
        live.dir.join(format!("input.{pid}.raw"))
    };
    // Checklist 10: raw mode on each member's console: one byte without Enter is logged before the baseline (a
    // cooked fallback never delivers it, and the wait fails). Only an absent first log is empty.
    for (agent, p) in &tagged {
        let path = raw(agent);
        let mut want = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => panic!("{}: {e}", path.display()),
        };
        want.push(b'x');
        live.call(&["send-keys", "-t", p, "-l", "x"]);
        live.wait(&format!("the canary byte in {}", path.display()), || {
            (std::fs::read(&path).ok()? == want).then_some(())
        });
    }
    // Checklist 8: bytes, read strictly (every log exists now).
    let typed = || -> Vec<Vec<u8>> {
        let read = |path: PathBuf| {
            std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        };
        tagged.iter().map(|(agent, _)| read(raw(agent))).collect()
    };
    let screen = |p: &str| -> Vec<String> {
        let mut lines: Vec<String> = live
            .screen(p)
            .lines()
            .map(|l| l.trim_end().to_string())
            .collect();
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines
    };
    for p in quiet.iter().chain(&commands) {
        live.wait(&format!("a prompt in {p}"), || {
            screen(p)
                .last()
                .is_some_and(|l| l.ends_with('>'))
                .then_some(())
        });
    }
    let focus = || {
        let format = "#{session_id}|#{window_id}|#{pane_id}|#{pane_active}|#{window_active}";
        (
            live.call(&["list-panes", "-a", "-F", format]),
            live.call(&["list-clients"]),
        )
    };
    let input = typed();
    let screens: Vec<Vec<String>> = quiet.iter().chain(&commands).map(|p| screen(p)).collect();
    let before = focus();
    assert!(
        before.0.contains(&format!("|{pane}|0|")),
        "b's pane is not active: {}",
        before.0
    );
    let r = by(&live, &mut sent, &leader, &["peek", "b"]);
    assert_eq!(
        (&r["ok"], r["pane"].as_str()),
        (&json!(true), Some(pane.as_str())),
        "the leader again: {r}"
    );
    let asked: [&[&str]; 2] = [&["peek", "b", "-Lines", "5"], &["peek", "b"]];
    let r = live.run_at(&person, &live.dir, asked[0]);
    assert_eq!(
        (&r["ok"], r["pane"].as_str()),
        (&json!(true), Some(pane.as_str())),
        "a person: {r}"
    );
    let r = by(&live, &mut sent, &c, &["peek", "b"]);
    assert_eq!(
        r["check"].as_str(),
        Some("authority"),
        "a sibling of b's parent: {r}"
    );
    let r = live.run_at(&t2_person, &live.dir, asked[1]);
    assert_eq!(
        r["check"].as_str(),
        Some("target"),
        "another team's person: {r}"
    );
    assert_eq!(focus(), before, "panes, windows and clients stay put");
    assert_eq!(typed(), input, "no member's client received a byte");
    // A command pane gains exactly `run_at`'s line after the prompt it showed, then one prompt (blank lines aside);
    // a quiet shell gains nothing.
    for (n, p) in quiet.iter().chain(&commands).enumerate() {
        let (was, now) = (&screens[n], screen(p));
        if n < quiet.len() {
            assert_eq!(&now, was, "shell pane {p} changed");
            continue;
        }
        let (keep, prompt) = (was.len() - 1, &was[was.len() - 1]);
        let line = live.at_line(&live.dir, asked[n - quiet.len()]);
        let rest: Vec<&String> = now
            .get(keep + 1..)
            .unwrap_or_default()
            .iter()
            .filter(|l| !l.is_empty())
            .collect();
        let harness = now.len() > keep + 1
            && now[..keep] == was[..keep]
            && now[keep] == format!("{prompt}{line}")
            && rest.len() == 1
            && rest[0].ends_with('>');
        assert!(
            harness,
            "command pane {p} gained more than the harness's line:\n{was:#?}\n{now:#?}"
        );
    }
}

/// Team messaging end to end on Windows (#103 4a): leader > a > b, and team T2 in another session. A broadcast is
/// one id each; a reply is hop 1; another team is refused; `children` is only the leader's own; the 121st send of
/// the hour refuses with `retryAt`.
#[test]
fn team_messaging_stays_in_the_team() {
    let Some(live) = Live::start() else { return };
    let (task, brief, body) = (
        live.dir.join("task.txt"),
        live.dir.join("brief.md"),
        live.dir.join("body.md"),
    );
    for (p, t) in [(&task, "say hi"), (&brief, "brief"), (&body, "hello")] {
        std::fs::write(p, t).unwrap();
    }
    let (task, brief, body) = (
        task.display().to_string(),
        brief.display().to_string(),
        body.display().to_string(),
    );
    let leader = s(&live.run(&["up", "-Client", "claude"])["leader"]["id"]);
    let mut sent = HashMap::new();
    let spawn = |sent: &mut HashMap<u32, usize>, who: &str, name: &str| {
        let r = by(
            &live,
            sent,
            who,
            &[
                "spawn",
                "-Client",
                "claude",
                "-TaskFile",
                &task,
                "-BriefFile",
                &brief,
                "-Context",
                "brief",
                "-Name",
                name,
            ],
        );
        assert_eq!(r["ok"], true, "{name}: {r}");
        s(&r["id"])
    };
    let a = spawn(&mut sent, &leader, "a");
    let b = spawn(&mut sent, &a, "b");
    // T2's session gets the first session's environment and state root (QA round 2, finding 3).
    let pane = live.new_session("t2");
    let up2 = live.run_at(&pane, &live.dir.join("t2"), &["up", "-Client", "claude"]);
    assert_eq!(up2["ok"], true, "{up2}");
    spawn(&mut sent, &s(&up2["leader"]["id"]), "x");
    let r = by(&live, &mut sent, &a, &["send", "team", "-BodyFile", &body]);
    let id = s(&r["id"]);
    for who in [&leader, &b] {
        let got = by(&live, &mut sent, who, &["read"]);
        // The ids a read served (#103 4b-i, `msg-plan-4b-i-3.md` F25, finding 15).
        let ids: Vec<String> = got["ids"].as_array().unwrap().iter().map(s).collect();
        assert_eq!(ids, std::slice::from_ref(&id), "one id each");
    }
    assert_eq!(
        by(
            &live,
            &mut sent,
            &b,
            &["send", "a", "-BodyFile", &body, "-Re", &id]
        )["hop"],
        1
    );
    assert_eq!(
        by(&live, &mut sent, &b, &["send", "x", "-BodyFile", &body])["check"],
        "target"
    );
    let kids = by(
        &live,
        &mut sent,
        &leader,
        &["send", "children", "-BodyFile", &body],
    );
    assert_eq!(
        kids["recipients"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| s(&x["id"]))
            .collect::<Vec<_>>(),
        std::slice::from_ref(&a)
    );
    // The quota through the live client (F22): a has sent one (team); 119 more fill the hour, the 121st refuses.
    for n in 0..119 {
        assert_eq!(
            by(
                &live,
                &mut sent,
                &a,
                &["send", "leader", "-BodyFile", &body]
            )["ok"],
            true,
            "send {}",
            n + 2
        );
    }
    let r = by(
        &live,
        &mut sent,
        &a,
        &["send", "leader", "-BodyFile", &body],
    );
    assert_eq!(
        (r["check"].as_str(), r["retryAt"].is_string()),
        (Some("quota"), true),
        "the 121st: {r}"
    );
}

/// Receipts end to end on Windows (#103 4b-i): b's unacked batch is served again, then acked; a resumed b (another
/// process) gets a rebuilt batch for unacked mail.
#[test]
fn an_unacked_batch_is_served_again_and_rebuilt_for_a_successor() {
    let Some(live) = Live::start() else { return };
    let (task, brief, body) = (
        live.dir.join("task.txt"),
        live.dir.join("brief.md"),
        live.dir.join("body.md"),
    );
    for (p, t) in [(&task, "say hi"), (&brief, "brief"), (&body, "hello")] {
        std::fs::write(p, t).unwrap();
    }
    let (task, brief, body) = (
        task.display().to_string(),
        brief.display().to_string(),
        body.display().to_string(),
    );
    let leader = s(&live.run(&["up", "-Client", "claude"])["leader"]["id"]);
    let mut sent = HashMap::new();
    let spawn = |sent: &mut HashMap<u32, usize>, name: &str| {
        let r = by(
            &live,
            sent,
            &leader,
            &[
                "spawn",
                "-Client",
                "claude",
                "-TaskFile",
                &task,
                "-BriefFile",
                &brief,
                "-Context",
                "brief",
                "-Name",
                name,
            ],
        );
        assert_eq!(r["ok"], true, "{name}: {r}");
        s(&r["id"])
    };
    let (a, b) = (spawn(&mut sent, "a"), spawn(&mut sent, "b"));
    for _ in 0..2 {
        assert_eq!(
            by(&live, &mut sent, &a, &["send", "b", "-BodyFile", &body])["ok"],
            true
        );
    }
    let first = by(&live, &mut sent, &b, &["read", "-Deliver"]);
    assert_eq!(first["ids"].as_array().map(Vec::len), Some(2), "{first}");
    assert_eq!(
        by(&live, &mut sent, &b, &["read", "-Deliver"])["batch"],
        first["batch"]
    );
    assert_eq!(
        by(&live, &mut sent, &b, &["read", "-Ack", &s(&first["batch"])])["acked"],
        true
    );
    by(&live, &mut sent, &a, &["send", "b", "-BodyFile", &body]);
    let pending = by(&live, &mut sent, &b, &["read", "-Deliver"]);
    let sid = s(&member_file(&live, &b, "record.json")["sessionId"]);
    let old = client_pid(&live, &sid, 0);
    ready::wait_conversation(&live.dir, &sid); // PM call 12
    assert_eq!(by(&live, &mut sent, &leader, &["stop", "b"])["ok"], true);
    let resumed = by(&live, &mut sent, &leader, &["resume", "b"]);
    assert_eq!(resumed["ok"], true, "{resumed}");
    proof_of(&live, &s(&resumed["agent"]["id"]));
    // The resumed B: matched by session id or argv as on Unix, another process than its predecessor (F20).
    let new = client_pid(&live, &sid, old);
    assert_ne!(new, old, "a new process");
    let rebuilt = by_pid(&live, &mut sent, new, &["read", "-Deliver"]);
    assert_eq!(rebuilt["ids"], pending["ids"]);
    assert_ne!(rebuilt["batch"], pending["batch"]);
}

/// Posts a background line (`@bg`) to member `id`, once its client took the previous command; the path of its reply.
fn post_bg(live: &Live, sent: &mut HashMap<u32, usize>, id: &str, args: &[&str]) -> PathBuf {
    proof_of(live, id);
    let pid = client_pid(
        live,
        &s(&member_file(live, id, "record.json")["sessionId"]),
        0,
    );
    let n = sent.entry(pid).or_default();
    *n += 1;
    let cmd = live.dir.join(format!("cmd.{pid}"));
    live.wait("client took its last command", || {
        (!cmd.exists()).then_some(())
    });
    let held = live.dir.join(format!("cmd.{pid}.part"));
    std::fs::write(&held, format!("@bg {}\n", args.join("\t"))).unwrap();
    std::fs::rename(held, &cmd).unwrap();
    live.dir.join(format!("reply.{pid}.{n}.json"))
}

/// What an "on call" timeout shows, with no further call through the root pane (it may be the one stuck): the last
/// `list -All` the poll saw (`last`), member `id`'s folder files with their text, the background reply `reply` (json,
/// err, code) so far, and the root pane's screen.
fn on_call_diagnosis(live: &Live, id: &str, last: &str, reply: &Path) -> String {
    let folders = std::fs::read_dir(live.dir.join("state"))
        .map(|d| {
            d.filter_map(Result::ok)
                .map(|e| e.path().join("agents").join(id))
                .filter(|p| p.is_dir())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut out = format!("last list -All: {last}\n");
    for folder in folders {
        for e in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
            let text = std::fs::read_to_string(e.path()).unwrap_or_default();
            out.push_str(&format!("{}: {}\n", e.path().display(), text.trim()));
        }
    }
    for ext in ["json", "err", "code"] {
        let file = reply.with_extension(ext);
        let text = std::fs::read_to_string(&file).unwrap_or_else(|e| format!("<{e}>"));
        out.push_str(&format!("{}: {}\n", file.display(), text.trim()));
    }
    out.push_str(&format!("root pane:\n{}", live.screen(&live.root)));
    out
}

#[test]
fn a_copilot_member_reports_done_then_listens_on_call() {
    let Some(live) = Live::start() else { return };
    let (task, brief, body) = (
        live.dir.join("task.txt"),
        live.dir.join("brief.md"),
        live.dir.join("body.md"),
    );
    for (p, t) in [(&task, "say hi"), (&brief, "brief"), (&body, "nonce-7f3a")] {
        std::fs::write(p, t).unwrap();
    }
    let (task, brief, body) = (
        task.display().to_string(),
        brief.display().to_string(),
        body.display().to_string(),
    );
    let leader = s(&live.run(&["up", "-Client", "claude"])["leader"]["id"]);
    let mut sent = HashMap::new();
    let spawn = |sent: &mut HashMap<u32, usize>, client: &str, name: &str| {
        s(&by(
            &live,
            sent,
            &leader,
            &[
                "spawn",
                "-Client",
                client,
                "-TaskFile",
                &task,
                "-BriefFile",
                &brief,
                "-Context",
                "brief",
                "-Name",
                name,
            ],
        )["id"])
    };
    let (a, b) = (
        spawn(&mut sent, "claude", "a"),
        spawn(&mut sent, "copilot", "b"),
    );
    // The last `list -All` row of b and how long the call took, for a timeout's diagnosis.
    let last = std::cell::RefCell::new(String::new());
    // Listed by the leader: a board holds only its caller's team (`membership::locals` skips other sessions), and the
    // root pane is not in the team `up` made.
    let on_call = |sent: &mut HashMap<u32, usize>| {
        let t = Instant::now();
        let list = by(&live, sent, &leader, &["list", "-All"]);
        let row = list["agents"]
            .as_array()
            .and_then(|a| a.iter().find(|x| x["name"] == "b").cloned());
        *last.borrow_mut() = format!("{:?} {}", t.elapsed(), row.as_ref().unwrap_or(&list));
        row.is_some_and(|x| x["activity"] == "on call")
    };
    assert_eq!(
        by(&live, &mut sent, &b, &["report", "done"])["state"],
        "done"
    );
    let reply = post_bg(&live, &mut sent, &b, &["read", "-Wait", "600"]);
    live.wait_or(
        "on call (first wait)",
        || on_call(&mut sent).then_some(()),
        || on_call_diagnosis(&live, &b, &last.borrow(), &reply),
    );
    by(&live, &mut sent, &a, &["send", "b", "-BodyFile", &body]);
    // Sender exit: A leaves before B collects; the mail is durable, the wait still returns it.
    let a_pid = client_pid(
        &live,
        &s(&member_file(&live, &a, "record.json")["sessionId"]),
        0,
    );
    std::fs::write(live.dir.join(format!("quit.{a_pid}")), "0").unwrap();
    live.wait("sender exit", || {
        live.dir
            .join(format!("exited.{a_pid}"))
            .exists()
            .then_some(())
    });
    let got: Value =
        serde_json::from_str(&live.wait("batch", || std::fs::read_to_string(&reply).ok())).unwrap();
    assert!(s(&got["text"]).contains("nonce-7f3a"), "{got}");
    let t = by(
        &live,
        &mut sent,
        &b,
        &["read", "-Wait", "1", "-Ack", &s(&got["batch"])],
    );
    assert_eq!(
        (&t["acked"], &t["timeout"]),
        (&serde_json::json!(true), &serde_json::json!(true)),
        "{t}"
    );
    let reply = post_bg(&live, &mut sent, &b, &["read", "-Wait", "600"]);
    live.wait_or(
        "on call (second wait)",
        || on_call(&mut sent).then_some(()),
        || on_call_diagnosis(&live, &b, &last.borrow(), &reply),
    );
    assert_eq!(
        by(&live, &mut sent, &b, &["read", "-Cancel"])["cancelled"],
        true
    );
    let c: Value =
        serde_json::from_str(&live.wait("cancelled", || std::fs::read_to_string(&reply).ok()))
            .unwrap();
    assert_eq!(c["cancelled"], true);
    by(
        &live,
        &mut sent,
        &leader,
        &["send", "b", "-BodyFile", &body],
    );
    let d1 = by(&live, &mut sent, &b, &["read", "-Deliver"]);
    assert_eq!(
        by(&live, &mut sent, &b, &["read", "-Deliver"])["batch"],
        d1["batch"],
        "no -Ack: re-served"
    );
    let mail = std::fs::read_dir(live.dir.join("state"))
        .unwrap()
        .map(|e| e.unwrap().path().join("mail"))
        .find(|p| p.is_dir())
        .unwrap();
    std::fs::write(mail.join("fault-file"), "").unwrap();
    assert_eq!(
        by(&live, &mut sent, &b, &["read", "-Ack", &s(&d1["batch"])])["check"],
        "mail"
    );
    std::fs::write(mail.join("fault-folder"), "").unwrap();
    assert_eq!(
        by(&live, &mut sent, &b, &["read", "-Ack", &s(&d1["batch"])])["check"],
        "mail"
    );
    assert_eq!(
        by(&live, &mut sent, &b, &["read", "-Ack", &s(&d1["batch"])])["duplicate"],
        true
    );
    // A tagged hook: a process with the terminal id of b, outside its client chain and outside rmux.
    let addr = s(&member_file(&live, &b, "record.json")["hcAddress"]);
    let (script, out, done) = (
        live.dir.join("tagged.cmd"),
        live.dir.join("tagged.json"),
        live.dir.join("tagged.done"),
    );
    std::fs::write(
        &script,
        [
            "@echo off",
            "set RMUX=",
            "set TMUX=",
            &format!("set HC_TERMINAL_ID={addr}"),
            &format!("\"{MUX_AGENT}\" read -Count > \"{}\"", out.display()),
            &format!("echo done> \"{}\"", done.display()),
        ]
        .join("\r\n"),
    )
    .unwrap();
    live.line(&live.root, &format!("cmd /d /c \"{}\"", script.display()));
    live.wait("tagged hook", || done.exists().then_some(()));
    assert_eq!(
        serde_json::from_str::<Value>(&std::fs::read_to_string(&out).unwrap()).unwrap()["check"],
        "tagged"
    );
    let screen = live.call(&["capture-pane", "-p", "-t", &live.root]);
    assert!(!screen.contains("mail from"), "no notice typed: {screen}");
    // No keystrokes reach a recipient either: b (the one recipient here) got the body only through `read`, never
    // typed into its client's stdin or drawn on its pane.
    let record = member_file(&live, &b, "record.json");
    let b_pid = client_pid(&live, &s(&record["sessionId"]), 0);
    let typed =
        std::fs::read_to_string(live.dir.join(format!("input.{b_pid}.log"))).unwrap_or_default();
    assert!(!typed.contains("nonce-7f3a"), "typed into b: {typed}");
    let b_screen = live.screen(&s(&member_file(&live, &b, "view.json")["pane"]));
    assert!(
        !b_screen.contains("nonce-7f3a") && !b_screen.contains("mail from"),
        "drawn on b: {b_screen}"
    );
}

/// A `cmd.exe` pane split into `session` (cascade-3d call 26: `up`'s team session), with the private core's
/// environment and this test's state root set in the pane itself (a split does not get the session's `-e` variables
/// in rmux 0.10).
fn team_pane(live: &Live, session: &str) -> String {
    let pane = live
        .call(&[
            "split-window",
            "-v",
            "-t",
            session,
            "-P",
            "-F",
            "#{pane_id}",
            "cmd.exe",
        ])
        .trim()
        .to_string();
    live.prompt(&pane);
    for (key, value) in &live.core.env {
        live.line(&pane, &format!("set {key}={}", value.to_string_lossy()));
    }
    let state = live.dir.join("state");
    live.line(&pane, &format!("set MUX_AGENTS_STATE={}", state.display()));
    pane
}

/// cascade-3d live on rmux (Windows): the Unix scenario through the same public paths (calls 12, 26). A person's
/// commands run from a pane in `up`'s team session, whose bar is closed (and proven closed) before the crash.
#[test]
fn a_cascade_stop_ends_b_before_a_and_a_second_stop_finishes() {
    let Some(mut live) = Live::start() else {
        return;
    };
    let (task, brief) = (live.dir.join("task.txt"), live.dir.join("brief.md"));
    std::fs::write(&task, "say hi").unwrap();
    std::fs::write(&brief, "brief").unwrap();
    let (task, brief) = (task.display().to_string(), brief.display().to_string());
    let up = live.run(&["up", "-Client", "claude"]);
    let leader = s(&up["leader"]["id"]);
    let team = team_pane(&live, &s(&up["session"]));
    let close = live.run_any_in(&team, &["sidebar", "-Close"]);
    assert_eq!(close["ok"], true, "{close}");
    let again = live.run_any_in(&team, &["sidebar", "-Close"]);
    let none = again["panes"].as_array().is_none_or(Vec::is_empty);
    assert!(
        again["ok"] == true && none,
        "the team's bar is closed: {again}"
    );
    let mut sent = HashMap::new();
    let args = |name: &str| -> Vec<String> {
        [
            "spawn",
            "-Client",
            "claude",
            "-TaskFile",
            &task,
            "-BriefFile",
            &brief,
            "-Context",
            "brief",
            "-Name",
            name,
        ]
        .map(String::from)
        .to_vec()
    };
    let spawn = |live: &Live, sent: &mut HashMap<u32, usize>, who: &str, name: &str| {
        let a = args(name);
        let r = by(
            live,
            sent,
            who,
            &a.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        assert_eq!(r["ok"], true, "{name}: {r}");
        // o2 call 35 (integration): ready before any stop ends it, so it ends an ordinary member.
        wait_ready(live, &s(&r["id"]));
        s(&r["id"])
    };
    let left = |live: &Live, id: &str| {
        let teams = agent_dir(live, id)
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("teams");
        std::fs::read_dir(teams).is_ok_and(|d| {
            d.flatten()
                .any(|e| e.path().join(format!("{id}.json")).exists())
        })
    };
    // 1. The leader stops a1 by name, inside the tree: b1, then a1; every notice; no journal left.
    let a1 = spawn(&live, &mut sent, &leader, "a1");
    let b1 = spawn(&live, &mut sent, &a1, "b1");
    let r = by(&live, &mut sent, &leader, &["stop", "a1"]);
    let got = (&r["stopped"], &r["unresolved"]);
    assert_eq!(
        got,
        (&serde_json::json!(true), &serde_json::json!([])),
        "{r}"
    );
    assert!(agent_dir(&live, &b1).join("gens/0/notice.json").exists() && !left(&live, &a1));
    // 2. A person stops a2 from the team's pane, killed right after b2's notice.
    let a2 = spawn(&live, &mut sent, &leader, "a2");
    let b2 = spawn(&live, &mut sent, &a2, "b2");
    let step = format!("cascade-noticed-{b2}");
    let code = run_crashed_in(&live, &team, &step, &["stop", "a2"]);
    assert_eq!(code, 86, "killed after b2's notice");
    let address = |id: &str| s(&member_file(&live, id, "record.json")["hcAddress"]);
    assert_eq!(
        hc_get(&live, &address(&b2))["state"],
        "finished",
        "b2 ended first"
    );
    assert_eq!(hc_get(&live, &address(&a2))["state"], "live", "a2 after b2");
    assert!(left(&live, &a2), "the journal is kept");
    let a = args("c2");
    let r = by(
        &live,
        &mut sent,
        &a2,
        &a.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    assert_eq!(r["check"], "closed", "a spawn under a2: {r}");
    // 3. The private core restarts; the journal is still there; a second person stop finishes it.
    assert!(live.core.stop(), "core stop");
    live.core.start();
    assert!(left(&live, &a2), "kept across the restart");
    live.wait("a second stop finished the cascade", || {
        let _ = live.run_any_in(&team, &["stop", "a2"]);
        (!left(&live, &a2)).then_some(())
    });
    assert!(
        agent_dir(&live, &a2).join("gens/0/notice.json").exists(),
        "a2 noticed"
    );
}

/// `<state>/<server key>/agents/<id>`: the one server key folder that holds member `id`.
fn agent_dir(live: &Live, id: &str) -> PathBuf {
    let found: Vec<PathBuf> = std::fs::read_dir(live.dir.join("state"))
        .unwrap()
        .map(|e| e.unwrap().path().join("agents").join(id))
        .filter(|p| p.is_dir())
        .collect();
    assert_eq!(found.len(), 1, "{found:?}");
    found.into_iter().next().unwrap()
}

/// `hc get <address> --json` on the private core.
fn hc_get(live: &Live, address: &str) -> Value {
    private_core::run(live.core.command(&["get", address, "--json"]), &live.dir).unwrap()
}

/// release-1d live on rmux (Windows): stop the core so hc keeps the terminal as PowerShell's recovery shell (Lost),
/// then release it: the shell stays live carrying exactly mux.kept and mux.keptName; released.json exists.
#[test]
fn a_lost_member_is_released_and_keeps_only_its_kept_labels() {
    let Some(mut live) = Live::start() else {
        return;
    };
    let w = live.spawn("w1");
    let id = w["id"].as_str().unwrap().to_string();
    let address = w["hcAddress"].as_str().unwrap().to_string();
    let seeded = live.run(&["__life", "seed", &id, "0"]);
    assert_eq!(seeded["ok"], true, "{seeded}");
    let before = hc_get(&live, &address)["labels"].clone();
    assert!(live.core.stop(), "core stop");
    live.core.start();
    live.wait("the terminal is Lost", || {
        (hc_get(&live, &address)["argv"][1] == "-NoExit").then_some(())
    });
    let out = live.run(&["__life", "release", &id, "0"]);
    assert_eq!(out["released"], true, "{out}");
    let t = hc_get(&live, &address);
    assert_eq!(t["state"], "live");
    let want =
        serde_json::json!({"mux.kept": before["mux.team"], "mux.keptName": before["mux.name"]});
    assert_eq!(t["labels"], want);
    assert!(agent_dir(&live, &id).join("gens/0/released.json").exists());
}

/// `mux-agent <args>` in the root pane with MUX_AGENTS_CRASH=<step> for that process only; its exit code.
fn run_crashed(live: &Live, step: &str, args: &[&str]) -> i32 {
    run_crashed_in(live, &live.root, step, args)
}

/// `run_crashed` typed into `pane` (cascade-3d call 26).
fn run_crashed_in(live: &Live, pane: &str, step: &str, args: &[&str]) -> i32 {
    let (script, code, done) = (
        live.dir.join("crash.cmd"),
        live.dir.join("crash.code"),
        live.dir.join("crash.done"),
    );
    let _ = std::fs::remove_file(&done);
    let args = args
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(" ");
    let body = format!(
        "@setlocal\r\n@set MUX_AGENTS_CRASH={step}\r\n@\"{MUX_AGENT}\" {args} > nul\r\n@echo %errorlevel%> \"{}\"\r\n@echo done> \"{}\"\r\n",
        code.display(),
        done.display()
    );
    std::fs::write(&script, body).unwrap();
    live.line(pane, &format!("\"{}\"", script.display()));
    live.wait("the crashed command", || done.exists().then_some(()));
    std::fs::read_to_string(code)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// settle-1b live on rmux (Windows), b-calls 38: a forward killed at `promote` leaves generation 0 orphan-running;
/// G11 ends it; hc shows it finished.
#[test]
fn an_orphan_left_by_a_forward_crash_is_ended_by_terminate() {
    let Some(live) = Live::start() else {
        return;
    };
    let w = live.spawn("w1");
    let id = w["id"].as_str().unwrap().to_string();
    let address = w["hcAddress"].as_str().unwrap().to_string();
    let seeded = live.run(&["__life", "seed", &id, "0"]);
    assert_eq!(seeded["ok"], true, "{seeded}");
    assert_eq!(
        run_crashed(&live, "promote", &["__life", "forward", &id, "1"]),
        86,
        "killed at promote"
    );
    let record = std::fs::read(agent_dir(&live, &id).join("record.json")).unwrap();
    let record: serde_json::Value = serde_json::from_slice(&record).unwrap();
    assert_eq!(
        record["generation"], "1",
        "generation 1 was promoted before the kill"
    );
    assert_eq!(
        hc_get(&live, &address)["state"],
        "live",
        "orphan 0 still runs"
    );
    live.wait("terminate ended the orphan", || {
        let out = live.run_any(&["__life", "terminate", &id, "0"]);
        (out["ok"] == true && out["ended"] == true).then_some(())
    });
    assert_eq!(hc_get(&live, &address)["state"], "finished");
    assert!(agent_dir(&live, &id).join("gens/0/ended.json").exists());
}

/// report-2b live on Windows (rmux), as tests/live.rs: without and with the launcher barrier (PM call 18).
#[test]
fn a_new_member_reports_ready_within_150_s() {
    for barrier in [false, true] {
        let Some(live) = Live::start() else {
            return;
        };
        let state = live.dir.join("state");
        if barrier {
            native::private_root(&state);
            std::fs::write(state.join("test-client-barrier"), "").unwrap();
        }
        // r2b call 35: one deadline from the launch boundary; the spawn and every wait up to ready.json spend it.
        let deadline = native::Deadline::start(native::READY);
        let y = live.spawn("w1");
        let (id, sid) = (s(&y["id"]), s(&y["sessionId"]));
        let (c, r) = deadline.wait(
            "client.json and ready.json",
            || {
                Some((
                    gen0_file(&live, &id, "client.json")?,
                    gen0_file(&live, &id, "ready.json")?,
                ))
            },
            || {
                format!(
                    "barrier {barrier}: client {:?} ready {:?}",
                    gen0_file(&live, &id, "client.json"),
                    gen0_file(&live, &id, "ready.json")
                )
            },
        );
        assert_eq!(
            (&r["pid"], &r["start"]),
            (&c["pid"], &c["start"]),
            "barrier {barrier}"
        );
        // After ready.json: the client lookup waits on its own, outside the readiness deadline.
        let client = client_of(&live, &sid);
        assert!(
            client["env"]["HC_TERMINAL_ID"]
                .as_str()
                .is_some_and(|v| !v.is_empty()),
            "{client}"
        );
        assert!(
            client["env"]["TMUX"].is_null() && client["env"]["RMUX"].is_null(),
            "{client}"
        );
        if barrier {
            let marks: Vec<String> = std::fs::read_dir(&state)
                .unwrap()
                .filter_map(|e| {
                    let e = e.ok()?;
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.starts_with("test-barrier-")
                        .then(|| std::fs::read_to_string(e.path()).unwrap())
                })
                .collect();
            assert_eq!(marks, ["report-first"], "report.json before client.json");
        }
    }
}

/// The fake client record (`client.<pid>.json`) whose `sessionId` is `sid`, through `clients.log`; waited for.
fn client_of(live: &Live, sid: &str) -> Value {
    live.wait("the client of the new member", || {
        let log = std::fs::read_to_string(live.dir.join("clients.log")).ok()?;
        log.lines().find_map(|pid| {
            let text =
                std::fs::read_to_string(live.dir.join(format!("client.{}.json", pid.trim())))
                    .ok()?;
            let v: Value = serde_json::from_str(&text).ok()?;
            (v["sessionId"] == sid).then_some(v)
        })
    })
}

/// o2 calls 33/35: waits for member `id`'s `ready.json` in its record's generation (its fake client's startup report
/// promoted it), so a test that then stops it ends an ordinary member, not one failed before readiness.
fn wait_ready(live: &Live, id: &str) {
    let path = || {
        let folder = agent_dir(live, id);
        let r = member_file(live, id, "record.json");
        let n = r["generation"].as_str().unwrap_or("0").to_string();
        folder.join("gens").join(n).join("ready.json")
    };
    native::Deadline::start(native::READY).wait(
        "ready.json",
        || path().exists().then_some(()),
        || format!("{id}: {}", path().display()),
    );
}

/// `gens/0/<file>` of member `id` (found by `agent_dir`); None until it is whole.
fn gen0_file(live: &Live, id: &str, file: &str) -> Option<Value> {
    let p = agent_dir(live, id).join("gens").join("0").join(file);
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

/// Quits member `id`'s fake client and waits for its exit marker.
fn quit_member(live: &Live, id: &str) {
    let pid = client_pid(
        live,
        &s(&member_file(live, id, "record.json")["sessionId"]),
        0,
    );
    std::fs::write(live.dir.join(format!("quit.{pid}")), "0").unwrap();
    live.wait("client exit", || {
        live.dir
            .join(format!("exited.{pid}"))
            .exists()
            .then_some(())
    });
}
fn finished(live: &Live, id: &str) -> bool {
    let address = s(&member_file(live, id, "record.json")["hcAddress"]);
    eventually(Duration::from_secs(60), || {
        hc_get(live, &address)["state"] == "finished"
    })
}
fn parent_and_depth(live: &Live, id: &str) -> (Value, Value) {
    let r = member_file(live, id, "record.json");
    (r["parentAgent"]["id"].clone(), r["depth"].clone())
}
/// The bodies in member `id`'s mailbox (`<state>/<key>/mail/<mailbox>.jsonl`; an inherited `mailbox` honoured).
fn mail_bodies(live: &Live, id: &str) -> Vec<Value> {
    let r = member_file(live, id, "record.json");
    let key = r
        .get("mailbox")
        .and_then(Value::as_str)
        .unwrap_or(id)
        .to_string();
    let keyed = agent_dir(live, id)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let text = std::fs::read_to_string(keyed.join("mail").join(format!("{key}.jsonl")))
        .unwrap_or_default();
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|m| m["body"].clone())
        .collect()
}
/// A person's command typed into the team's pane; its reply must be `ok`.
fn team_run(live: &Live, team: &str, args: &[&str]) -> Value {
    let v = live.run_any_in(team, args);
    assert_eq!(v["ok"], true, "{args:?}: {v}");
    v
}

/// adopt-3e live on rmux (Windows): the Unix scenario through the same public commands (calls 3, 16, 23). A person's
/// commands run from a pane in `up`'s team session, whose bar is closed.
#[test]
fn adoption_moves_b_and_a_resumed_successor_stops_its_child() {
    use serde_json::json;
    let Some(live) = Live::start() else { return };
    let (task, brief) = (live.dir.join("task.txt"), live.dir.join("brief.md"));
    std::fs::write(&task, "say hi").unwrap();
    std::fs::write(&brief, "brief").unwrap();
    let (task, brief) = (task.display().to_string(), brief.display().to_string());
    let up = live.run(&["up", "-Client", "claude"]);
    let x = s(&up["leader"]["id"]);
    let team = team_pane(&live, &s(&up["session"]));
    team_run(&live, &team, &["sidebar", "-Close"]);
    let mut sent = HashMap::new();
    let spawn = |live: &Live, sent: &mut HashMap<u32, usize>, who: &str, name: &str| {
        let args = [
            "spawn",
            "-Client",
            "claude",
            "-TaskFile",
            task.as_str(),
            "-BriefFile",
            brief.as_str(),
            "-Context",
            "brief",
            "-Name",
            name,
        ];
        let r = by(live, sent, who, &args);
        assert_eq!(r["ok"], true, "{name}: {r}");
        s(&r["id"])
    };
    let a = spawn(&live, &mut sent, &x, "a1");
    let b = spawn(&live, &mut sent, &a, "b1");
    quit_member(&live, &a);
    assert!(finished(&live, &a), "A finished");
    team_run(&live, &team, &["list"]); // reaps A
    team_run(&live, &team, &["list"]); // sweeps: X adopts B
    assert_eq!(
        parent_and_depth(&live, &b),
        (json!(x), json!(1)),
        "X adopted B"
    );
    let up_md = live.dir.join("up.md");
    std::fs::write(&up_md, "from b1").unwrap();
    let up_md = up_md.display().to_string();
    let r = by(
        &live,
        &mut sent,
        &b,
        &["send", "parent", "-BodyFile", up_md.as_str()],
    );
    assert_eq!(r["ok"], true, "{r}");
    assert!(
        mail_bodies(&live, &x).contains(&json!("from b1")),
        "X's mailbox"
    );
    let address = |id: &str| s(&member_file(&live, id, "record.json")["hcAddress"]);
    assert_eq!(hc_get(&live, &address(&b))["state"], "live", "B works");

    let c = spawn(&live, &mut sent, &x, "c1");
    let d = spawn(&live, &mut sent, &c, "d1");
    team_run(&live, &team, &["sidebar", "-Close"]); // nothing sweeps D under X before the resume
    quit_member(&live, &c);
    assert!(finished(&live, &c), "C finished");
    team_run(&live, &team, &["list"]); // records C's end (its sweep ran before the reap)
    let c2 = s(&team_run(&live, &team, &["resume", "c1"])["agent"]["id"]);
    assert_ne!(c2, c, "a successor");
    assert_eq!(
        parent_and_depth(&live, &d).0,
        json!(c),
        "D still names C (C0 resolves it)"
    );
    team_run(&live, &team, &["stop", "c1"]);
    assert!(finished(&live, &d), "stop C′ stopped D");

    let x_name = member_file(&live, &x, "record.json")["name"].clone();
    quit_member(&live, &x);
    team_run(&live, &team, &["list"]);
    let rows = team_run(&live, &team, &["list", "-All"]);
    let row = rows["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == x_name)
        .cloned();
    assert_eq!(
        row.unwrap()["life"],
        "ended",
        "the root's row ended: {rows}"
    );
    assert_eq!(
        parent_and_depth(&live, &b),
        (json!(x), json!(1)),
        "B under the ended root"
    );
    assert_eq!(
        hc_get(&live, &address(&b))["state"],
        "live",
        "B still works"
    );
    let up2 = live.dir.join("up2.md");
    std::fs::write(&up2, "after x ended").unwrap();
    let up2 = up2.display().to_string();
    let r = by(
        &live,
        &mut sent,
        &b,
        &["send", "parent", "-BodyFile", up2.as_str()],
    );
    assert_eq!(r["ok"], true, "{r}");
    assert!(
        mail_bodies(&live, &x).contains(&json!("after x ended")),
        "X's mailbox after X ended"
    );
}
