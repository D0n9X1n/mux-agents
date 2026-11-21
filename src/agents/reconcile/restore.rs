//! restore-6b row 4 (L3, L4, L5): gate, release generation n for restore, then occupancy and n+1 in the same Tx.
use super::super::gens::Gen;
use super::super::team_state::Tx;
use super::super::text;
use super::Seen;
use crate::error::{Refusal, Res};
use crate::hc::{Core, Lazy};
use serde_json::{Value, json};

/// What a restore attempt came to.
pub(super) enum Done {
    Restored(u64),
    Held(String, Option<String>),
}

/// L4 checks that need no release (ambiguity 5a): not closed (3d C2), every older generation done. Desired
/// `stopped` is row 1, decided from the one `desired.json` read in `observe` (6b-R1), so it is not reread here.
fn gate(tx: &Tx<'_>, id: &str, g: &Gen) -> Res<Option<String>> {
    match super::super::cascade::admit(tx, id) {
        Ok(()) => {}
        Err(e) if e.check == "closed" => return Ok(Some("closed".into())),
        Err(e) => return Err(e),
    }
    if !g.orphans()?.is_empty() {
        return Ok(Some("older".into()));
    }
    Ok(None)
}

/// The conversation to resume: generation n's valid `launch.json` `sessionId`, read once by `observe` (release-1d
/// R3; r6b call 15). None refuses `evidence` (Unknown) before anything is released.
pub(super) fn session(seen: &Seen) -> Res<String> {
    seen.session
        .clone()
        .ok_or_else(crate::state::evidence::unreadable)
}

fn client_of(g: &Gen) -> Res<crate::parse::Client> {
    super::super::spawn::client_named(&text(g.record().get("client")))
        .ok_or_else(|| Refusal::internal("the member names no known client"))
}

/// What a next generation launches (launch-1 G1 kinds): restore and retry resume `session`; fresh starts it.
pub(crate) struct Next<'a> {
    pub kind: &'static str,
    pub session: &'a str,
    pub fresh: bool,
}

/// L3, restore-6c P2: n+1 of member `id` through launch-1 G1: its stored options within the team ceiling, the
/// resume recipe, no prompt, no task (ambiguity 13, 6c-R4). One spec builder for restore and recovery.
pub(crate) fn launch_next(
    tx: &Tx<'_>,
    core: &Core<'_>,
    id: &str,
    g: &Gen,
    next: Next<'_>,
) -> Res<u64> {
    launch_next_viewed(tx, core, id, g, next, |_, _| Ok(json!({}))).map(|(n, _)| n)
}

/// `launch_next` whose launch opens a view with `attach` after the promotion, as spawn's does (G1.6): the generation
/// and the promoted record, which carries the view's fields, or `viewError` when it did not open.
pub(crate) fn launch_next_viewed(
    tx: &Tx<'_>,
    core: &Core<'_>,
    id: &str,
    g: &Gen,
    next: Next<'_>,
    attach: impl FnOnce(&Core<'_>, &Value) -> Res<Value>,
) -> Res<(u64, Value)> {
    use super::super::spawn::{Recipe, client_args, client_name, utf8};
    let r = g.record();
    let client = client_of(g)?;
    let server = tx.ctx.state_root.join(&tx.scope.key);
    let team = team_of(tx)?;
    let stored = super::super::options::stored(r)?;
    let ceiling = super::super::resume::ceiling(Some(&team));
    let flags = super::super::options::replay(client, &stored, ceiling)?;
    // A new conversation of a role's member gets its standing brief as spawn gave it; a resumed one already has it.
    let brief = if next.fresh {
        fresh_brief(g.folder(), r)?
    } else {
        None
    };
    let recipe = if next.fresh {
        Recipe::First(brief.as_deref())
    } else {
        Recipe::Resume(None)
    };
    let spec = json!({"schema": 1, "stateDirectory": utf8(&server)?, "client": client_name(client),
        "flags": flags, "parentPane": r.get("parentPane"),
        "args": client_args(client, recipe, next.session, None)});
    let launch = super::super::core_launch::Launch {
        id: id.into(),
        kind: next.kind,
        session_id: next.session.into(),
        create: None,
        spec,
        team: format!("{}:{}", tx.scope.key, tx.scope.session),
        name: text(r.get("name")),
        up_team: r.get("upTeam").and_then(Value::as_str).map(str::to_owned),
        directory: text(r.get("directory")),
    };
    let promoted = super::super::core_launch::forward(tx, core, launch, || Ok(()), attach)?;
    let n = super::super::gens::plain_gen(&text(promoted.get("generation")))
        .ok_or_else(|| Refusal::internal("the next generation is not a number"))?;
    Ok((n, promoted))
}

/// The first turn of a new conversation of member `record` in `folder`: none without a role; for a role's member the
/// line that names its standing brief, the role.md spawn wrote (and resume copied), never the role file. A role's
/// member whose role.md is gone refuses 2 `role`: it never starts without its brief.
pub(crate) fn fresh_brief(folder: &std::path::Path, record: &Value) -> Res<Option<String>> {
    let Some(name) = record.get("role").and_then(Value::as_str) else {
        return Ok(None);
    };
    let path = folder.join("role.md");
    if !path.is_file() {
        return crate::error::refuse(
            2,
            "role",
            format!(
                "{} has no role.md, the standing brief a new conversation of its role starts from",
                text(record.get("name"))
            ),
        );
    }
    Ok(Some(super::super::role::brief_line(name, &path)))
}

/// Checklist 7, 9 (call 25): the session's team record through the evidence reader (a link, folder, FIFO or torn
/// file is `evidence`), present, whole, and naming this session. A lifecycle member never launches without it.
fn team_of(tx: &Tx<'_>) -> Res<super::super::team::Team> {
    let file = super::super::team::path(&tx.ctx.state_root.join(&tx.scope.key), &tx.scope.session);
    crate::state::evidence::read(&file)
        .known()?
        .and_then(|v| super::super::team::Team::from_json(&v))
        .filter(|t| t.session == tx.scope.session)
        .ok_or_else(crate::state::evidence::unreadable)
}

fn relaunch(tx: &Tx<'_>, core: &Core<'_>, id: &str, g: &Gen, sid: &str) -> Res<u64> {
    launch_next(
        tx,
        core,
        id,
        g,
        Next {
            kind: "restore",
            session: sid,
            fresh: false,
        },
    )
}

/// L4 occupancy, asked after n is done (ambiguity 5c), then the relaunch.
fn occupied_or_run(tx: &Tx<'_>, core: &Core<'_>, id: &str, g: &Gen, sid: &str) -> Res<Done> {
    if let Some(hold) = super::super::occupancy::held(tx.ctx, sid, client_of(g)?, None) {
        return Ok(Done::Held("occupied".into(), Some(hold.message())));
    }
    relaunch(tx, core, id, g, sid).map(Done::Restored)
}

/// Row 0 (L2): the conversation, gate, then the cap (L1, reacquired at the anchor; never for the leader), then
/// occupancy and n+1 from the released generation's conversation.
pub(super) fn restore_awaiting(
    tx: &Tx<'_>,
    lazy: &Lazy<'_>,
    anchor: &crate::caller::Caller,
    id: &str,
    g: &Gen,
    seen: &Seen,
) -> Res<Done> {
    let sid = session(seen)?; // r6b call 15: no valid conversation id, nothing launches
    if let Some(why) = gate(tx, id, g)? {
        return Ok(Done::Held(why, None));
    }
    let core = lazy.get()?;
    if !super::super::tree::is_leader(g.record()) {
        let snapshot = super::super::membership::Snapshot::for_spawn(tx.ctx, anchor, core)?;
        match super::super::tree::limit(&snapshot) {
            Ok(()) => {}
            Err(e) if e.check == "limit" => return Ok(Done::Held("limit".into(), None)),
            Err(e) => return Err(e),
        }
    }
    occupied_or_run(tx, core, id, g, &sid)
}

/// Row 4 (L3): the conversation, gate, release n (`cause: restore`, X still reserved), then occupancy and n+1 in
/// the same Tx. Nothing is released without a valid conversation id or with a hold of the gate.
pub(super) fn restore_lost(
    tx: &Tx<'_>,
    lazy: &Lazy<'_>,
    id: &str,
    g: &Gen,
    seen: &Seen,
) -> Res<Done> {
    let sid = session(seen)?;
    if let Some(why) = gate(tx, id, g)? {
        return Ok(Done::Held(why, None));
    }
    let n = g
        .active()
        .ok_or_else(|| Refusal::internal("a Lost member has no active generation"))?;
    let address = text(g.record().get("hcAddress"));
    super::super::release::release(
        tx,
        lazy,
        id,
        n,
        &address,
        super::super::release::Cause::Restore,
    )?;
    occupied_or_run(tx, lazy.get()?, id, g, &sid)
}
