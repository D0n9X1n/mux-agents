#![cfg(feature = "live")]
//! Live tests: real tmux 3.8, tmux 3.2a and rmux servers on private sockets, never the user's own. Each server starts
//! from a temp dir with its own HOME and state, `claude`/`copilot` on PATH as the fake client, and dies with its guard.
//! Run with `cargo test --features live --test live -- --nocapture`; tmux 3.2a needs MUX_AGENTS_TMUX32 set from
//! `sh scripts/dev/tmux32.sh`.

#[cfg(unix)]
mod support;

#[cfg(unix)]
use support::{Dir, Mux, Server};

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
        let shell = server.wait_file("shell.pid");
        assert_eq!(shell.trim(), server.pane_pid().to_string(), "{mux:?}");

        // The harness's env reaches the pane, and nothing of the caller's own session does.
        let env = server.pane_env();
        let get = |name: &str| {
            env.lines()
                .find_map(|l| l.strip_prefix(&format!("{name}=")))
                .map(str::to_string)
        };
        assert_eq!(get("HOME"), Some(dir.join("home").display().to_string()));
        assert_eq!(get("MUX_IT_DIR"), Some(dir.path().display().to_string()));
        // rmux puts its tmux shim first, inside the harness's TMPDIR; <dir>/bin comes before anything outside it.
        let path = get("PATH").unwrap();
        let first_outside = path
            .split(':')
            .find(|p| !p.starts_with(&dir.join("tmp").display().to_string()));
        assert_eq!(
            first_outside,
            Some(dir.join("bin").to_str().unwrap()),
            "{mux:?} PATH={path}"
        );
        for unset in [
            "XDG_STATE_HOME",
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
echo 'bypass permissions on (shift+tab to cycle)'
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

    // spawn writes server.json, record.json, spec.json and last commit.json, tags the pane, and its launcher (the
    // pane's process) runs the PATH client with the record's session.
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
        let pane_pid: u64 = live.field(pane, "#{pane_pid}").parse().unwrap();
        assert_eq!(client["ppid"], pane_pid, "{mux:?}");
        let launcher = json_file(&folder.join("launcher.json"));
        assert_eq!(launcher["pid"], pane_pid, "{mux:?}");
        assert!(launcher["start"].is_i64(), "{mux:?} {launcher}");
        let (code, list) = live.run(&["list"]);
        assert_eq!(
            (code, names(&list)),
            (0, vec!["w1".to_string()]),
            "{mux:?} {list}"
        );
    }

    // Each worker is the PATH claude, forked from the caller's session, the pane's own process's child, in its own
    // terminal.
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
            let pane_pid: u64 = live.field(pane, "#{pane_pid}").parse().unwrap();
            assert_eq!(client["ppid"], pane_pid, "{mux:?}");
            let launcher = json_file(&live.agent_dir(s(&w["id"])).join("launcher.json"));
            assert_eq!(launcher["pid"], pane_pid, "{mux:?}");
            let tty = tty_of(client["pid"].as_u64().unwrap());
            assert_eq!(tty, live.field(pane, "#{pane_tty}"), "{mux:?}");
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
        let reader: &[&str] = &["read", "-Wait", "60"];
        let job = live.bg(&[reader])[0];
        live.armed(job);
        let (code, stop) = live.run(&["stop", "w1"]);
        let at = Instant::now();
        assert_eq!(code, 0, "{mux:?} {stop}");
        assert_eq!(
            (&stop["name"], &stop["survivors"], &stop["survivorPids"]),
            (&json!("w1"), &json!(0), &json!([])),
            "{mux:?}"
        );
        assert_eq!(
            stop["resume"],
            format!("claude --resume {}", s(&w1["sessionId"])),
            "{mux:?}"
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
        assert_eq!((code, &stop["survivors"]), (0, &json!(0)), "{mux:?} {stop}");
        let (code, read) = live.run(&["read", "-Wait", "30"]);
        assert_eq!(
            (code, bodies(&read)),
            (0, vec!["w2 was stopped.".to_string()]),
            "{mux:?}"
        );
    }

    // stop closes each pane and leaves no process of it: no quit file, no kill bypass.
    fn stop_leaves_no_surviving_process(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let (mut pids, mut panes) = (Vec::new(), Vec::new());
        for (name, extra) in [("w1", &[][..]), ("w2", &["-Split", "h"][..])] {
            let w = live.spawn(name, extra);
            pids.push(live.worker_pid(&w) as u32);
            let launcher = json_file(&live.agent_dir(s(&w["id"])).join("launcher.json"));
            pids.push(launcher["pid"].as_u64().unwrap() as u32);
            panes.push(s(&w["pane"]).to_string());
        }
        for name in ["w1", "w2"] {
            let (code, stop) = live.run(&["stop", name]);
            assert_eq!(code, 0, "{mux:?} {stop}");
            assert_eq!(
                (&stop["survivors"], &stop["survivorPids"]),
                (&json!(0), &json!([])),
                "{mux:?}"
            );
        }
        for pane in &panes {
            live.wait_gone(pane);
        }
        eventually(Duration::from_secs(10), || {
            !pids.iter().any(|p| support::alive(*p))
        });
        let left: Vec<u32> = pids
            .iter()
            .copied()
            .filter(|p| support::alive(*p))
            .collect();
        assert_eq!(pids.len(), 4, "{mux:?}");
        assert_eq!(left, Vec::<u32>::new(), "{mux:?}: survivors of {pids:?}");
        assert!(!live.dir.join("quit").exists(), "{mux:?}");
        assert!(live.tagged().is_empty(), "{mux:?}");
    }

    // A client that exits posts one `exited` notice with its code.
    fn client_exit_posts_one_exited_notice(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        live.call(&["set-environment", "-g", "MUX_IT_EXIT", "7"]);
        let w1 = live.spawn("w1", &[]);
        let client = live.server.client_by_session(s(&w1["sessionId"]));
        assert_eq!(client["env"]["MUX_IT_EXIT"], "7", "{mux:?}");
        let pid = client["pid"].as_u64().unwrap();
        support::write_atomic(&live.dir.join(format!("quit.{pid}")), "");
        assert_eq!(
            live.server.wait_file(&format!("exited.{pid}")).trim(),
            "7",
            "{mux:?}"
        );
        live.wait_gone(s(&w1["pane"]));
        one_exited_notice(&mut live, &w1, 7);
    }

    // The single `exited` notice of `w` with `code`: notice.json, launcher.json, one mailbox line, one read.
    fn one_exited_notice(live: &mut Live, w: &Value, code: i64) {
        let mux = live.mux();
        let folder = live.agent_dir(s(&w["id"]));
        let rel = folder
            .strip_prefix(live.dir.path())
            .unwrap()
            .join("notice.json");
        let notice: Value =
            serde_json::from_str(&live.server.wait_file(rel.to_str().unwrap())).unwrap();
        assert_eq!(
            (&notice["kind"], &notice["exitCode"]),
            (&json!("exited"), &json!(code)),
            "{mux:?}"
        );
        assert_eq!(
            json_file(&folder.join("launcher.json"))["exitCode"],
            code,
            "{mux:?}"
        );
        let (status, read) = live.run(&["read"]);
        let want = format!("{} exited with code {code}.", s(&w["name"]));
        assert_eq!((status, bodies(&read)), (0, vec![want]), "{mux:?}");
        let (_, again) = live.run(&["read"]);
        assert_eq!(again["messages"], json!([]), "{mux:?}");
        assert_eq!(live.mailbox(&live.root_box()).len(), 1, "{mux:?}");
        let (_, all) = live.run(&["list", "-All"]);
        assert_eq!(
            (&all["agents"][0]["name"], &all["agents"][0]["state"]),
            (&w["name"], &json!("exited")),
            "{mux:?}"
        );
    }

    // A PATH `claude` that cannot start (its shebang names a missing interpreter, so the start fails NotFound) still
    // leaves exit code 1 and posts one `exited` notice, never `lost`.
    fn client_start_error_posts_one_exited_notice(mux: Mux) {
        use std::os::unix::fs::PermissionsExt;
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let claude = live.dir.join("bin/claude");
        std::fs::remove_file(&claude).unwrap();
        std::fs::write(&claude, "#!/nonexistent/mux-it/interpreter\n").unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let w1 = live.spawn("w1", &[]);
        live.wait_gone(s(&w1["pane"]));
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
        // mux-agent ends the gone pane's agent with one notice (lost, or exited if its launcher outlived the pane)
        // and keeps the live one.
        let (code, all) = live.run(&["list", "-All"]);
        assert_eq!(code, 0, "{mux:?} {all}");
        let state = |name: &str| {
            all["agents"]
                .as_array()
                .unwrap()
                .iter()
                .find(|a| a["name"] == name)
                .map(|a| s(&a["state"]).to_string())
        };
        assert_eq!(state("w2").as_deref(), Some("running"), "{mux:?} {all}");
        assert!(
            matches!(state("w1").as_deref(), Some("lost" | "exited")),
            "{mux:?} {all}"
        );
        assert!(
            live.agent_dir(s(&w1["id"])).join("notice.json").is_file(),
            "{mux:?}"
        );
        assert_eq!(live.mailbox(&live.root_box()).len(), 1, "{mux:?}");
    }

    // A 42-column bar's row: `^. w[12]  claude  \d+[sm]$`.
    fn bar_row(line: &str) -> bool {
        let Some(rest) = line.strip_prefix("\u{25cf} ") else {
            return false;
        };
        let Some(age) = rest
            .strip_prefix("w1  claude  ")
            .or_else(|| rest.strip_prefix("w2  claude  "))
        else {
            return false;
        };
        age.len() >= 2
            && age.ends_with(['s', 'm'])
            && age[..age.len() - 1].bytes().all(|b| b.is_ascii_digit())
    }

    fn bar_ready(screen: &str) -> bool {
        let lines: Vec<&str> = screen.lines().map(str::trim_end).collect();
        let count = |f: &dyn Fn(&str) -> bool| lines.iter().filter(|l| f(l)).count();
        count(&|l| l == "mux-agents \u{b7} 2 running") == 1
            && count(&|l| l == "  checking the side bar") == 2
            && count(&bar_row) == 2
    }

    // sidebar opens one 42-column bar beside the caller showing the count and each status, outside list; asked
    // again it answers the same bar; -Close ends it and its watch.
    fn sidebar_opens_42_columns_and_closes(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let workers = [live.spawn("w1", &[]), live.spawn("w2", &[])];
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
        assert_eq!((code, &open["opened"]), (0, &json!(true)), "{mux:?} {open}");
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
        assert_eq!(fields[..4], [bar.as_str(), "sidebar", "", "42"], "{mux:?}");
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
        let (code, all) = live.run(&["list", "-All"]);
        assert_eq!(code, 0, "{mux:?} {all}");
        let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        assert_eq!(keys(&all), ["ok", "verb", "running", "agents"], "{mux:?}");
        assert_eq!(
            (&all["ok"], &all["verb"], &all["running"]),
            (&json!(true), &json!("list"), &json!(1)),
            "{mux:?}"
        );
        let rows = all["agents"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "{mux:?} {all}");
        const ROW: [&str; 14] = [
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
        ];
        // w2's stop notice is unread in the root's mailbox, from w2's pane.
        for (row, (w, state, unread)) in rows.iter().zip([(&w1, "running", 0), (&w2, "stopped", 1)])
        {
            assert_eq!(keys(row), ROW, "{mux:?}");
            let want = json!({
                "name": w["name"], "client": "claude", "pane": w["pane"], "window": w["window"], "context": "brief",
                "parent": root, "sessionId": w["sessionId"], "directory": live.path("work"), "state": state,
                "doing": "say hi", "doingFrom": "task", "unread": unread,
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
        // The mode line scrolled out of the screen's last rows (a Shift+Tab the transcript has not recorded yet).
        let pid = live.root_pid;
        for n in 1..=5 {
            live.server.send_keys(&format!("noise {n}"));
        }
        live.server
            .wait_line(&format!("input.{pid}.log"), "noise 5");
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

    // Through every verb, mux-agent starts only the multiplexer and the clients.
    fn children_are_only_mux_and_client(mux: Mux) {
        let Some(mut live) = Live::human(mux) else {
            return;
        };
        let w1 = live.spawn("w1", &[]);
        let (code, w2) = live.try_spawn_client("copilot", "w2", &[]);
        assert_eq!(code, 0, "{mux:?} {w2}");
        live.worker_pid(&w1);
        live.worker_pid(&w2);
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
        let allowed = ["tmux", "rmux", "claude", "copilot", "fake-client"];
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

    macro_rules! every_mux_scenario {
        ($module:ident, $mux:ident) => {
            tests_on!(
                $module,
                $mux,
                [
                    root_unsandboxed_terminal_driver_finished,
                    spawn_window_and_split_tagged,
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
                    sidebar_opens_42_columns_and_closes,
                    list_all_json_shape,
                    agent_caller_forks_with_bypass_evidence,
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
            two_sidebars_one_session_one_split,
            children_are_only_mux_and_client,
            panic_in_lock_frees_lock,
        ]
    );
}
