#![cfg(feature = "live")]
//! Live tests: real tmux 3.8, tmux 3.2a and rmux servers on private sockets, never the user's own. Each server starts
//! from a temp dir with its own HOME and state, `claude`/`copilot` on PATH as the fake client, and dies with its guard.
//! Run with `cargo test --features live --test live -- --nocapture`; tmux 3.2a needs MUX_AGENTS_TMUX32 set from
//! `sh scripts/dev/tmux32.sh`.

#[cfg(unix)]
#[path = "support/launch_evidence.rs"]
mod launch_evidence;

#[cfg(unix)]
mod support;

#[cfg(unix)]
use support::{Dir, Mux, Server};

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

        /// The bar's width by S1 (#59): `max(min, window * percent / 100)` of the width its window reads now, with
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
            let offset = std::fs::read_to_string(mail.join(format!("{key}.offset")))
                .ok()
                .and_then(|t| t.trim().parse().ok())
                .unwrap_or(0);
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
            self.server.client_by_session(s(&spawned["sessionId"]))["pid"]
                .as_u64()
                .unwrap()
        }

        fn client_of(&self, id: &str) -> u64 {
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
        reply["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                s(&m["text"])
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
        let out = Command::new("ps")
            .args(["-axo", "pid=,ppid=,comm="])
            .output()
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
        let out = Command::new("ps")
            .args(["-o", "tty=", "-p", &pid.to_string()])
            .output()
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

    // spawn writes server.json, record.json, spec.json and last commit.json, tags the pane, and its launcher (the
    // core-owned process) runs the PATH client with the record's session.
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
            ("pane", json!(pane)),
            ("window", w1["window"].clone()),
            ("parentPane", json!(root)),
            ("parentAgent", Value::Null),
            ("directory", json!(live.path("work"))),
            ("taskLine", json!("say hi")),
        ];
        for (field, value) in want {
            assert_eq!(record[field], value, "{mux:?} record.{field}");
        }
        assert!(folder.join("commit.json").is_file(), "{mux:?}");
        let spec = json_file(&folder.join("spec.json"));
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

    fn core_launcher(live: &Live, w: &Value, client: &Value) -> Value {
        let launcher = json_file(&live.agent_dir(s(&w["id"])).join("launcher.json"));
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
        assert_eq!(
            (&sent["to"], &sent["pane"]),
            (&json!("w1"), &w1["pane"]),
            "{mux:?}"
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
        assert_eq!(
            (&sent["to"], &sent["pane"]),
            (&json!("parent"), &json!(root)),
            "{mux:?}"
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
        assert!(
            s(&read["messages"][0]["text"]).starts_with(&head),
            "{mux:?} {read}"
        );
        let (_, again) = live.run(&["read"]);
        assert_eq!(again["messages"], json!([]), "{mux:?}");
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
        for w in &workers {
            let sid = s(&w["sessionId"]);
            let pid = live.worker_pid(w);
            std::fs::write(
                live.dir.join(format!("reply-{sid}.md")),
                format!("reply from {sid}"),
            )
            .unwrap();
            let reader: &[&str] = &["read", "-Wait", "30"];
            let job = live.bg(&[reader])[0];
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
            let messages = reply["messages"].as_array().unwrap();
            assert_eq!(messages.len(), 1, "{mux:?} {reply}");
            got.push(format!("{}:{}", s(&messages[0]["from"]), bodies(&reply)[0]));
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
        // #90: with no client attached nobody is looking at an agent, so -Next goes to the most urgent: w1, which
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
            json!({"ok":true,"verb":"stop","name":w1["name"],"hcAddress":w1["hcAddress"],"life":"ended"})
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
            json!({"ok":true,"verb":"stop","name":w2["name"],"hcAddress":w2["hcAddress"],"life":"ended"})
        );
        assert_eq!(code, 0);
        let (code, read) = live.run(&["read", "-Wait", "30"]);
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
            let launcher = json_file(&live.agent_dir(s(&w["id"])).join("launcher.json"));
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
                json!({"ok":true,"verb":"stop","name":w["name"],"hcAddress":w["hcAddress"],"life":"ended"})
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
        let folder = live.agent_dir(s(&w["id"]));
        let a = s(&w["hcAddress"]);
        assert!(eventually(
            LONG,
            || core(live, &["get", a, "--json"])["state"] == "finished"
        ));
        assert!(!folder.join("notice.json").exists());
        assert!(exit_messages(live, w).is_empty());
        let (status, list) = live.run(&["list"]);
        assert_eq!(status, 0, "{list}");
        let notice = json_file(&folder.join("notice.json"));
        assert_eq!(
            (&notice["kind"], &notice["exitCode"]),
            (&json!("exited"), &json!(code))
        );
        assert_eq!(json_file(&folder.join("launcher.json"))["exitCode"], code);
        let (status, read) = live.run(&["read"]);
        assert_eq!(
            (status, bodies(&read)),
            (
                0,
                vec![format!("{} exited with code {code}.", s(&w["name"]))]
            )
        );
        assert_eq!(live.run(&["read"]).1["messages"], json!([]));
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
        let dead = Command::new(live.server.binary())
            .arg("-S")
            .arg(format!("{}.none", live.server.socket().display()))
            .args(["-N", "display-message", "-p", "-t", p2, probe])
            .output()
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

    // S2, #82, #83: the bar draws in color at its pane's size with nothing stray; an agent in `read -Wait` with no
    // report shows `idle` until mail wakes it, then `working` again, and its waiting file is gone. An agent whose
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
            // Two states, so herdr's headings over compact rows: idle `◌ w1`, working `◉ w2`.
            screen.contains("\u{25cc} w1")
                && screen.contains("\u{25c9} w2")
                && screen.contains("Idle \u{b7} 1")
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
                && screen.contains("Idle \u{b7} 1")
                && screen.contains("1 need you")
                && matches!(rows, [Some(a), Some(b)] if a < b)
        });
        assert!(blocked, "{mux:?}: the bar shows:\n{screen}");
        assert!(screen.contains("! w2"), "{mux:?}:\n{screen}");
        // #83: nobody looks at w2's pane, so the bar tells the session's clients once. tmux 3.8 keeps the command in
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
            // Its watch ended, the bar is no longer provable (#47): the spawn names no bar and opens no second one.
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
                    // #97: the agent's own hc terminal, as the core sees it, so a missing view names its cause.
                    let address = s(&spawned["hcAddress"]);
                    let mut core = String::new();
                    for args in [
                        vec!["get", address, "--json"],
                        vec!["read", address, "--lines", "40"],
                    ] {
                        let out = live.server.core.as_ref().map(|c| c.command(&args).output());
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
    // width changes; resize-window -x 300 makes it 60 within one tick (#74: by default 20% of the window, at least
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

    // #59, #74: by default the bar is 20% of its window, at least 40 columns; a width option changed while it runs
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

    // #47 test 2: the bar's pane respawned to anything (a shell naming the old pid and `watch`, or this binary
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

    // #47 test 4: rmux 0.10.0's rotate-window leaves the bar's label on the user's pane. sidebar -Close still closes
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

    // #47 test 8: a pane in another window of the session carrying the label without proof (a 0.2.1 bar) refuses
    // -Close, which closes no bar: nothing is killed. #103: a label refuses only a bar of its own window, so sidebar
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
        let found = Command::new("pgrep")
            .args(["-P", &shell, "-f", "watch"])
            .output()
            .unwrap();
        let watch: u32 = String::from_utf8_lossy(&found.stdout)
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{mux:?}: no manual watch under {shell}: {found:?}"));
        let start = mux_agent::proc::host()
            .row(watch)
            .and_then(|row| row.start)
            .unwrap();
        // #103: the record a watch reads is its window's.
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
            // A live row then carries its activity (#82): no report yet, so working since nothing.
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
                s(&r["startCommand"])
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
        let out = Command::new(env!("CARGO"))
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
            .arg(&target)
            .output()
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

    // #103: each view's window keeps its own bar. The first spawn (a tab) opens one in its new window; a split
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

    // #103: spawns at once, each a tab, open one bar in each one's window; a spawn racing sidebar -Close never leaves
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

    // #103: three agents in three windows each show one bar, in its own window and proven by its window's record
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

    // #103 QA: break-pane moves the window's last view to a new window. The old bar, before it leaves, gives that
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
            live.agent_dir(s(&w2["id"])).join("commit.json").is_file(),
            "{mux:?}"
        );
        held.unlock().unwrap();
        drop(held);
        assert_eq!(live.tagged().len(), 2, "{mux:?}");
        // #103: w3's own window gets its own bar; w1's stays.
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
        let exit = format!("exit-{}", s(&w["id"]));
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
        assert!(!live.agent_dir(s(&w["id"])).join("notice.json").exists());
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
        assert!(!live.agent_dir(s(&w["id"])).join("notice.json").exists());
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
        // Both beside the caller (#103): the window keeps w2's view, and so its bar, when w1's pane goes.
        let w1 = live.spawn("w1", &["-Split", "h"]);
        let w2 = live.spawn("w2", &["-Split", "h"]);
        let bar = s(&w1["sidebar"]).to_string();
        let pid = live.worker_pid(&w1);
        live.worker_pid(&w2);
        live.log_frames(&bar);
        live.call(&["kill-pane", "-t", s(&w1["pane"])]);
        live.wait_gone(s(&w1["pane"]));
        live.wait_tick(&bar);
        let screen = live.call(&["capture-pane", "-p", "-t", &bar]);
        assert!(row_at(&screen, "w1").is_some() && row_at(&screen, "w2").is_some());
        let marker = live.agent_dir(s(&w1["id"])).join("notice.json");
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
        let r = json_file(&live.agent_dir(s(&w["id"])).join("record.json"));
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

    /// #61: with clicks on (no MUX_AGENTS_ASCII), `w1` and `w2` spawned and both drawn on the bar, its frames logged
    /// and the root pane active; answers the bar and w1's pane.
    fn two_rows(live: &mut Live) -> (String, String) {
        live.assert_not_ascii();
        // Both beside the caller (#103): their window, the caller's, keeps the bar a client sees.
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
    /// session's pane order (#103: `-Split h` from the root puts w2 between the root and w1), so a key that takes the
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

    // #61: a left click on an agent's name line focuses that agent's pane, and the bar keeps redrawing after it.
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

    // #61: `j` selects the first agent drawn and Enter focuses it.
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

    // #90: `n` (herdr's next agent) from a pane that is no agent's goes to the bar's first agent, `N` to its last.
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
        // (#103: a window option, read with -w). tmux's
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

    // #61: a press read after the bar shrank, before it drew at its new size, may land on a row that moved: it is
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
        child: std::process::Child,
        master: std::fs::File,
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
            let child = attach.spawn().unwrap();
            let master = std::fs::File::from(master);
            let mut drain = master.try_clone().unwrap();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while matches!(drain.read(&mut buf), Ok(n) if n > 0) {}
            });
            Client { child, master }
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

    // #61 through a real client: tmux 3.8 attached on a 120x40 terminal with `mouse on` hands a click on w1's name
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

    // #91 through a real client: `p` in the bar opens herdr's peek, a popup over the client paging the first agent's
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
        // A default-shell that is no POSIX shell (QA #91): the popup runs /bin/sh itself.
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
        let notice = json_file(&live.agent_dir(s(&w1["id"])).join("notice.json"));
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

    // #48 test 9: two split agents, then rotate-window (rmux leaves the @agent-* tags at their places; tmux moves them
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
            assert_eq!((code, &sent["pane"]), (0, &json!(pane)), "{mux:?} {sent}");
            let mail = live.mailbox(s(&w["id"]));
            assert!(
                mail.iter().any(|m| m["body"] == body.as_str()),
                "{mux:?} {name}: {mail:?}"
            );
        }

        // stop w1 closes only w1's pane.
        let (code, stop) = live.run_in(&clean, &["stop", "w1"]);
        assert_eq!(code, 0);
        assert_eq!(
            stop,
            json!({"ok":true,"verb":"stop","name":"w1","hcAddress":w1["hcAddress"],"life":"ended"})
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
        assert_eq!((code, &sent["pane"]), (0, &json!(p2)), "{mux:?} {sent}");
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
            assert!(!folder.join("notice.json").exists());
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
        assert_eq!(after["pane"], focus["pane"]);
        assert_eq!(live.worker_pid(&w), pid);
        let (code, stop) = live.run(&["stop", "w1"]);
        assert_eq!(code, 0, "{stop}");
        assert_eq!(
            stop,
            json!({"ok":true,"verb":"stop","name":"w1","hcAddress":address,"life":"ended"})
        );
        assert!(eventually(LONG, || core(
            &live,
            &["get", address, "--json"]
        )["state"]
            == "finished"));
        assert_eq!(exit_messages(&live, &w).len(), 1);
    }

    // A worker whose view is closed still reads its own mail (#104 phase 1c): the person sends to it after the view
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
        let (code, again) = live.server.command(pid, &["read"]);
        assert_eq!(
            (code, &again["messages"]),
            (0, &json!([])),
            "{mux:?} {again}"
        );
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
        assert_eq!(
            at + 2,
            argv.len(),
            "{mux:?}: no task after the conversation: {argv:?}"
        );
        assert!(!argv.contains(&"--fork-session"), "{mux:?} {argv:?}");
        assert_eq!(client["exe"], "claude", "{mux:?}");
        let folder = live.agent_dir(s(&y["id"]));
        let record = json_file(&folder.join("record.json"));
        assert_eq!(record["resumedFrom"], w["id"], "{mux:?}");
        assert_eq!(record["sessionId"], sid.as_str(), "{mux:?}");
        // The client runs in Y's hc terminal: its parent is the launcher, the terminal's own process.
        let launcher = json_file(&folder.join("launcher.json"));
        assert_eq!(client["ppid"], launcher["pid"], "{mux:?}");
        assert_eq!(
            core(&live, &["get", s(&y["hcAddress"]), "--json"])["pid"],
            launcher["pid"],
            "{mux:?}"
        );
        let pane = s(&record["pane"]);
        assert_eq!(live.field(pane, "#{@agent-id}"), s(&y["id"]), "{mux:?}");
        // The bar is beside Y: a live bar in the window of Y's view (#122, a bar per window).
        let bar = s(&resumed["sidebar"]).to_string();
        // `bars()` answers `<pane>|<window>|<width>`: the reply's pane is a labelled bar there, in Y's window.
        assert!(
            live.bars().iter().any(|b| {
                let mut fields = b.split('|');
                fields.next() == Some(bar.as_str()) && fields.next() == record["window"].as_str()
            }),
            "{mux:?} {resumed} {:?}",
            live.bars()
        );
        assert_eq!(
            live.field(&bar, "#{window_id}"),
            s(&record["window"]),
            "{mux:?} {resumed}"
        );
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
            let (code, again) = live.run(&["up", "-Client", "claude"]);
            assert_eq!(
                (code, again["check"].as_str()),
                (2, Some("leader")),
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
            assert_eq!(
                replies.iter().filter(|r| r["ok"] == true).count(),
                1,
                "{mux:?} {replies:?}"
            );
            assert!(
                replies.iter().any(|r| r["check"] == "leader"),
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

    #[cfg(feature = "test-panic")]
    #[test]
    fn up_whose_launch_panics_leaves_no_session_under_hostile_settings() {
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
            assert_eq!(code, 1, "{mux:?} {out}");
            assert!(
                eventually(Duration::from_secs(10), || live
                    .call(&["list-sessions", "-F", "#{session_name}"])
                    .lines()
                    .count()
                    == 1),
                "{mux:?}"
            );
            assert!(!marker.exists(), "{mux:?}");
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
                    root_unsandboxed_terminal_driver_finished,
                    spawn_window_and_split_tagged,
                    spawn_titles_tab_and_pane,
                    unnamed_spawns_at_once_get_distinct_names,
                    spawn_tags_pane_writes_record_and_commit_runs_client,
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

    // tmux 3.2a's display-popup has no -T (QA #91): peek works there too.
    tests_on!(tmux32_peek, Tmux32, [bar_real_client_peek]);
    // break-pane is tmux's; rmux 0.10 is not known to have it.
    tests_on!(
        tmux32_break,
        Tmux32,
        [break_pane_of_the_last_view_gives_its_new_window_a_bar]
    );

    #[test]
    fn members_spawn_and_act_only_down_the_tree() {
        for mux in Mux::ALL {
            let Some(mut live) = Live::human(mux) else {
                continue;
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
                serde_json::from_str(
                    &std::fs::read_to_string(live.agent_dir(id).join(file)).unwrap(),
                )
                .unwrap()
            };
            let c1 = ok(&mut live, &leader, "c1");
            let flags = read(&live, &c1, "spec.json")["flags"].clone();
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
            // Live besides the leader: c1, c2, h1 (no cascade stop); three more make six, a 7th refuses.
            for name in ["c3", "c4", "c5"] {
                ok(&mut live, &leader, name);
            }
            let (code, r) = live.spawn_by(&leader, "c6");
            assert_eq!(
                (code, r["check"].as_str()),
                (2, Some("limit")),
                "{mux:?} {r}"
            );
        }
    }
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
