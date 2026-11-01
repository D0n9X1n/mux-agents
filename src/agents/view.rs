//! list, focus and status (MuxAgents.ps1:1271-1285, 1335-1352).

use serde_json::{Map, Value, json};

use super::board::board;
use super::mail::{doing_line, mailbox_key};
use super::registry::{self, find_pane};
use super::{FILES_ONLY, lock, lock_key, state_dir, usage};
use crate::caller::{Kind, resolve};
use crate::cli::Request;
use crate::ctx::Ctx;
use crate::error::{Res, refuse};
use crate::mux::native;
use crate::state::fs::{BODY_MAX, read_text, write_json};

/// The board as the caller's pane sees it: unread from its own mailbox (an agent's id, else `pane-<n>`).
pub fn list(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let caller = resolve(ctx)?;
    let reader = mailbox_key(&caller.pane, &caller.agent_tag)?;
    board(ctx, &caller, &reader, request.switch("All"), false)
}

/// `list -Text`: the rows as `json::to_text` prints them, without the `color` column. With `color`, a live agent's
/// name is painted in its identity hue (as the side bar paints it) and an ended one's dim; the name is cleaned first.
pub fn list_text(reply: &Map<String, Value>, color: bool) -> String {
    use crate::paint::{Style, hue, identity, paint};
    let mut shown = reply.clone();
    if let Some(Value::Array(rows)) = shown.get_mut("agents") {
        for row in rows.iter_mut().filter_map(Value::as_object_mut) {
            let recorded = row.remove("color");
            if !color {
                continue;
            }
            let name = crate::paint::clean(&super::text(row.get("name")));
            let style = if row.get("state").and_then(Value::as_str) == Some("running") {
                Style {
                    color: hue(identity(recorded.as_ref().and_then(Value::as_str), &name)),
                    ..Style::default()
                }
            } else {
                Style {
                    dim: true,
                    ..Style::default()
                }
            };
            row.insert("name".into(), paint(&name, style, true).into());
        }
    }
    crate::json::to_text(&shown)
}

pub fn focus(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let caller = resolve(ctx)?;
    let pane = find_pane(ctx, &caller, first(request))?;
    native(ctx, &["select-window", "-t", &pane.window], false)?;
    native(ctx, &["select-pane", "-t", &pane.pane], false)?;
    let mut reply = Map::new();
    reply.insert("name".into(), pane.name.into());
    reply.insert("pane".into(), pane.pane.into());
    reply.insert("window".into(), pane.window.into());
    Ok(reply)
}

/// The positional the verb table requires.
pub(crate) fn first(request: &Request) -> &str {
    request.positional.first().map_or("", String::as_str)
}

/// An agent's one-line status in status.json, written under its agent lock. Only an agent pane with its own
/// record in this session reports status, and only from a file.
pub fn status(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let body_file = request.value("BodyFile");
    if request.value("Body").is_some() == body_file.is_some() {
        return Err(usage("status"));
    }
    let caller = resolve(ctx)?;
    let tag = caller.agent_tag.as_str();
    let record = match caller.kind {
        Kind::Agent if !tag.is_empty() => registry::record(ctx, &caller, tag)?,
        _ => None,
    };
    let Some(record) = record.filter(|r| {
        r.get("id").and_then(Value::as_str) == Some(tag)
            && r.get("session").and_then(Value::as_str) == Some(caller.session.as_str())
    }) else {
        return refuse(2, "target", "only an agent reports status");
    };
    let Some(body_file) = body_file else {
        return refuse(2, "usage", FILES_ONLY);
    };
    let text = doing_line(&read_text(&ctx.cwd.join(body_file), BODY_MAX)?);
    if text.is_empty() {
        return refuse(2, "usage", "the status is empty");
    }
    let file = state_dir(ctx, &caller)?
        .join("agents")
        .join(tag)
        .join("status.json");
    let at = crate::util::utc_o(ctx.clock.now());
    lock(ctx, &lock_key("agent", &caller, tag), || {
        write_json(&file, &json!({"text": text, "at": at}))?;
        // A status is work: the agent no longer waits.
        if let Some(folder) = file.parent() {
            super::message::clear_waiting(folder);
        }
        Ok(())
    })?;
    let mut reply = Map::new();
    reply.insert(
        "name".into(),
        record.get("name").cloned().unwrap_or(Value::Null),
    );
    reply.insert("text".into(), text.into());
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::fixture::{ID, World, write};
    use crate::agents::lock_key;
    use crate::agents::mail::add_mail;
    use crate::ctx::Env;
    use serde_json::json;

    type Handler = fn(&Ctx, &Request) -> Res<Map<String, Value>>;

    fn run(w: &World, handler: Handler, args: &[&str]) -> String {
        match handler(&w.ctx, &w.request(args)) {
            Ok(reply) => format!("ok {}", Value::Object(reply)),
            Err(r) => format!("{} {} {}", r.code, r.check, r.message),
        }
    }

    #[test]
    fn focus_selects_window_then_pane() {
        let w = World::human("");
        w.agent_record(ID, json!({}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        w.fakes.clear_log();
        assert_eq!(
            run(&w, focus, &["focus", "worker"]),
            r#"ok {"name":"worker","pane":"%9","window":"@5"}"#
        );
        let calls = w.fakes.mux_calls();
        assert_eq!(
            calls[2..],
            [
                vec!["-N", "select-window", "-t", "@5"],
                vec!["-N", "select-pane", "-t", "%9"]
            ]
        );
        assert_eq!(calls[1][1], "list-panes");
        // A failed select-window stops before the pane.
        w.fakes.mux_first("select-window", 1, "", "SECRET");
        w.fakes.clear_log();
        assert_eq!(
            run(&w, focus, &["focus", "worker"]),
            "1 rmux rmux call failed"
        );
        assert!(!w.fakes.mux_calls().iter().any(|c| c[1] == "select-pane"));
        assert_eq!(
            run(&w, focus, &["focus", "nobody"]),
            "2 target no live agent has that name"
        );
    }

    #[test]
    fn status_only_agent_from_file() {
        let target = "2 target only an agent reports status";
        let missing = ["status", "-BodyFile", "missing.md"];
        // A human pane with a tag and a record.
        let w = World::human(ID);
        w.agent_record(ID, json!({"pane": "%1"}));
        assert_eq!(run(&w, status, &missing), target);
        assert_eq!(run(&w, status, &["status", "-Body", "x"]), target);
        // An agent pane with no tag, a tag without a record, a record of another session or of another id.
        assert_eq!(run(&World::agent(""), status, &missing), target);
        assert_eq!(run(&World::agent("bbbbbbbbbbbb"), status, &missing), target);
        let w = World::agent(ID);
        w.agent_record(ID, json!({"session": "$4"}));
        assert_eq!(run(&w, status, &missing), target);
        w.agent_record(ID, json!({"id": "bbbbbbbbbbbb"}));
        assert_eq!(run(&w, status, &missing), target);

        let w = World::agent(ID);
        let folder = w.agent_record(ID, json!({"pane": "%1"}));
        let usage = "2 usage usage: mux-agent status (-Body <text> | -BodyFile <path>) [-Text]";
        w.file("blank.md", " \t\r\n\u{9b}\u{7}\n".as_bytes());
        w.fakes.clear_log();
        // Two bodies or none refuse before the caller is resolved.
        assert_eq!(
            run(
                &w,
                status,
                &["status", "-Body", "x", "-BodyFile", "blank.md"]
            ),
            usage
        );
        assert_eq!(run(&w, status, &["status"]), usage);
        assert!(w.fakes.mux_calls().is_empty());
        assert_eq!(
            run(&w, status, &["status", "-Body", "x"]),
            "2 usage agents pass free text only in files: use -BodyFile"
        );
        assert_eq!(
            run(&w, status, &["status", "-BodyFile", "blank.md"]),
            "2 usage the status is empty"
        );
        assert_eq!(
            run(&w, status, &missing),
            "2 usage a task or brief file cannot be read"
        );
        assert!(!folder.join("status.json").exists());

        // The cleaned first line, cut to 80, written while the agent lock is held.
        w.clock("2026-10-29T12:00:00.0000000Z");
        let body = format!(
            "\r\n  parse\u{1b}\u{9b}\tthe config {}\r\nnext",
            "z".repeat(100)
        );
        w.file("s.md", body.as_bytes());
        let file = folder.join("status.json");
        let seen = file.clone();
        w.fakes
            .lock_probe(move |_| format!("exists={}", seen.exists()));
        w.fakes.clear_log();
        let text = format!("parse the config {}", "z".repeat(63));
        assert_eq!(
            run(&w, status, &["status", "-BodyFile", "s.md"]),
            format!(r#"ok {{"name":"worker","text":"{text}"}}"#)
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            format!(r#"{{"text":"{text}","at":"2026-10-29T12:00:00.0000000Z"}}"#)
        );
        let agent = lock_key("agent", &w.caller(), ID);
        assert_eq!(
            w.fakes.locks(),
            [
                format!("+{agent} exists=false"),
                format!("-{agent} exists=true")
            ]
        );
    }

    #[test]
    fn list_reader_counts_own_mailbox() {
        let mut w = World::human("");
        let caller = w.caller();
        w.agent_record(ID, json!({}));
        let ended = w.agent_record("bbbbbbbbbbbb", json!({"name": "done"}));
        write(
            &ended.join("notice.json"),
            json!({"kind": "stopped", "exitCode": null, "at": "x"}),
        );
        for (key, n) in [("pane-1", 1), ("pane-2", 2), (ID, 3)] {
            for _ in 0..n {
                add_mail(&w.ctx, &caller, key, "worker", "%9", "x", None).unwrap();
            }
        }
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        let rows = |w: &World, args: &[&str]| -> Vec<Value> {
            let reply = list(&w.ctx, &w.request(args)).unwrap();
            reply["agents"].as_array().unwrap().clone()
        };
        assert_eq!(rows(&w, &["list"])[0]["unread"], 1);
        assert_eq!(rows(&w, &["list"]).len(), 1);
        assert_eq!(rows(&w, &["list", "-All"]).len(), 2);
        // Another human pane reads pane-2; a tagged pane reads its id.
        let tmux = w.ctx.env.get("TMUX").unwrap().to_string();
        w.ctx.env = Env::from_pairs([("TMUX", tmux), ("TMUX_PANE", "%2".into())]);
        assert_eq!(rows(&w, &["list"])[0]["unread"], 2);
        w.fakes.mux_first(
            "#{pid}|#{pane_pid}|#{session_id}|#{window_id}|#{@agent-id}",
            0,
            "900|100|$3|@1|aaaaaaaaaaaa\n",
            "",
        );
        assert_eq!(rows(&w, &["list"])[0]["unread"], 3);
        // Listing writes no offset.
        let offsets = std::fs::read_dir(w.state().join("mail"))
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|x| x == "offset")
            })
            .count();
        assert_eq!(offsets, 0);
    }
}
