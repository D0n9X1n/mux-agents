//! notebook-7b V1: the visible notebook block of every launch's first-turn text.
use serde_json::{Value, json};

use super::Author;
use crate::agents::team_state::{Scope, Tx};
use crate::caller::Caller;
use crate::ctx::Ctx;

/// The block's first line, alone when the notebook is empty or its certainty cannot be established.
pub const HEAD: &str = "Team notebook: shared notes of this team; read them with mux-agent notes.";
/// The block's bound in UTF-8 bytes, its line ends included.
pub const INDEX_MAX: usize = 1024;

fn older(n: usize) -> String {
    format!("+{n} older notes: run mux-agent notes")
}

/// `entries` (oldest first) as the block: HEAD, then `- <title> (<author>)` newest first while the block, with the
/// `+N` line for whatever is left out, stays within INDEX_MAX bytes.
pub fn render(entries: &[Value]) -> String {
    let mut block = HEAD.to_string();
    for (shown, e) in entries.iter().rev().enumerate() {
        let author = e.get("author").and_then(|a| a.get("name"));
        let line = format!(
            "\n- {} ({})",
            crate::agents::text(e.get("title")),
            crate::agents::text(author)
        );
        let left = entries.len() - shown - 1;
        let tail = if left == 0 { 0 } else { 1 + older(left).len() };
        if block.len() + line.len() + tail > INDEX_MAX {
            return format!("{block}\n{}", older(entries.len() - shown));
        }
        block.push_str(&line);
    }
    block
}

/// The block for the notebook of `scope`, read by a caller that already holds its team lock (the spawn lock). Only
/// the certain prefix is listed; a notebook whose certainty cannot be established is HEAD alone; any other failure
/// is HEAD and the check. Never an error: a launch never fails on its notebook.
pub fn index(ctx: &Ctx, scope: &Scope) -> String {
    let tx = Tx {
        ctx,
        scope: scope.clone(),
    };
    match super::entries(&tx) {
        Ok(entries) => render(&entries),
        Err(e) if e.check == "notes" => HEAD.to_string(),
        Err(e) => format!("{HEAD}\n(notebook unreadable: {})", e.check),
    }
}

/// V2: after a note publishes, one mail to each other current member of the team (`team_mail::roster`, read under the
/// team lock `note` holds), id `note-<entryId>`, so a retried note posts no second copy and finishes any it missed.
/// V3: a failed post is named in the errors and never skips the others; the note stands either way. Answers the
/// names notified and `[{name, check}]`.
pub fn notify(
    ctx: &Ctx,
    caller: &Caller,
    author: &Author,
    entry: &Value,
) -> (Vec<String>, Vec<Value>) {
    let text = |k: &str| crate::agents::text(entry.get(k));
    let id = text("id");
    let body = format!(
        "new note {id} from {}: {} — run mux-agent notes {id}",
        author.name,
        text("title")
    );
    let (mut notified, mut errors) = (Vec::new(), Vec::new());
    let failed = |e: &crate::error::Refusal| json!({"name": "team", "check": e.check});
    // A team with no hc member has no one to notify (as `send` decides team or 0.2).
    let members = match crate::agents::membership::locals(ctx, caller) {
        Ok(l) => l.iter().any(|l| l.stored.record.get("hcAddress").is_some()),
        Err(e) => return (notified, vec![failed(&e)]),
    };
    if !members {
        return (notified, errors);
    }
    let snapshot = match crate::agents::membership::Snapshot::take(ctx, caller) {
        Ok(s) => s,
        Err(e) => return (notified, vec![failed(&e)]),
    };
    if let Some(e) = &snapshot.core_error {
        return (notified, vec![failed(e)]);
    }
    let message_id = format!("note-{id}");
    for m in crate::agents::team_mail::roster(&snapshot) {
        let key = m.mailbox();
        if key == author.key {
            continue;
        }
        let posted = crate::agents::mail::add_mail(
            ctx,
            caller,
            &key,
            &author.name,
            &caller.pane,
            &body,
            Some(&message_id),
        );
        match posted {
            Ok(_) => notified.push(m.name),
            Err(e) => errors.push(json!({"name": m.name, "check": e.check})),
        }
    }
    (notified, errors)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod notice_tests;
