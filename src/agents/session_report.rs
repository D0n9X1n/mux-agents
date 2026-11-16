//! `report -Session` (#103 report-2b K3, K3a, K4): a client's own hook names its conversation. It writes that
//! client's registry entry and, from a generation's terminal, runs the readiness handshake. It proves no member and
//! grants nothing; it writes only the K2 entry, `report.json` and, through `gens::promote_ready`, `ready.json`.
use serde_json::{Map, Value, json};

use super::ancestry::{self, Above};
use crate::cli::Request;
use crate::ctx::Ctx;
use crate::error::Res;

pub fn run(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let (session, source) = input(request)?;
    let mut reply = Map::new();
    reply.insert("sessionId".into(), session.clone().into());
    reply.insert("source".into(), source.clone().into());
    // K3 and PM call 9: no client, a walk that cannot be trusted, or an unresolved identity writes nothing, exit 0.
    let (kind, (pid, start)) = match ancestry::first_client(ctx) {
        Above::Client(row, kind) => match ancestry::identity(ctx, &row, kind) {
            Some(found) => (kind, found),
            None => return Ok(nothing(reply, "unreadable".into())),
        },
        Above::None => return Ok(nothing(reply, Value::Null)),
        Above::Unreadable => return Ok(nothing(reply, "unreadable".into())),
    };
    let client = json!({"kind": super::spawn::client_name(kind), "pid": pid, "start": start});
    reply.insert("client".into(), client);
    // K2; an entry that cannot be written does not stop K4 (ambiguity 5).
    let seq = match crate::state::live::report(ctx, kind, pid, start, &session, &source) {
        Ok(seq) => Value::from(seq),
        Err(refusal) => refusal.check.into(),
    };
    reply.insert("seq".into(), seq);
    let (handshake, ready) = match labels(ctx) {
        Labels::None => ("none".into(), Value::Null),
        Labels::Unreadable => ("evidence".into(), Value::Null),
        Labels::Gen { key, id, n } => exchange(ctx, &key, &id, n, &session, (pid, start)),
    };
    reply.insert("handshake".into(), handshake);
    reply.insert("ready".into(), ready);
    Ok(reply)
}

fn nothing(mut reply: Map<String, Value>, client: Value) -> Map<String, Value> {
    reply.insert("client".into(), client);
    reply.insert("seq".into(), Value::Null);
    reply.insert("handshake".into(), "none".into());
    reply.insert("ready".into(), Value::Null);
    reply
}

/// The session and source, from `-Stdin`'s JSON (Copilot's `{sessionId, source, ..}`) or the positional and
/// `-Source`. An invalid session id is usage; an invalid or missing source is `unknown` (ambiguity 17).
fn input(request: &Request) -> Res<(String, String)> {
    let usage = || super::usage("report");
    let (session, source) = if request.switch("Stdin") {
        let body: Value = read_stdin()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or_else(usage)?;
        let field = |k: &str| body.get(k).and_then(Value::as_str).map(str::to_string);
        (field("sessionId"), field("source"))
    } else {
        (
            request.positional.first().cloned(),
            request.value("Source").map(str::to_string),
        )
    };
    let session = session
        .filter(|s| crate::checks::session_id(s))
        .ok_or_else(usage)?
        .to_ascii_lowercase();
    let word =
        |s: &String| !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_lowercase());
    Ok((
        session,
        source.filter(word).unwrap_or_else(|| "unknown".into()),
    ))
}

/// Ambiguity 17: never from a terminal; at most 64 KiB, and more than that is no input at all.
pub(crate) fn read_capped(terminal: bool, input: impl std::io::Read) -> Option<Vec<u8>> {
    use std::io::Read;
    const MAX: u64 = 64 * 1024;
    if terminal {
        return None;
    }
    let mut bytes = Vec::new();
    input.take(MAX + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= MAX).then_some(bytes)
}

/// What `-Stdin` reads: its bytes, and whether they come from a terminal. `std::io::IsTerminal` is sealed, so a test
/// reader cannot implement it; this trait is implemented for the real stdin, for files, and for test readers
/// (build checklist item 11).
pub(crate) trait Input: std::io::Read {
    fn is_terminal(&self) -> bool;
}

impl Input for std::io::StdinLock<'_> {
    fn is_terminal(&self) -> bool {
        std::io::IsTerminal::is_terminal(self)
    }
}

impl Input for std::fs::File {
    fn is_terminal(&self) -> bool {
        std::io::IsTerminal::is_terminal(self)
    }
}

/// The production reader (fix 2, re-audit #7): the terminal check is asked of the very input that is read.
pub(crate) fn read_input(input: impl Input) -> Option<Vec<u8>> {
    let terminal = input.is_terminal();
    read_capped(terminal, input)
}

/// `-Stdin`'s bytes: one wrapper for release and tests, so the terminal check that runs is the shipped one.
fn read_stdin() -> Option<Vec<u8>> {
    read_input(stdin_source())
}

/// The real stdin.
#[cfg(not(test))]
fn stdin_source() -> std::io::StdinLock<'static> {
    std::io::stdin().lock()
}

/// Tests: the seam's reader stands in for stdin (build checklist item 11).
#[cfg(test)]
fn stdin_source() -> hooks::Seam {
    hooks::reader()
}

/// The terminal's `mux.*` labels (K3a), read only in an hc terminal (ambiguity 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Labels {
    /// Not an hc terminal, or one with none of `mux.team`, `mux.id`, `mux.gen`: registry only.
    None,
    /// An hc error, or a partial or malformed set: no handshake.
    Unreadable,
    Gen {
        key: String,
        id: String,
        n: u64,
    },
}

/// The terminal's `mux.*` labels: only when `caller::hc::applies` (an hc terminal, no tmux or rmux).
pub(crate) fn labels(ctx: &Ctx) -> Labels {
    if !crate::caller::hc::applies(ctx) {
        return Labels::None;
    }
    let Some(address) = ctx.env.get("HC_TERMINAL_ID") else {
        return Labels::None;
    };
    let Ok(t) = crate::hc::connect(ctx).and_then(|core| core.get(address)) else {
        return Labels::Unreadable;
    };
    let get = |k: &str| t.labels.get(k).map(String::as_str);
    match (get("mux.team"), get("mux.id"), get("mux.gen")) {
        (None, None, None) => Labels::None,
        (Some(team), Some(id), Some(n)) => {
            let key = team.split_once(':').map(|(k, _)| k).unwrap_or_default();
            match super::gens::plain_gen(n) {
                Some(n) if super::gens::plain_key(key) && super::gens::plain_id(id) => {
                    Labels::Gen {
                        key: key.into(),
                        id: id.into(),
                        n,
                    }
                }
                _ => Labels::Unreadable,
            }
        }
        _ => Labels::Unreadable,
    }
}

/// K4; Task 5 replaces this body.
fn exchange(
    _: &Ctx,
    _key: &str,
    _id: &str,
    _n: u64,
    _session: &str,
    _client: (u32, i64),
) -> (Value, Value) {
    ("none".into(), Value::Null)
}

#[cfg(test)]
pub(crate) mod hooks {
    use std::cell::RefCell;
    thread_local! {
        static STDIN: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
    }
    /// What `-Stdin` reads on this thread; None stands for a terminal stdin.
    pub fn stdin(bytes: Option<&[u8]>) {
        STDIN.with(|s| *s.borrow_mut() = bytes.map(<[u8]>::to_vec));
    }

    /// The seam's reader (build checklist item 11). A terminal stdin carries a whole, valid body, so only the
    /// terminal check can refuse it.
    pub(super) struct Seam {
        terminal: bool,
        bytes: std::io::Cursor<Vec<u8>>,
    }

    impl std::io::Read for Seam {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.bytes.read(buf)
        }
    }

    impl super::Input for Seam {
        fn is_terminal(&self) -> bool {
            self.terminal
        }
    }

    pub(super) fn reader() -> Seam {
        let set = STDIN.with(|s| s.borrow().clone());
        let terminal = set.is_none();
        let body = br#"{"sessionId":"0f11fc2f-2032-48ef-a920-4af2a8e5af50","source":"startup"}"#;
        let bytes = set.unwrap_or_else(|| body.to_vec());
        Seam {
            terminal,
            bytes: std::io::Cursor::new(bytes),
        }
    }
}
