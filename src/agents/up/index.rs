//! reentry-6a E1: `<root>/up/<T>.json`, the one index of team T. Written only here, under `up|T`; durable evidence
//! (calls 14, 38): every folder from `up/` to the volume root flushed on every write and read, readers complete
//! outstanding flushes before acting, a linked `up/` refused, every recorded server tuple proven to hash its own fields.
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::caller::Anchor;
use crate::ctx::Ctx;
use crate::error::{Refusal, Res};
use crate::state::evidence::{self, Evidence};

pub const SCHEMA: u64 = 1;

/// What `up` was asked for (ambiguity 1): every E6 step builds from these, never from a later caller's flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inputs {
    pub client: String,
    pub options: Vec<String>,
    pub prompt: Vec<String>,
    pub name: String,
    pub folder: String,
}

/// A multiplexer server: its socket (or pipe), pid, start and the key that hashes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub socket: String,
    pub pid: u32,
    pub start: i64,
    pub key: String,
}

/// A team session on a server: the published view, or an alias it left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub server: Server,
    pub session: String,
    pub name: String,
}

/// A session asked for and not yet proven: its name and the `hold` release that ends it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub server: Server,
    pub name: String,
    pub release: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    pub team: String,
    pub inputs: Inputs,
    pub anchor: Option<Anchor>,
    pub view: Option<View>,
    pub pending: Option<Pending>,
    pub aliases: Vec<View>,
    /// Absolute hold-release paths still to publish (calls 25, 35).
    pub releases: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proof {
    Live,
    Gone,
}

fn refusal(code: u8, check: &'static str, message: impl Into<String>) -> Refusal {
    Refusal {
        code,
        check,
        message: message.into(),
        reason: None,
        leftover: None,
        details: Default::default(),
    }
}
/// 1 `index` with `message`.
pub fn bad(message: impl Into<String>) -> Refusal {
    refusal(1, "index", message)
}
fn not_flushed() -> Refusal {
    refusal(1, "state", crate::agents::gens::NOT_FLUSHED)
}
fn unreadable(at: &Path) -> Refusal {
    bad(format!(
        "the team index cannot be read; repair or remove {}",
        at.display()
    ))
}
pub fn path(ctx: &Ctx, team: &str) -> PathBuf {
    ctx.state_root.join("up").join(format!("{team}.json"))
}
/// `caller.rs`'s server key.
pub fn key_of(socket: &str, pid: u32, start: i64) -> String {
    crate::util::sha256_hex(format!("{socket}|{pid}|{start}"))[..16].to_string()
}
pub fn server_of(c: &crate::caller::Caller) -> Server {
    Server {
        socket: c.socket.clone(),
        pid: c.server_pid,
        start: c.server_start,
        key: c.server_key.clone(),
    }
}
/// Ambiguity 3: live with the recorded start; gone on no row or another start; a key not hashing its fields: `index`.
pub fn prove(ctx: &Ctx, s: &Server) -> Res<Proof> {
    if key_of(&s.socket, s.pid, s.start) != s.key {
        return Err(bad(
            "the team index names a server key that does not match its server",
        ));
    }
    Ok(match ctx.os.open(s.pid) {
        Some(o) if ctx.os.same_start(o.start, s.start) => Proof::Live,
        _ => Proof::Gone,
    })
}

#[cfg(test)]
thread_local! {
    // A path whose metadata read fails with `PermissionDenied` on this thread (R15).
    static META_FAULT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}
/// Test seam (R15): `symlink_metadata(p)` fails with `PermissionDenied` on this thread (none: no failure).
#[cfg(test)]
pub fn fail_metadata(p: Option<&Path>) {
    META_FAULT.with(|f| *f.borrow_mut() = p.map(Path::to_path_buf));
}
/// `symlink_metadata` (`follow`: `metadata`) through the test seam `fail_metadata`.
fn stat(p: &Path, follow: bool) -> std::io::Result<std::fs::Metadata> {
    #[cfg(test)]
    if META_FAULT.with(|f| f.borrow().as_deref() == Some(p)) {
        return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    }
    if follow {
        std::fs::metadata(p)
    } else {
        std::fs::symlink_metadata(p)
    }
}
/// Only a missing name, or one under a file, lets the absence walk go on (R15); anything else is unreadable.
fn missing(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// A real folder: not a link, not a Windows junction (its reparse point does not show in `is_dir`).
fn real_dir(m: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    m.is_dir() && !m.file_type().is_symlink()
}
/// `<root>/up` when it exists: a real folder, else 1 `index` naming it (call 14). Absent: `None` only when the first
/// ancestor that exists is a folder; a file in a folder's place is 1 `index` naming it on every OS (call 31). A
/// metadata error other than a missing name is 1 `index` naming the path, never absence (R15).
fn up_dir(ctx: &Ctx) -> Res<Option<PathBuf>> {
    let up = ctx.state_root.join("up");
    let not_real = |p: &Path| bad(format!("{} is not a real folder", p.display()));
    let cannot = |p: &Path| bad(format!("{} cannot be read", p.display()));
    match stat(&up, false) {
        Ok(m) if real_dir(&m) => return Ok(Some(up)),
        Ok(_) => return Err(not_real(&up)),
        Err(e) if missing(&e) => {}
        Err(_) => return Err(cannot(&up)),
    }
    let mut at = up.parent();
    while let Some(d) = at.filter(|d| !d.as_os_str().is_empty()) {
        // An ancestor may be a link to a folder (macOS `/tmp`): followed here, unlike `up/` itself.
        match stat(d, true) {
            Ok(m) if m.is_dir() => return Ok(None),
            Ok(_) => return Err(not_real(d)),
            Err(e) if missing(&e) => at = d.parent(),
            Err(_) => return Err(cannot(d)),
        }
    }
    Ok(None)
}
/// Call 38 (R1): the folders an index depends on, from `up/` to the volume root, flushed on every write and read,
/// so a crash after folders were made is finished by the next writer or reader without remembering what was made.
/// `up/` and the root through `sync_dir`; every folder above the root through `sync_ancestor` (skipped only where
/// this user cannot add an entry, as `leader::flush_to_root`).
fn flush_chain(ctx: &Ctx) -> Res<()> {
    let root = &ctx.state_root;
    for dir in [root.join("up"), root.clone()] {
        crate::state::fs::sync_dir(&dir).map_err(|_| not_flushed())?;
    }
    for dir in root
        .ancestors()
        .skip(1)
        .filter(|d| !d.as_os_str().is_empty())
    {
        crate::state::fs::sync_ancestor(dir).map_err(|_| not_flushed())?;
    }
    Ok(())
}

fn server_json(s: &Server) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("socket".into(), json!(s.socket));
    m.insert("serverPid".into(), json!(s.pid));
    m.insert("serverStart".into(), json!(s.start));
    m.insert("serverKey".into(), json!(s.key));
    m
}
fn view_json(v: &View) -> Value {
    let mut m = server_json(&v.server);
    m.insert("session".into(), json!(v.session));
    m.insert("name".into(), json!(v.name));
    Value::Object(m)
}
fn encode(ix: &Index) -> Value {
    let i = &ix.inputs;
    let mut v = json!({"schema": SCHEMA, "team": ix.team, "aliases": ix.aliases.iter().map(view_json).collect::<Vec<_>>(),
        "releases": ix.releases,
        "inputs": {"client": i.client, "options": i.options, "prompt": i.prompt, "name": i.name, "folder": i.folder}});
    if let Some(a) = &ix.anchor {
        v["anchor"] = json!({"key": a.key, "session": a.session});
    }
    if let Some(view) = &ix.view {
        v["view"] = view_json(view);
    }
    if let Some(p) = &ix.pending {
        let mut m = server_json(&p.server);
        m.insert("name".into(), json!(p.name));
        m.insert("release".into(), json!(p.release));
        v["pending"] = Value::Object(m);
    }
    v
}
fn text(v: &Value, k: &str) -> Option<String> {
    v.get(k)?.as_str().map(String::from)
}
fn texts(v: &Value, k: &str) -> Option<Vec<String>> {
    v.get(k)?
        .as_array()?
        .iter()
        .map(|t| t.as_str().map(String::from))
        .collect()
}
/// A server tuple, proven to hash its own fields wherever it is read (call 14).
fn server(v: &Value) -> Option<Server> {
    let s = Server {
        socket: text(v, "socket")?,
        pid: u32::try_from(v.get("serverPid")?.as_u64()?).ok()?,
        start: v.get("serverStart")?.as_i64()?,
        key: text(v, "serverKey")?,
    };
    (key_of(&s.socket, s.pid, s.start) == s.key).then_some(s)
}
fn view(v: &Value) -> Option<View> {
    Some(View {
        server: server(v)?,
        session: text(v, "session")?,
        name: text(v, "name")?,
    })
}
fn absolute(p: &str) -> bool {
    Path::new(p).is_absolute()
}
/// Ambiguity 2, calls 14, 21, 35, 36: every field typed; a present `null` is malformed, not absent; a view or alias
/// without an anchor is a state no step can repair.
fn parse(team: &str, v: &Value) -> Option<Index> {
    let opt = |k: &str| match v.get(k) {
        None => Some(None),
        Some(x) if x.is_null() => None,
        Some(x) => Some(Some(x)),
    };
    let i = v.get("inputs")?;
    let pending = match opt("pending")? {
        None => None,
        Some(p) => {
            let p = Pending {
                server: server(p)?,
                name: text(p, "name")?,
                release: text(p, "release")?,
            };
            Some(Some(p).filter(|p| absolute(&p.release))?)
        }
    };
    let ix = Index {
        team: text(v, "team").filter(|t| t == team)?,
        inputs: Inputs {
            client: text(i, "client")
                .filter(|c| crate::agents::spawn::client_named(c).is_some())?,
            options: texts(i, "options")?,
            prompt: texts(i, "prompt")?,
            name: text(i, "name")?,
            folder: text(i, "folder")?,
        },
        anchor: match opt("anchor")? {
            None => None,
            Some(a) => Some(Anchor {
                key: text(a, "key")?,
                session: text(a, "session")?,
            }),
        },
        view: match opt("view")? {
            None => None,
            Some(x) => Some(view(x)?),
        },
        pending,
        aliases: v
            .get("aliases")?
            .as_array()?
            .iter()
            .map(view)
            .collect::<Option<_>>()?,
        releases: texts(v, "releases").filter(|r| r.iter().all(|p| absolute(p)))?,
    };
    let orphan_view = ix.anchor.is_none() && (ix.view.is_some() || !ix.aliases.is_empty());
    (v.get("schema")?.as_u64()? == SCHEMA && !orphan_view).then_some(ix)
}

/// E1: `Ok(None)` when absent; flushes complete before a visible index is used (calls 14, 38); malformed: 1 `index`.
pub fn read(ctx: &Ctx, team: &str) -> Res<Option<Index>> {
    if up_dir(ctx)?.is_none() {
        return Ok(None);
    }
    let at = path(ctx, team);
    match evidence::read(&at) {
        Evidence::Absent => Ok(None),
        Evidence::Present(v) => {
            flush_chain(ctx)?;
            parse(team, &v).map(Some).ok_or_else(|| unreadable(&at))
        }
        Evidence::Unreadable => Err(unreadable(&at)),
    }
}

/// 6a-R2: missing folders made private; `up/` real; the final name absent or a regular file; `team` and `inputs`
/// fixed; a durable replace. Call 31: the folders this call makes are flushed before the index can appear, and
/// removed again on a failed flush. Call 38: the whole chain to the volume root is flushed on every call.
pub fn write(ctx: &Ctx, ix: &Index) -> Res<()> {
    let up = ctx.state_root.join("up");
    if up_dir(ctx)?.is_none() {
        let made: Vec<PathBuf> = up
            .ancestors()
            .take_while(|d| std::fs::symlink_metadata(d).is_err())
            .map(Path::to_path_buf)
            .collect();
        if let Some(parent) = ctx.state_root.parent() {
            std::fs::create_dir_all(parent)?;
        }
        #[cfg(unix)]
        crate::state::private_unix::private_dirs(&ctx.state_root, &[&up])?;
        #[cfg(windows)]
        crate::state::private_windows::private_dirs(&[&up])?;
        for d in &made {
            if crate::state::fs::sync_dir(d.parent().unwrap_or(d)).is_err() {
                for d in &made {
                    let _ = std::fs::remove_dir(d);
                }
                return Err(not_flushed());
            }
        }
    }
    up_dir(ctx)?;
    let at = path(ctx, &ix.team);
    match stat(&at, false) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Ok(m) if m.is_file() => {
            if let Some(old) = read(ctx, &ix.team)?
                && old.inputs != ix.inputs
            {
                return Err(Refusal::internal("the team index's inputs are fixed"));
            }
        }
        _ => return Err(unreadable(&at)),
    }
    let mut sync_failed = false;
    let written = crate::state::fs::write_json_durable_with(&at, &encode(ix), &mut |p, is_dir| {
        let r = if is_dir {
            crate::state::fs::sync_dir(p)
        } else {
            crate::state::fs::sync_file(p)
        };
        sync_failed |= r.is_err();
        r
    });
    match written {
        Err(_) if sync_failed => return Err(not_flushed()),
        Err(_) => return Err(refusal(1, "state", crate::agents::gens::NOT_PUBLISHED)),
        Ok(()) => {}
    }
    flush_chain(ctx)
}

/// Every index under `up/`: names `<16 lowercase hex>.json` only; each through `read`, which completes its flushes;
/// any unreadable one fails the scan naming its path (calls 2, 14).
pub fn every(ctx: &Ctx) -> Res<Vec<Index>> {
    let Some(up) = up_dir(ctx)? else {
        return Ok(Vec::new());
    };
    let listed = || bad(format!("{} cannot be listed", up.display()));
    let mut out = Vec::new();
    for e in std::fs::read_dir(&up).map_err(|_| listed())? {
        let name = e
            .map_err(|_| listed())?
            .file_name()
            .to_string_lossy()
            .into_owned();
        let Some(t) = name.strip_suffix(".json") else {
            continue;
        };
        if t.len() == 16
            && t.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            out.push(read(ctx, t)?.ok_or_else(|| unreadable(&up.join(&name)))?);
        }
    }
    out.sort_by(|a, b| a.team.cmp(&b.team));
    Ok(out)
}

#[cfg(test)]
mod tests;
