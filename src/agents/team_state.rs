//! The one writer of `record.json` and `desired.json` (#103 state-0 S3), and of `view.json` (S4).
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::error::{Refusal, Res};
use crate::state::evidence;
use crate::state::fs::{write_json, write_json_durable};
use crate::util::utc_o;

/// S1: a team is its anchor server key and session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub key: String,
    pub session: String,
}

impl Scope {
    pub fn of(caller: &Caller) -> Scope {
        Scope {
            key: caller.server_key.clone(),
            session: caller.session.clone(),
        }
    }
    pub fn agents(&self, root: &Path) -> PathBuf {
        root.join(&self.key).join("agents")
    }
    /// S1: the spawn lock of the session, the key launch_hc already takes.
    pub fn team_lock(&self) -> String {
        format!("spawn|{}|{}", self.key, self.session)
    }
    pub fn member_lock(&self, id: &str) -> String {
        format!("member|{}|{id}", self.key)
    }
    /// S9: the gen lock, keyed by member id and generation.
    pub fn gen_lock(&self, id: &str, n: u64) -> String {
        gen_lock_key(&self.key, id, n)
    }
}

/// S9 (report-2b ambiguity 9): the gen lock of member `id`'s generation `n` in the team `key`. K4, K5 and outcome-2 name
/// this one lock.
pub fn gen_lock_key(key: &str, id: &str, n: u64) -> String {
    format!("gen|{key}|{id}.{n}")
}

/// A replay hook open runs before its body (adopt-3e registers its journal, release-1d gens::complete).
pub type Hook = fn(&Tx<'_>) -> Res<()>;
const HOOKS: &[Hook] = &[super::release::complete, super::settle::fence];

#[cfg(test)]
thread_local! {
    static TEST_HOOK: std::cell::Cell<Option<Hook>> = const { std::cell::Cell::new(None) };
}
#[cfg(test)]
pub fn test_hook(hook: Option<Hook>) {
    TEST_HOOK.with(|h| h.set(hook));
}

/// The team lock held, every hook run.
pub struct Tx<'a> {
    pub ctx: &'a Ctx,
    pub scope: Scope,
}

/// Takes the team lock, runs every replay hook, then f.
pub fn open<T>(ctx: &Ctx, scope: &Scope, f: impl FnOnce(&Tx<'_>) -> Res<T>) -> Res<T> {
    super::lock(ctx, &scope.team_lock(), || held(ctx, scope, f))
}

/// open for a caller that already holds the team lock (launch_hc_locked, resume).
pub fn held<T>(ctx: &Ctx, scope: &Scope, f: impl FnOnce(&Tx<'_>) -> Res<T>) -> Res<T> {
    let tx = Tx {
        ctx,
        scope: scope.clone(),
    };
    for hook in HOOKS {
        hook(&tx)?;
    }
    #[cfg(test)]
    if let Some(hook) = TEST_HOOK.with(|h| h.get()) {
        hook(&tx)?;
    }
    f(&tx)
}

/// Call 18b: what a record write requires of the record it reads under the member lock. Every condition is listed
/// here, so one table tests them all (call 19).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guard<'a> {
    /// No record.
    Create,
    /// A lifecycle record; `None`: no `pending` key; `Some(n)`: `pending.gen == n`.
    PendingGen { n: Option<u64> },
    /// A lifecycle record whose `pending` names generation `n` with hc name `hc_name` (this launch).
    Pending { n: u64, hc_name: &'a str },
    /// A lifecycle record whose `completing.gen == n` (group B clears `completing`).
    CompletingGen { n: u64 },
}

impl Guard<'_> {
    /// Whether the record read (None: absent) qualifies for member `id`. `abort` asks this of its own read too: one
    /// definition of ownership. Call 35: every guard but `Create` (which admits no record) needs the record's `id`
    /// to be `id`.
    pub fn admits(self, id: &str, record: Option<&Value>) -> bool {
        let Some(r) = record else {
            return self == Guard::Create;
        };
        if !super::gens::lifecycle(r) {
            return false;
        }
        if r.get("id").and_then(Value::as_str) != Some(id) {
            return false;
        }
        let gen_of = |k: &str| r.get(k).and_then(|v| v.get("gen")).and_then(Value::as_u64);
        match self {
            Guard::Create => false,
            // A present key, `null` included, is a pending.
            Guard::PendingGen { n: None } => r.get("pending").is_none(),
            Guard::PendingGen { n: Some(n) } => gen_of("pending") == Some(n),
            Guard::Pending { n, hc_name } => {
                gen_of("pending") == Some(n) && r["pending"]["hcName"].as_str() == Some(hc_name)
            }
            Guard::CompletingGen { n } => gen_of("completing") == Some(n),
        }
    }
}

/// R-W3: a record write or one of its flushes failed.
pub const NOT_WRITTEN: &str = "the member record could not be written";

fn not_written() -> Refusal {
    Refusal {
        code: 1,
        check: "state",
        message: NOT_WRITTEN.into(),
        reason: None,
        leftover: None,
        details: Default::default(),
    }
}

impl Tx<'_> {
    /// Call 18b, R-W2: the one lifecycle record writer. Under the member lock: one read through the walk
    /// (`gens::record`; a link at `<id>`, torn bytes: `evidence`), `guard.admits` of that read, `patch`, one
    /// `write_json_durable` (content, then the member folder); a created record also flushes `agents`.
    /// `Ok(None)`: the guard refused, nothing written and the patch never called. A patch error is returned and
    /// nothing is written; a refused or failed create makes no folder.
    pub fn record_write(
        &self,
        id: &str,
        guard: Guard<'_>,
        patch: impl FnOnce(&mut Map<String, Value>) -> Res<()>,
    ) -> Res<Option<Value>> {
        let folder = self.folder(id);
        super::lock(self.ctx, &self.scope.member_lock(id), || {
            let read = super::gens::record(&folder).known()?;
            if !guard.admits(id, read.as_ref()) {
                return Ok(None);
            }
            let created = read.is_none();
            // `admits` takes only a lifecycle record (an object) or, for `Create`, none.
            let mut fields = match read {
                Some(Value::Object(m)) => m,
                _ => Map::new(),
            };
            patch(&mut fields)?;
            if created {
                fields.insert("lifecycle".into(), json!(1));
                fields.insert("id".into(), json!(id));
                super::spawn::private_folder(self.ctx, &folder).map_err(|_| not_written())?;
            }
            let record = Value::Object(fields);
            write_json_durable(&folder.join("record.json"), &record).map_err(|_| not_written())?;
            if created {
                let agents = self.scope.agents(&self.ctx.state_root);
                crate::state::fs::sync_dir(&agents).map_err(|_| not_written())?;
            }
            Ok(Some(record))
        })
    }

    /// launch-1 G1.5 and release-1d R4: one `record_write` guarded by `PendingGen { n: Some(n) }`. Sets
    /// `generation`, `hcAddress`, `team` and `launch.json`'s `hcName`, `launcherArgv`, `sessionId`; adds
    /// `completing` when given; drops `pending`. `Ok(Some(record))`: the record written. `Ok(None)`: pending is
    /// not n (the guard), nothing written. `internal`: the identity is incomplete. `evidence`: the record is
    /// Unreadable, including through a linked member folder. Neither error writes anything.
    pub fn promote(
        &self,
        id: &str,
        n: u64,
        launch: &Value,
        address: &str,
        team: &str,
        completing: Option<Value>,
    ) -> Res<Option<Value>> {
        let (Some(name), Some(argv), Some(sid)) = (
            launch.get("hcName").filter(|v| v.is_string()),
            launch.get("launcherArgv").filter(|v| v.is_array()),
            launch.get("sessionId").filter(|v| v.is_string()),
        ) else {
            return Err(Refusal::internal("incomplete launch identity"));
        };
        self.record_write(id, Guard::PendingGen { n: Some(n) }, |f| {
            for (k, v) in [
                ("generation", json!(n.to_string())),
                ("hcAddress", json!(address)),
                ("team", json!(team)),
                ("hcName", name.clone()),
                ("launcherArgv", argv.clone()),
                ("sessionId", sid.clone()),
            ] {
                f.insert(k.into(), v);
            }
            if let Some(c) = completing {
                f.insert("completing".into(), c);
            }
            f.remove("pending");
            Ok(())
        })
    }

    pub fn folder(&self, id: &str) -> PathBuf {
        self.scope.agents(&self.ctx.state_root).join(id)
    }

    /// The record as it is now; Unreadable refuses evidence.
    pub fn record(&self, id: &str) -> Res<Option<Value>> {
        evidence::read(&self.folder(id).join("record.json")).known()
    }

    /// Create-only, before any hc run: lifecycle 1, flushed with its folder entry.
    pub fn create(&self, id: &str, mut record: Value) -> Res<Value> {
        let folder = self.folder(id);
        super::spawn::private_folder(self.ctx, &folder)?;
        match evidence::read(&folder.join("record.json")) {
            evidence::Evidence::Absent => {}
            evidence::Evidence::Present(_) => {
                return Err(Refusal::internal("the member already has a record"));
            }
            evidence::Evidence::Unreadable => return Err(evidence::unreadable()),
        }
        record["lifecycle"] = json!(1);
        record["id"] = json!(id);
        write_json_durable(&folder.join("record.json"), &record)?;
        crate::state::fs::sync_dir(&self.scope.agents(&self.ctx.state_root))?;
        Ok(record)
    }

    /// Member lock, reread, set only the named fields and drop the named ones, write durably.
    pub fn patch(&self, id: &str, set: &[(&str, Value)], drop: &[&str]) -> Res<Value> {
        let folder = self.folder(id);
        super::lock(self.ctx, &self.scope.member_lock(id), || {
            let mut record = evidence::read(&folder.join("record.json"))
                .known()?
                .ok_or_else(|| Refusal::internal("the member has no record"))?;
            let fields = record
                .as_object_mut()
                .ok_or_else(|| Refusal::internal("the member record is not an object"))?;
            for (k, v) in set {
                fields.insert((*k).to_string(), v.clone());
            }
            for k in drop {
                fields.remove(*k);
            }
            write_json_durable(&folder.join("record.json"), &record)?;
            Ok(record)
        })
    }

    /// desired.json holds desired and at (state-0 S2). Reread under the member lock through the walk from the state
    /// root (A3a R5, b-calls 13): a link or a non-folder on the way, or an unreadable file, refuses and nothing is
    /// written. A failed write is `state`, `NOT_WRITTEN` (A3b R-W3).
    pub fn desired(&self, id: &str, running: bool) -> Res<()> {
        let folder = self.folder(id);
        let word = if running { "running" } else { "stopped" };
        let body = json!({"desired": word, "at": utc_o(self.ctx.clock.now())});
        super::lock(self.ctx, &self.scope.member_lock(id), || {
            super::gens::walk(&folder, &["desired.json"]).known()?;
            write_json_durable(&folder.join("desired.json"), &body).map_err(|_| not_written())
        })
    }
}

/// The presentation fields view.json carries (views-6f V1).
pub const VIEW_FIELDS: [&str; 3] = ["pane", "window", "startCommand"];

/// S4: the member lock only; replaces view.json, never touches record.json. A view names its generation (V1,
/// ambiguity 7).
pub fn view(ctx: &Ctx, scope: &Scope, id: &str, view: &Value) -> Res<()> {
    if !view.get("gen").is_some_and(Value::is_u64) {
        return Err(Refusal::internal("a view names its generation"));
    }
    let path = scope.agents(&ctx.state_root).join(id).join("view.json");
    super::lock(ctx, &scope.member_lock(id), || write_json(&path, view))
}

/// V1 (ambiguity 6; R1, R2, R4): a lifecycle record never presents its own pane, window or startCommand; view.json's
/// do, only when `gen` is the active generation, `hcAddress` the record's, and `hcName` that generation's
/// `launch.json` `hcName` (never the record's). view.json and launch.json are each read once; Unreadable refuses.
/// No active generation presents nothing. A pre-lifecycle record is returned as it is.
pub fn presented(folder: &Path, mut record: Value) -> Res<Value> {
    use super::gens;
    if !gens::lifecycle(&record) {
        return Ok(record);
    }
    if let Some(r) = record.as_object_mut() {
        for k in VIEW_FIELDS {
            r.remove(k);
        }
    }
    let Some(n) = record
        .get("generation")
        .and_then(Value::as_str)
        .and_then(gens::plain_gen)
    else {
        return Ok(record);
    };
    let Some(v) = gens::walk(folder, &["view.json"]).known()? else {
        return Ok(record);
    };
    let launch = gens::read(folder, n, "launch").known()?;
    let name = launch
        .as_ref()
        .and_then(|l| l.get("hcName"))
        .and_then(Value::as_str);
    let ours = v.get("gen").and_then(Value::as_u64) == Some(n)
        && v.get("hcAddress")
            .is_some_and(|a| Some(a) == record.get("hcAddress"))
        && name.is_some_and(|h| v.get("hcName").and_then(Value::as_str) == Some(h));
    if ours {
        for k in VIEW_FIELDS {
            if let Some(x) = v.get(k) {
                record[k] = x.clone();
            }
        }
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::fixture::{ID, World};
    use std::cell::Cell;

    fn world() -> (World, Scope) {
        let w = World::human("");
        w.state();
        let s = Scope::of(&w.caller());
        (w, s)
    }
    fn record(w: &World) -> Value {
        crate::state::fs::read_json(&w.folder(ID).join("record.json"))
            .unwrap()
            .unwrap()
    }

    /// The view fields `presented` shows for record `r` beside `view` (None for a field it does not show).
    fn shown(w: &World, v: &Value) -> Res<[Option<Value>; 3]> {
        let f = w.folder(ID);
        crate::state::fs::write_json(&f.join("view.json"), v).unwrap();
        let r = presented(&f, record(w))?;
        Ok(VIEW_FIELDS.map(|k| r.get(k).cloned()))
    }

    // V1 (a280e7 CRITICAL 1; audits A8-A11): gen, hcAddress and launch.json's hcName must be the active generation's;
    // a rejected view shows none of pane, window, startCommand, and never the record's own.
    #[test]
    fn only_the_active_generations_view_presents() {
        use crate::{agents::gens, state::evidence::publish};
        let (w, s) = world();
        let base = json!({"generation": "1", "hcAddress": "t1", "hcName": "m0123abcd", "pane": "%2",
            "window": "@2", "startCommand": "old"});
        open(&w.ctx, &s, |tx| tx.create(ID, base).map(|_| ())).unwrap();
        let f = w.folder(ID);
        publish(
            &gens::path(&f, 1, "launch"),
            &json!({"hcName": "m1111aaaa"}),
        )
        .unwrap();
        let good = json!({"pane": "%9", "window": "@5", "startCommand": "hc attach t1", "gen": 1,
            "hcAddress": "t1", "hcName": "m1111aaaa"});
        let none: [Option<Value>; 3] = [None, None, None];
        assert_eq!(
            shown(&w, &good).unwrap(),
            [
                Some(json!("%9")),
                Some(json!("@5")),
                Some(json!("hc attach t1"))
            ]
        );
        for (k, bad) in [
            ("gen", json!(0)),
            ("hcAddress", json!("t0")),
            ("hcName", json!("m0123abcd")),
        ] {
            let mut v = good.clone();
            v[k] = bad;
            assert_eq!(
                shown(&w, &v).unwrap(),
                none,
                "{k}: not the active generation's (hcName is launch.json's)"
            );
        }
        let mut v = good.clone();
        v.as_object_mut().unwrap().remove("gen");
        assert_eq!(shown(&w, &v).unwrap(), none, "a view without gen");
        std::fs::write(gens::path(&f, 1, "launch"), b"{\"hc").unwrap();
        assert_eq!(
            shown(&w, &good).map_err(|e| e.check).err(),
            Some("evidence"),
            "torn launch.json"
        );
        std::fs::remove_file(gens::path(&f, 1, "launch")).unwrap();
        std::fs::create_dir(gens::path(&f, 1, "launch")).unwrap();
        assert_eq!(
            shown(&w, &good).map_err(|e| e.check).err(),
            Some("evidence"),
            "launch.json a folder"
        );
        std::fs::remove_dir(gens::path(&f, 1, "launch")).unwrap();
        assert_eq!(
            shown(&w, &good).unwrap(),
            none,
            "no launch.json: no hcName to match"
        );
        std::fs::write(f.join("view.json"), b"{\"pa").unwrap();
        assert_eq!(
            presented(&f, record(&w)).map_err(|e| e.check).err(),
            Some("evidence"),
            "torn view"
        );
        let mut pending = record(&w);
        pending.as_object_mut().unwrap().remove("generation");
        pending["pending"] = json!({"gen": 1});
        crate::state::fs::write_json(&f.join("record.json"), &pending).unwrap();
        publish(
            &gens::path(&f, 1, "launch"),
            &json!({"hcName": "m1111aaaa"}),
        )
        .unwrap();
        assert_eq!(
            shown(&w, &good).unwrap(),
            none,
            "no active generation presents nothing"
        );
    }

    // Ambiguity 7: no writer makes a descriptor V1 rejects.
    #[test]
    fn a_view_without_gen_is_refused() {
        let (w, s) = world();
        open(&w.ctx, &s, |tx| tx.create(ID, json!({})).map(|_| ())).unwrap();
        for v in [json!({"pane": "P1"}), json!({"pane": "P1", "gen": "1"})] {
            let e = view(&w.ctx, &s, ID, &v).unwrap_err();
            assert_eq!(
                e.message, "unexpected error (a view names its generation)",
                "{v}"
            );
            assert!(!w.folder(ID).join("view.json").exists(), "{v}");
        }
        view(&w.ctx, &s, ID, &json!({"pane": "P1", "gen": 0})).unwrap();
    }

    #[test]
    fn create_marks_lifecycle_and_is_create_only() {
        let (w, s) = world();
        open(&w.ctx, &s, |tx| tx.create(ID, json!({"name": "worker"}))).unwrap();
        assert_eq!(record(&w)["lifecycle"], json!(1));
        assert_eq!(record(&w)["id"], json!(ID));
        let again = open(&w.ctx, &s, |tx| tx.create(ID, json!({"name": "other"})));
        assert!(again.is_err());
        assert_eq!(record(&w)["name"], "worker");
    }

    #[test]
    fn open_runs_hooks_under_the_team_lock_before_the_body() {
        fn hook(tx: &Tx<'_>) -> Res<()> {
            write_json(&tx.ctx.state_root.join("hook"), &json!(1))
        }
        let (w, s) = world();
        test_hook(Some(hook));
        let saw = open(&w.ctx, &s, |tx| Ok(tx.ctx.state_root.join("hook").exists()));
        test_hook(None);
        assert!(saw.unwrap());
        assert_eq!(w.fakes.locks()[0], format!("+{}", s.team_lock()));
    }

    // A-1: a view writer paused after its read; a patch changes parentAgent; both survive.
    #[test]
    fn a_view_write_and_a_patch_both_survive() {
        let (w, s) = world();
        open(&w.ctx, &s, |tx| {
            tx.create(
                ID,
                json!({"parentAgent": null, "generation": "0", "hcAddress": "t1"}),
            )
        })
        .unwrap();
        let folder = w.folder(ID);
        let name = format!("m{}", &ID[..8]);
        crate::state::evidence::publish(
            &crate::agents::gens::path(&folder, 0, "launch"),
            &json!({"hcName": name}),
        )
        .unwrap();
        let seen = record(&w);
        let parent = json!("bbbbbbbbbbbb");
        open(&w.ctx, &s, |tx| {
            tx.patch(ID, &[("parentAgent", parent.clone())], &[])
        })
        .unwrap();
        let before = std::fs::read(folder.join("record.json")).unwrap();
        let v = json!({"pane": "P9", "window": "W5", "startCommand": "hc attach t1", "gen": 0,
            "hcAddress": "t1", "hcName": name});
        view(&w.ctx, &s, ID, &v).unwrap();
        assert_eq!(std::fs::read(folder.join("record.json")).unwrap(), before);
        let r = presented(&folder, record(&w)).unwrap();
        assert_eq!(r["parentAgent"], parent);
        assert_eq!(r["pane"], json!("P9"));
        assert!(seen.get("pane").is_none());
    }

    // An erring write runs no dependent effect; the file is whole, old or new.
    #[test]
    fn an_erring_patch_runs_nothing_after_it() {
        let (w, s) = world();
        open(&w.ctx, &s, |tx| tx.create(ID, json!({"generation": "0"}))).unwrap();
        let ran = Cell::new(false);
        crate::state::fs::fail_durable_flush(Some(0));
        let got = open(&w.ctx, &s, |tx| {
            tx.patch(ID, &[("generation", json!("1"))], &[])?;
            ran.set(true);
            Ok(())
        });
        crate::state::fs::fail_durable_flush(None);
        assert!(got.is_err() && !ran.get());
        let r = evidence::read(&w.folder(ID).join("record.json"));
        assert!(matches!(r, evidence::Evidence::Present(_)));
    }

    #[test]
    fn patch_and_desired_take_the_member_lock_inside_the_team_lock() {
        let (w, s) = world();
        open(&w.ctx, &s, |tx| {
            tx.create(ID, json!({}))?;
            tx.patch(ID, &[("pending", json!({"gen": 1}))], &[])?;
            tx.patch(ID, &[], &["pending"])?;
            tx.desired(ID, false)
        })
        .unwrap();
        let locks = w.fakes.locks();
        assert_eq!(locks.first().unwrap(), &format!("+{}", s.team_lock()));
        assert!(locks.contains(&format!("+{}", s.member_lock(ID))));
        assert!(record(&w).get("pending").is_none());
        let d = crate::state::fs::read_json(&w.folder(ID).join("desired.json"));
        assert_eq!(d.unwrap().unwrap()["desired"], "stopped");
    }

    // S1, S9: the exact keys; the team lock is the spawn lock launch_hc takes.
    #[test]
    fn lock_keys_are_exact() {
        let s = Scope {
            key: "0123456789abcdef".into(),
            session: "$3".into(),
        };
        assert_eq!(s.team_lock(), "spawn|0123456789abcdef|$3");
        assert_eq!(s.member_lock(ID), "member|0123456789abcdef|aaaaaaaaaaaa");
        assert_eq!(s.gen_lock(ID, 2), "gen|0123456789abcdef|aaaaaaaaaaaa.2");
        assert_eq!(
            gen_lock_key("0123456789abcdef", ID, 2),
            "gen|0123456789abcdef|aaaaaaaaaaaa.2"
        );
        let (w, s) = world();
        let c = w.caller();
        assert_eq!(
            s.team_lock(),
            crate::agents::lock_key("spawn", &c, &c.session)
        );
    }

    // S3: replay hooks run while the team lock is held.
    #[test]
    fn hooks_run_inside_the_team_lock() {
        fn hook(tx: &Tx<'_>) -> Res<()> {
            super::super::lock(tx.ctx, "hook", || Ok(()))
        }
        let (w, s) = world();
        w.fakes.clear_log();
        crate::agents::core_fixture::online(&w);
        test_hook(Some(hook));
        let got = open(&w.ctx, &s, |_| Ok(()));
        test_hook(None);
        got.unwrap();
        assert!(
            w.fakes.hc_calls().is_empty(),
            "a hook with nothing to do called hc"
        );
        let t = s.team_lock();
        let want = [
            format!("+{t}"),
            "+hook".into(),
            "-hook".into(),
            format!("-{t}"),
        ];
        assert_eq!(w.fakes.locks(), want);
    }

    // S3: a refusing replay hook runs no body and patches nothing; its refusal is the answer.
    #[test]
    fn a_refusing_hook_runs_no_body_and_patches_nothing() {
        fn hook(_: &Tx<'_>) -> Res<()> {
            Err(evidence::unreadable())
        }
        let (w, s) = world();
        open(&w.ctx, &s, |tx| tx.create(ID, json!({"generation": "0"}))).unwrap();
        let at = w.folder(ID).join("record.json");
        let before = std::fs::read(&at).unwrap();
        let ran = Cell::new(false);
        test_hook(Some(hook));
        let got = open(&w.ctx, &s, |tx| {
            ran.set(true);
            tx.patch(ID, &[("generation", json!("1"))], &[]).map(|_| ())
        });
        test_hook(None);
        assert_eq!(got.map_err(|e| e.check), Err("evidence"));
        assert!(!ran.get(), "the body ran");
        assert_eq!(std::fs::read(&at).unwrap(), before);
    }

    fn member_locked(w: &World, s: &Scope, f: impl FnOnce(&Tx<'_>) -> Res<()>) -> Vec<String> {
        w.fakes.clear_log();
        open(&w.ctx, s, f).unwrap();
        w.fakes.locks()
    }

    // S3: patch and desired each take the member lock, inside the team lock.
    #[test]
    fn patch_and_desired_each_take_the_member_lock() {
        let (w, s) = world();
        open(&w.ctx, &s, |tx| tx.create(ID, json!({})).map(|_| ())).unwrap();
        let (t, m) = (s.team_lock(), s.member_lock(ID));
        let want = [
            format!("+{t}"),
            format!("+{m}"),
            format!("-{m}"),
            format!("-{t}"),
        ];
        let p = member_locked(&w, &s, |tx| {
            tx.patch(ID, &[("x", json!(1))], &[]).map(|_| ())
        });
        assert_eq!(p, want, "patch");
        let d = member_locked(&w, &s, |tx| tx.desired(ID, true));
        assert_eq!(d, want, "desired");
    }

    // S4: view takes the member lock only.
    #[test]
    fn view_takes_the_member_lock_only() {
        let (w, s) = world();
        open(&w.ctx, &s, |tx| tx.create(ID, json!({})).map(|_| ())).unwrap();
        w.fakes.clear_log();
        view(&w.ctx, &s, ID, &json!({"pane": "P1", "gen": 0})).unwrap();
        let m = s.member_lock(ID);
        assert_eq!(w.fakes.locks(), [format!("+{m}"), format!("-{m}")]);
    }

    // S3: create and desired are durable; a failed flush runs nothing after them.
    #[test]
    fn create_and_desired_are_durable() {
        let (w, s) = world();
        let ran = Cell::new(false);
        crate::state::fs::fail_durable_flush(Some(0));
        let got = open(&w.ctx, &s, |tx| {
            tx.create(ID, json!({}))?;
            ran.set(true);
            Ok(())
        });
        crate::state::fs::fail_durable_flush(None);
        assert!(got.is_err() && !ran.get(), "create");
        let (w, s) = world();
        open(&w.ctx, &s, |tx| tx.create(ID, json!({})).map(|_| ())).unwrap();
        crate::state::fs::fail_durable_flush(Some(0));
        let got = open(&w.ctx, &s, |tx| {
            tx.desired(ID, true)?;
            ran.set(true);
            Ok(())
        });
        crate::state::fs::fail_durable_flush(None);
        assert!(got.is_err() && !ran.get(), "desired");
    }

    // S3: desired rereads desired.json under the member lock; an unreadable one is refused, never replaced.
    #[test]
    fn desired_refuses_an_unreadable_file() {
        let (w, s) = world();
        open(&w.ctx, &s, |tx| tx.create(ID, json!({})).map(|_| ())).unwrap();
        let at = w.folder(ID).join("desired.json");
        std::fs::write(&at, b"{").unwrap();
        let got = open(&w.ctx, &s, |tx| tx.desired(ID, false));
        assert_eq!(got.map_err(|e| e.check), Err("evidence"));
        assert_eq!(std::fs::read(&at).unwrap(), b"{");
    }

    // cascade-3d C1, A3a R5, b-calls 13: desired.json is reread through the walk from the state root. A linked
    // member folder (Unix symlink; Windows directory symlink or junction) refuses `evidence` and the link's target
    // gains no desired.json. A failed flush is `state`, `NOT_WRITTEN`.
    #[test]
    fn desired_walks_from_the_state_root() {
        let w = World::human("");
        let s = Scope::of(&w.caller());
        let target = w._dir.path().join("elsewhere");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(w.folder(ID).parent().unwrap()).unwrap();
        crate::testing::link_dir(&target, &w.folder(ID));
        let got = open(&w.ctx, &s, |tx| tx.desired(ID, false)).unwrap_err();
        assert_eq!(got.check, "evidence", "a linked member folder");
        assert!(
            !target.join("desired.json").exists(),
            "nothing written through the link"
        );
        std::fs::remove_dir(w.folder(ID))
            .or_else(|_| std::fs::remove_file(w.folder(ID)))
            .unwrap();
        std::fs::create_dir_all(w.folder(ID)).unwrap();
        crate::state::fs::fail_sync_dir(Some(&w.folder(ID)));
        let got = open(&w.ctx, &s, |tx| tx.desired(ID, false)).unwrap_err();
        crate::state::fs::fail_sync_dir(None);
        assert_eq!(
            (got.check, got.message.as_str()),
            ("state", NOT_WRITTEN),
            "a failed flush"
        );
    }
    // G1.5, B-R2: one patch sets generation, hcAddress, team and launch.json's identity, drops pending, adds
    // completing when given; only while pending names n (exactly once; otherwise `Ok(None)`, nothing written); an
    // incomplete identity writes nothing; the answer is the record written.
    #[test]
    fn promote_sets_the_identity_once_while_pending_names_n() {
        use crate::agents::life_fixture::hc_name;
        let (w, s) = world();
        let launch = json!({"hcName": hc_name(2), "launcherArgv": ["/x"], "sessionId": "sid-2"});
        let r0 = json!({"sessionId": "old", "pending": {"gen": 2}});
        open(&w.ctx, &s, |tx| tx.create(ID, r0).map(|_| ())).unwrap();
        let wrong = open(&w.ctx, &s, |tx| {
            tx.promote(ID, 1, &launch, "t2", "k:$3", None)
        });
        assert_eq!(wrong.map_err(|e| e.check), Ok(None), "pending is not 1");
        assert_eq!(record(&w)["sessionId"], "old", "a refused promotion wrote");
        let c = json!({"gen": 2, "intent": "ended"});
        let wrote = open(&w.ctx, &s, |tx| {
            tx.promote(ID, 2, &launch, "t2", "k:$3", Some(c.clone()))
        })
        .unwrap();
        let r = record(&w);
        assert_eq!(wrote.as_ref(), Some(&r), "the answer is the record written");
        assert_eq!(
            (&r["generation"], &r["hcAddress"], &r["team"]),
            (&json!("2"), &json!("t2"), &json!("k:$3"))
        );
        assert_eq!(
            (&r["hcName"], &r["sessionId"], &r["completing"]),
            (&json!(hc_name(2)), &json!("sid-2"), &c)
        );
        assert_eq!(r["launcherArgv"], json!(["/x"]));
        assert!(r.get("pending").is_none());
        let again = open(&w.ctx, &s, |tx| {
            tx.promote(ID, 2, &launch, "t2", "k:$3", None)
        });
        assert_eq!(again.map_err(|e| e.check), Ok(None), "exactly once");
        open(&w.ctx, &s, |tx| {
            tx.patch(ID, &[("pending", json!({"gen": 3}))], &[])
                .map(|_| ())
        })
        .unwrap();
        let before = std::fs::read(w.folder(ID).join("record.json")).unwrap();
        let bad = json!({"hcName": hc_name(3)});
        let got = open(&w.ctx, &s, |tx| tx.promote(ID, 3, &bad, "t3", "k:$3", None));
        assert_eq!(
            got.map_err(|e| e.check),
            Err("internal"),
            "incomplete identity"
        );
        assert_eq!(
            std::fs::read(w.folder(ID).join("record.json")).unwrap(),
            before
        );
    }

    // b-calls 13 (F9): `<agents>/<id>` is a link (`crate::testing::link_dir`: Unix symlink; Windows directory
    // symlink or junction; every OS) to a folder whose record has pending 2. promote refuses `evidence`; the target
    // record is byte-identical. The target sits outside `agents`, so the open hooks list only the link (Unreadable,
    // skipped).
    #[test]
    fn promote_never_writes_through_a_linked_member_folder() {
        use crate::agents::life_fixture::hc_name;
        let (w, s) = world();
        let launch = json!({"hcName": hc_name(2), "launcherArgv": ["/x"], "sessionId": "sid-2"});
        open(&w.ctx, &s, |tx| {
            tx.create(ID, json!({"pending": {"gen": 2}})).map(|_| ())
        })
        .unwrap();
        let real = w.folder(ID);
        let elsewhere = w.ctx.state_root.parent().unwrap().join("elsewhere");
        std::fs::rename(&real, &elsewhere).unwrap();
        crate::testing::link_dir(&elsewhere, &real);
        let before = std::fs::read(elsewhere.join("record.json")).unwrap();
        let got = open(&w.ctx, &s, |tx| {
            tx.promote(ID, 2, &launch, "t2", "k:$3", None)
        });
        assert_eq!(
            got.map_err(|e| e.check),
            Err("evidence"),
            "a linked member folder"
        );
        assert_eq!(
            std::fs::read(elsewhere.join("record.json")).unwrap(),
            before,
            "written through the link"
        );
    }
}

#[cfg(test)]
#[path = "record_write_tests.rs"]
mod record_write_tests;
