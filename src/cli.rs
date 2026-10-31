//! The verb table, the argument parser and the dispatcher (MuxAgents.ps1:14-103, 1673-1681).

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

fn not_wired(_: &Ctx, _: &Request) -> Res<Map<String, Value>> {
    refuse(1, "internal", "not wired")
}

pub static VERBS: &[VerbDef] = &[
    VerbDef {
        name: "spawn",
        values: &["Client", "Task", "TaskFile", "BriefFile", "Name", "Split"],
        switches: &["NoFork"],
        min: 0,
        max: 0,
        usage: "usage: mux-agent spawn -Client <claude|copilot> [-Task <text>] [-TaskFile <path>] [-BriefFile <path>] [-Name <name>] [-Split <value>] [-NoFork] [-Text]",
        handler: not_wired,
    },
    VerbDef {
        name: "list",
        values: &[],
        switches: &["All"],
        min: 0,
        max: 0,
        usage: "usage: mux-agent list [-All] [-Text]",
        handler: not_wired,
    },
    VerbDef {
        name: "focus",
        values: &[],
        switches: &[],
        min: 1,
        max: 1,
        usage: "usage: mux-agent focus <name> [-Text]",
        handler: not_wired,
    },
    VerbDef {
        name: "send",
        values: &["Body", "BodyFile"],
        switches: &[],
        min: 1,
        max: 1,
        usage: "usage: mux-agent send <name|parent|%pane> (-Body <text> | -BodyFile <path>) [-Text]",
        handler: not_wired,
    },
    VerbDef {
        name: "read",
        values: &["Wait", "Lines"],
        switches: &[],
        min: 0,
        max: 1,
        usage: "usage: mux-agent read [-Wait <seconds>] | read <name> [-Lines <n>] [-Text]",
        handler: not_wired,
    },
    VerbDef {
        name: "stop",
        values: &[],
        switches: &[],
        min: 1,
        max: 1,
        usage: "usage: mux-agent stop <name> [-Text]",
        handler: not_wired,
    },
    VerbDef {
        name: "status",
        values: &["Body", "BodyFile"],
        switches: &[],
        min: 0,
        max: 0,
        usage: "usage: mux-agent status (-Body <text> | -BodyFile <path>) [-Text]",
        handler: not_wired,
    },
    VerbDef {
        name: "sidebar",
        values: &[],
        switches: &["Close"],
        min: 0,
        max: 0,
        usage: "usage: mux-agent sidebar [-Close] [-Text]",
        handler: not_wired,
    },
    VerbDef {
        name: "watch",
        values: &[],
        switches: &[],
        min: 0,
        max: 0,
        usage: "usage: mux-agent watch [-Text]",
        handler: not_wired,
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
    let data = (def.handler)(ctx, request)?;
    let mut reply = Map::new();
    reply.insert("ok".into(), true.into());
    reply.insert("verb".into(), def.name.into());
    reply.extend(data);
    Ok(reply)
}

/// Parse, run the verb and map the outcome to exit code 0, 1 or 2 and the reply text, never echoing an
/// error's own text. A panic in the verb is 1 `internal`; unwinding has already released its locks.
pub fn run(args: &[String], ctx: &Ctx) -> (u8, String) {
    let (verb, text, outcome) = match parse(args) {
        Err(refusal) => (None, false, Err(refusal)),
        Ok((def, request)) => {
            let outcome = panic::catch_unwind(AssertUnwindSafe(|| dispatch(def, &request, ctx)))
                .unwrap_or_else(|_| refuse(1, "internal", "unexpected error (panic)"));
            (Some(def.name), request.text, outcome)
        }
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
        assert_eq!(fakes.os_calls(), ["row 42"]);
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
}
