//! The entry end to end: argv in, one reply line and an exit code out (MuxAgents.ps1:29-57, 1673-1681).

use std::process::Command;

const NL: &str = if cfg!(windows) { "\r\n" } else { "\n" };

fn mux_agent(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_mux-agent"))
        .args(args)
        .env_remove("MUX_AGENTS_TEST_PANIC")
        .output()
        .unwrap();
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

// Handlers are stubs until they are wired, so a request that parses reaches this refusal.
fn not_wired(verb: &str) -> (i32, String, String) {
    let line = format!(
        r#"{{"ok":false,"verb":"{verb}","code":1,"check":"internal","message":"not wired"}}{NL}"#
    );
    (1, line, String::new())
}

const SPAWN: &str = "usage: mux-agent spawn -Client <claude|copilot> [-Task <text>] [-TaskFile <path>] [-BriefFile <path>] [-Name <name>] [-Split <value>] [-NoFork] [-Text]";
const LIST: &str = "usage: mux-agent list [-All] [-Text]";
const FOCUS: &str = "usage: mux-agent focus <name> [-Text]";
const READ: &str = "usage: mux-agent read [-Wait <seconds>] | read <name> [-Lines <n>] [-Text]";

#[test]
fn no_args_usage_exit_2() {
    let all = usage("usage: mux-agent <focus|list|read|send|sidebar|spawn|status|stop|watch> ...");
    assert_eq!(mux_agent(&[]), all);
    assert_eq!(mux_agent(&["bogus", "-Text"]), all);
    assert_eq!(mux_agent(&["-Text"]), all);
}

#[test]
fn verb_and_flags_match_without_case() {
    assert_eq!(
        mux_agent(&["LIST", "-all", "-TEXT"]),
        (
            1,
            format!("code: 1\ncheck: internal\nmessage: not wired{NL}"),
            String::new()
        )
    );
    assert_eq!(mux_agent(&["List", "-ALL"]), not_wired("list"));
    assert_eq!(
        mux_agent(&["spawn", "-client", "claude", "-NOFORK"]),
        not_wired("spawn")
    );
    // A value flag takes the next token whatever it looks like, so this -Text is the task.
    assert_eq!(
        mux_agent(&["spawn", "-Client", "claude", "-Task", "-Text"]),
        not_wired("spawn")
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
    assert_eq!(mux_agent(&["focus", "a"]), not_wired("focus"));
    assert_eq!(mux_agent(&["list", "x"]), usage(LIST));
    assert_eq!(mux_agent(&["read"]), not_wired("read"));
    assert_eq!(mux_agent(&["read", "a", "-Lines", "5"]), not_wired("read"));
    assert_eq!(mux_agent(&["read", "a", "b"]), usage(READ));
    assert_eq!(
        mux_agent(&["watch", ""]),
        usage("usage: mux-agent watch [-Text]")
    );
}
