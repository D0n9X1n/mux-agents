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

/// L3: n+1 through launch-1 G1, kind `restore`: the resume recipe (options-6e), no prompt, no task (ambiguity 13).
fn relaunch(tx: &Tx<'_>, core: &Core<'_>, id: &str, g: &Gen, sid: &str) -> Res<u64> {
    use super::super::spawn::{Recipe, client_args, client_name, utf8};
    let r = g.record();
    let client = client_of(g)?;
    let server = tx.ctx.state_root.join(&tx.scope.key);
    let team = super::super::team::read(&super::super::team::path(&server, &tx.scope.session))?;
    let stored = super::super::options::stored(r)?;
    let ceiling = super::super::resume::ceiling(team.as_ref());
    let flags = super::super::options::replay(client, &stored, ceiling)?;
    let spec = json!({"schema": 1, "stateDirectory": utf8(&server)?, "client": client_name(client),
        "flags": flags, "parentPane": r.get("parentPane"),
        "args": client_args(client, Recipe::Resume, sid, None)});
    let launch = super::super::core_launch::Launch {
        id: id.into(),
        kind: "restore",
        session_id: sid.into(),
        create: None,
        spec,
        team: format!("{}:{}", tx.scope.key, tx.scope.session),
        name: text(r.get("name")),
        up_team: r.get("upTeam").and_then(Value::as_str).map(str::to_owned),
        directory: text(r.get("directory")),
    };
    let promoted =
        super::super::core_launch::forward(tx, core, launch, || Ok(()), |_, _| Ok(json!({})))?;
    super::super::gens::plain_gen(&text(promoted.get("generation")))
        .ok_or_else(|| Refusal::internal("the restored generation is not a number"))
}

/// L4 occupancy, asked after n is done (ambiguity 5c), then the relaunch.
fn occupied_or_run(tx: &Tx<'_>, core: &Core<'_>, id: &str, g: &Gen, sid: &str) -> Res<Done> {
    if let Some(hold) = super::super::occupancy::held(tx.ctx, sid, client_of(g)?, None) {
        return Ok(Done::Held("occupied".into(), Some(hold.message())));
    }
    relaunch(tx, core, id, g, sid).map(Done::Restored)
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
