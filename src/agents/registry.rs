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
}

const LIST_FORMAT: &str = "#{pane_id}|#{window_id}|#{pane_pid}|#{@agent-id}|#{@agent-name}";

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
            let mut parts = fields.splitn(5, '|');
            let (pane, window, pid, id, name) = (
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

/// Posts `lost` (or `exited` with launcher.json's code) for each committed agent of the caller's session whose
/// pane is gone and that has no notice yet. `tagged` is a list-panes snapshot the caller already has; without
/// one, panes are listed only when some agent could need a notice.
pub fn update_notices(ctx: &Ctx, caller: &Caller, tagged: Option<&[Pane]>) -> Res<()> {
    let agents = super::server_dir(ctx, caller).join("agents");
    let entries = match std::fs::read_dir(&agents) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let folder = entry.path();
        if super::agent_id(&id)
            && folder.is_dir()
            && folder.join("commit.json").is_file()
            && !folder.join("notice.json").exists()
        {
            candidates.push(id);
        }
    }
    if candidates.is_empty() {
        return Ok(());
    }
    candidates.sort();
    let listed;
    let live = match tagged {
        Some(panes) => panes,
        None => {
            listed = self::tagged(ctx, caller)?;
            &listed[..]
        }
    };
    for id in candidates {
        if live.iter().any(|p| p.id == id) {
            continue;
        }
        let Some(record) = record(ctx, caller, &id)? else {
            continue;
        };
        if record.get("session").and_then(Value::as_str) != Some(caller.session.as_str()) {
            continue;
        }
        super::lock(ctx, &super::lock_key("notice", caller, &id), || {
            // The snapshot may predate a spawn that committed since: only the agent's own pane answering with
            // exit 0 and empty fields is gone; a live pane or a failed probe waits for the next try.
            let pane = super::text(record.get("pane"));
            let probe = crate::mux::native(
                ctx,
                &["display-message", "-p", "-t", &pane, GONE_PROBE],
                true,
            )?;
            if probe.code != 0 || probe.stdout.trim_end_matches(['\r', '\n']) != "|" {
                return Ok(());
            }
            let launcher = crate::state::fs::read_json(&agents.join(&id).join("launcher.json"))?;
            let notice = match launcher.as_ref().and_then(|l| l.get("exitCode")) {
                Some(code) if !code.is_null() => Notice::Exited(code.clone()),
                _ => Notice::Lost,
            };
            add_notice(ctx, caller, &id, &record, notice).map(|_| ())
        })?;
    }
    Ok(())
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
    fn update_notices_needs_gone_probe() {
        // No agents folder: nothing listed, nothing made.
        let w = World::human("");
        let caller = w.caller();
        w.fakes.clear_log();
        update_notices(&w.ctx, &caller, None).unwrap();
        assert!(w.fakes.mux_calls().is_empty());
        assert!(!w.ctx.state_root.exists());

        let folder = w.agent_record(ID, json!({}));
        let marker = folder.join("notice.json");
        // The snapshot misses the agent, but its pane answers live: no notice.
        w.panes(&["%1|@1|100||"]);
        let gone = "#{pane_id}|#{@agent-id}";
        w.fakes.mux_first(gone, 0, "%9|aaaaaaaaaaaa\n", "");
        w.fakes.clear_log();
        update_notices(&w.ctx, &caller, None).unwrap();
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
            update_notices(&w.ctx, &caller, None).unwrap();
            assert!(!marker.exists(), "{code} {out:?}");
        }
        // Gone: exit 0 and empty fields.
        w.fakes.mux_first(gone, 0, "|\r\n", "");
        w.fakes.clear_log();
        update_notices(&w.ctx, &caller, None).unwrap();
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
        update_notices(&w.ctx, &caller, None).unwrap();
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
        update_notices(&w.ctx, &caller, Some(&snapshot)).unwrap();
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
            update_notices(&w.ctx, &caller, None).unwrap();
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
}
