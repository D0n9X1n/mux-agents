//! restore-6c P6 (#103): a stopped team is retired before `up` makes its successor (calls 12-14, 20-22, 25; D19).
//! The marker `<key>/teams/<sha16(session)>.retired.json` binds the old scope; 6a's index is renamed under
//! `<root>/up/retired/<T>.<digits of at>.<sha16(key:session)>.json`.

use serde_json::{Value, json};

use super::index;
use crate::agents::{team_state::Scope, text};
use crate::{
    caller::Caller,
    ctx::Ctx,
    error::{Refusal, Res, refuse},
};

/// P6.4: retired only when the marker is bound to the scope and its renamed index reads back with that anchor.
#[derive(Debug)]
pub(crate) enum Retired {
    No,
    Yes,
    /// The bound marker is Present and 6a's live index still anchors the scope: step 3 is unfinished.
    Half(Value),
}

fn retiring(why: &str) -> Refusal {
    refuse::<()>(1, "retiring", why).unwrap_err()
}

pub(crate) fn marker_path(ctx: &Ctx, scope: &Scope) -> std::path::PathBuf {
    crate::agents::team::path(&ctx.state_root.join(&scope.key), &scope.session)
        .with_extension("retired.json")
}

/// Call 21: `<T>.<digits of at>.<sha16(key:session)>.json`; two retirements never share a name.
pub(crate) fn archive_of(team: &str, at: &str, scope: &Scope) -> String {
    let digits: String = at.chars().filter(char::is_ascii_digit).collect();
    let hash = crate::util::sha256_hex(format!("{}:{}", scope.key, scope.session));
    format!("{team}.{digits}.{}.json", &hash[..16])
}

/// Call 25 (10-13 F1): every field is the retirement of `scope` as team `team`, the T this `up` derived. Checked
/// before any archive or live-index evidence is read. Returns the archive name.
fn bound(body: &Value, scope: &Scope, team: &str) -> Option<String> {
    let at = body.get("at")?.as_str()?;
    let archive = body.get("archive")?.as_str()?;
    let ok = body.get("schema") == Some(&json!(1))
        && body.get("team")?.as_str()? == team
        && body.get("anchor") == Some(&json!({"key": scope.key, "session": scope.session}))
        && !at.is_empty()
        && archive == archive_of(team, at, scope);
    ok.then(|| archive.to_string())
}

fn anchored(ix: &index::Index, scope: &Scope, team: &str) -> bool {
    ix.team == team
        && ix
            .anchor
            .as_ref()
            .is_some_and(|a| a.key == scope.key && a.session == scope.session)
}

/// Whether `scope`'s team `team` is retired, half retired, or not; a marker or archive that is not bound to it
/// is 1 `retiring` (call 21). Replay re-runs the archive's folder flushes before accepting it (finding 4).
pub(crate) fn retired_state(ctx: &Ctx, scope: &Scope, team: &str) -> Res<Retired> {
    use crate::state::evidence::{Evidence, publish, read};
    let marker = marker_path(ctx, scope);
    let body = match read(&marker) {
        Evidence::Absent => return Ok(Retired::No),
        Evidence::Present(b) => b,
        Evidence::Unreadable => {
            return Err(retiring("the team's retirement marker cannot be read"));
        }
    };
    let archive = bound(&body, scope, team)
        .ok_or_else(|| retiring("the retirement marker is not this team's"))?;
    publish(&marker, &body).map_err(|_| retiring("the retirement marker cannot be flushed"))?;
    let Some(dir) = index::retired_dir(ctx)? else {
        return half(ctx, scope, team, body);
    };
    let at = dir.join(&archive);
    match std::fs::symlink_metadata(&at) {
        // 10-13 F7 and finding 4: `read_retired` refuses a link, junction or folder in the archive's place and
        // re-runs the archive's folder flushes before it is accepted (D27: one check, not two)
        Ok(_) => match index::read_retired(ctx, &at) {
            Ok(ix) if anchored(&ix, scope, team) => Ok(Retired::Yes),
            Ok(_) => Err(retiring("the retired index is not this team's")),
            Err(e) => Err(retiring(&e.message)),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => half(ctx, scope, team, body),
        Err(_) => Err(retiring("the retired index cannot be read")),
    }
}

/// The marker is bound but no archive exists: step 3 is unfinished only while 6a's live index anchors the scope.
fn half(ctx: &Ctx, scope: &Scope, team: &str, body: Value) -> Res<Retired> {
    match index::read(ctx, team) {
        Ok(Some(ix)) if anchored(&ix, scope, team) => Ok(Retired::Half(body)),
        _ => Err(retiring("the team's index is neither live nor retired")),
    }
}

/// P6 steps 1-3 under `up|T`: finish every cascade (unresolved: 1 `stopping`); publish the bound marker (halt
/// `retired`); rename 6a's index onto a destination that must not exist, then flush (halt `renamed`).
pub(crate) fn retire(ctx: &Ctx, anchor: &Caller, scope: &Scope, team: &str) -> Res<()> {
    let body = match retired_state(ctx, scope, team)? {
        Retired::Yes => return Ok(()),
        Retired::Half(body) => body,
        Retired::No => {
            crate::agents::team_state::open(ctx, scope, |tx| {
                let reports = crate::agents::cascade::finish(tx, anchor)?;
                if reports.iter().all(|r| r.stopped) {
                    Ok(())
                } else {
                    refuse(
                        1,
                        "stopping",
                        "the team's stop is unresolved; run stop again",
                    )
                }
            })?;
            let at = crate::util::utc_o(ctx.clock.now());
            let body = json!({"schema": 1, "team": team, "anchor": {"key": scope.key, "session": scope.session},
                "at": at, "archive": archive_of(team, &at, scope)});
            crate::state::evidence::publish(&marker_path(ctx, scope), &body)?;
            super::halt("retired")?;
            body
        }
    };
    let dir = index::retired_dir_made(ctx)?;
    let to = dir.join(text(body.get("archive")));
    if std::fs::symlink_metadata(&to).is_ok() {
        return Err(retiring("a retired index already has that name")); // never overwrite (a Windows rename would)
    }
    std::fs::rename(index::path(ctx, team), &to)?;
    for d in [&dir, &ctx.state_root.join("up")] {
        crate::state::fs::sync_dir(d)
            .map_err(|_| retiring("the retired index could not be flushed"))?;
    }
    super::halt("renamed")
}

/// Call 25 (10-13 F2): `live` plus the releases of every archived index of T, sorted and deduplicated. A release
/// publish is create-once, so a release carried twice publishes once. An unreadable archive refuses.
pub(crate) fn carried(ctx: &Ctx, team: &str, live: Vec<String>) -> Res<Vec<String>> {
    let mut all = live;
    for (_, ix) in index::every_retired(ctx, Some(team))? {
        all.extend(ix.releases);
    }
    all.sort();
    all.dedup();
    Ok(all)
}

/// Ambiguity 14, call 12 (restore-6c Task 11): the kept labels a team's bar lists. Its own `<key>:<session>`, then,
/// by archive name, each retired index of T's anchor, read through `index::read_retired`. An archive that does not
/// read, or a name that is not UTF-8, refuses (the board shows `keptError`, display only); never a guessed T.
pub(crate) fn kept_teams(ctx: &Ctx, scope: &Scope, team: &str) -> Res<Vec<String>> {
    let mut all = vec![format!("{}:{}", scope.key, scope.session)];
    for (_, ix) in index::every_retired(ctx, Some(team))? {
        let a = ix
            .anchor
            .ok_or_else(|| index::bad("a retired index has no anchor"))?;
        all.push(format!("{}:{}", a.key, a.session));
    }
    Ok(all)
}

/// Call 20: the stopped arm of 6a's `drive`. P6.1-3 (`retire`), then E6.1's fresh index `{team, inputs, pending}`
/// in one durable write, the pending request built as 6a builds it and the live and archived releases carried
/// (calls 14, 25); halt `reindexed`. Returns the retired `<key>:<session>`; `drive` continues the same loop.
pub(crate) fn successor(ctx: &Ctx, caller: &Caller, ix: &mut index::Index) -> Res<String> {
    let a = ix
        .anchor
        .clone()
        .ok_or_else(|| Refusal::internal("no anchor"))?;
    let scope = Scope {
        key: a.key,
        session: a.session,
    };
    let anchor = crate::agents::reconcile::anchor_caller(caller, &scope);
    retire(ctx, &anchor, &scope, &ix.team)?;
    let releases = carried(ctx, &ix.team, std::mem::take(&mut ix.releases))?;
    let mut fresh = index::Index {
        anchor: None,
        view: None,
        pending: None,
        aliases: vec![],
        releases,
        ..ix.clone()
    };
    fresh.pending = Some(super::publish::new_pending(ctx, caller, &fresh)?);
    index::write(ctx, &fresh)?;
    super::halt("reindexed")?;
    *ix = fresh;
    Ok(format!("{}:{}", scope.key, scope.session))
}
