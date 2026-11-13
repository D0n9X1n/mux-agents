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
        State::Live
            if t.argv.len() == 2
                && t.argv[1] == "-l"
                && ["sh", "bash", "zsh", "fish", "dash", "ksh"]
                    .contains(&t.argv[0].rsplit('/').next().unwrap_or("")) =>
        {
            Life::Lost
        }
        State::Finished if matches!(t.reason.as_ref(), Some(Reason::Exited | Reason::Killed)) => {
            Life::Ended
        }
        _ => Life::Unknown,
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
        let live_with_empty_argv = Terminal {
            address: "t1".into(),
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
