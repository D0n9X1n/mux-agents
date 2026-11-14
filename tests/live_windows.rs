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
        std::fs::copy(
            env!("CARGO_BIN_EXE_fake-client"),
            dir.join("bin").join("claude.exe"),
        )
        .unwrap();
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
        let mut start = live.rmux();
        start.args(["new-session", "-d", "-s", "it", "-x", "200", "-y", "50"]);
        for (name, value) in &live.core.env {
            start.arg("-e").arg(assign(name, value));
        }
        start.arg("-e").arg(assign(
            "MUX_AGENTS_STATE",
            live.dir.join("state").as_os_str(),
        ));
        let out = start.arg("cmd.exe").output().unwrap();
        assert!(out.status.success(), "new-session: {out:?}");
        live.root = live
            .call(&["list-panes", "-a", "-F", "#{pane_id}"])
            .trim()
            .to_string();
        live.prompt(&live.root.clone());
        Some(live)
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
    fn wait<T>(&self, what: &str, mut ready: impl FnMut() -> Option<T>) -> T {
        let end = Instant::now() + WAIT;
        loop {
            if let Some(value) = ready() {
                return value;
            }
            assert!(Instant::now() < end, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn run(&self, args: &[&str]) -> Value {
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
        self.wait("command reply", || done.exists().then_some(()));
        let v: Value = serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap();
        assert_eq!(v["ok"], true, "{v}");
        v
    }

    /// `spawn -Client claude -TaskFile <dir>\task.txt -Name <name>` typed into the root pane; its JSON reply, which
    /// must be `ok`.
    fn spawn(&self, name: &str) -> Value {
        let file = |ext: &str| self.dir.join(format!("{name}.{ext}"));
        let line = format!(
            "\"{MUX_AGENT}\" spawn -Client claude -TaskFile \"{}\" -Name {name} > \"{}\" 2> \"{}\" & echo done> \"{}\"",
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
        ("read", "messages", serde_json::json!([])),
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
    assert_eq!(live.run(&["read"])["messages"].as_array().unwrap().len(), 1);
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
    assert_eq!(live.run(&["read"])["messages"], serde_json::json!([]));
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

// Stop an agent, then resume it (#103): the reply names X as `from` and the successor Y (same name, a new hc
// terminal); the client in Y's hc terminal runs `--resume <X's session>` as the last arguments, with no task after
// them; and Y's record names X in `resumedFrom`. The bar is checked once #122 (a bar per window) is in.
#[test]
fn hc_stop_then_resume_returns_with_its_conversation() {
    let Some(live) = Live::start() else {
        return;
    };
    live.run(&["sidebar", "-Close"]);
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
    assert_eq!(record["resumedFrom"], w["id"]);
    assert_eq!(record["sessionId"], w["sessionId"]);
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

    // A press read within 50 ms of a frame is dropped, as the user may have aimed at the frame before it: try again.
    let mut clicked = false;
    for _ in 0..3 {
        live.focus_root();
        let screen = live.screen(&bar);
        let y = row_at(&screen, "w2").unwrap_or_else(|| panic!("no w2 row:\n{screen}"));
        for end in ['M', 'm'] {
            live.keys(&bar, &format!("\x1b[<0;3;{y}{end}"));
        }
        if eventually(Duration::from_secs(5), || live.focused() == p2) {
            clicked = true;
            break;
        }
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
