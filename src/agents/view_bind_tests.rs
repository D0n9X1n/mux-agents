//! views-6f V1/V2 through the shared readers: `presented` (A3c), `Identity::view`, `Identity::search`.
use super::core_identity::Key;
use super::fixture::{World, write};
use super::life_fixture as lf;
use super::registry::Row;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub(crate) fn read(p: &Path) -> Value {
    crate::state::fs::read_json(p).unwrap().unwrap()
}

/// Generations 0 (`t0`) and 1 (`t1`), both committed; the record names 1 (restore-6b's n+1).
pub(crate) fn promoted(w: &World) -> PathBuf {
    let f = lf::life(
        w,
        json!({"generation": "1", "hcAddress": "t1", "hcName": lf::hc_name(1),
        "launcherArgv": lf::ARGV, "directory": w.ctx.cwd, "context": "brief", "reason": "human"}),
    );
    for n in [0, 1] {
        lf::seed_gen(&f, n, None, lf::committed());
    }
    f
}

/// The fake hc binary (installed here), as `attachment.rs` runs it in a view pane and `view.json` records it.
pub(crate) fn hc_bin(w: &World) -> String {
    w.fakes.hc_install("hc").display().to_string()
}

/// What mux-agents starts in a view pane: the hc program, quoted, then `attach <address>`.
pub(crate) fn hc_attach(w: &World, address: &str) -> String {
    format!("\"{}\" attach {address}", hc_bin(w))
}

/// `view.json` for generation n at `pane` in `@5`, attaching `address`, named `hc_name`, under the caller's key, with
/// the program it runs (v6f call 13).
pub(crate) fn view_file(w: &World, f: &Path, n: u64, pane: &str, address: &str, hc_name: &str) {
    let c = w.caller();
    write(
        &f.join("view.json"),
        json!({"serverKey": c.server_key, "session": c.session, "pane": pane,
        "window": "@5", "startCommand": hc_attach(w, address), "attachProgram": hc_bin(w), "gen": n,
        "hcAddress": address, "hcName": hc_name}),
    );
}

pub(crate) fn row(w: &World, pane: &str, address: &str) -> Row {
    Row {
        pane: pane.into(),
        window: "@5".into(),
        pid: "300".into(),
        dead: false,
        command: hc_attach(w, address),
        tag: String::new(),
        name: String::new(),
    }
}

/// `list-panes` answers these `(pane, address)` rows in `@5`, each running `<hc> attach <address>`; `dead` panes
/// have `#{pane_dead}` 1. (`w.panes` answers launcher commands, never an attach.)
pub(crate) fn attach_panes(w: &World, rows: &[(&str, &str)], dead: &[&str]) {
    let rows: Vec<super::fixture::Listed> = rows
        .iter()
        .map(|(pane, address)| super::fixture::Listed {
            pane: pane.to_string(),
            window: "@5".into(),
            pid: "300".into(),
            dead: dead.contains(pane),
            command: hc_attach(w, address),
            tag: String::new(),
            name: String::new(),
        })
        .collect();
    w.fakes.mux_script("list-panes", move |argv| {
        Ok(crate::mux::Output {
            code: 0,
            stdout: super::fixture::listed_rows(argv, &rows, "\n"),
            stderr: String::new(),
        })
    });
}

/// `locals` then `view` under the caller's key: the predicate every shared reader applies.
fn seen(w: &World, rows: &[Row]) -> Option<String> {
    let l = super::membership::locals(&w.ctx, &w.caller())
        .unwrap()
        .remove(0);
    l.identity
        .view(&l.stored.record, rows, &Key::of(&w.caller()))
        .map(|p| p.pane)
}

/// The caller's own view of generation 1 at `%12`, then the same descriptor made on another server.
fn own_then_foreign(w: &World, f: &Path) -> Value {
    view_file(w, f, 1, "%12", "t1", &lf::hc_name(1));
    let mut v = read(&f.join("view.json"));
    v["serverKey"] = json!("0123456789abcdef");
    v
}

// V1 "board and capture refuse n's descriptor for n+1"; each field alone (audit 1-5 F14: `gen` alone).
#[test]
fn the_shared_readers_refuse_the_last_generations_view() {
    let w = World::human("");
    let f = promoted(&w);
    let rows = [row(&w, "%9", "t0"), row(&w, "%12", "t1")];
    view_file(&w, &f, 0, "%12", "t1", &lf::hc_name(1));
    assert_eq!(
        seen(&w, &rows),
        None,
        "gen alone: generation 0's descriptor on generation 1's pane"
    );
    view_file(&w, &f, 0, "%9", "t0", &lf::hc_name(0));
    assert_eq!(seen(&w, &rows), None, "generation 0's whole view");
    view_file(&w, &f, 1, "%12", "t1", &lf::hc_name(1));
    assert_eq!(
        seen(&w, &rows),
        Some("%12".to_string()),
        "generation 1's own view"
    );
}

// Ambiguity 2: the key comes only from view.json; a record's own copies are stripped.
#[test]
fn a_record_never_forges_the_view_key() {
    let w = World::human("");
    let f = promoted(&w);
    let c = w.caller();
    let mut r = lf::record(&f);
    for (k, v) in [
        ("viewServerKey", json!(c.server_key)),
        ("viewSession", json!(c.session)),
        ("pane", json!("%12")),
    ] {
        r[k] = v;
    }
    write(&f.join("record.json"), r);
    let l = super::membership::locals(&w.ctx, &c).unwrap().remove(0);
    let shown: Vec<Option<Value>> = ["viewServerKey", "viewSession", "pane"]
        .map(|k| l.stored.record.get(k).cloned())
        .into();
    assert_eq!(
        shown,
        vec![None, None, None],
        "no view.json: nothing presented, the forged key stripped"
    );
}

// Audit 1-5 F9, 6-10 F11: the board's own snapshot (`Snapshot::take`, `assemble`) refuses a foreign key.
#[test]
fn the_board_snapshot_refuses_a_foreign_key() {
    let w = World::human("");
    let f = promoted(&w);
    attach_panes(&w, &[("%12", "t1")], &[]);
    let foreign = own_then_foreign(&w, &f);
    let views = |w: &World| {
        super::membership::Snapshot::take(&w.ctx, &w.caller())
            .unwrap()
            .views
    };
    assert!(
        views(&w).contains_key(super::fixture::ID),
        "the caller's own descriptor"
    );
    write(&f.join("view.json"), foreign);
    assert!(views(&w).is_empty(), "a descriptor made on another server");
}

// Audit 6-10 F11: the bar's own reader (`view_windows`) refuses a foreign key.
#[test]
fn the_bars_view_windows_refuse_a_foreign_key() {
    let w = World::human("");
    let f = promoted(&w);
    attach_panes(&w, &[("%12", "t1")], &[]);
    let foreign = own_then_foreign(&w, &f);
    assert_eq!(
        crate::sidebar::view_windows(&w.ctx, &w.caller()),
        Some(vec!["@5".to_string()])
    );
    write(&f.join("view.json"), foreign);
    assert_eq!(
        crate::sidebar::view_windows(&w.ctx, &w.caller()),
        Some(Vec::new()),
        "a descriptor made on another server"
    );
}

// Call 18 (checklist 2), caller level: a recorded address that is not one plain token ("t1 extra") is never a view,
// even when the pane runs exactly `<hc> attach t1 extra` and every other field is generation 1's.
#[test]
fn a_malformed_recorded_address_is_never_a_view() {
    let w = World::human("");
    let f = promoted(&w);
    let mut r = lf::record(&f);
    r["hcAddress"] = json!("t1 extra");
    write(&f.join("record.json"), r);
    view_file(&w, &f, 1, "%12", "t1 extra", &lf::hc_name(1));
    assert_eq!(
        read(&f.join("view.json"))["startCommand"],
        json!(hc_attach(&w, "t1 extra"))
    );
    assert_eq!(seen(&w, &[row(&w, "%12", "t1 extra")]), None);
}
