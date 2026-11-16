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

// report-2b: the shared integration fixtures (private root, 150 s wait, deny_queries); occupy-3 uses them here.
mod native;

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

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
        match live.rmux().arg("-V").output() {
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
        let out = start.arg("cmd.exe").output().unwrap();
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
        let out = self.rmux().args(args).output().unwrap();
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
        let done = self.dir.join("call.done");
        let out = self.dir.join("call.json");
        let _ = std::fs::remove_file(&done);
        let args = args
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(" ");
        self.line(
            &self.root,
            &format!(
                "\"{MUX_AGENT}\" {args} > \"{}\" & echo done> \"{}\"",
                out.display(),
                done.display()
            ),
        );
        self.wait_or(
            &format!("command reply to {args}"),
            || done.exists().then_some(()),
            || format!("root pane:\n{}", self.screen(&self.root)),
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
        let _ = self.rmux().arg("kill-server").output();
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
        serde_json::json!({"ok":true,"verb":"stop","name":"w1","hcAddress":address,"life":"ended"})
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
    let address = s(&w["hcAddress"]);
    let get = |address: &str| {
        private_core::run(live.core.command(&["get", address, "--json"]), &live.dir).unwrap()
    };
    assert_eq!(
        live.run(&["stop", "w1"]),
        serde_json::json!({"ok":true,"verb":"stop","name":"w1","hcAddress":address,"life":"ended"})
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
    assert!(argv.len() >= 2, "{argv:?}");
    assert_eq!(
        argv[argv.len() - 2..],
        ["--resume".to_string(), sid.clone()],
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
    let get =
        |a: &str| private_core::run(live.core.command(&["get", a, "--json"]), &live.dir).unwrap();
    assert_eq!(live.run(&["stop", "w1"])["life"], "ended");
    live.wait("finished core terminal", || {
        (get(&address)["state"] == "finished").then_some(())
    });
    // E1: the launch stored the selected model as an option; the record is read as production wrote it.
    let r = member_file(&live, &s(&w["id"]), "record.json");
    assert_eq!(r["lifecycle"], 1, "{r}");
    assert!(r["prompt"].is_array(), "{r}");
    let opts: Vec<String> = serde_json::from_value(r["options"].clone()).unwrap();
    assert!(
        opts.ends_with(&["--model".to_string(), "x".to_string()]),
        "{opts:?}"
    );
    live.run(&["resume", "w1"]);
    let resumed = client_where(&live, "the resumed client", |a| {
        a.windows(2).any(|p| p[0] == "--resume" && p[1] == sid)
    });
    let argv: Vec<String> = resumed["argv"].as_array().unwrap().iter().map(s).collect();
    assert!(
        argv.windows(2).any(|p| p[0] == "--model" && p[1] == "x"),
        "{argv:?}"
    );
    assert_eq!(
        resumed["argv"],
        serde_json::json!(
            opts.iter()
                .cloned()
                .chain(["--resume".to_string(), sid.to_string()])
                .collect::<Vec<_>>()
        ),
        "the stored options only"
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
    assert_eq!(
        live.run_any(&["up", "-Client", "claude"])["check"],
        "leader"
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
/// mux-agent run by member `id`'s fake client (found by its record's session); its reply.
fn by(live: &Live, sent: &mut HashMap<u32, usize>, id: &str, args: &[&str]) -> Value {
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
    let probe = live
        .rmux()
        .args(["run-shell", "-b", "-C", &command])
        .output()
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
            live.core
                .command(&["read", &s(&grand["hcAddress"]), "--lines", "200"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
    };
    live.wait("hc keeps grand's marker", || {
        read().contains(&marker).then_some(())
    });
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
        let args = args
            .iter()
            .map(|a| format!("\"{a}\""))
            .collect::<Vec<_>>()
            .join(" ");
        self.line(
            pane,
            &format!(
                "cd /d \"{}\" & \"{MUX_AGENT}\" {args} > \"{}\" & echo done> \"{}\"",
                dir.display(),
                out.display(),
                done.display()
            ),
        );
        self.wait("reply in pane", || done.exists().then_some(()));
        serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap()
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
    assert_eq!(by(&live, &mut sent, &leader, &["stop", "b"])["ok"], true);
    assert_eq!(by(&live, &mut sent, &leader, &["resume", "b"])["ok"], true);
    // The resumed B: matched by session id or argv as on Unix, another process than its predecessor (F20).
    let new = client_pid(&live, &sid, old);
    assert_ne!(new, old, "a new process");
    let rebuilt = by_pid(&live, &mut sent, new, &["read", "-Deliver"]);
    assert_eq!(rebuilt["ids"], pending["ids"]);
    assert_ne!(rebuilt["batch"], pending["batch"]);
}

/// Posts a background line (`@bg`) to member `id`, once its client took the previous command; the path of its reply.
fn post_bg(live: &Live, sent: &mut HashMap<u32, usize>, id: &str, args: &[&str]) -> PathBuf {
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
    live.line(&live.root, &format!("\"{}\"", script.display()));
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
