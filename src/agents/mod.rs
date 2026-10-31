//! The agent verbs over the per-server state folder: registry, mailboxes, exit notices, board and messages.

use std::path::PathBuf;

use serde_json::Value;

use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::error::{Refusal, Res};

pub mod board;
pub mod mail;
pub mod message;
pub mod registry;
pub mod view;

/// `<root>/<serverKey>`; reads use it as is and never create it.
pub(crate) fn server_dir(ctx: &Ctx, caller: &Caller) -> PathBuf {
    ctx.state_root.join(&caller.server_key)
}

/// `<root>/<serverKey>` with its `agents` and `mail` folders, each made or checked private (MuxAgents.ps1:478-485).
pub(crate) fn state_dir(ctx: &Ctx, caller: &Caller) -> Res<PathBuf> {
    let state = server_dir(ctx, caller);
    let (agents, mail) = (state.join("agents"), state.join("mail"));
    #[cfg(unix)]
    crate::state::private_unix::private_dirs(&ctx.state_root, &[&state, &agents, &mail])?;
    #[cfg(windows)]
    crate::state::private_windows::private_dirs(&[&state, &agents, &mail])?;
    Ok(state)
}

/// Runs `f` under the per-user lock for `key`.
#[cfg(not(test))]
pub(crate) fn lock<T>(ctx: &Ctx, key: &str, f: impl FnOnce() -> Res<T>) -> Res<T> {
    crate::state::lock::with_lock(ctx, key, f)
}

/// Tests record the hold instead (state::lock has its own tests), so they never touch the real lock root under
/// the passwd home and can assert the lock order.
#[cfg(test)]
pub(crate) fn lock<T>(_: &Ctx, key: &str, f: impl FnOnce() -> Res<T>) -> Res<T> {
    crate::testing::locked(key, f)
}

/// 2 `usage` with the verb's usage line.
pub(crate) fn usage(verb: &str) -> Refusal {
    let line = crate::cli::VERBS
        .iter()
        .find(|v| v.name == verb)
        .map_or("", |v| v.usage);
    Refusal {
        code: 2,
        check: "usage",
        message: line.into(),
    }
}

pub(crate) const FILES_ONLY: &str = "agents pass free text only in files: use -BodyFile";

/// `<kind>|<serverKey>|<key>`.
pub(crate) fn lock_key(kind: &str, caller: &Caller, key: &str) -> String {
    format!("{kind}|{}|{key}", caller.server_key)
}

/// `^[0-9a-f]{12}$`: an agent id, safe as a folder or mailbox name.
pub(crate) fn agent_id(text: &str) -> bool {
    text.len() == 12 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `^%\d+$` with ASCII digits and no final LF.
pub(crate) fn pane_id(text: &str) -> bool {
    text.strip_prefix('%')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// `^[a-z][a-z0-9-]{0,23}\z`.
pub(crate) fn agent_name(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && text.len() <= 24
}

/// PowerShell's `"$value"` of a JSON value: null empty, True/False, arrays space-joined.
pub(crate) fn text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| text(Some(item)))
            .collect::<Vec<_>>()
            .join(" "),
        Some(object) => object.to_string(),
    }
}

#[cfg(test)]
pub(crate) mod fixture {
    //! A caller in pane %1 of session $3 on server 900, with a private state root in a temp folder.

    use std::path::PathBuf;

    use serde_json::{Value, json};

    use crate::caller::Caller;
    use crate::ctx::{Ctx, Env};
    use crate::parse::Client;
    use crate::testing::{self, Fakes, TempDir};

    pub const SERVER: u32 = 900;
    pub const PANE: u32 = 100;
    pub const CLAUDE: u32 = 150;
    pub const SHELL: u32 = 170;
    pub const CALLER: u32 = 4242;
    pub const ID: &str = "aaaaaaaaaaaa";
    pub const SESSION_ID: &str = "0f11fc2f-2032-48ef-a920-4af2a8e5af50";

    pub struct World {
        // Held so the temp folder lives as long as the world.
        pub _dir: TempDir,
        pub fakes: Fakes,
        pub ctx: Ctx,
    }

    fn socket() -> &'static str {
        if cfg!(windows) {
            r"\\.\pipe\rmux-me"
        } else {
            "/tmp/rmux-501/main"
        }
    }

    impl World {
        /// A human caller: caller > shell > pane, no client.
        pub fn human(tag: &str) -> World {
            World::new(false, tag)
        }

        /// A Claude agent caller: caller > shell > claude > pane.
        pub fn agent(tag: &str) -> World {
            World::new(true, tag)
        }

        fn new(agent: bool, tag: &str) -> World {
            let dir = TempDir::new();
            let fakes = Fakes::default();
            fakes.process(testing::row(SERVER, 1, 1));
            fakes.process(testing::row(PANE, SERVER, 1000));
            let below = if agent {
                fakes.process(testing::row(CLAUDE, PANE, 1500));
                fakes.client(CLAUDE, Client::Claude);
                CLAUDE
            } else {
                PANE
            };
            fakes.process(testing::row(SHELL, below, 1700));
            fakes.process(testing::row(CALLER, SHELL, 2000));
            fakes.shell(PANE);
            fakes.shell(SHELL);
            fakes.mux_rule(
                "#{pid}|#{pane_pid}|#{session_id}|#{window_id}|#{@agent-id}",
                0,
                &format!("900|100|$3|@1|{tag}\n"),
                "",
            );
            let mut ctx = fakes.ctx();
            ctx.env = Env::from_pairs([
                ("TMUX", format!("{},{SERVER},0", socket())),
                ("TMUX_PANE", "%1".to_string()),
            ]);
            ctx.state_root = dir.path().join("state");
            ctx.cwd = dir.path().join("work");
            std::fs::create_dir(&ctx.cwd).unwrap();
            World {
                _dir: dir,
                fakes,
                ctx,
            }
        }

        /// Sets the fake clock to a `YYYY-MM-DDTHH:MM:SS...Z` time at or after its own.
        pub fn clock(&self, utc: &str) {
            let at = crate::util::parse_utc(utc).unwrap();
            self.fakes
                .advance(at.duration_since(self.ctx.clock.now()).unwrap());
        }

        pub fn caller(&self) -> Caller {
            crate::caller::resolve(&self.ctx).unwrap()
        }

        /// What `list-panes` answers, one `<pane>|<window>|<pid>|<id>|<name>` per row.
        pub fn panes(&self, rows: &[&str]) {
            let mut out = rows.join("\n");
            out.push('\n');
            self.fakes.mux_first("list-panes", 0, &out, "");
        }

        pub fn state(&self) -> PathBuf {
            super::state_dir(&self.ctx, &self.caller()).unwrap()
        }

        pub fn folder(&self, id: &str) -> PathBuf {
            self.state().join("agents").join(id)
        }

        /// A committed agent of session $3 with `record`'s fields over the defaults; returns its folder.
        pub fn agent_record(&self, id: &str, fields: Value) -> PathBuf {
            let folder = self.uncommitted(id, fields);
            write(&folder.join("commit.json"), json!({"committedAt": "x"}));
            folder
        }

        /// record.json without commit.json.
        pub fn uncommitted(&self, id: &str, fields: Value) -> PathBuf {
            let folder = self.folder(id);
            std::fs::create_dir_all(&folder).unwrap();
            let mut record = json!({
                "schema": 1, "id": id, "name": "worker", "client": "claude", "sessionId": SESSION_ID,
                "context": "brief", "reason": "human", "session": "$3", "pane": "%9", "window": "@5",
                "parentPane": "%1", "parentAgent": null, "directory": "/work", "taskLine": "build it",
                "createdAt": "1970-01-01T00:00:00.0000000Z"
            });
            for (key, value) in fields.as_object().unwrap() {
                record[key] = value.clone();
            }
            write(&folder.join("record.json"), record);
            folder
        }
    }

    impl World {
        /// A file in the caller's cwd.
        pub fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.ctx.cwd.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        }

        /// A parsed call, as the dispatcher hands it to a handler.
        pub fn request(&self, args: &[&str]) -> crate::cli::Request {
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            crate::cli::parse(&args).unwrap().1
        }
    }

    pub fn write(path: &std::path::Path, value: Value) {
        crate::state::fs::write_json(path, &value).unwrap();
    }
}
