//! views-6f V1/V2 through the shared readers: `presented` (A3c), `Identity::view`, `Identity::search`.
use super::core_identity::Key;
use super::fixture::{ID, World, write};
use super::life_fixture as lf;
use super::registry::Row;
use super::view::{Focused, Member};
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

/// `select_view`'s read-back (`#{window_active}`, never the caller's own endpoint probe) answers `<pane>|1|1|1` for
/// the pane it names: every select is proven (audit 1-5 F5).
pub(crate) fn select_ok(w: &World) {
    w.fakes.mux_script("#{window_active}", |argv| {
        let at = argv.iter().position(|a| a == "-t").unwrap();
        Ok(crate::mux::Output {
            code: 0,
            stdout: format!("{}|1|1|1\n", argv[at + 1]),
            stderr: String::new(),
        })
    });
}

/// `focus_member` without hc (`core: None`): only a found view is focused.
fn focus(w: &World) -> Focused {
    match super::view::focus_member(&w.ctx, None, &w.caller(), ID, None).unwrap() {
        Member::Focused(f) => f,
        other => panic!("not focused: {other:?}"),
    }
}

fn aimed_at_old(w: &World) -> Vec<Vec<String>> {
    w.fakes
        .mux_calls()
        .into_iter()
        .filter(|c| c.iter().any(|t| t == "%8"))
        .collect()
}

fn new_windows(w: &World) -> usize {
    w.fakes
        .mux_calls()
        .into_iter()
        .filter(|c| c.iter().any(|t| t == "new-window"))
        .count()
}

// Spec "Generation binding": n's view stays open on its released shell; n+1 has none; focus opens a new view for
// n+1, never selects or counts n's pane and sends it nothing.
#[test]
fn focus_opens_a_new_view_for_the_next_generation_and_leaves_the_old_pane() {
    let w = World::human("");
    let f = promoted(&w);
    view_file(&w, &f, 0, "%8", "t0", &lf::hc_name(0));
    attach_panes(&w, &[("%8", "t0")], &[]);
    w.launches_hold();
    w.fakes.mux_rule(
        "new-window",
        0,
        &format!("%19|@8|{}\n", hc_attach(&w, "t1")),
        "",
    );
    w.fakes.mux_rule("#{window_active}", 0, "%19|1|1|1\n", "");
    super::core_fixture::online(&w);
    let core = crate::hc::connect(&w.ctx).unwrap();
    let got = super::view::focus_member(&w.ctx, Some(&core), &w.caller(), ID, None).unwrap();
    let Member::Focused(new) = got else {
        panic!("{got:?}")
    };
    assert_eq!(
        (new.pane.as_str(), new.duplicates),
        ("%19", 0),
        "a new view; n's pane is never counted"
    );
    let v = read(&f.join("view.json"));
    assert_eq!(
        (&v["gen"], &v["pane"], &v["hcAddress"]),
        (&json!(1), &json!("%19"), &json!("t1"))
    );
    assert_eq!(
        aimed_at_old(&w),
        Vec::<Vec<String>>::new(),
        "nothing is aimed at n's pane"
    );
    w.fakes.hc_done().unwrap();
}

// Spec: capture refuses n's descriptor for n+1. Only `gen` differs (audit 1-5 F10): pane, address, hc name and key
// are generation 1's, so only generation ownership refuses it.
#[test]
fn capture_refuses_the_last_generations_view() {
    let w = World::human("");
    let f = promoted(&w);
    view_file(&w, &f, 0, "%12", "t1", &lf::hc_name(1));
    attach_panes(&w, &[("%12", "t1")], &[]);
    w.fakes.clear_log();
    let e = super::message::capture_member(&w.ctx, &w.caller(), ID, None, 50).unwrap_err();
    assert_eq!(
        (e.code, e.check, e.message.as_str()),
        (2, "target", "the agent has no live view; run focus worker")
    );
    let captured = w
        .fakes
        .mux_calls()
        .into_iter()
        .filter(|c| c.iter().any(|t| t == "capture-pane"))
        .count();
    assert_eq!(captured, 0, "the pane is never read");
}

// Spec "Focus dies after attach, before view.json": the pane attaching n+1's address exists with no descriptor; the
// next focus finds it by V2, writes view.json once, and opens no second pane. The record's own hcName is stale
// (generation 0's): the adopted descriptor takes the active launch.json's (v6f call 6, audit 1-5 F3).
#[test]
fn a_pane_attached_before_a_crash_is_adopted_once() {
    let w = World::human("");
    let f = promoted(&w);
    let mut r = lf::record(&f);
    r["hcName"] = json!(lf::hc_name(0));
    write(&f.join("record.json"), r);
    attach_panes(&w, &[("%8", "t0"), ("%12", "t1")], &[]);
    select_ok(&w);
    let first = focus(&w);
    assert_eq!((first.pane.as_str(), first.duplicates), ("%12", 0));
    let mut v = read(&f.join("view.json"));
    let c = w.caller();
    assert_eq!(
        v,
        json!({"serverKey": c.server_key, "session": c.session, "pane": "%12", "window": "@5",
        "startCommand": hc_attach(&w, "t1"), "attachProgram": hc_bin(&w), "gen": 1, "hcAddress": "t1",
        "hcName": lf::hc_name(1)})
    );
    v["probe"] = json!(1);
    write(&f.join("view.json"), v.clone());
    let second = focus(&w);
    assert_eq!(second.pane, "%12", "the saved view is reused");
    assert_eq!(
        read(&f.join("view.json")),
        v,
        "a valid descriptor is never written again"
    );
    assert_eq!(new_windows(&w), 0, "no second pane");
    assert!(aimed_at_old(&w).is_empty(), "n's pane is never selected");
}

// V2: of several panes attaching the active address, the descriptor's is kept and the rest counted; without a valid
// descriptor the lowest numeric id is adopted. Ids are listed out of order, and their numeric and lexical orders
// differ (audit 1-5 F11): %30, %100, %9, %10 (dead), %8 (generation 0's).
#[test]
fn the_descriptors_pane_is_kept_and_the_rest_counted() {
    let w = World::human("");
    let f = promoted(&w);
    view_file(&w, &f, 1, "%100", "t1", &lf::hc_name(1));
    attach_panes(
        &w,
        &[
            ("%30", "t1"),
            ("%100", "t1"),
            ("%9", "t1"),
            ("%10", "t1"),
            ("%8", "t0"),
        ],
        &["%10"],
    );
    select_ok(&w);
    let before = std::fs::read(f.join("view.json")).unwrap();
    let kept = focus(&w);
    assert_eq!(
        (kept.pane.as_str(), kept.duplicates),
        ("%100", 2),
        "descriptor kept; %30, %9 counted; %10 dead"
    );
    assert_eq!(
        std::fs::read(f.join("view.json")).unwrap(),
        before,
        "nothing written"
    );
    std::fs::remove_file(f.join("view.json")).unwrap();
    let adopted = focus(&w);
    assert_eq!(
        (adopted.pane.as_str(), adopted.duplicates),
        ("%9", 2),
        "the lowest numeric id is adopted"
    );
    assert_eq!(read(&f.join("view.json"))["pane"], "%9");
}

// V1: a view.json without gen is never accepted; focus adopts the attaching pane and writes a whole descriptor.
#[test]
fn a_view_without_gen_is_never_presented_and_focus_replaces_it() {
    let w = World::human("");
    let f = promoted(&w);
    view_file(&w, &f, 1, "%12", "t1", &lf::hc_name(1));
    let mut v = read(&f.join("view.json"));
    v.as_object_mut().unwrap().remove("gen");
    write(&f.join("view.json"), v);
    assert_eq!(
        seen(&w, &[row(&w, "%12", "t1")]),
        None,
        "no gen: never a view"
    );
    attach_panes(&w, &[("%12", "t1")], &[]);
    select_ok(&w);
    assert_eq!(focus(&w).pane, "%12");
    assert_eq!(
        read(&f.join("view.json"))["gen"],
        json!(1),
        "replaced by a whole descriptor"
    );
}

// Ambiguity 6: the focus reply names duplicates only when there are some.
#[test]
fn the_focus_reply_names_duplicates_only_when_there_are_some() {
    let f = |duplicates| Focused {
        id: ID.into(),
        name: "worker".into(),
        pane: "%12".into(),
        window: "@5".into(),
        duplicates,
    };
    assert!(
        super::view::focus_reply(f(0)).get("duplicates").is_none(),
        "unchanged reply"
    );
    assert_eq!(super::view::focus_reply(f(2))["duplicates"], json!(2));
}

// v6f call 16 (checklist 5): only an absent view.json sends focus to the search. A torn one, one of the wrong JSON
// type and a folder in its place refuse `evidence` beside a live attaching pane and create nothing.
#[test]
fn a_damaged_view_file_refuses_evidence_and_creates_nothing() {
    for case in ["torn", "array", "folder"] {
        let w = World::human("");
        let f = promoted(&w);
        let at = f.join("view.json");
        match case {
            "torn" => std::fs::write(&at, b"{").unwrap(),
            "array" => std::fs::write(&at, b"[]").unwrap(),
            _ => std::fs::create_dir(&at).unwrap(),
        }
        attach_panes(&w, &[("%12", "t1")], &[]);
        w.launches_hold();
        select_ok(&w);
        w.fakes.mux_rule(
            "new-window",
            0,
            &format!("%19|@8|{}\n", hc_attach(&w, "t1")),
            "",
        );
        super::core_fixture::online(&w);
        let core = crate::hc::connect(&w.ctx).unwrap();
        let e =
            super::view::focus_member(&w.ctx, Some(&core), &w.caller(), ID, None).expect_err(case);
        assert_eq!((e.code, e.check), (1, "evidence"), "{case}: {e:?}");
        assert_eq!(new_windows(&w), 0, "{case}: nothing created");
        assert!(
            at.is_dir() || std::fs::read(&at).unwrap().len() <= 2,
            "{case}: nothing written"
        );
    }
}

// Call 18 (checklist 2), adoption: a recorded address that is not one plain token ("t1 extra") is never adopted,
// even when a live pane runs exactly `<hc> attach t1 extra`; no view.json is written.
#[test]
fn a_malformed_recorded_address_is_never_adopted() {
    let w = World::human("");
    let f = promoted(&w);
    let mut r = lf::record(&f);
    r["hcAddress"] = json!("t1 extra");
    write(&f.join("record.json"), r);
    attach_panes(&w, &[("%12", "t1 extra")], &[]);
    select_ok(&w);
    let got = super::view::focus_member(&w.ctx, None, &w.caller(), ID, None).unwrap();
    assert!(matches!(got, Member::NoView), "{got:?}");
    assert!(!f.join("view.json").exists(), "nothing adopted");
}

/// `new-window` answers `%19` in `@5` running `command`; from then on `list-panes` shows it (a live multiplexer).
fn one_window(w: &World, command: String) {
    let made = std::rc::Rc::new(std::cell::Cell::new(false));
    let (m, c) = (made.clone(), command.clone());
    w.fakes.mux_script("new-window", move |_| {
        m.set(true);
        Ok(crate::mux::Output {
            code: 0,
            stdout: format!("%19|@5|{c}\n"),
            stderr: String::new(),
        })
    });
    w.fakes.mux_script("list-panes", move |argv| {
        let rows: Vec<super::fixture::Listed> = if made.get() {
            vec![super::fixture::Listed {
                pane: "%19".into(),
                window: "@5".into(),
                pid: "300".into(),
                dead: false,
                command: command.clone(),
                tag: String::new(),
                name: String::new(),
            }]
        } else {
            Vec::new()
        };
        Ok(crate::mux::Output {
            code: 0,
            stdout: super::fixture::listed_rows(argv, &rows, "\n"),
            stderr: String::new(),
        })
    });
}

// v6f call 5: the launch paused after its promotion; a focus runs and creates the view; the launch resumes: it adopts
// that view. Exactly one `new-window`; view.json names its pane. Reversed: the launch creates, focus selects it.
#[test]
fn a_launch_and_a_focus_create_one_view_in_either_order() {
    for focus_first in [true, false] {
        let w = World::human("");
        let f = promoted(&w);
        one_window(&w, hc_attach(&w, "t1"));
        w.launches_hold();
        select_ok(&w);
        super::core_fixture::online(&w);
        let core = crate::hc::connect(&w.ctx).unwrap();
        let record = lf::record(&f);
        let launch =
            || super::view::launch_view(&w.ctx, &core, &w.caller(), &record, None).unwrap();
        let focused = || match super::view::focus_member(&w.ctx, Some(&core), &w.caller(), ID, None)
            .unwrap()
        {
            Member::Focused(x) => x,
            other => panic!("{other:?}"),
        };
        let (from_launch, from_focus) = if focus_first {
            let x = focused();
            (launch(), x)
        } else {
            let v = launch();
            (v, focused())
        };
        assert_eq!(new_windows(&w), 1, "focus_first {focus_first}: one view");
        assert_eq!(
            (&from_launch["pane"], from_focus.pane.as_str()),
            (&json!("%19"), "%19"),
            "focus_first {focus_first}"
        );
        assert_eq!(
            read(&f.join("view.json"))["pane"],
            "%19",
            "focus_first {focus_first}"
        );
        w.fakes.hc_done().unwrap();
    }
}

// v6f call 13 (re-audit 1-3 F2): a view made with only `herdr-core` installed stays the view after `hc` is installed:
// the recorded program decides, never what `PATH` resolves today. Focus opens no `new-window` and (checklist 7)
// leaves view.json byte-identical: a sentinel field shows it is never rewritten by an adoption.
#[test]
fn a_view_stays_its_own_after_another_hc_is_installed() {
    let w = World::human("");
    let f = promoted(&w);
    let herdr = w.fakes.hc_install("herdr-core").display().to_string();
    one_window(&w, format!("\"{herdr}\" attach t1"));
    w.launches_hold();
    select_ok(&w);
    super::core_fixture::answer(&w, &["--version"], "hc 0.1.21");
    let core = crate::hc::connect(&w.ctx).unwrap();
    let v = super::view::launch_view(&w.ctx, &core, &w.caller(), &lf::record(&f), None).unwrap();
    assert_eq!(
        v["attachProgram"],
        json!(herdr),
        "the program the launch ran"
    );
    let mut sentinel = read(&f.join("view.json"));
    sentinel["probe"] = json!(1);
    write(&f.join("view.json"), sentinel);
    let before = std::fs::read(f.join("view.json")).unwrap();
    w.fakes.hc_install("hc");
    let opened = new_windows(&w);
    let got = focus(&w);
    assert_eq!(
        (got.pane.as_str(), new_windows(&w) - opened),
        ("%19", 0),
        "the recorded program: no second view"
    );
    assert_eq!(
        std::fs::read(f.join("view.json")).unwrap(),
        before,
        "the descriptor stands"
    );
}

// v6f call 16 (checklist 5), the launch's path: a torn view.json beside the promoted member refuses `evidence`; the
// launch creates nothing and leaves the file as it was.
#[test]
fn a_launch_beside_a_torn_view_file_creates_nothing() {
    let w = World::human("");
    let f = promoted(&w);
    std::fs::write(f.join("view.json"), b"{").unwrap();
    one_window(&w, hc_attach(&w, "t1"));
    w.launches_hold();
    super::core_fixture::online(&w);
    let core = crate::hc::connect(&w.ctx).unwrap();
    let e =
        super::view::launch_view(&w.ctx, &core, &w.caller(), &lf::record(&f), None).unwrap_err();
    assert_eq!((e.code, e.check), (1, "evidence"), "{e:?}");
    assert_eq!(new_windows(&w), 0, "nothing created");
    assert_eq!(std::fs::read(f.join("view.json")).unwrap(), b"{");
}
