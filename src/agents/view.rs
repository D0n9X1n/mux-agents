//! list, focus, status (MuxAgents.ps1:1271-1285, 1335-1352) and report.
pub use super::attachment::{attach as attach_core, clean as clean_core};

use serde_json::{Map, Value, json};

use super::board::board;
use super::mail::doing_line;
use super::registry::{self, find_pane};
use super::{FILES_ONLY, lock, lock_key, state_dir, usage};
use crate::caller::{Caller, Kind, resolve};
use crate::cli::Request;
use crate::ctx::Ctx;
use crate::error::{Res, refuse};
use crate::mux::native;
use crate::state::fs::{BODY_MAX, read_json, read_text, write_json};

/// The board as the caller's pane sees it: unread from its own mailbox (a running agent's id, else `pane-<n>`).
pub fn list(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let caller = resolve(ctx)?;
    board(ctx, &caller, request.switch("All"), false)
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

/// The agent `focus_pane` selected: its name, pane and window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Focused {
    pub name: String,
    pub pane: String,
    pub window: String,
}

/// The `focus` verb: `focus_pane` on the agent its positional names, with no drawn pane to check; or, with `-Next` /
/// `-Prev`, on the live agent after (before) the one the session shows, in the board's ranked order (herdr's
/// agent.next / agent.prev), wrapping; from a pane that is no agent's, the first (last).
pub fn focus(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let (next, prev) = (request.switch("Next"), request.switch("Prev"));
    let named = !request.positional.is_empty();
    if usize::from(next) + usize::from(prev) + usize::from(named) != 1 {
        return Err(usage("focus"));
    }
    let caller = resolve(ctx)?;
    let name = if named {
        first(request).to_string()
    } else {
        step(ctx, &caller, next)?
    };
    let focused = focus_pane(ctx, &caller, &name, None)?;
    let mut reply = Map::new();
    reply.insert("name".into(), focused.name.into());
    reply.insert("pane".into(), focused.pane.into());
    reply.insert("window".into(), focused.window.into());
    Ok(reply)
}

/// The live agent next to (`forward`) or before the one on the pane the session shows, in the board's order.
fn step(ctx: &Ctx, caller: &Caller, forward: bool) -> Res<String> {
    let board = board(ctx, caller, false, false)?;
    let live: Vec<(String, String)> = board
        .get("agents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .filter(|row| row.get("state").and_then(Value::as_str) == Some("running"))
        .map(|row| (super::text(row.get("name")), super::text(row.get("pane"))))
        .collect();
    if live.is_empty() {
        return refuse(2, "target", "no live agent to switch to");
    }
    let shown = crate::sidebar::shown_pane(ctx, caller).flatten();
    let at = shown.and_then(|pane| live.iter().position(|(_, p)| *p == pane));
    let n = live.len();
    let to = match (at, forward) {
        (Some(at), true) => (at + 1) % n,
        (Some(at), false) => (at + n - 1) % n,
        (None, true) => 0,
        (None, false) => n - 1,
    };
    Ok(live[to].0.clone())
}

/// Selects a running agent's window and pane, only while its pane is still its live launch (checked by the server in
/// the same command), then reads it back: the pane must be active in the active window, live and still that launch.
/// With `expected` (the pane the side bar drew), an agent now on another pane is refused before anything is selected.
pub fn focus_pane(ctx: &Ctx, caller: &Caller, name: &str, expected: Option<&str>) -> Res<Focused> {
    let pane = find_pane(ctx, caller, name)?;
    if expected.is_some_and(|e| e != pane.pane) {
        return refuse(
            2,
            "target",
            "the agent is no longer on the pane the bar drew",
        );
    }
    select_view(ctx, &pane.name, &pane.pane, &pane.window, &pane.launch)
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
    // Only a running agent's own pane, as one snapshot shows it: never its tag.
    let me = match caller.kind {
        Kind::Agent => match registry::Snapshot::take(ctx, &caller)?.own(&caller) {
            registry::Own::Agent(me) => Some(me),
            _ => None,
        },
        Kind::Human => None,
    };
    let Some(me) = me else {
        return refuse(2, "target", "only an agent reports status");
    };
    let (tag, record) = (me.id.as_str(), &me.record);
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

/// What an agent's own client may report (herdr's OSC 7501 words; `done` is the reader's, never reported).
const REPORTED: [&str; 3] = ["working", "blocked", "idle"];
const KINDS: [&str; 2] = ["permission", "question"];

/// `report <working|blocked|idle> [-Kind <permission|question>]`: the agent's own state, as its client's hooks see it,
/// in activity.json under its agent lock. Enum words only, never free text. `since` moves only when the state or its
/// kind changes; an unreadable file counts as none.
pub fn report(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let state = first(request);
    let kind = request.value("Kind");
    let words = REPORTED.contains(&state) && kind.is_none_or(|k| KINDS.contains(&k));
    if !words || (kind.is_some() && state != "blocked") {
        return Err(usage("report"));
    }
    let caller = resolve(ctx)?;
    let me = match caller.kind {
        Kind::Agent => match registry::Snapshot::take(ctx, &caller)?.own(&caller) {
            registry::Own::Agent(me) => Some(me),
            _ => None,
        },
        Kind::Human => None,
    };
    let Some(me) = me else {
        return refuse(2, "target", "only an agent reports its activity");
    };
    let file = state_dir(ctx, &caller)?
        .join("agents")
        .join(&me.id)
        .join("activity.json");
    let now = crate::util::utc_o(ctx.clock.now());
    let kind = kind.map_or(Value::Null, Value::from);
    let since = lock(ctx, &lock_key("agent", &caller, &me.id), || {
        let old = read_json(&file).ok().flatten();
        let same = old.as_ref().is_some_and(|old| {
            old.get("state").and_then(Value::as_str) == Some(state)
                && old.get("kind") == Some(&kind)
        });
        // Kept only when it is still a time; anything else is repaired to now.
        let since = old
            .as_ref()
            .and_then(|old| old.get("since"))
            .filter(|since| same && since.as_str().and_then(crate::util::parse_utc).is_some())
            .cloned()
            .unwrap_or_else(|| now.clone().into());
        write_json(
            &file,
            &json!({"state": state, "kind": kind, "since": since}),
        )?;
        Ok(since)
    })?;
    let mut reply = Map::new();
    reply.insert(
        "name".into(),
        me.record.get("name").cloned().unwrap_or(Value::Null),
    );
    reply.insert("state".into(), state.into());
    reply.insert("kind".into(), kind);
    reply.insert("since".into(), since);
    Ok(reply)
}

fn select_view(
    ctx: &Ctx,
    name: &str,
    pane: &str,
    window: &str,
    launch: &registry::Launch,
) -> Res<Focused> {
    let select = registry::bound(
        pane,
        launch.live(),
        format!("select-window -t {pane} ; select-pane -t {pane}"),
    );
    let select: Vec<&str> = select.iter().map(String::as_str).collect();
    // Only the read-back decides: a select that failed, or a read that failed as a call, is no proof of focus.
    let _ = native(ctx, &select, true);
    let probe = format!(
        "#{{pane_id}}|#{{window_active}}|#{{pane_active}}|{}",
        launch.live()
    );
    let focused =
        native(ctx, &["display-message", "-p", "-t", pane, &probe], true).is_ok_and(|r| {
            r.code == 0 && r.stdout.trim_end_matches(['\r', '\n']) == format!("{pane}|1|1|1")
        });
    if !focused {
        return refuse(2, "target", "the agent's pane is no longer its launch");
    }
    Ok(Focused {
        name: name.into(),
        pane: pane.into(),
        window: window.into(),
    })
}
pub fn focus_hc(
    ctx: &Ctx,
    core: &crate::hc::Core<'_>,
    caller: &Caller,
    name: &str,
    expected: Option<&str>,
) -> Res<Focused> {
    let id = super::member_target::named(ctx, caller, name)?;
    lock(ctx, &lock_key("member", caller, &id.id), || {
        let record = super::member_target::current(ctx, caller, &id)?;
        let pane = id.view(&record, &registry::rows(ctx, caller)?);
        if expected.is_some_and(|e| pane.as_ref().is_none_or(|p| p.pane != e)) {
            return refuse(
                2,
                "target",
                "the agent is no longer on the pane the bar drew",
            );
        }
        match pane {
            Some(p) => select_view(ctx, &p.name, &p.pane, &p.window, &p.launch),
            None => restore_view(ctx, core, caller, &id, record),
        }
    })
}

fn restore_view(
    ctx: &Ctx,
    core: &crate::hc::Core<'_>,
    caller: &Caller,
    id: &super::core_identity::Identity,
    mut record: Value,
) -> Res<Focused> {
    let placement = super::placement::read_placement(ctx, caller)
        .ok()
        .flatten()
        .unwrap_or(super::placement::DEFAULT);
    let mut created = None;
    let result: Res<super::spawn::Created> = (|| {
        attach_core(
            ctx,
            core,
            caller,
            &record,
            (placement != "tab").then_some(placement),
            &mut created,
        )?;
        let v = created
            .as_ref()
            .ok_or_else(|| crate::error::Refusal::internal("unproven new view"))?;
        record["pane"] = v.pane.clone().into();
        record["window"] = v.window.clone().into();
        record["startCommand"] = v.command.clone().into();
        write_json(
            &super::server_dir(ctx, caller)
                .join("agents")
                .join(&id.id)
                .join("record.json"),
            &record,
        )?;
        Ok(v.clone())
    })();
    if result.is_err()
        && let Some(v) = &created
    {
        let _ = clean_core(ctx, v);
    }
    let v = result?;
    select_view(
        ctx,
        &id.name,
        &v.pane,
        &v.window,
        &registry::Launch {
            command: v.command,
            pid: None,
        },
    )
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
        w.fakes.mux_rule("#{window_active}", 0, "%9|1|1|1\n", "");
        w.fakes.clear_log();
        assert_eq!(
            run(&w, focus, &["focus", "worker"]),
            r#"ok {"name":"worker","pane":"%9","window":"@5"}"#
        );
        let live = registry::Launch {
            command: crate::agents::fixture::launch_command("%9"),
            pid: None,
        }
        .live();
        let calls = w.fakes.mux_calls();
        assert_eq!(calls[1][1], "list-panes");
        // One server-side command selects the window and the pane only while %9 is still its live launch; a read
        // then proves it active, live and that launch.
        assert_eq!(
            calls[2..],
            [
                vec![
                    "-N".to_string(),
                    "if-shell".into(),
                    "-F".into(),
                    "-t".into(),
                    "%9".into(),
                    live.clone(),
                    "select-window -t %9 ; select-pane -t %9".into()
                ],
                vec![
                    "-N".to_string(),
                    "display-message".into(),
                    "-p".into(),
                    "-t".into(),
                    "%9".into(),
                    format!("#{{pane_id}}|#{{window_active}}|#{{pane_active}}|{live}")
                ]
            ]
        );
        // Replaced between the list and the select (the condition failed: not active, or not its launch), or a
        // failed read: refused.
        for (code, out) in [
            (0, "%9|1|1|0\n"),
            (0, "%9|0|1|1\n"),
            (0, "%9|1|0|1\n"),
            (0, "|||\n"),
            (1, "%9|1|1|1\n"),
        ] {
            w.fakes.mux_first("#{window_active}", code, out, "");
            assert_eq!(
                run(&w, focus, &["focus", "worker"]),
                "2 target the agent's pane is no longer its launch",
                "{code} {out:?}"
            );
        }
        assert_eq!(
            run(&w, focus, &["focus", "nobody"]),
            "2 target no live agent has that name"
        );
        // A rotated tag names nobody: worker's pane is still %9.
        w.panes(&[
            "%9|@5|300|bbbbbbbbbbbb|other",
            "%8|@5|301|aaaaaaaaaaaa|worker",
        ]);
        w.fakes.mux_first("#{window_active}", 0, "%9|1|1|1\n", "");
        assert_eq!(
            run(&w, focus, &["focus", "worker"]),
            r#"ok {"name":"worker","pane":"%9","window":"@5"}"#
        );
        assert_eq!(
            run(&w, focus, &["focus", "other"]),
            "2 target no live agent has that name"
        );
    }

    #[test]
    fn focus_next_and_prev_step_through_the_ranked_board_and_wrap() {
        let w = World::human("");
        w.clock("2026-11-08T12:00:00.0000000Z");
        // In pane order: a works, b needs you, c is done (never seen). Ranked: b, c, a.
        for (id, name, pane, state) in [
            ("aaaaaaaaaaaa", "a", "%10", "working"),
            ("bbbbbbbbbbbb", "b", "%11", "blocked"),
            ("cccccccccccc", "c", "%12", "idle"),
        ] {
            let folder = w.agent_record(id, json!({"name": name, "pane": pane}));
            write(
                &folder.join("activity.json"),
                json!({"state": state, "kind": null, "since": "2026-11-08T11:59:00.0000000Z"}),
            );
        }
        w.panes(&[
            "%10|@5|300|aaaaaaaaaaaa|a",
            "%11|@6|301|bbbbbbbbbbbb|b",
            "%12|@7|302|cccccccccccc|c",
        ]);
        // Every select is proved by its read-back, whichever pane it was.
        w.fakes.mux_script("#{window_active}", |argv| {
            let pane = argv[argv.len() - 2].clone();
            Ok(crate::mux::Output {
                code: 0,
                stdout: format!("{pane}|1|1|1\n"),
                stderr: String::new(),
            })
        });
        let step = |shown: &str, flag: &str| {
            w.fakes.mux_first(
                "#{pane_id}|#{session_attached}",
                0,
                &format!("{shown}|1\n"),
                "",
            );
            run(&w, focus, &["focus", flag])
        };
        let name = |out: String| {
            out.split(r#""name":""#)
                .nth(1)
                .map(|r| r[..1].to_string())
                .unwrap_or(out)
        };
        // From a pane that is no agent's: -Next is the most urgent, -Prev the last.
        assert_eq!(name(step("%1", "-Next")), "b");
        assert_eq!(name(step("%1", "-Prev")), "a");
        // From an agent's pane, its neighbour in rank order, wrapping.
        assert_eq!(name(step("%11", "-Next")), "c");
        assert_eq!(name(step("%12", "-Next")), "a");
        assert_eq!(name(step("%10", "-Next")), "b");
        assert_eq!(name(step("%11", "-Prev")), "a");
        // No client attached or no answer: as from no agent's pane.
        w.fakes
            .mux_first("#{pane_id}|#{session_attached}", 0, "%11|0\n", "");
        assert_eq!(name(run(&w, focus, &["focus", "-Next"])), "b");
        // A name with a flag, both flags, or neither: usage, before the caller is resolved (as every verb's own usage).
        let usage = "2 usage usage: mux-agent focus (<name> | -Next | -Prev) [-Text]";
        w.fakes.clear_log();
        for args in [
            &["focus", "a", "-Next"][..],
            &["focus", "-Next", "-Prev"],
            &["focus"],
        ] {
            assert_eq!(run(&w, focus, args), usage, "{args:?}");
        }
        assert!(w.fakes.mux_calls().is_empty());
        // No live agent: refused.
        let w = World::human("");
        w.panes(&[]);
        w.fakes
            .mux_first("#{pane_id}|#{session_attached}", 0, "%1|1\n", "");
        assert_eq!(
            run(&w, focus, &["focus", "-Next"]),
            "2 target no live agent to switch to"
        );
    }

    #[test]
    fn focus_pane_refuses_a_moved_agent_before_selecting() {
        let w = World::human("");
        w.agent_record(ID, json!({"pane": "%10"}));
        w.panes(&["%10|@5|300|aaaaaaaaaaaa|worker"]);
        let caller = w.caller();
        // The bar drew worker on %9; it is on %10 now: refused before any select.
        w.fakes.clear_log();
        let moved = focus_pane(&w.ctx, &caller, "worker", Some("%9")).unwrap_err();
        assert_eq!(
            (moved.code, moved.check, moved.message.as_str()),
            (
                2,
                "target",
                "the agent is no longer on the pane the bar drew"
            )
        );
        let selects = |calls: Vec<Vec<String>>| {
            calls
                .iter()
                .filter(|c| c.iter().any(|arg| arg.contains("select-window -t %10")))
                .count()
        };
        // Nothing that could select a pane ran: no `if-shell`, no select of any pane.
        assert!(
            w.fakes
                .mux_calls()
                .iter()
                .all(|c| c[1] != "if-shell" && !c.iter().any(|arg| arg.contains("select-"))),
            "{:?}",
            w.fakes.mux_calls()
        );
        // Still on the pane the bar drew: it selects that pane, and the read-back proves it focused.
        w.fakes.clear_log();
        w.fakes.mux_first("#{window_active}", 0, "%10|1|1|1\n", "");
        let focused = focus_pane(&w.ctx, &caller, "worker", Some("%10")).unwrap();
        assert_eq!(selects(w.fakes.mux_calls()), 1, "{:?}", w.fakes.mux_calls());
        assert_eq!(
            (focused.name, focused.pane, focused.window),
            ("worker".into(), "%10".into(), "@5".into())
        );
        // A read-back that does not prove it focused.
        w.fakes.mux_first("#{window_active}", 0, "%10|1|0|1\n", "");
        let unproved = focus_pane(&w.ctx, &caller, "worker", Some("%10")).unwrap_err();
        assert_eq!((unproved.code, unproved.check), (2, "target"));
    }

    #[test]
    fn status_only_agent_from_file() {
        let target = "2 target only an agent reports status";
        let missing = ["status", "-BodyFile", "missing.md"];
        // A human pane with a tag and a record.
        let w = World::human(ID);
        w.agent_record(ID, json!({"pane": "%1"}));
        w.panes(&["%1|@1|100|aaaaaaaaaaaa|worker"]);
        assert_eq!(run(&w, status, &missing), target);
        assert_eq!(run(&w, status, &["status", "-Body", "x"]), target);
        // An agent pane with no tag, a tag without a record, a record of another session or of another id.
        assert_eq!(run(&World::agent(""), status, &missing), target);
        assert_eq!(run(&World::agent("bbbbbbbbbbbb"), status, &missing), target);
        let w = World::agent(ID);
        w.panes(&["%1|@1|100|aaaaaaaaaaaa|worker"]);
        w.agent_record(ID, json!({"session": "$4", "pane": "%1"}));
        assert_eq!(run(&w, status, &missing), target);
        w.agent_record(ID, json!({"id": "bbbbbbbbbbbb", "pane": "%1"}));
        assert_eq!(run(&w, status, &missing), target);
        // Tagged with the agent's id but not its pane (the tag rotated here), or its own pane not its launch.
        let w = World::agent(ID);
        w.agent_record(ID, json!({"pane": "%9"}));
        w.panes(&["%1|@1|100|aaaaaaaaaaaa|worker", "%9|@5|300||"]);
        assert_eq!(run(&w, status, &missing), target);
        let w = World::agent("");
        w.agent_record(ID, json!({"pane": "%1", "startCommand": "something else"}));
        w.panes(&["%1|@1|100||"]);
        assert_eq!(run(&w, status, &missing), target);

        // The agent's own pane, tagless (rmux moved its tag): still itself.
        let w = World::agent("");
        let folder = w.agent_record(ID, json!({"pane": "%1"}));
        w.panes(&["%1|@1|100||"]);
        w.file("s.md", b"tagless");
        assert_eq!(
            run(&w, status, &["status", "-BodyFile", "s.md"]),
            r#"ok {"name":"worker","text":"tagless"}"#
        );
        assert!(folder.join("status.json").is_file());

        let w = World::agent(ID);
        let folder = w.agent_record(ID, json!({"pane": "%1"}));
        w.panes(&["%1|@1|100|aaaaaaaaaaaa|worker"]);
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
    fn report_only_agent_enum_only() {
        let target = "2 target only an agent reports its activity";
        let usage = "2 usage usage: mux-agent report <working|blocked|idle> [-Kind <permission|question>] [-Quiet] [-Text]";
        // A human pane with a tag and a record.
        let w = World::human(ID);
        w.agent_record(ID, json!({"pane": "%1"}));
        w.panes(&["%1|@1|100|aaaaaaaaaaaa|worker"]);
        assert_eq!(run(&w, report, &["report", "idle"]), target);
        // An agent pane with no record.
        assert_eq!(run(&World::agent(""), report, &["report", "idle"]), target);

        let w = World::agent(ID);
        let folder = w.agent_record(ID, json!({"pane": "%1"}));
        w.panes(&["%1|@1|100|aaaaaaaaaaaa|worker"]);
        w.fakes.clear_log();
        // Any other word, a kind on a state that is not blocked, or another kind refuses before the caller is resolved.
        for args in [
            &["report", "done"][..],
            &["report", "Working"],
            &["report", "idle", "-Kind", "question"],
            &["report", "blocked", "-Kind", "auth"],
        ] {
            assert_eq!(run(&w, report, args), usage, "{args:?}");
        }
        assert!(w.fakes.mux_calls().is_empty());
        assert!(!folder.join("activity.json").exists());

        // Written while the agent lock is held.
        w.clock("2026-11-08T12:00:00.0000000Z");
        let file = folder.join("activity.json");
        let seen = file.clone();
        w.fakes
            .lock_probe(move |_| format!("exists={}", seen.exists()));
        w.fakes.clear_log();
        assert_eq!(
            run(&w, report, &["report", "working"]),
            r#"ok {"name":"worker","state":"working","kind":null,"since":"2026-11-08T12:00:00.0000000Z"}"#
        );
        let agent = lock_key("agent", &w.caller(), ID);
        assert_eq!(
            w.fakes.locks(),
            [
                format!("+{agent} exists=false"),
                format!("-{agent} exists=true")
            ]
        );
        // The same state again keeps its since; a change moves it; a new kind is a change.
        w.clock("2026-11-08T12:00:05.0000000Z");
        run(&w, report, &["report", "working"]);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            r#"{"state":"working","kind":null,"since":"2026-11-08T12:00:00.0000000Z"}"#
        );
        run(&w, report, &["report", "blocked", "-Kind", "question"]);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            r#"{"state":"blocked","kind":"question","since":"2026-11-08T12:00:05.0000000Z"}"#
        );
        w.clock("2026-11-08T12:00:09.0000000Z");
        run(&w, report, &["report", "blocked", "-Kind", "permission"]);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            r#"{"state":"blocked","kind":"permission","since":"2026-11-08T12:00:09.0000000Z"}"#
        );
        // A blocked report without a kind is a plain blocked.
        run(&w, report, &["report", "blocked"]);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            r#"{"state":"blocked","kind":null,"since":"2026-11-08T12:00:09.0000000Z"}"#
        );
        // A kept since that is not a time is repaired.
        std::fs::write(&file, br#"{"state":"blocked","kind":null,"since":null}"#).unwrap();
        run(&w, report, &["report", "blocked"]);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            r#"{"state":"blocked","kind":null,"since":"2026-11-08T12:00:09.0000000Z"}"#
        );
        // An unreadable file is replaced as if there were none.
        std::fs::write(&file, b"{").unwrap();
        run(&w, report, &["report", "idle"]);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            r#"{"state":"idle","kind":null,"since":"2026-11-08T12:00:09.0000000Z"}"#
        );
    }

    #[test]
    fn list_reader_counts_own_mailbox() {
        let mut w = World::human("");
        let caller = w.caller();
        w.agent_record(ID, json!({}));
        let ended = w.agent_record("bbbbbbbbbbbb", json!({"name": "done", "pane": "%7"}));
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
        // Another human pane reads pane-2, whatever tag it carries: a tag never names the reader.
        let tmux = w.ctx.env.get("TMUX").unwrap().to_string();
        w.ctx.env = Env::from_pairs([("TMUX", tmux.clone()), ("TMUX_PANE", "%2".into())]);
        assert_eq!(rows(&w, &["list"])[0]["unread"], 2);
        w.fakes.mux_first(
            "#{pid}|#{pane_pid}|#{session_id}|#{window_id}|#{@agent-id}",
            0,
            "900|100|$3|@1|aaaaaaaaaaaa\n",
            "",
        );
        assert_eq!(rows(&w, &["list"])[0]["unread"], 2);
        // The running agent's own pane reads its id, tagged or not.
        w.ctx.env = Env::from_pairs([("TMUX", tmux), ("TMUX_PANE", "%9".into())]);
        w.fakes.mux_first(
            "#{pid}|#{pane_pid}|#{session_id}|#{window_id}|#{@agent-id}",
            0,
            "900|100|$3|@1|\n",
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

    #[test]
    fn list_of_six_agents_is_one_pane_list() {
        // #48 test 10: six running agents, their tags rotated one place: one list-panes, every row by its own pane.
        let w = World::human("");
        let mut rows = Vec::new();
        for n in 0..6u32 {
            let id = format!("{:012x}", 0xe00 + n);
            let pane = format!("%{}", 30 + n);
            w.agent_record(&id, json!({"name": format!("w{n}"), "pane": pane}));
            let tag = format!("{:012x}", 0xe00 + (n + 1) % 6);
            rows.push(format!("{pane}|@5|{}|{tag}|w{}", 330 + n, (n + 1) % 6));
        }
        w.panes(&rows.iter().map(String::as_str).collect::<Vec<_>>());
        w.fakes.clear_log();
        let reply = list(&w.ctx, &w.request(&["list"])).unwrap();
        let got: Vec<String> = reply["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                format!(
                    "{}@{}",
                    r["name"].as_str().unwrap(),
                    r["pane"].as_str().unwrap()
                )
            })
            .collect();
        assert_eq!(
            got,
            ["w0@%30", "w1@%31", "w2@%32", "w3@%33", "w4@%34", "w5@%35"]
        );
        let verbs: Vec<String> = w.fakes.mux_calls().iter().map(|c| c[1].clone()).collect();
        assert_eq!(verbs, ["display-message", "list-panes"]);
    }

    #[test]
    fn focus_read_back_that_fails_to_run_is_a_target_refusal() {
        // D9: the read-back after the select fails as a call (no server, a timeout): `2 target`, never another check.
        let w = World::human("");
        w.agent_record(ID, json!({}));
        w.panes(&["%9|@5|300|aaaaaaaaaaaa|worker"]);
        w.fakes.mux_script("#{window_active}", |_| {
            Err(crate::error::Refusal {
                code: 1,
                check: "endpoint",
                message: "rmux endpoint did not answer".into(),
                reason: None,
            })
        });
        assert_eq!(
            run(&w, focus, &["focus", "worker"]),
            "2 target the agent's pane is no longer its launch"
        );
    }
}
