//! Classify validated hc terminal facts independently of any view.

use crate::hc::{Reason, State, Terminal};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Life {
    Running,
    Lost,
    Ended,
    Unknown,
}

pub fn classify(fact: Option<&Terminal>, launcher: &[String]) -> Life {
    let Some(t) = fact else {
        return Life::Unknown;
    };
    match t.state {
        State::Live if !launcher.is_empty() && t.argv == launcher => Life::Running,
        State::Live if recovery_shell(&t.argv) => Life::Lost,
        State::Finished if matches!(t.reason.as_ref(), Some(Reason::Exited | Reason::Killed)) => {
            Life::Ended
        }
        _ => Life::Unknown,
    }
}

/// The shell hc's keep puts in place of a lost program: a POSIX login shell (`<shell> -l`, macOS), a bare POSIX
/// shell (`<shell>`, Linux; call 43, observed `["/bin/sh"]`), or on Windows PowerShell with hc's prompt hook
/// (`<path>\pwsh.exe -NoExit -Command <hook>`, hc 0.1.21). Anything else is not proven lost.
fn recovery_shell(argv: &[String]) -> bool {
    let posix = |shell: &str| {
        ["sh", "bash", "zsh", "fish", "dash", "ksh"]
            .contains(&shell.rsplit('/').next().unwrap_or(""))
    };
    match argv {
        [shell] => posix(shell),
        [shell, flag] if flag == "-l" => posix(shell),
        [shell, no_exit, command, _hook] if no_exit == "-NoExit" && command == "-Command" => {
            let name = shell
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            ["pwsh", "pwsh.exe", "powershell", "powershell.exe"].contains(&name.as_str())
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hc, spawn::Out, testing::Fakes};
    use serde_json::{Value, json};
    #[test]
    fn hc_lifecycle_matrix() {
        let launcher = vec![
            "/bin/mux-agent".into(),
            "launcher".into(),
            "/s/spec.json".into(),
        ];
        let wire = |state: &str, reason: Value, argv: Value| {
            json!({
                "address": "t1", "state": state, "reason": reason, "argv": argv, "labels": {},
                "foreground": {"argv": ["/bin/zsh", "-l"], "pid": 8}, "exit_code": 7
            })
            .to_string()
        };
        let mut cases = vec![
            (wire("live", Value::Null, json!(launcher)), Life::Running),
            (
                wire("live", json!("exited"), json!(launcher)),
                Life::Running,
            ),
            (
                wire(
                    "live",
                    Value::Null,
                    json!(["/bin/mux-agent", "launcher", "/s/other.json"]),
                ),
                Life::Unknown,
            ),
            (
                wire("live", Value::Null, json!(["/bin/mux-agent", "launcher"])),
                Life::Unknown,
            ),
            (
                wire(
                    "live",
                    Value::Null,
                    json!(["/bin/mux-agent", "launcher", "/s/spec.json", "extra"]),
                ),
                Life::Unknown,
            ),
            (
                wire(
                    "live",
                    Value::Null,
                    json!(["/other/mux-agent", "launcher", "/s/spec.json"]),
                ),
                Life::Unknown,
            ),
            (
                wire("live", Value::Null, json!(["/bin/zsh", "-c"])),
                Life::Unknown,
            ),
            (
                wire("live", Value::Null, json!(["/bin/sh", "/x/run.sh"])),
                Life::Unknown,
            ),
            (
                wire("live", Value::Null, json!(["/bin/zsh", "-l", "extra"])),
                Life::Unknown,
            ),
            (
                wire("live", Value::Null, json!(["/usr/bin/not-a-shell", "-l"])),
                Life::Unknown,
            ),
            (wire("live", Value::Null, json!([])), Life::Unknown),
            (
                wire("finished", json!("exited"), json!(launcher)),
                Life::Ended,
            ),
            (
                wire("finished", json!("exited"), json!(["/bin/zsh", "-l"])),
                Life::Ended,
            ),
            (wire("finished", json!("killed"), json!([])), Life::Ended),
            (
                wire("finished", json!("server_stop"), json!(launcher)),
                Life::Unknown,
            ),
            (
                wire("finished", json!("new_reason"), json!([])),
                Life::Unknown,
            ),
            (wire("finished", Value::Null, json!([])), Life::Unknown),
        ];
        for shell in [
            "/bin/sh",
            "/bin/bash",
            "/bin/zsh",
            "/usr/bin/fish",
            "/bin/dash",
            "/bin/ksh",
        ] {
            cases.push((wire("live", Value::Null, json!([shell, "-l"])), Life::Lost));
        }
        // On Windows hc 0.1.21 keeps a lost program's terminal with PowerShell and its prompt hook (observed on a
        // private core restart): `<path>\pwsh.exe -NoExit -Command <hook>`.
        let hook = "$global:__hc_prompt = $function:prompt; function global:prompt { $p }";
        for shell in [
            r"C:\Users\u\AppData\Local\Microsoft\WindowsApps\pwsh.exe",
            r"C:\Program Files\PowerShell\7\PWSH.EXE",
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            "pwsh",
        ] {
            cases.push((
                wire(
                    "live",
                    Value::Null,
                    json!([shell, "-NoExit", "-Command", hook]),
                ),
                Life::Lost,
            ));
        }
        for argv in [
            json!([r"C:\x\pwsh.exe", "-NoExit"]),
            json!([r"C:\x\pwsh.exe", "-NoExit", "-Command"]),
            json!([r"C:\x\pwsh.exe", "-Command", hook]),
            json!([r"C:\x\pwsh.exe", "-NoExit", "-Command", hook, "extra"]),
            json!([r"C:\x\cmd.exe", "-NoExit", "-Command", hook]),
            json!([r"C:\x\zsh", "-l"]),
        ] {
            cases.push((wire("live", Value::Null, argv), Life::Unknown));
        }
        let mut no_foreground: Value =
            serde_json::from_str(&wire("live", Value::Null, json!(launcher))).unwrap();
        no_foreground["foreground"] = Value::Null;
        cases.push((no_foreground.to_string(), Life::Running));
        let outputs = cases.into_iter().map(|(stdout, want)| {
            (
                Ok(Out {
                    code: 0,
                    stdout,
                    stderr: String::new(),
                }),
                want,
            )
        });
        let failures = [
            Ok(Out {
                code: 0,
                stdout: "{}".into(),
                stderr: String::new(),
            }),
            Ok(Out {
                code: 1,
                stdout: String::new(),
                stderr: "missing".into(),
            }),
            Err(crate::error::Refusal::internal("unreachable")),
        ]
        .into_iter()
        .map(|answer| (answer, Life::Unknown));
        for (answer, want) in outputs.chain(failures) {
            let f = Fakes::default();
            f.hc_install("hc");
            f.hc_answer(Ok(Out {
                code: 0,
                stdout: "hc 0.1.21".into(),
                stderr: String::new(),
            }));
            f.hc_answer(answer);
            let ctx = f.ctx();
            let core = hc::connect(&ctx).unwrap();
            let fact = core.get("t1").ok();
            assert_eq!(classify(fact.as_ref(), &launcher), want, "{fact:?}");
            assert_eq!(f.hc_calls().len(), 2);
            assert_eq!(f.hc_calls()[1].1, ["get", "t1", "--json"]);
        }
        // Call 43: on Linux hc 0.1.21 keeps a lost program's terminal with a bare shell, no `-l` (observed in CI on
        // rmux, tmux 3.2a and 3.8: `"argv":["/bin/sh"]`, live, `keep: true`, labels kept). Only a listed POSIX
        // shell alone is Lost; a non-shell program or extra arguments are not, and the recorded launcher argv is
        // Running first even when it is a bare shell.
        let live = |argv: &[&str]| Terminal {
            address: "t1".into(),
            name: None,
            state: State::Live,
            reason: None,
            exit_code: None,
            argv: argv.iter().map(|s| s.to_string()).collect(),
            foreground: None,
            labels: Default::default(),
            pid: None,
        };
        for (argv, want) in [
            (&["/bin/sh"][..], Life::Lost),
            (&["/usr/bin/bash"], Life::Lost),
            (&["/bin/zsh"], Life::Lost),
            (&["/usr/bin/fish"], Life::Lost),
            (&["/bin/dash"], Life::Lost),
            (&["/bin/ksh"], Life::Lost),
            (&["/usr/bin/not-a-shell"], Life::Unknown),
            (&["/bin/mux-agent"], Life::Unknown),
            (&["/bin/sh", "extra"], Life::Unknown),
            (&["/bin/sh", "-l", "extra"], Life::Unknown),
            (&["/bin/sh", "-c", "x"], Life::Unknown),
            (&[""], Life::Unknown),
        ] {
            assert_eq!(classify(Some(&live(argv)), &launcher), want, "{argv:?}");
        }
        let bare: Vec<String> = vec!["/bin/sh".into()];
        assert_eq!(
            classify(Some(&live(&["/bin/sh"])), &bare),
            Life::Running,
            "the recorded launcher argv is Running first"
        );
        let live_with_empty_argv = Terminal {
            address: "t1".into(),
            name: None,
            state: State::Live,
            reason: None,
            exit_code: None,
            argv: Vec::new(),
            foreground: None,
            labels: Default::default(),
            pid: None,
        };
        assert_eq!(
            classify(Some(&live_with_empty_argv), &[]),
            Life::Unknown,
            "{live_with_empty_argv:?}"
        );
    }
}
