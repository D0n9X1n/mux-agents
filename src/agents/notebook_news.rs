//! notebook-7b V1 (#103): the visible notebook block of every launch's first-turn text.
use serde_json::Value;

use crate::agents::team_state::{Scope, Tx};
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

#[cfg(test)]
#[path = "notebook_news_tests.rs"]
mod tests;
