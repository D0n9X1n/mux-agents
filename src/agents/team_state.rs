//! The one writer of `record.json` and `desired.json` (#103 state-0 S3), and of `view.json` (S4).
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

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
        format!("gen|{}|{id}.{n}", self.key)
    }
}

/// A replay hook open runs before its body (adopt-3e registers its journal, release-1d gens::complete).
pub type Hook = fn(&Tx<'_>) -> Res<()>;
const HOOKS: &[Hook] = &[];

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

impl Tx<'_> {
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

    /// desired.json holds desired and at, written as patch writes.
    pub fn desired(&self, id: &str, running: bool) -> Res<()> {
        let path = self.folder(id).join("desired.json");
        let word = if running { "running" } else { "stopped" };
        let body = json!({"desired": word, "at": utc_o(self.ctx.clock.now())});
        super::lock(self.ctx, &self.scope.member_lock(id), || {
            // Reread under the lock, as patch does: an unreadable file is refused, never replaced.
            evidence::read(&path).known()?;
            write_json_durable(&path, &body)
        })
    }
}

/// The presentation fields view.json carries (views-6f V1).
pub const VIEW_FIELDS: [&str; 3] = ["pane", "window", "startCommand"];

/// S4: the member lock only; replaces view.json, never touches record.json.
pub fn view(ctx: &Ctx, scope: &Scope, id: &str, view: &Value) -> Res<()> {
    let path = scope.agents(&ctx.state_root).join(id).join("view.json");
    super::lock(ctx, &scope.member_lock(id), || write_json(&path, view))
}

/// A lifecycle record with its view.json fields over it, for readers that still take a pane from the record;
/// a pre-lifecycle record is returned as it is.
pub fn presented(folder: &Path, mut record: Value) -> Res<Value> {
    if !super::gens::lifecycle(&record) {
        return Ok(record);
    }
    if let Some(v) = evidence::read(&folder.join("view.json")).known()? {
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
        open(&w.ctx, &s, |tx| tx.create(ID, json!({"parentAgent": null}))).unwrap();
        let folder = w.folder(ID);
        let seen = record(&w);
        let parent = json!("bbbbbbbbbbbb");
        open(&w.ctx, &s, |tx| {
            tx.patch(ID, &[("parentAgent", parent.clone())], &[])
        })
        .unwrap();
        let before = std::fs::read(folder.join("record.json")).unwrap();
        let v = json!({"pane": "P9", "window": "W5", "startCommand": "hc attach t1"});
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
        test_hook(Some(hook));
        let got = open(&w.ctx, &s, |_| Ok(()));
        test_hook(None);
        got.unwrap();
        let t = s.team_lock();
        let want = [
            format!("+{t}"),
            "+hook".into(),
            "-hook".into(),
            format!("-{t}"),
        ];
        assert_eq!(w.fakes.locks(), want);
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
        view(&w.ctx, &s, ID, &json!({"pane": "P1"})).unwrap();
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
}
