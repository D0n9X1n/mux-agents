//! The entry end to end: argv in, one reply line and an exit code out (MuxAgents.ps1:29-57, 1673-1681).

use std::process::Command;

const NL: &str = if cfg!(windows) { "\r\n" } else { "\n" };

// Never inside the caller's own multiplexer: no endpoint, so no call can reach a real server.
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

fn usage(message: &str) -> (i32, String, String) {
    let line =
        format!(r#"{{"ok":false,"verb":null,"code":2,"check":"usage","message":"{message}"}}{NL}"#);
    (2, line, String::new())
}

// Outside a multiplexer pane, a request that parses and passes its own checks reaches this refusal.
fn endpoint(verb: &str) -> (i32, String, String) {
    let line = format!(
        r#"{{"ok":false,"verb":"{verb}","code":1,"check":"endpoint","message":"not inside an rmux pane"}}{NL}"#
    );
    (1, line, String::new())
}

const SPAWN: &str = "usage: mux-agent spawn -Client <claude|copilot> [-Task <text>] [-TaskFile <path>] [-BriefFile <path>] [-Name <name>] [-Split <value>] [-Tab] [-NoFork] [-Text]";
const LIST: &str = "usage: mux-agent list [-All] [-Text]";
const FOCUS: &str = "usage: mux-agent focus <name> [-Text]";
const READ: &str = "usage: mux-agent read [-Wait <seconds>] | read <name> [-Lines <n>] [-Text]";

#[test]
fn no_args_usage_exit_2() {
    let all = usage(
        "usage: mux-agent <focus|list|placement|read|send|sidebar|spawn|status|stop|watch> ...",
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
            format!("code: 1\ncheck: endpoint\nmessage: not inside an rmux pane{NL}"),
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
        usage(SPAWN)
    );
    assert_eq!(
        mux_agent(&["spawn", "-Client", "claude", "-client", "copilot"]),
        usage(SPAWN)
    );
    assert_eq!(mux_agent(&["spawn", "-Client"]), usage(SPAWN));
    // Switches and -Text may repeat.
    assert_eq!(mux_agent(&["list", "-All", "-All", "-Text", "-Text"]).0, 1);
}

#[test]
fn unknown_dash_token_is_usage() {
    assert_eq!(mux_agent(&["list", "-Bogus"]), usage(LIST));
    assert_eq!(mux_agent(&["list", "-"]), usage(LIST));
    assert_eq!(mux_agent(&["focus", "-All", "x"]), usage(FOCUS));
    assert_eq!(mux_agent(&["list", "--All"]), usage(LIST));
}

#[test]
fn positional_bounds() {
    assert_eq!(mux_agent(&["focus"]), usage(FOCUS));
    assert_eq!(mux_agent(&["focus", "a", "b"]), usage(FOCUS));
    assert_eq!(mux_agent(&["focus", "a"]), endpoint("focus"));
    assert_eq!(mux_agent(&["list", "x"]), usage(LIST));
    assert_eq!(mux_agent(&["read"]), endpoint("read"));
    assert_eq!(mux_agent(&["read", "a", "-Lines", "5"]), endpoint("read"));
    assert_eq!(mux_agent(&["read", "a", "b"]), usage(READ));
    assert_eq!(
        mux_agent(&["watch", ""]),
        usage("usage: mux-agent watch [-Text]")
    );
}

#[test]
fn list_outside_mux_refuses_endpoint_1() {
    assert_eq!(mux_agent(&["list"]), endpoint("list"));
    assert_eq!(mux_agent(&["list", "-All"]), endpoint("list"));
}

// The hidden pane entry reads only a spec.json inside the state root; it is not a verb, so its refusal goes to
// stderr, not a JSON reply.
#[test]
fn launcher_relative_spec_exit_2() {
    let (code, stdout, stderr) = mux_agent(&["launcher", "spec.json"]);
    assert_eq!(
        (code, stdout.as_str(), stderr.trim_end()),
        (
            2,
            "",
            "mux-agent: the launch spec is outside the state folder"
        )
    );
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
