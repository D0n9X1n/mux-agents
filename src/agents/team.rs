//! A team's record and its permission ceiling (#103 phase 2a-ii). Only a person's `up` creates a team; it records the
//! person's own client flags and a policy word derived from those flags alone, never from settings or the
//! environment, so the ceiling is never looser than what the person launched with. The record is written once and
//! never replaced: a later `up` that names other flags keeps it. Every later spawn and resume is checked against it
//! (phase 3).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::error::{Refusal, Res};
use crate::parse::Client;

/// How much a client may do without asking, least first, so a child is within a ceiling when it is not greater.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Policy {
    /// The client asks before acting (any mode short of full bypass, a deny rule, or no permission flag at all).
    Default,
    /// Copilot's `--allow-all` / `--yolo`, with no deny rule.
    AllowAll,
    /// Claude's `--dangerously-skip-permissions` or `--permission-mode bypassPermissions`, with no other mode named.
    Bypass,
}

impl Policy {
    pub fn word(self) -> &'static str {
        match self {
            Policy::Default => "default",
            Policy::AllowAll => "allow-all",
            Policy::Bypass => "bypass",
        }
    }

    pub fn from_word(word: &str) -> Option<Policy> {
        match word {
            "default" => Some(Policy::Default),
            "allow-all" => Some(Policy::AllowAll),
            "bypass" => Some(Policy::Bypass),
            _ => None,
        }
    }
}

// The flags each client takes a value for, as `--x v` or `--x=v`; any other dash token takes none.
const CLAUDE_VALUED: [&str; 13] = [
    "--model",
    "--add-dir",
    "--session-id",
    "-r",
    "--resume",
    "-n",
    "--name",
    "--permission-mode",
    "--effort",
    "--allowedTools",
    "--allowed-tools",
    "--disallowedTools",
    "--disallowed-tools",
];
const CLAUDE_BARE: [&str; 5] = [
    "--continue",
    "-c",
    "--fork-session",
    "--dangerously-skip-permissions",
    "--verbose",
];
const COPILOT_VALUED: [&str; 16] = [
    "--model",
    "--add-dir",
    "--session-id",
    "-r",
    "--resume",
    "-n",
    "--name",
    "--reasoning-effort",
    "-i",
    "--interactive",
    "-p",
    "--prompt",
    "--mode",
    "--deny-tool",
    "--deny-url",
    "--excluded-tools",
];
const COPILOT_BARE: [&str; 10] = [
    "--continue",
    "--allow-all",
    "--yolo",
    "--no-sandbox",
    "--allow-all-tools",
    "--allow-all-paths",
    "--allow-all-urls",
    "--autopilot",
    "--plan",
    "--assisted-approval",
];

/// The client's flags as (name, value) pairs, honouring argument boundaries: a valued flag consumes its value (so a
/// value that looks like a flag is never read as one), and nothing after `--` is a flag. None when a dash token is
/// not one the client is known to take: an unknown flag could change permissions in ways this does not model.
fn parsed(flags: &[String], valued: &[&str], bare: &[&str]) -> Option<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < flags.len() {
        let token = flags[i].as_str();
        if token == "--" {
            break;
        }
        if !token.starts_with('-') || token == "-" {
            // A positional (the task text): no permission meaning.
            i += 1;
            continue;
        }
        let (name, inline) = match token.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (token, None),
        };
        if valued.contains(&name) {
            let value = match inline {
                Some(v) => v.to_string(),
                None => {
                    i += 1;
                    flags.get(i)?.clone()
                }
            };
            out.push((name.to_string(), value));
        } else if bare.contains(&name) && inline.is_none() {
            out.push((name.to_string(), String::new()));
        } else {
            return None;
        }
        i += 1;
    }
    Some(out)
}

/// The policy `flags` give `client`, strictest-wins and fail-closed: anything that narrows the client, and any flag
/// this does not know, makes it `Default`, even beside a flag that would widen it.
pub fn policy(client: Client, flags: &[String]) -> Policy {
    let (valued, bare): (&[&str], &[&str]) = match client {
        Client::Claude => (&CLAUDE_VALUED, &CLAUDE_BARE),
        Client::Copilot => (&COPILOT_VALUED, &COPILOT_BARE),
    };
    let Some(pairs) = parsed(flags, valued, bare) else {
        return Policy::Default;
    };
    let named = |n: &str| pairs.iter().any(|(k, _)| k == n);
    match client {
        Client::Claude => {
            // Tool restrictions narrow it; any mode but bypassPermissions narrows it (dontAsk included: Claude Code
            // starts teammates of a dontAsk lead in Manual).
            let narrowed = pairs.iter().any(|(k, v)| {
                matches!(
                    k.as_str(),
                    "--allowedTools"
                        | "--allowed-tools"
                        | "--disallowedTools"
                        | "--disallowed-tools"
                ) || (k == "--permission-mode" && v != "bypassPermissions")
            });
            if narrowed {
                return Policy::Default;
            }
            if named("--dangerously-skip-permissions") || named("--permission-mode") {
                Policy::Bypass
            } else {
                Policy::Default
            }
        }
        Client::Copilot => {
            // Deny rules beat allow-all; plan mode and assisted approval narrow it.
            let narrowed = pairs.iter().any(|(k, v)| {
                matches!(
                    k.as_str(),
                    "--deny-tool"
                        | "--deny-url"
                        | "--excluded-tools"
                        | "--plan"
                        | "--assisted-approval"
                ) || (k == "--mode" && v != "interactive" && v != "autopilot")
            });
            if narrowed {
                return Policy::Default;
            }
            // `--allow-all` is documented as the three `--allow-all-*` flags together.
            let all = named("--allow-all")
                || named("--yolo")
                || (named("--allow-all-tools")
                    && named("--allow-all-paths")
                    && named("--allow-all-urls"));
            if all {
                Policy::AllowAll
            } else {
                Policy::Default
            }
        }
    }
}

/// Whether a child launched with `child` stays within `ceiling`.
pub fn within(ceiling: Policy, child: Policy) -> bool {
    child <= ceiling
}

/// One team's record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Team {
    pub team: String,
    pub session: String,
    pub directory: String,
    pub client: Client,
    pub flags: Vec<String>,
    pub policy: Policy,
    pub leader: String,
    pub created_at: String,
}

const SCHEMA: u64 = 1;

/// `<server folder>/teams/<sha256(session)[..16]>.json`: one record per team session of that server.
pub fn path(server_dir: &Path, session: &str) -> PathBuf {
    server_dir
        .join("teams")
        .join(format!("{}.json", &crate::util::sha256_hex(session)[..16]))
}

impl Team {
    fn to_json(&self) -> Value {
        json!({"schema": SCHEMA, "team": self.team, "session": self.session, "directory": self.directory,
            "client": crate::agents::spawn::client_name(self.client), "flags": self.flags,
            "policy": self.policy.word(), "leader": self.leader, "createdAt": self.created_at})
    }

    /// A record read back whole, or none for anything malformed, of another schema, or whose policy word is not the
    /// one its own flags give: a record is never trusted for more than its flags allow.
    fn from_json(v: &Value) -> Option<Team> {
        let text = |k: &str| v.get(k).and_then(Value::as_str).map(String::from);
        if v.get("schema").and_then(Value::as_u64) != Some(SCHEMA) {
            return None;
        }
        let client = crate::agents::spawn::client_named(&text("client")?)?;
        let flags: Vec<String> = v
            .get("flags")?
            .as_array()?
            .iter()
            .map(|f| f.as_str().map(String::from))
            .collect::<Option<_>>()?;
        let recorded = Policy::from_word(&text("policy")?)?;
        if recorded != policy(client, &flags) {
            return None;
        }
        let leader = text("leader")?;
        if !crate::agents::agent_id(&leader) {
            return None;
        }
        Some(Team {
            team: text("team")?,
            session: text("session")?,
            directory: text("directory")?,
            client,
            flags,
            policy: recorded,
            leader,
            created_at: text("createdAt")?,
        })
    }
}

/// Writes `team` at `file` only if no record is there: true when this call made it, false when one already was.
pub fn create(file: &Path, team: &Team) -> Res<bool> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::state::fs::claim_json(file, &team.to_json())
}

/// The record at `file`: none when there is none, a refusal when one is there but cannot be trusted.
pub fn read(file: &Path) -> Res<Option<Team>> {
    match crate::state::fs::read_json(file)? {
        None => Ok(None),
        Some(v) => Team::from_json(&v)
            .map(Some)
            .ok_or_else(|| Refusal::internal("the team record is malformed")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    fn v(flags: &[&str]) -> Vec<String> {
        flags.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn claude_policy_is_strictest_of_its_flags() {
        let p = |f: &[&str]| policy(Client::Claude, &v(f));
        assert_eq!(p(&[]), Policy::Default);
        assert_eq!(p(&["--dangerously-skip-permissions"]), Policy::Bypass);
        assert_eq!(
            p(&["--permission-mode", "bypassPermissions"]),
            Policy::Bypass
        );
        assert_eq!(p(&["--permission-mode=bypassPermissions"]), Policy::Bypass);
        // Any other mode narrows it, even beside the skip flag; dontAsk starts teammates in Manual.
        for mode in ["default", "plan", "acceptEdits", "dontAsk", ""] {
            assert_eq!(
                p(&["--dangerously-skip-permissions", "--permission-mode", mode]),
                Policy::Default,
                "{mode}"
            );
        }
        assert_eq!(p(&["--permission-mode"]), Policy::Default);
        assert_eq!(p(&["--model", "opus", "--effort", "high"]), Policy::Default);
        // Review of 214261e: a flag's value, or anything after `--`, is never read as a flag.
        assert_eq!(
            p(&["--name", "--dangerously-skip-permissions"]),
            Policy::Default
        );
        assert_eq!(
            p(&["--", "--dangerously-skip-permissions"]),
            Policy::Default
        );
        assert_eq!(
            p(&["--model", "--permission-mode", "bypassPermissions"]),
            Policy::Default
        );
        // Tool restrictions and unknown flags narrow it.
        assert_eq!(
            p(&[
                "--dangerously-skip-permissions",
                "--disallowed-tools",
                "Bash"
            ]),
            Policy::Default
        );
        assert_eq!(
            p(&["--dangerously-skip-permissions", "--something-new"]),
            Policy::Default
        );
        assert_eq!(
            p(&[
                "--dangerously-skip-permissions",
                "--effort",
                "high",
                "task text"
            ]),
            Policy::Bypass
        );
    }

    #[test]
    fn copilot_policy_is_deny_first() {
        let p = |f: &[&str]| policy(Client::Copilot, &v(f));
        assert_eq!(p(&[]), Policy::Default);
        assert_eq!(p(&["--allow-all"]), Policy::AllowAll);
        assert_eq!(p(&["--yolo"]), Policy::AllowAll);
        assert_eq!(p(&["--allow-all-tools"]), Policy::Default);
        for deny in [
            &["--deny-tool", "shell(rm)"][..],
            &["--deny-tool=shell(rm)"],
            &["--deny-url", "x"],
        ] {
            let mut f = vec!["--allow-all"];
            f.extend(deny);
            assert_eq!(p(&f), Policy::Default, "{deny:?}");
        }
        // Review of 214261e: the three --allow-all-* together are --allow-all; plan mode and assisted approval narrow.
        assert_eq!(
            p(&["--allow-all-tools", "--allow-all-paths", "--allow-all-urls"]),
            Policy::AllowAll
        );
        assert_eq!(
            p(&["--allow-all-tools", "--allow-all-paths"]),
            Policy::Default
        );
        assert_eq!(p(&["--yolo", "--plan"]), Policy::Default);
        assert_eq!(p(&["--yolo", "--assisted-approval"]), Policy::Default);
        assert_eq!(p(&["--yolo", "--mode", "plan"]), Policy::Default);
        assert_eq!(p(&["--yolo", "--mode=autopilot"]), Policy::AllowAll);
        assert_eq!(p(&["--name", "--allow-all"]), Policy::Default);
        assert_eq!(p(&["--", "--yolo"]), Policy::Default);
        assert_eq!(p(&["--yolo", "--unknown"]), Policy::Default);
    }

    #[test]
    fn a_child_is_within_a_ceiling_it_does_not_exceed() {
        assert!(within(Policy::Bypass, Policy::Default));
        assert!(within(Policy::Bypass, Policy::Bypass));
        assert!(within(Policy::Default, Policy::Default));
        assert!(!within(Policy::Default, Policy::AllowAll));
        assert!(!within(Policy::AllowAll, Policy::Bypass));
    }

    fn team(flags: &[&str]) -> Team {
        Team {
            team: "0123456789abcdef:$3".into(),
            session: "$3".into(),
            directory: "/work".into(),
            client: Client::Claude,
            flags: v(flags),
            policy: policy(Client::Claude, &v(flags)),
            leader: "aaaaaaaaaaaa".into(),
            created_at: "2026-11-14T00:00:00.0000000Z".into(),
        }
    }

    #[test]
    fn a_record_is_written_once_and_read_back_whole() {
        let dir = TempDir::new();
        let file = path(dir.path(), "$3");
        assert!(file.ends_with(format!(
            "teams/{}.json",
            &crate::util::sha256_hex("$3")[..16]
        )));
        assert_eq!(read(&file).unwrap(), None);
        let first = team(&["--dangerously-skip-permissions", "x y"]);
        assert!(create(&file, &first).unwrap());
        // A later `up` with other flags keeps the first record.
        assert!(!create(&file, &team(&["--permission-mode", "plan"])).unwrap());
        assert_eq!(read(&file).unwrap(), Some(first));
    }

    #[test]
    fn a_record_whose_policy_word_its_flags_do_not_give_is_refused() {
        let dir = TempDir::new();
        let file = path(dir.path(), "$3");
        let mut t = team(&["--permission-mode", "plan"]);
        t.policy = Policy::Bypass;
        assert!(create(&file, &t).unwrap());
        assert!(read(&file).is_err());
        for bad in [
            json!({"schema": 2}),
            json!({"schema": 1, "client": "claude", "flags": "x"}),
        ] {
            let other = dir.path().join("bad.json");
            crate::state::fs::write_json(&other, &bad).unwrap();
            assert!(read(&other).is_err(), "{bad}");
        }
    }
}
