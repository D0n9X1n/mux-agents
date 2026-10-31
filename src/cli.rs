//! The verb table, the argument parser and the dispatcher (MuxAgents.ps1:14-103, 1673-1681).

use std::borrow::Borrow;
use std::panic::{self, AssertUnwindSafe};

use serde_json::{Map, Value};

use crate::ctx::Ctx;
use crate::error::{Refusal, Res, refuse};
use crate::json;

/// One parsed call. Flag names are the table's spelling, whatever case the caller typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub verb: &'static str,
    pub values: Vec<(&'static str, String)>,
    pub switches: Vec<&'static str>,
    pub positional: Vec<String>,
    pub text: bool,
}

impl Request {
    pub fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn switch(&self, name: &str) -> bool {
        self.switches.contains(&name)
    }
}

/// A verb's reply fields after `ok` and `verb`, computed against the call's providers.
pub type Handler = fn(&Ctx, &Request) -> Res<Map<String, Value>>;

pub struct VerbDef {
    pub name: &'static str,
    pub values: &'static [&'static str],
    pub switches: &'static [&'static str],
    pub min: usize,
    pub max: usize,
    pub usage: &'static str,
    pub handler: Handler,
}

pub static VERBS: &[VerbDef] = &[
    VerbDef {
        name: "spawn",
        values: &["Client", "Task", "TaskFile", "BriefFile", "Name", "Split"],
        switches: &["NoFork"],
        min: 0,
        max: 0,
        usage: "usage: mux-agent spawn -Client <claude|copilot> [-Task <text>] [-TaskFile <path>] [-BriefFile <path>] [-Name <name>] [-Split <value>] [-NoFork] [-Text]",
        handler: crate::agents::spawn::spawn,
    },
    VerbDef {
        name: "list",
        values: &[],
        switches: &["All"],
        min: 0,
        max: 0,
        usage: "usage: mux-agent list [-All] [-Text]",
        handler: crate::agents::view::list,
    },
    VerbDef {
        name: "focus",
        values: &[],
        switches: &[],
        min: 1,
        max: 1,
        usage: "usage: mux-agent focus <name> [-Text]",
        handler: crate::agents::view::focus,
    },
    VerbDef {
        name: "send",
        values: &["Body", "BodyFile"],
        switches: &[],
        min: 1,
        max: 1,
        usage: "usage: mux-agent send <name|parent|%pane> (-Body <text> | -BodyFile <path>) [-Text]",
        handler: crate::agents::message::send,
    },
    VerbDef {
        name: "read",
        values: &["Wait", "Lines"],
        switches: &[],
        min: 0,
        max: 1,
        usage: "usage: mux-agent read [-Wait <seconds>] | read <name> [-Lines <n>] [-Text]",
        handler: crate::agents::message::read,
    },
    VerbDef {
        name: "stop",
        values: &[],
        switches: &[],
        min: 1,
        max: 1,
        usage: "usage: mux-agent stop <name> [-Text]",
        handler: crate::agents::message::stop,
    },
    VerbDef {
        name: "status",
        values: &["Body", "BodyFile"],
        switches: &[],
        min: 0,
        max: 0,
        usage: "usage: mux-agent status (-Body <text> | -BodyFile <path>) [-Text]",
        handler: crate::agents::view::status,
    },
    VerbDef {
        name: "sidebar",
        values: &[],
        switches: &["Close"],
        min: 0,
        max: 0,
        usage: "usage: mux-agent sidebar [-Close] [-Text]",
        handler: crate::sidebar::sidebar,
    },
    VerbDef {
        name: "watch",
        values: &[],
        switches: &[],
        min: 0,
        max: 0,
        usage: "usage: mux-agent watch [-Text]",
        handler: crate::sidebar::watch,
    },
];

fn flag<'a>(names: &[&'a str], token: &str) -> Option<&'a str> {
    let name = token.strip_prefix('-')?;
    names.iter().copied().find(|n| n.eq_ignore_ascii_case(name))
}

/// `<verb>`, then `-Value <next token, whatever it looks like>`, `-Switch` and positionals; `-Text` is a
/// switch of every verb.
pub fn parse(args: &[String]) -> Res<(&'static VerbDef, Request)> {
    let Some(def) = args
        .first()
        .and_then(|v| VERBS.iter().find(|d| d.name.eq_ignore_ascii_case(v)))
    else {
        let mut names: Vec<_> = VERBS.iter().map(|d| d.name).collect();
        names.sort_unstable();
        return refuse(
            2,
            "usage",
            format!("usage: mux-agent <{}> ...", names.join("|")),
        );
    };
    let mut request = Request {
        verb: def.name,
        values: Vec::new(),
        switches: Vec::new(),
        positional: Vec::new(),
        text: false,
    };
    let mut tokens = args[1..].iter();
    while let Some(token) = tokens.next() {
        if let Some(name) = flag(def.values, token) {
            match tokens.next() {
                Some(value) if request.value(name).is_none() => {
                    request.values.push((name, value.clone()))
                }
                _ => return refuse(2, "usage", def.usage),
            }
        } else if let Some(name) = flag(def.switches, token) {
            if !request.switch(name) {
                request.switches.push(name);
            }
        } else if flag(&["Text"], token).is_some() {
            request.text = true;
        } else if token.starts_with('-') {
            return refuse(2, "usage", def.usage);
        } else {
            request.positional.push(token.clone());
        }
    }
    if request.positional.len() < def.min || request.positional.len() > def.max {
        return refuse(2, "usage", def.usage);
    }
    Ok((def, request))
}

fn failure(verb: Option<&str>, refusal: &Refusal) -> Map<String, Value> {
    let mut reply = Map::new();
    reply.insert("ok".into(), false.into());
    reply.insert("verb".into(), verb.map_or(Value::Null, Value::from));
    reply.insert("code".into(), refusal.code.into());
    reply.insert("check".into(), refusal.check.into());
    reply.insert("message".into(), refusal.message.clone().into());
    reply
}

fn dispatch(def: &VerbDef, request: &Request, ctx: &Ctx) -> Res<Map<String, Value>> {
    #[cfg(feature = "test-panic")]
    if ctx.env.get("MUX_AGENTS_TEST_PANIC") == Some("dispatch") {
        panic!("PANIC-PAYLOAD-7f3a");
    }
    // Every public verb refuses a sandboxed caller before any multiplexer call (MuxAgents.ps1:94).
    crate::sandbox::assert_unsandboxed(ctx)?;
    let data = (def.handler)(ctx, request)?;
    let mut reply = Map::new();
    reply.insert("ok".into(), true.into());
    reply.insert("verb".into(), def.name.into());
    reply.extend(data);
    Ok(reply)
}

/// Parse, build the providers, run the verb and map the outcome to exit code 0, 1 or 2 and the reply text, never
/// echoing an error's own text. The providers are built only for a call that parsed; one that cannot be built
/// replies as JSON with no verb, as the reference's provider failure did. A panic in the verb is 1 `internal`;
/// unwinding has already released its locks.
pub fn run<C: Borrow<Ctx>>(args: &[String], ctx: impl FnOnce() -> Res<C>) -> (u8, String) {
    let (verb, text, outcome) = match parse(args) {
        Err(refusal) => (None, false, Err(refusal)),
        Ok((def, request)) => match ctx() {
            Err(refusal) => (None, false, Err(refusal)),
            Ok(ctx) => {
                let outcome =
                    panic::catch_unwind(AssertUnwindSafe(|| dispatch(def, &request, ctx.borrow())))
                        .unwrap_or_else(|_| refuse(1, "internal", "unexpected error (panic)"));
                (Some(def.name), request.text, outcome)
            }
        },
    };
    let (code, reply) = match outcome {
        Ok(reply) => (0, reply),
        Err(refusal) => (refusal.code, failure(verb, &refusal)),
    };
    let out = if text {
        json::to_text(&reply)
    } else {
        json::to_stdout(&Value::Object(reply))
    };
    (code, out)
}

/// The spec of the hidden `launcher <spec>` call a pane runs (spec D4); not a verb, so absent from the usage text.
pub fn launcher_spec(args: &[String]) -> Option<&str> {
    match args {
        [verb, spec] if verb == "launcher" => Some(spec),
        _ => None,
    }
}

/// The pane's process: the launcher's exit code, or the providers' refusal as JSON with its code. No sandbox check:
/// the pane was spawned by a call that passed it, and the launcher runs no verb.
pub fn run_launcher(spec: &str, ctx: impl FnOnce() -> Res<Ctx>) -> (i32, Option<String>) {
    match ctx() {
        Err(refusal) => (
            refusal.code.into(),
            Some(json::to_stdout(&Value::Object(failure(None, &refusal)))),
        ),
        Ok(ctx) => {
            // A panic still ends the pane with the launcher's own failure code.
            let code =
                panic::catch_unwind(AssertUnwindSafe(|| crate::launcher::launcher(&ctx, spec)))
                    .unwrap_or(1);
            (code, None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Fakes;

    fn args(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|t| t.to_string()).collect()
    }

    fn parsed(tokens: &[&str]) -> Request {
        parse(&args(tokens)).unwrap().1
    }

    fn usage_of(tokens: &[&str]) -> (u8, &'static str) {
        match parse(&args(tokens)) {
            Ok((_, request)) => panic!("parsed: {request:?}"),
            Err(refusal) => (refusal.code, refusal.check),
        }
    }

    fn probe(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
        let mut data = Map::new();
        data.insert("alive".into(), ctx.os.row(42).is_some().into());
        data.insert("pid".into(), ctx.pid.into());
        data.insert("args".into(), request.positional.len().into());
        Ok(data)
    }

    #[test]
    fn handler_receives_ctx() {
        let fakes = Fakes::default();
        fakes.process(crate::testing::row(42, 1, 4200));
        let ctx = fakes.ctx();
        let def = VerbDef {
            name: "probe",
            values: &[],
            switches: &[],
            min: 0,
            max: 1,
            usage: "usage: probe",
            handler: probe,
        };
        let request = Request {
            verb: "probe",
            values: Vec::new(),
            switches: Vec::new(),
            positional: vec!["x".into()],
            text: false,
        };
        let reply = dispatch(&def, &request, &ctx).unwrap();
        assert_eq!(
            Value::Object(reply).to_string(),
            r#"{"ok":true,"verb":"probe","alive":true,"pid":4242,"args":1}"#
        );
        // The sandbox check runs before the handler.
        assert_eq!(fakes.os_calls(), ["sandbox", "row 42"]);
    }

    #[test]
    fn matched_switches_take_canonical_names() {
        let list = parsed(&["LIST", "-all", "-TEXT"]);
        assert_eq!(
            (list.verb, list.switches.as_slice(), list.text),
            ("list", &["All"][..], true)
        );
        let spawn = parsed(&["spawn", "-client", "claude", "-NOFORK"]);
        assert_eq!(spawn.verb, "spawn");
        assert_eq!(spawn.values, [("Client", "claude".to_string())]);
        assert_eq!(spawn.switches, ["NoFork"]);
        assert!(spawn.switch("NoFork") && !spawn.text);
        let sidebar = parsed(&["Sidebar", "-close"]);
        assert_eq!(
            (sidebar.verb, sidebar.switches.as_slice()),
            ("sidebar", &["Close"][..])
        );
    }

    #[test]
    fn value_flag_takes_next_token_whatever_it_looks_like() {
        let spawn = parsed(&["spawn", "-Client", "claude", "-Task", "-Text"]);
        assert_eq!(
            spawn.values,
            [
                ("Client", "claude".to_string()),
                ("Task", "-Text".to_string())
            ]
        );
        assert_eq!(spawn.value("Task"), Some("-Text"));
        assert!(!spawn.text);
        assert!(spawn.positional.is_empty());
    }

    // A valid argv for each verb, positionals at the verb's minimum.
    fn valid(verb: &str) -> Vec<&'static str> {
        match verb {
            "focus" | "send" | "stop" => vec!["a"],
            _ => vec![],
        }
    }

    #[test]
    fn text_is_accepted_by_every_verb() {
        let names: Vec<_> = VERBS.iter().map(|d| d.name).collect();
        assert_eq!(
            names,
            [
                "spawn", "list", "focus", "send", "read", "stop", "status", "sidebar", "watch"
            ]
        );
        for name in names {
            let mut tokens = vec![name];
            tokens.extend(valid(name));
            assert!(!parsed(&tokens).text, "{name}");
            tokens.push("-Text");
            let request = parsed(&tokens);
            assert_eq!((request.verb, request.text), (name, true), "{name}");
        }
    }

    #[test]
    fn positional_bounds_of_every_verb() {
        // MuxAgents.ps1:1673-1681.
        let bounds = [
            ("spawn", 0, 0),
            ("list", 0, 0),
            ("focus", 1, 1),
            ("send", 1, 1),
            ("read", 0, 1),
            ("stop", 1, 1),
            ("status", 0, 0),
            ("sidebar", 0, 0),
            ("watch", 0, 0),
        ];
        assert_eq!(bounds.len(), VERBS.len());
        let pos = ["p1", "p2", "p3"];
        for (name, min, max) in bounds {
            let def = VERBS.iter().find(|d| d.name == name).unwrap();
            assert_eq!((def.min, def.max), (min, max), "{name}");
            let call = |n: usize| {
                let mut tokens = vec![name];
                tokens.extend(&pos[..n]);
                tokens
            };
            if min > 0 {
                assert_eq!(usage_of(&call(min - 1)), (2, "usage"), "{name} below");
            }
            assert_eq!(usage_of(&call(max + 1)), (2, "usage"), "{name} above");
            for n in min..=max {
                assert_eq!(parsed(&call(n)).positional, &pos[..n], "{name} {n}");
            }
        }
    }

    use crate::agents::fixture::{CLAUDE, ID, World, list_call};
    use crate::mux::Output;
    use crate::proc::Signals;
    use crate::testing::TempDir;
    use serde_json::json;

    // One call through the entry's dispatcher against the world's fakes.
    fn call(w: &World, tokens: &[&str]) -> (u8, String) {
        run(&args(tokens), || Ok(&w.ctx))
    }

    // A valid argv for each public verb.
    fn valid_call(verb: &str) -> Vec<&'static str> {
        match verb {
            "spawn" => vec!["spawn", "-Client", "claude", "-Task", "hi"],
            "list" => vec!["list"],
            "focus" => vec!["focus", "worker"],
            "send" => vec!["send", "worker", "-Body", "hi"],
            "read" => vec!["read"],
            "stop" => vec!["stop", "worker"],
            "status" => vec!["status", "-BodyFile", "s.md"],
            "sidebar" => vec!["sidebar"],
            "watch" => vec!["watch"],
            other => panic!("no valid call for {other}"),
        }
    }

    #[test]
    fn every_verb_runs_sandbox_check_before_mux() {
        let refused = [
            (Signals::SandboxCheck(1), "sandbox_check reports a sandbox"),
            (Signals::Status(None), "/proc/self/status is unreadable"),
        ];
        for def in VERBS {
            for (signals, message) in &refused {
                let w = World::human("");
                w.fakes.clear_log();
                w.fakes.signals(signals.clone());
                let (code, out) = call(&w, &valid_call(def.name));
                let want = format!(
                    r#"{{"ok":false,"verb":"{}","code":2,"check":"sandbox","message":"{message}"}}"#,
                    def.name
                );
                assert_eq!((code, out.as_str()), (2, want.as_str()), "{}", def.name);
                assert!(w.fakes.mux_calls().is_empty(), "{}", def.name);
                assert_eq!(w.fakes.events(), ["sandbox"], "{}", def.name);
            }
        }
    }

    // `[<scenario>]` then one line per multiplexer call, as tests/fixtures/mux-argv.txt writes them.
    fn render(w: &World, scenario: &str) -> String {
        let state = w.ctx.state_root.to_str().unwrap().to_string();
        let cwd = w.ctx.cwd.to_str().unwrap().to_string();
        let exe = w.ctx.exe.to_str().unwrap().to_string();
        let calls = w.fakes.mux_calls();
        // The new agent's id, as its tags name it.
        let id = calls
            .iter()
            .flat_map(|c| c.windows(2))
            .find(|pair| pair[0] == "@agent-id")
            .map(|pair| pair[1].clone());
        let spec = id.as_ref().map(|id| {
            w.ctx
                .state_root
                .join(&w.caller().server_key)
                .join("agents")
                .join(id)
                .join("spec.json")
                .to_str()
                .unwrap()
                .to_string()
        });
        let env = format!("MUX_AGENTS_STATE={state}");
        let mut out = format!("[{scenario}]\n");
        for c in &calls {
            let marker = list_call(c).then(|| c[6].split('|').next().unwrap().to_string());
            let tokens: Vec<String> = c
                .iter()
                .map(|t| {
                    assert!(!t.contains(' '), "a token with a space: {t:?}");
                    if Some(t) == spec.as_ref() {
                        "<spec>".into()
                    } else if Some(t) == id.as_ref() {
                        "<id>".into()
                    } else if *t == cwd {
                        "<cwd>".into()
                    } else if *t == exe {
                        "<exe>".into()
                    } else if *t == env {
                        "MUX_AGENTS_STATE=<state>".into()
                    } else if let Some(m) = &marker {
                        t.replace(m.as_str(), "<marker>")
                    } else {
                        t.clone()
                    }
                })
                .collect();
            out.push_str(&tokens.join(" "));
            out.push('\n');
        }
        out
    }

    // A human caller in %1 with live agent `worker` (%9) and a committed agent (%8) whose pane is gone from the list.
    fn agents_world() -> World {
        let w = World::human("");
        w.agent_record(ID, json!({"name": "worker", "pane": "%9", "window": "@5"}));
        w.agent_record(
            "bbbbbbbbbbbb",
            json!({"name": "gone", "pane": "%8", "window": "@4"}),
        );
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        w.fakes
            .mux_rule("#{session_id}|#{@agent-id}", 0, "$3|aaaaaaaaaaaa\n", "");
        w
    }

    fn spawn_world() -> World {
        let w = World::human("");
        w.fakes.mux_rule("new-window", 0, "%12|@7\n", "");
        w.fakes.mux_rule("split-window", 0, "%12|@7\n", "");
        w
    }

    // A Claude agent caller in full bypass by its transcript and its pane's mode line.
    fn claude_world(home: &TempDir) -> World {
        const SID: &str = "11111111-2222-4333-8444-555555555555";
        let mut w = World::agent("");
        let mut claude = crate::testing::row(CLAUDE, crate::agents::fixture::PANE, 1500);
        claude.argv = Some(vec!["claude".into()]);
        w.fakes.process(claude);
        w.fakes.prefer_last();
        let project = home.path().join(".claude").join("projects").join("p");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join(format!("{SID}.jsonl")),
            format!("{{\"type\":\"permission-mode\",\"permissionMode\":\"bypassPermissions\",\"sessionId\":\"{SID}\"}}\n"),
        )
        .unwrap();
        w.ctx.home = home.path().to_path_buf();
        let mut env: Vec<(String, String)> = w
            .ctx
            .env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        env.push(("CLAUDE_CODE_SESSION_ID".into(), SID.into()));
        w.ctx.env = crate::ctx::Env::from_pairs(env);
        w.fakes.mux_rule("#{pane_height}", 0, "40\n", "");
        w.fakes.mux_rule(
            "capture-pane",
            0,
            "work\n  \u{23f5}\u{23f5} bypass permissions on (shift+tab to cycle)\n",
            "",
        );
        w.file("t.md", b"build it");
        w.file("b.md", b"the brief");
        w
    }

    fn sidebar_world() -> World {
        let w = World::human("");
        w.fakes.mux_rule("split-window", 0, "%13|@1\n", "");
        w
    }

    #[test]
    fn recorded_argv_matches_fixture() {
        let mut got = String::new();
        let mut record = |w: &World, scenario: &str, tokens: &[&str], want: u8| {
            w.fakes.clear_log();
            let (code, out) = call(w, tokens);
            assert_eq!(code, want, "{scenario}: {out}");
            got.push_str(&render(w, scenario));
            got.push('\n');
        };
        record(
            &spawn_world(),
            "spawn -Client claude -Task hi",
            &["spawn", "-Client", "claude", "-Task", "hi"],
            0,
        );
        record(
            &spawn_world(),
            "spawn -Client copilot -Task hi -Split v -Name helper",
            &[
                "spawn", "-Client", "copilot", "-Task", "hi", "-Split", "v", "-Name", "helper",
            ],
            0,
        );
        let w = spawn_world();
        w.fakes.mux_first("set-option", 1, "", "no such pane");
        record(
            &w,
            "spawn -Client claude -Task hi, tagging fails",
            &["spawn", "-Client", "claude", "-Task", "hi"],
            1,
        );
        let home = TempDir::new();
        record(
            &claude_world(&home),
            "spawn -Client claude -TaskFile t.md -BriefFile b.md, from a Claude agent",
            &[
                "spawn",
                "-Client",
                "claude",
                "-TaskFile",
                "t.md",
                "-BriefFile",
                "b.md",
            ],
            2,
        );
        record(&agents_world(), "list", &["list"], 0);
        record(&agents_world(), "list -All", &["list", "-All"], 0);
        record(&agents_world(), "focus worker", &["focus", "worker"], 0);
        record(
            &agents_world(),
            "send worker -Body hi",
            &["send", "worker", "-Body", "hi"],
            0,
        );
        record(&agents_world(), "read", &["read"], 0);
        record(
            &agents_world(),
            "read worker -Lines 5",
            &["read", "worker", "-Lines", "5"],
            0,
        );
        record(&agents_world(), "stop worker", &["stop", "worker"], 0);
        let w = World::agent(ID);
        w.agent_record(ID, json!({"pane": "%1"}));
        w.file("s.md", b"parsing the config");
        record(
            &w,
            "status -BodyFile s.md, from a Claude agent",
            &["status", "-BodyFile", "s.md"],
            0,
        );
        record(&sidebar_world(), "sidebar", &["sidebar"], 0);
        let w = sidebar_world();
        w.fakes.mux_script("-l 42", |_| {
            Ok(Output {
                code: 1,
                stdout: String::new(),
                stderr: "size invalid\n".into(),
            })
        });
        record(&w, "sidebar, -l refused", &["sidebar"], 0);
        let w = World::human("");
        w.fakes.mux_rule("|end", 0, "%13|@1|sidebar||end\n", "");
        w.fakes.mux_rule("list-panes", 0, "%13|@1|sidebar|\n", "");
        record(&w, "sidebar -Close", &["sidebar", "-Close"], 0);
        // watch draws only to a terminal, so its one frame is drawn through watch_on; a closed output ends it.
        let w = World::human("");
        w.fakes.clear_log();
        let request = w.request(&["watch"]);
        let reply = crate::sidebar::watch_on(&w.ctx, &request, true, &mut |_| {
            Err(std::io::Error::other("closed"))
        })
        .unwrap();
        assert_eq!(reply["ended"], "output-closed");
        got.push_str(&render(&w, "watch, one frame"));
        got.push('\n');

        let fixture = include_str!("../tests/fixtures/mux-argv.txt");
        let mut want = String::new();
        for line in fixture.lines().filter(|l| !l.starts_with('#')) {
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') && !want.is_empty() {
                want.push('\n');
            }
            // A Windows pane gets no environment.
            let line = if cfg!(windows) {
                line.replace(" -e MUX_AGENTS_STATE=<state>", "")
            } else {
                line.to_string()
            };
            want.push_str(&line);
            want.push('\n');
        }
        want.push('\n');
        assert_eq!(got, want);
    }

    // One exported string of tests/claude/spawn-replies.ts.
    fn reply_fixture(name: &str) -> String {
        let ts = include_str!("../tests/claude/spawn-replies.ts");
        let head = format!("export const {name} = ");
        let line = ts
            .lines()
            .find_map(|l| l.strip_prefix(&head))
            .unwrap_or_else(|| panic!("{name} is not in spawn-replies.ts"));
        serde_json::from_str(line).unwrap()
    }

    // The reply with the random id and session id replaced by the fixture's fixed ones.
    fn fixed_ids(out: &str, id: &str, session: &str) -> String {
        out.replace(id, "0123456789ab")
            .replace(session, "00000000-0000-4000-8000-000000000000")
    }

    #[test]
    fn spawn_replies_fixture_matches() {
        let w = spawn_world();
        w.fakes.mux_first("new-window", 0, "%9|@5\n", "");
        let (code, out) = call(&w, &["spawn", "-Client", "claude", "-Task", "hi"]);
        assert_eq!(code, 0, "{out}");
        let reply: Value = serde_json::from_str(&out).unwrap();
        let (id, session) = (
            reply["id"].as_str().unwrap(),
            reply["sessionId"].as_str().unwrap(),
        );
        assert_eq!(fixed_ids(&out, id, session), reply_fixture("SPAWN_JSON"));

        let w = spawn_world();
        w.fakes.mux_first("new-window", 0, "%9|@5\n", "");
        let (code, out) = call(&w, &["spawn", "-Client", "claude", "-Task", "hi", "-Text"]);
        assert_eq!(code, 0, "{out}");
        let field = |name: &str| {
            out.lines()
                .find_map(|l| l.strip_prefix(&format!("{name}: ")))
                .unwrap()
                .to_string()
        };
        let (id, session) = (field("id"), field("sessionId"));
        assert_eq!(fixed_ids(&out, &id, &session), reply_fixture("SPAWN_TEXT"));

        let w = World::human(ID);
        let (code, out) = call(&w, &["spawn", "-Client", "claude", "-Task", "hi"]);
        assert_eq!((code, out), (2, reply_fixture("TAGGED_JSON")));
        let (code, out) = call(&w, &["spawn", "-Client", "claude", "-Task", "hi", "-Text"]);
        assert_eq!((code, out), (2, reply_fixture("TAGGED_TEXT")));
    }

    #[test]
    fn launcher_subcommand_hidden_from_usage() {
        // Not a verb: neither the table nor the usage text names it, and the parser refuses it as unknown.
        assert!(VERBS.iter().all(|d| d.name != "launcher"));
        let (code, check) = usage_of(&["launcher", "/s/spec.json"]);
        assert_eq!((code, check), (2, "usage"));
        let (code, out) = run(&args(&["launcher", "/s/spec.json"]), || -> Res<Ctx> {
            panic!("a refused call builds no providers")
        });
        assert_eq!(code, 2);
        assert!(!out.contains("launcher"), "{out}");
        // Only the exact `launcher <spec>` reaches the pane launcher.
        assert_eq!(
            launcher_spec(&args(&["launcher", "/s/spec.json"])),
            Some("/s/spec.json")
        );
        for tokens in [
            &["launcher"][..],
            &["launcher", "a", "b"],
            &["Launcher", "/s/spec.json"],
            &["list", "/s/spec.json"],
            &[],
        ] {
            assert_eq!(launcher_spec(&args(tokens)), None, "{tokens:?}");
        }
    }
}
