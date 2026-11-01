//! The session's agent panes, their records and exit notices (MuxAgents.ps1:535-546, 1014-1018, 1150-1204,
//! 1287-1296).

use serde_json::{Map, Value, json};

use super::mail;
use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::error::{Res, refuse};

/// One `list-panes` row tagged with a well-formed agent id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    pub pane: String,
    pub window: String,
    pub pid: String,
    pub id: String,
    pub name: String,
    // `#{pane_dead}` 1: its program ended and remain-on-exit kept the pane.
    pub dead: bool,
}

const LIST_FORMAT: &str =
    "#{pane_id}|#{window_id}|#{pane_pid}|#{pane_dead}|#{@agent-id}|#{@agent-name}";

/// The tagged-pane list format: each row framed by `marker` on both ends.
pub(crate) fn list_format(marker: &str) -> String {
    format!("{marker}|{LIST_FORMAT}|{marker}")
}
const GONE_PROBE: &str = "#{pane_id}|#{@agent-id}";

/// The caller session's panes tagged with a well-formed agent id and, unless `all`, only agents with
/// commit.json. Only reads: the state folder is never made here.
pub fn get_panes(ctx: &Ctx, caller: &Caller, all: bool) -> Res<Vec<Pane>> {
    let tagged = tagged(ctx, caller)?;
    Ok(if all {
        tagged
    } else {
        committed(ctx, caller, tagged)
    })
}

/// Every tagged pane of the caller's session, one `list-panes` call. Each row is framed by a fresh random marker on
/// both ends and read by that frame over the whole output: an @agent-name may hold LF or CR, and a live pane is never
/// dropped for its name, nor does its name's text read as a row of its own. A row whose pane, window, pid (a decimal of at least 2: never the kernel's or init's tree) or id is malformed is
/// not an agent.
pub(crate) fn tagged(ctx: &Ctx, caller: &Caller) -> Res<Vec<Pane>> {
    let marker = crate::util::guid()?.replace('-', "");
    let format = list_format(&marker);
    let listed = crate::mux::native(
        ctx,
        &["list-panes", "-s", "-t", &caller.session, "-F", &format],
        false,
    )?;
    let (head, tail) = (format!("{marker}|"), format!("|{marker}"));
    Ok(records(&listed.stdout, &head, &tail)
        .into_iter()
        .filter_map(|fields| {
            let mut parts = fields.splitn(6, '|');
            let (pane, window, pid, dead, id, name) = (
                parts.next()?,
                parts.next()?,
                parts.next()?,
                parts.next()?,
                parts.next()?,
                parts.next()?,
            );
            let window_ok = window
                .strip_prefix('@')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
            let pid = pid
                .bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| pid.parse::<u32>().ok())
                .flatten()
                .filter(|&pid| pid >= 2)?;
            (super::pane_id(pane) && window_ok && super::agent_id(id)).then(|| Pane {
                pane: pane.into(),
                window: window.into(),
                pid: pid.to_string(),
                id: id.into(),
                name: name.into(),
                // Only an exact 1 is dead: anything else is a live pane, never killed.
                dead: dead == "1",
            })
        })
        .collect())
}

/// The text of each record in `stdout`: from an opening `head` to the next closing `tail`. A record may hold LF and
/// CR (an @agent-name may), so records are found by their frame, never by line; text between records is skipped.
fn records<'a>(stdout: &'a str, head: &str, tail: &str) -> Vec<&'a str> {
    let mut found = Vec::new();
    let mut rest = stdout;
    while let Some(start) = rest.find(head) {
        let body = &rest[start + head.len()..];
        let Some(end) = body.find(tail) else {
            break;
        };
        found.push(&body[..end]);
        rest = &body[end + tail.len()..];
    }
    found
}

/// The panes whose agent has commit.json (a file).
pub(crate) fn committed(ctx: &Ctx, caller: &Caller, panes: Vec<Pane>) -> Vec<Pane> {
    let agents = super::server_dir(ctx, caller).join("agents");
    panes
        .into_iter()
        .filter(|p| agents.join(&p.id).join("commit.json").is_file())
        .collect()
}

/// record.json of a well-formed id; `None` when missing or not an object.
pub fn record(ctx: &Ctx, caller: &Caller, id: &str) -> Res<Option<Value>> {
    if !super::agent_id(id) {
        return Ok(None);
    }
    let path = super::server_dir(ctx, caller)
        .join("agents")
        .join(id)
        .join("record.json");
    Ok(crate::state::fs::read_json(&path)?.filter(Value::is_object))
}

/// The live committed agent named `name` in the caller's session; refuses 2 `target` otherwise.
pub fn find_pane(ctx: &Ctx, caller: &Caller, name: &str) -> Res<Pane> {
    let none = || refuse(2, "target", "no live agent has that name");
    if !super::agent_name(name) {
        return none();
    }
    match get_panes(ctx, caller, false)?
        .into_iter()
        .find(|p| p.name == name)
    {
        Some(pane) => Ok(pane),
        None => none(),
    }
}

/// The caller's name and mailbox key: an agent pane is its record's name and id; any other pane is root.
#[derive(Debug, Clone, PartialEq)]
pub struct Me {
    pub name: String,
    pub key: String,
    pub record: Option<Value>,
}

pub fn self_of(ctx: &Ctx, caller: &Caller) -> Res<Me> {
    let tag = caller.agent_tag.as_str();
    if tag.is_empty() {
        return Ok(Me {
            name: "root".into(),
            key: mail::mailbox_key(&caller.pane, "")?,
            record: None,
        });
    }
    // The key names a mailbox file, so the record must be this pane's own: its id is the tag.
    match record(ctx, caller, tag)? {
        Some(record) if record.get("id").and_then(Value::as_str) == Some(tag) => Ok(Me {
            name: super::text(record.get("name")),
            key: tag.to_string(),
            record: Some(record),
        }),
        _ => refuse(2, "parent", "this agent pane has no record"),
    }
}

/// An agent's one exit notice.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    Exited(Value),
    Stopped,
    Lost,
}

/// Posts agent `id`'s one exit notice to its parent pane's mailbox; false when already posted. The caller holds
/// the notice lock. The message goes first and notice.json last, so a failed post is retried by the next list,
/// read or spawn; the fixed message id keeps a retry after a failed marker write single.
pub fn add_notice(
    ctx: &Ctx,
    caller: &Caller,
    id: &str,
    record: &Value,
    notice: Notice,
) -> Res<bool> {
    let marker = super::server_dir(ctx, caller)
        .join("agents")
        .join(id)
        .join("notice.json");
    if marker.exists() {
        return Ok(false);
    }
    let name = super::text(record.get("name"));
    let (kind, code, body) = match notice {
        Notice::Exited(code) => {
            let body = format!("{name} exited with code {}.", super::text(Some(&code)));
            ("exited", code, body)
        }
        Notice::Stopped => ("stopped", Value::Null, format!("{name} was stopped.")),
        Notice::Lost => (
            "lost",
            Value::Null,
            format!("{name} is gone: its pane closed without an exit record."),
        ),
    };
    let key = mail::mailbox_key(&super::text(record.get("parentPane")), "")?;
    let pane = super::text(record.get("pane"));
    let mut fields = Map::new();
    fields.insert("kind".into(), kind.into());
    fields.insert("exitCode".into(), code.clone());
    let delivered = mail::add_once(
        ctx,
        caller,
        &key,
        &name,
        &pane,
        &body,
        &format!("exit-{id}"),
        &fields,
    )?;
    // The first notice wins: a message already delivered (a marker write that failed after it) decides the marker.
    let (kind, code) = match delivered {
        Some(message) => delivered_notice(&message, &name),
        None => (kind.to_string(), code),
    };
    let at = crate::util::utc_o(ctx.clock.now());
    let mut written = json!({"kind": kind, "exitCode": code, "at": at});
    if kind == "lost" {
        written["closedBy"] = "pane".into();
    }
    crate::state::fs::write_json(&marker, &written)?;
    Ok(true)
}

/// The (kind, exitCode) of a delivered exit message: its `kind` and `exitCode`, else (a 0.2.1 message) its fixed
/// body `<name> exited with code <N>.`, `<name> was stopped.` or `<name> is gone: ...`; anything else is lost.
fn delivered_notice(message: &Value, name: &str) -> (String, Value) {
    let code = message.get("exitCode").filter(|c| c.is_i64());
    match message.get("kind").and_then(Value::as_str) {
        Some("exited") if code.is_some() => {
            return ("exited".into(), code.cloned().unwrap_or(Value::Null));
        }
        Some(kind @ ("stopped" | "lost")) => return (kind.into(), Value::Null),
        _ => {}
    }
    let body = super::text(message.get("body"));
    let rest = body.strip_prefix(name).unwrap_or_default();
    if rest == " was stopped." {
        return ("stopped".into(), Value::Null);
    }
    let exited = rest
        .strip_prefix(" exited with code ")
        .and_then(|n| n.strip_suffix('.'))
        .and_then(|n| n.parse::<i64>().ok());
    match exited {
        Some(n) => ("exited".into(), n.into()),
        None => ("lost".into(), Value::Null),
    }
}

/// An uncommitted agent is closed only after the launcher's 30 s commit wait and 30 s more.
const UNCOMMITTED_MAX: std::time::Duration = std::time::Duration::from_secs(60);

/// The one server-side command that kills `pane` only while it is dead and still tagged `id`: the check and the
/// kill run in one command, so a respawn or a retag between them keeps the pane.
pub fn kill_if_dead(pane: &str, id: &str) -> Vec<String> {
    vec![
        "if-shell".into(),
        "-F".into(),
        "-t".into(),
        pane.into(),
        format!("#{{&&:#{{pane_dead}},#{{==:#{{@agent-id}},{id}}}}}"),
        format!("kill-pane -t {pane}"),
    ]
}

/// Closes the caller session's agents whose pane is gone, with one notice each, and the panes still tagged whose
/// program ended. True when it killed a pane or posted a notice: the caller then lists the panes again.
///
/// - A tagged pane `#{pane_dead}` (kept by remain-on-exit) of one of our agents is killed by `kill_if_dead` under
///   the agent's notice lock, noticed or not, and a committed agent without a notice is then noticed as below.
/// - A committed agent without a notice whose pane a fresh probe shows gone: `exited` with launcher.json's code,
///   else `lost`.
/// - An uncommitted agent without a notice, created over 60 s ago, likewise, but only under the session's spawn
///   lock (taken here unless `spawn_lock_held`, as inside spawn) and then its notice lock, and only while it is
///   still uncommitted. Lock order: spawn, then notice.
///
/// `tagged` is a list-panes snapshot the caller already has; without one, panes are listed only when some agent
/// could need a notice or its launcher recorded an exit (its pane may be dead). A pid is never signalled.
pub fn reap(
    ctx: &Ctx,
    caller: &Caller,
    tagged: Option<&[Pane]>,
    spawn_lock_held: bool,
) -> Res<bool> {
    let agents = super::server_dir(ctx, caller).join("agents");
    let entries = match std::fs::read_dir(&agents) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    let (mut known, mut committed, mut uncommitted) = (Vec::new(), Vec::new(), Vec::new());
    let mut exited = false;
    for entry in entries {
        let entry = entry?;
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let folder = entry.path();
        if !super::agent_id(&id) || !folder.is_dir() {
            continue;
        }
        let noticed = folder.join("notice.json").exists();
        if folder.join("commit.json").is_file() {
            if !noticed {
                committed.push(id.clone());
            }
        } else if !noticed && stale(ctx, caller, &id) {
            uncommitted.push(id.clone());
        }
        exited |= crate::state::fs::read_json(&folder.join("launcher.json"))
            .ok()
            .flatten()
            .and_then(|l| l.get("exitCode").filter(|c| !c.is_null()).cloned())
            .is_some();
        known.push(id);
    }
    if tagged.is_none() && committed.is_empty() && uncommitted.is_empty() && !exited {
        return Ok(false);
    }
    committed.sort();
    uncommitted.sort();
    let listed;
    let live = match tagged {
        Some(panes) => panes,
        None => {
            listed = self::tagged(ctx, caller)?;
            &listed[..]
        }
    };
    let mut acted = false;
    for pane in live.iter().filter(|p| p.dead && known.contains(&p.id)) {
        let Some(record) = own_record(ctx, caller, &pane.id)? else {
            continue;
        };
        super::lock(ctx, &super::lock_key("notice", caller, &pane.id), || {
            let kill = kill_if_dead(&pane.pane, &pane.id);
            let argv: Vec<&str> = kill.iter().map(String::as_str).collect();
            crate::mux::native(ctx, &argv, true)?;
            if agents.join(&pane.id).join("commit.json").is_file() {
                post_if_gone(ctx, caller, &pane.id, &record)?;
            }
            Ok(())
        })?;
        acted = true;
    }
    for id in committed {
        if live.iter().any(|p| p.id == id) {
            continue;
        }
        let Some(record) = own_record(ctx, caller, &id)? else {
            continue;
        };
        // The snapshot may predate a spawn that committed since: the fresh probe decides.
        acted |= super::lock(ctx, &super::lock_key("notice", caller, &id), || {
            post_if_gone(ctx, caller, &id, &record)
        })?;
    }
    // A dead pane killed above is probed afresh; a live one is a launcher still waiting or running.
    uncommitted.retain(|id| !live.iter().any(|p| &p.id == id && !p.dead));
    if !uncommitted.is_empty() {
        let close = || -> Res<bool> {
            let mut posted = false;
            for id in &uncommitted {
                let Some(record) = own_record(ctx, caller, id)? else {
                    continue;
                };
                posted |= super::lock(ctx, &super::lock_key("notice", caller, id), || {
                    // A commit that landed while reap waited for the locks makes it a live agent's.
                    if agents.join(id).join("commit.json").exists() {
                        return Ok(false);
                    }
                    post_if_gone(ctx, caller, id, &record)
                })?;
            }
            Ok(posted)
        };
        // Locks are not reentrant: inside spawn, which holds the spawn lock, it is never taken again.
        acted |= if spawn_lock_held {
            close()?
        } else {
            super::lock(
                ctx,
                &super::lock_key("spawn", caller, &caller.session),
                close,
            )?
        };
    }
    Ok(acted)
}

/// record.json of agent `id` when it names that id and the caller's session.
fn own_record(ctx: &Ctx, caller: &Caller, id: &str) -> Res<Option<Value>> {
    Ok(record(ctx, caller, id)?.filter(|record| {
        record.get("id").and_then(Value::as_str) == Some(id)
            && record.get("session").and_then(Value::as_str) == Some(caller.session.as_str())
    }))
}

/// Whether agent `id`'s record was created over 60 s ago; an unreadable record or time is not.
fn stale(ctx: &Ctx, caller: &Caller, id: &str) -> bool {
    record(ctx, caller, id)
        .ok()
        .flatten()
        .and_then(|r| {
            r.get("createdAt")
                .and_then(Value::as_str)
                .and_then(crate::util::parse_utc)
        })
        .and_then(|created| ctx.clock.now().duration_since(created).ok())
        .is_some_and(|age| age > UNCOMMITTED_MAX)
}

/// Whether `pane` is gone: only exit 0 and empty fields prove it; a live pane or a failed probe is no proof.
pub(crate) fn pane_gone(ctx: &Ctx, pane: &str) -> Res<bool> {
    let probe = crate::mux::native(
        ctx,
        &["display-message", "-p", "-t", pane, GONE_PROBE],
        true,
    )?;
    Ok(probe.code == 0 && probe.stdout.trim_end_matches(['\r', '\n']) == "|")
}

/// Under agent `id`'s notice lock: when it has no notice and a fresh probe shows its pane gone, posts `exited` with
/// launcher.json's code, else `lost`. True when it posted.
pub(crate) fn post_if_gone(ctx: &Ctx, caller: &Caller, id: &str, record: &Value) -> Res<bool> {
    let folder = super::server_dir(ctx, caller).join("agents").join(id);
    if folder.join("notice.json").exists() || !pane_gone(ctx, &super::text(record.get("pane")))? {
        return Ok(false);
    }
    let launcher = crate::state::fs::read_json(&folder.join("launcher.json"))?;
    let notice = match launcher.as_ref().and_then(|l| l.get("exitCode")) {
        Some(code) if !code.is_null() => Notice::Exited(code.clone()),
        _ => Notice::Lost,
    };
    add_notice(ctx, caller, id, record, notice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::fixture::{ID, World, list_call, listed, write};
    use crate::agents::mail::read_mail;
    use crate::agents::{lock_key, text};
    use serde_json::json;

    fn pane(pane: &str, window: &str, pid: &str, id: &str, name: &str) -> Pane {
        Pane {
            pane: pane.into(),
            window: window.into(),
            pid: pid.into(),
            id: id.into(),
            name: name.into(),
            dead: false,
        }
    }

    #[test]
    fn panes_filter_12hex_and_commit_unless_all() {
        // Nothing is made while listing.
        let w = World::human("");
        let caller = w.caller();
        w.panes(&["%2|@1|300|0123456789ab|claude-1"]);
        assert!(get_panes(&w.ctx, &caller, false).unwrap().is_empty());
        assert!(!w.ctx.state_root.exists());

        w.agent_record("0123456789ab", json!({}));
        w.uncommitted("abcdef012345", json!({}));
        // A folder named commit.json does not commit its agent.
        let odd = w.uncommitted("fedcba987654", json!({}));
        std::fs::create_dir(odd.join("commit.json")).unwrap();
        let rows = [
            "%2|@1|300|0123456789ab|claude-1",
            "%3|@1|310|abcdef012345|claude-2",
            "%4|@2|320||",
            "%5|@2|330|0123456789AB|claude-3",
            "%6|@2|340|fedcba987654|claude-4",
            "%7|@2|350|0123456789abc|long",
            "%8|@3|360|0123456789ab|a|b",
            "garbage",
        ];
        w.fakes.mux_script("list-panes", move |argv| {
            Ok(crate::mux::Output {
                code: 0,
                stdout: listed(argv, &rows, "\r\n"),
                stderr: String::new(),
            })
        });
        w.fakes.clear_log();
        assert_eq!(
            get_panes(&w.ctx, &caller, false).unwrap(),
            [
                pane("%2", "@1", "300", "0123456789ab", "claude-1"),
                pane("%8", "@3", "360", "0123456789ab", "a|b")
            ]
        );
        let calls = w.fakes.mux_calls();
        assert_eq!(calls.len(), 1);
        assert!(list_call(&calls[0]), "{calls:?}");
        let all: Vec<String> = get_panes(&w.ctx, &caller, true)
            .unwrap()
            .into_iter()
            .map(|p| p.pane)
            .collect();
        assert_eq!(all, ["%2", "%3", "%6", "%8"]);
        // A failed list refuses.
        w.fakes.mux_first("list-panes", 1, "", "SECRET");
        let refused = get_panes(&w.ctx, &caller, true).unwrap_err();
        assert_eq!((refused.code, refused.check), (1, "rmux"));
    }

    #[test]
    fn panes_rows_strict_no_injected_row() {
        let w = World::human("");
        let caller = w.caller();
        w.panes(&[
            // Pane pid 0 or 1, or no number: stop would walk from the kernel's or init's tree.
            "%2|@1|0|0123456789ab|zero",
            "%3|@1|1|abcdef012345|one",
            "%4|@1|00|aaaaaaaaaaaa|zeros",
            "%4|@1|x3|aaaaaaaaaaaa|nan",
            "%4|@1|99999999999|aaaaaaaaaaaa|huge",
            "%4|@1||aaaaaaaaaaaa|empty",
            // A pane or window that is not one.
            "4|@1|300|bbbbbbbbbbbb|nopct",
            "%5|1|300|cccccccccccc|noat",
            "%5 |@1|300|cccccccccccc|space",
            "%6|@1|300|dddddddddddd|ok",
            // A name holding LF and a whole forged row, valid fields and all, adds no row; the pane itself stays,
            // its name whole.
            "%7|@1|310|eeeeeeeeeeee|evil\n%8|@5|320|ffffffffffff|forged",
            "%7|@1|310|eeeeeeeeeeee|evil\r\n%9|@5|0|bbbbbbbbbbbb|worker",
            "%8|@1|320|ffffffffffff|cr\rname",
        ]);
        let all: Vec<String> = get_panes(&w.ctx, &caller, true)
            .unwrap()
            .into_iter()
            .map(|p| format!("{} {} {}", p.pane, p.pid, p.name))
            .collect();
        assert_eq!(
            all,
            [
                "%6 300 ok",
                "%7 310 evil\n%8|@5|320|ffffffffffff|forged",
                "%7 310 evil\r\n%9|@5|0|bbbbbbbbbbbb|worker",
                "%8 320 cr\rname"
            ]
        );
        let calls = w.fakes.mux_calls();
        assert!(list_call(calls.last().unwrap()), "{calls:?}");
        // Each list frames its rows with a fresh marker, so no stored name can know it.
        get_panes(&w.ctx, &caller, true).unwrap();
        let calls = w.fakes.mux_calls();
        let n = calls.len();
        assert_ne!(calls[n - 1][6], calls[n - 2][6]);
    }

    fn refusal<T: std::fmt::Debug>(result: Res<T>) -> String {
        let r = result.unwrap_err();
        format!("{} {} {}", r.code, r.check, r.message)
    }

    #[test]
    fn find_pane_named_committed_live() {
        let w = World::human("");
        let caller = w.caller();
        w.agent_record(ID, json!({}));
        w.uncommitted("bbbbbbbbbbbb", json!({"name": "idle"}));
        w.panes(&[
            "%9|@5|300|aaaaaaaaaaaa|worker",
            "%8|@6|301|bbbbbbbbbbbb|idle",
        ]);
        assert_eq!(
            find_pane(&w.ctx, &caller, "worker").unwrap(),
            pane("%9", "@5", "300", ID, "worker")
        );
        let none = "2 target no live agent has that name";
        w.fakes.clear_log();
        // A malformed name refuses before any multiplexer call.
        for name in ["Worker", "1x", "", "a_b", &"a".repeat(25)] {
            assert_eq!(refusal(find_pane(&w.ctx, &caller, name)), none, "{name}");
        }
        assert!(w.fakes.mux_calls().is_empty());
        for name in ["idle", "nobody", "worke"] {
            assert_eq!(refusal(find_pane(&w.ctx, &caller, name)), none, "{name}");
        }
    }

    #[test]
    fn self_of_record_or_root() {
        let w = World::human("");
        let me = self_of(&w.ctx, &w.caller()).unwrap();
        assert_eq!(
            (me.name.as_str(), me.key.as_str(), me.record),
            ("root", "pane-1", None)
        );

        let w = World::agent(ID);
        let caller = w.caller();
        assert_eq!(
            refusal(self_of(&w.ctx, &caller)),
            "2 parent this agent pane has no record"
        );
        w.uncommitted(ID, json!({"name": "me"}));
        let me = self_of(&w.ctx, &caller).unwrap();
        assert_eq!((me.name.as_str(), me.key.as_str()), ("me", ID));
        assert_eq!(me.record.unwrap()["name"], "me");
        // A record naming another id is not this pane's.
        w.uncommitted(ID, json!({"id": "bbbbbbbbbbbb"}));
        assert_eq!(
            refusal(self_of(&w.ctx, &caller)),
            "2 parent this agent pane has no record"
        );
        // A tag that is not an id has no record.
        let w = World::human("tag-7");
        assert_eq!(
            refusal(self_of(&w.ctx, &w.caller())),
            "2 parent this agent pane has no record"
        );
    }

    #[test]
    fn notice_once_message_before_marker() {
        let w = World::human("");
        let caller = w.caller();
        let folder = w.agent_record(ID, json!({}));
        let record = record(&w.ctx, &caller, ID).unwrap().unwrap();
        let marker = folder.join("notice.json");
        let seen = marker.clone();
        w.fakes
            .lock_probe(move |_| format!("marker={}", seen.exists()));
        assert!(add_notice(&w.ctx, &caller, ID, &record, Notice::Stopped).unwrap());
        let mail = lock_key("mail", &caller, "pane-1");
        assert_eq!(
            w.fakes.locks(),
            [
                format!("+{mail} marker=false"),
                format!("-{mail} marker=false")
            ]
        );
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            r#"{"kind":"stopped","exitCode":null,"at":"1970-01-01T00:00:00.0000000Z"}"#
        );
        // Posted once: the second call takes no lock and adds nothing.
        w.fakes.clear_log();
        assert!(!add_notice(&w.ctx, &caller, ID, &record, Notice::Lost).unwrap());
        assert!(w.fakes.locks().is_empty());
        // A marker lost after the message went out: the retry finds the message id and posts nothing new.
        std::fs::remove_file(&marker).unwrap();
        assert!(add_notice(&w.ctx, &caller, ID, &record, Notice::Stopped).unwrap());
        let messages = read_mail(&w.ctx, &caller, "pane-1").unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(
            (
                text(messages[0].get("id")),
                text(messages[0].get("from")),
                text(messages[0].get("pane")),
                text(messages[0].get("text"))
            ),
            (
                "exit-aaaaaaaaaaaa".into(),
                "worker".into(),
                "%9".into(),
                "[mux-agent from worker (%9) #exit-aaaaaaaaaaaa]\nworker was stopped.".into()
            )
        );
    }

    #[test]
    fn notice_failed_delivery_writes_no_marker() {
        let w = World::human("");
        let caller = w.caller();
        let folder = w.agent_record(ID, json!({}));
        let record = record(&w.ctx, &caller, ID).unwrap().unwrap();
        // A folder where the parent's mailbox goes: the append cannot happen.
        let mailbox = w.state().join("mail").join("pane-1.jsonl");
        std::fs::create_dir(&mailbox).unwrap();
        assert!(add_notice(&w.ctx, &caller, ID, &record, Notice::Stopped).is_err());
        assert!(!folder.join("notice.json").exists());
        // Once the mailbox can take it, the retry posts it.
        std::fs::remove_dir(&mailbox).unwrap();
        assert!(add_notice(&w.ctx, &caller, ID, &record, Notice::Stopped).unwrap());
        assert!(folder.join("notice.json").is_file());
        assert_eq!(read_mail(&w.ctx, &caller, "pane-1").unwrap().len(), 1);
    }

    // notice.json as (kind, exitCode, closedBy).
    fn marker_of(folder: &std::path::Path) -> (Value, Value, Value) {
        let marker = crate::state::fs::read_json(&folder.join("notice.json"))
            .unwrap()
            .unwrap();
        let field = |name: &str| marker.get(name).cloned().unwrap_or(Value::Null);
        (field("kind"), field("exitCode"), field("closedBy"))
    }

    #[test]
    fn notice_message_carries_kind_and_lost_is_closed_by_pane() {
        for (notice, kind, code, closed) in [
            (Notice::Exited(json!(3)), "exited", json!(3), Value::Null),
            (Notice::Stopped, "stopped", Value::Null, Value::Null),
            (Notice::Lost, "lost", Value::Null, json!("pane")),
        ] {
            let w = World::human("");
            let caller = w.caller();
            let folder = w.agent_record(ID, json!({}));
            let record = record(&w.ctx, &caller, ID).unwrap().unwrap();
            assert!(add_notice(&w.ctx, &caller, ID, &record, notice).unwrap());
            let mailbox = w.state().join("mail").join("pane-1.jsonl");
            let stored: Value =
                serde_json::from_str(std::fs::read_to_string(mailbox).unwrap().trim_end()).unwrap();
            assert_eq!(
                (&stored["kind"], &stored["exitCode"]),
                (&json!(kind), &code)
            );
            assert_eq!(marker_of(&folder), (json!(kind), code, closed));
        }
    }

    #[test]
    fn notice_follows_the_message_already_delivered() {
        let gone = "worker is gone: its pane closed without an exit record.";
        // (the stored message's body and fields, the notice this post tries, the notice.json it must write)
        let cases: Vec<(&str, Option<Value>, Notice, (Value, Value, Value))> = vec![
            // A message with kind and exitCode decides, whatever this post says.
            (
                "worker was stopped.",
                Some(json!({"kind": "stopped", "exitCode": null})),
                Notice::Exited(json!(0)),
                (json!("stopped"), Value::Null, Value::Null),
            ),
            (
                "worker exited with code 3.",
                Some(json!({"kind": "exited", "exitCode": 3})),
                Notice::Lost,
                (json!("exited"), json!(3), Value::Null),
            ),
            (
                gone,
                Some(json!({"kind": "lost", "exitCode": null})),
                Notice::Exited(json!(0)),
                (json!("lost"), Value::Null, json!("pane")),
            ),
            // A 0.2.1 message (body only) is read from its fixed body.
            (
                "worker was stopped.",
                None,
                Notice::Exited(json!(0)),
                (json!("stopped"), Value::Null, Value::Null),
            ),
            (
                "worker exited with code 3.",
                None,
                Notice::Stopped,
                (json!("exited"), json!(3), Value::Null),
            ),
            (
                "worker exited with code -1073741510.",
                None,
                Notice::Stopped,
                (json!("exited"), json!(-1073741510), Value::Null),
            ),
            (
                gone,
                None,
                Notice::Exited(json!(0)),
                (json!("lost"), Value::Null, json!("pane")),
            ),
            // Unparseable: lost, and the message stays the only one.
            (
                "something else",
                None,
                Notice::Exited(json!(0)),
                (json!("lost"), Value::Null, json!("pane")),
            ),
            (
                "worker exited with code x.",
                None,
                Notice::Stopped,
                (json!("lost"), Value::Null, json!("pane")),
            ),
            (
                "worker was stopped.",
                Some(json!({"kind": "paused"})),
                Notice::Exited(json!(0)),
                (json!("stopped"), Value::Null, Value::Null),
            ),
        ];
        for (body, fields, notice, want) in cases {
            let w = World::human("");
            let caller = w.caller();
            let folder = w.agent_record(ID, json!({}));
            let record = record(&w.ctx, &caller, ID).unwrap().unwrap();
            let id = format!("exit-{ID}");
            match &fields {
                Some(fields) => {
                    crate::agents::mail::add_once(
                        &w.ctx,
                        &caller,
                        "pane-1",
                        "worker",
                        "%9",
                        body,
                        &id,
                        fields.as_object().unwrap(),
                    )
                    .unwrap();
                }
                None => {
                    crate::agents::mail::add_mail(
                        &w.ctx,
                        &caller,
                        "pane-1",
                        "worker",
                        "%9",
                        body,
                        Some(&id),
                    )
                    .unwrap();
                }
            }
            assert!(add_notice(&w.ctx, &caller, ID, &record, notice).unwrap());
            assert_eq!(marker_of(&folder), want, "{body} {fields:?}");
            let messages = read_mail(&w.ctx, &caller, "pane-1").unwrap();
            assert_eq!(messages.len(), 1, "{body}");
            assert_eq!(text(messages[0].get("text")).split('\n').nth(1), Some(body));
        }
    }

    #[test]
    fn reap_needs_gone_probe() {
        // No agents folder: nothing listed, nothing made.
        let w = World::human("");
        let caller = w.caller();
        w.fakes.clear_log();
        reap(&w.ctx, &caller, None, false).unwrap();
        assert!(w.fakes.mux_calls().is_empty());
        assert!(!w.ctx.state_root.exists());

        let folder = w.agent_record(ID, json!({}));
        let marker = folder.join("notice.json");
        // The snapshot misses the agent, but its pane answers live: no notice.
        w.panes(&["%1|@1|100||"]);
        let gone = "#{pane_id}|#{@agent-id}";
        w.fakes.mux_first(gone, 0, "%9|aaaaaaaaaaaa\n", "");
        w.fakes.clear_log();
        reap(&w.ctx, &caller, None, false).unwrap();
        assert!(!marker.exists());
        let calls = w.fakes.mux_calls();
        assert_eq!(calls.len(), 2);
        assert!(list_call(&calls[0]), "{calls:?}");
        assert_eq!(calls[1], ["-N", "display-message", "-p", "-t", "%9", gone]);
        let notice = lock_key("notice", &caller, ID);
        let mail = lock_key("mail", &caller, "pane-1");
        assert_eq!(
            w.fakes.locks(),
            [format!("+{notice}"), format!("-{notice}")]
        );
        // A failed or odd probe is no proof.
        for (code, out) in [(1, "|\n"), (0, "|x\n"), (0, ""), (0, " |\n")] {
            w.fakes.mux_first(gone, code, out, "");
            reap(&w.ctx, &caller, None, false).unwrap();
            assert!(!marker.exists(), "{code} {out:?}");
        }
        // Gone: exit 0 and empty fields.
        w.fakes.mux_first(gone, 0, "|\r\n", "");
        w.fakes.clear_log();
        reap(&w.ctx, &caller, None, false).unwrap();
        assert!(marker.exists());
        assert_eq!(
            w.fakes.locks(),
            [
                format!("+{notice}"),
                format!("+{mail}"),
                format!("-{mail}"),
                format!("-{notice}")
            ]
        );
        // Posted: no candidate left, so no list, probe or lock.
        w.fakes.clear_log();
        reap(&w.ctx, &caller, None, false).unwrap();
        assert!(w.fakes.mux_calls().is_empty() && w.fakes.locks().is_empty());

        // Live in the snapshot, of another session, uncommitted, or without a record: never probed.
        w.agent_record("bbbbbbbbbbbb", json!({}));
        w.agent_record("cccccccccccc", json!({"session": "$4"}));
        w.uncommitted("dddddddddddd", json!({}));
        let bare = w.folder("eeeeeeeeeeee");
        std::fs::create_dir(&bare).unwrap();
        write(&bare.join("commit.json"), json!({}));
        let snapshot = [pane("%7", "@2", "310", "bbbbbbbbbbbb", "live")];
        w.fakes.clear_log();
        reap(&w.ctx, &caller, Some(&snapshot), false).unwrap();
        assert!(w.fakes.mux_calls().is_empty());
        assert!(w.fakes.locks().is_empty());
    }

    #[test]
    fn notice_lost_vs_exited_from_launcher_json() {
        let lost = "worker is gone: its pane closed without an exit record.";
        for (launcher, kind, body, code) in [
            (None, "lost", lost, Value::Null),
            (Some(json!({"pid": 1})), "lost", lost, Value::Null),
            (
                Some(json!({"pid": 1, "exitCode": null})),
                "lost",
                lost,
                Value::Null,
            ),
            (Some(json!(5)), "lost", lost, Value::Null),
            (
                Some(json!({"pid": 1, "exitCode": 7})),
                "exited",
                "worker exited with code 7.",
                json!(7),
            ),
            (
                Some(json!({"pid": 1, "exitCode": 0})),
                "exited",
                "worker exited with code 0.",
                json!(0),
            ),
            (
                Some(json!({"pid": 1, "exitCode": -1073741510})),
                "exited",
                "worker exited with code -1073741510.",
                json!(-1073741510),
            ),
        ] {
            let w = World::human("");
            let caller = w.caller();
            let folder = w.agent_record(ID, json!({}));
            if let Some(launcher) = &launcher {
                write(&folder.join("launcher.json"), launcher.clone());
            }
            w.panes(&[]);
            w.fakes.mux_first("#{pane_id}|#{@agent-id}", 0, "|\n", "");
            reap(&w.ctx, &caller, None, false).unwrap();
            let marker = crate::state::fs::read_json(&folder.join("notice.json"))
                .unwrap()
                .unwrap();
            assert_eq!(
                (&marker["kind"], &marker["exitCode"]),
                (&json!(kind), &code)
            );
            let messages = read_mail(&w.ctx, &caller, "pane-1").unwrap();
            assert_eq!(messages.len(), 1, "{launcher:?}");
            let text = text(messages[0].get("text"));
            assert_eq!(text.split('\n').nth(1), Some(body), "{launcher:?}");
        }
    }

    fn dead(pane: &str, id: &str) -> Pane {
        Pane {
            dead: true,
            ..self::pane(pane, "@5", "300", id, "worker")
        }
    }

    // The mailbox lines as stored, without moving the read offset.
    fn stored(w: &World) -> Vec<Value> {
        let path = w.state().join("mail").join("pane-1.jsonl");
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    const KILL_IF_DEAD: [&str; 7] = [
        "-N",
        "if-shell",
        "-F",
        "-t",
        "%9",
        "#{&&:#{pane_dead},#{==:#{@agent-id},aaaaaaaaaaaa}}",
        "kill-pane -t %9",
    ];

    #[test]
    fn tagged_reads_pane_dead() {
        let w = World::human("");
        let caller = w.caller();
        w.panes_dead(
            &[
                "%9|@5|300|aaaaaaaaaaaa|worker",
                "%8|@5|301|bbbbbbbbbbbb|idle",
            ],
            &["%9"],
        );
        let got: Vec<(String, bool)> = tagged(&w.ctx, &caller)
            .unwrap()
            .into_iter()
            .map(|p| (p.pane, p.dead))
            .collect();
        assert_eq!(got, [("%9".into(), true), ("%8".into(), false)]);
        assert_eq!(
            kill_if_dead("%9", ID),
            KILL_IF_DEAD[1..]
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn reap_kills_a_dead_pane_by_one_conditional_then_notices() {
        let w = World::human("");
        let caller = w.caller();
        let folder = w.agent_record(ID, json!({}));
        write(
            &folder.join("launcher.json"),
            json!({"pid": 300, "exitCode": 4}),
        );
        w.panes_dead(&["%9|@5|300|aaaaaaaaaaaa|worker"], &["%9"]);
        // Once the conditional ran, the pane answers gone.
        w.fakes.mux_rule("#{pane_id}|#{@agent-id}", 0, "|\n", "");
        w.fakes.clear_log();
        assert!(reap(&w.ctx, &caller, None, false).unwrap());
        let calls = w.fakes.mux_calls();
        assert!(list_call(&calls[0]), "{calls:?}");
        assert_eq!(calls[1], KILL_IF_DEAD);
        assert_eq!(
            calls[2],
            ["-N", "display-message", "-p", "-t", "%9", GONE_PROBE]
        );
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert_eq!(marker_of(&folder).0, "exited");
        assert_eq!(marker_of(&folder).1, 4);
        assert_eq!(stored(&w).len(), 1);
        let (notice, mail) = (
            lock_key("notice", &caller, ID),
            lock_key("mail", &caller, "pane-1"),
        );
        assert_eq!(
            w.fakes.locks(),
            [
                format!("+{notice}"),
                format!("+{mail}"),
                format!("-{mail}"),
                format!("-{notice}")
            ]
        );
        // Never a pid signal: the pane goes by the multiplexer only.
        assert!(!w.fakes.os_calls().iter().any(|c| c.starts_with("kill ")));

        // Already noticed (the launcher posted before it exited): killed again, no probe, no second message.
        w.fakes.clear_log();
        assert!(reap(&w.ctx, &caller, Some(&[dead("%9", ID)]), false).unwrap());
        assert_eq!(w.fakes.mux_calls(), [KILL_IF_DEAD]);
        assert_eq!(stored(&w).len(), 1);
        assert_eq!(
            w.fakes.locks(),
            [format!("+{notice}"), format!("-{notice}")]
        );

        // A noticed agent whose launcher recorded an exit is reaped without a snapshot too.
        w.fakes.clear_log();
        assert!(reap(&w.ctx, &caller, None, false).unwrap());
        let calls = w.fakes.mux_calls();
        assert!(
            list_call(&calls[0]) && calls[1] == KILL_IF_DEAD,
            "{calls:?}"
        );
    }

    #[test]
    fn reap_never_kills_a_pane_not_its_own() {
        let w = World::human("");
        let caller = w.caller();
        // Another session's record, and a tag without a folder: never killed, never locked.
        w.agent_record("bbbbbbbbbbbb", json!({"session": "$4"}));
        let snapshot = [dead("%7", "bbbbbbbbbbbb"), dead("%6", "cccccccccccc")];
        w.fakes.clear_log();
        assert!(!reap(&w.ctx, &caller, Some(&snapshot), false).unwrap());
        assert!(w.fakes.mux_calls().is_empty());
        assert!(w.fakes.locks().is_empty());
    }

    // A multiplexer that keeps pane %9 as (dead, @agent-id) and evaluates the conditional as it would.
    fn emulated(
        w: &World,
        dead: bool,
        tag: &str,
    ) -> std::rc::Rc<std::cell::RefCell<Option<(bool, String)>>> {
        let pane = std::rc::Rc::new(std::cell::RefCell::new(Some((dead, tag.to_string()))));
        let state = pane.clone();
        w.fakes.mux_script("if-shell", move |argv| {
            let condition = &argv[5];
            let mut held = state.borrow_mut();
            let holds = held.as_ref().is_some_and(|(dead, tag)| {
                *dead
                    && *condition == format!("#{{&&:#{{pane_dead}},#{{==:#{{@agent-id}},{tag}}}}}")
            });
            if holds && argv[6] == "kill-pane -t %9" {
                *held = None;
            }
            Ok(crate::mux::Output {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });
        let state = pane.clone();
        w.fakes.mux_script("#{pane_id}|#{@agent-id}", move |_| {
            let stdout = match state.borrow().as_ref() {
                Some((_, tag)) => format!("%9|{tag}\n"),
                None => "|\n".into(),
            };
            Ok(crate::mux::Output {
                code: 0,
                stdout,
                stderr: String::new(),
            })
        });
        pane
    }

    #[test]
    fn reap_conditional_keeps_a_retagged_or_live_pane() {
        // The snapshot saw %9 dead and ours; by the conditional it was retagged, or respawned live: kept.
        for (dead_now, tag, kept) in [
            (true, "bbbbbbbbbbbb", true),
            (false, ID, true),
            (true, ID, false),
        ] {
            let w = World::human("");
            let caller = w.caller();
            let folder = w.agent_record(ID, json!({}));
            let pane = emulated(&w, dead_now, tag);
            reap(&w.ctx, &caller, Some(&[dead("%9", ID)]), false).unwrap();
            assert_eq!(pane.borrow().is_some(), kept, "{dead_now} {tag}");
            assert_eq!(
                folder.join("notice.json").exists(),
                !kept,
                "{dead_now} {tag}"
            );
            assert_eq!(stored(&w).len(), usize::from(!kept), "{dead_now} {tag}");
        }
    }

    const NOW: &str = "2026-10-29T12:00:00.0000000Z";

    #[test]
    fn reap_uncommitted_after_60s_under_spawn_then_notice() {
        let (old, edge, young) = (
            "2026-10-29T11:58:59.0000000Z",
            "2026-10-29T11:59:00.0000000Z",
            "2026-10-29T11:59:30.0000000Z",
        );
        // (createdAt, launcher.json, notice.json already, the marker kind and code after, a message posted)
        let cases: Vec<(&str, Option<Value>, bool, Option<(&str, Value)>)> = vec![
            (old, None, false, Some(("lost", Value::Null))),
            (
                old,
                Some(json!({"pid": 300, "aborted": true, "exitCode": 1})),
                false,
                Some(("exited", json!(1))),
            ),
            (old, Some(json!({"pid": 300, "exitCode": 1})), true, None),
            (edge, None, false, None),
            (young, None, false, None),
        ];
        for (created, launcher, noticed, want) in cases {
            let w = World::human("");
            w.clock(NOW);
            let caller = w.caller();
            let folder = w.uncommitted(ID, json!({"createdAt": created}));
            if let Some(launcher) = &launcher {
                write(&folder.join("launcher.json"), launcher.clone());
            }
            if noticed {
                write(
                    &folder.join("notice.json"),
                    json!({"kind": "exited", "exitCode": 1, "at": "x"}),
                );
            }
            w.panes(&[]);
            w.fakes.mux_rule("#{pane_id}|#{@agent-id}", 0, "|\n", "");
            w.fakes.clear_log();
            let acted = reap(&w.ctx, &caller, None, false).unwrap();
            let label = format!("{created} {launcher:?} {noticed}");
            assert_eq!(acted, want.is_some(), "{label}");
            match &want {
                Some((kind, code)) => {
                    let (k, c, closed) = marker_of(&folder);
                    assert_eq!((k, c), (json!(kind), code.clone()), "{label}");
                    assert_eq!(closed == json!("pane"), *kind == "lost", "{label}");
                    assert_eq!(stored(&w).len(), 1, "{label}");
                    let spawn = lock_key("spawn", &caller, "$3");
                    let notice = lock_key("notice", &caller, ID);
                    let mail = lock_key("mail", &caller, "pane-1");
                    assert_eq!(
                        w.fakes.locks(),
                        [
                            format!("+{spawn}"),
                            format!("+{notice}"),
                            format!("+{mail}"),
                            format!("-{mail}"),
                            format!("-{notice}"),
                            format!("-{spawn}")
                        ],
                        "{label}"
                    );
                }
                None => {
                    assert!(stored(&w).is_empty(), "{label}");
                    assert!(w.fakes.locks().is_empty(), "{label}");
                    assert_eq!(folder.join("notice.json").exists(), noticed, "{label}");
                }
            }
        }
    }

    #[test]
    fn reap_uncommitted_untouched_once_committed_or_live() {
        let old = json!({"createdAt": "2026-10-29T11:58:00.0000000Z"});
        // commit.json lands while reap waits for the spawn lock: untouched.
        let w = World::human("");
        w.clock(NOW);
        let caller = w.caller();
        let folder = w.uncommitted(ID, old.clone());
        w.panes(&[]);
        w.fakes.mux_rule("#{pane_id}|#{@agent-id}", 0, "|\n", "");
        let spawn = lock_key("spawn", &caller, "$3");
        let commit = folder.join("commit.json");
        w.fakes.lock_probe(move |event| {
            if event == format!("+{spawn}") {
                write(&commit, json!({"committedAt": "x"}));
            }
            String::new()
        });
        reap(&w.ctx, &caller, None, false).unwrap();
        assert!(!folder.join("notice.json").exists());
        assert!(stored(&w).is_empty());
        // Its pane answers live: untouched.
        let w = World::human("");
        w.clock(NOW);
        let caller = w.caller();
        let folder = w.uncommitted(ID, old.clone());
        w.panes(&[]);
        w.fakes
            .mux_rule("#{pane_id}|#{@agent-id}", 0, "%9|aaaaaaaaaaaa\n", "");
        assert!(!reap(&w.ctx, &caller, None, false).unwrap());
        assert!(!folder.join("notice.json").exists());
        // Inside spawn's own lock, reap never takes it again.
        let w = World::human("");
        w.clock(NOW);
        let caller = w.caller();
        let folder = w.uncommitted(ID, old);
        w.panes(&[]);
        w.fakes.mux_rule("#{pane_id}|#{@agent-id}", 0, "|\n", "");
        let spawn = lock_key("spawn", &caller, "$3");
        w.fakes.clear_log();
        crate::agents::lock(&w.ctx, &spawn, || reap(&w.ctx, &caller, None, true)).unwrap();
        assert!(folder.join("notice.json").exists());
        let notice = lock_key("notice", &caller, ID);
        assert_eq!(
            w.fakes.locks()[..2],
            [format!("+{spawn}"), format!("+{notice}")]
        );
        assert_eq!(
            w.fakes
                .locks()
                .iter()
                .filter(|l| l.contains("spawn|"))
                .count(),
            2
        );
    }
}
