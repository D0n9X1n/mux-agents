//! Replay-safe client options (#103 options-6e E1, E2): a launch argv split into options, session selectors and
//! prompt; records keep options and prompt apart and relaunches replay options only.
use crate::error::{Res, refuse};
use crate::parse::Client;

use super::team::{Arg, args};

/// A classified launch argv.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Classified {
    /// Known flags with their values, in order, each in its given spelling.
    pub options: Vec<String>,
    /// Positionals, the prompt flags and their values, and everything from the delimiter on.
    pub prompt: Vec<String>,
}

/// Session selectors: never stored; the recipe alone chooses the session.
const SESSION: [&str; 6] = [
    "--resume",
    "-r",
    "--session-id",
    "--continue",
    "-c",
    "--fork-session",
];
/// The flags that carry the task: prompt, never options.
const CLAUDE_PROMPT: [&str; 3] = ["-p", "--print", "--prompt"];
const COPILOT_PROMPT: [&str; 4] = ["-p", "--prompt", "-i", "--interactive"];

/// E1: each token into options, session selectors (dropped) or prompt, read by `team::args`, the grammar the ceiling
/// reads; an unknown flag refuses 2 `usage`.
pub fn classify(client: Client, argv: &[String]) -> Res<Classified> {
    let prompt: &[&str] = match client {
        Client::Claude => &CLAUDE_PROMPT,
        Client::Copilot => &COPILOT_PROMPT,
    };
    let mut out = Classified::default();
    for arg in args(client, argv)? {
        match arg {
            Arg::Positional(p) => out.prompt.push(p),
            Arg::Rest(rest) => out.prompt.extend(rest),
            // Dropped: the recipe chooses the session.
            Arg::Flag { name, .. } if SESSION.contains(&name.as_str()) => {}
            Arg::Flag { name, tokens, .. } if prompt.contains(&name.as_str()) => {
                out.prompt.extend(tokens)
            }
            Arg::Flag { tokens, .. } => out.options.extend(tokens),
        }
    }
    Ok(out)
}

/// E1, E2: the stored options a relaunch replays; refuses anything that is not purely options or that is above the
/// team ceiling (phase 3 D6).
pub fn replay(client: Client, stored: &[String], ceiling: super::team::Policy) -> Res<Vec<String>> {
    let again = classify(client, stored)?;
    if !again.prompt.is_empty() || again.options != stored {
        return refuse(
            2,
            "usage",
            "stored options carry a prompt or a session choice",
        );
    }
    // The policy of every spelling classify accepts; one it cannot classify is above any ceiling (fail closed).
    let within =
        super::team::policy_of(client, stored).is_some_and(|p| super::team::within(ceiling, p));
    if !within {
        return refuse(2, "ceiling", "the replayed options exceed the team ceiling");
    }
    Ok(again.options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::team::{Policy, policy_of};

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }
    fn c(client: Client, a: &[&str]) -> Classified {
        classify(client, &v(a)).unwrap()
    }
    fn refused<T>(r: Res<T>) -> Option<(u8, String)> {
        r.err().map(|e| (e.code, e.check.to_string()))
    }
    fn usage() -> Option<(u8, String)> {
        Some((2, "usage".into()))
    }
    fn ceiling() -> Option<(u8, String)> {
        Some((2, "ceiling".into()))
    }

    #[test]
    fn options_prompt_and_session_selectors_are_told_apart() {
        let got = c(Client::Claude, &["--model", "x", "fix"]);
        assert_eq!(
            (got.options, got.prompt),
            (v(&["--model", "x"]), v(&["fix"]))
        );
        assert_eq!(c(Client::Claude, &["--model=x"]).options, v(&["--model=x"]));
        assert_eq!(c(Client::Claude, &["-nworker"]).options, v(&["-nworker"]));
        let got = c(
            Client::Claude,
            &["-p", "--prompt", "v", "--effort", "high", "--", "-x", "y"],
        );
        assert_eq!(got.options, v(&["--effort", "high"]));
        assert_eq!(got.prompt, v(&["-p", "--prompt", "v", "--", "-x", "y"]));
        for drop in [
            &["--resume", "s"][..],
            &["--session-id=s"],
            &["--continue"],
            &["-c"],
            &["--fork-session"],
        ] {
            assert_eq!(c(Client::Claude, drop), Classified::default(), "{drop:?}");
        }
        let got = c(
            Client::Copilot,
            &["--allow-all", "-i", "task", "--resume=s", "-p", "q"],
        );
        assert_eq!(got.options, v(&["--allow-all"]));
        assert_eq!(got.prompt, v(&["-i", "task", "-p", "q"]));
        assert_eq!(
            c(Client::Copilot, &["--session-id", "s", "-r"]),
            Classified::default()
        );
    }

    #[test]
    fn an_unknown_flag_refuses_usage() {
        for argv in [&["--frobnicate"][..], &["--verbose=1"], &["--model"]] {
            assert_eq!(
                refused(classify(Client::Claude, &v(argv))),
                usage(),
                "{argv:?}"
            );
        }
    }

    #[test]
    fn replay_yields_only_options_within_the_ceiling() {
        let stored = v(&["--model", "x", "--dangerously-skip-permissions"]);
        assert_eq!(
            replay(Client::Claude, &stored, Policy::Bypass).ok(),
            Some(stored.clone())
        );
        assert_eq!(
            refused(replay(Client::Claude, &stored, Policy::Default)),
            ceiling()
        );
        for bad in [&["fix"][..], &["--resume", "s"], &["-p"], &["--", "x"]] {
            assert!(
                refused(replay(Client::Claude, &v(bad), Policy::Bypass)).is_some(),
                "{bad:?}"
            );
        }
    }

    // Every spelling classify accepts is one the ceiling reads; anything it cannot classify fails closed.
    #[test]
    fn the_ceiling_reads_every_spelling_classify_accepts() {
        let attached = v(&["--dangerously-skip-permissions", "-nworker"]);
        assert_eq!(
            c(
                Client::Claude,
                &["--dangerously-skip-permissions", "-nworker"]
            )
            .options,
            attached
        );
        assert_eq!(
            refused(replay(Client::Claude, &attached, Policy::Default)),
            ceiling()
        );
        assert_eq!(
            replay(Client::Claude, &attached, Policy::Bypass).ok(),
            Some(attached.clone())
        );
        for name in [
            &["-nworker"][..],
            &["-n=worker"],
            &["--name=worker"],
            &["-n", "worker"],
        ] {
            let mut flags = v(&["--dangerously-skip-permissions"]);
            flags.extend(v(name));
            assert_eq!(
                policy_of(Client::Claude, &flags),
                Some(Policy::Bypass),
                "{name:?}"
            );
        }
        let copilot = v(&["--allow-all", "-nworker"]);
        assert_eq!(policy_of(Client::Copilot, &copilot), Some(Policy::AllowAll));
        assert_eq!(
            refused(replay(Client::Copilot, &copilot, Policy::Default)),
            ceiling()
        );
        assert_eq!(policy_of(Client::Claude, &v(&["--frobnicate"])), None);
    }

    // One argv grammar: classify, the ceiling the argv gives, and replay agree for every spelling of a prompt.
    #[test]
    fn a_prompt_in_any_spelling_keeps_the_ceiling_its_options_give() {
        let cases: [(Client, &str, Policy, &[&[&str]]); 2] = [
            (
                Client::Claude,
                "--dangerously-skip-permissions",
                Policy::Bypass,
                &[
                    &["-p", "fix"],
                    &["--print", "fix"],
                    &["--prompt", "fix"],
                    &["--prompt=fix"],
                    &["fix"],
                    &["--", "fix"],
                    &["-p", "--", "-x"],
                ],
            ),
            (
                Client::Copilot,
                "--allow-all",
                Policy::AllowAll,
                &[
                    &["-p", "fix"],
                    &["-pfix"],
                    &["-p=fix"],
                    &["--prompt", "fix"],
                    &["--prompt=fix"],
                    &["-i", "fix"],
                    &["-ifix"],
                    &["--interactive=fix"],
                    &["--", "fix"],
                ],
            ),
        ];
        for (client, widen, want, prompts) in cases {
            for prompt in prompts {
                let mut argv = v(&[widen, "--model", "x"]);
                argv.extend(v(prompt));
                let got = classify(client, &argv).unwrap();
                assert_eq!(got.options, v(&[widen, "--model", "x"]), "{argv:?}");
                assert_eq!(got.prompt, v(prompt), "{argv:?}");
                assert_eq!(policy_of(client, &argv), Some(want), "{argv:?}");
                assert_eq!(crate::agents::team::policy(client, &argv), want, "{argv:?}");
                assert_eq!(
                    replay(client, &got.options, want).ok(),
                    Some(got.options.clone()),
                    "{argv:?}"
                );
            }
        }
    }

    // Every spelling the clients accept for a valued short flag (separated, attached, attached with =, =) gives one
    // policy, for both clients; an unknown short flag fails closed.
    #[test]
    fn every_spelling_of_a_short_value_reads_alike() {
        let cases = [
            (
                Client::Claude,
                "--dangerously-skip-permissions",
                "-n",
                Policy::Bypass,
            ),
            (Client::Copilot, "--allow-all", "-n", Policy::AllowAll),
            (Client::Copilot, "--allow-all", "-i", Policy::AllowAll),
        ];
        for (client, widen, short, want) in cases {
            let attached = format!("{short}worker");
            let attached_eq = format!("{short}worker=blue");
            let eq = format!("{short}=worker");
            let spellings = [
                vec![widen.to_string(), short.to_string(), "worker".to_string()],
                vec![
                    widen.to_string(),
                    short.to_string(),
                    "worker=blue".to_string(),
                ],
                vec![widen.to_string(), attached],
                vec![widen.to_string(), attached_eq],
                vec![widen.to_string(), eq],
            ];
            for flags in spellings {
                assert_eq!(
                    policy_of(client, &flags),
                    Some(want),
                    "{client:?} {flags:?}"
                );
                assert_eq!(
                    crate::agents::team::policy(client, &flags),
                    want,
                    "{client:?} {flags:?}"
                );
                assert!(classify(client, &flags).is_ok(), "{client:?} {flags:?}");
                if short == "-n" {
                    assert_eq!(
                        replay(client, &flags, want).ok(),
                        Some(flags.clone()),
                        "{flags:?}"
                    );
                    assert_eq!(
                        refused(replay(client, &flags, Policy::Default)),
                        ceiling(),
                        "{flags:?}"
                    );
                }
            }
        }
        let unknown = v(&["--dangerously-skip-permissions", "-zworker=blue"]);
        assert_eq!(policy_of(Client::Claude, &unknown), None);
        assert_eq!(
            crate::agents::team::policy(Client::Claude, &unknown),
            Policy::Default
        );
        assert_eq!(refused(classify(Client::Claude, &unknown)), usage());
    }
}
