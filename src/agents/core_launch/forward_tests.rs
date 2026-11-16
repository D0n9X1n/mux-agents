use super::*;
use crate::agents::core_fixture as cf;
use crate::agents::fixture::{ID, SESSION_ID, World};
use crate::agents::{gens, team_state};
use crate::hc;
use crate::spawn::Out;
use std::path::{Path, PathBuf};
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
    let desired = crate::state::fs::read_json(&f.join("desired.json"))
        .ok()
        .flatten()
        .unwrap_or(Value::Null);
    json!({"pending": r["pending"]["gen"], "address": r["pending"]["hcAddress"], "session": r["sessionId"],
            "generation": r["generation"], "spec": file("spec"), "launch": file("launch"),
            "commit": commit.map(|c| format!("{:?}", gens::decide(&c))), "runs": runs,
            "desired": desired["desired"]})
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
    desired: Value,
}
fn row(r: Row) -> Value {
    json!({"pending": r.pending, "address": r.address, "session": r.session, "generation": r.generation,
            "spec": r.spec, "launch": r.launch, "commit": r.commit, "runs": r.runs, "desired": r.desired})
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
        desired: z.clone(),
    };
    // cascade-3d C1: a creating launch is desired `running` from step `desired` on.
    let desired = Row {
        desired: json!("running"),
        ..base.clone()
    };
    let spec = Row {
        spec: true,
        ..desired.clone()
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
        ("desired", row(desired)),
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

// cascade-3d C1: a launch that creates a record writes desired `running` after `pending`, before `hc run`. A
// failed desired write goes to `abort` (F1): no `hc run`, the pending generation dropped.
#[test]
fn a_new_member_is_desired_running_before_hc_run() {
    let w = World::human("");
    w.state();
    std::fs::create_dir_all(w.folder(ID).join("desired.json")).unwrap(); // a folder: Unreadable
    cf::online(&w);
    let got = go(&w, launch_of(&w, true, SESSION_ID), free, never).unwrap_err();
    assert_eq!(got.check, "evidence");
    assert!(
        w.fakes.hc_calls().iter().all(|c| c.1[0] != "run"),
        "no hc run"
    );
    assert!(
        gens::record(&w.folder(ID))
            .known()
            .unwrap()
            .is_none_or(|r| r.get("pending").is_none())
    );
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
    // cascade-3d C3: only a creating launch writes desired state; a retry never reopens a member by itself.
    assert!(
        !f.join("desired.json").exists(),
        "a retry writes no desired state"
    );
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
                crate::state::evidence::publish(&gens::path(&f2, 1, "spec"), &json!({})).unwrap();
            }
            ("pending", "spec folder") => {
                std::fs::create_dir_all(gens::path(&f2, 1, "spec")).unwrap()
            }
            ("pending", "spec content") => {
                crate::state::fs::fail_sync_file(Some(&gens::path(&f2, 1, "spec")))
            }
            ("spec", "launch exists") => {
                crate::state::evidence::publish(&gens::path(&f2, 1, "launch"), &json!({})).unwrap();
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
        "member at desired, new member",
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
            // cascade-3d C1: `desired.json` is flushed into the member folder at `desired`; armed after it,
            // the fault still meets the spec publication's member-folder flush.
            "member at desired, new member" => ("desired", f.clone()),
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

// Call 35: a record naming another member is never written. `existing`: the promoted record's id is changed,
// so the first pending write refuses. `spec`: changed after the pending write, so the address write refuses and
// `abort` owns nothing. `commit`: changed after the commit, so the promotion refuses.
#[test]
fn a_record_naming_another_id_is_never_written() {
    for case in ["existing", "spec", "commit"] {
        let w = world();
        let _reset = hooks::Reset;
        let existing = case == "existing";
        let f = w.folder(ID);
        if existing {
            promoted(&w);
            edit(&f, |r| r["id"] = json!("bbbbbbbbbbbb"));
            version(&w);
        } else {
            let f2 = f.clone();
            hooks::after(Some(Box::new(move |s: &str| {
                if s == case {
                    edit(&f2, |r| r["id"] = json!("bbbbbbbbbbbb"));
                }
            })));
            replies(&w, address("t1"));
        }
        let before = std::fs::read(f.join("record.json")).ok();
        let e = go(&w, launch_of(&w, !existing, NEW), free, never).unwrap_err();
        assert_eq!(e.check, "launch", "{case}: {e:?}");
        let r = record(&w);
        assert_eq!(
            r["id"],
            json!("bbbbbbbbbbbb"),
            "{case}: the other id stands"
        );
        match case {
            "existing" => {
                assert!(
                    e.message
                        .contains("changed before this launch was recorded"),
                    "{e:?}"
                );
                assert!(e.message.ends_with("; nothing was fenced"), "{e:?}");
                assert_eq!(
                    std::fs::read(f.join("record.json")).ok(),
                    before,
                    "nothing written"
                );
                assert!(
                    !gens::dir(&f, 1).exists() && runs(&w).len() == 1,
                    "no generation 1"
                );
            }
            "spec" => {
                assert!(
                    e.message
                        .starts_with("the launch no longer owns the record"),
                    "{e:?}"
                );
                assert!(e.message.ends_with("; nothing was fenced"), "{e:?}");
                assert_eq!(
                    decided(&w, 0),
                    None,
                    "spec: nothing fenced for another member"
                );
                assert!(
                    r["pending"].get("hcAddress").is_none(),
                    "spec: no address written"
                );
                assert!(r.get("generation").is_none(), "spec: nothing promoted");
            }
            _ => {
                assert!(e.message.contains("no longer owns the record"), "{e:?}");
                assert!(
                    r.get("generation").is_none() && r.get("sessionId").is_none(),
                    "commit: nothing promoted"
                );
                assert_eq!(
                    (decided(&w, 0), kills(&w)),
                    (Some(gens::Decision::Run), 0),
                    "nothing rolled back"
                );
            }
        }
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
                crate::state::evidence::publish(&gens::path(&f2, 1, "commit"), &winner).unwrap();
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
