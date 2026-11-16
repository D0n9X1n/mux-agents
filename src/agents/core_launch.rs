use super::gens;
use super::team_state::{Guard, Tx};
use crate::{
    error::{Refusal, Res},
    hc::{Core, Run},
    state::evidence::{self, Evidence, Published},
    util::utc_o,
};
use serde_json::{Value, json};
/// One launch of a member, built by an adapter (A3b-2). `forward` owns every file it makes.
pub struct Launch {
    pub id: String,
    /// `spawn`, `resume`, `leader` (this slice); `fresh`, `restore`, `retry` come with their specs.
    pub kind: &'static str,
    /// G1.1: a new uuid for spawn and a new leader; the ended record session for resume.
    pub session_id: String,
    /// A new member record, without `generation`, `hcAddress`, `hcName`, `launcherArgv`, `team`, `sessionId`
    /// (F4 sets them together). None: a new generation of the existing record.
    pub create: Option<Value>,
    /// The spec body; G1.2 adds `id`, `generation`, `kind`, `sessionId`, `deadline`.
    pub spec: Value,
    pub team: String,
    pub name: String,
    pub up_team: Option<String>,
    pub directory: String,
}

/// outcome-2: the launch deadline of a generation.
const DEADLINE_SECS: u64 = 150;

/// F3: after each durable forward step, the test hook, then the crash point.
fn step(name: &str) {
    #[cfg(test)]
    hooks::fire(name);
    evidence::crash_point(name);
}

/// A file this call must be the first to make; an earlier one is not this launch's.
fn made(p: Res<Published>) -> Res<()> {
    match p? {
        Published::Published => Ok(()),
        Published::Exists => Err(Refusal::internal("generation evidence already exists")),
    }
}

/// Refusal 1 `launch`.
fn refused(message: &str) -> Refusal {
    Refusal {
        code: 1,
        check: "launch",
        message: message.into(),
        reason: None,
        leftover: None,
        details: Default::default(),
    }
}

/// launch-1 G1 under the team lock `tx` holds: pending, spec, launch.json, hc run, the address, the commit claim,
/// one promotion, the view. `debit` is the last step before hc run. `attach` opens the view after the promotion and
/// answers its fields; they go to the reply (the adapter writes view.json), never to record.json.
pub fn forward(
    tx: &Tx<'_>,
    core: &Core<'_>,
    l: Launch,
    debit: impl FnOnce() -> Res<()>,
    attach: impl FnOnce(&Core<'_>, &Value) -> Res<Value>,
) -> Res<Value> {
    if l.directory.is_empty() {
        return Err(Refusal::internal("the launch has no directory")); // ambiguity 10
    }
    let folder = tx.folder(&l.id);
    let creates = l.create.is_some();
    // R5: one walk read before any write. F2: the first pending write is guarded by what it saw.
    let (first, current) = match (gens::member_of(&folder), &l.create) {
        (gens::Member::Unreadable(e), _) => return Err(e),
        (gens::Member::Absent, Some(_)) => (Guard::Create, None),
        (gens::Member::Life(g), None) => {
            if g.status()?.pending_done == Some(false) {
                return Err(refused("an earlier launch of this member is undecided")); // ambiguity 1
            }
            (
                Guard::PendingGen { n: g.pending() },
                Some(g.record().clone()),
            )
        }
        (_, Some(_)) => return Err(Refusal::internal("the member already has a record")),
        (_, None) => return Err(Refusal::internal("the member has no lifecycle record")),
    };
    let n = gens::next(&folder, current.as_ref())?;
    let hc_name = format!("m{}", &crate::util::hex_id()?[..8]);
    let spec_path = gens::path(&folder, n, "spec");
    let argv = crate::launcher_scope::argv(tx.ctx, &spec_path)?;
    step("allocated");
    // G1.1: pending first, durably. A new member's record is the adapter body plus pending (`record_write` adds
    // `lifecycle` and `id`); an existing record gets only the new pending.
    let pending = json!({"gen": n, "kind": l.kind, "hcName": hc_name, "sessionId": l.session_id,
        "launcherArgv": argv});
    let base = l
        .create
        .as_ref()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let wrote = tx.record_write(&l.id, first, |r| {
        r.extend(base);
        r.insert("pending".into(), pending.clone());
        Ok(())
    });
    match wrote {
        Ok(Some(_)) => {}
        Ok(None) => {
            let mut e = refused("the member record changed before this launch was recorded");
            e.message.push_str("; nothing was fenced");
            return Err(e);
        }
        // F2: a failed flush after the rename may have landed; `abort` fences only what this call owns.
        Err(e) => return Err(abort(tx, &l.id, n, &hc_name, e)),
    }
    step("pending");
    // Every failure from here to the claim goes to `abort` (F1).
    let fence = |e: Refusal| abort(tx, &l.id, n, &hc_name, e);
    // cascade-3d C1: a new member (spawn, a resume's successor, a new leader) is desired `running` before `hc run`;
    // absent already reads `running`, so a death before this write changes nothing. A failure is fenced (F1).
    if creates {
        tx.desired(&l.id, true).map_err(&fence)?;
        step("desired");
    }
    // G1.2: the spec, then launch.json, both create-once through the one publisher (R-W1).
    let mut spec = l.spec.clone();
    spec["id"] = json!(l.id);
    spec["generation"] = json!(n.to_string());
    spec["kind"] = json!(l.kind);
    spec["sessionId"] = json!(l.session_id);
    let deadline = tx.ctx.clock.now() + std::time::Duration::from_secs(DEADLINE_SECS);
    spec["deadline"] = json!(utc_o(deadline));
    made(gens::publish_once(&folder, n, "spec", &spec)).map_err(&fence)?;
    step("spec");
    let identity = json!({"hcName": hc_name, "launcherArgv": argv, "sessionId": l.session_id});
    made(gens::publish_once(&folder, n, "launch", &identity)).map_err(&fence)?;
    step("launch");
    // G1.3: hc run starts the launcher under the new name and the generation label (G2).
    let gen_label = n.to_string();
    let mut labels = vec![
        ("mux.managed", "1"),
        ("mux.team", l.team.as_str()),
        ("mux.id", l.id.as_str()),
        ("mux.gen", gen_label.as_str()),
        ("mux.name", l.name.as_str()),
    ];
    if let Some(up) = l.up_team.as_deref() {
        labels.push(("mux.up", up));
    }
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    let input = Run {
        bg: true,
        keep: true,
        outlive_parent: true,
        no_notify: true,
        name: Some(&hc_name),
        labels: &labels,
        cwd: Some(&l.directory),
        argv: &args,
    };
    debit().map_err(&fence)?;
    let address = attempt(|| core.run(&input)).map_err(&fence)?;
    step("run");
    // G1.4: the address, written only while the record still names (n, hcName) (F1, call 15).
    let mut with_address = pending;
    with_address["hcAddress"] = json!(address);
    let own = Guard::Pending {
        n,
        hc_name: &hc_name,
    };
    let set_address = |r: &mut serde_json::Map<String, Value>| {
        r.insert("pending".into(), with_address);
        Ok(())
    };
    if tx
        .record_write(&l.id, own, set_address)
        .map_err(&fence)?
        .is_none()
    {
        return Err(fence(refused("the launch no longer owns the record")));
    }
    step("address");
    // The commit claim. A failed publication keeps pending and its address for settle-1b (ambiguity 6).
    let commit = json!({"committed": true, "at": utc_o(tx.ctx.clock.now())});
    match gens::publish_once(&folder, n, "commit", &commit)? {
        Published::Published => {}
        Published::Exists => return Err(lost(tx, &l.id, n, &hc_name)),
    }
    step("commit");
    // G1.5, F4: one write; session id and terminal identity change together, pending gone in the same write.
    // F1: only while `pending.gen` is still n. A failure is returned; nothing is rolled back (ambiguity 5).
    let record = tx
        .promote(&l.id, n, &identity, &address, &l.team, None)?
        .ok_or_else(|| refused("the launch no longer owns the record"))?;
    step("promote");
    // G1.6, F4: the view, optional; nothing is rolled back. Only the view fields reach the reply.
    let mut reply = record.clone();
    match attempt(|| attach(core, &record)) {
        Ok(view) => {
            for k in super::team_state::VIEW_FIELDS {
                if let Some(v) = view.get(k) {
                    reply[k] = v.clone();
                }
            }
        }
        Err(e) => reply["viewError"] = json!(e.message),
    }
    Ok(reply)
}

/// F1: generation n is this call's own when record.json, read once through the walk, is admitted by
/// `Guard::Pending { n, hc_name }`, the guard every later record write of this launch uses. Absent or
/// Unreadable (a link, torn bytes): not proven.
fn owns(tx: &Tx<'_>, id: &str, n: u64, hc_name: &str) -> bool {
    match gens::record(&tx.folder(id)) {
        Evidence::Present(r) => Guard::Pending { n, hc_name }.admits(id, Some(&r)),
        Evidence::Absent | Evidence::Unreadable => false,
    }
}

/// Call 15: drops `pending` only while the record still names (n, hc_name), judged by `record_write` inside the
/// member lock. Refused or failed, the refusal says so; the aborted commit stands either way (ambiguity 9).
fn drop_pending(tx: &Tx<'_>, id: &str, n: u64, hc_name: &str, e: &mut Refusal) {
    let clear = |r: &mut serde_json::Map<String, Value>| {
        r.remove("pending");
        Ok(())
    };
    match tx.record_write(id, Guard::Pending { n, hc_name }, clear) {
        Ok(Some(_)) => {}
        Ok(None) => e.message.push_str("; pending is not this launch's, kept"),
        Err(_) => e.message.push_str("; pending could not be dropped"),
    }
}

/// G1 before the claim (F1): only a generation this call still owns is fenced. Owned: commit {aborted} through the
/// one publisher, then `drop_pending`; a commit already there decides (`lost`); a failed publication (a visible but
/// unflushed abort included) keeps pending for settle-1b (ambiguity 8). Not owned: nothing is written.
fn abort(tx: &Tx<'_>, id: &str, n: u64, hc_name: &str, mut e: Refusal) -> Refusal {
    if !owns(tx, id, n, hc_name) {
        e.message.push_str("; nothing was fenced");
        return e;
    }
    let aborted = json!({"aborted": true, "at": utc_o(tx.ctx.clock.now()), "message": e.message});
    match gens::publish_once(&tx.folder(id), n, "commit", &aborted) {
        Ok(Published::Published) => {
            drop_pending(tx, id, n, hc_name, &mut e);
            e
        }
        Ok(Published::Exists) => lost(tx, id, n, hc_name),
        Err(_) => {
            e.message.push_str("; abort evidence could not be written");
            e
        }
    }
}

/// G1.4, ambiguity 7: the claim lost. Aborted (the launcher gave up): the generation is void (done); refusal
/// `launch`; pending dropped by `drop_pending`, so only while this call still owns it, judged inside the member
/// lock (F1, call 15). Run or Invalid: not this launch's; pending kept for settle-1b. Absent or Unreadable on the
/// reread: evidence, pending kept.
fn lost(tx: &Tx<'_>, id: &str, n: u64, hc_name: &str) -> Refusal {
    match gens::read(&tx.folder(id), n, "commit") {
        Evidence::Present(c) if gens::decide(&c) == gens::Decision::Aborted => {
            let mut e = refused("the launcher gave up before the commit");
            drop_pending(tx, id, n, hc_name, &mut e);
            e
        }
        Evidence::Present(_) => Refusal::internal(&format!(
            "the commit of generation {n} is not this launch's"
        )),
        Evidence::Absent | Evidence::Unreadable => evidence::unreadable(),
    }
}

pub(crate) fn attempt<T>(f: impl FnOnce() -> Res<T>) -> Res<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(Refusal::internal("launch boundary panicked")))
}

/// Test hook (F3): a callback after every forward step. `Reset` also clears every fs fault a test may arm.
#[cfg(test)]
#[path = "core_launch_hooks.rs"]
pub(crate) mod hooks;

#[cfg(test)]
#[path = "core_launch_forward_tests.rs"]
pub(crate) mod forward_tests;

/// Crash tests (launch-1 crash model). Call 28: A3b-1 guarantees convergence by a direct retry after `allocated`
/// (nothing written) and after the launcher gives up (`launch`, `run`, `address`: commit Aborted, the generation
/// void). A crash after `pending`, `spec` or an unpromoted `commit` is not converged by a retry (it is refused while
/// the generation is undecided); group B's settle (G3) replays it.
#[cfg(test)]
#[path = "core_launch_crash_tests.rs"]
pub(crate) mod crash;
