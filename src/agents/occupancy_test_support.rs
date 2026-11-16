//! Fixtures the occupancy test modules share.
use super::*;
pub(super) use crate::agents::fixture::World;
pub(super) use crate::parse::Client;
pub(super) use crate::proc::Row;
pub(super) use crate::state::root::OsKind;

// S is the fixture's SESSION_ID; X, A, B are other conversations.
pub(super) const S: &str = "0f11fc2f-2032-48ef-a920-4af2a8e5af50";
pub(super) const X: &str = "11111111-2222-4333-8444-555555555555";
pub(super) const A: &str = "22222222-2222-4222-8222-222222222222";

/// (the rule that held, the holding pid) — PM call 2: every refusal names both.
pub(super) type Who = (Rule, Option<u32>);

pub(super) fn who(h: Option<Hold>) -> Option<Who> {
    h.map(|h| (h.rule, h.pid))
}

/// No exempt client; Task 4b adds the caller's (`Own`).
pub(super) fn held(w: &World, s: &str, kind: Client) -> Option<Who> {
    who(held_on(&w.ctx, s, kind, OsKind::Unix, None))
}

/// Row 7001 under pid 1 with these fields.
pub(super) fn row(
    start: Option<i64>,
    path: Option<&str>,
    argv: Option<&[&str]>,
    image: Option<&str>,
) -> Row {
    Row {
        pid: 7001,
        ppid: 1,
        start,
        path: path.map(String::from),
        argv: argv.map(|a| a.iter().map(|s| s.to_string()).collect()),
        image: image.map(String::from),
    }
}

pub(super) use crate::agents::gens;
pub(super) use crate::proc::Boot;
pub(super) use crate::state::evidence::publish;
pub(super) use serde_json::{Value, json};
pub(super) use std::path::{Path, PathBuf};

pub(super) const Y: &str = "bbbbbbbbbbbb";
pub(super) const B: &str = "33333333-3333-4333-8333-333333333333";
/// Mixed-case ids (hex letters), so lowercasing is observable (finding 13).
pub(super) const M: &str = "ABCDEF01-2222-4333-8444-555555555555";
pub(super) const N: &str = "AbCdEf02-2222-4333-8444-555555555555";

/// Lifecycle member Y of `client` at generation 1 (record `sessionId` S), its generation committed, its spec naming
/// `spec`. Nothing proves its client yet.
pub(super) fn life(w: &World, client: &str, spec: &str) -> PathBuf {
    let f = w.uncommitted(
        Y,
        json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd", "client": client, "sessionId": S}),
    );
    publish(&gens::path(&f, 1, "commit"), &json!({"committed": true})).unwrap();
    publish(&gens::path(&f, 1, "spec"), &json!({"sessionId": spec})).unwrap();
    f
}

/// Generation `n` recorded launcher 31337 (start 1000, `startedAt` the fake clock's now, as the launcher writes it)
/// and client 7100 (start 1500), both listed, the client a Claude whose argv names S.
pub(super) fn launched(w: &World, f: &Path, n: u64) {
    let at = crate::util::utc_o(w.ctx.clock.now());
    publish(
        &gens::path(f, n, "launcher"),
        &json!({"pid": 31337, "start": 1000, "startedAt": at}),
    )
    .unwrap();
    publish(
        &gens::path(f, n, "client"),
        &json!({"pid": 7100, "start": 1500}),
    )
    .unwrap();
    w.fakes.process(crate::testing::row(31337, 1, 1000));
    w.fakes.process(Row {
        pid: 7100,
        ppid: 31337,
        start: Some(1500),
        path: Some("/usr/bin/claude".into()),
        argv: Some(vec!["claude".into(), "--session-id".into(), S.into()]),
        image: Some("claude".into()),
    });
    w.fakes.client(7100, Client::Claude);
}

pub(super) fn torn(p: &Path) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, b"{\"se").unwrap();
}

pub(super) fn plant(p: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

/// (rule, pid, image, member) of a hold.
pub(super) type Named = (Rule, Option<u32>, Option<String>, Option<String>);

pub(super) fn named(h: Option<Hold>) -> Option<Named> {
    h.map(|h| (h.rule, h.pid, h.image, h.member))
}

/// The hold every query gets from managed client 7100 of member Y whose report cannot be used (finding 8).
pub(super) fn client_evidence() -> Option<Named> {
    Some((
        Rule::Evidence,
        Some(7100),
        Some("claude".into()),
        Some(Y.into()),
    ))
}

pub(super) use crate::agents::fixture::{CLAUDE, ID, PANE};

/// `held` with an exempt client.
pub(super) fn held_as(w: &World, s: &str, kind: Client, os: OsKind, own: &Own) -> Option<Who> {
    who(held_on(&w.ctx, s, kind, os, Some(own)))
}

/// `World::agent`'s pane caller, proven agent 150 (start 1500), no standing.
pub(super) fn pane_own(w: &World) -> Own {
    own_of(&w.ctx, &w.caller(), None).unwrap()
}

/// Rows `node` 7100 (`loader_start`) and its native child `copilot` 7101 (start 2101), both Copilot clients.
pub(super) fn copilot_pair_rows(w: &World, loader_start: Option<i64>) {
    let proc_row = |pid: u32, ppid: u32, start: Option<i64>, file: &str| Row {
        pid,
        ppid,
        start,
        path: Some(format!("/usr/lib/{file}")),
        argv: Some(vec![file.into()]),
        image: Some(file.into()),
    };
    w.fakes.process(proc_row(7100, 1, loader_start, "node"));
    w.fakes.process(proc_row(7101, 7100, Some(2101), "copilot"));
    w.fakes.client(7100, Client::Copilot);
    w.fakes.client(7101, Client::Copilot);
}
