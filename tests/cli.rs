//! The entry end to end: argv in, one reply line and an exit code out (MuxAgents.ps1:29-57, 1673-1681).

use std::process::Command;

const NL: &str = if cfg!(windows) { "\r\n" } else { "\n" };

// Never inside the caller's own multiplexer: no endpoint, so no call can reach a real server. Never in a herdr terminal
// either: run from one, every verb would refuse `herdr` before the check a test names.
fn command(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mux-agent"));
    command.args(args);
    for name in [
        "MUX_AGENTS_TEST_PANIC",
        "TMUX",
        "RMUX",
        "TMUX_PANE",
        "SANDBOX_RUNTIME",
        "MUX_AGENTS_STATE",
        "HC_TERMINAL_ID",
    ] {
        command.env_remove(name);
    }
    command
}

fn mux_agent(args: &[&str]) -> (i32, String, String) {
    output(command(args))
}

fn output(mut command: Command) -> (i32, String, String) {
    let out = command.output().unwrap();
    let text = |bytes: Vec<u8>| String::from_utf8(bytes).unwrap();
    (
        out.status.code().unwrap(),
        text(out.stdout),
        text(out.stderr),
    )
}

// An unknown or missing verb: no verb named.
fn usage(message: &str) -> (i32, String, String) {
    let line =
        format!(r#"{{"ok":false,"verb":null,"code":2,"check":"usage","message":"{message}"}}{NL}"#);
    (2, line, String::new())
}

// A known verb's own usage refusal names it.
fn verb_usage(verb: &str, message: &str) -> (i32, String, String) {
    let line = format!(
        r#"{{"ok":false,"verb":"{verb}","code":2,"check":"usage","message":"{message}"}}{NL}"#
    );
    (2, line, String::new())
}

// Outside a multiplexer pane, a request that parses and passes its own checks reaches this refusal; with neither
// RMUX nor TMUX set it names both.
fn endpoint(verb: &str) -> (i32, String, String) {
    endpoint_in(verb, "a tmux or rmux")
}

fn endpoint_in(verb: &str, mux: &str) -> (i32, String, String) {
    let line = format!(
        r#"{{"ok":false,"verb":"{verb}","code":1,"check":"endpoint","message":"not inside {mux} pane"}}{NL}"#
    );
    (1, line, String::new())
}

const SPAWN: &str = "usage: mux-agent spawn -Client <claude|copilot> [-Task <text>] [-TaskFile <path>] [-BriefFile <path>] [-Name <name>] [-Model <model>] [-Split <value>] [-Tab] [-NoFork] [-Text]";
const LIST: &str = "usage: mux-agent list [-All] [-Text]";
const FOCUS: &str = "usage: mux-agent focus (<name> | -Next | -Prev) [-Text]";
const READ: &str = "usage: mux-agent read [-Wait <seconds>] | read <name> [-Lines <n>] [-Text]";

#[test]
fn no_args_usage_exit_2() {
    let all = usage(
        "usage: mux-agent <focus|list|placement|read|report|resume|send|sidebar|spawn|status|stop|version|watch> ...",
    );
    assert_eq!(mux_agent(&[]), all);
    assert_eq!(mux_agent(&["bogus", "-Text"]), all);
    assert_eq!(mux_agent(&["-Text"]), all);
}

// What the parser hands the verb (canonical names, values, -Text, every verb's bounds) is checked by the
// unit tests in src/cli.rs; these check the bytes and exit code the entry produces.
#[test]
fn verb_and_flags_match_without_case() {
    assert_eq!(
        mux_agent(&["LIST", "-all", "-TEXT"]),
        (
            1,
            format!("code: 1\ncheck: endpoint\nmessage: not inside a tmux or rmux pane{NL}"),
            String::new()
        )
    );
    assert_eq!(mux_agent(&["List", "-ALL"]), endpoint("list"));
    // No task: the verb's own usage refusal, so the verb is named.
    let spawn_usage = format!(
        r#"{{"ok":false,"verb":"spawn","code":2,"check":"usage","message":"{SPAWN}"}}{NL}"#
    );
    assert_eq!(
        mux_agent(&["spawn", "-client", "claude", "-NOFORK"]),
        (2, spawn_usage, String::new())
    );
    // A value flag takes the next token whatever it looks like, so this -Text is the task.
    assert_eq!(
        mux_agent(&["spawn", "-Client", "claude", "-Task", "-Text"]),
        endpoint("spawn")
    );
}

#[test]
fn value_flag_twice_is_usage() {
    assert_eq!(
        mux_agent(&["spawn", "-Client", "claude", "-Client", "copilot"]),
        verb_usage("spawn", SPAWN)
    );
    assert_eq!(
        mux_agent(&["spawn", "-Client", "claude", "-client", "copilot"]),
        verb_usage("spawn", SPAWN)
    );
    assert_eq!(mux_agent(&["spawn", "-Client"]), verb_usage("spawn", SPAWN));
    // Switches and -Text may repeat.
    assert_eq!(mux_agent(&["list", "-All", "-All", "-Text", "-Text"]).0, 1);
}

#[test]
fn unknown_dash_token_is_usage() {
    assert_eq!(mux_agent(&["list", "-Bogus"]), verb_usage("list", LIST));
    assert_eq!(mux_agent(&["list", "-"]), verb_usage("list", LIST));
    assert_eq!(
        mux_agent(&["focus", "-All", "x"]),
        verb_usage("focus", FOCUS)
    );
    assert_eq!(mux_agent(&["list", "--All"]), verb_usage("list", LIST));
}

// A known verb's usage refusal honours -Text wherever it stands; an unknown verb's stays JSON with no verb.
#[test]
fn usage_refusal_keeps_verb_and_text() {
    let text = (
        2,
        format!("code: 2\ncheck: usage\nmessage: {LIST}{NL}"),
        String::new(),
    );
    assert_eq!(mux_agent(&["list", "-Bogus", "-Text"]), text);
    assert_eq!(mux_agent(&["LIST", "-text", "x"]), text);
    let all = usage(
        "usage: mux-agent <focus|list|placement|read|report|resume|send|sidebar|spawn|status|stop|version|watch> ...",
    );
    assert_eq!(mux_agent(&["bogus", "-Bogus", "-Text"]), all);
}

// version needs no pane, no state root and no caller: it answers outside a multiplexer, with a state root that
// would refuse every other verb.
#[test]
fn version_outside_mux() {
    let version = env!("CARGO_PKG_VERSION");
    let json = (
        0,
        format!(r#"{{"ok":true,"verb":"version","version":"{version}"}}{NL}"#),
        String::new(),
    );
    let text = (0, format!("version {version}{NL}"), String::new());
    assert_eq!(mux_agent(&["version"]), json);
    assert_eq!(mux_agent(&["--version"]), json);
    assert_eq!(mux_agent(&["version", "-Text"]), text);
    assert_eq!(mux_agent(&["--version", "-Text"]), text);
    for args in [&["version"][..], &["--version"]] {
        let mut call = command(args);
        call.env("MUX_AGENTS_STATE", "relative/state")
            .env("SANDBOX_RUNTIME", "1");
        assert_eq!(output(call), json, "{args:?}");
    }
    assert_eq!(
        mux_agent(&["version", "x"]),
        verb_usage("version", "usage: mux-agent version [-Text]")
    );
}

#[test]
fn positional_bounds() {
    assert_eq!(mux_agent(&["focus"]), verb_usage("focus", FOCUS));
    assert_eq!(mux_agent(&["focus", "a", "b"]), verb_usage("focus", FOCUS));
    assert_eq!(mux_agent(&["focus", "a"]), endpoint("focus"));
    assert_eq!(mux_agent(&["list", "x"]), verb_usage("list", LIST));
    assert_eq!(mux_agent(&["read"]), endpoint("read"));
    assert_eq!(mux_agent(&["read", "a", "-Lines", "5"]), endpoint("read"));
    assert_eq!(mux_agent(&["read", "a", "b"]), verb_usage("read", READ));
    assert_eq!(
        mux_agent(&["watch", ""]),
        verb_usage("watch", "usage: mux-agent watch [-Text]")
    );
}

#[test]
fn list_outside_mux_refuses_endpoint_1() {
    assert_eq!(mux_agent(&["list"]), endpoint("list"));
    assert_eq!(mux_agent(&["list", "-All"]), endpoint("list"));
}

// The refusal names the multiplexer the caller's variable names, even when its value is unusable: RMUX before TMUX.
#[test]
fn endpoint_refusal_names_the_mux() {
    for (vars, mux) in [
        (&[("TMUX", "relative,1,0")][..], "a tmux"),
        (&[("RMUX", "relative,1,0")], "an rmux"),
        (
            &[("RMUX", "relative,1,0"), ("TMUX", "relative,1,0")],
            "an rmux",
        ),
        (&[("RMUX", ""), ("TMUX", "relative,1,0")], "a tmux"),
    ] {
        let mut call = command(&["list"]);
        call.envs(vars.iter().copied());
        assert_eq!(output(call), endpoint_in("list", mux), "{vars:?}");
    }
}

// The hidden pane entry reads only a spec.json inside the state root; it is not a verb, so its refusal goes to
// stderr, not a JSON reply.
#[test]
fn launcher_relative_spec_exit_2() {
    let root = std::env::temp_dir();
    let root = root.to_str().unwrap();
    let (code, stdout, stderr) =
        mux_agent(&["launcher", "spec.json", "--state-root", root, "--path", ""]);
    assert_eq!(
        (code, stdout.as_str(), stderr.trim_end()),
        (
            2,
            "",
            "mux-agent: the launch spec is outside the state folder"
        )
    );
    // The v0.2 two-argument form is no launch any more: it is a usage refusal like any unknown verb (#104).
    let (code, stdout, _) = mux_agent(&["launcher", "spec.json"]);
    assert_eq!(code, 2);
    assert!(stdout.contains(r#""check":"usage""#), "{stdout}");
}

// The providers are built before the arguments are parsed, as the reference's entry did, so a bad state root
// refuses with no verb even for a call that would not parse.
#[cfg(unix)]
#[test]
fn relative_state_root_refuses_state_1() {
    let want = (
        1,
        format!(
            r#"{{"ok":false,"verb":null,"code":1,"check":"state","message":"MUX_AGENTS_STATE is not an absolute path"}}{NL}"#
        ),
        String::new(),
    );
    for args in [&["list"][..], &["bogus"], &["list", "-Text"], &[]] {
        let mut call = command(args);
        call.env("MUX_AGENTS_STATE", "relative/state");
        assert_eq!(output(call), want, "{args:?}");
    }
}

// A relative HOME would resolve against the cwd, where a planted transcript could pass a check; with a valid state
// root it still refuses before any verb runs.
#[cfg(unix)]
#[test]
fn relative_home_refuses_state_1() {
    let state = std::env::temp_dir();
    assert!(state.is_absolute());
    let mut call = command(&["list"]);
    call.env("MUX_AGENTS_STATE", &state)
        .env("HOME", "relative/home");
    assert_eq!(
        output(call),
        (
            1,
            format!(
                r#"{{"ok":false,"verb":null,"code":1,"check":"state","message":"HOME is not an absolute path"}}{NL}"#
            ),
            String::new()
        )
    );
}

// `watch` reaches its own handler through the verb table: with stdout a pipe it refuses as only watch does.
#[test]
fn watch_dispatches_to_watch_handler() {
    assert_eq!(
        mux_agent(&["watch"]),
        (
            2,
            format!(
                r#"{{"ok":false,"verb":"watch","code":2,"check":"usage","message":"watch draws only to a terminal: it runs in the side-bar pane"}}{NL}"#
            ),
            String::new()
        )
    );
}

// #93: a Copilot hook runs `report -Quiet`; the binary prints nothing at all (not even a line end) and exits 0, here
// outside a multiplexer where report refuses.
#[test]
fn report_quiet_prints_no_byte() {
    let out = Command::new(env!("CARGO_BIN_EXE_mux-agent"))
        .args(["report", "working", "-Quiet"])
        .env_remove("TMUX")
        .env_remove("RMUX")
        .env_remove("TMUX_PANE")
        .env_remove("HC_TERMINAL_ID")
        .output()
        .unwrap();
    assert_eq!(
        (out.status.code(), out.stdout, out.stderr),
        (Some(0), Vec::new(), Vec::new())
    );
}

#[cfg(feature = "live")]
#[test]
fn scoped_launcher_runs_once_with_explicit_context() {
    use serde_json::{Value, json};
    use std::{fs, path::PathBuf};
    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let t = Temp(
        fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("mux-scope-{}", mux_agent::util::hex_id().unwrap())),
    );
    for sub in ["bin", "work", "state/key/agents/aaaaaaaaaaaa"] {
        fs::create_dir_all(t.0.join(sub)).unwrap();
    }
    let bin = t.0.join("bin");
    let work = t.0.join("work");
    let state = t.0.join("state");
    let folder = state.join("key/agents/aaaaaaaaaaaa");
    let spec = folder.join("spec.json");
    fs::copy(
        env!("CARGO_BIN_EXE_fake-client"),
        bin.join(format!("claude{}", std::env::consts::EXE_SUFFIX)),
    )
    .unwrap();
    fs::write(&spec, json!({"schema":1,"generation":"0","client":"claude","flags":["--dangerously-skip-permissions","--effort","high"],"args":["--session-id","0f11fc2f-2032-48ef-a920-4af2a8e5af50"]}).to_string()).unwrap();
    fs::write(folder.join("commit.json"), "{}").unwrap();
    fs::write(state.join("key/server.json"), "{}").unwrap();
    fs::write(
        folder.join("record.json"),
        json!({"id":"aaaaaaaaaaaa","name":"worker","session":"$3","parentPane":"%1","pane":"%9"})
            .to_string(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&state, &state.join("key"), &state.join("key/agents")] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    fs::write(t.0.join("quit"), "").unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_mux-agent"))
            .args([
                "launcher",
                spec.to_str().unwrap(),
                "--state-root",
                state.to_str().unwrap(),
                "--path",
                bin.to_str().unwrap(),
            ])
            .current_dir(&work)
            .env("MUX_AGENTS_STATE", "relative-hostile-root")
            .env("PATH", "")
            .env("MUX_IT_DIR", &t.0)
            .env("MUX_IT_EXIT", "0")
            .env_remove("MUX_AGENTS_TEST_PANIC")
            .env_remove("TMUX")
            .env_remove("RMUX")
            .env_remove("TMUX_PANE")
            .output()
            .unwrap()
    };
    let first = run();
    assert!(first.status.success(), "scoped launch failed: {first:?}");
    assert!(folder.join("gens/0.claim").is_file());
    let log = fs::read_to_string(t.0.join("clients.log")).unwrap();
    let pid = log.trim();
    let client: Value =
        serde_json::from_slice(&fs::read(t.0.join(format!("client.{pid}.json"))).unwrap()).unwrap();
    let state_text = state.display().to_string();
    let work_text = work.display().to_string();
    assert_eq!(client["env"]["MUX_AGENTS_STATE"], state_text);
    assert_eq!(
        client["cwd"].as_str().unwrap().trim_start_matches(r"\\?\"),
        work_text.trim_start_matches(r"\\?\")
    );
    assert_eq!(client["env"]["CLAUDE_CODE_SESSION_ID"], Value::Null);
    assert!(run().status.success());
    assert_eq!(fs::read_to_string(t.0.join("clients.log")).unwrap(), log);
    assert!(!folder.join("notice.json").exists());
}
