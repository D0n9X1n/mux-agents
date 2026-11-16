//! cascade-3d fixtures: a team L -> A -> B -> C of lifecycle members, generation 1 committed, parent pane PANE.
use std::path::PathBuf;

use serde_json::{Value, json};

use super::fixture::World;
use super::life_fixture as lf;
use super::team_state::{Scope, Tx};

pub const L: &str = "111111111111";
pub const A: &str = "aaaaaaaaaaa1";
pub const B: &str = "bbbbbbbbbbb1";
pub const C: &str = "ccccccccccc1";
pub const S: &str = "555555555555";
pub const PANE: &str = "%5";

/// Generation n's terminal address of member `id`.
pub fn addr_at(id: &str, n: u64) -> String {
    format!("t{n}-{id}")
}
pub fn addr(id: &str) -> String {
    addr_at(id, 1)
}
pub fn name(id: &str) -> String {
    format!("w-{}", &id[..1])
}
/// A Tx over the caller's scope with no hooks run (the cascade's own entry points are under test).
pub fn tx<'a>(w: &'a World, s: &Scope) -> Tx<'a> {
    Tx {
        ctx: &w.ctx,
        scope: s.clone(),
    }
}
/// Lifecycle member `id` under `parent` (None: a leader at depth 0), `fields` over it; generation 1 committed.
pub fn member(w: &World, id: &str, parent: Option<&str>, depth: u64, fields: Value) -> PathBuf {
    let m = w.folder(id);
    std::fs::create_dir_all(&m).unwrap();
    let mut r = json!({"lifecycle": 1, "id": id, "name": name(id), "session": w.caller().session,
        "sessionId": lf::SID, "team": lf::team(w), "depth": depth, "parentPane": PANE, "generation": "1",
        "hcAddress": addr(id), "hcName": lf::hc_name(1), "launcherArgv": lf::ARGV});
    if let Some(p) = parent {
        r["parentAgent"] = json!({"id": p});
    }
    for (k, v) in fields.as_object().unwrap() {
        r[k] = v.clone();
    }
    crate::state::fs::write_json(&m.join("record.json"), &r).unwrap();
    lf::seed_gen(&m, 1, Some("run"), lf::committed());
    m
}
/// L -> A -> B -> C.
pub fn chain(w: &World) {
    member(w, L, None, 0, json!({}));
    member(w, A, Some(L), 1, json!({}));
    member(w, B, Some(A), 2, json!({}));
    member(w, C, Some(B), 3, json!({}));
}
/// A successor of `from` whose only launch is proven void (claim `abandoned`): `never_ran_recorded` is true.
pub fn void(w: &World, id: &str, from: &str, parent: &str) {
    let m = w.folder(id);
    std::fs::create_dir_all(&m).unwrap();
    let r = json!({"lifecycle": 1, "id": id, "name": name(id), "session": w.caller().session,
        "sessionId": lf::SID, "team": lf::team(w), "depth": 1, "parentPane": PANE,
        "parentAgent": {"id": parent}, "resumedFrom": from, "pending": lf::pending(0)});
    crate::state::fs::write_json(&m.join("record.json"), &r).unwrap();
    lf::seed_gen(&m, 0, Some("abandoned"), None);
}

/// The journal file of `root` in the caller's scope.
pub fn journal_of(w: &World, root: &str) -> PathBuf {
    super::cascade::dir_at(&w.ctx, &Scope::of(&w.caller())).join(format!("{root}.json"))
}
/// A journal of `root` listing `ids` deepest first, each the parent of the one before (the last is the root).
pub fn body(root: &str, ids: &[&str], prepared: bool) -> super::cascade::Journal {
    let entries = (ids.iter().enumerate())
        .map(|(i, id)| super::cascade::Entry {
            id: id.to_string(),
            name: name(id),
            parents: ids
                .get(i + 1)
                .map(|p| vec![p.to_string()])
                .unwrap_or_default(),
            gens: vec![1],
        })
        .collect();
    super::cascade::Journal {
        root: root.into(),
        entries,
        prepared,
        removing: false,
    }
}
/// `body`, published.
pub fn seed(w: &World, root: &str, ids: &[&str], prepared: bool) -> super::cascade::Journal {
    let j = body(root, ids, prepared);
    let s = Scope::of(&w.caller());
    super::cascade::publish(&tx(w, &s), &j).unwrap();
    j
}
/// The next `release::halt(step)` refuses `halted` (the text is leaked: `halt_after` takes `&'static str`).
pub fn at(step: &str) {
    super::release::halt_after(Some(Box::leak(step.to_owned().into_boxed_str())));
}
