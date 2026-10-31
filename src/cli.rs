//! The verb table, the argument parser and the dispatcher (MuxAgents.ps1:14-103, 1673-1681).

use std::panic::{self, AssertUnwindSafe};

use serde_json::{Map, Value};

use crate::ctx::Env;
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

/// A verb's reply fields after `ok` and `verb`.
pub type Handler = fn(&Request) -> Res<Map<String, Value>>;

pub struct VerbDef {
    pub name: &'static str,
    pub values: &'static [&'static str],
    pub switches: &'static [&'static str],
    pub min: usize,
    pub max: usize,
    pub usage: &'static str,
    pub handler: Handler,
}

fn not_wired(_: &Request) -> Res<Map<String, Value>> {
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

fn dispatch(def: &VerbDef, request: &Request, env: &Env) -> Res<Map<String, Value>> {
    #[cfg(feature = "test-panic")]
    if env.get("MUX_AGENTS_TEST_PANIC") == Some("dispatch") {
        panic!("PANIC-PAYLOAD-7f3a");
    }
    let _ = env;
    let data = (def.handler)(request)?;
    let mut reply = Map::new();
    reply.insert("ok".into(), true.into());
    reply.insert("verb".into(), def.name.into());
    reply.extend(data);
    Ok(reply)
}

/// Parse, run the verb and map the outcome to exit code 0, 1 or 2 and the reply text, never echoing an
/// error's own text. A panic in the verb is 1 `internal`; unwinding has already released its locks.
pub fn run(args: &[String], env: &Env) -> (u8, String) {
    let (verb, text, outcome) = match parse(args) {
        Err(refusal) => (None, false, Err(refusal)),
        Ok((def, request)) => {
            let outcome = panic::catch_unwind(AssertUnwindSafe(|| dispatch(def, &request, env)))
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
