//! The agent verbs over the per-server state folder: registry, mailboxes, exit notices, board and messages.

use std::path::PathBuf;

use serde_json::Value;

use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::error::{Refusal, Res};

pub mod board;
pub mod copilot;
pub mod mail;
pub mod message;
pub mod placement;
pub mod registry;
pub mod spawn;
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

/// Runs `f` under the per-user lock for `key` when one attempt takes it, never waiting; `None` when it is busy.
#[cfg(not(test))]
pub(crate) fn try_lock<T>(ctx: &Ctx, key: &str, f: impl FnOnce() -> Res<T>) -> Res<Option<T>> {
    crate::state::lock::try_with_lock(ctx, key, f)
}

/// Tests record the hold as `lock` does; a key held elsewhere (a shared gate or `lock_timeout`) is busy at once.
#[cfg(test)]
pub(crate) fn try_lock<T>(_: &Ctx, key: &str, f: impl FnOnce() -> Res<T>) -> Res<Option<T>> {
    crate::testing::try_locked(key, f)
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
        reason: None,
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

        /// What `list-panes` answers, one `<pane>|<window>|<pid>|<id>|<name>` per row, expanded into the call's
        /// own `-F` format as the multiplexer would.
        pub fn panes(&self, rows: &[&str]) {
            self.panes_dead(rows, &[]);
        }

        /// As `panes`, with the panes in `dead` answering `#{pane_dead}` 1 (remain-on-exit kept them).
        pub fn panes_dead(&self, rows: &[&str], dead: &[&str]) {
            let rows: Vec<String> = rows.iter().map(|r| r.to_string()).collect();
            let dead: Vec<String> = dead.iter().map(|d| d.to_string()).collect();
            self.fakes.mux_script("list-panes", move |argv| {
                let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
                let dead: Vec<&str> = dead.iter().map(String::as_str).collect();
                Ok(crate::mux::Output {
                    code: 0,
                    stdout: listed_dead(argv, &rows, "\n", &dead),
                    stderr: String::new(),
                })
            });
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
            // Its pane's launch, unless the fields name one.
            if record.get("startCommand").is_none() {
                let pane = record["pane"].as_str().unwrap_or("").to_string();
                record["startCommand"] = launch_command(&pane).into();
            }
            write(&folder.join("record.json"), record);
            folder
        }

        /// Makes agent `id`'s record a 0.2.0-0.2.2 one: no `startCommand`.
        pub fn legacy(&self, id: &str) {
            let path = self.folder(id).join("record.json");
            let mut record = crate::state::fs::read_json(&path).unwrap().unwrap();
            record.as_object_mut().unwrap().remove("startCommand");
            write(&path, record);
        }

        /// spawn's probe after the tags answers every pane as still the launch its reply named.
        pub fn launches_hold(&self) {
            self.fakes
                .mux_script("}|#{==:#{pane_start_command},", |argv| {
                    let pane = argv
                        .iter()
                        .skip_while(|a| *a != "-t")
                        .nth(1)
                        .cloned()
                        .unwrap_or_default();
                    Ok(crate::mux::Output {
                        code: 0,
                        stdout: format!("{pane}|1\n"),
                        stderr: String::new(),
                    })
                });
        }

        /// The recipient probe answers every pane as a live pane of session $3.
        pub fn recipients_live(&self) {
            self.fakes.mux_script("#{pane_id}|#{session_id}", |argv| {
                let pane = argv
                    .iter()
                    .skip_while(|a| *a != "-t")
                    .nth(1)
                    .cloned()
                    .unwrap_or_default();
                Ok(crate::mux::Output {
                    code: 0,
                    stdout: format!("{pane}|$3\n"),
                    stderr: String::new(),
                })
            });
        }

        /// What `list-panes` answers, row by row.
        pub fn pane_rows(&self, rows: &[Listed]) {
            let rows = rows.to_vec();
            self.fakes.mux_script("list-panes", move |argv| {
                Ok(crate::mux::Output {
                    code: 0,
                    stdout: listed_rows(argv, &rows, "\n"),
                    stderr: String::new(),
                })
            });
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

    /// The fixture's launch command of an agent whose pane is `pane`: what its record's `startCommand` holds and
    /// what the pane list answers for that pane, unless a test says otherwise.
    pub fn launch_command(pane: &str) -> String {
        format!(
            "/bin/mux-agent launcher /state/agents/{}/spec.json",
            pane.trim_start_matches('%')
        )
    }

    /// One pane as the fake `list-panes` answers it.
    #[derive(Clone, Debug)]
    pub struct Listed {
        pub pane: String,
        pub window: String,
        pub pid: String,
        pub dead: bool,
        pub command: String,
        pub tag: String,
        pub name: String,
    }

    /// `<pane>|<window>|<pid>|<id>|<name>` (the name to the end, free to hold `|` or LF), started by its
    /// `launch_command`, dead when listed in `dead`.
    pub fn row_of(row: &str, dead: &[&str]) -> Listed {
        let mut fields = row.splitn(5, '|');
        let mut next = || fields.next().unwrap_or("").to_string();
        let (pane, window, pid, tag, name) = (next(), next(), next(), next(), next());
        Listed {
            dead: dead.contains(&pane.as_str()),
            command: launch_command(&pane),
            pane,
            window,
            pid,
            tag,
            name,
        }
    }

    /// `rows` expanded into the `-F` format of `argv`, each row ended by `sep`.
    pub fn listed(argv: &[String], rows: &[&str], sep: &str) -> String {
        listed_dead(argv, rows, sep, &[])
    }

    /// As `listed`, with `#{pane_dead}` 1 for the panes in `dead` and 0 for the others.
    pub fn listed_dead(argv: &[String], rows: &[&str], sep: &str, dead: &[&str]) -> String {
        let rows: Vec<Listed> = rows.iter().map(|r| row_of(r, dead)).collect();
        listed_rows(argv, &rows, sep)
    }

    /// `rows` expanded into the `-F` format of `argv` as the multiplexer would: each field's text as is, in one
    /// pass, so no field's text is read as a placeholder.
    pub fn listed_rows(argv: &[String], rows: &[Listed], sep: &str) -> String {
        let format = argv
            .iter()
            .skip_while(|a| *a != "-F")
            .nth(1)
            .cloned()
            .unwrap_or_default();
        rows.iter()
            .map(|row| {
                let fields = [
                    ("#{pane_id}", row.pane.as_str()),
                    ("#{window_id}", &row.window),
                    ("#{pane_pid}", &row.pid),
                    ("#{pane_dead}", if row.dead { "1" } else { "0" }),
                    ("#{pane_start_command}", &row.command),
                    ("#{@agent-id}", &row.tag),
                    ("#{@agent-name}", &row.name),
                ];
                let mut line = String::new();
                let mut rest = format.as_str();
                'scan: while let Some(c) = rest.chars().next() {
                    for (key, value) in fields {
                        if let Some(after) = rest.strip_prefix(key) {
                            line.push_str(value);
                            rest = after;
                            continue 'scan;
                        }
                    }
                    line.push(c);
                    rest = &rest[c.len_utf8()..];
                }
                format!("{line}{sep}")
            })
            .collect()
    }

    /// Whether `call` is the session's pane list: every field framed by one fresh 32-hex marker.
    pub fn list_call(call: &[String]) -> bool {
        let prefix = ["-N", "list-panes", "-s", "-t", "$3", "-F"];
        let marker = call.get(6).map_or("", |f| f.get(..32).unwrap_or(""));
        call.len() == 7
            && call[..6] == prefix[..]
            && marker.len() == 32
            && marker.bytes().all(|b| b.is_ascii_hexdigit())
            && call[6] == super::registry::list_format(marker)
    }

    pub fn write(path: &std::path::Path, value: Value) {
        crate::state::fs::write_json(path, &value).unwrap();
    }
}
