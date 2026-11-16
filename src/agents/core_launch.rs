use super::gens;
use super::spawn::Created;
use super::team_state::{Guard, Tx};
use crate::{
    ctx::Ctx,
    error::{Refusal, Res},
    hc::{self, Core, Run},
    state::evidence::{self, Evidence, Published},
    state::fs::write_json,
    util::utc_o,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
/// Prepared input for the launch transaction the spawn adapter calls in b23-B.
pub struct Prepared {
    pub folder: PathBuf,
    pub record: Value,
    pub argv: Vec<String>,
}
fn field<'a>(v: &'a Value, k: &str) -> Res<&'a str> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| Refusal::internal("invalid prepared identity"))
}
/// The launch transaction the spawn adapter calls in b23-B.
pub fn execute(
    ctx: &Ctx,
    prepare: impl FnOnce() -> Res<Prepared>,
    attach: impl FnOnce(&Core<'_>, &Prepared, &mut Option<Created>) -> Res<()>,
    commit: impl FnOnce(&Path) -> Res<()>,
    clean: impl FnOnce(&Created) -> Res<bool>,
) -> Res<Value> {
    let core = hc::connect(ctx)?;
    execute_connected(&core, prepare, || Ok(()), attach, commit, clean)
}
pub fn execute_connected(
    core: &Core<'_>,
    prepare: impl FnOnce() -> Res<Prepared>,
    debit: impl FnOnce() -> Res<()>,
    attach: impl FnOnce(&Core<'_>, &Prepared, &mut Option<Created>) -> Res<()>,
    commit: impl FnOnce(&Path) -> Res<()>,
    clean: impl FnOnce(&Created) -> Res<bool>,
) -> Res<Value> {
    let mut p = prepare()?;
    let mut candidate = p.record.clone();
    candidate["hcAddress"] = json!("pending");
    candidate["launcherArgv"] = json!(p.argv);
    let id = field(&p.record, "id")?;
    let team = field(&p.record, "team")?;
    let spec = crate::state::fs::read_json(&p.folder.join("spec.json"))?;
    if super::core_identity::Identity::read(&candidate, id, team).is_none()
        || spec
            .as_ref()
            .and_then(|s| s.get("generation"))
            .and_then(Value::as_str)
            != Some("0")
    {
        return Err(Refusal::internal("invalid prepared identity or spec"));
    }
    let id = field(&p.record, "id")?;
    if !super::agent_id(id) || p.argv.is_empty() || field(&p.record, "generation")? != "0" {
        return Err(Refusal::internal("invalid prepared generation"));
    }
    let name = format!("m{}", &id[..8]);
    let mut labels = vec![
        ("mux.managed", "1"),
        ("mux.team", field(&p.record, "team")?),
        ("mux.id", id),
        ("mux.gen", "0"),
        ("mux.name", field(&p.record, "name")?),
    ];
    // `up`'s leader carries its team (#103).
    if let Some(up) = p.record.get("upTeam").and_then(Value::as_str) {
        labels.push(("mux.up", up));
    }
    let argv: Vec<&str> = p.argv.iter().map(String::as_str).collect();
    // The run's whole input, built and checked before the token: a refusal here ran nothing and spends nothing.
    let input = field(&p.record, "directory").and_then(|cwd| {
        let input = Run {
            bg: true,
            keep: true,
            outlive_parent: true,
            no_notify: true,
            name: Some(&name),
            labels: &labels,
            cwd: Some(cwd),
            argv: &argv,
        };
        input.check().map(|()| input)
    });
    let input = input.map_err(|e| super::spawn::abort_evidence(&p.folder, e))?;
    // A member's budget token: the last step before `hc run`, after every check of the request and of the run's input,
    // so a refusal before here spends nothing and only the run and the launch after it keep it (#103 D5).
    debit().map_err(|e| super::spawn::abort_evidence(&p.folder, e))?;
    let run = attempt(|| core.run(&input));
    let address = match run {
        Ok(a) => a,
        Err(mut e) => {
            if write_json(
                &p.folder.join("abort.json"),
                &json!({"runUncertain":true,"hcAddress":null,"message":e.message}),
            )
            .is_err()
            {
                e.message.push_str("; abort evidence could not be written");
            }
            return Err(e);
        }
    };
    p.record["hcAddress"] = address.clone().into();
    p.record["launcherArgv"] = json!(p.argv);
    let mut pane = None;
    let operation = || -> Res<Value> {
        crate::state::fs::write_json_durable(&p.folder.join("record.json"), &p.record)?;
        attach(core, &p, &mut pane)?;
        let v = pane
            .as_ref()
            .filter(|v| !v.command.is_empty())
            .ok_or_else(|| Refusal::internal("unproven new view"))?;
        p.record["pane"] = v.pane.clone().into();
        p.record["window"] = v.window.clone().into();
        p.record["startCommand"] = v.command.clone().into();
        crate::state::fs::write_json_durable(&p.folder.join("record.json"), &p.record)?;
        commit(&p.folder)?;
        Ok(p.record.clone())
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation))
        .unwrap_or_else(|_| Err(Refusal::internal("launch panicked")));
    match result {
        Ok(record) => Ok(record),
        Err(mut e) => {
            if !rollback(core, &p, &address, pane.as_ref(), clean) {
                e.message.push_str("; abort evidence could not be written");
            }
            Err(e)
        }
    }
}
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

/// Test hook (F3): a callback after every forward step. `Reset` also clears every fs fault a test may arm.
#[cfg(test)]
pub(crate) mod hooks {
    use std::cell::RefCell;
    pub(crate) type After = Box<dyn Fn(&str)>;
    thread_local! {
        static AFTER: RefCell<Option<After>> = const { RefCell::new(None) };
    }
    /// Runs `f` with the step name after every forward step.
    pub(crate) fn after(f: Option<After>) {
        AFTER.with(|a| *a.borrow_mut() = f);
    }
    pub(super) fn fire(step: &str) {
        AFTER.with(|a| {
            if let Some(f) = a.borrow().as_ref() {
                f(step);
            }
        });
    }
    /// Clears the hook and every fault when the test ends.
    pub(crate) struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            after(None);
            crate::state::fs::fail_durable_flush(None);
            crate::state::fs::fail_sync_dir(None);
            crate::state::fs::fail_sync_file(None);
        }
    }
}

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
    let promote = |r: &mut serde_json::Map<String, Value>| {
        r.insert("generation".into(), json!(n.to_string()));
        r.insert("hcAddress".into(), json!(address));
        r.insert("hcName".into(), json!(hc_name));
        r.insert("launcherArgv".into(), json!(argv));
        r.insert("sessionId".into(), json!(l.session_id));
        r.insert("team".into(), json!(l.team));
        r.remove("pending");
        Ok(())
    };
    let record = tx
        .record_write(&l.id, Guard::PendingGen { n: Some(n) }, promote)?
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
        Evidence::Present(r) => Guard::Pending { n, hc_name }.admits(Some(&r)),
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

fn attempt<T>(f: impl FnOnce() -> Res<T>) -> Res<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(Refusal::internal("launch boundary panicked")))
}
fn rollback(
    core: &Core<'_>,
    p: &Prepared,
    address: &str,
    pane: Option<&Created>,
    clean: impl FnOnce(&Created) -> Res<bool>,
) -> bool {
    let mut evidence = json!({"hcAddress":address,"identity":p.record,"pane":pane.map(|v|v.pane.as_str()),"kill":"pending","viewCleanup":"pending"});
    let path = p.folder.join("abort.json");
    let first = write_json(&path, &evidence).is_ok();
    evidence["kill"] = match attempt(|| core.kill(address)) {
        Ok(()) => json!("acknowledged"),
        Err(e) => json!({"uncertain":true,"message":e.message}),
    };
    evidence["viewCleanup"] = match pane.filter(|v| !v.command.is_empty()) {
        None => json!("unproven"),
        Some(v) => match attempt(|| clean(v)) {
            Ok(gone) => json!({"gone":gone}),
            Err(e) => json!({"failed":true,"message":e.message}),
        },
    };
    write_json(&path, &evidence).is_ok() && first
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{agents::fixture::World, spawn::Out, testing::Fakes};
    use serde_json::json;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    fn answer(f: &Fakes, text: &str) {
        f.hc_answer(Ok(Out {
            stdout: text.into(),
            ..Out::default()
        }));
    }
    fn ready(w: &World) {
        w.fakes.hc_install("hc");
        answer(&w.fakes, "hc 0.1.21");
    }
    fn prepared(w: &World) -> Prepared {
        let folder = w.ctx.state_root.join("key/agents/aaaaaaaaaaaa");
        std::fs::create_dir_all(&folder).unwrap();
        let spec = folder.join("spec.json");
        write_json(&spec, &json!({"generation":"0"})).unwrap();
        let mut record = super::super::core_identity::tests::record();
        record.as_object_mut().unwrap().remove("hcAddress");
        record["directory"] = w.ctx.cwd.to_str().unwrap().into();
        Prepared {
            folder,
            argv: crate::launcher_scope::argv(&w.ctx, &spec).unwrap(),
            record,
        }
    }
    fn created() -> Created {
        Created {
            pane: "%9".into(),
            window: "@5".into(),
            command: "hc attach t1".into(),
        }
    }
    #[test]
    fn connected_transaction_does_not_reconnect() {
        let w = World::human("");
        ready(&w);
        let core = hc::connect(&w.ctx).unwrap();
        answer(&w.fakes, r#"{"address":"t1"}"#);
        execute_connected(
            &core,
            || Ok(prepared(&w)),
            || Ok(()),
            |_, _, view| {
                *view = Some(created());
                Ok(())
            },
            |p| write_json(&p.join("commit.json"), &json!({})),
            |_| panic!("clean"),
        )
        .unwrap();
        assert_eq!(
            w.fakes
                .hc_calls()
                .iter()
                .map(|(_, a, _)| a[0].clone())
                .collect::<Vec<_>>(),
            ["--version", "run"]
        );
        w.fakes.hc_done().unwrap();
    }
    #[test]
    fn core_connect_precedes_all_preparation() {
        for version in [None, Some("hc 0.1.20"), Some("garbled")] {
            let w = World::human("");
            if let Some(v) = version {
                w.fakes.hc_install("hc");
                answer(&w.fakes, v);
            }
            let called = Cell::new(false);
            let result = execute(
                &w.ctx,
                || {
                    called.set(true);
                    Ok(prepared(&w))
                },
                |_, _, _| panic!("attach"),
                |_| panic!("commit"),
                |_| panic!("clean"),
            );
            assert!(result.is_err());
            assert!(!called.get());
            assert!(!w.ctx.state_root.exists());
        }
    }
    #[test]
    fn identity_precedes_view_and_commit() {
        let w = World::human("");
        ready(&w);
        answer(&w.fakes, r#"{"address":"t1"}"#);
        let events = RefCell::new(Vec::new());
        let r = execute(
            &w.ctx,
            || {
                events.borrow_mut().push("prepare");
                Ok(prepared(&w))
            },
            |core, p, pane| {
                assert_eq!(core.binary(), w.fakes.hc_install("hc"));
                let r = crate::state::fs::read_json(&p.folder.join("record.json"))?.unwrap();
                assert_eq!(r["hcAddress"], "t1");
                assert_eq!(r["generation"], "0");
                assert!(!p.folder.join("commit.json").exists());
                assert_eq!(r["launcherArgv"], json!(p.argv));
                events.borrow_mut().push("attach");
                *pane = Some(created());
                Ok(())
            },
            |folder| {
                let r = crate::state::fs::read_json(&folder.join("record.json"))?.unwrap();
                assert_eq!(r["pane"], "%9");
                events.borrow_mut().push("commit");
                write_json(&folder.join("commit.json"), &json!({}))
            },
            |_| panic!("clean"),
        )
        .unwrap();
        assert_eq!(*events.borrow(), ["prepare", "attach", "commit"]);
        assert_eq!(r["hcAddress"], "t1");
        let calls = w.fakes.hc_calls();
        assert_eq!(calls.len(), 2);
        let a = &calls[1].1;
        assert_eq!(
            &a[..6],
            [
                "run",
                "--bg",
                "--keep",
                "--outlive-parent",
                "--no-notify",
                "--name"
            ]
        );
        assert_eq!(a[6], "maaaaaaaa");
        for label in [
            "mux.managed=1",
            "mux.team=k:$3",
            "mux.id=aaaaaaaaaaaa",
            "mux.gen=0",
            "mux.name=worker",
        ] {
            assert!(a.iter().any(|s| s == label));
        }
        assert_eq!(a.iter().filter(|s| s.as_str() == "run").count(), 1);
    }

    #[test]
    fn post_run_fault_matrix_preserves_evidence_and_never_skips_cleanup() {
        for stage in [
            "identity",
            "attach",
            "reply",
            "tags",
            "proof",
            "record",
            "commit",
            "timeout",
            "panic",
            "abort-write",
            "kill",
            "cleanup",
            "cleanup-panic",
            "title",
        ] {
            let w = World::human("");
            ready(&w);
            answer(&w.fakes, r#"{"address":"t1"}"#);
            if stage == "kill" {
                w.fakes.hc_answer(Err(Refusal::internal("kill uncertain")));
            } else {
                answer(&w.fakes, "{}");
            }
            let folder = w.ctx.state_root.join("key/agents/aaaaaaaaaaaa");
            let cleaned = Rc::new(Cell::new(0));
            let seen = cleaned.clone();
            let result = execute(
                &w.ctx,
                || {
                    let p = prepared(&w);
                    if stage == "identity" {
                        std::fs::create_dir(p.folder.join("record.json"))?;
                    }
                    Ok(p)
                },
                |_, p, pane| {
                    if ["attach", "reply"].contains(&stage) {
                        return Err(Refusal::internal(stage));
                    }
                    *pane = Some(created());
                    if stage == "record" {
                        std::fs::remove_file(p.folder.join("record.json"))?;
                        std::fs::create_dir(p.folder.join("record.json"))?;
                    }
                    if stage == "abort-write" {
                        std::fs::create_dir(p.folder.join("abort.json"))?;
                    }
                    if stage == "panic" {
                        panic!("after attach");
                    }
                    if [
                        "tags",
                        "proof",
                        "abort-write",
                        "kill",
                        "cleanup",
                        "cleanup-panic",
                    ]
                    .contains(&stage)
                    {
                        return Err(Refusal::internal(stage));
                    }
                    Ok(())
                },
                |p| {
                    if stage == "timeout" {
                        write_json(&p.join("commit.json"), &json!({"aborted":true}))?;
                    }
                    if ["commit", "timeout"].contains(&stage) {
                        return Err(Refusal::internal(stage));
                    }
                    write_json(&p.join("commit.json"), &json!({}))
                },
                move |_| {
                    seen.set(seen.get() + 1);
                    if stage == "cleanup-panic" {
                        panic!("cleanup");
                    }
                    if stage == "cleanup" {
                        return Err(Refusal::internal("probe failed"));
                    }
                    Ok(true)
                },
            );
            if stage == "title" {
                assert!(result.is_ok());
                assert_eq!(cleaned.get(), 0);
                continue;
            }
            assert!(result.is_err(), "{stage}");
            assert!(folder.join("spec.json").exists());
            let calls = w.fakes.hc_calls();
            assert_eq!(calls.iter().filter(|(_, a, _)| a[0] == "run").count(), 1);
            assert_eq!(
                calls
                    .iter()
                    .filter(|(_, a, _)| a[0] == "kill")
                    .map(|(_, a, _)| a[1].as_str())
                    .collect::<Vec<_>>(),
                ["t1"]
            );
            assert_eq!(
                cleaned.get(),
                usize::from(!["identity", "attach", "reply"].contains(&stage)),
                "{stage}"
            );
            if stage != "abort-write" {
                let e = crate::state::fs::read_json(&folder.join("abort.json"))
                    .unwrap()
                    .unwrap();
                assert_eq!(e["hcAddress"], "t1");
                if stage == "kill" {
                    assert_eq!(e["kill"]["uncertain"], true);
                }
                if stage == "cleanup-panic" {
                    assert_eq!(e["viewCleanup"]["failed"], true);
                }
            }
            assert!(!crate::agents::committed(&folder), "{stage}");
        }
    }

    // Each record.json write is flushed to disk; a failed flush at either one rolls the launch back: no commit, so
    // the launcher never starts the client, and the run is killed.
    #[test]
    fn a_failed_record_flush_never_commits() {
        for nth in [0, 1] {
            let w = World::human("");
            ready(&w);
            answer(&w.fakes, r#"{"address":"t1"}"#);
            answer(&w.fakes, "{}");
            crate::state::fs::fail_durable_flush(Some(nth));
            let committed = Cell::new(false);
            let got = execute(
                &w.ctx,
                || Ok(prepared(&w)),
                |_, _, pane| {
                    *pane = Some(created());
                    Ok(())
                },
                |_| {
                    committed.set(true);
                    Ok(())
                },
                |_| Ok(true),
            );
            crate::state::fs::fail_durable_flush(None);
            assert!(got.is_err(), "{nth}");
            assert!(!committed.get(), "{nth}");
            assert_eq!(
                w.fakes.hc_calls().last().unwrap().1,
                ["kill", "t1", "--json"],
                "{nth}"
            );
            w.fakes.hc_done().unwrap();
        }
    }

    #[test]
    fn rollback_writes_pending_before_cleanup_and_remembers_first_write_failure() {
        for block_first_write in [false, true] {
            let w = World::human("");
            ready(&w);
            answer(&w.fakes, r#"{"address":"t1"}"#);
            answer(&w.fakes, "{}");
            let path = w.ctx.state_root.join("key/agents/aaaaaaaaaaaa/abort.json");
            let saw_pending = Cell::new(false);
            let cleaned = Cell::new(false);
            let got = execute(
                &w.ctx,
                || Ok(prepared(&w)),
                |_, _, pane| {
                    *pane = Some(created());
                    if block_first_write {
                        std::fs::create_dir(&path)?;
                    }
                    panic!("after attach");
                },
                |_| panic!("commit"),
                |_| {
                    if block_first_write {
                        // Let the final write succeed without hiding the first write's failure.
                        std::fs::remove_dir(&path)?;
                    } else {
                        // Observe pending evidence before the final write can replace it.
                        if let Some(e) = crate::state::fs::read_json(&path)? {
                            saw_pending.set(
                                e["hcAddress"] == "t1"
                                    && e["kill"] == "pending"
                                    && e["viewCleanup"] == "pending",
                            );
                        }
                    }
                    cleaned.set(true);
                    Ok(true)
                },
            )
            .unwrap_err();
            assert!(cleaned.get());
            if !block_first_write {
                assert!(saw_pending.get(), "pending evidence must precede cleanup");
            }
            assert_eq!(
                got.message
                    .contains("; abort evidence could not be written"),
                block_first_write,
                "first evidence write failure must remain reported"
            );
            let evidence = crate::state::fs::read_json(&path).unwrap().unwrap();
            assert_eq!(evidence["kill"], "acknowledged");
            assert_eq!(evidence["viewCleanup"]["gone"], true);
        }
    }

    #[test]
    fn empty_view_command_never_commits_or_authorizes_cleanup() {
        for attach_fails in [false, true] {
            let w = World::human("");
            ready(&w);
            answer(&w.fakes, r#"{"address":"t1"}"#);
            answer(&w.fakes, "{}");
            let committed = Cell::new(false);
            let cleaned = Cell::new(false);
            let got = execute(
                &w.ctx,
                || Ok(prepared(&w)),
                |_, _, pane| {
                    *pane = Some(Created {
                        command: String::new(),
                        ..created()
                    });
                    if attach_fails {
                        return Err(Refusal::internal("proof failed"));
                    }
                    Ok(())
                },
                |_| {
                    committed.set(true);
                    Ok(())
                },
                |_| {
                    cleaned.set(true);
                    Ok(true)
                },
            );
            assert!(got.is_err(), "empty view command must refuse");
            assert!(!committed.get(), "unproven view must not commit");
            assert!(
                !cleaned.get(),
                "empty view command must not authorize cleanup"
            );
            let evidence = crate::state::fs::read_json(
                &w.ctx.state_root.join("key/agents/aaaaaaaaaaaa/abort.json"),
            )
            .unwrap()
            .unwrap();
            assert_eq!(evidence["viewCleanup"], "unproven");
            assert_eq!(
                w.fakes.hc_calls().last().unwrap().1,
                ["kill", "t1", "--json"]
            );
        }
    }

    #[test]
    fn uncertain_run_reports_failed_abort_write_without_retry() {
        let w = World::human("");
        ready(&w);
        w.fakes.hc_answer(Err(Refusal::internal("timeout")));
        let got = execute(
            &w.ctx,
            || {
                let p = prepared(&w);
                std::fs::create_dir(p.folder.join("abort.json"))?;
                Ok(p)
            },
            |_, _, _| panic!("attach"),
            |_| panic!("commit"),
            |_| panic!("clean"),
        )
        .unwrap_err();
        assert!(
            got.message
                .contains("; abort evidence could not be written")
        );
        let calls = w.fakes.hc_calls();
        assert_eq!(calls.iter().filter(|(_, a, _)| a[0] == "run").count(), 1);
        assert!(!calls.iter().any(|(_, a, _)| a[0] == "kill"));
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn uncertain_run_is_never_retried_or_guessed_killed() {
        struct PanicRun(Fakes);
        impl crate::hc::Hc for PanicRun {
            fn find(&self, n: &str, p: &str) -> Option<PathBuf> {
                crate::hc::Hc::find(&self.0, n, p)
            }
            fn captured(&self, p: &Path, a: &[&str], t: std::time::Duration) -> Res<Out> {
                let out = crate::hc::Hc::captured(&self.0, p, a, t);
                if a[0] == "run" {
                    panic!("run uncertain");
                }
                out
            }
        }
        for mode in ["nonzero", "malformed", "timeout", "panic"] {
            let mut w = World::human("");
            ready(&w);
            match mode {
                "nonzero" => w.fakes.hc_answer(Ok(Out {
                    code: 1,
                    stdout: r#"{"address":"do-not-kill"}"#.into(),
                    stderr: String::new(),
                })),
                "timeout" => w.fakes.hc_answer(Err(Refusal::internal("timeout"))),
                _ => answer(&w.fakes, "not-json"),
            }
            if mode == "panic" {
                w.ctx.hc = Box::new(PanicRun(w.fakes.clone()));
            }
            let got = execute(
                &w.ctx,
                || Ok(prepared(&w)),
                |_, _, _| panic!("attach"),
                |_| panic!("commit"),
                |_| panic!("clean"),
            );
            assert!(got.is_err());
            let calls = w.fakes.hc_calls();
            assert_eq!(calls.iter().filter(|(_, a, _)| a[0] == "run").count(), 1);
            assert!(!calls.iter().any(|(_, a, _)| a[0] == "kill"));
            let e = crate::state::fs::read_json(
                &w.ctx.state_root.join("key/agents/aaaaaaaaaaaa/abort.json"),
            )
            .unwrap()
            .unwrap();
            assert_eq!(e["runUncertain"], true);
            assert!(e["hcAddress"].is_null());
        }
    }
    #[test]
    fn failed_abort_write_is_reported_without_skipping_kill_or_cleanup() {
        let w = World::human("");
        ready(&w);
        answer(&w.fakes, r#"{"address":"t1"}"#);
        answer(&w.fakes, "{}");
        let cleaned = Cell::new(false);
        let got = execute(
            &w.ctx,
            || Ok(prepared(&w)),
            |_, p, pane| {
                *pane = Some(created());
                std::fs::create_dir(p.folder.join("abort.json"))?;
                Err(Refusal::internal("proof failed"))
            },
            |_| panic!("commit"),
            |_| {
                cleaned.set(true);
                Ok(true)
            },
        )
        .unwrap_err();
        assert!(got.message.contains("abort evidence could not be written"));
        assert!(got.message.contains("proof failed"));
        assert!(cleaned.get());
        assert_eq!(
            w.fakes.hc_calls().last().unwrap().1,
            ["kill", "t1", "--json"]
        );
    }
    #[test]
    fn prepare_failure_has_no_run_and_kill_error_still_cleans() {
        let w = World::human("");
        ready(&w);
        let got = execute(
            &w.ctx,
            || Err(Refusal::internal("prepare failed")),
            |_, _, _| panic!("attach"),
            |_| panic!("commit"),
            |_| panic!("clean"),
        );
        assert!(got.is_err());
        assert_eq!(w.fakes.hc_calls().len(), 1);
        let w = World::human("");
        ready(&w);
        let ctx = &w.ctx;
        let core = hc::connect(ctx).unwrap();
        let p = prepared(&w);
        let cleaned = Cell::new(false);
        let value: Res<()> = attempt(|| panic!("boundary panicked"));
        assert!(value.is_err());
        w.fakes.hc_answer(Err(Refusal::internal("kill failed")));
        assert!(rollback(&core, &p, "t1", Some(&created()), |_| {
            cleaned.set(true);
            Ok(true)
        }));
        assert!(cleaned.get());
    }

    #[test]
    fn invalid_preparation_never_runs() {
        for key in ["generation", "team", "sessionId", "spec"] {
            let w = World::human("");
            ready(&w);
            let got = execute(
                &w.ctx,
                || {
                    let mut p = prepared(&w);
                    if key == "spec" {
                        write_json(&p.folder.join("spec.json"), &json!({"generation":"1"}))?;
                    } else {
                        p.record[key] = json!("bad");
                    }
                    Ok(p)
                },
                |_, _, _| panic!("attach"),
                |_| panic!("commit"),
                |_| panic!("clean"),
            );
            assert!(got.is_err());
            assert_eq!(w.fakes.hc_calls().len(), 1, "{key}");
        }
    }
    #[test]
    fn kill_provider_panic_still_cleans_the_proven_view() {
        struct PanicKill(Fakes);
        impl crate::hc::Hc for PanicKill {
            fn find(&self, n: &str, p: &str) -> Option<PathBuf> {
                crate::hc::Hc::find(&self.0, n, p)
            }
            fn captured(&self, p: &Path, a: &[&str], t: std::time::Duration) -> Res<Out> {
                let out = crate::hc::Hc::captured(&self.0, p, a, t);
                if a[0] == "kill" {
                    panic!("kill uncertain");
                }
                out
            }
        }
        let mut w = World::human("");
        ready(&w);
        answer(&w.fakes, r#"{"address":"t1"}"#);
        answer(&w.fakes, "{}");
        w.ctx.hc = Box::new(PanicKill(w.fakes.clone()));
        let cleaned = Cell::new(false);
        let e = execute(
            &w.ctx,
            || Ok(prepared(&w)),
            |_, _, pane| {
                *pane = Some(created());
                Err(Refusal::internal("proof"))
            },
            |_| panic!("commit"),
            |_| {
                cleaned.set(true);
                Ok(true)
            },
        )
        .unwrap_err();
        assert_eq!(e.message, "unexpected error (proof)");
        assert!(cleaned.get());
        let v = crate::state::fs::read_json(
            &w.ctx.state_root.join("key/agents/aaaaaaaaaaaa/abort.json"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(v["kill"]["uncertain"], true);
        assert_eq!(v["viewCleanup"]["gone"], true);
    }
}

#[cfg(test)]
pub(crate) mod forward_tests {
    use super::*;
    use crate::agents::fixture::{ID, SESSION_ID, World};
    use crate::agents::{gens, team_state};
    use crate::spawn::Out;
    use std::{cell::RefCell, rc::Rc};

    pub(crate) const NEW: &str = "1f11fc2f-2032-48ef-a920-4af2a8e5af51";

    pub(crate) fn world() -> World {
        let w = World::human("");
        w.state();
        w.fakes.hc_install("hc");
        w
    }
    /// The version `hc::connect` asks.
    pub(crate) fn version(w: &World) {
        w.fakes.hc_answer(Ok(Out {
            stdout: "hc 0.1.21".into(),
            ..Out::default()
        }));
    }
    /// One launch: the version, then the answer of `hc run`. An unqueued `hc run` answers
    /// `internal`, "unexpected error (unconfigured hc fake)".
    pub(crate) fn replies(w: &World, run: Res<Out>) {
        version(w);
        w.fakes.hc_answer(run);
    }
    pub(crate) fn address(a: &str) -> Res<Out> {
        Ok(Out {
            stdout: json!({"address": a}).to_string(),
            ..Out::default()
        })
    }
    pub(crate) fn launch_of(w: &World, create: bool, session: &str) -> Launch {
        let caller = w.caller();
        let dir = w.ctx.cwd.to_str().unwrap().to_string();
        let state = w.state().to_str().unwrap().to_string();
        Launch {
            id: ID.into(),
            kind: if create { "spawn" } else { "retry" },
            session_id: session.into(),
            create: create.then(|| {
                json!({"schema": 1, "name": "worker", "client": "claude", "session": caller.session,
                    "directory": dir, "options": ["--effort", "high"], "prompt": ["fix"]})
            }),
            spec: json!({"schema": 1, "stateDirectory": state, "client": "claude",
                "flags": ["--effort", "high"], "args": ["--session-id", session], "parentPane": null,
                "prompt": "fix"}),
            team: crate::agents::membership::team(&caller),
            name: "worker".into(),
            up_team: None,
            directory: dir,
        }
    }
    pub(crate) fn go(
        w: &World,
        l: Launch,
        debit: impl FnOnce() -> Res<()>,
        attach: impl FnOnce(&Core<'_>, &Value) -> Res<Value>,
    ) -> Res<Value> {
        let s = team_state::Scope::of(&w.caller());
        team_state::open(&w.ctx, &s, |tx| {
            let core = hc::connect(tx.ctx)?;
            forward(tx, &core, l, debit, attach)
        })
    }
    pub(crate) fn free() -> Res<()> {
        Ok(())
    }
    pub(crate) fn no_view(_: &Core<'_>, _: &Value) -> Res<Value> {
        Ok(json!({}))
    }
    pub(crate) fn never(_: &Core<'_>, _: &Value) -> Res<Value> {
        panic!("attach after a failed launch")
    }
    pub(crate) fn record(w: &World) -> Value {
        crate::state::fs::read_json(&w.folder(ID).join("record.json"))
            .unwrap()
            .unwrap()
    }
    /// Rewrites `<member>/record.json` as another writer would: no lock, no walk.
    pub(crate) fn edit(f: &Path, change: impl FnOnce(&mut Value)) {
        let path = f.join("record.json");
        let mut r = crate::state::fs::read_json(&path).unwrap().unwrap();
        change(&mut r);
        crate::state::fs::write_json(&path, &r).unwrap();
    }
    pub(crate) fn runs(w: &World) -> Vec<Vec<String>> {
        let calls = w.fakes.hc_calls().into_iter();
        calls
            .filter(|(_, a, _)| a[0] == "run")
            .map(|(_, a, _)| a)
            .collect()
    }
    pub(crate) fn kills(w: &World) -> usize {
        w.fakes
            .hc_calls()
            .iter()
            .filter(|(_, a, _)| a[0] == "kill")
            .count()
    }
    pub(crate) fn name_of(run: &[String]) -> String {
        run[run.iter().position(|a| a == "--name").unwrap() + 1].clone()
    }
    pub(crate) fn decided(w: &World, n: u64) -> Option<gens::Decision> {
        gens::read(&w.folder(ID), n, "commit")
            .known()
            .unwrap()
            .map(|c| gens::decide(&c))
    }
    /// Generation 0 launched and promoted, for the tests of generation 1.
    pub(crate) fn promoted(w: &World) {
        replies(w, address("t1"));
        go(w, launch_of(w, true, SESSION_ID), free, no_view).unwrap();
    }
    /// What the files say now (F3).
    fn seen(f: &Path, fakes: &crate::testing::Fakes, n: u64) -> Value {
        let r = crate::state::fs::read_json(&f.join("record.json"))
            .ok()
            .flatten()
            .unwrap_or(Value::Null);
        let file = |k: &str| gens::path(f, n, k).is_file();
        let commit = gens::read(f, n, "commit").known().ok().flatten();
        let runs = fakes
            .hc_calls()
            .iter()
            .filter(|(_, a, _)| a[0] == "run")
            .count();
        json!({"pending": r["pending"]["gen"], "address": r["pending"]["hcAddress"], "session": r["sessionId"],
            "generation": r["generation"], "spec": file("spec"), "launch": file("launch"),
            "commit": commit.map(|c| format!("{:?}", gens::decide(&c))), "runs": runs})
    }
    /// Records `seen` after every forward step.
    fn watch(w: &World, n: u64) -> Rc<RefCell<Vec<(String, Value)>>> {
        let log = Rc::new(RefCell::new(Vec::new()));
        let (l2, f, fakes) = (log.clone(), w.folder(ID), w.fakes.clone());
        hooks::after(Some(Box::new(move |s: &str| {
            l2.borrow_mut().push((s.to_string(), seen(&f, &fakes, n)))
        })));
        log
    }
    /// One expected `seen` value (call 17: a struct, not eight arguments).
    #[derive(Clone)]
    struct Row {
        pending: Value,
        address: Value,
        session: Value,
        generation: Value,
        spec: bool,
        launch: bool,
        commit: Value,
        runs: usize,
    }
    fn row(r: Row) -> Value {
        json!({"pending": r.pending, "address": r.address, "session": r.session, "generation": r.generation,
            "spec": r.spec, "launch": r.launch, "commit": r.commit, "runs": r.runs})
    }

    // G1, F3, F4: each step leaves exactly its files; the view sees the commit and the promotion.
    #[test]
    fn each_forward_step_leaves_exactly_its_files() {
        let w = world();
        let _reset = hooks::Reset;
        let log = watch(&w, 0);
        replies(&w, address("t1"));
        let f = w.folder(ID);
        // The view records what it saw; asserting inside it would be caught by `attempt` as a viewError.
        let at_view = Rc::new(RefCell::new(None));
        let v2 = at_view.clone();
        let view = move |_: &Core<'_>, r: &Value| {
            *v2.borrow_mut() = Some((
                r["generation"].clone(),
                r.get("pending").is_none(),
                gens::committed(&f, 0).ok(),
            ));
            Ok(json!({"pane": "%9", "window": "@5", "startCommand": "hc attach t1", "x": 1}))
        };
        let r = go(&w, launch_of(&w, true, SESSION_ID), free, view).unwrap();
        let z = Value::Null;
        let base = Row {
            pending: json!(0),
            address: z.clone(),
            session: z.clone(),
            generation: z.clone(),
            spec: false,
            launch: false,
            commit: z.clone(),
            runs: 0,
        };
        let spec = Row {
            spec: true,
            ..base.clone()
        };
        let launch = Row {
            launch: true,
            ..spec.clone()
        };
        let run = Row {
            runs: 1,
            ..launch.clone()
        };
        let address = Row {
            address: json!("t1"),
            ..run.clone()
        };
        let commit = Row {
            commit: json!("Run"),
            ..address.clone()
        };
        let promote = Row {
            pending: z.clone(),
            address: z.clone(),
            session: json!(SESSION_ID),
            generation: json!("0"),
            ..commit.clone()
        };
        let want = [
            (
                "allocated",
                row(Row {
                    pending: z.clone(),
                    ..base.clone()
                }),
            ),
            ("pending", row(base)),
            ("spec", row(spec)),
            ("launch", row(launch)),
            ("run", row(run)),
            ("address", row(address)),
            ("commit", row(commit)),
            ("promote", row(promote)),
        ];
        let got = log.borrow().clone();
        assert_eq!(got.len(), want.len(), "{got:?}");
        for ((step, seen), (name, row)) in got.iter().zip(want.iter()) {
            assert_eq!((step.as_str(), seen), (*name, row), "after {name}");
        }
        let by_view = at_view.borrow().clone();
        assert_eq!(
            by_view,
            Some((json!("0"), true, Some(true))),
            "the view follows the commit and the promotion"
        );
        let got = (
            &r["generation"],
            &r["hcAddress"],
            &r["sessionId"],
            &r["lifecycle"],
        );
        assert_eq!(
            got,
            (&json!("0"), &json!("t1"), &json!(SESSION_ID), &json!(1))
        );
        assert_eq!(
            r["team"],
            json!(crate::agents::membership::team(&w.caller()))
        );
        assert_eq!(
            (&r["pane"], r.get("x")),
            (&json!("%9"), None),
            "only the view fields reach the reply"
        );
        assert!(
            record(&w).get("pane").is_none(),
            "a view never reaches record.json"
        );
        let verbs: Vec<String> = w
            .fakes
            .hc_calls()
            .into_iter()
            .map(|(_, a, _)| a[0].clone())
            .collect();
        assert_eq!(verbs, ["--version", "run"], "one connection, one run");
        assert!(runs(&w)[0].contains(&"mux.gen=0".to_string()));
    }

    // G12: the launcher accepts what forward wrote, pending and promoted; launch.json is what hc ran.
    #[test]
    fn the_launcher_accepts_what_forward_wrote() {
        let w = world();
        let _reset = hooks::Reset;
        let f = w.folder(ID);
        let pending = Rc::new(RefCell::new(false));
        let (p2, f2) = (pending.clone(), f.clone());
        hooks::after(Some(Box::new(move |s: &str| {
            if s == "address" {
                *p2.borrow_mut() = crate::launcher::own_spec(&f2, 0).is_some();
            }
        })));
        replies(&w, address("t1"));
        let r = go(&w, launch_of(&w, true, SESSION_ID), free, no_view).unwrap();
        assert!(
            *pending.borrow(),
            "the launcher accepts the pending generation it is started for"
        );
        assert!(
            crate::launcher::own_spec(&f, 0).is_some(),
            "and the promoted one"
        );
        let launch = gens::read(&f, 0, "launch").known().unwrap().unwrap();
        let run = &runs(&w)[0];
        assert_eq!(launch["hcName"], json!(name_of(run)));
        assert!(crate::agents::core_identity::is_hc_name(&name_of(run)));
        assert_ne!(
            name_of(run),
            format!("m{}", &ID[..8]),
            "a new generation never derives its name"
        );
        let promoted = (&r["hcName"], &r["launcherArgv"], &r["sessionId"]);
        assert_eq!(
            (
                &launch["hcName"],
                &launch["launcherArgv"],
                &launch["sessionId"]
            ),
            promoted
        );
        let argv: Vec<String> = serde_json::from_value(launch["launcherArgv"].clone()).unwrap();
        assert!(
            run.ends_with(&argv),
            "hc run was given exactly the recorded launcher argv"
        );
        let spec = gens::path(&f, 0, "spec");
        assert!(
            argv.iter().any(|a| a.as_str() == spec.to_str().unwrap()),
            "the launcher runs the spec of n"
        );
    }

    // G2, G1.5: two generations get two names and labels; the session changes only at promotion.
    #[test]
    fn two_generations_get_two_names_labels_and_sessions() {
        let w = world();
        let _reset = hooks::Reset;
        promoted(&w);
        let log = watch(&w, 1);
        replies(&w, address("t2"));
        let r = go(&w, launch_of(&w, false, NEW), free, no_view).unwrap();
        let all = runs(&w);
        assert_ne!(name_of(&all[0]), name_of(&all[1]));
        assert!(all[1].contains(&"mux.gen=1".to_string()));
        let at_commit = log
            .borrow()
            .iter()
            .find(|(s, _)| s == "commit")
            .unwrap()
            .1
            .clone();
        assert_eq!(
            at_commit["session"],
            json!(SESSION_ID),
            "the old session until promotion"
        );
        let got = (
            &r["generation"],
            &r["sessionId"],
            &r["hcAddress"],
            &r["hcName"],
        );
        assert_eq!(
            got,
            (
                &json!("1"),
                &json!(NEW),
                &json!("t2"),
                &json!(name_of(&all[1]))
            )
        );
        assert_eq!(decided(&w, 1), Some(gens::Decision::Run));
    }

    // G1.1: n follows the active generation the record names, files or not.
    #[test]
    fn a_new_generation_follows_the_active_one() {
        let w = world();
        let f = w.folder(ID);
        crate::agents::spawn::private_folder(&w.ctx, &f).unwrap();
        let team = crate::agents::membership::team(&w.caller());
        let r = json!({"lifecycle": 1, "id": ID, "name": "worker", "session": w.caller().session, "team": team,
            "generation": "3", "hcAddress": "t3", "hcName": "m0123abcd", "launcherArgv": ["x"],
            "sessionId": SESSION_ID});
        crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
        replies(&w, address("t4"));
        let got = go(&w, launch_of(&w, false, NEW), free, no_view).unwrap();
        assert_eq!(got["generation"], json!("4"), "never 0 beside an active 3");
        assert!(!gens::dir(&f, 0).exists() && runs(&w)[0].contains(&"mux.gen=4".to_string()));
    }

    // Ambiguity 1: an undecided pending generation is never superseded.
    #[test]
    fn an_undecided_pending_generation_is_never_superseded() {
        let w = world();
        let f = w.folder(ID);
        crate::agents::spawn::private_folder(&w.ctx, &f).unwrap();
        let r = json!({"lifecycle": 1, "id": ID, "name": "worker", "session": w.caller().session,
            "pending": {"gen": 0, "kind": "spawn", "hcName": "m0123abcd", "sessionId": SESSION_ID}});
        crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
        let before = std::fs::read(f.join("record.json")).unwrap();
        version(&w);
        let e = go(&w, launch_of(&w, false, NEW), free, never).unwrap_err();
        assert_eq!((e.code, e.check), (1, "launch"), "{e:?}");
        assert!(e.message.contains("undecided"), "{e:?}");
        assert_eq!(
            std::fs::read(f.join("record.json")).unwrap(),
            before,
            "the record is untouched"
        );
        assert!(
            !f.join("gens").exists() && runs(&w).is_empty(),
            "nothing made, nothing run"
        );
    }

    // Ambiguity 10: kind, session and a deadline 150 s from the clock (the fake clock moves only on a sleep).
    #[test]
    fn the_spec_names_its_kind_session_and_150_second_deadline() {
        let w = world();
        let at = crate::util::utc_o(w.ctx.clock.now() + std::time::Duration::from_secs(150));
        replies(&w, address("t1"));
        go(&w, launch_of(&w, true, SESSION_ID), free, no_view).unwrap();
        let spec = gens::read(&w.folder(ID), 0, "spec")
            .known()
            .unwrap()
            .unwrap();
        let got = (
            &spec["kind"],
            &spec["sessionId"],
            &spec["deadline"],
            &spec["id"],
            &spec["generation"],
        );
        assert_eq!(
            got,
            (
                &json!("spawn"),
                &json!(SESSION_ID),
                &json!(at),
                &json!(ID),
                &json!("0")
            )
        );
        assert_eq!(
            (&spec["client"], &spec["flags"]),
            (&json!("claude"), &json!(["--effort", "high"]))
        );
    }

    // Ambiguity 6, R-W3: the commit publication fails before its link (`content`), at its read (a folder there),
    // or at a folder flush after the link. Pending and its address stay; no abort, promotion or view.
    #[test]
    fn a_failed_commit_publication_keeps_pending_and_runs_nothing_after() {
        for how in ["content", "folder", "flush"] {
            let w = world();
            let _reset = hooks::Reset;
            let f = w.folder(ID);
            let (commit, n0) = (gens::path(&f, 0, "commit"), gens::dir(&f, 0));
            hooks::after(Some(Box::new(move |s: &str| match (s, how) {
                ("address", "content") => crate::state::fs::fail_sync_file(Some(&commit)),
                ("address", "folder") => std::fs::create_dir_all(&commit).unwrap(),
                ("address", _) => crate::state::fs::fail_sync_dir(Some(&n0)),
                _ => {}
            })));
            replies(&w, address("t1"));
            let e = go(&w, launch_of(&w, true, SESSION_ID), free, never).unwrap_err();
            let got = (e.check, e.message.as_str());
            match how {
                "content" => assert_eq!(got, ("state", gens::NOT_PUBLISHED), "{how}"),
                "flush" => assert_eq!(got, ("state", gens::NOT_FLUSHED), "{how}"),
                _ => assert_eq!(e.check, "evidence", "{how}: {e:?}"),
            }
            let r = record(&w);
            assert_eq!(
                (&r["pending"]["gen"], &r["pending"]["hcAddress"]),
                (&json!(0), &json!("t1")),
                "{how}"
            );
            assert!(
                r.get("generation").is_none() && r.get("sessionId").is_none(),
                "{how}: no promotion"
            );
            if how != "folder" {
                let want = (how == "flush").then_some(gens::Decision::Run);
                assert_eq!(
                    decided(&w, 0),
                    want,
                    "{how}: no abort; a visible unflushed commit is for settle"
                );
            }
            assert_eq!(kills(&w), 0, "{how}");
        }
    }

    // Ambiguity 5, F4: the promotion rename lands and its member-folder flush fails. One write: every promoted
    // field together, pending gone in the same write; the commit stands; nothing is killed or attached.
    #[test]
    fn a_failed_promotion_flush_leaves_one_whole_promotion() {
        let w = world();
        let _reset = hooks::Reset;
        let f2 = w.folder(ID);
        hooks::after(Some(Box::new(move |s: &str| {
            if s == "commit" {
                crate::state::fs::fail_sync_dir(Some(&f2));
            }
        })));
        replies(&w, address("t1"));
        let e = go(&w, launch_of(&w, true, SESSION_ID), free, never).unwrap_err();
        assert_eq!(
            (e.check, e.message.as_str()),
            ("state", team_state::NOT_WRITTEN),
            "{e:?}"
        );
        let r = record(&w);
        let launch = gens::read(&w.folder(ID), 0, "launch")
            .known()
            .unwrap()
            .unwrap();
        assert!(
            r.get("pending").is_none(),
            "pending is dropped by the promoting write itself"
        );
        let team = json!(crate::agents::membership::team(&w.caller()));
        let got = (
            &r["generation"],
            &r["hcAddress"],
            &r["hcName"],
            &r["launcherArgv"],
            &r["sessionId"],
            &r["team"],
        );
        let want = (
            &json!("0"),
            &json!("t1"),
            &launch["hcName"],
            &launch["launcherArgv"],
            &launch["sessionId"],
            &team,
        );
        assert_eq!(
            got, want,
            "session id and terminal identity change together"
        );
        assert_eq!(
            (decided(&w, 0), kills(&w)),
            (Some(gens::Decision::Run), 0),
            "nothing rolled back"
        );
    }

    // Ambiguity 5: the record is unreadable at promotion. Refused evidence; the bytes are never rewritten.
    #[test]
    fn a_refused_promotion_rolls_nothing_back() {
        let w = world();
        let _reset = hooks::Reset;
        let f = w.folder(ID);
        let f2 = f.clone();
        hooks::after(Some(Box::new(move |s: &str| {
            if s == "commit" {
                std::fs::write(f2.join("record.json"), br#"{"lifec"#).unwrap();
            }
        })));
        replies(&w, address("t1"));
        let e = go(&w, launch_of(&w, true, SESSION_ID), free, never).unwrap_err();
        assert_eq!(e.check, "evidence", "{e:?}");
        assert_eq!(
            std::fs::read(f.join("record.json")).unwrap(),
            br#"{"lifec"#.to_vec(),
            "never rewritten"
        );
        assert_eq!((decided(&w, 0), kills(&w)), (Some(gens::Decision::Run), 0));
    }

    // G1.6, F4: the view is optional; its failure is viewError and nothing is rolled back.
    #[test]
    fn a_view_failure_after_the_commit_is_view_error() {
        let w = world();
        replies(&w, address("t1"));
        let fail = |_: &Core<'_>, _: &Value| Err(Refusal::internal("no pane"));
        let r = go(&w, launch_of(&w, true, SESSION_ID), free, fail).unwrap();
        assert!(r["viewError"].as_str().unwrap().contains("no pane"));
        assert_eq!(
            (decided(&w, 0), kills(&w)),
            (Some(gens::Decision::Run), 0),
            "nothing rolled back"
        );
        assert!(
            record(&w).get("viewError").is_none(),
            "the reply carries it, never the record"
        );
    }

    // R5, every OS (`testing::link_dir`): a member folder linked to another member is refused before any write.
    #[test]
    fn a_linked_member_folder_is_refused_before_any_write() {
        let w = world();
        let other = w.folder("bbbbbbbbbbbb");
        std::fs::create_dir_all(&other).unwrap();
        let theirs = json!({"lifecycle": 1, "id": "bbbbbbbbbbbb",
            "pending": {"gen": 0, "hcName": "m0123abcd", "sessionId": SESSION_ID}});
        crate::state::fs::write_json(&other.join("record.json"), &theirs).unwrap();
        let before = std::fs::read(other.join("record.json")).unwrap();
        crate::testing::link_dir(&other, &w.folder(ID));
        version(&w);
        let e = go(&w, launch_of(&w, true, SESSION_ID), free, never).unwrap_err();
        assert_eq!(e.check, "evidence", "{e:?}");
        assert_eq!(std::fs::read(other.join("record.json")).unwrap(), before);
        assert!(
            !other.join("gens").exists() && runs(&w).is_empty(),
            "nothing written through the link"
        );
    }

    // Ambiguity 10: an empty directory refuses before any write, any debit and any run.
    #[test]
    fn an_empty_directory_refuses_before_any_write() {
        let w = world();
        version(&w);
        let mut l = launch_of(&w, true, SESSION_ID);
        l.directory = String::new();
        let debit = || -> Res<()> { panic!("no debit before the directory check") };
        let e = go(&w, l, debit, never).unwrap_err();
        assert_eq!(e.check, "internal", "{e:?}");
        assert!(e.message.contains("no directory"), "{e:?}");
        assert!(
            !w.folder(ID).exists() && runs(&w).is_empty(),
            "nothing made, nothing run"
        );
    }

    // R5, every OS: a torn record refuses evidence before any write, on the create and the existing path.
    #[test]
    fn a_torn_record_is_refused_before_any_write() {
        for create in [true, false] {
            let w = world();
            let f = w.folder(ID);
            crate::agents::spawn::private_folder(&w.ctx, &f).unwrap();
            std::fs::write(f.join("record.json"), br#"{"lifec"#).unwrap();
            version(&w);
            let e = go(&w, launch_of(&w, create, NEW), free, never).unwrap_err();
            assert_eq!(e.check, "evidence", "{create}: {e:?}");
            assert!(!f.join("gens").exists() && runs(&w).is_empty(), "{create}");
        }
    }
    // F1, call 15 (finding 1to2-I6): the address write is `Guard::Pending { n, hc_name }`. At `spec` another writer
    // changes the pending generation alone (name kept), or the name alone; the run succeeds, the address write
    // refuses, nothing is committed or promoted, and that writer's pending stands.
    #[test]
    fn a_record_taken_before_the_address_patch_stops_the_launch() {
        for case in ["gen", "name"] {
            let w = world();
            let _reset = hooks::Reset;
            let f2 = w.folder(ID);
            hooks::after(Some(Box::new(move |s: &str| {
                if s == "spec" {
                    edit(&f2, |r| match case {
                        "gen" => r["pending"]["gen"] = json!(5),
                        _ => r["pending"]["hcName"] = json!("m0123abcd"),
                    });
                }
            })));
            replies(&w, address("t1"));
            let e = go(&w, launch_of(&w, true, SESSION_ID), free, never).unwrap_err();
            assert_eq!(e.check, "launch", "{case}: {e:?}");
            assert!(
                e.message
                    .starts_with("the launch no longer owns the record"),
                "{case}: {e:?}"
            );
            assert_eq!(
                runs(&w).len(),
                1,
                "{case}: the run happened; nothing after it"
            );
            let r = record(&w);
            assert!(
                r["pending"].get("hcAddress").is_none() && r.get("generation").is_none(),
                "{case}"
            );
            assert_eq!(decided(&w, 0), None, "{case}: no commit");
        }
    }

    // F1, call 15: the promotion is `Guard::PendingGen { n: Some(n) }`. The pending generation changes after the
    // commit; the promotion refuses and promotes nothing.
    #[test]
    fn a_record_taken_before_promotion_is_not_promoted() {
        let w = world();
        let _reset = hooks::Reset;
        let f2 = w.folder(ID);
        hooks::after(Some(Box::new(move |s: &str| {
            if s == "commit" {
                edit(&f2, |r| r["pending"]["gen"] = json!(5));
            }
        })));
        replies(&w, address("t1"));
        let e = go(&w, launch_of(&w, true, SESSION_ID), free, never).unwrap_err();
        assert_eq!(e.check, "launch", "{e:?}");
        assert!(e.message.contains("no longer owns the record"), "{e:?}");
        let r = record(&w);
        assert!(
            r.get("generation").is_none() && r.get("sessionId").is_none(),
            "nothing promoted"
        );
        assert_eq!(r["pending"]["gen"], json!(5), "the record is left as found");
        assert_eq!(
            (decided(&w, 0), kills(&w)),
            (Some(gens::Decision::Run), 0),
            "nothing rolled back"
        );
    }

    // F2: the first pending write is guarded by what the precheck read (`Create`; `PendingGen { n: g.pending() }`).
    // Right after the precheck another writer makes a record (new member) or a pending generation (existing
    // member): refused 1 `launch`, nothing written, fenced or run.
    #[test]
    fn the_first_pending_write_is_guarded_by_what_the_precheck_saw() {
        for case in ["create", "existing"] {
            let w = world();
            let _reset = hooks::Reset;
            let create = case == "create";
            if !create {
                promoted(&w);
            }
            let f = w.folder(ID);
            crate::agents::spawn::private_folder(&w.ctx, &f).unwrap();
            let f2 = f.clone();
            hooks::after(Some(Box::new(move |s: &str| {
                if s != "allocated" {
                    return;
                }
                let theirs = json!({"gen": if create { 0 } else { 1 }, "hcName": "m0123abcd"});
                if create {
                    let r = json!({"lifecycle": 1, "id": ID, "pending": theirs});
                    crate::state::fs::write_json(&f2.join("record.json"), &r).unwrap();
                } else {
                    edit(&f2, |r| r["pending"] = theirs);
                }
            })));
            version(&w);
            let e = go(&w, launch_of(&w, create, NEW), free, never).unwrap_err();
            assert_eq!(e.check, "launch", "{case}: {e:?}");
            assert!(
                e.message
                    .contains("changed before this launch was recorded"),
                "{case}: {e:?}"
            );
            assert!(e.message.ends_with("; nothing was fenced"), "{case}: {e:?}");
            assert_eq!(
                record(&w)["pending"]["hcName"],
                json!("m0123abcd"),
                "{case}: theirs stands"
            );
            let n = u64::from(!create);
            assert!(
                !gens::dir(&f, n).exists(),
                "{case}: no generation {n} files"
            );
            assert_eq!(runs(&w).len(), usize::from(!create), "{case}: nothing run");
        }
    }

    /// The fence's end state: commit {aborted}, no pending, the record still generation 0, nothing killed.
    fn fenced(w: &World, n: u64, what: &str) {
        assert_eq!(
            decided(w, n),
            Some(gens::Decision::Aborted),
            "{what}: the generation is fenced"
        );
        assert!(
            gens::done(&w.folder(ID), n).unwrap(),
            "{what}: void, so its slot is free"
        );
        let r = record(w);
        assert!(r.get("pending").is_none(), "{what}: pending dropped");
        let kept = (&r["generation"], &r["sessionId"], &r["hcAddress"]);
        assert_eq!(
            kept,
            (&json!("0"), &json!(SESSION_ID), &json!("t1")),
            "{what}: generation 0 kept"
        );
        assert_eq!(kills(w), 0, "{what}");
    }

    /// Moves the member folder aside to `<agents>/real` and links its name to it (`testing::link_dir`, every OS).
    fn linked_member(f: &Path) -> PathBuf {
        let real = f.parent().unwrap().join("real");
        std::fs::rename(f, &real).unwrap();
        crate::testing::link_dir(&real, f);
        real
    }

    // G1: each failure between pending and the claim aborts generation 1. `content` cases fail the content flush
    // before the link (R-W3 `NOT_PUBLISHED`); `address flush` lands the address rename and fails its flush.
    #[test]
    fn every_failure_before_the_claim_fences_its_own_generation() {
        let cases = [
            "spec exists",
            "spec folder",
            "spec content",
            "launch exists",
            "launch content",
            "debit",
            "run error",
            "run taken",
            "address flush",
        ];
        for case in cases {
            let w = world();
            let _reset = hooks::Reset;
            promoted(&w);
            let f = w.folder(ID);
            let f2 = f.clone();
            hooks::after(Some(Box::new(move |s: &str| match (s, case) {
                ("pending", "spec exists") => {
                    crate::state::evidence::publish(&gens::path(&f2, 1, "spec"), &json!({}))
                        .unwrap();
                }
                ("pending", "spec folder") => {
                    std::fs::create_dir_all(gens::path(&f2, 1, "spec")).unwrap()
                }
                ("pending", "spec content") => {
                    crate::state::fs::fail_sync_file(Some(&gens::path(&f2, 1, "spec")))
                }
                ("spec", "launch exists") => {
                    crate::state::evidence::publish(&gens::path(&f2, 1, "launch"), &json!({}))
                        .unwrap();
                }
                ("spec", "launch content") => {
                    crate::state::fs::fail_sync_file(Some(&gens::path(&f2, 1, "launch")));
                }
                ("run", "address flush") => crate::state::fs::fail_durable_flush(Some(0)),
                _ => {}
            })));
            let run = match case {
                "run error" => Err(Refusal::internal("hc run timed out")),
                "run taken" => Ok(Out {
                    code: 1,
                    stderr: "terminal name already in use".into(),
                    ..Out::default()
                }),
                _ => address("t2"),
            };
            let ran = ["run error", "run taken", "address flush"].contains(&case);
            if ran {
                replies(&w, run);
            } else {
                version(&w);
            }
            let (f3, fakes) = (f.clone(), w.fakes.clone());
            let debit = move || {
                assert!(
                    gens::path(&f3, 1, "launch").is_file(),
                    "the debit follows launch.json"
                );
                let before = fakes
                    .hc_calls()
                    .iter()
                    .filter(|(_, a, _)| a[0] == "run")
                    .count();
                assert_eq!(before, 1, "the debit precedes hc run");
                if case == "debit" {
                    Err(Refusal::internal("no token"))
                } else {
                    Ok(())
                }
            };
            assert!(
                go(&w, launch_of(&w, false, NEW), debit, never).is_err(),
                "{case}"
            );
            assert_eq!(
                runs(&w).len(),
                1 + usize::from(ran),
                "{case}: never retried, nothing run early"
            );
            fenced(&w, 1, case);
            if case == "spec folder" {
                assert!(
                    gens::path(&f, 1, "spec").is_dir(),
                    "the folder is never replaced"
                );
            }
        }
    }

    // F1, F2: a failed first pending write fences only a record that names this call. The create landed and its
    // folder flush failed: owned, so generation 0 is fenced. The record is torn between the precheck and the
    // write: refused evidence, nothing fenced, the bytes untouched.
    #[test]
    fn a_failed_pending_write_fences_only_its_own_generation() {
        let w = world();
        let _reset = hooks::Reset;
        version(&w);
        crate::state::fs::fail_durable_flush(Some(0));
        let e = go(&w, launch_of(&w, true, SESSION_ID), free, never).unwrap_err();
        assert_eq!(
            (e.check, e.message.as_str()),
            ("state", team_state::NOT_WRITTEN),
            "{e:?}"
        );
        assert_eq!(
            decided(&w, 0),
            Some(gens::Decision::Aborted),
            "own create fenced"
        );
        let r = record(&w);
        assert!(r.get("pending").is_none() && r.get("sessionId").is_none() && runs(&w).is_empty());
        assert!(!gens::path(&w.folder(ID), 0, "spec").exists());
        let w = world();
        let _reset = hooks::Reset;
        promoted(&w);
        let f = w.folder(ID);
        let f2 = f.clone();
        hooks::after(Some(Box::new(move |s: &str| {
            if s == "allocated" {
                std::fs::write(f2.join("record.json"), br#"{"lifec"#).unwrap();
            }
        })));
        version(&w);
        let e = go(&w, launch_of(&w, false, NEW), free, never).unwrap_err();
        assert_eq!(e.check, "evidence", "{e:?}");
        assert!(e.message.ends_with("; nothing was fenced"), "{e:?}");
        assert!(
            !gens::dir(&f, 1).exists(),
            "an unreadable record is never taken for this call"
        );
        assert_eq!(
            std::fs::read(f.join("record.json")).unwrap(),
            br#"{"lifec"#.to_vec(),
            "torn bytes untouched"
        );
    }

    // F2, R5 (finding 1to2-C3), every OS: the member folder becomes a link right after the precheck. The first
    // pending write walks (`record_write`), refuses evidence, and nothing reaches the real record.
    #[test]
    fn a_member_linked_after_the_precheck_gets_no_pending() {
        let w = world();
        let _reset = hooks::Reset;
        promoted(&w);
        let f = w.folder(ID);
        let real = f.parent().unwrap().join("real");
        let f2 = f.clone();
        hooks::after(Some(Box::new(move |s: &str| {
            if s == "allocated" {
                linked_member(&f2);
            }
        })));
        version(&w);
        let e = go(&w, launch_of(&w, false, NEW), free, never).unwrap_err();
        assert_eq!(e.check, "evidence", "{e:?}");
        assert!(e.message.ends_with("; nothing was fenced"), "{e:?}");
        let r = crate::state::fs::read_json(&real.join("record.json"))
            .unwrap()
            .unwrap();
        assert!(
            r.get("pending").is_none(),
            "no pending through the link: {r}"
        );
        assert!(
            !gens::dir(&real, 1).exists() && runs(&w).len() == 1,
            "no generation 1, no second run"
        );
    }

    // R-W1, every OS: a linked gens/<n> is never written through, by the spec nor by the abort; pending is kept.
    #[test]
    fn a_linked_generation_folder_is_never_written_through() {
        let w = world();
        let _reset = hooks::Reset;
        promoted(&w);
        let elsewhere = w.ctx.cwd.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let (f2, e2) = (w.folder(ID), elsewhere.clone());
        hooks::after(Some(Box::new(move |s: &str| {
            if s == "pending" {
                crate::testing::link_dir(&e2, &gens::dir(&f2, 1));
            }
        })));
        version(&w);
        let e = go(&w, launch_of(&w, false, NEW), free, never).unwrap_err();
        assert_eq!(e.check, "evidence", "{e:?}");
        assert!(
            e.message.ends_with("; abort evidence could not be written"),
            "{e:?}"
        );
        assert_eq!(
            std::fs::read_dir(&elsewhere).unwrap().count(),
            0,
            "nothing written through the link"
        );
        assert_eq!(
            record(&w)["pending"]["gen"],
            json!(1),
            "unfenced, so settle-1b decides"
        );
        assert_eq!(runs(&w).len(), 1, "no second run");
    }

    // Ambiguity 8: the abort cannot read the commit (a folder there): nothing published, pending kept.
    #[test]
    fn an_abort_that_cannot_read_the_commit_keeps_pending() {
        let w = world();
        let _reset = hooks::Reset;
        promoted(&w);
        let f2 = w.folder(ID);
        hooks::after(Some(Box::new(move |s: &str| {
            if s == "spec" {
                std::fs::create_dir_all(gens::path(&f2, 1, "commit")).unwrap();
                crate::state::fs::fail_sync_file(Some(&gens::path(&f2, 1, "launch")));
            }
        })));
        version(&w);
        let e = go(&w, launch_of(&w, false, NEW), free, never).unwrap_err();
        let want = format!(
            "{}; abort evidence could not be written",
            gens::NOT_PUBLISHED
        );
        assert_eq!(
            (e.check, e.message.as_str()),
            ("state", want.as_str()),
            "the original error, then the suffix"
        );
        assert_eq!(record(&w)["pending"]["gen"], json!(1));
        assert!(
            gens::path(&w.folder(ID), 1, "commit").is_dir(),
            "never replaced"
        );
    }

    // Ambiguity 9: the abort is published, then the pending drop lands its rename and fails its flush. Reported;
    // the aborted commit stands, so the generation is done.
    #[test]
    fn a_failed_pending_drop_is_reported_and_the_generation_is_void() {
        let w = world();
        let _reset = hooks::Reset;
        promoted(&w);
        let f2 = w.folder(ID);
        hooks::after(Some(Box::new(move |s: &str| {
            if s == "spec" {
                crate::state::fs::fail_sync_file(Some(&gens::path(&f2, 1, "launch")));
                crate::state::fs::fail_durable_flush(Some(0));
            }
        })));
        version(&w);
        let e = go(&w, launch_of(&w, false, NEW), free, never).unwrap_err();
        assert!(
            e.message.ends_with("; pending could not be dropped"),
            "{e:?}"
        );
        assert_eq!(decided(&w, 1), Some(gens::Decision::Aborted));
        assert!(record(&w).get("pending").is_none(), "the rename landed");
        assert!(
            gens::done(&w.folder(ID), 1).unwrap(),
            "void whatever pending says"
        );
        assert_eq!(runs(&w).len(), 1, "nothing ran");
    }

    // F1, call 15: one case per condition of ownership, each failing when only that condition is removed from
    // `owns`: a readable record, the hcName, the pending generation, a present record, the lifecycle mark, the
    // walk (a linked member folder). The launch publication fails at `spec`, so each reaches `abort`.
    #[test]
    fn ownership_refuses_each_condition_alone() {
        for case in ["torn", "name", "gen", "absent", "lifecycle", "link"] {
            let w = world();
            let _reset = hooks::Reset;
            promoted(&w);
            let f = w.folder(ID);
            let home = if case == "link" {
                f.parent().unwrap().join("real")
            } else {
                f.clone()
            };
            let f2 = f.clone();
            hooks::after(Some(Box::new(move |s: &str| {
                if s != "spec" {
                    return;
                }
                crate::state::fs::fail_sync_file(Some(&gens::path(&f2, 1, "launch")));
                match case {
                    "torn" => std::fs::write(f2.join("record.json"), br#"{"lifec"#).unwrap(),
                    "name" => edit(&f2, |r| r["pending"]["hcName"] = json!("m0123abcd")),
                    "gen" => edit(&f2, |r| r["pending"]["gen"] = json!(2)),
                    "absent" => std::fs::remove_file(f2.join("record.json")).unwrap(),
                    "lifecycle" => edit(&f2, |r| {
                        r.as_object_mut().unwrap().remove("lifecycle");
                    }),
                    _ => {
                        linked_member(&f2);
                    }
                }
            })));
            version(&w);
            let e = go(&w, launch_of(&w, false, NEW), free, never).unwrap_err();
            assert!(e.message.ends_with("; nothing was fenced"), "{case}: {e:?}");
            assert!(
                !gens::path(&home, 1, "commit").exists(),
                "{case}: no commit for a generation not owned"
            );
        }
    }

    // F1, call 15: `abort` judges ownership again inside its pending drop. The record is swapped, or the member
    // folder becomes a link, when the drop takes the member lock (after the aborted commit was published while
    // owned). `owns` and the publication take no member lock, so the drop takes the first one after `spec`.
    #[test]
    fn abort_rechecks_ownership_inside_its_pending_drop() {
        for case in ["swap", "link"] {
            let w = world();
            let _reset = hooks::Reset;
            promoted(&w);
            let f = w.folder(ID);
            let key = format!("+{}", team_state::Scope::of(&w.caller()).member_lock(ID));
            let (f2, fakes) = (f.clone(), w.fakes.clone());
            hooks::after(Some(Box::new(move |s: &str| {
                if s != "spec" {
                    return;
                }
                crate::state::fs::fail_sync_file(Some(&gens::path(&f2, 1, "launch")));
                let (f3, key2, fired) = (f2.clone(), key.clone(), std::cell::Cell::new(false));
                fakes.lock_probe(move |event| {
                    if event == key2 && !fired.replace(true) {
                        if case == "swap" {
                            edit(&f3, |r| r["pending"]["hcName"] = json!("m0123abcd"));
                        } else {
                            linked_member(&f3);
                        }
                    }
                    String::new()
                });
            })));
            version(&w);
            let e = go(&w, launch_of(&w, false, NEW), free, never).unwrap_err();
            let real = if case == "link" {
                f.parent().unwrap().join("real")
            } else {
                f.clone()
            };
            let r = crate::state::fs::read_json(&real.join("record.json"))
                .unwrap()
                .unwrap();
            if case == "swap" {
                assert!(
                    e.message.ends_with("; pending is not this launch's, kept"),
                    "{case}: {e:?}"
                );
                assert_eq!(
                    r["pending"]["hcName"],
                    json!("m0123abcd"),
                    "the other launch keeps its pending"
                );
            } else {
                assert!(
                    e.message.ends_with("; pending could not be dropped"),
                    "{case}: {e:?}"
                );
                assert_eq!(
                    r["pending"]["gen"],
                    json!(1),
                    "nothing written through the link"
                );
            }
            assert!(
                gens::path(&real, 1, "commit").is_file(),
                "{case}: the abort was published while owned"
            );
        }
    }

    // R-W1 (findings 1to2-C1, C2, I1, I2, I3): a failed folder flush is a failed publication, at every pre-claim
    // site, on every call (`Exists` included). The abort meets the same fault, so its commit is visible but
    // unflushed: the suffix, pending kept for settle-1b (ambiguity 8). Asserted by check and stable phrase.
    #[test]
    fn a_failed_generation_folder_flush_is_a_failed_publication() {
        for case in [
            "gens at pending",
            "gens/n at pending",
            "member at pending, new member",
            "gens/n at spec",
        ] {
            let w = world();
            let _reset = hooks::Reset;
            let new = case.ends_with("new member");
            if !new {
                promoted(&w);
            }
            let f = w.folder(ID);
            let n = u64::from(!new);
            let (at, target) = match case {
                "gens at pending" => ("pending", f.join("gens")),
                "member at pending, new member" => ("pending", f.clone()),
                "gens/n at spec" => ("spec", gens::dir(&f, n)),
                _ => ("pending", gens::dir(&f, n)),
            };
            hooks::after(Some(Box::new(move |s: &str| {
                if s == at {
                    crate::state::fs::fail_sync_dir(Some(&target));
                }
            })));
            version(&w);
            let e = go(&w, launch_of(&w, new, NEW), free, never).unwrap_err();
            let want = format!("{}; abort evidence could not be written", gens::NOT_FLUSHED);
            assert_eq!(
                (e.check, e.message.as_str()),
                ("state", want.as_str()),
                "{case}"
            );
            assert_eq!(
                record(&w)["pending"]["gen"],
                json!(n),
                "{case}: pending kept"
            );
            assert_eq!(
                decided(&w, n),
                Some(gens::Decision::Aborted),
                "{case}: the abort is visible, unflushed"
            );
            assert_eq!(
                runs(&w).len(),
                usize::from(!new),
                "{case}: no run of generation {n}"
            );
        }
    }

    /// Another writer makes generation n commit.json right after `at`: `Some(body)` whole, `None` torn bytes.
    fn commit_after(w: &World, at: &'static str, n: u64, body: Option<Value>) {
        let commit = gens::path(&w.folder(ID), n, "commit");
        hooks::after(Some(Box::new(move |s: &str| {
            if s != at {
                return;
            }
            match &body {
                Some(v) => {
                    crate::state::evidence::publish(&commit, v).unwrap();
                }
                None => {
                    std::fs::create_dir_all(commit.parent().unwrap()).unwrap();
                    std::fs::write(&commit, br#"{"comm"#).unwrap();
                }
            }
        })));
    }

    // G1.4, ambiguity 7: Aborted drops pending; Run, Invalid and torn keep it (torn: the publisher refuses
    // `evidence` at its read, so `lost` is never reached).
    #[test]
    fn a_lost_commit_claim_is_decided_by_its_winner() {
        let cases = [
            (
                Some(json!({"aborted": true})),
                "launch",
                "gave up before the commit",
                false,
            ),
            (
                Some(json!({"committed": true})),
                "internal",
                "is not this launch",
                true,
            ),
            (
                Some(json!({"x": 1})),
                "internal",
                "is not this launch",
                true,
            ),
            (None, "evidence", "", true),
        ];
        for (body, check, says, kept) in cases {
            let w = world();
            let _reset = hooks::Reset;
            commit_after(&w, "address", 0, body.clone());
            replies(&w, address("t1"));
            let e = go(&w, launch_of(&w, true, SESSION_ID), free, never).unwrap_err();
            assert_eq!((e.code, e.check), (1, check), "{body:?}: {e:?}");
            assert!(e.message.contains(says), "{body:?}: {e:?}");
            let r = record(&w);
            assert_eq!(r.get("pending").is_some(), kept, "{body:?}");
            if kept {
                assert_eq!(
                    r["pending"]["hcAddress"],
                    json!("t1"),
                    "{body:?}: the address stays for settle-1b"
                );
            }
            assert!(
                r.get("generation").is_none() && r.get("sessionId").is_none(),
                "{body:?}: no promotion"
            );
            assert_eq!(kills(&w), 0, "{body:?}");
        }
    }

    // Ambiguity 8: an abort that meets a commit already there lets that commit decide.
    #[test]
    fn an_abort_meeting_a_commit_lets_it_decide() {
        for (body, check, kept) in [
            (json!({"aborted": true}), "launch", false),
            (json!({"committed": true}), "internal", true),
        ] {
            let w = world();
            let _reset = hooks::Reset;
            promoted(&w);
            let f2 = w.folder(ID);
            let winner = body.clone();
            hooks::after(Some(Box::new(move |s: &str| {
                if s == "spec" {
                    // The winner lands, then the launch publication fails, so the abort meets the winner.
                    crate::state::evidence::publish(&gens::path(&f2, 1, "commit"), &winner)
                        .unwrap();
                    crate::state::fs::fail_sync_file(Some(&gens::path(&f2, 1, "launch")));
                }
            })));
            version(&w);
            let e = go(&w, launch_of(&w, false, NEW), free, never).unwrap_err();
            assert_eq!(e.check, check, "{body}: {e:?}");
            let r = record(&w);
            assert_eq!(r.get("pending").is_some(), kept, "{body}");
            assert_eq!(
                r["generation"],
                json!("0"),
                "{body}: generation 0 stays active"
            );
            assert_eq!(runs(&w).len(), 1, "{body}: no second run");
        }
    }

    // R-W1, G1.4: a readable winner decides before any write is tried, even where the publication would fail
    // (its content flush is armed to fail).
    #[test]
    fn a_readable_winner_decides_before_publication_is_tried() {
        let w = world();
        let _reset = hooks::Reset;
        let commit = gens::path(&w.folder(ID), 0, "commit");
        hooks::after(Some(Box::new(move |s: &str| {
            if s == "address" {
                crate::state::evidence::publish(&commit, &json!({"aborted": true})).unwrap();
                crate::state::fs::fail_sync_file(Some(&commit));
            }
        })));
        replies(&w, address("t1"));
        let e = go(&w, launch_of(&w, true, SESSION_ID), free, never).unwrap_err();
        assert_eq!((e.code, e.check), (1, "launch"), "{e:?}");
        assert!(
            record(&w).get("pending").is_none(),
            "the aborted winner voids the generation"
        );
    }

    // Ambiguity 9: the claim lost to Aborted, then the pending drop lands its rename and fails its flush. Reported.
    #[test]
    fn a_failed_pending_drop_after_a_lost_claim_is_reported() {
        let w = world();
        let _reset = hooks::Reset;
        let commit = gens::path(&w.folder(ID), 0, "commit");
        hooks::after(Some(Box::new(move |s: &str| {
            if s == "address" {
                crate::state::evidence::publish(&commit, &json!({"aborted": true})).unwrap();
                crate::state::fs::fail_durable_flush(Some(0));
            }
        })));
        replies(&w, address("t1"));
        let e = go(&w, launch_of(&w, true, SESSION_ID), free, never).unwrap_err();
        assert_eq!((e.code, e.check), (1, "launch"), "{e:?}");
        assert!(
            e.message.ends_with("; pending could not be dropped"),
            "{e:?}"
        );
        assert!(record(&w).get("pending").is_none(), "the rename landed");
        assert_eq!(decided(&w, 0), Some(gens::Decision::Aborted));
    }

    // Ambiguity 7: the winner vanished or tore between `Exists` and the reread inside `lost`. Evidence; pending
    // untouched. `lost` is called directly: no forward path reaches this window deterministically.
    #[test]
    fn a_lost_claim_whose_winner_cannot_be_reread_keeps_pending() {
        for how in ["absent", "torn"] {
            let w = world();
            let f = w.folder(ID);
            crate::agents::spawn::private_folder(&w.ctx, &f).unwrap();
            let r = json!({"lifecycle": 1, "id": ID, "name": "worker", "session": w.caller().session,
                "pending": {"gen": 0, "kind": "spawn", "hcName": "m4567cdef", "sessionId": SESSION_ID,
                    "hcAddress": "t1"}});
            crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
            if how == "torn" {
                std::fs::create_dir_all(gens::dir(&f, 0)).unwrap();
                std::fs::write(gens::path(&f, 0, "commit"), br#"{"comm"#).unwrap();
            }
            let before = std::fs::read(f.join("record.json")).unwrap();
            let s = team_state::Scope::of(&w.caller());
            let e = team_state::open(&w.ctx, &s, |tx| Ok(lost(tx, ID, 0, "m4567cdef"))).unwrap();
            assert_eq!(e.check, "evidence", "{how}: {e:?}");
            assert_eq!(
                std::fs::read(f.join("record.json")).unwrap(),
                before,
                "{how}: pending untouched"
            );
        }
    }

    // F1: the claim lost to Aborted while the record already names another launch pending (changed at
    // `address`, before `lost` runs). The refusal stands; that pending is kept.
    #[test]
    fn a_lost_claim_never_drops_a_pending_it_does_not_own() {
        let w = world();
        let _reset = hooks::Reset;
        let f = w.folder(ID);
        let (f2, commit) = (f.clone(), gens::path(&f, 0, "commit"));
        hooks::after(Some(Box::new(move |s: &str| {
            if s == "address" {
                crate::state::evidence::publish(&commit, &json!({"aborted": true})).unwrap();
                edit(&f2, |r| r["pending"]["hcName"] = json!("m0123abcd"));
            }
        })));
        replies(&w, address("t1"));
        let e = go(&w, launch_of(&w, true, SESSION_ID), free, never).unwrap_err();
        assert_eq!((e.code, e.check), (1, "launch"), "{e:?}");
        assert!(
            e.message.ends_with("; pending is not this launch's, kept"),
            "{e:?}"
        );
        assert_eq!(
            record(&w)["pending"]["hcName"],
            json!("m0123abcd"),
            "the other launch keeps pending"
        );
    }

    // Call 15, finding 3to4-I1 (the auditor's test, written out): `lost` checks ownership inside the member lock,
    // not before taking it. At `address` the aborted winner is published and a lock probe is armed; when `lost`
    // takes the member lock to drop pending, another launch takes the pending (same generation, new hcName). The
    // drop is refused inside the lock; the replacement pending stands. A check before the lock followed by an
    // unconditional drop (`owns(..)` then `tx.patch(id, &[], &["pending"])`) removes the replacement and fails here.
    #[test]
    fn a_lost_claim_checks_ownership_inside_the_member_lock() {
        let w = world();
        let _reset = hooks::Reset;
        let f = w.folder(ID);
        let key = format!("+{}", team_state::Scope::of(&w.caller()).member_lock(ID));
        let (f2, commit, fakes) = (f.clone(), gens::path(&f, 0, "commit"), w.fakes.clone());
        hooks::after(Some(Box::new(move |s: &str| {
            if s != "address" {
                return;
            }
            crate::state::evidence::publish(&commit, &json!({"aborted": true})).unwrap();
            // Neither the commit publication nor the reread takes a member lock: the first one is the drop.
            let (f3, key2, fired) = (f2.clone(), key.clone(), std::cell::Cell::new(false));
            fakes.lock_probe(move |event| {
                if event == key2 && !fired.replace(true) {
                    edit(&f3, |r| r["pending"]["hcName"] = json!("m0123abcd"));
                }
                String::new()
            });
        })));
        replies(&w, address("t1"));
        let e = go(&w, launch_of(&w, true, SESSION_ID), free, never).unwrap_err();
        assert_eq!((e.code, e.check), (1, "launch"), "{e:?}");
        assert!(
            e.message.ends_with("; pending is not this launch's, kept"),
            "{e:?}"
        );
        let r = record(&w);
        assert_eq!(
            r["pending"]["hcName"],
            json!("m0123abcd"),
            "the replacement pending stands"
        );
        assert_eq!(r["pending"]["gen"], json!(0));
        assert_eq!(
            decided(&w, 0),
            Some(gens::Decision::Aborted),
            "the winner decides"
        );
    }
}

/// Crash tests (launch-1 crash model). Call 28: A3b-1 guarantees convergence by a direct retry after `allocated`
/// (nothing written) and after the launcher gives up (`launch`, `run`, `address`: commit Aborted, the generation
/// void). A crash after `pending`, `spec` or an unpromoted `commit` is not converged by a retry (it is refused while
/// the generation is undecided); group B's settle (G3) replays it.
#[cfg(test)]
pub(crate) mod crash {
    use super::forward_tests::*;
    use crate::agents::fixture::{ID, SESSION_ID, World};
    use crate::agents::gens;
    use serde_json::json; // finding 4-1: the glob import of forward_tests does not bring the macro
    use std::path::{Path, PathBuf};

    const STEPS: [&str; 7] = [
        "pending", "spec", "launch", "run", "address", "commit", "promote",
    ];

    /// A root without the Windows verbatim prefix, as `launcher::tests::setup` makes it (a spec path must classify).
    fn plain(root: &Path) -> PathBuf {
        #[cfg(windows)]
        if let Some(p) = root.to_str().and_then(|r| r.strip_prefix(r"\\?\")) {
            return PathBuf::from(p);
        }
        root.to_path_buf()
    }

    /// Runs `child` again in this binary with MUX_AGENTS_CRASH=step; answers the `CRASH-<KEY> <value>` lines.
    pub(crate) fn killed_at(child: &str, step: &str) -> Vec<(String, String)> {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                child,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("MUX_AGENTS_CRASH", step)
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(86),
            "{step}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        text.lines()
            .filter_map(|l| l.split_once("CRASH-").map(|(_, kv)| kv))
            .filter_map(|kv| kv.split_once(char::is_whitespace))
            .map(|(k, v)| (k.to_string(), v.trim().to_string()))
            .collect()
    }
    pub(crate) fn printed(kv: &[(String, String)], key: &str) -> PathBuf {
        PathBuf::from(&kv.iter().find(|(k, _)| k == key).expect(key).1)
    }

    #[test]
    #[ignore = "a child of a_kill_after_each_forward_step_leaves_a_state_the_readers_classify"]
    fn child_forward() {
        let mut w = World::human("");
        w.ctx.state_root = plain(&w.ctx.state_root);
        w.state();
        w.fakes.hc_install("hc");
        replies(&w, address("t1"));
        println!("CRASH-ROOT {}", w.ctx.state_root.display());
        let _ = go(&w, launch_of(&w, true, SESSION_ID), free, no_view);
        std::mem::forget(w);
    }

    /// The fake client on PATH and its process row (parent: this process), as `launcher::tests::setup` makes them.
    fn with_client(w: &mut World) {
        let bin = w.ctx.state_root.parent().unwrap().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join(if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        });
        std::fs::write(&exe, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut pairs: Vec<(String, String)> = w
            .ctx
            .env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        pairs.push(("PATH".into(), bin.to_str().unwrap().into()));
        w.ctx.env = crate::ctx::Env::from_pairs(pairs);
        w.fakes.process(crate::testing::row(4300, w.ctx.pid, 1600));
    }

    /// The real launcher of generation 0, in this process: its exit code and how many clients it started.
    fn launcher_once(w: &World, f: &Path) -> (i32, usize) {
        let spec = gens::path(f, 0, "spec");
        let started = std::cell::Cell::new(0);
        let code = crate::launcher::run(
            &w.ctx,
            spec.to_str().unwrap(),
            &mut |_| {},
            &mut |_, _, _, told| {
                started.set(started.get() + 1);
                told(4300);
                Ok(0)
            },
        );
        (code, started.get())
    }

    fn decided_at(f: &Path) -> Option<gens::Decision> {
        gens::read(f, 0, "commit")
            .known()
            .unwrap()
            .map(|c| gens::decide(&c))
    }

    // launch-1 crash guarantee (finding aa9286): killed after each step, the files are what that step left, the
    // readers hold the slot, a retry is refused while undecided, and the launcher starts the client at most
    // once and only after the commit.
    #[test]
    fn a_kill_after_each_forward_step_leaves_a_state_the_readers_classify() {
        for (i, step) in STEPS.iter().enumerate() {
            let root = printed(
                &killed_at("agents::core_launch::crash::child_forward", step),
                "ROOT",
            );
            let mut w = World::human("");
            w.ctx.state_root = root.clone();
            let f = w.folder(ID);
            let at = |s: &str| i >= STEPS.iter().position(|x| *x == s).unwrap();
            let r = crate::state::fs::read_json(&f.join("record.json"))
                .unwrap()
                .unwrap();
            assert_eq!(
                r.get("pending").is_some(),
                !at("promote"),
                "{step}: pending until promotion"
            );
            let pending_gen = if at("promote") { json!(null) } else { json!(0) };
            assert_eq!(r["pending"]["gen"], pending_gen, "{step}");
            let addressed = r["pending"].get("hcAddress").is_some();
            assert_eq!(addressed, at("address") && !at("promote"), "{step}");
            assert_eq!(
                r.get("sessionId").is_some(),
                at("promote"),
                "{step}: the session only at promotion"
            );
            assert_eq!(gens::path(&f, 0, "spec").is_file(), at("spec"), "{step}");
            assert_eq!(
                gens::path(&f, 0, "launch").is_file(),
                at("launch"),
                "{step}"
            );
            assert_eq!(gens::committed(&f, 0).unwrap(), at("commit"), "{step}");
            assert!(!gens::dir(&f, 1).exists(), "{step}: one generation");
            // A3a readers: the member holds its name and slot (held before promotion), never finished.
            let gens::Member::Life(g) = gens::member_of(&f) else {
                panic!("{step}: not a lifecycle member")
            };
            let s = g.status().unwrap();
            let got = (s.subject, s.held, s.committed, s.finished);
            assert_eq!(
                got,
                (Some(0), !at("promote"), at("commit"), false),
                "{step}"
            );
            let locals = crate::agents::membership::locals(&w.ctx, &w.caller()).unwrap();
            assert_eq!(locals.len(), 1, "{step}: the slot is held");
            assert_eq!(locals[0].held, !at("promote"), "{step}");
            // Nothing launches twice: a retry is refused while the generation is undecided.
            if !at("promote") {
                w.fakes.hc_install("hc");
                version(&w);
                let e = go(&w, launch_of(&w, false, NEW), free, never).unwrap_err();
                assert_eq!(e.check, "launch", "{step}: {e:?}");
                assert!(!gens::dir(&f, 1).exists() && runs(&w).is_empty(), "{step}");
            }
            with_client(&mut w);
            let (code, started) = launcher_once(&w, &f);
            match (at("launch"), at("commit")) {
                // Finding 4-2: no spec.json yet, so the launcher cannot canonicalize it (main: exit 2, launcher.rs
                // line 212, its test at line 997); a spec without launch.json is not its own (exit 1).
                (false, _) if !at("spec") => assert_eq!((code, started), (2, 0), "{step}: no spec"),
                (false, _) => {
                    assert_eq!((code, started), (1, 0), "{step}: not a launch of its own")
                }
                (true, false) => {
                    assert_eq!(started, 0, "{step}: no commit, no client");
                    assert_eq!(
                        decided_at(&f),
                        Some(gens::Decision::Aborted),
                        "{step}: the launcher gave up"
                    );
                    let gens::Member::Life(g) = gens::member_of(&f) else {
                        panic!("{step}")
                    };
                    assert!(
                        g.status().unwrap().finished,
                        "{step}: void, so the slot is free"
                    );
                }
                (true, true) => {
                    assert_eq!(started, 1, "{step}");
                    assert_eq!(
                        launcher_once(&w, &f),
                        (0, 0),
                        "{step}: a second launcher starts nothing"
                    );
                }
            }
            let _ = std::fs::remove_dir_all(root.parent().unwrap());
        }
    }
    // F3 (finding re3to4-1): killed after `allocated`, before any write: exit 86, no member, no slot held, and a
    // first launch then succeeds as generation 0.
    #[test]
    fn a_kill_after_allocation_leaves_no_member_and_a_launch_succeeds() {
        let root = printed(
            &killed_at("agents::core_launch::crash::child_forward", "allocated"),
            "ROOT",
        );
        let mut w = World::human("");
        w.ctx.state_root = root.clone();
        let f = w.folder(ID);
        assert!(
            matches!(gens::member_of(&f), gens::Member::Absent),
            "nothing was written"
        );
        assert!(
            crate::agents::membership::locals(&w.ctx, &w.caller())
                .unwrap()
                .is_empty(),
            "no slot is held"
        );
        w.fakes.hc_install("hc");
        replies(&w, address("t1"));
        let r = go(&w, launch_of(&w, true, SESSION_ID), free, no_view).unwrap();
        assert_eq!(
            (&r["generation"], r.get("pending")),
            (&json!("0"), None),
            "the first launch converges"
        );
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    // Finding re3to4-2: killed after launch, run or address, the launcher gives up (commit Aborted) and the
    // generation is void; a retry of forward then converges: generation 1 is launched and promoted.
    #[test]
    fn after_the_launcher_gives_up_a_retry_converges() {
        for step in ["launch", "run", "address"] {
            let root = printed(
                &killed_at("agents::core_launch::crash::child_forward", step),
                "ROOT",
            );
            let mut w = World::human("");
            w.ctx.state_root = root.clone();
            let f = w.folder(ID);
            with_client(&mut w);
            assert_eq!(
                launcher_once(&w, &f).1,
                0,
                "{step}: no client without a commit"
            );
            assert_eq!(
                decided_at(&f),
                Some(gens::Decision::Aborted),
                "{step}: the launcher gave up"
            );
            // The retry runs in a fresh world on the same root: the launcher PATH of `with_client` stays out of it.
            let mut w2 = World::human("");
            w2.ctx.state_root = root.clone();
            w2.fakes.hc_install("hc");
            replies(&w2, address("t2"));
            let r = go(&w2, launch_of(&w2, false, NEW), free, no_view)
                .unwrap_or_else(|e| panic!("{step}: the retry converges: {e:?}"));
            let got = (
                &r["generation"],
                &r["hcAddress"],
                &r["sessionId"],
                r.get("pending"),
            );
            assert_eq!(
                got,
                (&json!("1"), &json!("t2"), &json!(NEW), None),
                "{step}"
            );
            assert!(
                gens::committed(&f, 1).unwrap(),
                "{step}: generation 1 committed"
            );
            let _ = std::fs::remove_dir_all(root.parent().unwrap());
        }
    }

    // Call 34: the commit is visible and its `gens/0` flush failed (forward answers NOT_FLUSHED). With the fault
    // still armed the launcher cannot make the commit durable: it starts no client and exits as an uncommitted
    // launch. With the fault cleared before it runs, it starts the client once, and a second launcher starts none.
    #[test]
    fn a_commit_the_launcher_cannot_flush_starts_no_client() {
        for armed in [true, false] {
            let _reset = super::hooks::Reset;
            let mut w = World::human("");
            w.ctx.state_root = plain(&w.ctx.state_root);
            w.state();
            w.fakes.hc_install("hc");
            let f = w.folder(ID);
            let n0 = gens::dir(&f, 0);
            super::hooks::after(Some(Box::new(move |s: &str| {
                if s == "address" {
                    crate::state::fs::fail_sync_dir(Some(&n0));
                }
            })));
            replies(&w, address("t1"));
            let e = go(&w, launch_of(&w, true, SESSION_ID), free, never).unwrap_err();
            let got = (e.check, e.message.as_str());
            assert_eq!(got, ("state", gens::NOT_FLUSHED), "{armed}: {e:?}");
            assert_eq!(
                decided_at(&f),
                Some(gens::Decision::Run),
                "{armed}: the commit is visible"
            );
            super::hooks::after(None);
            if !armed {
                crate::state::fs::fail_sync_dir(None);
            }
            with_client(&mut w);
            if armed {
                assert_eq!(
                    launcher_once(&w, &f),
                    (1, 0),
                    "an unflushed commit starts no client"
                );
            } else {
                let (first, synced) = crate::state::fs::record_syncs(|| launcher_once(&w, &f));
                assert_eq!(first, (0, 1), "a flushed commit starts the client");
                // The commit's folders, innermost first, as `publish_once` flushes them (call 34).
                let parent = f.parent().unwrap().to_path_buf();
                let want = [gens::dir(&f, 0), f.join("gens"), f.clone(), parent];
                assert!(
                    synced.windows(4).any(|x| x == want),
                    "the launcher flushes the commit's folders: {synced:?}"
                );
                assert_eq!(
                    launcher_once(&w, &f),
                    (0, 0),
                    "a second launcher starts nothing"
                );
            }
        }
    }
}
