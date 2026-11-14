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

/// A flag's name (before `=`) and its value: the `=` part, else the next argument.
fn named<'a>(flags: &'a [String], name: &str) -> Vec<&'a str> {
    let mut found = Vec::new();
    let mut i = 0;
    while i < flags.len() {
        let (head, inline) = match flags[i].split_once('=') {
            Some((h, v)) => (h, Some(v)),
            None => (flags[i].as_str(), None),
        };
        if head == name {
            match inline {
                Some(v) => found.push(v),
                None => {
                    found.push(flags.get(i + 1).map_or("", String::as_str));
                    i += 1;
                }
            }
        }
        i += 1;
    }
    found
}

fn has(flags: &[String], names: &[&str]) -> bool {
    flags
        .iter()
        .any(|f| names.contains(&f.split('=').next().unwrap_or(f)))
}

/// The policy `flags` give `client`, deny-first and strictest-wins: anything that narrows the client (a deny rule, a
/// non-bypass mode) makes it `Default`, even beside a flag that would widen it.
pub fn policy(client: Client, flags: &[String]) -> Policy {
    match client {
        Client::Claude => {
            let modes = named(flags, "--permission-mode");
            if modes.iter().any(|m| *m != "bypassPermissions") {
                return Policy::Default;
            }
            if has(flags, &["--dangerously-skip-permissions"]) || !modes.is_empty() {
                Policy::Bypass
            } else {
                Policy::Default
            }
        }
        Client::Copilot => {
            if has(flags, &["--deny-tool", "--deny-url", "--deny-path"]) {
                return Policy::Default;
            }
            if has(flags, &["--allow-all", "--yolo"]) {
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
    }

    #[test]
    fn copilot_policy_is_deny_first() {
        let p = |f: &[&str]| policy(Client::Copilot, &v(f));
        assert_eq!(p(&[]), Policy::Default);
        assert_eq!(p(&["--allow-all"]), Policy::AllowAll);
        assert_eq!(p(&["--yolo"]), Policy::AllowAll);
        assert_eq!(p(&["--allow-all-tools"]), Policy::Default);
        for deny in [
            "--deny-tool",
            "--deny-tool=shell(rm)",
            "--deny-url",
            "--deny-path",
        ] {
            assert_eq!(p(&["--allow-all", deny]), Policy::Default, "{deny}");
        }
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
