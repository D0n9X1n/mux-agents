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
