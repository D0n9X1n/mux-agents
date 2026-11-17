#![cfg(feature = "live")]
//! Live tests: real tmux 3.8, tmux 3.2a and rmux servers on private sockets, never the user's own. Each server starts
//! from a temp dir with its own HOME and state, `claude`/`copilot` on PATH as the fake client, and dies with its guard.
//! Run with `cargo test --features live --test live -- --nocapture`; tmux 3.2a needs MUX_AGENTS_TMUX32 set from
//! `sh scripts/dev/tmux32.sh`.

#[cfg(unix)]
#[path = "support/launch_evidence.rs"]
mod launch_evidence;

// A3c Task 7: readiness barriers and the fake client's guards (PM calls 12, 26, 27).
#[cfg(unix)]
#[path = "support/ready.rs"]
mod ready;

#[cfg(unix)]
mod support;

// `ready.rs` reaches the one bounded runner as `crate::bounded`, as in tests/live_windows.rs.
#[cfg(unix)]
use support::bounded;

// report-2b: the shared integration fixtures (private root, 150 s wait, deny_queries); occupy-3 uses them here.
mod native;

#[cfg(unix)]
use support::{Dir, Mux, Server};

// outcome-2 call 14 (build checklist 4): a child past its bound is killed and refused `TimedOut` well before its own
// end; a watched child past its bound is killed by its watchdog.
#[cfg(unix)]
#[test]
fn a_child_past_its_bound_is_killed_and_refused() {
    use std::process::Command;
    use std::time::{Duration, Instant};
    let begun = Instant::now();
    let got =
        support::bounded::output(Command::new("/bin/sleep").arg("30"), Duration::from_secs(1));
    assert_eq!(
        got.map(|_| ()).map_err(|e| e.kind()),
        Err(std::io::ErrorKind::TimedOut)
    );
    assert!(
        begun.elapsed() < Duration::from_secs(10),
        "{:?}",
        begun.elapsed()
    );
    let mut held =
        support::bounded::spawn(Command::new("/bin/sleep").arg("30"), Duration::from_secs(1))
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

// o2 call 29 (QA F3): the Unix twin of live_windows.rs's: a descendant keeps stdout open 8 s after the child
// exits: refused `TimedOut` naming stdout, the read handle dropped; a normal child keeps its whole output.
#[cfg(unix)]
#[test]
fn bounded_output_never_answers_with_unread_output() {
    use std::process::Command;
    use std::time::{Duration, Instant};
    let begun = Instant::now();
    let got = support::bounded::output(
        Command::new("/bin/sh").args(["-c", "echo expected output; (sleep 8; echo late) &"]),
        support::bounded::LIMIT,
    );
    let took = begun.elapsed();
    let err = match got.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()) {
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
    let out = support::bounded::output(
        Command::new("/bin/sh").args([
            "-c",
            "i=1; while [ $i -le 3000 ]; do echo line-$i; i=$((i+1)); done",
        ]),
        support::bounded::LIMIT,
    )
    .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(text.lines().count(), 3000, "the whole output");
    assert_eq!(text.lines().last(), Some("line-3000"));
}

// Which multiplexers this run covers: each one's `-V` and binary, or why it is skipped. MUX_AGENTS_LIVE_REQUIRE
// (`tmux,tmux32,rmux`, any subset) turns a skip into a failure, here and in every scenario.
#[cfg(unix)]
#[test]
fn preflight_reports_each_multiplexer() {
    let _ = support::private_core::PrivateCore::installed();
    for mux in Mux::ALL {
        if let Some((bin, version)) = support::available(mux) {
            eprintln!("preflight {}: {version} at {}", mux.name(), bin.display());
        }
    }
}

// The harness itself on every mux: a plain shell answers keys, mux-agent runs in the pane with the harness's env and
// reaches this server's binary, the fake client records itself and runs a command, and the server dies on kill.
#[cfg(unix)]
#[test]
fn harness_starts_private_server_and_runs_shell() {
    let mut ran = 0;
    for mux in Mux::ALL {
        let dir = Dir::new();
        let Some(mut server) = Server::start(mux, &dir) else {
            continue;
        };

        // The root pane is `sh`, and it answers keys.
        server.send_keys(&format!("echo $$ > {}/shell.pid", dir.path().display()));
        // The shell's `>` creates the file before echo writes it: wait for the whole line.
        let pid_file = dir.path().join("shell.pid");
        let shell = server.wait("shell.pid", || {
            std::fs::read_to_string(&pid_file)
                .ok()
                .filter(|t| t.ends_with('\n'))
        });
        assert_eq!(shell.trim(), server.pane_pid().to_string(), "{mux:?}");

        // The harness's env reaches the pane, and nothing of the caller's own session does.
        let env = server.pane_env();
        let get = |name: &str| {
            env.lines()
                .find_map(|l| l.strip_prefix(&format!("{name}=")))
                .map(str::to_string)
        };
        assert_eq!(get("HOME"), Some(dir.join("home").display().to_string()));
        assert_eq!(
            get("XDG_STATE_HOME"),
            Some(dir.join("core-state").display().to_string())
        );
        assert_eq!(get("MUX_IT_DIR"), Some(dir.path().display().to_string()));
        // rmux puts its tmux shim first, inside the private runtime or temp dir; <dir>/bin precedes outside paths.
        let path = get("PATH").unwrap();
        let first_outside = path.split(':').find(|p| {
            !["run", "tmp"]
                .iter()
                .any(|sub| std::path::Path::new(p).starts_with(dir.join(sub)))
        });
        assert_eq!(
            first_outside,
            Some(dir.join("bin").to_str().unwrap()),
            "{mux:?} PATH={path}"
        );
        for unset in [
            "MUX_AGENTS_STATE",
            "CLAUDE_CODE_SESSION_ID",
            "COPILOT_AGENT_SESSION_ID",
        ] {
            assert_eq!(get(unset), None, "{mux:?} {unset}");
        }
        let endpoint = format!("{},{},0", server.socket().display(), server.pid());
        assert_eq!(get(mux.endpoint_var()), Some(endpoint), "{mux:?}");

        // mux-agent runs in the pane with a controlled env and its reply comes back: `list` on a fresh state root
        // answers no agents, after asking this server's own binary.
        let state = dir.join("state");
        let (code, reply) = server.run(&["list"], &[("MUX_AGENTS_STATE", state.to_str().unwrap())]);
        assert_eq!(code, 0, "{mux:?} {reply}");
        assert_eq!(
            reply,
            serde_json::json!({"ok": true, "verb": "list", "running": 0, "agents": []}),
            "{mux:?}"
        );
        let spawned = std::fs::read_to_string(dir.join("spawned.log")).unwrap();
        assert!(
            // The same file: rmux's server image is found through PATH, so the log may hold its symlink.
            spawned
                .lines()
                .any(|l| std::fs::canonicalize(l).is_ok_and(|p| p == server.binary())),
            "{mux:?}: {spawned}"
        );

        // `claude` on PATH is the fake client: it records argv, tty and pids, runs a command line, logs its input
        // and leaves with MUX_IT_EXIT on quit.
        server.send_keys(
            "MUX_IT_EXIT=3 claude --session-id 11111111-2222-3333-4444-555555555555 --model x",
        );
        let client = server.wait_client();
        assert_eq!(client["exe"], "claude", "{mux:?}");
        assert_eq!(
            client["argv"],
            serde_json::json!([
                "--session-id",
                "11111111-2222-3333-4444-555555555555",
                "--model",
                "x"
            ])
        );
        assert_eq!(client["sessionId"], "11111111-2222-3333-4444-555555555555");
        assert_eq!(client["ppid"], server.pane_pid(), "{mux:?}");
        assert_eq!(
            client["tty"],
            serde_json::json!({"stdin": true, "stdout": true})
        );
        let pid = client["pid"].as_u64().unwrap();

        let (code, reply) = server.command(pid, &["list"]);
        assert_eq!(code, 0, "{mux:?} {reply}");
        assert_eq!(
            reply,
            serde_json::json!({"ok": true, "verb": "list", "running": 0, "agents": []}),
            "{mux:?}"
        );

        server.send_keys("typed into claude");
        server.wait_line(&format!("input.{pid}.log"), "typed into claude");

        server.quit();
        assert_eq!(
            server.wait_file(&format!("exited.{pid}")).trim(),
            "3",
            "{mux:?}"
        );

        // kill leaves no server process.
        let pid = server.pid();
        server.kill();
        assert!(!support::alive(pid), "{mux:?}");
        eprintln!("harness ok on {mux:?}");
        ran += 1;
    }
    assert!(ran > 0, "no multiplexer to run on");
}

// End to end: each test starts its own private server, so a failure names one multiplexer and one scenario. The
// PowerShell integration suite at a96b866 (tests/integration) is the reference for what each one asserts.
#[cfg(unix)]
mod e2e {
    use std::collections::BTreeSet;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};

    use super::support::{self, Dir, Job, Mux, Run, Server};

    // The root agent's Claude session.
    const SID: &str = "11111111-2222-4333-8444-555555555555";
    const LONG: Duration = Duration::from_secs(60);
    // Run by the root pane's shell: a session file as Claude writes it, the mode line on the screen, then the client.
    const ROOT_SH: &str = r#"printf '{"pid":%s,"sessionId":"%s","cwd":"%s"}\n' "$$" "$CLAUDE_CODE_SESSION_ID" "$(pwd -P)" > "$HOME/.claude/sessions/$$.json"
echo '  ⏵⏵ bypass permissions on (shift+tab to cycle)'
exec claude --dangerously-skip-permissions
"#;

    /// One test's server and dir; the server drops first (killed, its lock files removed), then the dir.
    struct Live {
        server: Server,
        dir: Dir,
        // The root agent client's pid; 0 for a human root.
        root_pid: u64,
    }

    impl Live {
        /// A human caller: the root pane is `sh`; each run passes MUX_AGENTS_STATE=<dir>/state.
        fn human(mux: Mux) -> Option<Live> {
            let dir = Dir::new();
            let server = Server::start(mux, &dir)?;
            std::fs::write(dir.join("task.txt"), "say hi").unwrap();
            Some(Live {
                server,
                dir,
                root_pid: 0,
            })
        }

        /// A Claude agent caller in full bypass, as the reference's root: the pane execs the fake
        /// `claude --dangerously-skip-permissions` with CLAUDE_CODE_SESSION_ID, CLAUDE_CODE_ENTRYPOINT=cli and
        /// MUX_AGENTS_STATE set in that shell only, after leaving a bypassPermissions transcript, a session file and
        /// the mode line on its screen.
        fn agent(mux: Mux) -> Option<Live> {
            let mut live = Live::human(mux)?;
            let projects = live.dir.join("home/.claude/projects/it");
            std::fs::create_dir_all(&projects).unwrap();
            std::fs::write(
                projects.join(format!("{SID}.jsonl")),
                "{\"type\":\"permission-mode\",\"permissionMode\":\"bypassPermissions\"}\n",
            )
            .unwrap();
            let script = live.dir.join("root.sh");
            std::fs::write(&script, ROOT_SH).unwrap();
            live.server.send_keys(&format!(
                "export CLAUDE_CODE_SESSION_ID={SID} CLAUDE_CODE_ENTRYPOINT=cli MUX_AGENTS_STATE={}; exec sh {}",
                support::quote(&live.state_text()),
                support::quote(script.to_str().unwrap()),
            ));
            let root = live.server.wait_client();
            assert_eq!(
                root["pid"],
                live.server.pane_pid(),
                "{mux:?}: the root client is the pane's process"
            );
            live.root_pid = root["pid"].as_u64().unwrap();
            Some(live)
        }

        fn mux(&self) -> Mux {
            self.server.mux()
        }

        fn state(&self) -> PathBuf {
            self.dir.join("state")
        }

        fn state_text(&self) -> String {
            self.state().display().to_string()
        }

        /// `<state>/<server key>`.
        fn keyed(&self) -> PathBuf {
            self.state().join(self.server.key())
        }

        fn agent_dir(&self, id: &str) -> PathBuf {
            self.keyed().join("agents").join(id)
        }

        /// A member file where its generation keeps it: `gens/<generation>/` of a lifecycle record, else the folder.
        fn gen_file(&self, id: &str, file: &str) -> PathBuf {
            let folder = self.agent_dir(id);
            let r = json_file(&folder.join("record.json"));
            match (r["lifecycle"].as_u64(), r["generation"].as_str()) {
                (Some(1), Some(n)) => folder.join("gens").join(n).join(file),
                _ => folder.join(file),
            }
        }

        /// o2 calls 33/35: waits for member `w`'s `ready.json` (its fake client's startup report promoted it), so a test
        /// that then stops, kills or quits it ends an ordinary member, not one failed before readiness (which outcome-2
        /// rightly flags `recover`).
        fn ready(&self, w: &Value) {
            let id = s(&w["id"]).to_string();
            let path = || self.gen_file(&id, "ready.json");
            crate::native::Deadline::start(crate::native::READY).wait(
                "ready.json",
                || path().exists().then_some(()),
                || format!("{}: {}", s(&w["name"]), path().display()),
            );
        }

        /// The view a member's launch recorded (V1): `view.json`, never record.json's presentation fields.
        fn view_of(&self, id: &str) -> Value {
            json_file(&self.agent_dir(id).join("view.json"))
        }

        fn path(&self, rel: &str) -> String {
            self.dir.join(rel).display().to_string()
        }

        /// mux-agent from the root pane's shell, with this test's state root.
        fn run(&mut self, args: &[&str]) -> (i32, Value) {
            let state = self.state_text();
            self.server
                .run(args, &[("MUX_AGENTS_STATE", state.as_str())])
        }

        /// mux-agent run by the root agent client.
        fn root(&mut self, args: &[&str]) -> (i32, Value) {
            let pid = self.root_pid;
            self.server.command(pid, args)
        }

        fn spawn_args(&self, client: &str, name: &str, extra: &[&str]) -> Vec<String> {
            let mut args: Vec<String> = ["spawn", "-Client", client, "-TaskFile"]
                .iter()
                .map(|a| a.to_string())
                .collect();
            args.extend([self.path("task.txt"), "-Name".into(), name.into()]);
            args.extend(extra.iter().map(|a| a.to_string()));
            args
        }

        /// `spawn -Client <client> -TaskFile <dir>/task.txt -Name <name> <extra>` from the root.
        fn try_spawn_client(&mut self, client: &str, name: &str, extra: &[&str]) -> (i32, Value) {
            let args = self.spawn_args(client, name, extra);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            if self.root_pid == 0 {
                self.run(&args)
            } else {
                self.root(&args)
            }
        }

        fn try_spawn(&mut self, name: &str, extra: &[&str]) -> (i32, Value) {
            self.try_spawn_client("claude", name, extra)
        }

        /// A claude spawn that must succeed.
        fn spawn(&mut self, name: &str, extra: &[&str]) -> Value {
            let (code, reply) = self.try_spawn(name, extra);
            assert_eq!(code, 0, "{:?} spawn {name}: {reply}", self.mux());
            reply
        }

        /// `sidebar -Close`, which must succeed: every bar of the session is gone and they stay off. A test counting
        /// mux calls closes them first, since each watch also calls the multiplexer every 2 s.
        fn close_bar(&mut self) -> Value {
            let (code, close) = self.run(&["sidebar", "-Close"]);
            assert_eq!(code, 0, "{:?} {close}", self.mux());
            for pane in close["panes"].as_array().into_iter().flatten() {
                self.wait_gone(pane.as_str().unwrap());
            }
            close
        }

        /// A new detached session `name` of this server running `sh` in `<dir>/work`; answers its pane.
        fn new_session(&self, name: &str) -> String {
            let work = self.path("work");
            self.call(&[
                "new-session",
                "-d",
                "-s",
                name,
                "-x",
                "200",
                "-y",
                "50",
                "-c",
                &work,
                "sh",
            ]);
            self.call(&["list-panes", "-t", name, "-F", "#{pane_id}"])
                .trim()
                .to_string()
        }

        /// The side-bar panes of the server, as `<pane>|<window>|<width>`.
        fn bars(&self) -> Vec<String> {
            self.panes("#{pane_id}|#{window_id}|#{pane_width}|#{@agent-role}|#{@agent-id}")
                .into_iter()
                .filter(|r| r.ends_with("|sidebar|"))
                .map(|r| r.rsplitn(3, '|').nth(2).unwrap().to_string())
                .collect()
        }

        /// The bar's width by S1: `max(min, window * percent / 100)` of the width its window reads now, with
        /// the width options as the bar's pane reads them.
        fn bar_width(&self, bar: &str) -> String {
            let window: u32 = self.num(bar, "#{window_width}");
            let options = self.field(
                bar,
                "#{@mux-agents-sidebar-min}|#{@mux-agents-sidebar-percent}",
            );
            let (min, percent) = options.split_once('|').unwrap_or(("", ""));
            let sizing = mux_agent::sidebar::Sizing::read(min, percent);
            mux_agent::sidebar::bar_width(window, sizing).to_string()
        }

        /// The bar's place as `<at right>|<at top>|<at bottom>|<width>`: `1|1|1|<bar_width>` when it is locked to
        /// the right edge, full height, at its width.
        fn bar_place(&self, bar: &str) -> String {
            let format = "#{pane_at_right}|#{pane_at_top}|#{pane_at_bottom}|#{pane_width}";
            let end = Instant::now() + Duration::from_secs(5);
            loop {
                // Never empty when read: see `num`.
                let place = self.field(bar, format);
                if place.matches('|').count() == 3 || Instant::now() >= end {
                    return place;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        /// What `bar_place` answers for a bar in place.
        fn in_place(&self, bar: &str) -> String {
            format!("1|1|1|{}", self.bar_width(bar))
        }

        /// The active pane of `target`'s window.
        fn active(&self, target: &str) -> String {
            self.call(&[
                "list-panes",
                "-t",
                target,
                "-F",
                "#{pane_id}|#{pane_active}",
            ])
            .lines()
            .find_map(|row| row.strip_suffix("|1"))
            .unwrap_or_else(|| panic!("{:?}: no active pane in {target}'s window", self.mux()))
            .to_string()
        }

        /// A new `sh` pane split from `target` with `flags` (`-h` or `-v`), focus kept.
        fn split(&self, target: &str, flags: &[&str]) -> String {
            let mut args = vec!["split-window", "-d"];
            args.extend(flags);
            args.extend(["-P", "-F", "#{pane_id}", "-t", target, "sh"]);
            self.call(&args).trim().to_string()
        }

        /// mux-agent from `pane`'s shell, with this test's state root.
        fn run_in(&mut self, pane: &str, args: &[&str]) -> (i32, Value) {
            let state = self.state_text();
            self.server
                .run_in(pane, args, &[("MUX_AGENTS_STATE", state.as_str())])
        }

        /// `sidebar` from the root pane, which must open the bar; answers it once it is in place, its output piped
        /// to `<dir>/frames.<n>` so `wait_tick` can count its redraws.
        fn open_bar(&mut self) -> String {
            let (code, open) = self.run(&["sidebar"]);
            assert_eq!(
                (code, &open["opened"]),
                (0, &json!(true)),
                "{:?} {open}",
                self.mux()
            );
            let bar = s(&open["pane"]).to_string();
            self.wait_place(&bar, "the bar to open in place");
            self.log_frames(&bar);
            bar
        }

        /// Pipes the bar's output to `<dir>/frames.<n>`, so `frames` counts its redraws from here.
        fn log_frames(&self, bar: &str) {
            let log = self.frames_path(bar);
            let pipe = format!("cat >> {}", support::quote(log.to_str().unwrap()));
            self.call(&["pipe-pane", "-o", "-t", bar, &pipe]);
        }

        /// A left click on column 3 of the bar's row `y` (1-based), as the bar's pane gets it with SGR mouse
        /// reporting: the press, then the release.
        fn press(&self, bar: &str, y: usize) {
            for end in ['M', 'm'] {
                self.call(&["send-keys", "-t", bar, "-l", &format!("\x1b[<0;3;{y}{end}")]);
            }
        }

        /// The 1-based line of the bar's screen whose second token is `name`: that agent's name line.
        fn row_of(&self, bar: &str, name: &str) -> usize {
            let screen = self.call(&["capture-pane", "-p", "-t", bar]);
            row_at(&screen, name)
                .unwrap_or_else(|| panic!("{:?}: no row for {name}:\n{screen}", self.mux()))
        }

        /// Returns just after a redraw of the bar: its next tick is about 1.9 s away.
        fn after_redraw(&self, bar: &str) {
            let n = self.frames(bar);
            self.server
                .wait("a redraw", || (self.frames(bar) > n).then_some(()));
            std::thread::sleep(Duration::from_millis(100));
        }

        /// The active pane of the session's active window.
        fn focused(&self) -> String {
            self.panes("#{pane_id}|#{window_active}|#{pane_active}")
                .iter()
                .filter(|r| r.ends_with("|1|1"))
                .map(|r| r.split('|').next().unwrap().to_string())
                .collect::<Vec<_>>()
                .join(",")
        }

        /// Clicks are off with MUX_AGENTS_ASCII=1: a scenario that clicks asserts the server does not set it.
        fn assert_not_ascii(&self) {
            let out = self
                .server
                .try_call(&["show-environment", "-g", "MUX_AGENTS_ASCII"]);
            let shown = String::from_utf8_lossy(&out.stdout);
            assert!(
                !out.status.success() || shown.starts_with('-'),
                "{:?}: MUX_AGENTS_ASCII is set: {shown}",
                self.mux()
            );
        }

        fn frames_path(&self, bar: &str) -> PathBuf {
            self.dir.join(format!("frames.{}", &bar[1..]))
        }

        /// The bar's redraws since `log_frames`: each frame starts with synchronized output on and home (S2.8).
        fn frames(&self, bar: &str) -> usize {
            std::fs::read(self.frames_path(bar)).map_or(0, |bytes| {
                bytes
                    .windows(11)
                    .filter(|w| *w == b"\x1b[?2026h\x1b[H")
                    .count()
            })
        }

        /// Returns once the bar's watch has run a whole tick that started after the call: two more redraws than
        /// counted now, since the first may end a tick already under way and the next tick starts 2 s after it.
        /// (The multiplexer resumes a stopped pane process at once, so a watch cannot be paused from outside.)
        fn wait_tick(&self, bar: &str) {
            let want = self.frames(bar) + 2;
            self.server
                .wait("two redraws", || (self.frames(bar) >= want).then_some(()))
        }

        /// Waits until the bar is at the right edge, full height, at its width.
        fn wait_place(&self, bar: &str, what: &str) {
            self.server.wait(what, || {
                (self.bar_place(bar) == self.in_place(bar)).then_some(())
            })
        }

        /// Ends the bar's watch, keeping its pane and its tag: from here no tick can repair anything.
        fn freeze(&self, bar: &str) {
            self.call(&["respawn-pane", "-k", "-t", bar, "sleep", "1000"]);
        }

        /// Background mux-agent runs from the root pane's shell, each with this test's state root.
        fn bg(&mut self, runs: &[&[&str]]) -> Vec<Job> {
            let state = self.state_text();
            let vars = [("MUX_AGENTS_STATE", state.as_str())];
            let none: &[&str] = &[];
            let runs: Vec<Run> = runs.iter().map(|args| (*args, &vars[..], none)).collect();
            self.server.bg(&runs)
        }

        fn call(&self, args: &[&str]) -> String {
            self.server.call(args)
        }

        /// `display-message -p -t <target> <format>`, its line end cut.
        fn field(&self, target: &str, format: &str) -> String {
            self.call(&["display-message", "-p", "-t", target, format])
                .trim_end()
                .to_string()
        }

        /// A number `field` answers, read again until it parses: tmux 3.2a can answer `display-message -p` with
        /// success and no output (seen on a loaded CI runner), and a missing pane already fails in `call`. After 5 s
        /// it fails with every pane's place.
        fn num<T: std::str::FromStr>(&self, target: &str, format: &str) -> T {
            let end = Instant::now() + Duration::from_secs(5);
            loop {
                let got = self.field(target, format);
                if let Ok(n) = got.parse() {
                    return n;
                }
                assert!(
                    Instant::now() < end,
                    "{:?}: {target} {format} = {got:?}; panes {:?}",
                    self.server.mux(),
                    self.panes("#{pane_id}|#{pane_dead}|#{pane_left}|#{pane_right}|#{pane_pid}|#{@agent-role}")
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        /// `list-panes -a -F <format>`, one row each.
        fn panes(&self, format: &str) -> Vec<String> {
            self.call(&["list-panes", "-a", "-F", format])
                .lines()
                .map(str::to_string)
                .collect()
        }

        /// The panes tagged with an agent id, as `<pane>|<name>`, sorted.
        fn tagged(&self) -> Vec<String> {
            let mut rows: Vec<String> = self
                .panes("#{pane_id}|#{@agent-id}|#{@agent-name}")
                .into_iter()
                .filter_map(|row| {
                    let mut parts = row.splitn(3, '|');
                    let (pane, id, name) = (parts.next()?, parts.next()?, parts.next()?);
                    (id.len() == 12).then(|| format!("{pane}|{name}"))
                })
                .collect();
            rows.sort();
            rows
        }

        /// Waits until `pane` is no longer listed.
        fn wait_gone(&self, pane: &str) {
            self.server.wait(&format!("pane {pane} to close"), || {
                (!self.panes("#{pane_id}").iter().any(|p| p == pane)).then_some(())
            })
        }

        /// The parsed lines of mailbox `<key>.jsonl`; none when it does not exist.
        fn mailbox(&self, key: &str) -> Vec<Value> {
            let path = self.keyed().join("mail").join(format!("{key}.jsonl"));
            std::fs::read_to_string(path)
                .unwrap_or_default()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        }

        /// The root pane's mailbox key.
        fn root_box(&self) -> String {
            format!("pane-{}", &self.server.pane()[1..])
        }

        /// Unread bytes in the root pane's mailbox: its length past the read offset.
        fn unread(&self) -> u64 {
            let mail = self.keyed().join("mail");
            let key = self.root_box();
            let len = std::fs::metadata(mail.join(format!("{key}.jsonl"))).map_or(0, |m| m.len());
            // The cursor (4b-i) holds the offset; before its first commit, a legacy offset may.
            let cursor = std::fs::read_to_string(mail.join(format!("{key}.cursor.json")))
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .and_then(|c| c["offset"].as_u64());
            let offset = cursor.unwrap_or_else(|| {
                std::fs::read_to_string(mail.join(format!("{key}.offset")))
                    .ok()
                    .and_then(|t| t.trim().parse().ok())
                    .unwrap_or(0)
            });
            len - offset
        }

        /// A `read -Wait` job seen waiting, as the reference's driver judged it: 3 s after it started it is still
        /// running, has made more mux calls since than one pass makes (each pass lists the panes), and nothing is
        /// unread.
        fn armed(&self, job: Job) {
            let log = self.dir.join("spawned.log");
            let count = || std::fs::read_to_string(&log).map_or(0, |t| t.lines().count());
            let before = count();
            std::thread::sleep(Duration::from_secs(3));
            let after = count();
            let mux = self.mux();
            assert!(
                !self.server.job_done(job),
                "{mux:?}: the reader returned before any message"
            );
            assert!(
                support::alive(self.server.job_pid(job)),
                "{mux:?}: the reader is gone"
            );
            assert!(
                after >= before + 2,
                "{mux:?}: the reader is not polling ({before} -> {after} mux calls)"
            );
            assert_eq!(self.unread(), 0, "{mux:?}: mail was already waiting");
        }

        /// The fake client record of `pid`.
        fn client(&self, pid: u64) -> Value {
            json_file(&self.dir.join(format!("client.{pid}.json")))
        }

        /// The pid of the worker client of a spawn reply.
        fn worker_pid(&self, spawned: &Value) -> u64 {
            if let Some(id) = spawned["id"].as_str() {
                self.proof_of(id);
            }
            self.server.client_by_session(s(&spawned["sessionId"]))["pid"]
                .as_u64()
                .unwrap()
        }

        /// PM call 26: waits for member `id`'s proof evidence (its active generation's client.json) before it is
        /// commanded. A pre-lifecycle member has none and is not waited for.
        fn proof_of(&self, id: &str) {
            crate::ready::wait_member(&self.agent_dir(id));
        }

        fn client_of(&self, id: &str) -> u64 {
            self.proof_of(id);
            let text = std::fs::read_to_string(self.agent_dir(id).join("record.json")).unwrap();
            let r: Value = serde_json::from_str(&text).unwrap();
            self.server
                .client_by_session(r["sessionId"].as_str().unwrap())["pid"]
                .as_u64()
                .unwrap()
        }
        /// `spawn` by member `id`'s client (found by its record's session); members pass files (D1).
        fn spawn_by(&mut self, id: &str, name: &str) -> (i32, Value) {
            let pid = self.client_of(id);
            let (task, brief) = (self.path("task.txt"), self.path("brief.md"));
            self.server.command(
                pid,
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
        }

        /// A detached session whose first pane starts in `dir`; its pane id.
        fn session_in(&self, name: &str, dir: &Path) -> String {
            std::fs::create_dir_all(dir).unwrap();
            let d = dir.display().to_string();
            self.call(&[
                "new-session",
                "-d",
                "-s",
                name,
                "-x",
                "200",
                "-y",
                "50",
                "-c",
                &d,
                "sh",
            ]);
            self.call(&["list-panes", "-t", name, "-F", "#{pane_id}"])
                .trim()
                .to_string()
        }

        /// mux-agent run by member `id` (its fake client).
        fn by(&mut self, id: &str, args: &[&str]) -> (i32, Value) {
            let pid = self.client_of(id);
            self.server.command(pid, args)
        }

        /// Waits for the fake client of session `sid` (started with `--session-id sid`, or resumed with `sid` in its
        /// argv) that is not `old`; its pid (`msg-plan-4b-i-3.md` F20).
        fn successor(&self, id: &str, sid: &str, old: u64) -> u64 {
            self.proof_of(id);
            self.server.wait(&format!("successor of {old}"), || {
                let log = std::fs::read_to_string(self.dir.join("clients.log")).ok()?;
                log.lines().rev().find_map(|p| {
                    let text =
                        std::fs::read_to_string(self.dir.join(format!("client.{}.json", p.trim())))
                            .ok()?;
                    let r: Value = serde_json::from_str(&text).ok()?;
                    let mine = r["sessionId"] == sid
                        || r["argv"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|x| x == sid));
                    let pid = r["pid"].as_u64()?;
                    (mine && pid != old).then_some(pid)
                })
            })
        }
    }

    fn s(value: &Value) -> &str {
        value
            .as_str()
            .unwrap_or_else(|| panic!("not a string: {value}"))
    }

    fn json_file(path: &Path) -> Value {
        let text =
            std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}: {text}", path.display()))
    }

    fn append(path: &Path, text: &str) {
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }

    fn strings(value: &Value) -> Vec<&str> {
        value.as_array().unwrap().iter().map(s).collect()
    }

    /// Each message's body: the text after its `[mux-agent from ...]` header line.
    fn bodies(reply: &Value) -> Vec<String> {
        // A served batch (4b-i) is one `text`: each entry starts at its header and ends in LF.
        s(&reply["text"])
            .split("[mux-agent from ")
            .skip(1)
            .map(|e| {
                e.trim_end_matches('\n')
                    .split_once('\n')
                    .map_or("", |(_, b)| b)
                    .to_string()
            })
            .collect()
    }

    fn names(reply: &Value) -> Vec<String> {
        let mut names: Vec<String> = reply["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| s(&a["name"]).to_string())
            .collect();
        names.sort();
        names
    }

    /// True when a `less` runs below process `root`, by ps's parent links.
    fn pagers_under(root: u64) -> bool {
        let out = crate::support::bounded::output(
            Command::new("ps").args(["-axo", "pid=,ppid=,comm="]),
            crate::support::bounded::LIMIT,
        )
        .unwrap();
        let rows: Vec<(u64, u64, String)> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let mut f = l.split_whitespace();
                let pid = f.next()?.parse().ok()?;
                let ppid = f.next()?.parse().ok()?;
                Some((pid, ppid, f.collect::<Vec<_>>().join(" ")))
            })
            .collect();
        let mut below = vec![root];
        let mut i = 0;
        while i < below.len() {
            let parent = below[i];
            below.extend(rows.iter().filter(|r| r.1 == parent).map(|r| r.0));
            i += 1;
        }
        rows.iter()
            .any(|r| below.contains(&r.0) && r.2.rsplit('/').next() == Some("less"))
    }

    /// `/dev/<tty>` of a process, by ps.
    fn tty_of(pid: u64) -> String {
        let out = crate::support::bounded::output(
            Command::new("ps").args(["-o", "tty=", "-p", &pid.to_string()]),
            crate::support::bounded::LIMIT,
        )
        .unwrap();
        format!("/dev/{}", String::from_utf8_lossy(&out.stdout).trim())
    }

    fn mux_name(mux: Mux) -> &'static str {
        match mux {
            Mux::Rmux => "rmux",
            Mux::Tmux38 | Mux::Tmux32 => "tmux",
        }
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

    // The root runs unsandboxed by the production signals, as a full-bypass agent in a terminal; its driver's verb
    // passes the sandbox check and reaches this server, and the root finishes with its own exit code.
    fn root_unsandboxed_terminal_driver_finished(mux: Mux) {
        let Some(mut live) = Live::agent(mux) else {
            return;
        };
        let root = live.client(live.root_pid);
        assert_eq!(root["exe"], "claude", "{mux:?}");
        assert_eq!(
            root["argv"],
            json!(["--dangerously-skip-permissions"]),
            "{mux:?}"
        );
        assert_eq!(root["worker"], false, "{mux:?}");
        assert_eq!(
            root["tty"],
            json!({"stdin": true, "stdout": true}),
            "{mux:?}"
        );
        assert_eq!(root["env"]["CLAUDE_CODE_SESSION_ID"], SID, "{mux:?}");
        assert_eq!(root["env"]["CLAUDE_CODE_ENTRYPOINT"], "cli", "{mux:?}");
        // No SANDBOX_RUNTIME on the server, and the OS signal the entry reads (macOS sandbox_check, Linux
        // NoNewPrivs) says unsandboxed for this process tree.
        assert!(
            !live
                .server
                .try_call(&["show-environment", "-g", "SANDBOX_RUNTIME"])
                .status
                .success(),
            "{mux:?}"
        );
        let signals = mux_agent::proc::host().sandbox();
        assert!(
            mux_agent::sandbox::decide(&signals).is_ok(),
            "{mux:?}: {signals:?}"
        );
        let (code, reply) = live.root(&["list"]);
        assert_eq!(code, 0, "{mux:?} {reply}");
        assert_eq!(
            reply,
            json!({"ok": true, "verb": "list", "running": 0, "agents": []}),
            "{mux:?}"
        );
        live.server.quit();
        let exited = live.server.wait_file(&format!("exited.{}", live.root_pid));
        assert_eq!(exited.trim(), "0", "{mux:?}");
    }

    // spawn makes a tagged window and a tagged split beside the caller, both forks of a same-client caller.
    fn spawn_window_and_split_tagged(mux: Mux) {
        let Some(mut live) = Live::agent(mux) else {
            return;
        };
        let root = live.server.pane().to_string();
        let root_window = live.field(&root, "#{window_id}");
        let w1 = live.spawn("w1", &[]);
        let w2 = live.spawn("w2", &["-Split", "h"]);
        for w in [&w1, &w2] {
            assert_eq!(w["context"], "fork", "{mux:?} {w}");
            assert_eq!(w["reason"], "same-client", "{mux:?} {w}");
        }
        assert_ne!(w1["window"], root_window.as_str(), "{mux:?}");
        assert_eq!(w2["window"], root_window.as_str(), "{mux:?}");
        let mut tags: Vec<String> = live
            .panes("#{pane_id}|#{window_id}|#{@agent-id}|#{@agent-name}|#{@agent-parent}|#{@agent-context}")
            .into_iter()
            .filter(|row| row.split('|').nth(2).is_some_and(|id| id.len() == 12))
            .collect();
        tags.sort();
        let mut want: Vec<String> = [(&w1, "w1"), (&w2, "w2")]
            .iter()
            .map(|(w, name)| {
                format!(
                    "{}|{}|{}|{name}|{root}|fork:same-client",
                    s(&w["pane"]),
                    s(&w["window"]),
                    s(&w["id"])
                )
            })
            .collect();
        want.sort();
        assert_eq!(tags, want, "{mux:?}");
    }

    // A tab is titled with the agent's name and keeps it (automatic-rename off) once its client runs; a split's pane
    // title is the name, and the active pane stays the caller's. A client that sets its own title (OSC 0) after it
    // starts leaves the pane title alone (allow-set-title off), except on tmux 3.2a, which has no such option: there
    // the title may change, and the spawn still succeeded.
    fn spawn_titles_tab_and_pane(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let root = live.server.pane().to_string();
        let active = |live: &Live| {
            live.panes("#{pane_id}|#{window_active}|#{pane_active}")
                .iter()
                .filter(|r| r.ends_with("|1|1"))
                .map(|r| r.split('|').next().unwrap().to_string())
                .collect::<Vec<_>>()
                .join(",")
        };
        // The client's own title escape, written to its tty, then a marker the pane must show: once it does, the
        // multiplexer has read the escape before it.
        let retitle = |live: &Live, client: &Value, pane: &str, marker: &str| {
            let tty = tty_of(client["pid"].as_u64().unwrap());
            let mut out = std::fs::OpenOptions::new().write(true).open(&tty).unwrap();
            write!(out, "\x1b]0;client-title\x07{marker}\r\n").unwrap();
            live.server.wait(&format!("{marker} on {pane}"), || {
                live.call(&["capture-pane", "-p", "-t", pane])
                    .contains(marker)
                    .then_some(())
            });
        };
        let tab = live.spawn("tab-agent", &[]);
        let client = live.server.client_by_session(s(&tab["sessionId"]));
        let window = s(&tab["window"]);
        assert_eq!(live.field(window, "#{window_name}"), "tab-agent", "{mux:?}");
        let rename = live.call(&["show-options", "-wv", "-t", window, "automatic-rename"]);
        assert_eq!(rename.trim(), "off", "{mux:?}");
        retitle(&live, &client, s(&tab["pane"]), "tab-marker");
        assert_eq!(live.field(window, "#{window_name}"), "tab-agent", "{mux:?}");
        let split = live.spawn("split-agent", &["-Split", "h"]);
        let client = live.server.client_by_session(s(&split["sessionId"]));
        assert_eq!(
            live.field(s(&split["pane"]), "#{pane_title}"),
            "split-agent",
            "{mux:?}"
        );
        assert_eq!(active(&live), root, "{mux:?}");
        retitle(&live, &client, s(&split["pane"]), "split-marker");
        let titles = [
            live.field(s(&tab["pane"]), "#{pane_title}"),
            live.field(s(&split["pane"]), "#{pane_title}"),
        ];
        if mux == Mux::Tmux32 {
            // The documented limit: no allow-set-title, so the client's title may win; both agents still run.
            assert_eq!(live.tagged().len(), 2, "{mux:?} {titles:?}");
        } else {
            assert_eq!(titles, ["tab-agent", "split-agent"], "{mux:?}");
        }
        assert_eq!(active(&live), root, "{mux:?}");
    }

    // Two identical spawns with no -Name at once: the spawn lock serializes them into distinct names.
    fn unnamed_spawns_at_once_get_distinct_names(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let task = live.path("task.txt");
        let args = ["spawn", "-Client", "claude", "-TaskFile", task.as_str()];
        let jobs = live.bg(&[&args[..], &args[..]]);
        let mut names: Vec<String> = jobs
            .iter()
            .map(|j| {
                let (code, reply) = live.server.job_wait(*j, LONG);
                assert_eq!(code, 0, "{mux:?} {reply}");
                s(&reply["name"]).to_string()
            })
            .collect();
        names.sort();
        assert_eq!(names, ["say-hi", "say-hi-2"], "{mux:?}");
        let mut tagged: Vec<String> = live
            .tagged()
            .iter()
            .map(|row| row.split('|').nth(1).unwrap().to_string())
            .collect();
        tagged.sort();
        assert_eq!(tagged, names, "{mux:?}");
    }

    // spawn writes server.json, a lifecycle record.json, generation 0's spec.json and commit.json, then view.json, tags
    // the pane, and its launcher (the core-owned process) runs the PATH client with the record's session.
    fn spawn_tags_pane_writes_record_and_commit_runs_client(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let root = live.server.pane().to_string();
        let w1 = live.spawn("w1", &[]);
        let (id, pane, sid) = (s(&w1["id"]), s(&w1["pane"]), s(&w1["sessionId"]));
        assert_eq!(
            (
                s(&w1["name"]),
                s(&w1["client"]),
                s(&w1["context"]),
                s(&w1["reason"])
            ),
            ("w1", "claude", "brief", "human"),
            "{mux:?}"
        );
        let server = json_file(&live.keyed().join("server.json"));
        assert_eq!(
            server["socket"],
            live.server.socket().display().to_string(),
            "{mux:?}"
        );
        assert_eq!(server["serverPid"], live.server.pid(), "{mux:?}");
        let folder = live.agent_dir(id);
        let record = json_file(&folder.join("record.json"));
        let session = live.field(&root, "#{session_id}");
        let want = [
            ("id", json!(id)),
            ("name", json!("w1")),
            ("client", json!("claude")),
            ("sessionId", json!(sid)),
            ("context", json!("brief")),
            ("reason", json!("human")),
            ("session", json!(session)),
            ("parentPane", json!(root)),
            ("parentAgent", Value::Null),
            ("directory", json!(live.path("work"))),
            ("taskLine", json!("say hi")),
        ];
        for (field, value) in want {
            assert_eq!(record[field], value, "{mux:?} record.{field}");
        }
        assert_eq!(
            (&record["lifecycle"], &record["generation"]),
            (&json!(1), &json!("0")),
            "{mux:?}"
        );
        for k in ["pane", "window", "startCommand"] {
            assert!(record.get(k).is_none(), "{mux:?} record.{k}");
        }
        let view = live.view_of(id);
        assert_eq!(
            (&view["pane"], &view["window"], &view["gen"]),
            (&json!(pane), &w1["window"], &json!(0)),
            "{mux:?}"
        );
        assert!(!folder.join("commit.json").exists(), "{mux:?}");
        assert!(live.gen_file(id, "commit.json").is_file(), "{mux:?}");
        let spec = json_file(&live.gen_file(id, "spec.json"));
        assert_eq!(spec["id"], id, "{mux:?}");
        assert_eq!(spec["client"], "claude", "{mux:?}");
        assert_eq!(spec["parentPane"], root.as_str(), "{mux:?}");
        assert_eq!(
            spec["stateDirectory"],
            live.keyed().display().to_string(),
            "{mux:?}"
        );
        assert_eq!(
            live.field(
                pane,
                "#{@agent-id}|#{@agent-name}|#{@agent-parent}|#{@agent-context}"
            ),
            format!("{id}|w1|{root}|brief:human"),
            "{mux:?}"
        );
        let client = live.server.client_by_session(sid);
        assert_eq!(client["env"]["MUX_AGENTS_GEN"], "0", "{mux:?}");
        let argv = strings(&client["argv"]);
        assert_eq!(argv.len(), 6, "{mux:?} {argv:?}");
        assert_eq!(
            argv[..5],
            [
                "--dangerously-skip-permissions",
                "--effort",
                "high",
                "--session-id",
                sid
            ],
            "{mux:?}"
        );
        let head = format!(
            "[mux-agent] You are w1 (claude, id {id}), started in this {} session.",
            mux_name(mux)
        );
        assert!(
            argv[5].starts_with(&head) && argv[5].ends_with("\n\nsay hi"),
            "{mux:?}: {}",
            argv[5]
        );
        assert_eq!(client["cwd"], live.path("work"), "{mux:?}");
        let launcher = core_launcher(&live, &w1, &client);
        assert!(launcher["start"].is_i64(), "{mux:?} {launcher}");
        let (code, list) = live.run(&["list"]);
        assert_eq!(
            (code, names(&list)),
            (0, vec!["w1".to_string()]),
            "{mux:?} {list}"
        );
    }

    /// report-2b live (PM calls 4 and 18): a new member's client.json and ready.json within 150 s. With `barrier`,
    /// the launcher holds client.json until report.json exists, and its mark proves that order.
    fn ready_within_150_s(mux: Mux, barrier: bool) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        if barrier {
            crate::native::private_root(&live.state()); // PM call 16
            std::fs::write(live.state().join("test-client-barrier"), "").unwrap();
        }
        // r2b call 35: one deadline from the launch boundary; the spawn and every wait up to ready.json spend it.
        let deadline = crate::native::Deadline::start(crate::native::READY);
        let w1 = live.spawn("w1", &[]);
        let (id, sid) = (s(&w1["id"]).to_string(), s(&w1["sessionId"]).to_string());
        let read = |f: &str| {
            let text = std::fs::read_to_string(live.gen_file(&id, f)).ok()?;
            serde_json::from_str::<Value>(&text).ok()
        };
        let (c, r) = deadline.wait(
            "client.json and ready.json",
            || Some((read("client.json")?, read("ready.json")?)),
            || {
                format!(
                    "{mux:?}: client {:?} ready {:?}",
                    read("client.json"),
                    read("ready.json")
                )
            },
        );
        assert_eq!(
            (&r["pid"], &r["start"]),
            (&c["pid"], &c["start"]),
            "{mux:?}"
        );
        // After ready.json: the client lookup waits on its own, outside the readiness deadline.
        let client = live.server.client_by_session(&sid);
        let env = &client["env"];
        assert!(
            env["HC_TERMINAL_ID"]
                .as_str()
                .is_some_and(|v| !v.is_empty()),
            "{mux:?}: {client}"
        );
        assert!(
            env["TMUX"].is_null() && env["RMUX"].is_null(),
            "{mux:?}: the member sees no multiplexer: {client}"
        );
        if barrier {
            assert_eq!(
                barrier_marks(&live.state()),
                ["report-first"],
                "{mux:?}: report.json before client.json"
            );
        }
    }

    fn barrier_marks(state: &Path) -> Vec<String> {
        std::fs::read_dir(state)
            .unwrap()
            .filter_map(|e| {
                let e = e.ok()?;
                let name = e.file_name().to_string_lossy().into_owned();
                name.starts_with("test-barrier-")
                    .then(|| std::fs::read_to_string(e.path()).unwrap())
            })
            .collect()
    }

    fn a_new_member_reports_ready_within_150_s(mux: Mux) {
        ready_within_150_s(mux, false)
    }

    fn a_new_member_reports_before_its_client_json_is_published(mux: Mux) {
        ready_within_150_s(mux, true)
    }

    fn core_launcher(live: &Live, w: &Value, client: &Value) -> Value {
        let launcher = json_file(&live.gen_file(s(&w["id"]), "launcher.json"));
        assert_eq!(client["ppid"], launcher["pid"]);
        assert_ne!(
            launcher["pid"],
            live.num::<u64>(s(&w["pane"]), "#{pane_pid}")
        );
        assert_eq!(
            core(live, &["get", s(&w["hcAddress"]), "--json"])["pid"],
            launcher["pid"]
        );
        launcher
    }

    // Each worker is the PATH claude, forked from the caller's session, with a core-owned launcher and PTY.
    fn worker_is_path_claude_forked_own_tty(mux: Mux) {
        let Some(mut live) = Live::agent(mux) else {
            return;
        };
        let root_tty = tty_of(live.root_pid);
        assert_eq!(
            root_tty,
            live.field(live.server.pane(), "#{pane_tty}"),
            "{mux:?}"
        );
        let mut ttys = vec![root_tty];
        for (name, extra) in [("w1", &[][..]), ("w2", &["-Split", "h"][..])] {
            let w = live.spawn(name, extra);
            let sid = s(&w["sessionId"]);
            let client = live.server.client_by_session(sid);
            let argv = strings(&client["argv"]);
            assert_eq!(argv.len(), 9, "{mux:?} {argv:?}");
            assert_eq!(
                argv[..8],
                [
                    "--dangerously-skip-permissions",
                    "--effort",
                    "high",
                    "--resume",
                    SID,
                    "--fork-session",
                    "--session-id",
                    sid
                ],
                "{mux:?}"
            );
            assert!(
                argv[8].starts_with(&format!("[mux-agent] You are {name} (claude")),
                "{mux:?}"
            );
            assert_eq!(client["exe"], "claude", "{mux:?}");
            assert_eq!(
                client["tty"],
                json!({"stdin": true, "stdout": true}),
                "{mux:?}"
            );
            let pane = s(&w["pane"]);
            let launcher = core_launcher(&live, &w, &client);
            let tty = tty_of(client["pid"].as_u64().unwrap());
            assert_eq!(tty, tty_of(launcher["pid"].as_u64().unwrap()));
            assert_ne!(tty, live.field(pane, "#{pane_tty}"));
            ttys.push(tty);
        }
        let distinct: BTreeSet<&String> = ttys.iter().collect();
        assert_eq!(distinct.len(), 3, "{mux:?}: {ttys:?}");
        let claude = std::fs::canonicalize(live.dir.join("bin/claude")).unwrap();
        let spawned = std::fs::read_to_string(live.dir.join("spawned.log")).unwrap();
        let started = spawned
            .lines()
            .filter(|l| std::fs::canonicalize(l).is_ok_and(|p| p == claude))
            .count();
        assert_eq!(started, 2, "{mux:?}: {spawned}");
    }

    // A state root set only in the caller's shell reaches each new pane; the server never carries it.
    fn state_root_reaches_pane_not_server(mux: Mux) {
        let Some(mut live) = Live::agent(mux) else {
            return;
        };
        let unset = |live: &Live| {
            !live
                .server
                .try_call(&["show-environment", "-g", "MUX_AGENTS_STATE"])
                .status
                .success()
        };
        assert!(unset(&live), "{mux:?}");
        let state = live.state_text();
        assert_eq!(
            live.client(live.root_pid)["env"]["MUX_AGENTS_STATE"],
            state.as_str()
        );
        for (name, extra) in [("w1", &[][..]), ("w2", &["-Split", "h"][..])] {
            let w = live.spawn(name, extra);
            let client = live.server.client_by_session(s(&w["sessionId"]));
            assert_eq!(
                client["env"]["MUX_AGENTS_STATE"],
                state.as_str(),
                "{mux:?} {name}"
            );
            assert!(
                live.agent_dir(s(&w["id"])).join("record.json").is_file(),
                "{mux:?}"
            );
        }
        assert!(unset(&live), "{mux:?}");
        assert!(
            !live.dir.join("home/.local/state/mux-agents").exists(),
            "{mux:?}"
        );
    }

    // list, send and read <name> answer.
    fn list_send_read_name_answer(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let w1 = live.spawn("w1", &[]);
        live.spawn("w2", &[]);
        let (code, list) = live.run(&["list"]);
        assert_eq!(
            (code, names(&list)),
            (0, vec!["w1".to_string(), "w2".into()]),
            "{mux:?}"
        );
        assert_eq!(list["running"], 2, "{mux:?}");
        std::fs::write(live.dir.join("to-w1.txt"), "hello w1").unwrap();
        let body = live.path("to-w1.txt");
        let (code, sent) = live.run(&["send", "w1", "-BodyFile", &body]);
        assert_eq!(code, 0, "{mux:?} {sent}");
        // A send in a session with hc members takes the team path (4a): it names its recipients, not a pane.
        assert_eq!(
            (&sent["to"], sent["id"].is_string(), &sent["recipients"]),
            (&json!("w1"), true, &json!([{"name": "w1", "id": w1["id"]}])),
            "{mux:?} {sent}"
        );
        // read <name> is the pane's screen: here a line typed into the worker.
        let pid = live.worker_pid(&w1);
        let pane = s(&w1["pane"]);
        live.call(&["send-keys", "-t", pane, "-l", "worker ready"]);
        live.call(&["send-keys", "-t", pane, "Enter"]);
        live.server
            .wait_line(&format!("input.{pid}.log"), "worker ready");
        let (code, read) = live.run(&["read", "w1", "-Lines", "20"]);
        assert_eq!(code, 0, "{mux:?} {read}");
        assert_eq!(
            (&read["name"], &read["pane"]),
            (&json!("w1"), &w1["pane"]),
            "{mux:?}"
        );
        assert!(
            s(&read["text"]).lines().any(|l| l.trim() == "worker ready"),
            "{mux:?}: {read}"
        );
        let (code, missing) = live.run(&["read", "nobody", "-Lines", "5"]);
        assert_eq!((code, &missing["check"]), (2, &json!("target")), "{mux:?}");
    }

    // send appends to the agent's mailbox and never types into its pane; the worker's reply reaches the parent's.
    fn send_delivers_to_mailbox(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let root = live.server.pane().to_string();
        let w1 = live.spawn("w1", &[]);
        let pid = live.worker_pid(&w1);
        std::fs::write(live.dir.join("to-w1.txt"), "hello w1").unwrap();
        let body = live.path("to-w1.txt");
        let (code, sent) = live.run(&["send", "w1", "-BodyFile", &body]);
        assert_eq!(code, 0, "{mux:?} {sent}");
        let mail = live.mailbox(s(&w1["id"]));
        assert_eq!(mail.len(), 1, "{mux:?} {mail:?}");
        assert_eq!(
            (&mail[0]["body"], &mail[0]["from"], &mail[0]["pane"]),
            (&json!("hello w1"), &json!("root"), &json!(root)),
            "{mux:?}"
        );
        std::thread::sleep(Duration::from_secs(1));
        let typed =
            std::fs::read_to_string(live.dir.join(format!("input.{pid}.log"))).unwrap_or_default();
        assert!(!typed.contains("hello w1"), "{mux:?}: typed {typed:?}");

        std::fs::write(live.dir.join("reply.md"), "reply from w1").unwrap();
        let (code, sent) = live
            .server
            .command(pid, &["send", "parent", "-BodyFile", "{dir}/reply.md"]);
        assert_eq!(code, 0, "{mux:?} {sent}");
        // The team path (4a): w1 has no running parent member, so `parent` is its parentPane, the root.
        assert_eq!(
            (&sent["to"], sent["id"].is_string(), &sent["recipients"]),
            (&json!("parent"), true, &json!([{"name": root, "id": null}])),
            "{mux:?} {sent}"
        );
        let mail = live.mailbox(&live.root_box());
        assert_eq!(mail.len(), 1, "{mux:?} {mail:?}");
        assert_eq!(
            (&mail[0]["body"], &mail[0]["from"], &mail[0]["pane"]),
            (&json!("reply from w1"), &json!("w1"), &w1["pane"]),
            "{mux:?}"
        );
        let (code, read) = live.run(&["read"]);
        assert_eq!(
            (code, bodies(&read)),
            (0, vec!["reply from w1".to_string()]),
            "{mux:?}"
        );
        let head = format!("[mux-agent from w1 ({}) #", s(&w1["pane"]));
        assert!(s(&read["text"]).starts_with(&head), "{mux:?} {read}");
        // Nothing is consumed without an ack (4b-i): the next read acks this batch and finds nothing new.
        let (_, again) = live.run(&["read", "-Deliver", "-Ack", s(&read["batch"])]);
        assert_eq!(again["ids"], json!([]), "{mux:?} {again}");
    }

    // read -Wait waits on an empty mailbox and returns each worker's reply soon after it is sent.
    fn read_wait_wakes_on_message(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let workers = [live.spawn("w1", &[]), live.spawn("w2", &["-Split", "h"])];
        // The bar's watch calls the multiplexer too; without it, only the reader can grow the count armed reads.
        live.close_bar();
        let mut got = Vec::new();
        // Nothing is consumed without an ack (4b-i): each reader acks the batch the one before it was served.
        let mut acked: Option<String> = None;
        for w in &workers {
            let sid = s(&w["sessionId"]);
            let pid = live.worker_pid(w);
            std::fs::write(
                live.dir.join(format!("reply-{sid}.md")),
                format!("reply from {sid}"),
            )
            .unwrap();
            let reader: Vec<&str> = match &acked {
                Some(b) => vec!["read", "-Wait", "30", "-Ack", b],
                None => vec!["read", "-Wait", "30"],
            };
            let job = live.bg(&[&reader])[0];
            live.armed(job);
            let file = format!("{{dir}}/reply-{sid}.md");
            let (code, sent) = live
                .server
                .command(pid, &["send", "parent", "-BodyFile", &file]);
            assert_eq!(code, 0, "{mux:?} {sent}");
            let at = Instant::now();
            let (code, reply) = live.server.job_wait(job, LONG);
            let took = at.elapsed();
            assert_eq!(code, 0, "{mux:?} {reply}");
            assert!(
                took < Duration::from_secs(5),
                "{mux:?}: the reader took {took:?} after the send"
            );
            assert_eq!(
                reply["ids"].as_array().map(Vec::len),
                Some(1),
                "{mux:?} {reply}"
            );
            // The sender is the entry's header: `[mux-agent from <name> (<pane>) #<id>]`.
            let from = s(&reply["text"])
                .strip_prefix("[mux-agent from ")
                .and_then(|h| h.split_once(' '))
                .map_or("", |(name, _)| name)
                .to_string();
            got.push(format!("{from}:{}", bodies(&reply)[0]));
            acked = reply["batch"].as_str().map(String::from);
        }
        let want: Vec<String> = ["w1", "w2"]
            .iter()
            .zip(&workers)
            .map(|(name, w)| format!("{name}:reply from {}", s(&w["sessionId"])))
            .collect();
        assert_eq!(got, want, "{mux:?}");
    }

    // focus selects the window and the pane, also a split beside an active pane.
    fn focus_selects_pane(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let w1 = live.spawn("w1", &[]);
        let w2 = live.spawn("w2", &["-Split", "h"]);
        let active = |live: &Live| {
            live.panes("#{pane_id}|#{window_active}|#{pane_active}")
                .iter()
                .filter(|r| r.ends_with("|1|1"))
                .map(|r| r.split('|').next().unwrap().to_string())
                .collect::<Vec<_>>()
                .join(",")
        };
        assert_eq!(active(&live), live.server.pane(), "{mux:?}");
        let (code, focused) = live.run(&["focus", "w1"]);
        assert_eq!(code, 0, "{mux:?} {focused}");
        assert_eq!(
            (&focused["pane"], &focused["window"]),
            (&w1["pane"], &w1["window"]),
            "{mux:?}"
        );
        assert_eq!(active(&live), s(&w1["pane"]), "{mux:?}");
        let (code, focused) = live.run(&["focus", "w2"]);
        assert_eq!(code, 0, "{mux:?} {focused}");
        assert_eq!(active(&live), s(&w2["pane"]), "{mux:?}");
        // with no client attached nobody is looking at an agent, so -Next goes to the most urgent: w1, which
        // needs the person, ranks before w2; -Prev to the last.
        let pid = live.worker_pid(&w1);
        support::write_atomic(&live.dir.join(format!("cmd.{pid}")), "report\tblocked\n");
        let reply = live.dir.join(format!("reply.{pid}.1.json"));
        assert!(
            eventually(Duration::from_secs(30), || reply.is_file()),
            "{mux:?}: the report did not run"
        );
        assert_eq!(json_file(&reply)["ok"], true);
        let (code, focused) = live.run(&["focus", "-Next"]);
        assert_eq!(
            (code, &focused["name"]),
            (0, &w1["name"]),
            "{mux:?} {focused}"
        );
        assert_eq!(active(&live), s(&w1["pane"]), "{mux:?}");
        let (code, focused) = live.run(&["focus", "-Prev"]);
        assert_eq!(
            (code, &focused["name"]),
            (0, &w2["name"]),
            "{mux:?} {focused}"
        );
        assert_eq!(active(&live), s(&w2["pane"]), "{mux:?}");
    }

    // stop posts `stopped` to a reader already waiting, which returns it soon after.
    fn stop_notifies_waiting_reader(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let w1 = live.spawn("w1", &[]);
        let w2 = live.spawn("w2", &["-Split", "h"]);
        live.worker_pid(&w1);
        live.worker_pid(&w2);
        live.close_bar();
        let reader: &[&str] = &["read", "-Wait", "60"];
        let job = live.bg(&[reader])[0];
        live.armed(job);
        let (code, stop) = live.run(&["stop", "w1"]);
        let at = Instant::now();
        assert_eq!(code, 0, "{mux:?} {stop}");
        assert_eq!(
            stop,
            json!({"ok":true,"verb":"stop","name":w1["name"],"hcAddress":w1["hcAddress"],"life":"ended","stopped":true,"unresolved":[]})
        );
        let (code, reply) = live.server.job_wait(job, LONG);
        let took = at.elapsed();
        assert_eq!(
            (code, bodies(&reply)),
            (0, vec!["w1 was stopped.".to_string()]),
            "{mux:?}"
        );
        assert!(
            took < Duration::from_secs(5),
            "{mux:?}: the reader took {took:?} after stop"
        );
        let (code, stop) = live.run(&["stop", "w2"]);
        assert_eq!(
            stop,
            json!({"ok":true,"verb":"stop","name":w2["name"],"hcAddress":w2["hcAddress"],"life":"ended","stopped":true,"unresolved":[]})
        );
        assert_eq!(code, 0);
        // Nothing is consumed without an ack (4b-i): this read acks w1's batch, then waits for w2's.
        let (code, read) = live.run(&["read", "-Wait", "30", "-Ack", s(&reply["batch"])]);
        assert_eq!(
            (code, bodies(&read)),
            (0, vec!["w2 was stopped.".to_string()]),
            "{mux:?}"
        );
    }

    // Stop ends core-owned launchers and clients; attachment views disappear without a pane kill or quit file.
    // The detached Unix child remains fixture-owned and is cleaned up by its Sentinels guard.
    fn stop_leaves_no_surviving_process(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let (mut pids, mut workers) = (Vec::new(), Vec::new());
        for (name, extra) in [("w1", &[][..]), ("w2", &["-Split", "h"][..])] {
            let w = live.spawn(name, extra);
            pids.push(live.worker_pid(&w) as u32);
            let launcher = json_file(&live.gen_file(s(&w["id"]), "launcher.json"));
            pids.push(launcher["pid"].as_u64().unwrap() as u32);
            workers.push(w);
        }
        let worker = u64::from(pids[0]);
        let (code, detached) = live.server.command(worker, &["@detach"]);
        assert_eq!(code, 0, "{mux:?} {detached}");
        let child = detached["pid"].as_u64().unwrap() as u32;
        let child_start = support::start_of(child);
        // Killed when the test ends, whatever happened: it outlives the server and is outside its tree by then.
        let _child = support::Sentinels(vec![(child, child_start)]);
        let row = mux_agent::proc::host()
            .row(child)
            .expect("the detached child");
        assert_eq!(
            u64::from(row.ppid),
            worker,
            "{mux:?}: not the worker's child"
        );
        for w in &workers {
            let (code, stop) = live.run(&["stop", s(&w["name"])]);
            assert_eq!(code, 0, "{stop}");
            assert_eq!(
                stop,
                json!({"ok":true,"verb":"stop","name":w["name"],"hcAddress":w["hcAddress"],"life":"ended","stopped":true,"unresolved":[]})
            );
            assert!(eventually(LONG, || core(
                &live,
                &["get", s(&w["hcAddress"]), "--json"]
            )["state"]
                == "finished"));
            live.wait_gone(s(&w["pane"]));
        }
        assert!(eventually(LONG, || !pids
            .iter()
            .any(|p| support::alive(*p))));
        assert!(
            live.tagged().is_empty(),
            "live agent view tags survived stop"
        );
        assert!(!live.dir.join("quit").exists());
        for pid in pids.iter().copied().chain([child]) {
            assert!(!live.dir.join(format!("quit.{pid}")).exists());
        }
    }

    // A client that exits posts one `exited` notice with its code.
    fn client_exit_posts_one_exited_notice(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        live.close_bar();
        let w1 = live.spawn("w1", &[]);
        let client = live.server.client_by_session(s(&w1["sessionId"]));
        let pid = client["pid"].as_u64().unwrap();
        support::write_atomic(&live.dir.join(format!("quit.{pid}")), "7\n");
        assert_eq!(
            live.server.wait_file(&format!("exited.{pid}")).trim(),
            "7",
            "{mux:?}"
        );
        one_exited_notice(&mut live, &w1, 7);
    }

    // The single `exited` notice of `w` with `code`: notice.json, launcher.json, one mailbox line, one read.
    fn one_exited_notice(live: &mut Live, w: &Value, code: i64) {
        let a = s(&w["hcAddress"]);
        assert!(eventually(
            LONG,
            || core(live, &["get", a, "--json"])["state"] == "finished"
        ));
        assert!(!live.gen_file(s(&w["id"]), "notice.json").exists());
        assert!(exit_messages(live, w).is_empty());
        let (status, list) = live.run(&["list"]);
        assert_eq!(status, 0, "{list}");
        let notice = json_file(&live.gen_file(s(&w["id"]), "notice.json"));
        assert_eq!(
            (&notice["kind"], &notice["exitCode"]),
            (&json!("exited"), &json!(code))
        );
        assert_eq!(
            json_file(&live.gen_file(s(&w["id"]), "exit.json"))["exitCode"],
            code
        );
        let (status, read) = live.run(&["read"]);
        assert_eq!(
            (status, bodies(&read)),
            (
                0,
                vec![format!("{} exited with code {code}.", s(&w["name"]))]
            )
        );
        // Nothing is consumed without an ack (4b-i): acking the notice's batch leaves nothing new.
        let (_, again) = live.run(&["read", "-Deliver", "-Ack", s(&read["batch"])]);
        assert_eq!(again["ids"], json!([]), "{again}");
        assert_eq!(exit_messages(live, w).len(), 1);
        let (_, all) = live.run(&["list", "-All"]);
        assert_eq!(all["agents"][0]["state"], format!("exited {code}"));
        assert_eq!(all["agents"][0]["life"], "ended");
    }

    // A PATH `claude` that cannot start (its shebang names a missing interpreter, so the start fails NotFound) still
    // leaves exit code 1 and posts one `exited` notice, never `lost`.
    fn client_start_error_posts_one_exited_notice(mux: Mux) {
        use std::os::unix::fs::PermissionsExt;
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        live.close_bar();
        let claude = live.dir.join("bin/claude");
        std::fs::remove_file(&claude).unwrap();
        std::fs::write(&claude, "#!/nonexistent/mux-it/interpreter\n").unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let w1 = live.spawn("w1", &[]);
        one_exited_notice(&mut live, &w1, 1);
        assert!(
            !live.dir.join("clients.log").exists(),
            "{mux:?}: a client started"
        );
        let spawned = std::fs::read_to_string(live.dir.join("spawned.log")).unwrap();
        assert!(
            spawned.lines().any(|l| Path::new(l) == claude),
            "{mux:?}: {spawned}"
        );
    }

    // An agent pane cannot spawn; the real multiplexer tells a gone pane from a dead socket, and so does mux-agent.
    fn tagged_pane_cannot_spawn_and_gone_pane_vs_dead_socket(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        std::fs::write(live.dir.join("nested.txt"), "nested").unwrap();
        let w1 = live.spawn("w1", &[]);
        let w2 = live.spawn("w2", &["-Split", "h"]);
        for w in [&w1, &w2] {
            let pid = live.worker_pid(w);
            let (code, reply) = live.server.command(
                pid,
                &[
                    "spawn",
                    "-Client",
                    "claude",
                    "-TaskFile",
                    "{dir}/nested.txt",
                ],
            );
            assert_eq!(code, 2, "{mux:?} {reply}");
            assert_eq!(
                (&reply["ok"], &reply["code"], &reply["check"]),
                (&json!(false), &json!(2), &json!("tagged")),
                "{mux:?}"
            );
        }
        assert_eq!(live.tagged().len(), 2, "{mux:?}");
        let (p1, p2) = (s(&w1["pane"]), s(&w2["pane"]));
        let probe = "#{pane_id}|#{@agent-id}";
        live.call(&["kill-pane", "-t", p1]);
        live.wait_gone(p1);
        let answer = |out: std::process::Output| {
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).trim_end().to_string(),
            )
        };
        let gone = live
            .server
            .try_call(&["display-message", "-p", "-t", p1, probe]);
        assert_eq!(answer(gone), (Some(0), "|".to_string()), "{mux:?}");
        let alive = live
            .server
            .try_call(&["display-message", "-p", "-t", p2, probe]);
        assert_eq!(
            answer(alive),
            (Some(0), format!("{p2}|{}", s(&w2["id"]))),
            "{mux:?}"
        );
        let dead = crate::support::bounded::output(
            Command::new(live.server.binary())
                .arg("-S")
                .arg(format!("{}.none", live.server.socket().display()))
                .args(["-N", "display-message", "-p", "-t", p2, probe]),
            crate::support::bounded::LIMIT,
        )
        .unwrap();
        assert_ne!(dead.status.code(), Some(0), "{mux:?}");
        assert_detached_running(&mut live, &w1);
        assert!(exit_messages(&live, &w2).is_empty());
        let (_, list) = live.run(&["list"]);
        assert_eq!(
            list["agents"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["name"] == "w2")
                .unwrap()["view"],
            "attached"
        );
    }

    // A bar's live row (S2.1): `◉ w[12] … working <n>[sm]`.
    fn bar_row(line: &str) -> bool {
        let words: Vec<&str> = line.split_whitespace().collect();
        let ["\u{25c9}", "w1" | "w2", "working", age] = words[..] else {
            return false;
        };
        age.len() >= 2
            && age.ends_with(['s', 'm'])
            && age[..age.len() - 1].bytes().all(|b| b.is_ascii_digit())
    }

    // A live row's state word, by the agent's name.
    fn bar_word(screen: &str, name: &str) -> Option<String> {
        screen.lines().find_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            (words.get(1) == Some(&name))
                .then(|| words.get(2).map(|w| w.to_string()))
                .flatten()
        })
    }

    fn bar_ready(screen: &str) -> bool {
        let lines: Vec<&str> = screen.lines().map(str::trim).collect();
        let count = |f: &dyn Fn(&str) -> bool| lines.iter().filter(|l| f(l)).count();
        // Both counts (no report yet: working), or the working count alone in a bar too narrow for both (a bar set
        // to 30 columns).
        let header = |l: &str| {
            let words: Vec<&str> = l.split_whitespace().collect();
            words == ["mux-agents", "2", "working", "/", "0", "ended"]
                || words == ["mux-agents", "2", "working"]
        };
        count(&header) == 1 && count(&|l| l == "checking the side bar") == 2 && count(&bar_row) == 2
    }

    // Every captured row fits the bar's width in display columns, and no control text leaked onto the screen.
    fn bar_clean(live: &Live, bar: &str, screen: &str) {
        let width: usize = live.num(bar, "#{pane_width}");
        for line in screen.lines() {
            assert!(
                mux_agent::paint::width(line) <= width,
                "{:?}: {line:?} is wider than {width}",
                live.mux()
            );
            assert!(
                !line.contains('\u{1b}')
                    && !line.contains("2026")
                    && !line.contains("[K")
                    && !line.contains("[J")
                    && !line.contains("|sidebar|"),
                "{:?}: stray text {line:?}",
                live.mux()
            );
        }
    }

    // S2 (QA round 2): a doing line of emoji ZWJ sequences, which tmux 3.2a draws part by part (4 columns each), never
    // wraps: after the bar redrew, its first row is still the header and no row is wider than the pane.
    fn bar_keeps_its_header_with_emoji_sequences(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let w1 = live.spawn("w1", &[]);
        let bar = s(&w1["sidebar"]).to_string();
        let pid = live.worker_pid(&w1);
        let coder = "\u{1f469}\u{200d}\u{1f4bb}";
        std::fs::write(live.dir.join("emoji.txt"), coder.repeat(16)).unwrap();
        let (code, status) = live
            .server
            .command(pid, &["status", "-BodyFile", "{dir}/emoji.txt"]);
        assert_eq!(code, 0, "{mux:?} {status}");
        let mut screen = String::new();
        let drawn = eventually(Duration::from_secs(30), || {
            screen = live.call(&["capture-pane", "-p", "-t", &bar]);
            screen.contains('\u{1f4bb}')
        });
        assert!(drawn, "{mux:?}: the bar shows:\n{screen}");
        // Two more redraws (a tick is 2 s): a wrapped row would have scrolled the header off by now.
        std::thread::sleep(Duration::from_millis(4500));
        screen = live.call(&["capture-pane", "-p", "-t", &bar]);
        let first = screen.lines().next().unwrap_or_default();
        assert!(
            first.trim_start().starts_with("mux-agents") && first.contains("working"),
            "{mux:?}: the first row is {first:?}:\n{screen}"
        );
        // Not wrapped: the doing line is one row, and every drawn row starts with the bar's margin (a wrapped rest
        // would start at column 0).
        let rows: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(
            rows.iter().filter(|l| l.contains('\u{1f4bb}')).count(),
            1,
            "{mux:?}: wrapped:\n{screen}"
        );
        assert!(
            rows.iter().all(|l| l.starts_with(' ')),
            "{mux:?}: wrapped:\n{screen}"
        );
        assert!(screen.contains('\u{1f4bb}'), "{mux:?}:\n{screen}");
        bar_clean(&live, &bar, &screen);
    }

    // S2, the bar draws in color at its pane's size with nothing stray; an agent in `read -Wait` with no
    // report holds a live lease, so it shows `on call` (W6) until mail wakes it, then `working` again, and its waiting
    // file is gone. An agent whose
    // client reports `blocked` comes first under herdr's `Needs you` heading, the header counting it; its next report
    // moves it back.
    fn bar_shows_waiting_then_working(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let w1 = live.spawn("w1", &[]);
        let w2 = live.spawn("w2", &[]);
        let bar = s(&w1["sidebar"]).to_string();
        let pid = live.worker_pid(&w1);
        live.worker_pid(&w2);
        // w1's client runs `read -Wait 60` from its own pane (its first command, so its reply is number 1).
        support::write_atomic(&live.dir.join(format!("cmd.{pid}")), "read\t-Wait\t60\n");
        let mut screen = String::new();
        let waiting = eventually(Duration::from_secs(30), || {
            screen = live.call(&["capture-pane", "-p", "-t", &bar]);
            // Two states, so herdr's headings over compact rows: on call `◌ w1`, working `◉ w2`.
            screen.contains("\u{25cc} w1")
                && screen.contains("\u{25c9} w2")
                && screen.contains("On call \u{b7} 1")
                && screen.contains("Working \u{b7} 1")
        });
        assert!(waiting, "{mux:?}: the bar shows:\n{screen}");
        // w2's client asks the person: needs you, ranked first, counted in the header.
        let pid2 = live.worker_pid(&w2);
        support::write_atomic(
            &live.dir.join(format!("cmd.{pid2}")),
            "report\tblocked\t-Kind\tquestion\n",
        );
        let blocked = eventually(Duration::from_secs(30), || {
            screen = live.call(&["capture-pane", "-p", "-t", &bar]);
            let rows = [row_at(&screen, "w2"), row_at(&screen, "w1")];
            // Two states: herdr's headings, and under them the compact row (glyph, name, time).
            screen.contains("! w2")
                && screen.contains("Needs you \u{b7} 1")
                && screen.contains("On call \u{b7} 1")
                && screen.contains("1 need you")
                && matches!(rows, [Some(a), Some(b)] if a < b)
        });
        assert!(blocked, "{mux:?}: the bar shows:\n{screen}");
        assert!(screen.contains("! w2"), "{mux:?}:\n{screen}");
        // nobody looks at w2's pane, so the bar tells the session's clients once. tmux 3.8 keeps the command in
        // its message log; tmux 3.2a shows that log only to a client and rmux keeps none, so theirs is the unit tests'
        // to prove.
        if mux == Mux::Tmux38 {
            let mut log = String::new();
            let told = eventually(Duration::from_secs(10), || {
                log = live.call(&["show-messages"]);
                log.contains("mux-agents: w2 needs you")
            });
            assert!(told, "{mux:?}: no alert in\n{log}");
            assert_eq!(log.matches("w2 needs you").count(), 1, "{mux:?}:\n{log}");
        }
        support::write_atomic(&live.dir.join(format!("cmd.{pid2}")), "report\tworking\n");
        let unblocked = eventually(Duration::from_secs(30), || {
            screen = live.call(&["capture-pane", "-p", "-t", &bar]);
            screen.contains("\u{25c9} w2") && !screen.contains("need you")
        });
        assert!(unblocked, "{mux:?}: the bar shows:\n{screen}");
        bar_clean(&live, &bar, &screen);
        let folder = live.agent_dir(s(&w1["id"]));
        let files = || -> Vec<String> {
            std::fs::read_dir(&folder)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("waiting-"))
                .collect()
        };
        assert_eq!(files().len(), 1, "{mux:?}");
        let escaped = live.call(&["capture-pane", "-e", "-p", "-t", &bar]);
        assert!(
            escaped.contains("mux-agents") && escaped.contains('\u{1b}'),
            "{mux:?}: no color in {escaped:?}"
        );
        let (code, sent) = live.run(&["send", "w1", "-Body", "go"]);
        assert_eq!(code, 0, "{mux:?} {sent}");
        let reply = live.dir.join(format!("reply.{pid}.1.json"));
        assert!(
            eventually(Duration::from_secs(30), || reply.is_file()),
            "{mux:?}: the read did not return"
        );
        let back = eventually(Duration::from_secs(30), || {
            screen = live.call(&["capture-pane", "-p", "-t", &bar]);
            bar_word(&screen, "w1").as_deref() == Some("working")
        });
        assert!(back, "{mux:?}: the bar shows:\n{screen}");
        assert_eq!(files(), Vec::<String>::new(), "{mux:?}");
        bar_clean(&live, &bar, &screen);
    }

    // The first spawn beside the caller opens its window's one bar at the right edge, `bar_width` columns, showing
    // the count and each status, outside list; the next spawn in that window and sidebar answer the same bar; -Close
    // ends it and its watch.
    fn sidebar_opens_at_bar_width_and_closes(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let workers = [
            live.spawn("w1", &["-Split", "h"]),
            live.spawn("w2", &["-Split", "h"]),
        ];
        let opened = s(&workers[0]["sidebar"]).to_string();
        assert_eq!(workers[1]["sidebar"], opened.as_str(), "{mux:?}");
        std::fs::write(live.dir.join("status.txt"), "checking the side bar").unwrap();
        for w in &workers {
            let pid = live.worker_pid(w);
            let (code, status) = live
                .server
                .command(pid, &["status", "-BodyFile", "{dir}/status.txt"]);
            assert_eq!(
                (code, &status["text"]),
                (0, &json!("checking the side bar")),
                "{mux:?} {status}"
            );
        }
        let (code, open) = live.run(&["sidebar"]);
        assert_eq!(
            (code, &open["opened"], &open["pane"]),
            (0, &json!(false), &json!(opened)),
            "{mux:?} {open}"
        );
        let bar = s(&open["pane"]).to_string();
        let (code, again) = live.run(&["sidebar"]);
        assert_eq!(
            (code, &again["opened"], &again["pane"]),
            (0, &json!(false), &json!(bar)),
            "{mux:?}"
        );
        let rows: Vec<String> = live
            .panes("#{pane_id}|#{@agent-role}|#{@agent-id}|#{pane_width}|#{pane_pid}")
            .into_iter()
            .filter(|r| r.split('|').nth(1) == Some("sidebar"))
            .collect();
        assert_eq!(rows.len(), 1, "{mux:?} {rows:?}");
        let fields: Vec<&str> = rows[0].split('|').collect();
        let width = live.bar_width(&bar);
        assert_eq!(
            fields[..4],
            [bar.as_str(), "sidebar", "", &width],
            "{mux:?}"
        );
        let bar_pid: u32 = fields[4].parse().unwrap();
        let mut screen = String::new();
        let drawn = eventually(Duration::from_secs(30), || {
            screen = live.call(&["capture-pane", "-p", "-t", &bar]);
            bar_ready(&screen)
        });
        assert!(drawn, "{mux:?}: the bar shows:\n{screen}");
        let (code, list) = live.run(&["list"]);
        assert_eq!(
            (code, names(&list)),
            (0, vec!["w1".to_string(), "w2".into()]),
            "{mux:?}"
        );
        let (code, close) = live.run(&["sidebar", "-Close"]);
        assert_eq!(
            (code, &close["closed"], &close["pane"]),
            (0, &json!(true), &json!(bar)),
            "{mux:?}"
        );
        live.wait_gone(&bar);
        assert!(
            eventually(Duration::from_secs(10), || !support::alive(bar_pid)),
            "{mux:?}: watch outlived its pane"
        );
    }

    // S1: from the lower-left pane of a 3-pane window (neither rightmost nor full height), sidebar opens the bar at
    // the window's right edge, full height, `bar_width` columns, and the active pane is unchanged. With its watch
    // ended (so no tick can repair anything), a spawn by `placement h` and by `placement v` from that pane puts the
    // agent left of the bar, and the bar stays rightmost and full height; the ended bar is no longer provable, so
    // the spawn reports it unproven and opens no second bar.
    fn bar_opens_at_right_edge_from_any_pane(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let root = live.server.pane().to_string();
        let right = live.split(&root, &["-h"]);
        let lower = live.split(&root, &["-v"]);
        let active = live.active(&root);
        let (code, open) = live.run_in(&lower, &["sidebar"]);
        assert_eq!((code, &open["opened"]), (0, &json!(true)), "{mux:?} {open}");
        let bar = s(&open["pane"]).to_string();
        // The watch's first tick runs before the bar is tagged, so it repairs nothing; ended now, it never will.
        live.freeze(&bar);
        assert_eq!(live.bar_place(&bar), live.in_place(&bar), "{mux:?}");
        assert_eq!(live.active(&root), active, "{mux:?}: opening moved focus");
        assert_eq!(live.field(&right, "#{pane_at_right}"), "0", "{mux:?}");
        for (n, placement) in ["h", "v"].into_iter().enumerate() {
            let (code, set) = live.run_in(&lower, &["placement", placement]);
            assert_eq!(code, 0, "{mux:?} {set}");
            let name = format!("w{n}");
            let args = live.spawn_args("claude", &name, &[]);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let (code, spawned) = live.run_in(&lower, &args);
            assert_eq!(code, 0, "{mux:?} {spawned}");
            // Its watch ended, the bar is no longer provable: the spawn names no bar and opens no second one.
            assert_eq!(spawned["sidebar"], Value::Null, "{mux:?} {spawned}");
            assert!(
                s(&spawned["sidebarError"]).starts_with(&format!(
                    "pane {bar} carries the side-bar label but no side bar of this session can be proven"
                )),
                "{mux:?} {spawned}"
            );
            assert_eq!(live.bars().len(), 1, "{mux:?}");
            let agent = s(&spawned["pane"]);
            let edge = |pane: &str, format: &str| -> u32 {
                crate::launch_evidence::checked(
                    &live.agent_dir(s(&spawned["id"])),
                    live.dir.path(),
                    || live.num(pane, format),
                )
                .unwrap_or_else(|e| {
                    // the agent's own hc terminal, as the core sees it, so a missing view names its cause.
                    let address = s(&spawned["hcAddress"]);
                    let mut core = String::new();
                    for args in [
                        vec!["get", address, "--json"],
                        vec!["read", address, "--lines", "40"],
                    ] {
                        let out = live.server.core.as_ref().map(|c| {
                            crate::support::bounded::output(
                                &mut c.command(&args),
                                crate::support::bounded::LIMIT,
                            )
                        });
                        core.push_str(&format!(
                            "
hc {}:
{}",
                            args.join(" "),
                            match out {
                                Some(Ok(o)) => format!(
                                    "{}{}",
                                    String::from_utf8_lossy(&o.stdout),
                                    String::from_utf8_lossy(&o.stderr)
                                ),
                                Some(Err(e)) => format!("unavailable: {e}"),
                                None => "no private core".into(),
                            }
                        ));
                    }
                    panic!("{mux:?} placement {placement}: {e}{core}")
                })
            };
            assert!(
                edge(agent, "#{pane_right}") < edge(&bar, "#{pane_left}"),
                "{mux:?} placement {placement}: the agent is not left of the bar"
            );
            let place = live.bar_place(&bar);
            assert_eq!(
                place[..6],
                *"1|1|1|",
                "{mux:?} placement {placement}: {place}"
            );
        }
    }

    /// `args`, then `select-pane -t <focus>` and the bar's `<at right>|<at top>|<at bottom>`, as one command list,
    /// so no tick of the watch runs in between. Answers the list's output lines; the last is the bar's place.
    fn damage(live: &Live, bar: &str, args: &[&str], focus: &str) -> Vec<String> {
        let mut list: Vec<&str> = args.to_vec();
        list.extend([
            ";",
            "select-pane",
            "-t",
            focus,
            ";",
            "display-message",
            "-p",
            "-t",
            bar,
        ]);
        list.push("#{pane_at_right}|#{pane_at_top}|#{pane_at_bottom}");
        live.call(&list).lines().map(str::to_string).collect()
    }

    // S1 test 3: a pane split right of the bar; after one tick the bar is rightmost, full height, at its width again,
    // the new pane is left of it and the active pane is unchanged.
    fn bar_repaired_after_a_split_right_of_it(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let root = live.server.pane().to_string();
        let bar = live.open_bar();
        let active = live.active(&root);
        let split = [
            "split-window",
            "-d",
            "-h",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            &bar,
            "sh",
        ];
        let lines = damage(&live, &bar, &split, &active);
        assert_eq!(
            lines[1], "0|1|1",
            "{mux:?}: the split left the bar at the right edge"
        );
        live.wait_tick(&bar);
        assert_eq!(live.bar_place(&bar), live.in_place(&bar), "{mux:?}");
        assert_eq!(
            live.active(&root),
            active,
            "{mux:?}: the repair moved focus"
        );
        let edge = |pane: &str, format: &str| -> u32 { live.num(pane, format) };
        assert!(
            edge(&lines[0], "#{pane_right}") < edge(&bar, "#{pane_left}"),
            "{mux:?}: the new pane is not left of the bar"
        );
    }

    // S1 test 4: height-only damage (split -v of the bar), even-vertical, swap-pane and rotate-window each put the
    // bar out of place; each is repaired within a tick, once with the root pane active and once with the bar active,
    // and the active pane after the repair is exactly the one before.
    fn bar_repair_keeps_focus(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let root = live.server.pane().to_string();
        let bar = live.open_bar();
        for name in ["split -v", "even-vertical", "swap-pane", "rotate-window"] {
            for focus in [root.clone(), bar.clone()] {
                let split = [
                    "split-window",
                    "-d",
                    "-v",
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "-t",
                    &bar,
                    "sh",
                ];
                let layout = ["select-layout", "-t", &bar, "even-vertical"];
                let swap = ["swap-pane", "-d", "-s", &bar, "-t", &root];
                let rotate = ["rotate-window", "-t", &bar];
                let args: &[&str] = match name {
                    "split -v" => &split,
                    "even-vertical" => &layout,
                    "swap-pane" => &swap,
                    _ => &rotate,
                };
                let lines = damage(&live, &bar, args, &focus);
                let place = lines.last().unwrap();
                assert_ne!(
                    place, "1|1|1",
                    "{mux:?} {name}: the bar was not put out of place"
                );
                live.wait_tick(&bar);
                assert_eq!(
                    live.bar_place(&bar),
                    live.in_place(&bar),
                    "{mux:?} {name} with {focus} active"
                );
                assert_eq!(
                    live.active(&root),
                    focus,
                    "{mux:?} {name}: the repair moved focus"
                );
                if name == "split -v" {
                    live.call(&["kill-pane", "-t", &lines[0]]);
                }
            }
        }
    }

    // S1 test 5: a bar dragged to 55 columns at the right edge is left so (no move, no resize) until the window's
    // width changes; resize-window -x 300 makes it 60 within one tick (by default 20% of the window, at least
    // 40).
    fn bar_drag_kept_until_the_window_resizes(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let bar = live.open_bar();
        // The bar's first tagged tick sets its width; a drag comes after it.
        live.wait_tick(&bar);
        live.call(&["resize-pane", "-t", &bar, "-x", "55"]);
        assert_eq!(live.bar_place(&bar), "1|1|1|55", "{mux:?}");
        live.wait_tick(&bar);
        assert_eq!(
            live.bar_place(&bar),
            "1|1|1|55",
            "{mux:?}: the drag was undone"
        );
        live.call(&["resize-window", "-t", &bar, "-x", "300"]);
        live.wait_tick(&bar);
        assert_eq!(live.bar_place(&bar), "1|1|1|60", "{mux:?}");
    }

    // by default the bar is 20% of its window, at least 40 columns; a width option changed while it runs
    // (on the server, then the session, the window and the bar's own pane, each overriding the one before; another
    // pane's value never counts) sets its width within one tick, and a value that is no number is its default alone.
    fn bar_width_follows_its_options(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let bar = live.open_bar();
        live.wait_tick(&bar);
        live.call(&["resize-window", "-t", &bar, "-x", "800"]);
        live.wait_tick(&bar);
        assert_eq!(live.bar_place(&bar), "1|1|1|160", "{mux:?}: 20% of 800");
        live.call(&["set-option", "-g", "@mux-agents-sidebar-percent", "10"]);
        live.wait_tick(&bar);
        assert_eq!(live.bar_place(&bar), "1|1|1|80", "{mux:?}: 10% of 800");
        let session = live.field(&bar, "#{session_id}");
        live.call(&[
            "set-option",
            "-t",
            &session,
            "@mux-agents-sidebar-percent",
            "15",
        ]);
        live.wait_tick(&bar);
        assert_eq!(
            live.bar_place(&bar),
            "1|1|1|120",
            "{mux:?}: the session's 15%"
        );
        let window = live.field(&bar, "#{window_id}");
        live.call(&[
            "set-option",
            "-w",
            "-t",
            &window,
            "@mux-agents-sidebar-percent",
            "25",
        ]);
        live.wait_tick(&bar);
        assert_eq!(
            live.bar_place(&bar),
            "1|1|1|200",
            "{mux:?}: the window's 25%"
        );
        let root = live.server.pane().to_string();
        live.call(&[
            "set-option",
            "-p",
            "-t",
            &root,
            "@mux-agents-sidebar-percent",
            "50",
        ]);
        live.call(&[
            "set-option",
            "-p",
            "-t",
            &bar,
            "@mux-agents-sidebar-percent",
            "12",
        ]);
        live.wait_tick(&bar);
        assert_eq!(
            live.bar_place(&bar),
            "1|1|1|96",
            "{mux:?}: the bar's own 12%"
        );
        live.call(&[
            "set-option",
            "-p",
            "-u",
            "-t",
            &bar,
            "@mux-agents-sidebar-percent",
        ]);
        live.call(&[
            "set-option",
            "-w",
            "-u",
            "-t",
            &window,
            "@mux-agents-sidebar-percent",
        ]);
        live.call(&[
            "set-option",
            "-t",
            &session,
            "@mux-agents-sidebar-percent",
            "1|5",
        ]);
        live.call(&["set-option", "-g", "@mux-agents-sidebar-min", "250"]);
        live.wait_tick(&bar);
        assert_eq!(
            live.bar_place(&bar),
            "1|1|1|250",
            "{mux:?}: 20% of 800, at least 250"
        );
        assert_eq!(live.bar_place(&bar), live.in_place(&bar), "{mux:?}");
    }

    // S1 amendment: while a pane is zoomed the bar is left alone, so a window resize does not unzoom it (resize-pane
    // would); once unzoomed, the width for the new window is applied.
    fn zoomed_window_left_alone(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let root = live.server.pane().to_string();
        let bar = live.open_bar();
        live.wait_tick(&bar);
        live.call(&[
            "resize-pane",
            "-Z",
            "-t",
            &root,
            ";",
            "resize-window",
            "-t",
            &bar,
            "-x",
            "300",
        ]);
        live.wait_tick(&bar);
        assert_eq!(
            live.field(&bar, "#{window_zoomed_flag}"),
            "1",
            "{mux:?}: the watch unzoomed the window"
        );
        live.call(&["resize-pane", "-Z", "-t", &root]);
        live.wait_tick(&bar);
        assert_eq!(live.bar_place(&bar), "1|1|1|60", "{mux:?}");
    }

    /// Every pane of the server as `<pane>|<left>|<top>|<width>`, sorted.
    fn geometry(live: &Live) -> Vec<String> {
        let mut rows = live.panes("#{pane_id}|#{pane_left}|#{pane_top}|#{pane_width}");
        rows.sort();
        rows
    }

    // test 2: the bar's pane respawned to anything (a shell naming the old pid and `watch`, or this binary
    // `watch` again) is a new process: not proven, not killed, not reused; -Close refuses and the pane stays.
    fn replaced_bar_is_never_closed(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let bin = env!("CARGO_BIN_EXE_mux-agent");
        let bar = live.open_bar();
        let old = live.field(&bar, "#{pane_pid}");
        let naming = format!("sleep 1000 # {old} watch");
        let commands: [&[&str]; 2] = [&["sh", "-c", &naming], &[bin, "watch"]];
        for command in commands {
            let mut args = vec!["respawn-pane", "-k", "-t", &bar];
            args.extend(command);
            live.call(&args);
            assert_ne!(live.field(&bar, "#{pane_pid}"), old, "{mux:?}");
            let (code, close) = live.run(&["sidebar", "-Close"]);
            assert_eq!(code, 1, "{mux:?} {command:?} {close}");
            assert!(
                s(&close["message"]).starts_with(&format!("pane {bar} carries the side-bar label")),
                "{mux:?} {close}"
            );
            assert!(
                live.panes("#{pane_id}").contains(&bar),
                "{mux:?}: the pane was killed"
            );
            let (code, open) = live.run(&["sidebar"]);
            assert_eq!(code, 1, "{mux:?}: reused or reopened: {open}");
            assert_eq!(
                live.panes("#{pane_id}").len(),
                2,
                "{mux:?}: a second bar opened"
            );
        }
    }

    // test 4: rmux 0.10.0's rotate-window leaves the bar's label on the user's pane. sidebar -Close still closes
    // the bar, proven by its process, and the user's pane lives on.
    fn close_after_rotate_spares_the_users_pane(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let root = live.server.pane().to_string();
        let bar = live.open_bar();
        live.call(&["rotate-window", "-t", &bar]);
        let (code, close) = live.run(&["sidebar", "-Close"]);
        assert_eq!(
            (code, &close["closed"], &close["pane"]),
            (0, &json!(true), &json!(bar)),
            "{mux:?} {close}"
        );
        live.wait_gone(&bar);
        assert!(
            live.panes("#{pane_id}").contains(&root),
            "{mux:?}: the user's pane was closed"
        );
    }

    // test 8: a pane in another window of the session carrying the label without proof (a 0.2.1 bar) refuses
    // -Close, which closes no bar: nothing is killed. a label refuses only a bar of its own window, so sidebar
    // and a spawn's bar open in their own windows.
    fn unproven_label_in_another_window_refuses(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let root = live.server.pane().to_string();
        let session = format!("{}:", live.field(&root, "#{session_id}"));
        let other = live
            .call(&[
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                &session,
                "sh",
            ])
            .trim()
            .to_string();
        live.call(&["set-option", "-p", "-t", &other, "@agent-role", "sidebar"]);
        let refused = format!("pane {other} carries the side-bar label");
        let (code, reply) = live.run(&["sidebar", "-Close"]);
        assert_eq!(code, 1, "{mux:?} {reply}");
        assert!(
            s(&reply["message"]).starts_with(&refused),
            "{mux:?} {reply}"
        );
        let (code, open) = live.run(&["sidebar"]);
        assert_eq!((code, &open["opened"]), (0, &json!(true)), "{mux:?} {open}");
        let (code, spawned) = live.try_spawn("w1", &[]);
        assert_eq!(code, 0, "{mux:?} {spawned}");
        assert!(spawned.get("sidebarError").is_none(), "{mux:?} {spawned}");
        assert_ne!(spawned["sidebar"], Value::Null, "{mux:?} {spawned}");
        assert!(
            live.panes("#{pane_id}").contains(&other),
            "{mux:?}: the pane was killed"
        );
        let labelled = live
            .panes("#{@agent-role}")
            .into_iter()
            .filter(|r| r == "sidebar")
            .count();
        assert_eq!(
            labelled, 3,
            "{mux:?}: the label and one bar in each of the two windows"
        );
    }

    // QA on S1: `mux-agent watch` run by hand in the shell pane beside the real bar only draws: over several ticks no
    // pane moves or changes width, and the real bar stays at the right edge.
    fn manual_watch_moves_nothing(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let root = live.server.pane().to_string();
        let bar = live.open_bar();
        live.wait_tick(&bar);
        let before = geometry(&live);
        manual_watch(&mut live, &root);
        // Sampled every 100 ms: a manual keeper and the bar's own would undo each other between two ticks.
        stays(&live, &before);
        assert_eq!(live.bar_place(&bar), live.in_place(&bar), "{mux:?}");
        live.call(&["send-keys", "-t", &root, "C-c"]);
    }

    // QA on S1 (c): with no bar at all, `mux-agent watch` run by hand in a pane that is not at the right edge only
    // draws: nothing moves it there.
    fn manual_watch_without_a_bar_moves_nothing(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let root = live.server.pane().to_string();
        live.split(&root, &["-h"]);
        let before = geometry(&live);
        manual_watch(&mut live, &root);
        stays(&live, &before);
        live.call(&["send-keys", "-t", &root, "C-c"]);
    }

    // T9 test 5, live: a bar replaced between a passing proof and the kill survives the exact pid-bound conditional
    // kill the code sends (`kill_command`), its pane read again shows the replacement, and -Close then refuses it.
    fn replaced_between_proof_and_kill_survives(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let bar = live.open_bar();
        let (code, open) = live.run(&["sidebar"]);
        assert_eq!(
            (code, &open["opened"], &open["pane"]),
            (0, &json!(false), &json!(bar)),
            "{mux:?}: the bar is not proven: {open}"
        );
        let pid: u32 = live.num(&bar, "#{pane_pid}");
        live.call(&["respawn-pane", "-k", "-t", &bar, "sleep", "1000"]);
        let new = live.field(&bar, "#{pane_pid}");
        assert_ne!(new, pid.to_string(), "{mux:?}");
        let kill = mux_agent::sidebar::kill_command(&bar, pid);
        let kill: Vec<&str> = kill.iter().map(String::as_str).collect();
        live.call(&kill);
        assert_eq!(
            live.field(&bar, mux_agent::sidebar::GONE_FORMAT),
            format!("{bar}|{new}"),
            "{mux:?}: the replacement was killed"
        );
        let (code, close) = live.run(&["sidebar", "-Close"]);
        assert_eq!(code, 1, "{mux:?} {close}");
        assert!(
            s(&close["message"]).starts_with(&format!("pane {bar} carries the side-bar label")),
            "{mux:?} {close}"
        );
        assert!(live.panes("#{pane_id}").contains(&bar), "{mux:?}");
    }

    // T9-QA round 2, live: a bar replaced between a passing proof and open's resize keeps its width under the exact
    // pid-bound resize the code sends (`resize_command`); bound to the replacement's own pid, the same command does
    // resize it, so the guard is the pid and nothing else.
    fn replaced_bar_is_not_resized(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let bar = live.open_bar();
        let (code, open) = live.run(&["sidebar"]);
        assert_eq!(
            (code, &open["opened"], &open["pane"]),
            (0, &json!(false), &json!(bar)),
            "{mux:?}: the bar is not proven: {open}"
        );
        let pid: u32 = live.num(&bar, "#{pane_pid}");
        live.call(&["respawn-pane", "-k", "-t", &bar, "sleep", "1000"]);
        let new: u32 = live.num(&bar, "#{pane_pid}");
        assert_ne!(new, pid, "{mux:?}");
        live.call(&["resize-pane", "-t", &bar, "-x", "60"]);
        let width = live.field(&bar, "#{pane_width}");
        assert_ne!(
            width, "40",
            "{mux:?}: the replacement could not be resized away from 40"
        );
        let resize = |pid: u32| {
            let args = mux_agent::sidebar::resize_command(&bar, pid, "40");
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            live.call(&args);
        };
        resize(pid);
        assert_eq!(
            live.field(&bar, "#{pane_width}"),
            width,
            "{mux:?}: the replacement was resized"
        );
        resize(new);
        assert_eq!(live.field(&bar, "#{pane_width}"), "40", "{mux:?}");
    }

    // T9-QA round 3, live: a bar replaced between a passing proof and open's label stays unlabelled under the exact
    // pid-bound label the code sends (`label_command`); bound to the replacement's own pid, it does label it.
    fn replaced_bar_is_not_labelled(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let bar = live.open_bar();
        let (code, open) = live.run(&["sidebar"]);
        assert_eq!(
            (code, &open["opened"], &open["pane"]),
            (0, &json!(false), &json!(bar)),
            "{mux:?}: the bar is not proven: {open}"
        );
        let pid: u32 = live.num(&bar, "#{pane_pid}");
        live.call(&["respawn-pane", "-k", "-t", &bar, "sleep", "1000"]);
        let new: u32 = live.num(&bar, "#{pane_pid}");
        assert_ne!(new, pid, "{mux:?}");
        live.call(&["set-option", "-p", "-u", "-t", &bar, "@agent-role"]);
        assert_eq!(live.field(&bar, "#{@agent-role}"), "", "{mux:?}");
        let label = |pid: u32| {
            let args = mux_agent::sidebar::label_command(&bar, pid);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            live.call(&args);
        };
        label(pid);
        assert_eq!(
            live.field(&bar, "#{@agent-role}"),
            "",
            "{mux:?}: the replacement was labelled"
        );
        label(new);
        assert_eq!(live.field(&bar, "#{@agent-role}"), "sidebar", "{mux:?}");
    }

    // T9-QA 2, live: a record forged for a watch run by hand, naming its own pane and its own pid with a stale start
    // or its real one, proves nothing: that pane runs the shell, not the watch. The manual watch moves nothing.
    fn forged_record_does_not_make_a_manual_watch_keep(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let root = live.server.pane().to_string();
        live.split(&root, &["-h"]);
        let before = geometry(&live);
        manual_watch(&mut live, &root);
        let shell = live.field(&root, "#{pane_pid}");
        let found = crate::support::bounded::output(
            Command::new("pgrep").args(["-P", &shell, "-f", "watch"]),
            crate::support::bounded::LIMIT,
        )
        .unwrap();
        let watch: u32 = String::from_utf8_lossy(&found.stdout)
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{mux:?}: no manual watch under {shell}: {found:?}"));
        let start = mux_agent::proc::host()
            .row(watch)
            .and_then(|row| row.start)
            .unwrap();
        // the record a watch reads is its window's.
        let window = live.field(&root, "#{window_id}");
        for stamp in [1, start] {
            let record = format!("{root} {watch} {stamp}");
            live.call(&[
                "set-option",
                "-w",
                "-t",
                &window,
                "@mux-agents-sidebar-id",
                &record,
            ]);
            stays(&live, &before);
        }
        live.call(&["send-keys", "-t", &root, "C-c"]);
    }

    /// Runs `mux-agent watch` by hand in `pane`'s shell and waits until it has drawn.
    fn manual_watch(live: &mut Live, pane: &str) {
        let line = format!(
            "MUX_AGENTS_STATE={} {} watch",
            support::quote(&live.state_text()),
            support::quote(env!("CARGO_BIN_EXE_mux-agent"))
        );
        live.server.send_keys_to(pane, &line);
        live.server.wait("the manual watch to draw", || {
            live.call(&["capture-pane", "-p", "-t", pane])
                .lines()
                .any(|l| l.trim_start().starts_with("mux-agents") && l.contains(" running"))
                .then_some(())
        });
    }

    /// Every pane keeps `before`'s geometry for 7 s (three ticks and more), read every 100 ms.
    fn stays(live: &Live, before: &[String]) {
        let end = Instant::now() + Duration::from_secs(7);
        while Instant::now() < end {
            assert_eq!(
                geometry(live),
                before,
                "{:?}: the manual watch moved a pane",
                live.mux()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    // list -All: the reply's and each row's fields, in order; running rows first, then ended ones.
    fn list_all_json_shape(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let root = live.server.pane().to_string();
        let w1 = live.spawn("w1", &[]);
        let w2 = live.spawn("w2", &[]);
        // o2 call 35: both are ready, so w2's stop ends an ordinary member.
        live.ready(&w1);
        live.ready(&w2);
        let (code, stop) = live.run(&["stop", "w2"]);
        assert_eq!(code, 0, "{mux:?} {stop}");
        live.wait_gone(s(&w2["pane"]));
        let (code, all) = live.run(&["list", "-All"]);
        assert_eq!(code, 0, "{mux:?} {all}");
        let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        assert_eq!(
            keys(&all),
            ["ok", "verb", "running", "endedTotal", "agents"],
            "{mux:?}"
        );
        assert_eq!(
            (
                &all["ok"],
                &all["verb"],
                &all["running"],
                &all["endedTotal"]
            ),
            (&json!(true), &json!("list"), &json!(1), &json!(1)),
            "{mux:?}"
        );
        let rows = all["agents"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "{mux:?} {all}");
        const ROW: [&str; 16] = [
            "name",
            "client",
            "pane",
            "window",
            "context",
            "parent",
            "sessionId",
            "directory",
            "createdAt",
            "state",
            "doing",
            "doingFrom",
            "age",
            "unread",
            "model",
            "color",
        ];
        // w2's stop notice is unread in the root's mailbox, from w2's pane.
        for (row, (w, state, unread)) in rows.iter().zip([(&w1, "running", 0), (&w2, "stopped", 1)])
        {
            // An ended row ends with its exitCode: null for a stop.
            let mut want_keys = ROW.to_vec();
            // A live row then carries its activity: no report yet, so working since nothing.
            if state == "running" {
                want_keys.extend(["activity", "since", "changedAt"]);
                assert_eq!(
                    (&row["activity"], &row["since"], &row["changedAt"]),
                    (&json!("working"), &Value::Null, &Value::Null),
                    "{mux:?} {row}"
                );
            }
            if state != "running" {
                want_keys.push("exitCode");
                assert_eq!(row["exitCode"], Value::Null, "{mux:?} {row}");
                // `list -All` offers the command that starts an ended member again.
                want_keys.push("resume");
                assert_eq!(
                    row["resume"],
                    json!(format!("mux-agent resume {}", s(&w["id"]))),
                    "{mux:?} {row}"
                );
            }
            want_keys.extend(["life", "view", "hcAddress"]);
            assert_eq!(keys(row), want_keys, "{mux:?}");
            let want = json!({
                "name": w["name"], "client": "claude", "pane": if state=="running" {w["pane"].clone()} else {Value::Null}, "window": if state=="running" {w["window"].clone()} else {Value::Null}, "context": "brief",
                "parent": root, "sessionId": w["sessionId"], "directory": live.path("work"), "state": state,
                "life": if state=="running" {"running"} else {"ended"},
                "view": if state=="running" {"attached"} else {"detached"}, "hcAddress": w["hcAddress"],
                "doing": "say hi", "doingFrom": "task", "unread": unread, "model": null,
            });
            for (field, value) in want.as_object().unwrap() {
                assert_eq!(&row[field], value, "{mux:?} {row}");
            }
            assert!(
                row["age"].is_u64() && row["createdAt"].is_string(),
                "{mux:?} {row}"
            );
        }
        let (code, plain) = live.run(&["list"]);
        assert_eq!(
            (code, names(&plain), &plain["running"]),
            (0, vec!["w1".to_string()], &json!(1)),
            "{mux:?}"
        );
    }

    // spawn -Model: the reply names it, record.json keeps it, the client gets --model before its prompt and list shows
    // it; without -Model the model is inherited and listed as null.
    fn spawn_model_recorded_and_listed(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let w1 = live.spawn("w1", &["-Model", "sonnet"]);
        assert_eq!(w1["model"], "sonnet", "{mux:?} {w1}");
        let record = json_file(&live.agent_dir(s(&w1["id"])).join("record.json"));
        assert_eq!(record["model"], "sonnet", "{mux:?} {record}");
        let client = live.server.client_by_session(s(&w1["sessionId"]));
        let argv = strings(&client["argv"]);
        assert_eq!(argv.len(), 8, "{mux:?} {argv:?}");
        assert_eq!(
            argv[3..7],
            ["--session-id", s(&w1["sessionId"]), "--model", "sonnet"],
            "{mux:?}"
        );
        let w2 = live.spawn("w2", &[]);
        assert_eq!(w2["model"], "inherited", "{mux:?} {w2}");
        let record = json_file(&live.agent_dir(s(&w2["id"])).join("record.json"));
        assert!(record.get("model").is_none(), "{mux:?} {record}");
        let (code, list) = live.run(&["list"]);
        assert_eq!(code, 0, "{mux:?} {list}");
        let models: Vec<(&str, &Value)> = list["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| (s(&row["name"]), &row["model"]))
            .collect();
        assert_eq!(
            models,
            [("w1", &json!("sonnet")), ("w2", &Value::Null)],
            "{mux:?}"
        );
    }

    // An agent caller forks only with bypass evidence: the transcript's last mode and the pane's mode line.
    fn agent_caller_forks_with_bypass_evidence(mux: Mux) {
        let Some(mut live) = Live::agent(mux) else {
            return;
        };
        let w1 = live.spawn("w1", &[]);
        assert_eq!(
            (s(&w1["context"]), s(&w1["reason"])),
            ("fork", "same-client"),
            "{mux:?}"
        );
        let transcript = live
            .dir
            .join(format!("home/.claude/projects/it/{SID}.jsonl"));
        append(
            &transcript,
            "{\"type\":\"permission-mode\",\"permissionMode\":\"default\"}\n",
        );
        let (code, refused) = live.try_spawn("w2", &[]);
        assert_eq!(
            (code, &refused["check"]),
            (2, &json!("policy")),
            "{mux:?} {refused}"
        );
        assert_eq!(live.tagged().len(), 1, "{mux:?}");
        append(
            &transcript,
            "{\"type\":\"permission-mode\",\"permissionMode\":\"bypassPermissions\"}\n",
        );
        let w2 = live.spawn("w2", &[]);
        assert_eq!(s(&w2["context"]), "fork", "{mux:?}");
        // The mode line scrolled out of the screen's last 8 rows (a Shift+Tab the transcript has not recorded yet).
        let pid = live.root_pid;
        for n in 1..=8 {
            live.server.send_keys(&format!("noise {n}"));
        }
        live.server
            .wait_line(&format!("input.{pid}.log"), "noise 8");
        let (code, refused) = live.try_spawn("w3", &[]);
        assert_eq!(
            (code, &refused["check"]),
            (2, &json!("policy")),
            "{mux:?} {refused}"
        );
        assert_eq!(live.tagged().len(), 2, "{mux:?}");
    }

    // Two spawns from one pane with different state roots, at once, beside 5 agents: the cap holds across roots.
    fn cap_serializes_across_state_roots(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        for n in 1..=5 {
            live.spawn(&format!("w{n}"), &[]);
        }
        let other = live.dir.join("other");
        std::fs::create_dir_all(other.join("home")).unwrap();
        std::fs::create_dir_all(other.join("xdg")).unwrap();
        let state_b = live.path("state-b");
        let (xdg, home) = (
            other.join("xdg").display().to_string(),
            other.join("home").display().to_string(),
        );
        let (a, b) = (
            live.spawn_args("claude", "w6", &[]),
            live.spawn_args("claude", "w7", &[]),
        );
        let a: Vec<&str> = a.iter().map(String::as_str).collect();
        let b: Vec<&str> = b.iter().map(String::as_str).collect();
        let va = [("MUX_AGENTS_STATE", state_b.as_str())];
        let vb = [("XDG_STATE_HOME", xdg.as_str()), ("HOME", home.as_str())];
        let (none, unset): (&[&str], &[&str]) = (&[], &["MUX_AGENTS_STATE"]);
        let jobs = live
            .server
            .bg(&[(&a[..], &va[..], none), (&b[..], &vb[..], unset)]);
        let mut results: Vec<(i32, Value)> = jobs
            .iter()
            .map(|j| live.server.job_wait(*j, LONG))
            .collect();
        results.sort_by_key(|(code, _)| *code);
        assert_eq!((results[0].0, results[1].0), (0, 2), "{mux:?} {results:?}");
        assert_eq!(results[1].1["check"], "cap", "{mux:?} {results:?}");
        assert_eq!(live.tagged().len(), 6, "{mux:?}");
    }

    // Two `-Name w` spawns at once across state roots: one wins, the other is refused 2 `name`; one pane.
    fn same_name_two_state_roots_one_wins(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let (state, state_b) = (live.state_text(), live.path("state-b"));
        let args = live.spawn_args("claude", "w", &[]);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let va = [("MUX_AGENTS_STATE", state.as_str())];
        let vb = [("MUX_AGENTS_STATE", state_b.as_str())];
        let none: &[&str] = &[];
        let jobs = live
            .server
            .bg(&[(&args[..], &va[..], none), (&args[..], &vb[..], none)]);
        let mut results: Vec<(i32, Value)> = jobs
            .iter()
            .map(|j| live.server.job_wait(*j, LONG))
            .collect();
        results.sort_by_key(|(code, _)| *code);
        assert_eq!((results[0].0, results[1].0), (0, 2), "{mux:?} {results:?}");
        assert_eq!(results[1].1["check"], "name", "{mux:?} {results:?}");
        let tagged = live.tagged();
        assert_eq!(tagged.len(), 1, "{mux:?} {tagged:?}");
        assert!(tagged[0].ends_with("|w"), "{mux:?} {tagged:?}");
    }

    // A spawn lock held elsewhere for 15 s refuses 2 `lock`; released, the next spawn locks at once. The key comes
    // from the folder that holds server.json.
    fn lock_held_15s_refuses_lock(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        live.spawn("w1", &[]);
        let folders: Vec<PathBuf> = std::fs::read_dir(live.state())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("server.json").is_file())
            .collect();
        assert_eq!(folders.len(), 1, "{mux:?} {folders:?}");
        let key = folders[0]
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(key, live.server.key(), "{mux:?}");
        let session = live.field(live.server.pane(), "#{session_id}");
        let path = support::lock_file(
            &support::lock_root().unwrap(),
            &format!("spawn|{key}|{session}"),
        );
        // The spawn above made it; the harness removes it with the server.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        held.lock().unwrap();
        let at = Instant::now();
        let (code, refused) = live.try_spawn("w2", &[]);
        let took = at.elapsed();
        assert_eq!(code, 2, "{mux:?} {refused}");
        assert_eq!(
            (&refused["check"], &refused["message"]),
            (
                &json!("lock"),
                &json!("timed out waiting for the agent lock")
            ),
            "{mux:?}"
        );
        assert!(
            took >= Duration::from_secs(15) && took < Duration::from_secs(30),
            "{mux:?}: {took:?}"
        );
        held.unlock().unwrap();
        drop(held);
        let at = Instant::now();
        live.spawn("w3", &[]);
        assert!(
            at.elapsed() < Duration::from_secs(5),
            "{mux:?}: {:?}",
            at.elapsed()
        );
        assert_eq!(live.tagged().len(), 2, "{mux:?}");
    }

    // Two sidebar calls at once in one session: one opens the bar, the other answers it; one bar pane.
    fn two_sidebars_one_session_one_split(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let sidebar: &[&str] = &["sidebar"];
        let jobs = live.bg(&[sidebar, sidebar]);
        let results: Vec<(i32, Value)> = jobs
            .iter()
            .map(|j| live.server.job_wait(*j, LONG))
            .collect();
        assert!(
            results.iter().all(|(code, _)| *code == 0),
            "{mux:?} {results:?}"
        );
        let mut opened: Vec<bool> = results
            .iter()
            .map(|(_, r)| r["opened"].as_bool().unwrap())
            .collect();
        opened.sort();
        assert_eq!(opened, [false, true], "{mux:?} {results:?}");
        assert_eq!(results[0].1["pane"], results[1].1["pane"], "{mux:?}");
        let bars = live
            .panes("#{@agent-role}")
            .iter()
            .filter(|r| *r == "sidebar")
            .count();
        assert_eq!(bars, 1, "{mux:?}");
        let (code, close) = live.run(&["sidebar", "-Close"]);
        assert_eq!(
            (code, &close["closed"]),
            (0, &json!(true)),
            "{mux:?} {close}"
        );
    }

    // Through every verb, mux-agent starts only the multiplexer, core CLI, and clients.
    // Core launcher argv and the mux-owned attachment command are checked separately from the spawn audit.
    fn children_are_only_mux_and_client(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let w1 = live.spawn("w1", &[]);
        let (code, w2) = live.try_spawn_client("copilot", "w2", &[]);
        assert_eq!(code, 0, "{mux:?} {w2}");
        live.worker_pid(&w1);
        live.worker_pid(&w2);
        for w in [&w1, &w2] {
            let r = json_file(&live.agent_dir(s(&w["id"])).join("record.json"));
            assert_eq!(
                core(&live, &["get", s(&w["hcAddress"]), "--json"])["argv"],
                r["launcherArgv"]
            );
            assert_eq!(
                live.field(s(&w["pane"]), "#{pane_start_command}"),
                s(&live.view_of(s(&w["id"]))["startCommand"])
            );
        }
        std::fs::write(live.dir.join("to-w1.txt"), "hello w1").unwrap();
        let body = live.path("to-w1.txt");
        for args in [
            &["list", "-All"][..],
            &["send", "w1", "-BodyFile", &body],
            &["read"],
            &["read", "w1", "-Lines", "5"],
            &["focus", "w1"],
            &["sidebar"],
            &["sidebar", "-Close"],
            &["stop", "w1"],
            &["stop", "w2"],
        ] {
            let (code, reply) = live.run(args);
            assert_eq!(code, 0, "{mux:?} {args:?}: {reply}");
        }
        live.wait_gone(s(&w1["pane"]));
        live.wait_gone(s(&w2["pane"]));
        let spawned = std::fs::read_to_string(live.dir.join("spawned.log")).unwrap();
        let names: BTreeSet<String> = spawned
            .lines()
            .map(|l| {
                Path::new(l)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        let command = live.server.core.as_ref().unwrap().command(&[]);
        let hc_name = Path::new(command.get_program())
            .file_name()
            .unwrap()
            .to_string_lossy();
        let allowed = [
            "tmux",
            "rmux",
            "claude",
            "copilot",
            "fake-client",
            hc_name.as_ref(),
        ];
        assert!(names.contains(&hc_name.to_string()));
        assert!(
            names.iter().all(|n| allowed.contains(&n.as_str())),
            "{mux:?}: {names:?}"
        );
        for need in [mux_name(mux), "claude", "copilot"] {
            assert!(names.contains(need), "{mux:?}: no {need} in {names:?}");
        }
    }

    /// mux-agent built in release with test-panic, in its own target dir (cargo's lock is per target dir).
    fn release_bin() -> PathBuf {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let target = Path::new(manifest).join("target/live-release");
        let out = crate::support::bounded::output(
            Command::new(env!("CARGO"))
                .current_dir(manifest)
                .args([
                    "build",
                    "--release",
                    "--features",
                    "test-panic",
                    "--bin",
                    "mux-agent",
                    "--target-dir",
                ])
                .arg(&target),
            crate::support::bounded::HELD,
        )
        .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        target.join("release/mux-agent")
    }

    // A release binary that panics holding the spawn lock answers 1 `internal` as JSON with nothing else on either
    // stream, and leaves the lock free: the next spawn takes it at once.
    fn panic_in_lock_frees_lock(mux: Mux) {
        let bin = release_bin();
        let bin = bin.to_str().unwrap();
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let state = live.state_text();
        let (w1, w2) = (
            live.spawn_args("claude", "w1", &[]),
            live.spawn_args("claude", "w2", &[]),
        );
        let w1: Vec<&str> = w1.iter().map(String::as_str).collect();
        let w2: Vec<&str> = w2.iter().map(String::as_str).collect();
        let plain = [("MUX_AGENTS_STATE", state.as_str())];
        let (code, out, err) = live.server.run_raw(bin, &w1, &plain, &[]);
        assert_eq!(code, 0, "{mux:?} {out} {err}");
        let panicking = [
            ("MUX_AGENTS_STATE", state.as_str()),
            ("MUX_AGENTS_TEST_PANIC", "lock"),
            ("RUST_BACKTRACE", "1"),
        ];
        let (code, out, err) = live.server.run_raw(bin, &w2, &panicking, &[]);
        let internal = "{\"ok\":false,\"verb\":\"spawn\",\"code\":1,\"check\":\"internal\",\"message\":\"unexpected error (panic)\"}\n";
        assert_eq!(
            (code, out.as_str(), err.as_str()),
            (1, internal, ""),
            "{mux:?}"
        );
        assert!(
            !out.contains("PANIC-PAYLOAD") && !err.contains("PANIC-PAYLOAD"),
            "{mux:?}"
        );
        assert_eq!(live.tagged().len(), 1, "{mux:?}");
        let at = Instant::now();
        let (code, out, err) = live.server.run_raw(bin, &w2, &plain, &[]);
        let took = at.elapsed();
        assert_eq!(code, 0, "{mux:?} {out} {err}");
        assert!(
            took < Duration::from_secs(5),
            "{mux:?}: the next spawn waited {took:?}"
        );
        assert_eq!(live.tagged().len(), 2, "{mux:?}");
    }

    // The placement is the session's: set from the root pane, another pane of that session reads it and spawns
    // there by it; another session, and a new session after the first is gone, read the default.
    fn placement_is_per_session(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let state = live.state_text();
        let vars = [("MUX_AGENTS_STATE", state.as_str())];
        let (code, unset) = live.run(&["placement"]);
        assert_eq!(
            (code, &unset["placement"], &unset["source"]),
            (0, &json!("tab"), &json!("default")),
            "{mux:?} {unset}"
        );
        let a = live.new_session("A");
        let (code, set) = live.server.run_in(&a, &["placement", "h"], &vars);
        assert_eq!(
            (code, &set["placement"], &set["source"]),
            (0, &json!("h"), &json!("session")),
            "{mux:?} {set}"
        );
        let a2 = live
            .call(&[
                "split-window",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                &a,
                "sh",
            ])
            .trim()
            .to_string();
        let (code, seen) = live.server.run_in(&a2, &["placement"], &vars);
        assert_eq!(
            (code, &seen["placement"], &seen["source"]),
            (0, &json!("h"), &json!("session")),
            "{mux:?} {seen}"
        );
        let (code, root) = live.run(&["placement"]);
        assert_eq!(
            (code, &root["placement"], &root["source"]),
            (0, &json!("tab"), &json!("default")),
            "{mux:?} {root}"
        );
        live.call(&["kill-session", "-t", "A"]);
        let c = live.new_session("C");
        let (code, fresh) = live.server.run_in(&c, &["placement"], &vars);
        assert_eq!(
            (code, &fresh["placement"], &fresh["source"]),
            (0, &json!("tab"), &json!("default")),
            "{mux:?} {fresh}"
        );
        // Nothing of it is kept on disk.
        assert!(!live.state().exists(), "{mux:?}");
    }

    // each view's window keeps its own bar. The first spawn (a tab) opens one in its new window; a split
    // beside the caller opens one in the caller's window, and a later split there answers that same bar; a tab gets
    // its own. After sidebar -Close every bar is gone and a spawn opens none; sidebar opens one in the caller's window
    // again, and each view window gets its own back.
    fn spawn_bar_once_reused_and_close_stays_closed(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let root = live.server.pane().to_string();
        let root_window = live.field(&root, "#{window_id}");
        let w1 = live.spawn("w1", &[]);
        assert_eq!(
            (&w1["placement"], &w1["placementSource"]),
            (&json!("tab"), &json!("default")),
            "{mux:?} {w1}"
        );
        assert_ne!(w1["window"], root_window.as_str(), "{mux:?}");
        let bar1 = s(&w1["sidebar"]).to_string();
        assert!(w1.get("sidebarError").is_none(), "{mux:?} {w1}");
        let width = live.bar_width(&bar1);
        assert_eq!(
            live.bars(),
            [format!("{bar1}|{}|{width}", s(&w1["window"]))],
            "{mux:?}"
        );
        let (code, set) = live.run(&["placement", "h"]);
        assert_eq!(code, 0, "{mux:?} {set}");
        let w2 = live.spawn("w2", &[]);
        assert_eq!(
            (&w2["placement"], &w2["placementSource"]),
            (&json!("h"), &json!("session")),
            "{mux:?} {w2}"
        );
        assert_eq!(w2["window"], root_window.as_str(), "{mux:?}");
        let bar2 = s(&w2["sidebar"]).to_string();
        assert_ne!(bar2, bar1, "{mux:?}");
        assert_eq!(live.field(&bar2, "#{window_id}"), root_window, "{mux:?}");
        // Split beside the caller: to its right, not at the window's left edge.
        assert_ne!(live.field(s(&w2["pane"]), "#{pane_left}"), "0", "{mux:?}");
        let w2b = live.spawn("w2b", &[]);
        assert_eq!(w2b["sidebar"], bar2.as_str(), "{mux:?} {w2b}");
        let w3 = live.spawn("w3", &["-Tab"]);
        assert_eq!(
            (&w3["placement"], &w3["placementSource"]),
            (&json!("tab"), &json!("flag")),
            "{mux:?} {w3}"
        );
        assert_ne!(w3["window"], root_window.as_str(), "{mux:?}");
        let bar3 = s(&w3["sidebar"]).to_string();
        assert!(bar3 != bar1 && bar3 != bar2, "{mux:?} {w3}");
        assert_eq!(
            live.field(&bar3, "#{window_id}"),
            s(&w3["window"]),
            "{mux:?}"
        );
        assert_eq!(live.bars().len(), 3, "{mux:?} {:?}", live.bars());
        let close = live.close_bar();
        let mut closed: Vec<&str> = close["panes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap())
            .collect();
        closed.sort();
        let mut want = [bar1.as_str(), bar2.as_str(), bar3.as_str()];
        want.sort();
        assert_eq!(closed, want, "{mux:?} {close}");
        assert!(live.bars().is_empty(), "{mux:?} {:?}", live.bars());
        let w4 = live.spawn("w4", &["-Tab"]);
        assert_eq!(w4["sidebar"], Value::Null, "{mux:?} {w4}");
        assert!(w4.get("sidebarError").is_none(), "{mux:?} {w4}");
        assert!(live.bars().is_empty(), "{mux:?} {:?}", live.bars());
        let (code, reopened) = live.run(&["sidebar"]);
        assert_eq!(
            (code, &reopened["opened"], &reopened["window"]),
            (0, &json!(true), &json!(root_window)),
            "{mux:?} {reopened}"
        );
        // -Text names the bar of the new view's window on its own last line.
        let state = live.state_text();
        let args = live.spawn_args("claude", "w5", &["-Tab", "-Text"]);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let (code, out, err) = live.server.run_raw(
            env!("CARGO_BIN_EXE_mux-agent"),
            &args,
            &[("MUX_AGENTS_STATE", state.as_str())],
            &[],
        );
        assert_eq!(code, 0, "{mux:?} {out} {err}");
        let (_, list) = live.run(&["list"]);
        let w5 = list["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "w5")
            .unwrap();
        let bar5 = live
            .bars()
            .into_iter()
            .find(|b| b.split('|').nth(1) == w5["window"].as_str())
            .map(|b| b.split('|').next().unwrap().to_string())
            .unwrap_or_else(|| panic!("{mux:?}: no bar in w5's window {w5}"));
        let tail: Vec<&str> = out.lines().rev().take(6).collect();
        assert_eq!(
            tail,
            [
                format!("sidebar {bar5}"),
                format!("hcAddress: {}", s(&w5["hcAddress"])),
                "view: attached".into(),
                "life: running".into(),
                "placementSource: flag".into(),
                "placement: tab".into()
            ]
        );
        // The caller's bar gives each other view window its own on its next ticks.
        assert!(
            eventually(LONG, || one_bar_per_view_window(&mut live)),
            "{mux:?} {:?}",
            live.bars()
        );
    }

    /// Whether each window that shows a live agent's view holds a bar, and no window holds two.
    fn one_bar_per_view_window(live: &mut Live) -> bool {
        let (_, list) = live.run(&["list"]);
        let windows: Vec<String> = live
            .bars()
            .iter()
            .map(|b| b.split('|').nth(1).unwrap().to_string())
            .collect();
        let mut distinct = windows.clone();
        distinct.sort();
        distinct.dedup();
        distinct.len() == windows.len()
            && list["agents"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["window"].as_str())
                .all(|w| windows.iter().any(|b| b == w))
    }

    // spawns at once, each a tab, open one bar in each one's window; a spawn racing sidebar -Close never leaves
    // a bar once the close has returned, and a later spawn opens none.
    fn spawn_races_keep_one_bar_per_window(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let (a, b) = (
            live.spawn_args("claude", "w1", &["-Tab"]),
            live.spawn_args("claude", "w2", &["-Tab"]),
        );
        let a: Vec<&str> = a.iter().map(String::as_str).collect();
        let b: Vec<&str> = b.iter().map(String::as_str).collect();
        let jobs = live.bg(&[&a, &b]);
        let results: Vec<(i32, Value)> = jobs
            .iter()
            .map(|j| live.server.job_wait(*j, LONG))
            .collect();
        assert!(
            results.iter().all(|(code, _)| *code == 0),
            "{mux:?} {results:?}"
        );
        assert_ne!(results[0].1["sidebar"], results[1].1["sidebar"], "{mux:?}");
        for (_, r) in &results {
            assert_eq!(
                live.field(s(&r["sidebar"]), "#{window_id}"),
                s(&r["window"]),
                "{mux:?} {r}"
            );
        }
        assert_eq!(live.bars().len(), 2, "{mux:?} {:?}", live.bars());

        let c = live.spawn_args("claude", "w3", &["-Tab"]);
        let c: Vec<&str> = c.iter().map(String::as_str).collect();
        let close: &[&str] = &["sidebar", "-Close"];
        let jobs = live.bg(&[&c, close]);
        let (code, spawned) = live.server.job_wait(jobs[0], LONG);
        assert_eq!(code, 0, "{mux:?} {spawned}");
        let (code, closed) = live.server.job_wait(jobs[1], LONG);
        assert_eq!(code, 0, "{mux:?} {closed}");
        let panes: Vec<Value> = closed["panes"].as_array().unwrap().clone();
        // Whichever ran first, the spawn answered a bar the close then closed, or none.
        assert!(
            spawned["sidebar"] == Value::Null || panes.contains(&spawned["sidebar"]),
            "{mux:?} {spawned} {closed}"
        );
        assert!(spawned.get("sidebarError").is_none(), "{mux:?} {spawned}");
        for pane in &panes {
            live.wait_gone(pane.as_str().unwrap());
        }
        assert!(live.bars().is_empty(), "{mux:?} {:?}", live.bars());
        let w4 = live.spawn("w4", &["-Tab"]);
        assert_eq!(w4["sidebar"], Value::Null, "{mux:?} {w4}");
        assert!(live.bars().is_empty(), "{mux:?} {:?}", live.bars());
    }

    // three agents in three windows each show one bar, in its own window and proven by its window's record
    // (its pane, its pane's process, that process `<exe> watch`); closing a view closes its bar and never stops the
    // agent.
    fn three_agents_in_three_windows_each_get_one_proven_bar(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let workers = [
            live.spawn("w1", &["-Tab"]),
            live.spawn("w2", &["-Tab"]),
            live.spawn("w3", &["-Tab"]),
        ];
        let mut windows: Vec<String> = Vec::new();
        for w in &workers {
            assert!(w.get("sidebarError").is_none(), "{mux:?} {w}");
            let (bar, window) = (s(&w["sidebar"]), s(&w["window"]));
            assert_eq!(live.field(bar, "#{window_id}"), window, "{mux:?} {w}");
            let pid = live.field(bar, "#{pane_pid}");
            let record = live.call(&[
                "show-options",
                "-wqv",
                "-t",
                window,
                "@mux-agents-sidebar-id",
            ]);
            assert!(
                record.trim_end().starts_with(&format!("{bar} {pid} ")),
                "{mux:?} {window}: {record:?}"
            );
            let row = mux_agent::proc::host()
                .row(pid.parse().unwrap())
                .unwrap_or_else(|| panic!("{mux:?}: no process {pid}"));
            assert_eq!(
                row.argv
                    .as_deref()
                    .and_then(|a| a.get(1))
                    .map(String::as_str),
                Some("watch"),
                "{mux:?} {row:?}"
            );
            windows.push(window.to_string());
        }
        windows.sort();
        windows.dedup();
        assert_eq!(windows.len(), 3, "{mux:?}");
        assert_eq!(live.bars().len(), 3, "{mux:?} {:?}", live.bars());
        // Closing w1's view closes its bar; w1 keeps running, detached.
        let bar1 = s(&workers[0]["sidebar"]).to_string();
        let pid = live.worker_pid(&workers[0]);
        let (code, child) = live.server.command(pid, &["@detach"]);
        assert_eq!(code, 0, "{mux:?}");
        let child = child["pid"].as_u64().unwrap() as u32;
        let _owned = support::Sentinels(vec![(child, support::start_of(child))]);
        live.call(&["kill-pane", "-t", s(&workers[0]["pane"])]);
        live.wait_gone(s(&workers[0]["pane"]));
        live.wait_gone(&bar1);
        let (code, list) = live.run(&["list", "-All"]);
        assert_eq!(code, 0, "{mux:?} {list}");
        let row = list["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "w1")
            .unwrap();
        assert_eq!(
            (&row["life"], &row["view"]),
            (&json!("running"), &json!("detached")),
            "{mux:?} {row}"
        );
        assert_eq!(live.bars().len(), 2, "{mux:?} {:?}", live.bars());
    }

    // QA: break-pane moves the window's last view to a new window. The old bar, before it leaves, gives that
    // window its own bar; nothing else would (no other bar runs). Each of `bars` is `<pane>|<window>|<width>`.
    fn break_pane_of_the_last_view_gives_its_new_window_a_bar(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let w1 = live.spawn("w1", &["-Split", "h"]);
        assert!(w1.get("sidebarError").is_none(), "{mux:?} {w1}");
        let bar = s(&w1["sidebar"]).to_string();
        let pane = s(&w1["pane"]).to_string();
        live.worker_pid(&w1);
        // The spawn marked the window as one that held a view before it returned, so the bar leaves whether or not
        // a tick of it ever saw the view.
        let viewed = live.call(&[
            "show-options",
            "-wqv",
            "-t",
            s(&w1["window"]),
            "@mux-agents-viewed",
        ]);
        assert_eq!(viewed.trim_end(), "1", "{mux:?}");
        live.call(&["break-pane", "-d", "-s", &pane]);
        let window = live.field(&pane, "#{window_id}");
        assert_ne!(window, s(&w1["window"]), "{mux:?}");
        live.wait_gone(&bar);
        live.server.wait("a bar in the view's new window", || {
            live.bars()
                .iter()
                .any(|b| b.split('|').nth(1) == Some(window.as_str()))
                .then_some(())
        });
        assert_eq!(live.bars().len(), 1, "{mux:?} {:?}", live.bars());
    }

    // A sidebar lock held elsewhere for 15 s: the spawn still succeeds, its agent recorded and committed, with no bar
    // and the lock refusal as sidebarError; the spawn lock was already released, so a later spawn opens the bar.
    fn sidebar_lock_timeout_still_spawns(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        // The first spawn opens the bar, and so makes the sidebar lock file; the harness removes it with the server.
        let w1 = live.spawn("w1", &["-Tab"]);
        let bar = s(&w1["sidebar"]).to_string();
        let session = live.field(live.server.pane(), "#{session_id}");
        let path = support::lock_file(
            &support::lock_root().unwrap(),
            &format!("sidebar|{}|{session}", live.server.key()),
        );
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        held.lock().unwrap();
        let at = Instant::now();
        let w2 = live.spawn("w2", &["-Tab"]);
        let took = at.elapsed();
        assert_eq!(
            (&w2["sidebar"], &w2["sidebarError"]),
            (&Value::Null, &json!("timed out waiting for the agent lock")),
            "{mux:?} {w2}"
        );
        assert!(
            took >= Duration::from_secs(15) && took < Duration::from_secs(30),
            "{mux:?}: {took:?}"
        );
        assert!(
            live.gen_file(s(&w2["id"]), "commit.json").is_file(),
            "{mux:?}"
        );
        held.unlock().unwrap();
        drop(held);
        assert_eq!(live.tagged().len(), 2, "{mux:?}");
        // w3's own window gets its own bar; w1's stays.
        let w3 = live.spawn("w3", &["-Tab"]);
        let bar3 = s(&w3["sidebar"]);
        assert_ne!(bar3, bar.as_str(), "{mux:?} {w3}");
        assert_eq!(
            live.field(bar3, "#{window_id}"),
            s(&w3["window"]),
            "{mux:?}"
        );
        assert!(
            live.bars()
                .iter()
                .any(|b| b.starts_with(&format!("{bar}|"))),
            "{mux:?} {:?}",
            live.bars()
        );
    }

    // The messages in the root's mailbox for agent `w`'s exit, as stored.
    fn exit_messages(live: &Live, w: &Value) -> Vec<Value> {
        let r = json_file(&live.agent_dir(s(&w["id"])).join("record.json"));
        let exit = match r["generation"].as_str() {
            Some(n) if r["lifecycle"] == 1 => format!("exit-{}-{n}", s(&w["id"])),
            _ => format!("exit-{}", s(&w["id"])),
        };
        live.mailbox(&live.root_box())
            .into_iter()
            .filter(|m| m["id"] == exit.as_str())
            .collect()
    }

    // Spawns a tab agent and a split agent and kills both panes from outside, as a user closing them would.
    fn two_killed(live: &mut Live, n: u32) -> [Value; 2] {
        let tab = live.spawn(&format!("v{n}"), &["-Tab"]);
        let split = live.spawn(&format!("v{}", n + 1), &["-Split", "h"]);
        for w in [&tab, &split] {
            live.worker_pid(w);
        }
        for w in [&tab, &split] {
            live.call(&["kill-pane", "-t", s(&w["pane"])]);
            live.wait_gone(s(&w["pane"]));
        }
        [tab, split]
    }

    // Closing a view leaves its core member running; each foreground trigger preserves it.
    fn assert_detached_running(live: &mut Live, w: &Value) {
        assert!(!live.gen_file(s(&w["id"]), "notice.json").exists());
        let (code, list) = live.run(&["list", "-All"]);
        assert_eq!(code, 0);
        let row = list["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == w["name"])
            .unwrap();
        assert_eq!(
            (&row["life"], &row["view"], &row["hcAddress"]),
            (&json!("running"), &json!("detached"), &w["hcAddress"])
        );
        assert!(row["pane"].is_null());
        assert!(exit_messages(live, w).is_empty());
        assert!(!live.gen_file(s(&w["id"]), "notice.json").exists());
        assert_eq!(
            core(live, &["get", s(&w["hcAddress"]), "--json"])["state"],
            "live"
        );
    }
    fn gone_pane_closed_by_each_trigger(mux: Mux) {
        for trigger in ["list", "read", "read other", "spawn", "stop other"] {
            let Some(mut live) = Live::human(mux) else {
                return;
            };
            live.close_bar();
            let other = live.spawn("other", &[]);
            live.worker_pid(&other);
            let closed = two_killed(&mut live, 1);
            let (code, reply) = if trigger == "spawn" {
                (0, live.spawn("fresh", &[]))
            } else {
                live.run(&trigger.split(' ').collect::<Vec<_>>())
            };
            assert_eq!(code, 0, "{reply}");
            for w in &closed {
                assert_detached_running(&mut live, w);
                for _ in 0..2 {
                    let (code, stop) = live.run(&["stop", s(&w["name"])]);
                    assert_eq!(code, 0, "{stop}");
                    assert_eq!(stop["hcAddress"], w["hcAddress"]);
                    assert_eq!(stop["life"], "ended");
                }
                assert_eq!(exit_messages(&live, w).len(), 1);
            }
        }
    }

    // Watch preserves detached members and reaps only terminal completion.
    fn bar_drops_a_gone_agent(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        // Both beside the caller: the window keeps w2's view, and so its bar, when w1's pane goes.
        let w1 = live.spawn("w1", &["-Split", "h"]);
        let w2 = live.spawn("w2", &["-Split", "h"]);
        let bar = s(&w1["sidebar"]).to_string();
        let pid = live.worker_pid(&w1);
        live.worker_pid(&w2);
        live.log_frames(&bar);
        // o2 call 35: both are ready before w1's view goes and its client quits.
        live.ready(&w1);
        live.ready(&w2);
        live.call(&["kill-pane", "-t", s(&w1["pane"])]);
        live.wait_gone(s(&w1["pane"]));
        live.wait_tick(&bar);
        let screen = live.call(&["capture-pane", "-p", "-t", &bar]);
        assert!(row_at(&screen, "w1").is_some() && row_at(&screen, "w2").is_some());
        let marker = live.gen_file(s(&w1["id"]), "notice.json");
        assert!(!marker.exists());
        assert!(exit_messages(&live, &w1).is_empty());
        support::write_atomic(&live.dir.join(format!("quit.{pid}")), "7\n");
        assert!(eventually(LONG, || {
            let screen = live.call(&["capture-pane", "-p", "-t", &bar]);
            marker.exists()
                && screen.contains("1 working")
                && screen.contains("1 ended")
                && screen
                    .lines()
                    .any(|l| l.contains("w1") && l.contains("exited 7"))
        }));
        assert_eq!(json_file(&marker)["kind"], "exited");
        assert_eq!(exit_messages(&live, &w1).len(), 1);
    }
    fn kill_dead_view(live: &Live, w: &Value) {
        let r = live.view_of(s(&w["id"]));
        let launch = mux_agent::agents::registry::Launch {
            command: s(&r["startCommand"]).into(),
            pid: None,
        };
        let args = mux_agent::agents::registry::kill_if_dead(s(&w["pane"]), &launch);
        live.call(&args.iter().map(String::as_str).collect::<Vec<_>>());
    }

    /// The 1-based line of `screen` whose second token is `name`: that agent's name line (`◉`/`◌`, then the name).
    fn row_at(screen: &str, name: &str) -> Option<usize> {
        screen
            .lines()
            .position(|l| l.split_whitespace().nth(1) == Some(name))
            .map(|i| i + 1)
    }

    /// with clicks on (no MUX_AGENTS_ASCII), `w1` and `w2` spawned and both drawn on the bar, its frames logged
    /// and the root pane active; answers the bar and w1's pane.
    fn two_rows(live: &mut Live) -> (String, String) {
        live.assert_not_ascii();
        // Both beside the caller: their window, the caller's, keeps the bar a client sees.
        let w1 = live.spawn("w1", &["-Split", "h"]);
        let w2 = live.spawn("w2", &["-Split", "h"]);
        let bar = s(&w1["sidebar"]).to_string();
        live.worker_pid(&w1);
        live.worker_pid(&w2);
        live.log_frames(&bar);
        let mut screen = String::new();
        let drawn = eventually(Duration::from_secs(30), || {
            screen = live.call(&["capture-pane", "-p", "-t", &bar]);
            row_at(&screen, "w1").is_some() && row_at(&screen, "w2").is_some()
        });
        assert!(drawn, "{:?}: the bar shows:\n{screen}", live.mux());
        assert_eq!(live.focused(), live.server.pane(), "{:?}", live.mux());
        (bar, s(&w1["pane"]).to_string())
    }

    /// The panes of w1 and w2 in the order the bar draws them now, top first. The bar lists running agents in the
    /// session's pane order (`-Split h` from the root puts w2 between the root and w1), so a key that takes the
    /// first drawn agent takes w2 here, not the first spawned.
    fn drawn_ends(live: &Live, bar: &str) -> (String, String) {
        let screen = live.call(&["capture-pane", "-p", "-t", bar]);
        let mut names = ["w1", "w2"];
        names.sort_by_key(|n| {
            row_at(&screen, n)
                .unwrap_or_else(|| panic!("{:?}: no row for {n}:\n{screen}", live.mux()))
        });
        let pane = |name: &str| {
            live.panes("#{pane_id}|#{@agent-name}")
                .iter()
                .find(|r| r.ends_with(&format!("|{name}")))
                .map(|r| r.split('|').next().unwrap().to_string())
                .unwrap()
        };
        (pane(names[0]), pane(names[1]))
    }

    /// Waits up to 10 s for `pane` to be the session's active pane.
    fn wait_focused(live: &Live, pane: &str, what: &str) {
        let mut now = String::new();
        let moved = eventually(Duration::from_secs(10), || {
            now = live.focused();
            now == pane
        });
        assert!(
            moved,
            "{:?}: {what}: the active pane is {now}, not {pane}",
            live.mux()
        );
    }

    // a left click on an agent's name line focuses that agent's pane, and the bar keeps redrawing after it.
    fn bar_click_focuses_an_agent(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let (bar, p1) = two_rows(&mut live);
        live.after_redraw(&bar);
        let y = live.row_of(&bar, "w1");
        live.press(&bar, y);
        wait_focused(&live, &p1, "a click on w1's row");
        live.wait_tick(&bar);
    }

    // `j` selects the first agent drawn and Enter focuses it.
    fn bar_keys_focus_an_agent(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let (bar, _) = two_rows(&mut live);
        live.after_redraw(&bar);
        let (top, _) = drawn_ends(&live, &bar);
        live.call(&["send-keys", "-t", &bar, "-l", "j"]);
        live.call(&["send-keys", "-t", &bar, "Enter"]);
        wait_focused(&live, &top, "j then Enter");
    }

    // `n` (herdr's next agent) from a pane that is no agent's goes to the bar's first agent, `N` to its last.
    fn bar_n_and_shift_n_step_through_agents(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let (bar, _) = two_rows(&mut live);
        live.after_redraw(&bar);
        let (top, _) = drawn_ends(&live, &bar);
        live.call(&["send-keys", "-t", &bar, "-l", "n"]);
        wait_focused(&live, &top, "n from the root pane");
        let root = live.server.pane().to_string();
        live.call(&["select-pane", "-t", &root]);
        live.after_redraw(&bar);
        let (top, bottom) = drawn_ends(&live, &bar);
        live.call(&["send-keys", "-t", &bar, "-l", "N"]);
        wait_focused(&live, &bottom, "N from the root pane");
        // The documented binding: a run-shell (not below any pane) sends `n` to the bar the current window records
        // (a window option, read with -w). tmux's
        // run-shell knows its own server; the multiplexer's binary is named by the server under test.
        if mux != Mux::Rmux {
            live.call(&["select-pane", "-t", &root]);
            live.after_redraw(&bar);
            let tmux = live.server.binary().to_string_lossy().into_owned();
            let socket = live.server.socket().to_string_lossy().into_owned();
            let line = format!(
                "{tmux} -S '{socket}' send-keys -t \"$({tmux} -S '{socket}' show -wqv @mux-agents-sidebar-id | cut -d' ' -f1)\" n"
            );
            live.call(&["run-shell", &line]);
            wait_focused(&live, &top, "the documented binding");
        }
    }

    // a press read after the bar shrank, before it drew at its new size, may land on a row that moved: it is
    // dropped and the bar redraws at once (well before its next tick, ~1.9 s away). Nothing is focused.
    fn bar_click_after_shrinking_does_nothing(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let (bar, _) = two_rows(&mut live);
        let root = live.server.pane().to_string();
        let width = live.field(&bar, "#{pane_width}");
        let mut attempt = 1;
        let n = loop {
            let y = live.row_of(&bar, "w1");
            live.after_redraw(&bar);
            let n = live.frames(&bar);
            live.call(&["resize-pane", "-t", &bar, "-x", "15"]);
            live.press(&bar, y);
            // A tick redraw fell between the resize and the press: try again from the bar's own width.
            if live.frames(&bar) < n + 2 {
                break n;
            }
            assert!(attempt < 3, "{mux:?}: a tick fell before the press 3 times");
            attempt += 1;
            live.call(&["resize-pane", "-t", &bar, "-x", &width]);
            live.wait_place(&bar, "the bar back at its width");
        };
        let mut seen = n;
        let redrew = eventually(Duration::from_millis(500), || {
            seen = live.frames(&bar);
            seen > n
        });
        assert!(
            redrew,
            "{mux:?}: no redraw within 500 ms of the press ({n} -> {seen} frames)"
        );
        assert_eq!(live.focused(), root, "{mux:?}: the dropped press focused");
    }

    /// A real tmux client attached to a session on a 120x40 pty, the master drained by a thread; killed on drop.
    struct Client {
        child: crate::support::bounded::Watched,
        master: std::fs::File,
        // Every byte the client's terminal was sent, as the drain thread read it.
        seen: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }

    impl Client {
        fn attach(live: &Live, session: &str) -> Client {
            use std::io::Read;
            use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
            use std::os::unix::process::CommandExt;
            use std::process::Stdio;

            // SAFETY: each call takes the descriptor `posix_openpt` just returned, or the name `ptsname` returned
            // for it, read before any other call (no other thread of this binary calls `ptsname`).
            let (master, slave) = unsafe {
                let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
                assert!(
                    master >= 0,
                    "posix_openpt: {}",
                    std::io::Error::last_os_error()
                );
                let master = OwnedFd::from_raw_fd(master);
                let fd = master.as_raw_fd();
                assert_eq!(libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC), 0);
                assert_eq!(libc::grantpt(fd), 0, "grantpt");
                assert_eq!(libc::unlockpt(fd), 0, "unlockpt");
                let name = libc::ptsname(fd);
                assert!(!name.is_null(), "ptsname");
                let slave = libc::open(name, libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
                assert!(
                    slave >= 0,
                    "open slave: {}",
                    std::io::Error::last_os_error()
                );
                (master, OwnedFd::from_raw_fd(slave))
            };
            let size = libc::winsize {
                ws_row: 40,
                ws_col: 120,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            // SAFETY: `size` is valid for the call.
            let set = unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCSWINSZ as _, &size) };
            assert_eq!(set, 0, "TIOCSWINSZ: {}", std::io::Error::last_os_error());
            let mut attach = Command::new(live.server.binary());
            attach
                .arg("-S")
                .arg(live.server.socket())
                .args(["attach", "-t", session])
                .env_clear()
                .env("TERM", "xterm-256color")
                .env("HOME", live.dir.join("home"))
                .stdin(Stdio::from(slave.try_clone().unwrap()))
                .stdout(Stdio::from(slave.try_clone().unwrap()))
                .stderr(Stdio::from(slave));
            if let Some(lang) = std::env::var_os("LANG") {
                attach.env("LANG", lang);
            }
            // SAFETY: the hook only calls setsid and ioctl, both async-signal-safe: the pty becomes the client's
            // controlling terminal.
            unsafe {
                attach.pre_exec(|| {
                    if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child =
                crate::support::bounded::spawn(&mut attach, crate::support::bounded::HELD).unwrap();
            let master = std::fs::File::from(master);
            let mut drain = master.try_clone().unwrap();
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let into = seen.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = drain.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    into.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            });
            Client {
                child,
                master,
                seen,
            }
        }

        /// A checkpoint in what the client terminal was sent: `text_since` reads only what arrives after it.
        fn mark(&self) -> usize {
            self.seen.lock().unwrap().len()
        }

        /// What the client terminal was sent after checkpoint `from`, as `visible_text` shows it.
        fn text_since(&self, from: usize) -> String {
            let seen = self.seen.lock().unwrap();
            visible_text(&seen[from.min(seen.len())..])
        }

        /// Bytes typed on the client's terminal.
        fn write(&mut self, bytes: &[u8]) {
            self.master.write_all(bytes).unwrap();
        }
    }

    impl Drop for Client {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    // through a real client: tmux 3.8 attached on a 120x40 terminal with `mouse on` hands a click on w1's name
    // line to the bar, which focuses w1.
    fn bar_real_client_click_focuses(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let (bar, p1) = two_rows(&mut live);
        let session = live.field(live.server.pane(), "#{session_id}");
        let mut client = Client::attach(&live, &session);
        live.server.wait("the client to attach", || {
            (!live.call(&["list-clients"]).trim().is_empty()).then_some(())
        });
        live.call(&["set-option", "-g", "mouse", "on"]);
        live.wait_place(&bar, "the bar in place at the client's size");
        live.after_redraw(&bar);
        let y = live.row_of(&bar, "w1");
        let left: usize = live.num(&bar, "#{pane_left}");
        let top: usize = live.num(&bar, "#{pane_top}");
        for end in ['M', 'm'] {
            client.write(format!("\x1b[<0;{};{}{end}", left + 3, top + y).as_bytes());
        }
        wait_focused(&live, &p1, "a real client's click on w1's row");
        drop(client);
    }

    // through a real client: `p` in the bar opens herdr's peek, a popup over the client paging the first agent's
    // last lines (tmux's display-popup needs a client); `q` closes it and the peek file is gone.
    fn bar_real_client_peek(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let (bar, _) = two_rows(&mut live);
        let session = live.field(live.server.pane(), "#{session_id}");
        let mut client = Client::attach(&live, &session);
        live.server.wait("the client to attach", || {
            (!live.call(&["list-clients"]).trim().is_empty()).then_some(())
        });
        live.wait_place(&bar, "the bar in place at the client's size");
        live.after_redraw(&bar);
        // A default-shell that is no POSIX shell (QA): the popup runs /bin/sh itself.
        if std::path::Path::new("/bin/tcsh").exists() {
            live.call(&["set-option", "-g", "default-shell", "/bin/tcsh"]);
        }
        live.call(&["send-keys", "-t", &bar, "-l", "p"]);
        // The peek files in the state folder.
        let peeks = |live: &Live| {
            let mut found = 0;
            for dir in std::fs::read_dir(live.dir.join("state"))
                .into_iter()
                .flatten()
                .flatten()
            {
                for f in std::fs::read_dir(dir.path())
                    .into_iter()
                    .flatten()
                    .flatten()
                {
                    if f.file_name().to_string_lossy().starts_with("peek-") {
                        found += 1;
                    }
                }
            }
            found
        };
        // The popup is open: its pager runs under this server, on a deleted file (the shell deleted it as it
        // started), and the bar keeps redrawing on its ticks meanwhile, never waiting on the popup.
        let server: u64 = live.field(live.server.pane(), "#{pid}").parse().unwrap();
        live.server.wait("the popup's pager under this server", || {
            pagers_under(server).then_some(())
        });
        assert_eq!(peeks(&live), 0, "the popup deleted its file as it opened");
        // A tick is 2 s; a bar blocked on the popup would not draw again within 5 s.
        let n = live.frames(&bar);
        assert!(
            eventually(Duration::from_secs(5), || live.frames(&bar) > n),
            "{mux:?}: the bar stopped drawing while the popup was open"
        );
        assert!(pagers_under(server), "the popup is still open");
        client.write(b"q");
        live.server.wait("q to close the popup", || {
            (!pagers_under(server)).then_some(())
        });
        drop(client);
    }

    // remain-on-exit keeps an ended member's view dead: list reaps completion without view cleanup.
    // The explicit conditional kills only the original dead view, regardless of tags.
    fn dead_pane_closed_by_one_conditional(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        live.close_bar();
        live.call(&["set-option", "-g", "remain-on-exit", "on"]);
        let quit_dead = |live: &mut Live, w: &Value| {
            // o2 call 35: a member that exits after readiness ends ordinarily.
            live.ready(w);
            let pid = live.worker_pid(w);
            support::write_atomic(&live.dir.join(format!("quit.{pid}")), "5\n");
            live.server.wait_file(&format!("exited.{pid}"));
            let pane = s(&w["pane"]).to_string();
            live.server.wait("the pane to be dead", || {
                (live.field(&pane, "#{pane_dead}") == "1").then_some(())
            });
        };
        let w1 = live.spawn("w1", &[]);
        quit_dead(&mut live, &w1);
        // List posts `exited 5` but leaves the dead attachment view in place.
        let (code, list) = live.run(&["list"]);
        assert_eq!(code, 0, "{mux:?} {list}");
        assert!(!names(&list).contains(&"w1".to_string()), "{mux:?} {list}");
        assert!(
            live.panes("#{pane_id}")
                .contains(&s(&w1["pane"]).to_string())
        );
        kill_dead_view(&live, &w1);
        live.wait_gone(s(&w1["pane"]));
        let notice = json_file(&live.gen_file(s(&w1["id"]), "notice.json"));
        assert_eq!(
            (&notice["kind"], &notice["exitCode"]),
            (&json!("exited"), &json!(5)),
            "{mux:?} {notice}"
        );
        assert_eq!(exit_messages(&live, &w1).len(), 1, "{mux:?}");

        let w2 = live.spawn("w2", &[]);
        let w3 = live.spawn("w3", &[]);
        live.worker_pid(&w3);
        quit_dead(&mut live, &w2);
        let (p2, p3) = (s(&w2["pane"]).to_string(), s(&w3["pane"]).to_string());
        let listed = |live: &Live, pane: &str| live.panes("#{pane_id}").iter().any(|p| p == pane);
        // Live with its own launch: kept.
        kill_dead_view(&live, &w3);
        assert!(listed(&live, &p3), "{mux:?}: a live pane was killed");
        // Its tag moved (as rmux's rotate-window leaves it): explicit cleanup still owns the dead view.
        live.call(&["set-option", "-p", "-t", &p2, "@agent-id", "bbbbbbbbbbbb"]);
        let (code, list) = live.run(&["list"]);
        assert_eq!(
            (code, names(&list)),
            (0, vec!["w3".to_string()]),
            "{mux:?} {list}"
        );
        assert!(live.panes("#{pane_id}").contains(&p2));
        kill_dead_view(&live, &w2);
        live.wait_gone(&p2);
        assert_eq!(exit_messages(&live, &w2).len(), 1, "{mux:?}");
        assert!(listed(&live, &p3), "{mux:?}");
        // A replacement (respawned with another command, then dead): kept by the conditional and by list.
        let w4 = live.spawn("w4", &[]);
        live.worker_pid(&w4);
        let p4 = s(&w4["pane"]).to_string();
        live.call(&["respawn-pane", "-k", "-t", &p4, "true"]);
        live.server.wait("the replacement to be dead", || {
            (live.field(&p4, "#{pane_dead}") == "1").then_some(())
        });
        kill_dead_view(&live, &w4);
        assert!(
            listed(&live, &p4),
            "{mux:?}: a replaced dead pane was killed"
        );
        let (code, list) = live.run(&["list"]);
        assert_eq!(
            (code, names(&list)),
            (0, vec!["w3".to_string(), "w4".to_string()]),
            "{mux:?} {list}"
        );
        assert_detached_running(&mut live, &w4);
        assert!(listed(&live, &p4), "{mux:?}: list killed a replacement");
        assert!(listed(&live, &p3), "{mux:?}");
    }

    // test 9: two split agents, then rotate-window (rmux leaves the @agent-* tags at their places; tmux moves them
    // with the panes). From a clean pane in another window, list, read, focus and send reach each agent by its own
    // pane; stop closes only that agent's pane; the other is still reachable and the user's pane lives on.
    fn rotated_window_reaches_each_agent_by_its_pane(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        live.close_bar();
        let root = live.server.pane().to_string();
        let w1 = live.spawn("w1", &["-Split", "h"]);
        let w2 = live.spawn("w2", &["-Split", "v"]);
        let (p1, p2) = (s(&w1["pane"]).to_string(), s(&w2["pane"]).to_string());
        let (pid1, pid2) = (live.worker_pid(&w1), live.worker_pid(&w2));
        let session = live.field(&root, "#{session_id}");
        let clean = live
            .call(&[
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                &format!("{session}:"),
                "sh",
            ])
            .trim()
            .to_string();
        live.call(&["rotate-window", "-t", &root]);
        let tags = live.panes("#{pane_id}|#{@agent-name}");
        eprintln!("{mux:?} after rotate-window: {tags:?}");

        let (code, list) = live.run_in(&clean, &["list"]);
        assert_eq!(code, 0, "{mux:?} {list}");
        let rows: BTreeSet<String> = list["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| format!("{}@{}", s(&a["name"]), s(&a["pane"])))
            .collect();
        let want: BTreeSet<String> = [format!("w1@{p1}"), format!("w2@{p2}")].into();
        assert_eq!(rows, want, "{mux:?} {list}");

        for (w, pane, pid) in [(&w1, &p1, pid1), (&w2, &p2, pid2)] {
            let name = s(&w["name"]);
            // read <name> is that agent's own screen.
            let line = format!("{name} screen");
            live.call(&["send-keys", "-t", pane, "-l", &line]);
            live.call(&["send-keys", "-t", pane, "Enter"]);
            live.server.wait_line(&format!("input.{pid}.log"), &line);
            let (code, read) = live.run_in(&clean, &["read", name, "-Lines", "20"]);
            assert_eq!((code, &read["pane"]), (0, &json!(pane)), "{mux:?} {read}");
            assert!(
                s(&read["text"]).lines().any(|l| l.trim() == line),
                "{mux:?} {name}: {read}"
            );
            // focus selects that agent's own pane.
            let (code, focused) = live.run_in(&clean, &["focus", name]);
            assert_eq!(
                (code, &focused["pane"]),
                (0, &json!(pane)),
                "{mux:?} {focused}"
            );
            assert_eq!(live.active(pane), *pane, "{mux:?} {name}");
            // send reaches that agent's own mailbox.
            let body = format!("to {name}");
            let (code, sent) = live.run_in(&clean, &["send", name, "-Body", &body]);
            assert_eq!(
                (code, &sent["recipients"]),
                (0, &json!([{"name": name, "id": w["id"]}])),
                "{mux:?} {sent}"
            );
            let mail = live.mailbox(s(&w["id"]));
            assert!(
                mail.iter().any(|m| m["body"] == body.as_str()),
                "{mux:?} {name}: {mail:?}"
            );
        }

        // stop w1 closes only w1's pane.
        // o2 call 35: w1 is ready before its stop.
        live.ready(&w1);
        let (code, stop) = live.run_in(&clean, &["stop", "w1"]);
        assert_eq!(code, 0);
        assert_eq!(
            stop,
            json!({"ok":true,"verb":"stop","name":"w1","hcAddress":w1["hcAddress"],"life":"ended","stopped":true,"unresolved":[]})
        );
        assert!(eventually(LONG, || core(
            &live,
            &["get", s(&w1["hcAddress"]), "--json"]
        )["state"]
            == "finished"));
        live.wait_gone(&p1);
        let panes = live.panes("#{pane_id}");
        for pane in [&root, &p2, &clean] {
            assert!(
                panes.contains(pane),
                "{mux:?}: {pane} was closed: {panes:?}"
            );
        }
        // w2 is still reachable.
        let (code, list) = live.run_in(&clean, &["list"]);
        assert_eq!(
            (code, names(&list)),
            (0, vec!["w2".to_string()]),
            "{mux:?} {list}"
        );
        let (code, sent) = live.run_in(&clean, &["send", "w2", "-Body", "still here"]);
        assert_eq!(
            (code, &sent["recipients"]),
            (0, &json!([{"name": "w2", "id": w2["id"]}])),
            "{mux:?} {sent}"
        );
        let (code, read) = live.run_in(&clean, &["read", "w2", "-Lines", "20"]);
        assert_eq!((code, &read["pane"]), (0, &json!(p2)), "{mux:?} {read}");
        // A root left carrying an agent's tag is no agent: it cannot send as one.
        if !live.field(&root, "#{@agent-id}").is_empty() {
            let (code, refused) = live.run(&["send", "w2", "-Body", "x"]);
            assert_eq!(
                (code, &refused["check"]),
                (2, &json!("parent")),
                "{mux:?} {refused}"
            );
        }
    }

    // One test fn per scenario, each on its own fresh server.
    macro_rules! tests_on {
        ($module:ident, $mux:ident, [$($name:ident),* $(,)?]) => {
            mod $module {
                $(
                    #[test]
                    fn $name() {
                        super::$name(super::Mux::$mux)
                    }
                )*
            }
        };
    }

    fn core(live: &Live, args: &[&str]) -> Value {
        support::private_core::run(
            live.server.core.as_ref().unwrap().command(args),
            live.dir.path(),
        )
        .unwrap()
    }
    // release-1d live: stop the core so hc keeps the terminal as a recovery shell (Lost), then release it: the
    // shell stays live carrying exactly mux.kept and mux.keptName; released.json exists.
    fn a_lost_member_is_released_and_keeps_only_its_kept_labels(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let w = live.spawn("w1", &[]);
        let (id, address) = (s(&w["id"]).to_string(), s(&w["hcAddress"]).to_string());
        let (code, seeded) = live.run(&["__life", "seed", &id, "0"]);
        assert_eq!(code, 0, "{mux:?}: {seeded}");
        let before = core(&live, &["get", &address, "--json"])["labels"].clone();
        let pc = live.server.core.as_mut().unwrap();
        assert!(pc.stop(), "{mux:?}: core stop");
        pc.start();
        // Call 43: wait on the product's own classification (`list -All` row `life`, from `lifecycle::classify`),
        // not on one OS's recovery argv; on a timeout report the terminal hc shows and the last list.
        let (started, mut last, mut rows) = (Instant::now(), Value::Null, Value::Null);
        let lost = eventually(LONG, || {
            last = core(&live, &["get", &address, "--json"]);
            rows = live.run(&["list", "-All"]).1;
            rows["agents"]
                .as_array()
                .into_iter()
                .flatten()
                // Call 52: an ordinary list row has no `id`; the member's row is the one at its recorded address.
                .any(|r| r["hcAddress"] == json!(address) && r["life"] == "lost")
        });
        assert!(
            lost,
            "{mux:?}: Lost not seen after {:?}; last get: {last}; last list: {rows}",
            started.elapsed()
        );
        let (code, out) = live.run(&["__life", "release", &id, "0"]);
        assert_eq!(
            (code, &out["released"]),
            (0, &json!(true)),
            "{mux:?}: {out}"
        );
        let t = core(&live, &["get", &address, "--json"]);
        assert_eq!(t["state"], "live", "{mux:?}");
        let want = json!({"mux.kept": before["mux.team"], "mux.keptName": before["mux.name"]});
        assert_eq!(t["labels"], want, "{mux:?}");
        assert!(
            live.agent_dir(&id).join("gens/0/released.json").exists(),
            "{mux:?}"
        );
    }

    // settle-1b live, b-calls 38: a forward killed at `promote` by A3b-1's real crash harness leaves generation 0
    // orphan-running; G11 ends it, hc shows it finished, and the record keeps naming generation 1.
    fn an_orphan_left_by_a_forward_crash_is_ended_by_terminate(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let w = live.spawn("w1", &[]);
        let (id, address) = (s(&w["id"]).to_string(), s(&w["hcAddress"]).to_string());
        let (code, seeded) = live.run(&["__life", "seed", &id, "0"]);
        assert_eq!(code, 0, "{mux:?}: {seeded}");
        let state = live.state_text();
        let vars = [
            ("MUX_AGENTS_STATE", state.as_str()),
            ("MUX_AGENTS_CRASH", "promote"),
        ];
        let bin = env!("CARGO_BIN_EXE_mux-agent");
        let (code, out, err) =
            live.server
                .run_raw(bin, &["__life", "forward", &id, "1"], &vars, &[]);
        assert_eq!(code, 86, "{mux:?}: killed at promote: {out} {err}");
        let record = std::fs::read(live.agent_dir(&id).join("record.json")).unwrap();
        let record: Value = serde_json::from_slice(&record).unwrap();
        assert_eq!(
            record["generation"], "1",
            "{mux:?}: generation 1 was promoted before the kill"
        );
        assert_eq!(
            core(&live, &["get", &address, "--json"])["state"],
            "live",
            "{mux:?}: orphan 0 still runs"
        );
        let ended = || {
            let (code, out) = live.run(&["__life", "terminate", &id, "0"]);
            code == 0 && out["ended"] == true
        };
        assert!(eventually(LONG, ended), "{mux:?}: terminate never ended it");
        assert_eq!(
            core(&live, &["get", &address, "--json"])["state"],
            "finished",
            "{mux:?}"
        );
        assert!(
            live.agent_dir(&id).join("gens/0/ended.json").exists(),
            "{mux:?}"
        );
    }
    fn hc_view_close_survives_focus_stop(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        live.close_bar();
        let w = live.spawn("w1", &[]);
        let pid = live.worker_pid(&w);
        let (code, child) = live.server.command(pid, &["@detach"]);
        assert_eq!(code, 0);
        let child = child["pid"].as_u64().unwrap() as u32;
        let _owned = support::Sentinels(vec![(child, support::start_of(child))]);
        let address = s(&w["hcAddress"]);
        let folder = live.agent_dir(s(&w["id"]));
        let before = json_file(&folder.join("record.json"));
        live.call(&["kill-pane", "-t", s(&w["pane"])]);
        live.wait_gone(s(&w["pane"]));
        for _ in 0..2 {
            let (code, list) = live.run(&["list", "-All"]);
            assert_eq!(code, 0, "{list}");
            let row = &list["agents"][0];
            assert_eq!(
                (&row["life"], &row["view"], &row["hcAddress"]),
                (&json!("running"), &json!("detached"), &w["hcAddress"])
            );
            assert!(row["pane"].is_null() && row["window"].is_null());
            assert!(!live.gen_file(s(&w["id"]), "notice.json").exists());
            assert!(exit_messages(&live, &w).is_empty());
        }
        assert_eq!(core(&live, &["get", address, "--json"])["state"], "live");
        let (code, focus) = live.run(&["focus", "w1"]);
        assert_eq!(code, 0, "{focus}");
        assert_ne!(focus["pane"], w["pane"]);
        let after = json_file(&folder.join("record.json"));
        for k in [
            "id",
            "team",
            "generation",
            "sessionId",
            "hcAddress",
            "launcherArgv",
        ] {
            assert_eq!(after[k], before[k], "{k}");
        }
        // V1 (views-6f): a lifecycle member's view is in view.json; record.json never holds its pane.
        assert_eq!(live.view_of(s(&w["id"]))["pane"], focus["pane"]);
        assert!(after.get("pane").is_none(), "{after}");
        assert_eq!(live.worker_pid(&w), pid);
        let (code, stop) = live.run(&["stop", "w1"]);
        assert_eq!(code, 0, "{stop}");
        assert_eq!(
            stop,
            json!({"ok":true,"verb":"stop","name":"w1","hcAddress":address,"life":"ended","stopped":true,"unresolved":[]})
        );
        assert!(eventually(LONG, || core(
            &live,
            &["get", address, "--json"]
        )["state"]
            == "finished"));
        assert_eq!(exit_messages(&live, &w).len(), 1);
    }

    // proof-0b K2 live (4to6 items 3, 4, 19): a team (up), its leader spawns w2 down the tree with a brief; both are
    // proven from client.json (`spawn_by` waits for the leader's, `wait_proof` for each; PM call 26); with the
    // multiplexer server killed and no `up`, the leader sends (a file: agents pass free text only in files), w2 reads
    // and reports; focus is refused `views`; list and stop run with views down.
    fn members_mail_without_the_multiplexer(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let (code, up) = live.run(&["up", "-Client", "claude"]);
        assert_eq!(code, 0, "{mux:?} {up}");
        std::fs::write(live.dir.join("brief.md"), "brief").unwrap();
        std::fs::write(live.dir.join("note.md"), "without the views").unwrap();
        let lead = s(&up["leader"]["id"]).to_string();
        let (code, child) = live.spawn_by(&lead, "w2");
        assert_eq!(code, 0, "{mux:?}: the leader spawns w2: {child}");
        let w2 = s(&child["id"]).to_string();
        for id in [&lead, &w2] {
            let gen0 = live.agent_dir(id).join("gens").join("0");
            let client = crate::ready::wait_proof(&live.agent_dir(id), 0);
            assert!(
                client["pid"].is_u64(),
                "{mux:?} {id}: client.json published"
            );
            assert!(
                json_file(&gen0.join("launcher.json"))
                    .get("client")
                    .is_none(),
                "{mux:?} {id}: no client field"
            );
        }
        let (p1, p2, note) = (
            live.client_of(&lead),
            live.client_of(&w2),
            live.path("note.md"),
        );
        let server = live.server.pid();
        // Checklist 12: `try_call` is a bounded child (support::bounded) with the server's own environment.
        let _ = live.server.try_call(&["kill-server"]);
        assert!(
            eventually(LONG, || !support::alive(server)),
            "{mux:?}: the server is gone"
        );
        let (code, sent) = live.server.command(p1, &["send", "w2", "-BodyFile", &note]);
        assert_eq!(code, 0, "{mux:?} {sent}");
        let (code, read) = live.server.command(p2, &["read"]);
        assert_eq!(
            (code, bodies(&read)),
            (0, vec!["without the views".to_string()]),
            "{mux:?} {read}"
        );
        let (code, report) = live
            .server
            .command(p2, &["report", "blocked", "-Kind", "question"]);
        assert_eq!(
            (code, &report["state"]),
            (0, &json!("blocked")),
            "{mux:?} {report}"
        );
        let (code, focus) = live.server.command(p1, &["focus", "w2"]);
        assert_eq!(
            (code, &focus["check"]),
            (1, &json!("views")),
            "{mux:?} {focus}"
        );
        let (code, list) = live.server.command(p1, &["list"]);
        let row = list["agents"]
            .as_array()
            .and_then(|a| a.iter().find(|x| x["name"] == "w2"))
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            code, 0,
            "{mux:?}: list runs with views down (PM call 18) {list}"
        );
        assert!(
            !row.is_null() && row["pane"].is_null() && row["window"].is_null(),
            "{mux:?}: w2 listed without pane fields: {list}"
        );
        let (code, stop) = live.server.command(p1, &["stop", "w2"]);
        assert_eq!(
            (code, &stop["life"]),
            (0, &json!("ended")),
            "{mux:?}: stop needs no view (4to7re item 15) {stop}"
        );
    }

    // launch-1 live, a dropped `hc run` reply (ambiguity 14): spawn's process dies right after `hc run` returns; the
    // variable is set for that one process only. Nothing runs; the launcher gives up; a retry runs once.
    fn a_dropped_run_reply_runs_nothing_then_a_retry_runs_once(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let log = live.dir.join("clients.log");
        let clients = || {
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .count()
        };
        let before = clients();
        let args = live.spawn_args("claude", "w1", &[]);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let state = live.state_text();
        let vars = [
            ("MUX_AGENTS_STATE", state.as_str()),
            ("MUX_AGENTS_CRASH", "run"),
        ];
        let (code, _, _) = live
            .server
            .run_raw(env!("CARGO_BIN_EXE_mux-agent"), &args, &vars, &[]);
        assert_eq!(code, 86, "{mux:?}: killed after hc run returned");
        let ids: Vec<_> = std::fs::read_dir(live.keyed().join("agents"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        assert_eq!(ids.len(), 1, "{mux:?}: one member folder: {ids:?}");
        let commit = ids[0].join("gens").join("0").join("commit.json");
        let aborted = || {
            std::fs::read_to_string(&commit)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .is_some_and(|c| c["aborted"] == true)
        };
        assert!(eventually(LONG, aborted), "{mux:?}: the launcher gave up");
        assert_eq!(clients(), before, "{mux:?}: nothing ran");
        let (code, again) = live.try_spawn("w1", &[]);
        assert_eq!(
            code, 0,
            "{mux:?}: the name is free once the generation is void, and the variable did not leak: {again}"
        );
        live.worker_pid(&again);
        assert_eq!(
            clients(),
            before + 1,
            "{mux:?}: the retry ran exactly one client"
        );
    }

    // A worker whose view is closed still reads its own mail (phase 1c): the person sends to it after the view
    // pane is gone, and `read` run by the worker in its hc terminal, which has no TMUX or RMUX, returns that message.
    fn hc_worker_reads_after_its_view_closes(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        live.close_bar();
        let w = live.spawn("w1", &[]);
        let pid = live.worker_pid(&w);
        let client = live.client(pid);
        assert_eq!(client["env"]["HC_TERMINAL_ID"], w["hcAddress"], "{mux:?}");
        assert!(
            client["env"]["TMUX"].is_null() && client["env"]["RMUX"].is_null(),
            "{mux:?} {client}"
        );
        live.call(&["kill-pane", "-t", s(&w["pane"])]);
        live.wait_gone(s(&w["pane"]));
        let (code, sent) = live.run(&["send", "w1", "-Body", "after the view closed"]);
        assert_eq!(code, 0, "{mux:?} {sent}");
        let (code, read) = live.server.command(pid, &["read"]);
        assert_eq!(
            (code, bodies(&read)),
            (0, vec!["after the view closed".to_string()]),
            "{mux:?} {read}"
        );
        // Nothing is consumed without an ack (4b-i): the worker acks its batch, then finds nothing new.
        let (code, again) = live
            .server
            .command(pid, &["read", "-Deliver", "-Ack", s(&read["batch"])]);
        assert_eq!((code, &again["ids"]), (0, &json!([])), "{mux:?} {again}");
    }

    /// The fake client a resume started for `sid`: the one whose argv carries `--resume <sid>`. Its `sessionId` is
    /// null (the fake reads only `--session-id`), so it is found by its argv.
    fn resumed_client(live: &Live, sid: &str) -> Value {
        let log = live.dir.join("clients.log");
        let end = Instant::now() + LONG;
        loop {
            let found = std::fs::read_to_string(&log).ok().and_then(|text| {
                text.lines().find_map(|pid| {
                    let path = live.dir.join(format!("client.{}.json", pid.trim()));
                    let record: Value =
                        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
                    let resumes = record["argv"]
                        .as_array()?
                        .windows(2)
                        .any(|a| a[0] == "--resume" && a[1] == sid);
                    resumes.then_some(record)
                })
            });
            if let Some(record) = found {
                return record;
            }
            assert!(Instant::now() < end, "no client resumed {sid}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The fake client whose argv holds `word`: its `client.<pid>.json` record.
    fn client_with(live: &Live, word: &str) -> Value {
        let log = live.dir.join("clients.log");
        let end = Instant::now() + LONG;
        loop {
            let found = std::fs::read_to_string(&log).ok().and_then(|text| {
                text.lines().find_map(|pid| {
                    let path = live.dir.join(format!("client.{}.json", pid.trim()));
                    let record: Value =
                        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
                    record["argv"]
                        .as_array()?
                        .iter()
                        .any(|a| a == word)
                        .then_some(record)
                })
            });
            if let Some(record) = found {
                return record;
            }
            assert!(Instant::now() < end, "no client ran with {word}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    // options-6e E1 through the real CLI: an unknown flag makes no session; the leader runs the person's options,
    // then the session the recipe chose (never a selector the person gave), then the prompt.
    #[test]
    fn up_runs_the_leader_with_options_then_session_then_prompt() {
        for mux in Mux::ALL {
            let Some(mut live) = Live::human(mux) else {
                continue;
            };
            let sessions = |live: &Live| live.call(&["list-sessions", "-F", "#{session_id}"]);
            let before = sessions(&live);
            let (code, refused) =
                live.run(&["up", "-Client", "claude", "--", "fix", "--frobnicate"]);
            assert_eq!(
                (code, refused["check"].as_str()),
                (2, Some("usage")),
                "{mux:?} {refused}"
            );
            assert_eq!(
                sessions(&live),
                before,
                "{mux:?}: a refused up makes no session"
            );
            let argv = [
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
            ];
            let (code, up) = live.run(&argv);
            assert_eq!(code, 0, "{mux:?} {up}");
            let leader = client_with(&live, "fix");
            let sid = leader["sessionId"].as_str().unwrap().to_string();
            assert_eq!(
                leader["argv"],
                json!(["--model", "x", "--session-id", sid, "fix"]),
                "{mux:?}"
            );
        }
    }

    // options-6e live, unseeded: spawned with `-Model x`, its record written by `forward`; the first client is up and
    // its conversation exists before the stop (4to6 item 8); resumed: argv = the stored options, then `--resume <sid>`, no
    // task; the successor keeps the conversation id (4to6 item 11) and reloads it once (4to6 item 7); a fresh hc name.
    fn a_stopped_member_resumes_with_its_stored_options_only(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let w = live.spawn("w1", &["-Model", "x"]);
        let (sid, address) = (
            s(&w["sessionId"]).to_string(),
            s(&w["hcAddress"]).to_string(),
        );
        let first = strings(&client_with(&live, &sid)["argv"]).join(" ");
        assert!(
            first.contains("--model x"),
            "{mux:?}: started with --model x: {first}"
        );
        let conv = live.dir.join(format!("conversation.{sid}.log"));
        crate::ready::wait_conversation(live.dir.path(), &sid); // PM call 12
        let r = json_file(&live.agent_dir(s(&w["id"])).join("record.json"));
        let options: Vec<String> = strings(&r["options"])
            .iter()
            .map(|o| o.to_string())
            .collect();
        assert_eq!(r["lifecycle"], 1, "{mux:?}: written by forward, not seeded");
        assert!(r["prompt"].is_array(), "{mux:?} {r}");
        assert!(
            options.windows(2).any(|p| p[0] == "--model" && p[1] == "x"),
            "{mux:?}: --model x stored: {options:?}"
        );
        assert!(
            !options.iter().any(|o| o.contains("say hi")),
            "{mux:?}: the task is not an option"
        );
        let (code, stop) = live.run(&["stop", "w1"]);
        assert_eq!(code, 0, "{mux:?} {stop}");
        assert!(
            eventually(LONG, || core(&live, &["get", &address, "--json"])["state"]
                == "finished"),
            "{mux:?}"
        );
        let (code, resumed) = live.run(&["resume", "w1"]);
        assert_eq!(code, 0, "{mux:?} {resumed}");
        let y = json_file(
            &live
                .agent_dir(s(&resumed["agent"]["id"]))
                .join("record.json"),
        );
        assert_eq!(
            y["sessionId"],
            sid.as_str(),
            "{mux:?}: the successor keeps the conversation"
        );
        let client = resumed_client(&live, &sid);
        let mut want = options.clone();
        // notebook-7b: a resumed client's first text is the notebook news block.
        want.extend([
            "--resume".to_string(),
            sid.clone(),
            mux_agent::agents::notebook::news::HEAD.to_string(),
        ]);
        assert_eq!(
            client["argv"],
            json!(want),
            "{mux:?}: the stored options only"
        );
        assert_eq!(
            client["env"]["MUX_AGENTS_GEN"], "0",
            "{mux:?}: the successor's generation 0"
        );
        let lines: Vec<String> = std::fs::read_to_string(&conv)
            .unwrap()
            .lines()
            .map(|l| l.split(' ').nth(1).unwrap_or("").to_string())
            .collect();
        assert_eq!(
            lines,
            ["start", "resume"],
            "{mux:?}: started once, reloaded once"
        );
        let name = s(&y["hcName"]);
        let fresh = name.len() == 9
            && name.starts_with('m')
            && name[1..].bytes().all(|b| b.is_ascii_hexdigit());
        assert!(fresh, "{mux:?}: an m<8 hex> name: {name}");
        assert_ne!(y["hcName"], r["hcName"], "{mux:?}: a fresh hc name");
    }

    // Stop an agent, then resume it: the successor runs `claude <flags> --resume <its session>`, no task and no fork,
    // as the client of its own new hc terminal; its record links back with the same session; its view is tagged with
    // its id; and the session's bar stands.
    fn stop_then_resume_returns_with_its_conversation(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let w = live.spawn("w1", &[]);
        let sid = s(&w["sessionId"]).to_string();
        let address = s(&w["hcAddress"]).to_string();
        crate::ready::wait_conversation(live.dir.path(), &sid); // PM call 12
        let (code, stop) = live.run(&["stop", "w1"]);
        assert_eq!(code, 0, "{mux:?} {stop}");
        assert!(
            eventually(LONG, || core(&live, &["get", &address, "--json"])["state"]
                == "finished"),
            "{mux:?}"
        );
        let (code, resumed) = live.run(&["resume", "w1"]);
        assert_eq!(code, 0, "{mux:?} {resumed}");
        assert_eq!(resumed["from"], w["id"], "{mux:?}");
        let y = resumed["agent"].clone();
        assert_eq!(y["name"], "w1", "{mux:?}");
        assert_ne!(y["hcAddress"], w["hcAddress"], "{mux:?}");
        let client = resumed_client(&live, &sid);
        let argv = strings(&client["argv"]);
        let at = argv.iter().position(|a| *a == "--resume").unwrap();
        assert_eq!(argv[at + 1], sid, "{mux:?}");
        // notebook-7b V1: the only task after the conversation is the notebook block.
        assert_eq!(
            argv[at + 2..],
            [mux_agent::agents::notebook::news::HEAD],
            "{mux:?}: only the notebook block after the conversation: {argv:?}"
        );
        assert!(!argv.contains(&"--fork-session"), "{mux:?} {argv:?}");
        assert_eq!(client["exe"], "claude", "{mux:?}");
        let folder = live.agent_dir(s(&y["id"]));
        let record = json_file(&folder.join("record.json"));
        assert_eq!(record["resumedFrom"], w["id"], "{mux:?}");
        assert_eq!(record["sessionId"], sid.as_str(), "{mux:?}");
        // The client runs in Y's hc terminal: its parent is the launcher, the terminal's own process.
        let launcher = json_file(&live.gen_file(s(&y["id"]), "launcher.json"));
        assert_eq!(client["ppid"], launcher["pid"], "{mux:?}");
        assert_eq!(
            core(&live, &["get", s(&y["hcAddress"]), "--json"])["pid"],
            launcher["pid"],
            "{mux:?}"
        );
        let view = live.view_of(s(&y["id"]));
        let pane = s(&view["pane"]);
        assert_eq!(live.field(pane, "#{@agent-id}"), s(&y["id"]), "{mux:?}");
        // The bar is beside Y: a live bar in the window of Y's view (a bar per window).
        let bar = s(&resumed["sidebar"]).to_string();
        // `bars()` answers `<pane>|<window>|<width>`: the reply's pane is a labelled bar there, in Y's window.
        assert!(
            live.bars().iter().any(|b| {
                let mut fields = b.split('|');
                fields.next() == Some(bar.as_str()) && fields.next() == view["window"].as_str()
            }),
            "{mux:?} {resumed} {:?}",
            live.bars()
        );
        assert_eq!(
            live.field(&bar, "#{window_id}"),
            s(&view["window"]),
            "{mux:?} {resumed}"
        );
    }

    // occupy-3 H2 live: managed Copilot member w2 switches into w1's ended conversation (its fake client runs the
    // `report -Session` its sessionStart hook runs after `/resume`). Resuming w1 is refused while w2's newest report
    // names it, and admitted once w2 reports its own conversation again. w1's terminal is finished first, so w1's own
    // client is not the holder named.
    fn a_managed_copilot_resumed_into_a_conversation_holds_it(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let w1 = live.spawn("w1", &[]);
        let (code, w2) = live.try_spawn_client("copilot", "w2", &[]);
        assert_eq!(code, 0, "{mux:?} {w2}");
        let (sid1, sid2) = (
            s(&w1["sessionId"]).to_string(),
            s(&w2["sessionId"]).to_string(),
        );
        let address = s(&w1["hcAddress"]).to_string();
        let (code, stop) = live.run(&["stop", "w1"]);
        assert_eq!(code, 0, "{mux:?} {stop}");
        assert!(
            eventually(LONG, || core(&live, &["get", &address, "--json"])["state"]
                == "finished"),
            "{mux:?}"
        );
        let pid = crate::native::client_at(live.dir.path(), s(&w2["hcAddress"]));
        crate::native::startup_report(live.dir.path(), pid);
        let line = |sid: &str| format!("report\t-Session\t{sid}\t-Source\tresume");
        let r = crate::native::client_cmd(live.dir.path(), pid, &line(&sid1));
        assert_eq!(
            r["client"]["pid"], pid,
            "{mux:?} the walk found w2's own client: {r}"
        );
        let (code, refused) = live.run(&["resume", "w1"]);
        assert_eq!(
            (code, &refused["check"]),
            (2, &json!("session")),
            "{mux:?} {refused}"
        );
        // PM call 2: w2's client, its image, and the managed rule.
        let message = s(&refused["message"]);
        assert!(
            message.starts_with(&format!("process {pid} (copilot)")),
            "{mux:?} {message}"
        );
        assert!(
            message.contains(mux_agent::agents::occupancy::Rule::Managed.phrase()),
            "{mux:?} {message}"
        );
        crate::native::client_cmd(live.dir.path(), pid, &line(&sid2));
        let (code, resumed) = live.run(&["resume", "w1"]);
        assert_eq!(code, 0, "{mux:?} {resumed}");
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
            &format!("touch '{}'; exec claude", marker.display()),
        ]);
        marker
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
    /// A peek file left in the state folder (no client ran the popup that deletes it).
    fn peek_file(live: &Live) -> Option<String> {
        std::fs::read_dir(live.dir.join("state"))
            .ok()?
            .flatten()
            .flat_map(|d| std::fs::read_dir(d.path()).into_iter().flatten().flatten())
            .find(|f| f.file_name().to_string_lossy().starts_with("peek-"))
            .and_then(|f| std::fs::read_to_string(f.path()).ok())
    }
    /// Terminal bytes as text, escape sequences and blanks removed: a popup's text however it was drawn.
    fn visible_text(bytes: &[u8]) -> String {
        let bytes = String::from_utf8_lossy(bytes).into_owned();
        let (mut out, mut chars) = (String::new(), bytes.chars());
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                if chars.next() == Some('[') {
                    for d in chars.by_ref() {
                        if ('@'..='~').contains(&d) {
                            break;
                        }
                    }
                }
            } else if !c.is_whitespace() {
                out.push(c);
            }
        }
        out
    }
    /// Whether text a client was sent after a checkpoint is grand's output popup: its pager footer and grand's marker,
    /// both drawn after the checkpoint (review: grand's own view, drawn before it, never counts).
    fn popup_shows(after: &str, marker: &str) -> bool {
        after.contains("--peek:grand(qcloses)--") && after.contains(marker)
    }
    // The popup check needs no multiplexer: a popup without the retained text, or the marker seen only in grand's
    // own view before the checkpoint, fails it.
    #[test]
    fn popup_check_requires_the_marker_drawn_after_the_checkpoint() {
        let marker = "grand-said-7";
        let stream = format!("\u{1b}[2J {marker} \u{1b}[H").into_bytes();
        let at = stream.len();
        let mut popup = stream.clone();
        popup.extend_from_slice("\u{1b}[1;1H-- peek: grand (q closes) --".as_bytes());
        assert!(
            !popup_shows(&visible_text(&popup[at..]), marker),
            "a popup without the retained text fails"
        );
        let mut shown = stream;
        shown.extend_from_slice(
            format!("\u{1b}[3;1H{marker}\r\n-- peek: grand (q closes) --").as_bytes(),
        );
        assert!(
            popup_shows(&visible_text(&shown[at..]), marker),
            "the popup that pages the marker passes"
        );
        assert!(
            !popup_shows(&visible_text(&shown[..at]), marker),
            "grand's own view alone never passes"
        );
    }
    #[test]
    fn team_bar_draws_the_tree_and_opens_any_row() {
        for mux in Mux::ALL {
            let Some(mut live) = Live::human(mux) else {
                continue;
            };
            live.assert_not_ascii();
            let tmux = !matches!(mux, Mux::Rmux);
            let client = tmux.then(|| {
                let client =
                    Client::attach(&live, &live.field(live.server.pane(), "#{session_id}"));
                live.server.wait("the client to attach", || {
                    (!live.call(&["list-clients"]).trim().is_empty()).then_some(())
                });
                client
            });
            let (code, up) = live.run(&["up", "-Client", "claude"]);
            assert_eq!(code, 0, "{mux:?} {up}");
            let session = s(&up["session"]).to_string();
            // V1: the leader's view is in view.json, never in its lifecycle record (a3b call 42, TL F2).
            let lead = live.view_of(s(&up["leader"]["id"]));
            // Members pass a brief (phase 3: a member without one is refused `brief`); `spawn_by` sends -BriefFile and -Context brief.
            std::fs::write(live.dir.join("brief.md"), "brief").unwrap();
            let spawn = |live: &mut Live, by: &str, name: &str| {
                let (code, reply) = live.spawn_by(by, name);
                assert_eq!(code, 0, "{mux:?} spawn {name}: {reply}");
                reply
            };
            let child = spawn(&mut live, s(&up["leader"]["id"]), "child");
            let child_pid = live.client_of(s(&child["id"]));
            let grand = spawn(&mut live, s(&child["id"]), "grand");
            let r = json_file(&live.agent_dir(s(&grand["id"])).join("record.json"));
            assert_eq!(
                (&r["parentAgent"]["id"], &r["depth"]),
                (&child["id"], &json!(2)),
                "{mux:?}"
            );
            let views = [
                (s(&lead["pane"]).to_string(), "leader"),
                (s(&child["pane"]).to_string(), "child"),
                (s(&grand["pane"]).to_string(), "grand"),
            ];
            let bars = [
                s(&up["sidebar"]).to_string(),
                s(&child["sidebar"]).to_string(),
                s(&grand["sidebar"]).to_string(),
            ];
            for ((view, _), bar) in views.iter().zip(&bars) {
                // B9: every view window has its own bar
                assert_eq!(
                    live.field(bar, "#{window_id}"),
                    live.field(view, "#{window_id}"),
                    "{mux:?}"
                );
            }
            let check = |live: &Live| {
                let shown = live.field(&session, "#{pane_id}");
                let marked = views
                    .iter()
                    .find(|(p, _)| tmux && *p == shown)
                    .map(|(_, n)| *n);
                for bar in &bars {
                    let mut screen = String::new();
                    let ok = eventually(Duration::from_secs(10), || {
                        screen = live.call(&["capture-pane", "-p", "-t", bar]);
                        tree_rows(&screen, &["leader", "child", "grand"]).is_some_and(|r| {
                            r[0].0 < r[1].0
                                && r[1].0 < r[2].0
                                && r.iter().map(|x| x.2).eq([1, 3, 5])
                                && r.iter()
                                    .zip(["leader", "child", "grand"])
                                    .all(|(x, n)| (x.1 == '\u{25cf}') == (marked == Some(n)))
                        })
                    });
                    assert!(ok, "{mux:?} {bar}, shown {shown}:\n{screen}");
                }
            };
            check(&live);
            live.call(&["send-keys", "-t", &bars[0], "-l", "jjj"]);
            live.call(&["send-keys", "-t", &bars[0], "Enter"]);
            assert!(
                eventually(Duration::from_secs(10), || live
                    .field(&session, "#{pane_id}")
                    == views[2].0),
                "{mux:?}: j to grand, Enter"
            );
            check(&live); // `●` follows to grand on tmux
            // Grand prints a marker on its own terminal (the fake client prints nothing else), and hc keeps it before grand
            // stops: the output the popup must show. One word, as `Client::text` drops blanks.
            let marker = format!("grand-said-{}", std::process::id());
            let grand_pid = live.client_of(s(&grand["id"]));
            let (code, out) = live.server.command(grand_pid, &["@say", &marker]);
            assert_eq!(code, 0, "{mux:?} @say: {out}");
            let read = |live: &Live| {
                let hc = live.server.core.as_ref().unwrap();
                String::from_utf8(
                    crate::support::bounded::output(
                        &mut hc.command(&["read", s(&grand["hcAddress"]), "--lines", "200"]),
                        crate::support::bounded::LIMIT,
                    )
                    .unwrap()
                    .stdout,
                )
                .unwrap()
            };
            assert!(
                eventually(Duration::from_secs(10), || read(&live).contains(&marker)),
                "{mux:?}: hc keeps grand's marker: {:?}",
                read(&live)
            );
            // Stopped, grand stays in place with its state word, dimmed where the capture carries SGR (tmux: always).
            // o2 call 35: grand is ready before its stop, so the bar shows `stopped`, not `recover`.
            live.ready(&grand);
            let (code, out) = live.server.command(child_pid, &["stop", "grand"]);
            assert_eq!(code, 0, "{mux:?} {out}");
            let mut line = String::new();
            assert!(
                eventually(Duration::from_secs(15), || {
                    line = live
                        .call(&["capture-pane", "-e", "-p", "-t", &bars[0]])
                        .lines()
                        .find(|l| l.contains("grand"))
                        .unwrap_or_default()
                        .to_string();
                    line.contains("stopped")
                }),
                "{mux:?}: {line:?}"
            );
            let dim = line.split("\u{1b}[").skip(1).any(|p| {
                p.split("m")
                    .next()
                    .unwrap_or("")
                    .split(";")
                    .any(|n| n == "2")
            });
            assert!(
                dim || (!tmux && !line.contains("\u{1b}")),
                "{mux:?}: the ended row is dimmed: {line:?}"
            );
            let plain = live.call(&["capture-pane", "-p", "-t", &bars[0]]);
            assert!(
                tree_rows(&plain, &["leader", "child", "grand"])
                    .is_some_and(|r| r[1].0 < r[2].0 && r[2].2 == 5),
                "{mux:?}: in place\n{plain}"
            );
            // Harness prerequisite: the bar's own popup command, run as the bar runs it (`run-shell -b -C`) with no client, must
            // answer 0 and leave its peek file; else the bar deletes the file unseen and the output check below fails for another reason.
            if client.is_none() {
                let file = live.dir.join("peek-probe.txt");
                std::fs::write(&file, "probe").unwrap();
                let command = mux_agent::sidebar::popup_command(&bars[0], file.to_str().unwrap());
                let probe = live.server.try_call(&["run-shell", "-b", "-C", &command]);
                std::thread::sleep(Duration::from_secs(1));
                assert!(
                    probe.status.success() && file.exists(),
                    "{mux:?}: the popup command without a client: {probe:?}"
                );
                std::fs::remove_file(&file).unwrap();
            }
            // Opening the ended grand shows its retained output (`hc read`, plain text); nothing restarts.
            let kept = read(&live);
            assert!(
                kept.contains(&marker),
                "{mux:?}: the stopped grand keeps its marker: {kept:?}"
            );
            let last = kept
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .expect("grand kept output")
                .replace(" ", "");
            // review: grand's own view drew the marker, so the client switches away from grand to the leader's view,
            // waits until it has redrawn and gone quiet, and takes a checkpoint: only what it is sent after it can show
            // the popup, and before the popup opens grand's marker must not be among it.
            let from = client.as_ref().map(|client| {
                live.call(&["select-window", "-t", &views[0].0]);
                live.call(&["select-pane", "-t", &views[0].0]);
                assert!(
                    eventually(Duration::from_secs(10), || live
                        .field(&session, "#{pane_id}")
                        == views[0].0),
                    "{mux:?}: the leader's view is shown"
                );
                let quiet = eventually(Duration::from_secs(10), || {
                    let before = client.mark();
                    std::thread::sleep(Duration::from_millis(500));
                    client.mark() == before
                });
                assert!(quiet, "{mux:?}: the client went quiet on the leader's view");
                let from = client.mark();
                std::thread::sleep(Duration::from_secs(1));
                let early = client.text_since(from);
                assert!(
                    !early.contains(&marker),
                    "{mux:?}: grand's marker before the popup: {early:?}"
                );
                from
            });
            live.call(&["send-keys", "-t", &bars[0], "-l", "jjj"]);
            live.call(&["send-keys", "-t", &bars[0], "Enter"]);
            match (&client, from) {
                (Some(client), Some(from)) => {
                    let server: u64 = live.field(live.server.pane(), "#{pid}").parse().unwrap();
                    live.server
                        .wait("the output popup", || pagers_under(server).then_some(()));
                    let shows = || {
                        let t = client.text_since(from);
                        popup_shows(&t, &marker) && t.contains(&last)
                    };
                    assert!(
                        eventually(Duration::from_secs(10), shows),
                        "{mux:?}: the popup pages grand's output, drawn after the checkpoint: {:?}",
                        client.text_since(from)
                    );
                }
                _ => {
                    let mut text = None;
                    assert!(
                        eventually(Duration::from_secs(10), || {
                            text = peek_file(&live);
                            text.is_some()
                        }),
                        "{mux:?}"
                    );
                    let text = text.unwrap();
                    assert!(
                        text.contains(&marker),
                        "{mux:?}: the peek file holds grand's marker: {text:?}"
                    );
                    assert_eq!(
                        text,
                        format!("{kept}\n-- peek: grand (q closes) --\n"),
                        "{mux:?}"
                    );
                }
            }
            let screen = live.call(&["capture-pane", "-p", "-t", &bars[0]]);
            assert!(
                !screen.contains("focus failed") && !screen.contains("nothing to open"),
                "{mux:?}: no popup error\n{screen}"
            );
            assert_eq!(
                core(&live, &["get", s(&grand["hcAddress"]), "--json"])["state"],
                "finished",
                "{mux:?}: opening restarts nothing"
            );
            drop(client);
        }
    }

    #[test]
    fn up_creates_a_team_in_a_new_session() {
        for mux in Mux::ALL {
            let Some(mut live) = Live::human(mux) else {
                continue;
            };
            let marker = hostile(&live);
            // A real client on the person's session (tmux: the pty harness runs there), so switch-client has one to
            // move; rmux runs without one and is held to the other warnings.
            let client = (!matches!(mux, Mux::Rmux)).then(|| {
                let root = live.field(live.server.pane(), "#{session_id}");
                let client = Client::attach(&live, &root);
                live.server.wait("the client to attach", || {
                    (!live.call(&["list-clients"]).trim().is_empty()).then_some(())
                });
                client
            });
            let (code, up) = live.run(&["up", "-Client", "claude"]);
            assert_eq!(code, 0, "{mux:?} {up}");
            assert!(
                up.get("sidebarError").is_none() && up.get("teamRecordError").is_none(),
                "{mux:?} {up}"
            );
            let session = up["session"].as_str().unwrap().to_string();
            if client.is_some() {
                assert!(up.get("layoutError").is_none(), "{mux:?} {up}");
                let name = up["name"].as_str().unwrap();
                assert!(
                    eventually(Duration::from_secs(10), || live
                        .call(&["list-clients", "-F", "#{client_session}"])
                        .trim()
                        == name),
                    "{mux:?}: the client did not switch to {name}"
                );
            }
            let core = live.server.core.as_ref().unwrap();
            let get = support::private_core::run(
                core.command(&["get", up["leader"]["hcAddress"].as_str().unwrap(), "--json"]),
                live.dir.path(),
            )
            .unwrap();
            assert_eq!(get["state"], "live", "{mux:?}");
            assert!(up["sidebar"].is_string(), "{mux:?} {up}");
            assert!(
                eventually(Duration::from_secs(10), || live
                    .call(&[
                        "list-windows",
                        "-t",
                        &session,
                        "-F",
                        "#{window_name}|#{window_active}"
                    ])
                    .trim()
                    == "leader|1"),
                "{mux:?}"
            );
            assert!(!marker.exists(), "{mux:?}");
            // 6b ambiguity 8 (r6b call 7): a second `up` reconciles the team; nothing is lost, nothing restored.
            let (code, again) = live.run(&["up", "-Client", "claude"]);
            // reentry-6a Task 9: it re-enters the team the index records.
            assert_eq!(
                (
                    code,
                    &again["created"],
                    &again["reentered"],
                    &again["restored"]
                ),
                (0, &json!(false), &json!(true), &json!([])),
                "{mux:?} {again}"
            );
        }
    }

    #[test]
    fn two_servers_racing_up_in_one_folder_launch_one_leader() {
        for mux in Mux::ALL {
            let (Some(a), Some(b)) = (Live::human(mux), Live::human(mux)) else {
                continue;
            };
            let (state, work, go, bin) = (
                a.state_text(),
                a.dir.join("work"),
                a.dir.join("go"),
                env!("CARGO_BIN_EXE_mux-agent"),
            );
            std::fs::create_dir_all(&work).unwrap();
            for (n, live) in [&a, &b].into_iter().enumerate() {
                let pane = live
                    .call(&["list-panes", "-a", "-F", "#{pane_id}"])
                    .lines()
                    .next()
                    .unwrap()
                    .to_string();
                let line = format!(
                    "cd '{}' && while [ ! -e '{}' ]; do sleep 0.01; done; MUX_AGENTS_STATE='{state}' '{bin}' up -Client claude > '{}'; echo $? > '{}'",
                    work.display(),
                    go.display(),
                    a.dir.join(format!("out{n}")).display(),
                    a.dir.join(format!("done{n}")).display()
                );
                live.call(&["send-keys", "-t", &pane, "-l", &line]);
                live.call(&["send-keys", "-t", &pane, "Enter"]);
            }
            std::fs::write(&go, "").unwrap();
            assert!(
                eventually(Duration::from_secs(60), || (0..2)
                    .all(|n| a.dir.join(format!("done{n}")).exists())),
                "{mux:?}"
            );
            let replies: Vec<Value> = (0..2)
                .map(|n| {
                    serde_json::from_str(
                        &std::fs::read_to_string(a.dir.join(format!("out{n}"))).unwrap(),
                    )
                    .unwrap()
                })
                .collect();
            // 6b ambiguity 8 (r6b call 7): both succeed; the later one reconciles the team the first made.
            assert_eq!(
                replies.iter().filter(|r| r["ok"] == true).count(),
                2,
                "{mux:?} {replies:?}"
            );
            assert_eq!(
                replies.iter().filter(|r| r["created"] == false).count(),
                1,
                "{mux:?}: exactly one creates"
            );
            // reentry-6a Task 9: the later one re-enters the team the index records.
            assert!(
                replies
                    .iter()
                    .filter(|r| r["created"] == false)
                    .all(|r| r["reentered"] == true),
                "{mux:?} {replies:?}"
            );
            let t = replies.iter().find(|r| r["ok"] == true).unwrap()["team"]
                .as_str()
                .unwrap()
                .to_string();
            let leaders: usize = [&a, &b]
                .iter()
                .map(|l| {
                    support::private_core::run(
                        l.server.core.as_ref().unwrap().command(&[
                            "ls",
                            "--all",
                            "--label",
                            "mux.managed=1",
                            "--label",
                            &format!("mux.up={t}"),
                            "--json",
                        ]),
                        l.dir.path(),
                    )
                    .unwrap()["terminals"]
                        .as_array()
                        .map_or(0, Vec::len)
                })
                .sum();
            assert_eq!(
                leaders, 1,
                "{mux:?}: exactly one mux.up={t} leader terminal across both private cores"
            );
        }
    }

    /// Every file below `dir`, path to bytes (6b-R2: a byte-identical folder).
    fn tree_bytes(dir: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        let mut all = std::collections::BTreeMap::new();
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                all.extend(tree_bytes(&p));
            } else {
                all.insert(p.clone(), std::fs::read(&p).unwrap());
            }
        }
        all
    }

    /// restore-6b: a team made by `up`: the leader, then A (claude) and B (copilot) spawned by the leader, each told
    /// its marker; each first generation ready. Its ids: leader, A, B.
    fn team_of_three(live: &mut Live) -> [String; 3] {
        let mux = live.mux();
        let (code, up) = live.run(&["up", "-Client", "claude"]);
        assert_eq!(code, 0, "{mux:?} {up}");
        let lead = s(&up["leader"]["id"]).to_string();
        std::fs::write(live.dir.join("brief.md"), "brief").unwrap();
        let brief = live.path("brief.md");
        let mut ids = vec![lead.clone()];
        for (client, name, marker) in [("claude", "a", "MARKER-A"), ("copilot", "b", "MARKER-B")] {
            let task = live.path(&format!("task-{name}.txt"));
            std::fs::write(&task, format!("say {marker}")).unwrap();
            let args = [
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
            ];
            let (code, r) = live.by(&lead, &args);
            assert_eq!(code, 0, "{mux:?} spawn {name}: {r}");
            ids.push(s(&r["id"]).to_string());
        }
        for id in &ids {
            let ready = live.agent_dir(id).join("gens/0/ready.json");
            live.server
                .wait(&format!("{id} ready"), || ready.is_file().then_some(()));
        }
        [ids[0].clone(), ids[1].clone(), ids[2].clone()]
    }

    /// restore-6b ambiguity 14 (r6b-calls "how a live test produces a Lost member"): kills the core (`kill -9`) and each
    /// target member's terminal (its client and its `hold`, by pid) while the core is down, restarts the core, and waits
    /// until hc shows every member terminal Lost (a live shell, not its launcher argv). A core kill alone loses nothing:
    /// hc's `hold` keeps the terminal and the restarted core adopts it (`a_core_kill_alone_leaves_members_running`).
    fn lose_the_core(live: &mut Live, ids: &[String]) {
        let hc = live.server.core.as_ref().expect("private core");
        let terminals: Vec<(u32, Option<i64>)> = ids
            .iter()
            .flat_map(|id| hc.terminal_of(live.client_of(id) as u32))
            .collect();
        live.server
            .core
            .as_mut()
            .expect("private core")
            .kill_hard_with(&terminals);
        let live: &Live = live;
        for id in ids {
            let r = json_file(&live.agent_dir(id).join("record.json"));
            let (address, argv) = (s(&r["hcAddress"]).to_string(), r["launcherArgv"].clone());
            live.server.wait(&format!("{id} lost"), || {
                let t = core(live, &["get", &address, "--json"]);
                (t["state"] == "live" && t["argv"] != argv).then_some(())
            });
        }
    }

    /// restore-6b spec live "Abrupt loss", "Ready after restore", "Ended"; coordinator (p1c call 6). Public commands
    /// only: `up`, member `spawn`, member `stop`, `list`; the loss is `kill -9` of the hc core. Predecessor client
    /// pids are captured before the kill and every later command goes to the successors (r6b call 9).
    #[test]
    fn restore_resumes_every_lost_member_after_a_core_kill() {
        for mux in Mux::ALL {
            let Some(mut live) = Live::human(mux) else {
                continue;
            };
            let ids = team_of_three(&mut live);
            let before: Vec<Value> = ids
                .iter()
                .map(|id| json_file(&live.agent_dir(id).join("record.json")))
                .collect();
            let old: Vec<u64> = ids.iter().map(|id| live.client_of(id)).collect();
            let pre = live.agent_dir("999999999999");
            std::fs::create_dir_all(&pre).unwrap();
            std::fs::write(
                pre.join("record.json"),
                r#"{"id":"999999999999","name":"old","hcAddress":"t9"}"#,
            )
            .unwrap();
            let pre_bytes = tree_bytes(&pre);
            lose_the_core(&mut live, &ids);
            let (code, up) = live.run(&["up", "-Client", "claude"]);
            assert_eq!((code, &up["created"]), (0, &json!(false)), "{mux:?} {up}");
            let mut restored: Vec<&str> = up["restored"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| s(&e["id"]))
                .collect();
            restored.sort();
            let mut want: Vec<&str> = ids.iter().map(String::as_str).collect();
            want.sort();
            assert_eq!(restored, want, "{mux:?}: every lost member restored");
            let mut news = Vec::new();
            for (i, id) in ids.iter().enumerate() {
                let r = json_file(&live.agent_dir(id).join("record.json"));
                assert_eq!(
                    (&r["generation"], &r["sessionId"]),
                    (&json!("1"), &before[i]["sessionId"]),
                    "{mux:?} {id}"
                );
                assert_ne!(
                    r["hcName"], before[i]["hcName"],
                    "{mux:?} {id}: a new terminal"
                );
                let new = live.successor(id, s(&r["sessionId"]), old[i]);
                news.push(new);
                let c = live.client(new);
                let argv: Vec<&str> = strings(&c["argv"]);
                assert!(
                    argv.windows(2)
                        .any(|w| w == ["--resume", s(&r["sessionId"])]),
                    "{mux:?} {argv:?}"
                );
                if i > 0 {
                    let marker = if i == 1 { "MARKER-A" } else { "MARKER-B" };
                    assert!(
                        !argv.iter().any(|a| a.contains(marker)),
                        "{mux:?}: the task is never resent"
                    );
                    assert!(
                        c["recalled"].to_string().contains(marker),
                        "{mux:?}: the same conversation"
                    );
                }
                let ready = live.agent_dir(id).join("gens/1/ready.json");
                live.server.wait(&format!("{id} ready again"), || {
                    ready.is_file().then_some(())
                });
                let kept = core(&live, &["get", s(&before[i]["hcAddress"]), "--json"]);
                let mut keys: Vec<&str> = kept["labels"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                keys.sort();
                assert_eq!(
                    keys,
                    ["mux.kept", "mux.keptName"],
                    "{mux:?}: the old shell keeps only kept labels"
                );
            }
            let (code, listed) = live.server.command(news[0], &["list"]);
            assert_eq!(code, 0, "{mux:?} {listed}");
            for id in &ids {
                let outcome = json_file(&live.agent_dir(id).join("gens/1/outcome.json"));
                assert_eq!(outcome["outcome"], "ok", "{mux:?} {id}");
            }
            let clients = std::fs::read_to_string(live.dir.join("clients.log"))
                .unwrap()
                .lines()
                .count();
            let (_, again) = live.run(&["up", "-Client", "claude"]);
            assert_eq!(
                again["restored"],
                json!([]),
                "{mux:?}: a second up starts nothing"
            );
            let now = std::fs::read_to_string(live.dir.join("clients.log"))
                .unwrap()
                .lines()
                .count();
            assert_eq!(now, clients, "{mux:?}");
            assert_eq!(
                tree_bytes(&pre),
                pre_bytes,
                "{mux:?}: the pre-lifecycle folder is byte-identical"
            );
            // Ended: A quits on its own; no up brings it back.
            let a_pid = news[1];
            support::write_atomic(&live.dir.join(format!("quit.{a_pid}")), "0");
            live.server.wait("A exited", || {
                live.dir
                    .join(format!("exited.{a_pid}"))
                    .is_file()
                    .then_some(())
            });
            live.server.command(news[0], &["list"]);
            // Stopped: the leader stops B.
            let (code, stopped) = live.server.command(news[0], &["stop", "b"]);
            assert_eq!(code, 0, "{mux:?} {stopped}");
            let (_, third) = live.run(&["up", "-Client", "claude"]);
            assert_eq!(
                third["restored"],
                json!([]),
                "{mux:?}: ended and stopped stay down"
            );
            for id in &ids[1..] {
                let r = json_file(&live.agent_dir(id).join("record.json"));
                assert_eq!(r["generation"], "1", "{mux:?}");
            }
        }
    }

    /// r6b-calls "how a live test produces a Lost member": a core kill alone loses no member. hc's `hold` keeps each
    /// terminal and the restarted core adopts it live with its launcher argv; `up`, run from the harness's own pane,
    /// then finds every member Running and relaunches nothing; each member stays live with the same client process
    /// and terminal. The team's view session may end with the core (all-calls call 10): reported, never asserted.
    #[test]
    fn a_core_kill_alone_leaves_members_running() {
        let count = |live: &Live| {
            std::fs::read_to_string(live.dir.join("clients.log"))
                .unwrap()
                .lines()
                .count()
        };
        for mux in Mux::ALL {
            let Some(mut live) = Live::human(mux) else {
                continue;
            };
            let ids = team_of_three(&mut live);
            let before: Vec<Value> = ids
                .iter()
                .map(|id| json_file(&live.agent_dir(id).join("record.json")))
                .collect();
            // Each member's client pid with its start, so a reused pid never passes for the same client.
            let old: Vec<(u64, i64)> = ids
                .iter()
                .map(|id| {
                    let pid = live.client_of(id);
                    let start = mux_agent::proc::host()
                        .open(pid as u32)
                        .map(|o| o.start)
                        .unwrap_or_else(|| panic!("{mux:?} {id}: client {pid} is not running"));
                    (pid, start)
                })
                .collect();
            let launched = count(&live);
            live.server.core.as_mut().expect("private core").kill_hard();
            for r in &before {
                let address = s(&r["hcAddress"]).to_string();
                live.server.wait(&format!("{address} adopted"), || {
                    let t = core(&live, &["get", &address, "--json"]);
                    (t["state"] == "live" && t["argv"] == r["launcherArgv"]).then_some(())
                });
            }
            // all-calls call 10: everything below runs from the harness's own pane; the team's view session is not
            // used (views run `hc attach` and may end with the core; `up` restoring them is reentry-6a Task 11).
            let (code, up) = live.run(&["up", "-Client", "claude"]);
            // all-calls call 9: what hc answered for each member right after `up`, carried by every failure below.
            let seen: Vec<(String, Option<Value>)> = before
                .iter()
                .map(|r| {
                    let address = s(&r["hcAddress"]);
                    let core = live.server.core.as_ref().expect("private core");
                    let got = support::private_core::run(
                        core.command(&["get", address, "--json"]),
                        live.dir.path(),
                    );
                    (format!("{} {address}: {got:?}", s(&r["id"])), got.ok())
                })
                .collect();
            let evidence: Vec<&String> = seen.iter().map(|(e, _)| e).collect();
            // A diagnostic only (call 10): whether the team's view session outlived the core kill.
            let sid = s(&before[0]["session"]);
            let sessions = live
                .server
                .try_call(&["list-sessions", "-F", "#{session_id}"]);
            let sessions = String::from_utf8_lossy(&sessions.stdout).to_string();
            if !sessions.lines().any(|l| l == sid) {
                eprintln!(
                    "{mux:?}: the team's view session {sid} ended with the core (by design): {sessions:?}"
                );
            }
            assert_eq!(
                (code, &up["created"]),
                (0, &json!(false)),
                "{mux:?} {up} hc: {evidence:#?}"
            );
            for key in ["restored", "held", "unknown", "recover"] {
                assert_eq!(
                    up[key],
                    json!([]),
                    "{mux:?} {key}: up: {up} hc: {evidence:#?}"
                );
            }
            // Re-added with reentry-6a (call 10): `up` brought the views back, so a person's shell in its session runs
            // `restore`, which also finds every member Running.
            let shell = team_shell(&live, s(&up["session"]));
            let (code, restore) = live.run_in(&shell, &["restore"]);
            assert_eq!(code, 0, "{mux:?} {restore} hc: {evidence:#?}");
            for key in ["restored", "held", "unknown", "recover"] {
                assert_eq!(
                    restore[key],
                    json!([]),
                    "{mux:?} {key}: restore: {restore} up: {up} hc: {evidence:#?}"
                );
            }
            for (i, id) in ids.iter().enumerate() {
                let t = seen[i].1.as_ref();
                assert!(
                    t.is_some_and(
                        |t| t["state"] == "live" && t["argv"] == before[i]["launcherArgv"]
                    ),
                    "{mux:?} {id}: live with its launcher after up: {evidence:#?}"
                );
                assert_eq!(
                    mux_agent::proc::host()
                        .open(old[i].0 as u32)
                        .map(|o| o.start),
                    Some(old[i].1),
                    "{mux:?} {id}: the same client process {} still runs",
                    old[i].0
                );
                assert_eq!(
                    live.client_of(id),
                    old[i].0,
                    "{mux:?} {id}: the same client"
                );
                let r = json_file(&live.agent_dir(id).join("record.json"));
                assert_eq!(
                    r["hcName"], before[i]["hcName"],
                    "{mux:?} {id}: the same terminal"
                );
                assert!(
                    !live.agent_dir(id).join("gens/1").exists(),
                    "{mux:?} {id}: no new generation"
                );
            }
            assert_eq!(count(&live), launched, "{mux:?}: nothing relaunched");
        }
    }

    /// restore-6b spec live "Managed vs unmanaged" (moved from occupy-3). Clients this team did not start hold only the
    /// conversations they are on: a Claude and a Copilot on unrelated conversations hold nothing; a Claude resumed into
    /// A's conversation holds A and names its pid, a Copilot resumed into B's holds B, and each quit frees its member.
    #[test]
    fn restore_resume_is_held_by_an_unmanaged_client_until_it_quits() {
        for mux in Mux::ALL {
            let Some(mut live) = Live::human(mux) else {
                continue;
            };
            let ids = team_of_three(&mut live);
            lose_the_core(&mut live, &ids);
            let sid = |id: &str| {
                s(&json_file(&live.agent_dir(id).join("record.json"))["sessionId"]).to_string()
            };
            let (a_sid, b_sid) = (sid(&ids[1]), sid(&ids[2]));
            // Unrelated clients: another Claude Code and another Copilot session on this machine hold nothing.
            for (client, other) in [
                ("claude", "5e55fc2f-2032-48ef-a920-4af2a8e5af55"),
                ("copilot", "6e66fc2f-2032-48ef-a920-4af2a8e5af56"),
            ] {
                let pane = live.session_in(&format!("other-{client}"), &live.dir.join("work"));
                live.server
                    .send_keys_to(&pane, &format!("exec {client} --session-id {other}"));
                live.server.client_by_session(other);
            }
            // Strays resumed into A's and B's conversations hold exactly those.
            let mut strays = Vec::new();
            for (client, on) in [("claude", &a_sid), ("copilot", &b_sid)] {
                let pane = live.session_in(&format!("stray-{client}"), &live.dir.join("work"));
                live.server
                    .send_keys_to(&pane, &format!("exec {client} --resume {on}"));
                let pid = live.server.wait(&format!("stray {client} on {on}"), || {
                    let log = std::fs::read_to_string(live.dir.join("clients.log")).ok()?;
                    log.lines().rev().find_map(|p| {
                        let r: Value = serde_json::from_str(
                            &std::fs::read_to_string(
                                live.dir.join(format!("client.{}.json", p.trim())),
                            )
                            .ok()?,
                        )
                        .ok()?;
                        (r["argv"].as_array()?.iter().any(|a| a == on.as_str()))
                            .then(|| p.trim().parse::<u64>().ok())?
                    })
                });
                strays.push(pid);
            }
            let (code, up) = live.run(&["up", "-Client", "claude"]);
            assert_eq!(code, 0, "{mux:?} {up}");
            let restored: Vec<&str> = up["restored"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| s(&e["id"]))
                .collect();
            assert_eq!(
                restored,
                [ids[0].as_str()],
                "{mux:?}: only the leader is free {up}"
            );
            let held = up["held"].as_array().unwrap();
            assert_eq!(held.len(), 2, "{mux:?} {up}");
            for h in held {
                assert_eq!(h["reason"], "occupied", "{mux:?} {h}");
                let pid = strays[if h["id"] == json!(ids[1]) { 0 } else { 1 }];
                assert!(s(&h["holder"]).contains(&pid.to_string()), "{mux:?} {h}");
            }
            support::write_atomic(&live.dir.join(format!("quit.{}", strays[0])), "0");
            live.server.wait("stray claude gone", || {
                live.dir
                    .join(format!("exited.{}", strays[0]))
                    .is_file()
                    .then_some(())
            });
            let (_, second) = live.run(&["up", "-Client", "claude"]);
            assert_eq!(
                second["restored"][0]["id"],
                json!(ids[1]),
                "{mux:?}: A is free once its stray quit {second}"
            );
            assert_eq!(
                second["held"][0]["id"],
                json!(ids[2]),
                "{mux:?}: the Copilot stray still holds B"
            );
            support::write_atomic(&live.dir.join(format!("quit.{}", strays[1])), "0");
            live.server.wait("stray copilot gone", || {
                live.dir
                    .join(format!("exited.{}", strays[1]))
                    .is_file()
                    .then_some(())
            });
            let (_, third) = live.run(&["up", "-Client", "claude"]);
            assert_eq!(third["restored"][0]["id"], json!(ids[2]), "{mux:?}");
        }
    }

    #[cfg(feature = "test-panic")]
    #[test]
    fn up_whose_view_panics_keeps_the_leader_under_hostile_settings() {
        for mux in Mux::ALL {
            let Some(mut live) = Live::human(mux) else {
                continue;
            };
            let (marker, state) = (hostile(&live), live.state_text());
            let (code, out) = live.server.run(
                &["up", "-Client", "claude"],
                &[
                    ("MUX_AGENTS_STATE", state.as_str()),
                    ("MUX_AGENTS_TEST_PANIC", "launch"),
                ],
            );
            // The panic fires inside the view step, after the commit: up succeeds with viewError (PM calls 1, 10).
            assert_eq!(code, 0, "{mux:?} {out}");
            assert!(s(&out["viewError"]).contains("panicked"), "{mux:?} {out}");
            assert!(!marker.exists(), "{mux:?}");
            let leader = s(&out["leader"]["id"]).to_string();
            assert!(
                live.gen_file(&leader, "commit.json").is_file(),
                "{mux:?}: the leader is committed"
            );
            live.proof_of(&leader);
            // PM call 16 (call 10: views need not survive; up restores them): the views may be gone, the leader is not.
            // A second `up` without the panic finds it Running and shows its view; the hostile default-command never
            // runs.
            let (code, again) = live.server.run(
                &["up", "-Client", "claude"],
                &[("MUX_AGENTS_STATE", state.as_str())],
            );
            assert_eq!(code, 0, "{mux:?} {again}");
            let member = |r: &Value| {
                r["members"]
                    .as_array()
                    .and_then(|m| m.iter().find(|m| m["id"] == json!(leader)).cloned())
            };
            assert_eq!(
                member(&again).map(|m| m["life"].clone()),
                Some(json!("running")),
                "{mux:?}: the leader is still Running: {again}"
            );
            let view = again["views"]
                .as_array()
                .and_then(|v| v.iter().find(|v| v["id"] == json!(leader)).cloned())
                .unwrap_or_else(|| panic!("{mux:?}: the leader's view is shown: {again}"));
            let pane = s(&view["pane"]).to_string();
            assert!(
                eventually(Duration::from_secs(10), || live
                    .call(&[
                        "list-panes",
                        "-s",
                        "-t",
                        s(&again["session"]),
                        "-F",
                        "#{pane_id}"
                    ])
                    .lines()
                    .any(|p| p == pane)),
                "{mux:?}: the leader's view pane {pane} is in the team session: {again}"
            );
            assert!(!marker.exists(), "{mux:?}: after the second up");
        }
    }

    macro_rules! every_mux_scenario {
        ($module:ident, $mux:ident) => {
            tests_on!(
                $module,
                $mux,
                [
                    hc_view_close_survives_focus_stop,
                    hc_worker_reads_after_its_view_closes,
                    stop_then_resume_returns_with_its_conversation,
                    a_managed_copilot_resumed_into_a_conversation_holds_it,
                    a_stopped_member_resumes_with_its_stored_options_only,
                    members_mail_without_the_multiplexer,
                    a_dropped_run_reply_runs_nothing_then_a_retry_runs_once,
                    root_unsandboxed_terminal_driver_finished,
                    spawn_window_and_split_tagged,
                    spawn_titles_tab_and_pane,
                    unnamed_spawns_at_once_get_distinct_names,
                    spawn_tags_pane_writes_record_and_commit_runs_client,
                    a_new_member_reports_ready_within_150_s,
                    a_new_member_reports_before_its_client_json_is_published,
                    worker_is_path_claude_forked_own_tty,
                    state_root_reaches_pane_not_server,
                    list_send_read_name_answer,
                    send_delivers_to_mailbox,
                    read_wait_wakes_on_message,
                    focus_selects_pane,
                    stop_notifies_waiting_reader,
                    stop_leaves_no_surviving_process,
                    client_exit_posts_one_exited_notice,
                    client_start_error_posts_one_exited_notice,
                    tagged_pane_cannot_spawn_and_gone_pane_vs_dead_socket,
                    sidebar_opens_at_bar_width_and_closes,
                    bar_opens_at_right_edge_from_any_pane,
                    bar_repaired_after_a_split_right_of_it,
                    bar_repair_keeps_focus,
                    bar_drag_kept_until_the_window_resizes,
                    bar_width_follows_its_options,
                    replaced_bar_is_never_closed,
                    close_after_rotate_spares_the_users_pane,
                    unproven_label_in_another_window_refuses,
                    manual_watch_moves_nothing,
                    manual_watch_without_a_bar_moves_nothing,
                    replaced_between_proof_and_kill_survives,
                    replaced_bar_is_not_resized,
                    replaced_bar_is_not_labelled,
                    forged_record_does_not_make_a_manual_watch_keep,
                    list_all_json_shape,
                    agent_caller_forks_with_bypass_evidence,
                    spawn_model_recorded_and_listed,
                    placement_is_per_session,
                    spawn_bar_once_reused_and_close_stays_closed,
                    spawn_races_keep_one_bar_per_window,
                    three_agents_in_three_windows_each_get_one_proven_bar,
                    gone_pane_closed_by_each_trigger,
                    bar_drops_a_gone_agent,
                    bar_shows_waiting_then_working,
                    bar_keeps_its_header_with_emoji_sequences,
                    dead_pane_closed_by_one_conditional,
                    rotated_window_reaches_each_agent_by_its_pane,
                    bar_click_focuses_an_agent,
                    bar_n_and_shift_n_step_through_agents,
                    bar_keys_focus_an_agent,
                    bar_click_after_shrinking_does_nothing,
                    members_spawn_and_act_only_down_the_tree,
                    a_cascade_stop_ends_b_before_a_and_a_second_stop_finishes,
                    adoption_moves_b_and_a_resumed_successor_stops_its_child,
                    team_messaging_stays_in_the_team,
                    notebook_and_peek_stay_in_the_team,
                    an_unacked_batch_is_served_again_and_rebuilt_for_a_successor,
                    a_lost_member_is_released_and_keeps_only_its_kept_labels,
                    an_orphan_left_by_a_forward_crash_is_ended_by_terminate,
                    a_client_rejecting_resume_needs_recovery_and_fresh_clears_it,
                    a_retried_member_runs_its_lifecycle,
                    an_exited_leader_is_resumed_by_up_and_messages_its_child,
                    stopping_the_leader_then_up_makes_a_new_team,
                ]
            );
        };
    }

    every_mux_scenario!(tmux38, Tmux38);
    every_mux_scenario!(tmux32, Tmux32);
    every_mux_scenario!(rmux, Rmux);

    tests_on!(
        tmux38_only,
        Tmux38,
        [
            cap_serializes_across_state_roots,
            same_name_two_state_roots_one_wins,
            lock_held_15s_refuses_lock,
            sidebar_lock_timeout_still_spawns,
            two_sidebars_one_session_one_split,
            zoomed_window_left_alone,
            children_are_only_mux_and_client,
            panic_in_lock_frees_lock,
            bar_real_client_click_focuses,
            bar_real_client_peek,
            break_pane_of_the_last_view_gives_its_new_window_a_bar,
        ]
    );

    // tmux 3.2a's display-popup has no -T (QA): peek works there too.
    tests_on!(tmux32_peek, Tmux32, [bar_real_client_peek]);
    // break-pane is tmux's; rmux 0.10 is not known to have it.
    tests_on!(
        tmux32_break,
        Tmux32,
        [break_pane_of_the_last_view_gives_its_new_window_a_bar]
    );

    /// The tree end to end, one multiplexer per test (`every_mux_scenario`), so one panic hides no other.
    fn members_spawn_and_act_only_down_the_tree(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        std::fs::write(live.dir.join("brief.md"), "brief").unwrap();
        let (code, up) = live.run(&["up", "-Client", "claude"]);
        assert_eq!(code, 0, "{mux:?} {up}");
        let leader = up["leader"]["id"].as_str().unwrap().to_string();
        let ok = |live: &mut Live, by: &str, name: &str| {
            let (code, r) = live.spawn_by(by, name);
            assert_eq!(code, 0, "{mux:?} {name}: {r}");
            r["id"].as_str().unwrap().to_string()
        };
        let read = |live: &Live, id: &str, file: &str| -> Value {
            serde_json::from_str(&std::fs::read_to_string(live.agent_dir(id).join(file)).unwrap())
                .unwrap()
        };
        let c1 = ok(&mut live, &leader, "c1");
        let flags = read(&live, &c1, "gens/0/spec.json")["flags"].clone();
        assert!(
            !flags
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f == "--dangerously-skip-permissions"),
            "{mux:?}: a default-permission leader's child runs without bypass: {flags}"
        );
        let c2 = ok(&mut live, &leader, "c2");
        let g1 = ok(&mut live, &c1, "g1");
        let h1 = ok(&mut live, &g1, "h1");
        let r = read(&live, &h1, "record.json");
        assert_eq!(
            (&r["depth"], &r["parentAgent"]["id"]),
            (&json!(3), &json!(g1)),
            "{mux:?}"
        );
        let (code, r) = live.spawn_by(&h1, "x");
        assert_eq!(
            (code, r["check"].as_str()),
            (2, Some("depth")),
            "{mux:?} {r}"
        );
        for verb in ["stop", "read"] {
            let pid = live.client_of(&c2);
            let (code, r) = live.server.command(pid, &[verb, "c1"]);
            assert_eq!(
                (code, r["check"].as_str()),
                (2, Some("authority")),
                "{mux:?} a sibling's {verb}: {r}"
            );
        }
        let pid = live.client_of(&leader);
        let (code, r) = live.server.command(pid, &["stop", "g1"]);
        assert_eq!(code, 0, "{mux:?} the leader stops a grandchild: {r}");
        // cascade-3d: stopping g1 stops its sub-tree; h1 is ended and noticed too.
        let notice = json_file(&live.gen_file(&h1, "notice.json"));
        assert_eq!(notice["kind"], "stopped", "{mux:?} h1 stopped with g1");
        // Live besides the leader: c1, c2; four more make six, a 7th refuses.
        for name in ["c3", "c4", "c5", "c6"] {
            ok(&mut live, &leader, name);
        }
        let (code, r) = live.spawn_by(&leader, "c7");
        assert_eq!(
            (code, r["check"].as_str()),
            (2, Some("limit")),
            "{mux:?} {r}"
        );
    }

    /// A journal of member `id` is left under its server key's `teams/<hash>.cascade/`.
    fn journal_left(live: &Live, id: &str) -> bool {
        let teams = live
            .agent_dir(id)
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("teams");
        std::fs::read_dir(teams).is_ok_and(|d| {
            d.flatten()
                .any(|e| e.path().join(format!("{id}.json")).exists())
        })
    }

    // cascade-3d live (spec "Live tests"; calls 12, 26): public paths only. A person's commands run from a shell pane
    // in `up`'s team session, whose bar is closed (and proven closed) before the crash, so no watch tick finishes the
    // cascade before the restart assertions.
    fn a_cascade_stop_ends_b_before_a_and_a_second_stop_finishes(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        std::fs::write(live.dir.join("brief.md"), "brief").unwrap();
        let (code, up) = live.run(&["up", "-Client", "claude"]);
        assert_eq!(code, 0, "{mux:?} {up}");
        let leader = s(&up["leader"]["id"]).to_string();
        let team = live.split(s(&up["session"]), &["-v"]);
        let (code, close) = live.run_in(&team, &["sidebar", "-Close"]);
        assert_eq!(code, 0, "{mux:?} {close}");
        for pane in close["panes"].as_array().into_iter().flatten() {
            live.wait_gone(s(pane));
        }
        let (code, again) = live.run_in(&team, &["sidebar", "-Close"]);
        let none = again["panes"].as_array().is_none_or(Vec::is_empty);
        assert!(
            code == 0 && none,
            "{mux:?}: the team's bar is closed: {again}"
        );
        let spawn = |live: &mut Live, by: &str, name: &str| {
            let (code, r) = live.spawn_by(by, name);
            assert_eq!(code, 0, "{mux:?} {name}: {r}");
            // o2 call 35 (integration): ready before any stop ends it, so it ends an ordinary member.
            live.ready(&r);
            s(&r["id"]).to_string()
        };
        // 1. The leader stops a1 by name, inside the tree: b1, then a1; every notice; no journal left.
        let a1 = spawn(&mut live, &leader, "a1");
        let b1 = spawn(&mut live, &a1, "b1");
        let (code, r) = live.by(&leader, &["stop", "a1"]);
        let got = (code, &r["stopped"], &r["unresolved"]);
        assert_eq!(got, (0, &json!(true), &json!([])), "{mux:?} {r}");
        for id in [&a1, &b1] {
            assert!(
                live.gen_file(id, "notice.json").exists(),
                "{mux:?} {id} noticed"
            );
        }
        assert!(!journal_left(&live, &a1), "{mux:?}");
        // 2. A person stops a2 from the team's pane, killed right after b2's notice: b2 finished, a2 still live
        //    (bottom-up), the journal kept; a spawn under a2 is refused `closed`.
        let a2 = spawn(&mut live, &leader, "a2");
        let b2 = spawn(&mut live, &a2, "b2");
        let state = live.state_text();
        let step = format!("cascade-noticed-{b2}");
        let vars = [
            ("MUX_AGENTS_STATE", state.as_str()),
            ("MUX_AGENTS_CRASH", step.as_str()),
        ];
        let bin = env!("CARGO_BIN_EXE_mux-agent");
        let (code, out, err) = live
            .server
            .run_raw_in(&team, bin, &["stop", "a2"], &vars, &[]);
        assert_eq!(code, 86, "{mux:?}: killed after b2's notice: {out} {err}");
        let state_of = |live: &Live, id: &str| {
            let r = json_file(&live.agent_dir(id).join("record.json"));
            core(live, &["get", s(&r["hcAddress"]), "--json"])["state"].clone()
        };
        assert_eq!(state_of(&live, &b2), "finished", "{mux:?}: b2 ended first");
        assert_eq!(state_of(&live, &a2), "live", "{mux:?}: a2 after b2");
        assert!(journal_left(&live, &a2), "{mux:?}: the journal is kept");
        let (code, r) = live.spawn_by(&a2, "c2");
        let got = (code, r["check"].as_str());
        assert_eq!(got, (2, Some("closed")), "{mux:?}: a spawn under a2: {r}");
        // 3. The private core restarts; the journal is still there; a second person stop finishes it.
        assert!(
            live.server.core.as_mut().unwrap().stop(),
            "{mux:?}: core stop"
        );
        live.server.core.as_mut().unwrap().start();
        assert!(journal_left(&live, &a2), "{mux:?}: kept across the restart");
        let deadline = Instant::now() + LONG;
        loop {
            let _ = live.run_in(&team, &["stop", "a2"]); // finishes every journal before it names a2 (C8)
            if !journal_left(&live, &a2) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{mux:?}: a second stop never finished it"
            );
            std::thread::sleep(Duration::from_millis(500));
        }
        let a0 = live.agent_dir(&a2).join("gens/0");
        let done = a0.join("ended.json").exists() || a0.join("released.json").exists();
        assert!(done, "{mux:?}: a2 done");
        assert!(a0.join("notice.json").exists(), "{mux:?}: a2 noticed");
    }

    fn record_of(live: &Live, id: &str) -> Value {
        json_file(&live.agent_dir(id).join("record.json"))
    }
    /// Quits member `id`'s fake client and waits for its exit marker (`quit.<pid>` / `exited.<pid>`).
    fn quit_member(live: &Live, id: &str) {
        let sid = record_of(live, id)["sessionId"].clone();
        let pid = live.server.client_by_session(s(&sid))["pid"]
            .as_u64()
            .unwrap();
        support::write_atomic(&live.dir.join(format!("quit.{pid}")), "0\n");
        assert_eq!(live.server.wait_file(&format!("exited.{pid}")).trim(), "0");
    }
    fn hc_state(live: &Live, id: &str) -> Value {
        let r = record_of(live, id);
        core(live, &["get", s(&r["hcAddress"]), "--json"])["state"].clone()
    }
    fn parent_and_depth(live: &Live, id: &str) -> (Value, Value) {
        let r = record_of(live, id);
        (r["parentAgent"]["id"].clone(), r["depth"].clone())
    }
    /// The bodies in member `id`'s mailbox (an inherited `mailbox` honoured).
    fn mail_bodies(live: &Live, id: &str) -> Vec<Value> {
        let r = record_of(live, id);
        let key = r
            .get("mailbox")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_string();
        live.mailbox(&key)
            .iter()
            .map(|m| m["body"].clone())
            .collect()
    }
    /// `sidebar -Close` from `pane` (in the team's session), proven closed.
    fn close_team_bar(live: &mut Live, mux: Mux, pane: &str) {
        let (code, close) = live.run_in(pane, &["sidebar", "-Close"]);
        assert_eq!(code, 0, "{mux:?} {close}");
        for p in close["panes"].as_array().into_iter().flatten() {
            live.wait_gone(s(p));
        }
    }

    // adopt-3e live (spec "Live tests", A7, A9; calls 3, 16, 23): public commands only; a person's commands run from a
    // shell pane in `up`'s team session, whose bar is closed. X (the leader) -> A -> B; A's client exits; `list` reaps
    // A, the next `list` adopts B under X; B's `send parent` reaches X. X -> C -> D; C exits and is reaped; the person
    // resumes C as C′; `stop c1` (C′) stops D. Then X exits: it stays the root, its row ended, B under it and live, and
    // B's `send parent` still reaches X (Ended, viewless; call 5).
    fn adoption_moves_b_and_a_resumed_successor_stops_its_child(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        std::fs::write(live.dir.join("brief.md"), "brief").unwrap();
        std::fs::write(live.dir.join("task.txt"), "say hi").unwrap();
        let (code, up) = live.run(&["up", "-Client", "claude"]);
        assert_eq!(code, 0, "{mux:?} {up}");
        let x = s(&up["leader"]["id"]).to_string();
        let team = live.split(s(&up["session"]), &["-v"]);
        close_team_bar(&mut live, mux, &team);
        let spawn = |live: &mut Live, by: &str, name: &str| {
            let (code, r) = live.spawn_by(by, name);
            assert_eq!(code, 0, "{mux:?} {name}: {r}");
            s(&r["id"]).to_string()
        };
        let list = |live: &mut Live, args: &[&str]| {
            let (code, r) = live.run_in(&team, args);
            assert_eq!(code, 0, "{mux:?} {args:?}: {r}");
            r
        };
        let a = spawn(&mut live, &x, "a1");
        let b = spawn(&mut live, &a, "b1");
        quit_member(&live, &a);
        assert!(
            eventually(LONG, || hc_state(&live, &a) == "finished"),
            "{mux:?}: A finished"
        );
        list(&mut live, &["list"]); // reaps A
        list(&mut live, &["list"]); // sweeps: X adopts B
        assert_eq!(
            parent_and_depth(&live, &b),
            (json!(x), json!(1)),
            "{mux:?}: X adopted B"
        );
        std::fs::write(live.dir.join("up.md"), "from b1").unwrap();
        let body = live.path("up.md");
        let (code, sent) = live.by(&b, &["send", "parent", "-BodyFile", &body]);
        assert_eq!(code, 0, "{mux:?} {sent}");
        assert!(
            mail_bodies(&live, &x).contains(&json!("from b1")),
            "{mux:?}: X's mailbox"
        );
        assert_eq!(hc_state(&live, &b), "live", "{mux:?}: B works");

        let c = spawn(&mut live, &x, "c1");
        let d = spawn(&mut live, &c, "d1");
        close_team_bar(&mut live, mux, &team); // nothing sweeps D under X before the resume
        quit_member(&live, &c);
        assert!(
            eventually(LONG, || hc_state(&live, &c) == "finished"),
            "{mux:?}: C finished"
        );
        list(&mut live, &["list"]); // records C's end (its sweep ran before the reap)
        let resumed = list(&mut live, &["resume", "c1"]);
        let c2 = s(&resumed["agent"]["id"]).to_string();
        assert_ne!(c2, c, "{mux:?}: a successor");
        assert_eq!(
            parent_and_depth(&live, &d).0,
            json!(c),
            "{mux:?}: D still names C (C0 resolves it)"
        );
        list(&mut live, &["stop", "c1"]);
        assert!(
            eventually(LONG, || hc_state(&live, &d) == "finished"),
            "{mux:?}: stop C′ stopped D"
        );

        let x_name = record_of(&live, &x)["name"].clone();
        quit_member(&live, &x);
        list(&mut live, &["list"]);
        let rows = list(&mut live, &["list", "-All"]);
        let row = rows["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == x_name)
            .cloned();
        assert_eq!(
            row.unwrap()["life"],
            "ended",
            "{mux:?}: the root's row ended: {rows}"
        );
        assert_eq!(
            parent_and_depth(&live, &b),
            (json!(x), json!(1)),
            "{mux:?}: B stays under the ended root"
        );
        assert_eq!(hc_state(&live, &b), "live", "{mux:?}: B still works");
        std::fs::write(live.dir.join("up2.md"), "after x ended").unwrap();
        let body = live.path("up2.md");
        let (code, sent) = live.by(&b, &["send", "parent", "-BodyFile", &body]);
        assert_eq!(code, 0, "{mux:?} {sent}");
        assert!(
            mail_bodies(&live, &x).contains(&json!("after x ended")),
            "{mux:?}: X's mailbox after X ended"
        );
    }

    // The ids a read served (4b-i, `msg-plan-4b-i-3.md` F25).
    fn ids_of(reply: &Value) -> Vec<String> {
        reply["ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| s(v).to_string())
            .collect()
    }

    /// Team messaging end to end (4a): leader > a > b, and team T2 in another session. A broadcast is one id
    /// each; a reply is hop 1; another team is refused; `children` is only the leader's own; the 121st send of the
    /// hour refuses with `retryAt`.
    fn team_messaging_stays_in_the_team(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        std::fs::write(live.dir.join("brief.md"), "brief").unwrap();
        let body = live.path("body.md");
        std::fs::write(&body, "hello").unwrap();
        let (code, up) = live.run(&["up", "-Client", "claude"]);
        assert_eq!(code, 0, "{mux:?} {up}");
        let leader = s(&up["leader"]["id"]).to_string();
        let spawn = |live: &mut Live, by: &str, name: &str| {
            let (code, r) = live.spawn_by(by, name);
            assert_eq!(code, 0, "{mux:?} {name}: {r}");
            s(&r["id"]).to_string()
        };
        let a = spawn(&mut live, &leader, "a");
        let b = spawn(&mut live, &a, "b");
        // Team T2: `up` from another folder in another session.
        let pane = live.session_in("t2", &live.dir.join("t2"));
        let (code, up2) = live.run_in(&pane, &["up", "-Client", "claude"]);
        assert_eq!(code, 0, "{mux:?} {up2}");
        spawn(&mut live, s(&up2["leader"]["id"]), "x");
        let (code, sent) = live.by(&a, &["send", "team", "-BodyFile", &body]);
        assert_eq!(code, 0, "{mux:?} {sent}");
        let id = s(&sent["id"]).to_string();
        for who in [&leader, &b] {
            let (_, r) = live.by(who, &["read"]);
            assert_eq!(ids_of(&r), std::slice::from_ref(&id), "{mux:?} one id each");
        }
        let (code, r) = live.by(&b, &["send", "a", "-BodyFile", &body, "-Re", &id]);
        assert_eq!((code, &r["hop"]), (0, &json!(1)), "{mux:?} {r}");
        let (code, r) = live.by(&b, &["send", "x", "-BodyFile", &body]);
        assert_eq!(
            (code, s(&r["check"])),
            (2, "target"),
            "{mux:?} another team: {r}"
        );
        let (_, r) = live.by(&leader, &["send", "children", "-BodyFile", &body]);
        assert_eq!(
            r["recipients"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| s(&x["id"]).to_string())
                .collect::<Vec<_>>(),
            std::slice::from_ref(&a),
            "{mux:?}"
        );
        // The quota through the live client (F22): a has sent one (team); 119 more fill the hour, the 121st refuses.
        for n in 0..119 {
            let (code, r) = live.by(&a, &["send", "leader", "-BodyFile", &body]);
            assert_eq!(code, 0, "{mux:?} send {}: {r}", n + 2);
        }
        let (code, r) = live.by(&a, &["send", "leader", "-BodyFile", &body]);
        assert_eq!(
            (code, s(&r["check"]), r["retryAt"].is_string()),
            (2, "quota", true),
            "{mux:?} the 121st: {r}"
        );
    }

    /// Phase 7a end to end: leader > a > b, sibling c, team T2. b lists a's notes newest first; the leader and a
    /// person of the team peek b; c and T2's person cannot. Every pane of the server is inventoried after the bars
    /// close (checklist 9): each member's fake client is proven raw by a canary byte (10), then receives no byte (8);
    /// every pane but the harness's two command panes (members, b among them, and the quiet shells) keeps its screen;
    /// panes, windows and client selection stay as arranged (b's window has another pane active; tmux: a client is
    /// attached).
    fn notebook_and_peek_stay_in_the_team(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        std::fs::write(live.dir.join("brief.md"), "brief").unwrap();
        // PM call 10: every fake client of this scenario keeps the bytes it receives in `input.<pid>.raw`.
        std::fs::write(live.dir.join("raw-input"), "").unwrap();
        let body = live.path("note.md");
        std::fs::write(&body, "the plan").unwrap();
        let (code, up) = live.run(&["up", "-Client", "claude"]);
        assert_eq!(code, 0, "{mux:?} {up}");
        let session = s(&up["session"]).to_string();
        let id = |w: &Value| s(&w["id"]).to_string();
        let spawn = |live: &mut Live, by: &Value, name: &str| {
            let (code, r) = live.spawn_by(s(&by["id"]), name);
            assert_eq!(code, 0, "{mux:?} {name}: {r}");
            r
        };
        let leader = up["leader"].clone();
        let a = spawn(&mut live, &leader, "a");
        let b = spawn(&mut live, &a, "b");
        let c = spawn(&mut live, &leader, "c");
        let mut notices = Vec::new();
        for title in ["first", "second"] {
            let (code, r) = live.by(&id(&a), &["note", "-Title", title, "-BodyFile", &body]);
            assert_eq!(code, 0, "{mux:?} {r}");
            notices.push(format!("note-{}", s(&r["id"])));
        }
        // 7b V2: b's next read serves each note's notice once.
        let (code, r) = live.by(&id(&b), &["read"]);
        assert_eq!((code, ids_of(&r)), (0, notices), "{mux:?} the notices: {r}");
        let (code, r) = live.by(&id(&b), &["notes"]);
        let titles: Vec<&str> = r["notes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| s(&n["title"]))
            .collect();
        assert_eq!(
            (code, titles),
            (0, vec!["second", "first"]),
            "{mux:?} newest first: {r}"
        );
        // Every pane, session and client exists before the snapshots: a person of the team, T2, its person.
        let root = live.server.pane().to_string();
        let person = live.split(&session, &["-v"]);
        let t2 = live.session_in("t2", &live.dir.join("t2"));
        let (code, up2) = live.run_in(&t2, &["up", "-Client", "claude"]);
        assert_eq!(code, 0, "{mux:?} {up2}");
        let t2_leader = up2["leader"].clone();
        let t2_person = live.split(s(&up2["session"]), &["-v"]);
        let members = [&leader, &a, &b, &c, &t2_leader];
        // o2 call 35: the spawned members are ready before anything is sent to them (the canary proves each client).
        for w in [&a, &b, &c] {
            live.ready(w);
        }
        // Checklist 9: every bar closes through the public command, from a shell pane of its own session.
        let shells = [root.clone(), t2.clone(), person.clone(), t2_person.clone()];
        let bar_sessions: BTreeSet<String> = live
            .panes("#{session_id}|#{@agent-role}")
            .into_iter()
            .filter_map(|r| r.strip_suffix("|sidebar").map(str::to_string))
            .collect();
        for bar_session in bar_sessions {
            let from = shells
                .iter()
                .find(|p| live.field(p, "#{session_id}") == bar_session)
                .unwrap_or_else(|| panic!("{mux:?}: no shell pane in {bar_session}"))
                .clone();
            let (code, close) = live.run_in(&from, &["sidebar", "-Close"]);
            assert_eq!(code, 0, "{mux:?} {close}");
            for bar in close["panes"].as_array().into_iter().flatten() {
                live.wait_gone(s(bar));
            }
        }
        // Checklist 14: client selection is asserted where a client can attach (tmux, as the bar scenario does).
        let client = (!matches!(mux, Mux::Rmux)).then(|| {
            let client = Client::attach(&live, &session);
            live.server.wait("the client to attach", || {
                (!live.call(&["list-clients"]).trim().is_empty()).then_some(())
            });
            client
        });
        // b's pane, then another pane of b's window made active: a peek that selects b would show.
        let (code, seen) = live.by(&id(&leader), &["peek", "b"]);
        assert_eq!(code, 0, "{mux:?} the leader peeks its grandchild: {seen}");
        let pane = s(&seen["pane"]).to_string();
        let other = live.split(&pane, &["-h"]);
        live.call(&["select-pane", "-t", &other]);
        // `up`'s own release hold (`mux-agent hold <state>/up/<team>.release`) runs until `up` publishes the release
        // file, then exits. It belongs to neither team, so the census waits for it to close rather than count it.
        for row in live.panes("#{pane_id}|#{pane_start_command}") {
            let (p, start) = row.split_once('|').unwrap_or((row.as_str(), ""));
            if start.contains(" hold ") && start.contains("/up/") && start.contains(".release") {
                live.wait_gone(p);
            }
        }
        // Checklist 9: every pane is a member of either team or one of the five known shells.
        let (quiet, commands) = ([root, other, t2], [person.clone(), t2_person.clone()]);
        let mut tagged: Vec<(String, String)> = Vec::new();
        let mut all: Vec<String> = Vec::new();
        for row in live.panes("#{pane_id}|#{@agent-id}|#{@agent-role}|#{pane_start_command}") {
            let mut part = row.splitn(4, '|');
            let (p, agent) = (part.next().unwrap(), part.next().unwrap_or(""));
            all.push(p.to_string());
            let (role, start) = (part.next().unwrap_or(""), part.next().unwrap_or(""));
            if agent.is_empty() {
                assert!(
                    quiet.iter().chain(&commands).any(|q| q == p),
                    "{mux:?}: pane {p} is neither a member nor a known shell (role {role:?}, start {start:?})"
                );
            } else {
                tagged.push((agent.to_string(), p.to_string()));
            }
        }
        let mut got: Vec<String> = tagged.iter().map(|(i, _)| i.clone()).collect();
        let mut want: Vec<String> = members.iter().map(|w| id(w)).collect();
        got.sort();
        want.sort();
        assert_eq!(
            got, want,
            "{mux:?}: the tagged panes are the members of both teams"
        );
        // Checklist 10: raw mode on each member's real terminal: one byte without Enter is logged before the
        // baseline (a cooked fallback never delivers it, and the wait fails). Only an absent first log is empty.
        let raw =
            |live: &Live, id: &str| live.dir.join(format!("input.{}.raw", live.client_of(id)));
        for (agent, p) in &tagged {
            let path = raw(&live, agent);
            let mut want = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(e) => panic!("{mux:?} {}: {e}", path.display()),
            };
            want.push(b'x');
            live.call(&["send-keys", "-t", p, "-l", "x"]);
            live.server
                .wait(&format!("the canary byte in {}", path.display()), || {
                    (std::fs::read(&path).ok()? == want).then_some(())
                });
        }
        // Checklist 8: bytes, read strictly (every log exists now).
        let typed = |live: &Live| -> Vec<Vec<u8>> {
            let read = |path: PathBuf| {
                std::fs::read(&path).unwrap_or_else(|e| panic!("{mux:?} {}: {e}", path.display()))
            };
            tagged
                .iter()
                .map(|(agent, _)| read(raw(live, agent)))
                .collect()
        };
        let screen = |live: &Live, p: &str| -> Vec<String> {
            let text = live.call(&["capture-pane", "-p", "-J", "-t", p]);
            let mut lines: Vec<String> = text.lines().map(|l| l.trim_end().to_string()).collect();
            while lines.last().is_some_and(|l| l.is_empty()) {
                lines.pop();
            }
            lines
        };
        for p in quiet.iter().chain(&commands) {
            live.server.wait(&format!("a prompt in {p}"), || {
                screen(&live, p)
                    .last()
                    .is_some_and(|l| l.ends_with('$') || l.ends_with('#'))
                    .then_some(())
            });
        }
        let focus = |live: &Live| {
            let format = "#{session_id}|#{window_id}|#{pane_id}|#{pane_active}|#{window_active}";
            let clients = live.call(&["list-clients", "-F", "#{client_name}|#{client_session}"]);
            (live.call(&["list-panes", "-a", "-F", format]), clients)
        };
        let input = typed(&live);
        // all-calls call 9: every member pane (b, the peek target, among them) and every quiet shell keeps its screen;
        // the harness's command panes are left out (it types there by design; a narrowed pane redraws its line).
        let compared = super::unchanged_panes(&all, &commands);
        assert!(
            tagged.iter().all(|(_, p)| compared.contains(p)) && compared.contains(&pane),
            "{mux:?}: every member pane is compared: {compared:?} {tagged:?}"
        );
        let screens: Vec<Vec<String>> = compared.iter().map(|p| screen(&live, p)).collect();
        let before = focus(&live);
        assert!(
            before.0.contains(&format!("|{pane}|0|")),
            "{mux:?} b's pane is not active: {}",
            before.0
        );
        let (code, r) = live.by(&id(&leader), &["peek", "b"]);
        assert_eq!(
            (code, r["pane"].as_str()),
            (0, Some(pane.as_str())),
            "{mux:?} the leader again: {r}"
        );
        let (code, r) = live.run_in(&person, &["peek", "b", "-Lines", "5"]);
        assert_eq!(
            (code, r["pane"].as_str()),
            (0, Some(pane.as_str())),
            "{mux:?} a person of the team: {r}"
        );
        let (code, r) = live.by(&id(&c), &["peek", "b"]);
        assert_eq!(
            (code, r["check"].as_str()),
            (2, Some("authority")),
            "{mux:?} a sibling of b's parent: {r}"
        );
        let (code, r) = live.run_in(&t2_person, &["peek", "b"]);
        assert_eq!(
            (code, r["check"].as_str()),
            (2, Some("target")),
            "{mux:?} another team's person: {r}"
        );
        assert_eq!(
            focus(&live),
            before,
            "{mux:?} panes, windows and client selection stay put"
        );
        assert_eq!(
            typed(&live),
            input,
            "{mux:?} no member's client received a byte"
        );
        for (n, p) in compared.iter().enumerate() {
            assert_eq!(screen(&live, p), screens[n], "{mux:?}: pane {p} changed");
        }
        drop(client);
    }

    /// Receipts end to end (4b-i): a sends b two messages; b's unacked batch is served again, then acked; a
    /// resumed b (another process, new id, the same mailbox) gets a rebuilt batch for unacked mail, which no other
    /// member can ack.
    fn an_unacked_batch_is_served_again_and_rebuilt_for_a_successor(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        std::fs::write(live.dir.join("brief.md"), "brief").unwrap();
        let body = live.path("body.md");
        std::fs::write(&body, "hello").unwrap();
        let (code, up) = live.run(&["up", "-Client", "claude"]);
        assert_eq!(code, 0, "{mux:?} {up}");
        let leader = s(&up["leader"]["id"]).to_string();
        let a = s(&live.spawn_by(&leader, "a").1["id"]).to_string();
        let b = s(&live.spawn_by(&leader, "b").1["id"]).to_string();
        for _ in 0..2 {
            assert_eq!(
                live.by(&a, &["send", "b", "-BodyFile", &body]).0,
                0,
                "{mux:?}"
            );
        }
        let (_, first) = live.by(&b, &["read", "-Deliver"]);
        assert_eq!(
            first["ids"].as_array().map(Vec::len),
            Some(2),
            "{mux:?} {first}"
        );
        // B exits before acking; read again: the same receipt.
        let (_, again) = live.by(&b, &["read", "-Deliver"]);
        assert_eq!(again["batch"], first["batch"], "{mux:?}");
        let (_, acked) = live.by(&b, &["read", "-Ack", s(&first["batch"])]);
        assert_eq!(acked["acked"], true, "{mux:?} {acked}");
        // Unacked mail, then a resumed successor (new id, gen 0, same mailbox): a rebuilt batch.
        assert_eq!(
            live.by(&a, &["send", "b", "-BodyFile", &body]).0,
            0,
            "{mux:?}"
        );
        let (_, pending) = live.by(&b, &["read", "-Deliver"]);
        let sid = s(&json_file(&live.agent_dir(&b).join("record.json"))["sessionId"]).to_string();
        let old = live.client_of(&b);
        crate::ready::wait_conversation(live.dir.path(), &sid); // PM call 12
        assert_eq!(live.by(&leader, &["stop", "b"]).0, 0, "{mux:?}");
        let (code, resumed) = live.by(&leader, &["resume", "b"]);
        assert_eq!(code, 0, "{mux:?} {resumed}");
        let pid = live.successor(s(&resumed["agent"]["id"]), &sid, old);
        assert_ne!(pid, old, "{mux:?} a new process");
        let (_, rebuilt) = live.server.command(pid, &["read", "-Deliver"]);
        assert_eq!(
            rebuilt["ids"], pending["ids"],
            "{mux:?} the same unacked mail"
        );
        assert_ne!(
            rebuilt["batch"], pending["batch"],
            "{mux:?} rebuilt for the new owner"
        );
        let (_, stale) = live.by(&a, &["read", "-Ack", s(&pending["batch"])]);
        assert_eq!(stale["check"], "ack", "{mux:?} another owner cannot ack it");
    }

    /// `name`'s activity on the board as `leader` lists it: a board holds only its caller's team (`membership::locals`
    /// skips other sessions), and the root pane is not in the team `up` made.
    fn activity_of(live: &mut Live, leader: &str, name: &str) -> String {
        let (_, l) = live.by(leader, &["list", "-All"]);
        l["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == name)
            .map_or(String::new(), |a| s(&a["activity"]).to_string())
    }

    #[test]
    fn a_copilot_member_reports_done_then_listens_on_call() {
        for mux in Mux::ALL {
            let Some(mut live) = Live::human(mux) else {
                continue;
            };
            for (f, t) in [
                ("brief.md", "brief"),
                ("task.txt", "say hi"),
                ("body.md", "nonce-7f3a"),
            ] {
                std::fs::write(live.dir.join(f), t).unwrap();
            }
            let (body, task, brief) = (
                live.path("body.md"),
                live.path("task.txt"),
                live.path("brief.md"),
            );
            let (_, up) = live.run(&["up", "-Client", "claude"]);
            let leader = s(&up["leader"]["id"]).to_string();
            let a = s(&live.spawn_by(&leader, "a").1["id"]).to_string();
            let lp = live.client_of(&leader);
            let (code, r) = live.server.command(
                lp,
                &[
                    "spawn",
                    "-Client",
                    "copilot",
                    "-TaskFile",
                    &task,
                    "-BriefFile",
                    &brief,
                    "-Context",
                    "brief",
                    "-Name",
                    "b",
                ],
            );
            assert_eq!(code, 0, "{mux:?} {r}");
            let b = s(&r["id"]).to_string();
            let bp = live.client_of(&b);
            // W5: done first, then listening; the bar shows on call.
            assert_eq!(
                live.by(&b, &["report", "done"]).1["state"],
                "done",
                "{mux:?}"
            );
            let n = live.server.post(bp, &["@bg read", "-Wait", "600"]);
            assert!(
                eventually(Duration::from_secs(10), || activity_of(
                    &mut live, &leader, "b"
                ) == "on call"),
                "{mux:?}"
            );
            // A sends and exits; the wait returns the batch.
            assert_eq!(live.by(&a, &["send", "b", "-BodyFile", &body]).0, 0);
            let ap = live.client_of(&a);
            std::fs::write(live.dir.join(format!("quit.{ap}")), "0").unwrap();
            let (_, got) = live.server.collect(bp, n);
            assert!(s(&got["text"]).contains("nonce-7f3a"), "{mux:?} {got}");
            // Re-armed with -Ack; a timeout re-arms; -Cancel ends a running wait and holds until new mail.
            let (_, t) = live.by(&b, &["read", "-Wait", "1", "-Ack", s(&got["batch"])]);
            assert_eq!(
                (&t["acked"], &t["timeout"]),
                (&json!(true), &json!(true)),
                "{mux:?} {t}"
            );
            let n = live.server.post(bp, &["@bg read", "-Wait", "600"]);
            assert!(eventually(Duration::from_secs(10), || activity_of(
                &mut live, &leader, "b"
            ) == "on call"));
            assert_eq!(live.by(&b, &["read", "-Cancel"]).1["cancelled"], true);
            assert_eq!(
                live.server.collect(bp, n).1["cancelled"],
                true,
                "{mux:?} cancel during a wait"
            );
            assert_eq!(
                live.by(&b, &["read", "-Wait", "600"]).1["cancelled"],
                true,
                "{mux:?} no re-arm until new mail"
            );
            // b's row must be there: an empty board is not "not on call".
            assert!(eventually(Duration::from_secs(10), || {
                let now = activity_of(&mut live, &leader, "b");
                !now.is_empty() && now != "on call"
            }));
            // Failure matrix, as the plugin drives the CLI.
            assert_eq!(live.by(&leader, &["send", "b", "-BodyFile", &body]).0, 0);
            let (_, d1) = live.by(&b, &["read", "-Deliver"]); // stripped block, unforwarded echo, drop: no -Ack follows
            let (_, d2) = live.by(&b, &["read", "-Deliver"]);
            assert_eq!(d1["batch"], d2["batch"], "{mux:?} re-served, never lost");
            let mail = live.keyed().join("mail");
            std::fs::write(mail.join("fault-file"), "").unwrap(); // failed ack
            assert_eq!(
                live.by(&b, &["read", "-Ack", s(&d1["batch"])]).1["check"],
                "mail",
                "{mux:?}"
            );
            assert_eq!(
                live.by(&b, &["read", "-Deliver"]).1["batch"],
                d1["batch"],
                "{mux:?} the receipt stands"
            );
            std::fs::write(mail.join("fault-folder"), "").unwrap(); // uncertain ack (4b-i R5)
            assert_eq!(
                live.by(&b, &["read", "-Ack", s(&d1["batch"])]).1["check"],
                "mail",
                "{mux:?}"
            );
            assert_eq!(
                live.by(&b, &["read", "-Ack", s(&d1["batch"])]).1["duplicate"],
                true,
                "{mux:?} retired"
            );
            // A tagged hook: a process holding the terminal id of b, outside its client chain, outside tmux/rmux.
            let addr =
                s(&json_file(&live.agent_dir(&b).join("record.json"))["hcAddress"]).to_string();
            // The state root reaches it as it reaches `live.run`: without it the hook looks in the default root, finds
            // no member for the terminal, and is refused as herdr's, never reaching the tagged check.
            let state = live.state_text();
            let (code, out, _) = live.server.run_raw(
                env!("CARGO_BIN_EXE_mux-agent"),
                &["read", "-Count"],
                &[
                    ("HC_TERMINAL_ID", addr.as_str()),
                    ("MUX_AGENTS_STATE", state.as_str()),
                ],
                &["TMUX", "RMUX"],
            );
            assert_eq!((code, out.contains("tagged")), (2, true), "{mux:?} {out}");
            // Owner change mid-delivery: a resumed b is a new owner; the old receipt is stale for it.
            live.by(&leader, &["send", "b", "-BodyFile", &body]);
            let (_, old) = live.by(&b, &["read", "-Deliver"]);
            let sid =
                s(&json_file(&live.agent_dir(&b).join("record.json"))["sessionId"]).to_string();
            let before = live.client_of(&b);
            crate::ready::wait_conversation(live.dir.path(), &sid); // PM call 12
            live.by(&leader, &["stop", "b"]);
            let (_, resumed) = live.by(&leader, &["resume", "b"]);
            let b2 = live.successor(s(&resumed["agent"]["id"]), &sid, before);
            assert_ne!(b2, before, "{mux:?} a new process");
            let (_, stale) = live.server.command(b2, &["read", "-Ack", s(&old["batch"])]);
            assert_eq!(stale["check"], "ack", "{mux:?}");
            // No keystrokes reached any terminal: each client's own input log (the fake client appends every line it
            // reads on stdin) holds neither the body nonce nor a notice. This checks what each client received, not
            // what each pane shows: the side bars draw the leader's last-sent line by design, and panes can close.
            for pid in [lp, ap, bp, b2] {
                let typed = std::fs::read_to_string(live.dir.join(format!("input.{pid}.log")))
                    .unwrap_or_default();
                assert!(
                    !typed.contains("nonce-7f3a") && !typed.contains("mail from"),
                    "{mux:?} client {pid} was typed into: {typed}"
                );
            }
            // The person's own root pane (a shell, never a bar, open for the whole test) shows neither either.
            let root = live.call(&["capture-pane", "-p", "-t", live.server.pane()]);
            assert!(
                !root.contains("nonce-7f3a") && !root.contains("mail from"),
                "{mux:?} the root pane was typed into: {root}"
            );
        }
    }

    /// reentry-6a: `up` from the root pane, then the leader spawns `a`; both ready before any test ends a member (o2
    /// call 35). Answers (leader id, a id, session, the anchor key read before any restart).
    fn team_of_two(live: &mut Live) -> (String, String, String, String) {
        let mux = live.mux();
        let (code, up) = live.run(&["up", "-Client", "claude"]);
        assert_eq!(code, 0, "{mux:?} {up}");
        assert!(up.get("sidebarError").is_none(), "{mux:?}: F6 {up}");
        let (lead, session) = (
            s(&up["leader"]["id"]).to_string(),
            s(&up["session"]).to_string(),
        );
        std::fs::write(live.dir.join("brief.md"), "brief").unwrap();
        let (task, brief) = (live.path("task.txt"), live.path("brief.md"));
        let (code, r) = live.by(
            &lead,
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
                "a",
            ],
        );
        assert_eq!(code, 0, "{mux:?} spawn a: {r}");
        let a = s(&r["id"]).to_string();
        live.ready(&json!({"id": lead, "name": "leader"}));
        live.ready(&json!({"id": a, "name": "a"}));
        (lead, a, session, live.server.key().to_string())
    }

    /// A person's `sh` pane in `session` (Unix twin of live_windows' `team_pane`): a split, focus kept, of the
    /// session's first pane that is not `up`'s `hold` (its window ends with the release). It has the server's
    /// environment, so the private core's; open it before a core loss, which ends the views.
    fn team_shell(live: &Live, session: &str) -> String {
        let panes = live.call(&[
            "list-panes",
            "-s",
            "-t",
            session,
            "-F",
            "#{pane_id} #{pane_start_command}",
        ]);
        let pane = panes
            .lines()
            .find(|l| !l.contains(" hold "))
            .and_then(|l| l.split(' ').next())
            .unwrap_or_else(|| panic!("{:?}: no pane to split in {session}: {panes}", live.mux()))
            .to_string();
        // In the root pane's folder (`<dir>/work`), so `up` from it names the same team; a split otherwise starts in
        // the server's own cwd.
        let work = live.dir.join("work").display().to_string();
        live.split(&pane, &["-v", "-c", &work])
    }

    /// How many panes of `session` attach `address` (views-6f's `attach_program` on the real start command; F11).
    fn attached(live: &Live, session: &str, address: &str) -> usize {
        use mux_agent::agents::core_identity::attach_program;
        use mux_agent::state::root::OsKind;
        live.call(&[
            "list-panes",
            "-s",
            "-t",
            session,
            "-F",
            "#{pane_start_command}",
        ])
        .lines()
        .filter(|c| attach_program(c, address, OsKind::Unix).is_some())
        .count()
    }

    /// F6 (checklist 37): the reply's bar is opened and proven: `sidebar` is a pane of the reply's session, and no
    /// `sidebarError`.
    fn bar_shown(live: &Live, reply: &Value) {
        let mux = live.mux();
        assert!(
            reply.get("sidebarError").is_none(),
            "{mux:?}: no sidebarError {reply}"
        );
        assert!(reply["sidebar"].is_string(), "{mux:?}: the bar {reply}");
        let panes = live.call(&[
            "list-panes",
            "-s",
            "-t",
            s(&reply["session"]),
            "-F",
            "#{pane_id}",
        ]);
        assert!(
            panes.lines().any(|p| p == s(&reply["sidebar"])),
            "{mux:?}: the bar is a pane of the team's session: {panes} {reply}"
        );
    }

    /// The reply's view count (0 when the reply has no `views`).
    fn views_of(reply: &Value) -> usize {
        reply["views"].as_array().map_or(0, Vec::len)
    }

    /// The team indexes under `<state>/up`.
    fn index_count(live: &Live) -> usize {
        std::fs::read_dir(live.state().join("up")).map_or(0, |d| {
            d.filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|x| x == "json")
            })
            .count()
        })
    }

    /// One `up` from the root pane with `MUX_AGENTS_CRASH=<step>` set for that command only; its exit code.
    fn crash_up(live: &mut Live, step: &str) -> i32 {
        let state = live.state_text();
        let vars = [
            ("MUX_AGENTS_STATE", state.as_str()),
            ("MUX_AGENTS_CRASH", step),
        ];
        live.server
            .run_raw(
                env!("CARGO_BIN_EXE_mux-agent"),
                &["up", "-Client", "claude"],
                &vars,
                &[],
            )
            .0
    }

    /// The spec: "Close both views: up restores them." A person's shell keeps the session; `up` from it re-enters:
    /// each view back exactly once with the bar, nothing created; a second `up` adds nothing.
    #[test]
    fn up_restores_closed_views() {
        for mux in Mux::ALL {
            let Some(mut live) = Live::human(mux) else {
                continue;
            };
            let (lead, a, session, key) = team_of_two(&mut live);
            let shell = team_shell(&live, &session);
            let mut addresses = Vec::new();
            for id in [&lead, &a] {
                let v = json_file(
                    &live
                        .state()
                        .join(&key)
                        .join("agents")
                        .join(id)
                        .join("view.json"),
                );
                live.call(&["kill-pane", "-t", s(&v["pane"])]);
                addresses.push(s(&v["hcAddress"]).to_string());
            }
            let (code, again) = live.run_in(&shell, &["up", "-Client", "claude"]);
            assert_eq!(code, 0, "{mux:?} {again}");
            assert_eq!(
                (&again["reentered"], &again["created"]),
                (&json!(true), &json!(false)),
                "{mux:?} {again}"
            );
            assert_eq!(views_of(&again), 2, "{mux:?} {again}");
            bar_shown(&live, &again);
            for address in &addresses {
                assert_eq!(
                    attached(&live, &session, address),
                    1,
                    "{mux:?}: one view of {address}"
                );
            }
            // Row 13.1 (call 6): a second `up` from the same shell adopts what is open.
            let (code, again2) = live.run_in(&shell, &["up", "-Client", "claude"]);
            assert_eq!(code, 0, "{mux:?} {again2}");
            assert_eq!(
                (views_of(&again2), &again2["duplicates"]),
                (2, &json!(0)),
                "{mux:?} {again2}"
            );
            bar_shown(&live, &again2);
            for address in &addresses {
                assert_eq!(
                    attached(&live, &session, address),
                    1,
                    "{mux:?}: still one view of {address}"
                );
            }
        }
    }

    /// The spec: "`kill-server`; `up`: a new session, both views, the bar." The members's terminals live on in the
    /// private core; `up` from the new server's root pane re-enters at the old anchor.
    #[test]
    fn up_after_kill_server_makes_a_new_session_with_both_views_and_the_bar() {
        for mux in Mux::ALL {
            let Some(mut live) = Live::human(mux) else {
                continue;
            };
            let (lead, a, _, k1) = team_of_two(&mut live);
            live.server.restart();
            assert_ne!(live.server.key(), k1, "{mux:?}: a new key");
            let (code, again) = live.run(&["up", "-Client", "claude"]);
            assert_eq!(code, 0, "{mux:?} {again}");
            assert_eq!(
                (&again["reentered"], &again["created"]),
                (&json!(true), &json!(false)),
                "{mux:?} {again}"
            );
            let session = s(&again["session"]).to_string();
            let mut shown: Vec<&str> = again["views"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| s(&v["id"]))
                .collect();
            shown.sort();
            let mut want = vec![lead.as_str(), a.as_str()];
            want.sort();
            assert_eq!(shown, want, "{mux:?}: both views");
            bar_shown(&live, &again);
            for id in [&lead, &a] {
                let v = json_file(
                    &live
                        .state()
                        .join(&k1)
                        .join("agents")
                        .join(id)
                        .join("view.json"),
                );
                assert_eq!(
                    attached(&live, &session, s(&v["hcAddress"])),
                    1,
                    "{mux:?}: {id}"
                );
                assert_eq!(
                    s(&v["serverKey"]),
                    live.server.key(),
                    "{mux:?}: the view is on the new server"
                );
            }
            assert!(
                !live.state().join(live.server.key()).join("agents").exists(),
                "{mux:?}: state stays at the anchor"
            );
            let shell = team_shell(&live, &session);
            let (code, listed) = live.run_in(&shell, &["list"]);
            assert_eq!(
                code, 0,
                "{mux:?}: a person in the new view reaches the team {listed}"
            );
            assert!(listed.to_string().contains("\"a\""), "{mux:?} {listed}");
        }
    }

    /// The spec: "Abrupt core loss: up restores members and opens their views." The person's shell is opened before
    /// the loss (the views end with the core); each restored member is ready before teardown ends it.
    #[test]
    fn up_after_a_core_loss_restores_members_and_opens_their_views() {
        for mux in Mux::ALL {
            let Some(mut live) = Live::human(mux) else {
                continue;
            };
            let (lead, a, session, key) = team_of_two(&mut live);
            let shell = team_shell(&live, &session);
            lose_the_core(&mut live, &[lead.clone(), a.clone()]);
            let (code, again) = live.run_in(&shell, &["up", "-Client", "claude"]);
            assert_eq!(code, 0, "{mux:?} {again}");
            let mut restored: Vec<&str> = again["restored"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| s(&e["id"]))
                .collect();
            restored.sort();
            let mut want = vec![lead.as_str(), a.as_str()];
            want.sort();
            assert_eq!(restored, want, "{mux:?} {again}");
            assert_eq!(views_of(&again), 2, "{mux:?}: views opened {again}");
            bar_shown(&live, &again);
            for id in [&lead, &a] {
                let v = json_file(
                    &live
                        .state()
                        .join(&key)
                        .join("agents")
                        .join(id)
                        .join("view.json"),
                );
                assert_eq!(v["gen"], 1, "{mux:?}: the view is generation 1's");
                assert_eq!(
                    attached(&live, &session, s(&v["hcAddress"])),
                    1,
                    "{mux:?}: {id}"
                );
                live.ready(&json!({"id": id, "name": id}));
            }
        }
    }

    /// The spec: "A crash after E6.1: the next up makes one team." Row 13.5 (call 6): the index is counted right after
    /// the crash, not only after the retry.
    #[test]
    fn a_crash_after_the_index_then_up_makes_one_team() {
        for mux in Mux::ALL {
            let Some(mut live) = Live::human(mux) else {
                continue;
            };
            assert_eq!(
                crash_up(&mut live, "index"),
                86,
                "{mux:?}: crashed after the index"
            );
            assert_eq!(
                index_count(&live),
                1,
                "{mux:?}: the index exists right after the crash"
            );
            let sessions = |live: &Live| {
                live.call(&["list-sessions", "-F", "#{session_name}"])
                    .lines()
                    .count()
            };
            assert_eq!(sessions(&live), 1, "{mux:?}: no session yet");
            let (code, up) = live.run(&["up", "-Client", "claude"]);
            assert_eq!((code, &up["created"]), (0, &json!(true)), "{mux:?} {up}");
            assert_eq!(sessions(&live), 2, "{mux:?}: one team session");
            assert_eq!(
                std::fs::read_dir(live.keyed().join("agents"))
                    .unwrap()
                    .count(),
                1,
                "{mux:?}: one leader folder"
            );
            assert_eq!(index_count(&live), 1, "{mux:?}: one index");
            live.ready(&json!({"id": up["leader"]["id"], "name": "leader"}));
        }
    }

    /// Call 5: the hold window still exists when the first view opens: a crash at `presented` (views, bar and switch
    /// done, the release not yet published) leaves the `hold` window and the leader's view; the next `up`, from a
    /// person's shell in that session (checklist 50), publishes the release and the hold window ends.
    #[test]
    fn the_hold_window_outlives_the_first_view() {
        let commands =
            |live: &Live| live.call(&["list-panes", "-a", "-F", "#{pane_start_command}"]);
        for mux in Mux::ALL {
            let Some(mut live) = Live::human(mux) else {
                continue;
            };
            assert_eq!(
                crash_up(&mut live, "presented"),
                86,
                "{mux:?}: crashed after the views, before the release"
            );
            // Row 13.6: five of `hold`'s polls, so a release published before the crash would have ended it by now.
            std::thread::sleep(mux_agent::hold::POLL * 5);
            let now = commands(&live);
            assert!(
                now.lines().any(|c| c.contains(" hold ")),
                "{mux:?}: the hold window is still there: {now}"
            );
            assert!(
                now.lines().any(|c| c.contains(" attach ")),
                "{mux:?}: and the first view is open: {now}"
            );
            let root = live.field(live.server.pane(), "#{session_id}");
            let listed = live.call(&["list-sessions", "-F", "#{session_id}"]);
            let team = listed
                .lines()
                .find(|l| *l != root)
                .unwrap_or_else(|| panic!("{mux:?}: no team session: {listed}"))
                .to_string();
            let shell = team_shell(&live, &team);
            // rmux gives the session the crashed `up`'s environment, so a pane in it has MUX_AGENTS_CRASH: the retry
            // unsets it (the Windows twin's `setlocal`, finding 14.5).
            let state = live.state_text();
            let (code, out, err) = live.server.run_raw_in(
                &shell,
                env!("CARGO_BIN_EXE_mux-agent"),
                &["up", "-Client", "claude"],
                &[("MUX_AGENTS_STATE", state.as_str())],
                &["MUX_AGENTS_CRASH"],
            );
            assert_eq!(code, 0, "{mux:?} {out} {err}");
            let again: Value =
                serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{mux:?} {e}: {out}"));
            live.server.wait("the hold window to end", || {
                (!commands(&live).lines().any(|c| c.contains(" hold "))).then_some(())
            });
            live.ready(&json!({"id": again["leader"]["id"], "name": "leader"}));
        }
    }

    /// restore-6c Task 12: the bound on each lifecycle wait.
    const LIFE: Duration = Duration::from_secs(150);

    /// The `list -All` row of the member in `folder`, by its recorded `hcAddress` (ordinary rows have no `id`).
    fn row(live: &mut Live, pane: &str, folder: &Path) -> Value {
        let address = json_file(&folder.join("record.json"))["hcAddress"].clone();
        let (_, list) = live.run_in(pane, &["list", "-All"]);
        list["agents"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| r["hcAddress"] == address)
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// What a failed restore-6c assertion shows: the `list -All` reply, hc's view and the panes.
    fn seen(live: &mut Live, pane: &str) -> String {
        let (_, list) = live.run_in(pane, &["list", "-All"]);
        let hc = support::private_core::run(
            live.server
                .core
                .as_ref()
                .expect("private core")
                .command(&["ls", "--all", "--json"]),
            live.dir.path(),
        )
        .map_or_else(|e| format!("hc ls: {e}"), |v| v.to_string());
        let panes = live.call(&[
            "list-panes",
            "-a",
            "-F",
            "#{session_name} #{pane_id} #{pane_start_command}",
        ]);
        format!("list: {list}\nhc: {hc}\npanes:\n{panes}")
    }

    /// The member's recorded generation (a decimal string).
    fn generation(folder: &Path) -> u64 {
        s(&json_file(&folder.join("record.json"))["generation"])
            .parse()
            .unwrap()
    }

    /// Generation n's own client pid: its generation client evidence (`gens/<n>/client.json`), never a lookup by
    /// session, which can find a dead predecessor of the same session.
    fn gen_client(folder: &Path, n: u64) -> u64 {
        let path = folder.join("gens").join(n.to_string()).join("client.json");
        assert!(eventually(LIFE, || path.exists()), "{}", path.display());
        json_file(&path)["pid"].as_u64().unwrap()
    }

    /// Call 16, call 25 (checklist 45): the same-id lifecycle of generation n of `id`, driven through n's own
    /// client: ready, reads mail, spawns, exits. `parent` sends the mail and is told of the exit; without one (the
    /// leader) the person's `pane` sends and no mailbox is told. Its marker `{kind, exitCode, at}` (checklist 22, 62:
    /// a field set, not an order) is written once, bytes stable.
    fn lifecycle(live: &mut Live, pane: &str, parent: Option<&str>, id: &str, n: u64) {
        let mux = live.mux();
        let folder = live.agent_dir(id);
        let gen_dir = folder.join("gens").join(n.to_string());
        let name = s(&json_file(&folder.join("record.json"))["name"]).to_string();
        live.ready(&json!({"id": id, "name": name}));
        let pid = gen_client(&folder, n);
        let hello = format!("hello {name} generation {n}");
        let (code, sent) = match parent {
            Some(p) => {
                let body = live.dir.join(format!("hello-{id}-{n}.md"));
                std::fs::write(&body, &hello).unwrap();
                live.by(p, &["send", &name, "-BodyFile", body.to_str().unwrap()])
            }
            None => live.run_in(pane, &["send", &name, "-Body", &hello]),
        };
        assert_eq!(code, 0, "{mux:?} {sent}");
        let (code, mail) = live.server.command(pid, &["read"]);
        assert!(
            code == 0 && mail.to_string().contains(&hello),
            "{mux:?}: generation {n} of {name} reads its mail: {mail}"
        );
        let (task, brief, child) = (
            live.path("task.txt"),
            live.path("brief.md"),
            format!("{name}-child"),
        );
        let (code, made) = live.server.command(
            pid,
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
                &child,
            ],
        );
        assert_eq!(code, 0, "{mux:?}: generation {n} of {name} spawns: {made}");
        live.ready(&made); // o2 call 35: nothing ends while a member is still starting
        support::write_atomic(&live.dir.join(format!("quit.{pid}")), "0");
        let notice = gen_dir.join("notice.json");
        let recorded = eventually(LIFE, || {
            live.run_in(pane, &["list"]);
            notice.exists()
        });
        assert!(
            recorded,
            "{mux:?}: {name}'s exit is recorded\n{}",
            seen(live, pane)
        );
        let first = std::fs::read(&notice).unwrap();
        let marker: Value = serde_json::from_slice(&first).unwrap();
        let keys: BTreeSet<&str> = marker
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            (keys, &marker["kind"], &marker["exitCode"]),
            (
                BTreeSet::from(["at", "exitCode", "kind"]),
                &json!("exited"),
                &json!(0)
            ),
            "{mux:?}: the marker's own contents: {marker}"
        );
        let exit = format!("exit-{id}-{n}");
        let delivered = |live: &Live| {
            parent.map_or(0, |p| {
                live.mailbox(p)
                    .iter()
                    .filter(|m| m["id"] == exit.as_str())
                    .count()
            })
        };
        if parent.is_some() {
            assert!(
                eventually(LIFE, || delivered(live) >= 1),
                "{mux:?}: the parent is told {exit}\n{}",
                seen(live, pane)
            );
        }
        live.run_in(pane, &["list"]);
        assert_eq!(
            std::fs::read(&notice).unwrap(),
            first,
            "{mux:?}: recorded once"
        );
        assert_eq!(
            delivered(live),
            usize::from(parent.is_some()),
            "{mux:?}: delivered once, with its generation"
        );
    }

    /// A member made by today's resume of an Ended `w` while the client rejects `--resume`: it fails before ready,
    /// so it needs recovery. Returns its folder.
    fn failed_member(live: &mut Live, pane: &str, leader: &str, w: &str) -> PathBuf {
        let mux = live.mux();
        let (code, made) = live.spawn_by(leader, w);
        assert_eq!(code, 0, "{mux:?} {made}");
        live.ready(&made); // a member is stopped only after it reported ready
        let (code, stopped) = live.by(leader, &["stop", w]);
        assert_eq!(code, 0, "{mux:?} {stopped}");
        support::write_atomic(&live.dir.join("resume-gone"), "");
        let (code, y) = live.run_in(pane, &["resume", w]);
        assert_eq!(code, 0, "{mux:?} {y}");
        let folder = live.agent_dir(s(&y["agent"]["id"]));
        let flagged = eventually(LIFE, || row(live, pane, &folder)["needsRecovery"] == true);
        assert!(flagged, "{mux:?}: {w} needs recovery\n{}", seen(live, pane));
        folder
    }

    /// After `up` from the root pane (in the scenario, so the serial guard sees it): the brief seeded, the leader
    /// ready; answers (leader id, a person's pane in the team session).
    fn team_of(live: &mut Live, code: i32, up: &Value) -> (String, String) {
        let mux = live.mux();
        assert_eq!(code, 0, "{mux:?} {up}");
        std::fs::write(live.dir.join("brief.md"), "brief").unwrap();
        let leader = s(&up["leader"]["id"]).to_string();
        live.ready(&json!({"id": leader, "name": "leader"}));
        (leader, team_shell(live, s(&up["session"])))
    }

    /// A recovered generation runs without a view: its view verbs (`spawn`) refuse `views` ("the team's views are
    /// down; run up"). Once it is ready, `up` from the person's pane re-enters the team and presents it (D33).
    fn presented(live: &mut Live, pane: &str, id: &str) {
        let mux = live.mux();
        let name = s(&json_file(&live.agent_dir(id).join("record.json"))["name"]).to_string();
        live.ready(&json!({"id": id, "name": name}));
        let (code, again) = live.run_in(pane, &["up", "-Client", "claude"]);
        assert_eq!(
            (code, &again["created"]),
            (0, &json!(false)),
            "{mux:?}: up presents the recovered {name}: {again}\n{}",
            seen(live, pane)
        );
    }

    /// Spec live 1, call 23: `recover` on the bar and in `list`. A retry still meets the rejecting client, and its
    /// generation's failure is recorded. Then `-Fresh` takes a new session id, reports ready, clears recovery, and
    /// runs the same-id lifecycle.
    fn a_client_rejecting_resume_needs_recovery_and_fresh_clears_it(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let (code, up) = live.run(&["up", "-Client", "claude"]);
        let (leader, pane) = team_of(&mut live, code, &up);
        let y = failed_member(&mut live, &pane, &leader, "w1");
        let r = row(&mut live, &pane, &y);
        assert_eq!(
            r["recoveryReason"], "client exited before readiness",
            "{mux:?}: {r}"
        );
        if let Some(bar) = up["sidebar"].as_str() {
            let shown = eventually(Duration::from_secs(30), || {
                live.call(&["capture-pane", "-p", "-t", bar])
                    .lines()
                    .any(|l| l.contains("w1") && l.contains("recover"))
            });
            let screen = live.call(&["capture-pane", "-p", "-t", bar]);
            assert!(shown, "{mux:?}: the bar shows recover:\n{screen}");
        }
        let (code, retried) = live.run_in(&pane, &["resume", "w1"]);
        assert_eq!(
            (code, &retried["agent"]["kind"]),
            (0, &json!("retry")),
            "{mux:?} {retried}"
        );
        let n = retried["agent"]["generation"].as_u64().unwrap();
        let outcome = y.join("gens").join(n.to_string()).join("outcome.json");
        let recorded = eventually(LIFE, || {
            live.run_in(&pane, &["list"]);
            outcome.exists()
        });
        assert!(
            recorded,
            "{mux:?}: the retry's outcome is recorded\n{}",
            seen(&mut live, &pane)
        );
        assert_eq!(
            json_file(&outcome)["outcome"],
            "failed",
            "{mux:?}: the retry met the rejecting client"
        );
        let before = s(&json_file(&y.join("record.json"))["sessionId"]).to_string();
        std::fs::remove_file(live.dir.join("resume-gone")).unwrap();
        let (code, fresh) = live.run_in(&pane, &["resume", "w1", "-Fresh"]);
        assert_eq!(
            (code, &fresh["agent"]["kind"]),
            (0, &json!("fresh")),
            "{mux:?} {fresh}"
        );
        let m = fresh["agent"]["generation"].as_u64().unwrap();
        assert_ne!(
            s(&fresh["agent"]["sessionId"]),
            before,
            "{mux:?}: a new session id"
        );
        let cleared = eventually(LIFE, || row(&mut live, &pane, &y)["needsRecovery"] != true);
        assert!(
            cleared,
            "{mux:?}: fresh clears recovery\n{}",
            seen(&mut live, &pane)
        );
        assert_eq!(
            s(&json_file(&y.join("record.json"))["sessionId"]),
            s(&fresh["agent"]["sessionId"]),
            "{mux:?}"
        );
        let id = s(&json_file(&y.join("record.json"))["id"]).to_string();
        presented(&mut live, &pane, &id);
        lifecycle(&mut live, &pane, Some(&leader), &id, m);
    }

    /// Call 16, retry leg: once the client accepts `--resume` again, a retry resumes the member's conversation and
    /// that generation runs the same-id lifecycle.
    fn a_retried_member_runs_its_lifecycle(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let (code, up) = live.run(&["up", "-Client", "claude"]);
        let (leader, pane) = team_of(&mut live, code, &up);
        let y = failed_member(&mut live, &pane, &leader, "w2");
        std::fs::remove_file(live.dir.join("resume-gone")).unwrap();
        let sid = s(&json_file(&y.join("record.json"))["sessionId"]).to_string();
        let (code, retried) = live.run_in(&pane, &["resume", "w2"]);
        assert_eq!(
            (
                code,
                &retried["agent"]["kind"],
                s(&retried["agent"]["sessionId"])
            ),
            (0, &json!("retry"), sid.as_str()),
            "{mux:?} {retried}"
        );
        let n = retried["agent"]["generation"].as_u64().unwrap();
        let id = s(&json_file(&y.join("record.json"))["id"]).to_string();
        presented(&mut live, &pane, &id);
        lifecycle(&mut live, &pane, Some(&leader), &id, n);
    }

    /// Spec live 2 (checklist 45): the leader exits; its child runs on; `up` resumes the leader as n+1 of the same
    /// id; it messages the child, then runs the whole same-id lifecycle. The parentless leader's exit is its
    /// generation's notice, recorded once (no mailbox).
    fn an_exited_leader_is_resumed_by_up_and_messages_its_child(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let (code, up) = live.run(&["up", "-Client", "claude"]);
        let (leader, pane) = team_of(&mut live, code, &up);
        let (code, w1) = live.spawn_by(&leader, "w1");
        assert_eq!(code, 0, "{mux:?} {w1}");
        live.ready(&w1);
        let lead = live.agent_dir(&leader);
        let n = generation(&lead);
        let old = gen_client(&lead, n);
        support::write_atomic(&live.dir.join(format!("quit.{old}")), "0");
        let notice = lead.join("gens").join(n.to_string()).join("notice.json");
        let recorded = eventually(LIFE, || {
            live.run_in(&pane, &["list"]);
            notice.exists()
        });
        assert!(
            recorded,
            "{mux:?}: the leader's notice is recorded\n{}",
            seen(&mut live, &pane)
        );
        let first = std::fs::read(&notice).unwrap();
        let marker: Value = serde_json::from_slice(&first).unwrap();
        assert_eq!(marker["kind"], "exited", "{mux:?}: {marker}");
        let (code, again) = live.run_in(&pane, &["up", "-Client", "claude"]);
        assert_eq!(
            (
                code,
                &again["created"],
                &again["leaderResume"]["resumed"]["generation"]
            ),
            (0, &json!(false), &json!(n + 1)),
            "{mux:?} {again}\n{}",
            seen(&mut live, &pane)
        );
        assert_eq!(
            std::fs::read(&notice).unwrap(),
            first,
            "{mux:?}: recorded once"
        );
        let rec = json_file(&lead.join("record.json"));
        assert_eq!(
            (&rec["id"], &rec["depth"]),
            (&json!(leader), &json!(0)),
            "{mux:?}: the same root"
        );
        live.ready(&json!({"id": leader, "name": "leader"}));
        let new = gen_client(&lead, n + 1); // 5-9 F16: the successor's own client, not the dead predecessor
        assert_ne!(new, old, "{mux:?}: a new client");
        let body = live.dir.join("to-w1.md");
        std::fs::write(&body, "hello from the resumed leader").unwrap();
        let (code, sent) = live
            .server
            .command(new, &["send", "w1", "-BodyFile", body.to_str().unwrap()]);
        assert_eq!(code, 0, "{mux:?} {sent}");
        let (code, mail) = live.by(s(&w1["id"]), &["read"]);
        assert!(
            code == 0 && mail.to_string().contains("hello from the resumed leader"),
            "{mux:?}: {mail}"
        );
        lifecycle(&mut live, &pane, None, &leader, n + 1);
    }

    /// Spec live 3: stop the leader; `up` retires the team and makes a new one; old members stay stopped; the old
    /// session's team is retired, so a person verb there answers 1 `moved` (D20, r6c call 34).
    fn stopping_the_leader_then_up_makes_a_new_team(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let (code, up) = live.run(&["up", "-Client", "claude"]);
        let (leader, pane) = team_of(&mut live, code, &up);
        let session = s(&up["session"]).to_string();
        let (code, w1) = live.spawn_by(&leader, "w1");
        assert_eq!(code, 0, "{mux:?} {w1}");
        live.ready(&w1);
        let (code, stopped) = live.run_in(&pane, &["stop", "leader"]);
        assert_eq!(
            (code, &stopped["stopped"]),
            (0, &json!(true)),
            "{mux:?} {stopped}"
        );
        let (code, next) = live.run_in(&pane, &["up", "-Client", "claude"]);
        assert_eq!(
            (code, &next["created"]),
            (0, &json!(true)),
            "{mux:?} {next}\n{}",
            seen(&mut live, &pane)
        );
        assert_ne!(next["session"], up["session"], "{mux:?}: a new session");
        assert_ne!(
            next["leader"]["id"], up["leader"]["id"],
            "{mux:?}: a new leader"
        );
        assert!(
            s(&next["retired"]).ends_with(&format!(":{session}")),
            "{mux:?} {next}"
        );
        for id in [leader.as_str(), s(&w1["id"])] {
            assert_eq!(
                json_file(&live.agent_dir(id).join("desired.json"))["desired"],
                "stopped",
                "{mux:?}: {id}"
            );
        }
        let (code, restored) = live.run_in(&pane, &["restore"]);
        assert_eq!(
            (code, &restored["check"]),
            (1, &json!("moved")),
            "{mux:?} {restored}"
        );
        live.ready(&json!({"id": next["leader"]["id"], "name": "leader"}));
    }
}

/// The panes whose screens must not change while a team peeks (all-calls call 9): every pane of the server except
/// the harness's command panes, where the harness types by design and a narrowed pane redraws its own line.
fn unchanged_panes(all: &[String], commands: &[String]) -> Vec<String> {
    all.iter()
        .filter(|p| !commands.contains(p))
        .cloned()
        .collect()
}

// (macOS CI 133755859846): a command pane redrew the harness's own line after the pane narrowed. It is left out
// of the screen comparison; every member pane (b, the peek target, among them) and every quiet shell stays in.
#[test]
fn only_the_harness_command_panes_leave_the_screen_comparison() {
    let s = |v: &[&str]| v.iter().map(|p| p.to_string()).collect::<Vec<_>>();
    let all = s(&["%0", "%1", "%2", "%3", "%4", "%5", "%6", "%7", "%8"]);
    let commands = s(&["%5", "%8"]);
    assert_eq!(
        unchanged_panes(&all, &commands),
        s(&["%0", "%1", "%2", "%3", "%4", "%6", "%7"]),
        "members and quiet shells compared, command panes not"
    );
}

#[cfg(unix)]
#[test]
fn private_core_unix() {
    for mux in Mux::ALL {
        let d = Dir::new();
        let Some(mut s) = Server::start(mux, &d) else {
            continue;
        };
        let core = s.core.as_ref().expect("server must own a core");
        let expected = core
            .env
            .iter()
            .filter(|(k, _)| ["HC_SESSION", "HC_SOCKET", "HOME", "XDG_CONFIG_HOME"].contains(k))
            .map(|(k, v)| format!("{k}={}", v.to_string_lossy()))
            .collect::<Vec<_>>();
        let probe = core.command(&["server", "stop", "--json"]);
        let env = s.pane_env();
        for entry in expected {
            assert!(env.lines().any(|line| line == entry), "missing {entry}");
        }
        s.kill();
        assert_eq!(
            support::private_core::run(probe, d.path()).unwrap()["stopped"],
            false
        );
    }
}

#[cfg(unix)]
#[test]
fn private_core_stop_failure_kills_mux_and_keeps_evidence() {
    check_private_core_stop_failure(false);
}

#[cfg(unix)]
#[test]
fn private_core_stop_failure_during_unwind_kills_mux_and_keeps_evidence() {
    check_private_core_stop_failure(true);
}

#[cfg(unix)]
fn check_private_core_stop_failure(unwinding: bool) {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    struct KillOnUnwind<'a>(&'a mut Server);
    impl Drop for KillOnUnwind<'_> {
        fn drop(&mut self) {
            self.0.kill();
        }
    }

    for mux in Mux::ALL {
        let dir = Dir::new();
        let Some(mut server) = Server::start(mux, &dir) else {
            continue;
        };
        server.send_keys("claude");
        let client = server.wait_client()["pid"].as_u64().unwrap() as u32;
        let pid = server.pid();
        let path = dir.path().to_path_buf();
        // Stop the real private core before injecting a genuine command-spawn failure.
        assert!(server.core.as_mut().unwrap().stop());
        let mut failed = support::private_core::PrivateCore::new(
            dir.join("missing-core"),
            dir.path(),
            &std::env::var_os("PATH").unwrap(),
        );
        assert!(catch_unwind(AssertUnwindSafe(|| failed.start())).is_err());
        server.core = Some(failed);
        let result = catch_unwind(AssertUnwindSafe(|| {
            if unwinding {
                let _guard = KillOnUnwind(&mut server);
                panic!("fixture failure");
            }
            server.kill();
        }));
        let mux_gone = !support::alive(pid);
        let client_gone = !support::alive(client);
        // Recover even against the old early-return teardown before making assertions.
        server.core = None;
        server.kill();
        drop(server);
        drop(dir);
        let kept = path.is_dir() && path.join("core-stop-failed").is_file();
        std::fs::remove_dir_all(&path).unwrap();

        assert!(
            mux_gone,
            "{mux:?} unwinding={unwinding}: mux survived failed core stop"
        );
        assert!(
            client_gone,
            "{mux:?} unwinding={unwinding}: client survived failed core stop"
        );
        assert!(kept, "{mux:?}: evidence folder was removed");
        let panic = result.expect_err("failed core stop must report its failure");
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap();
        let expected = if unwinding {
            "fixture failure".to_string()
        } else {
            format!(
                "private core stop failed; fixture kept at {}",
                path.display()
            )
        };
        assert_eq!(message, expected);
    }
}
