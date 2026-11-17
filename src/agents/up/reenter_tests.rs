//! reentry-6a E4-E5: every Running member of the anchor's session gets its view in the view's session; none
//! Running: a (reused) status window; the leader's view is selected, else the oldest Running member's.
use super::reenter::{self, Running};
use crate::agents::core_fixture as cf;
use crate::agents::fixture::World;
use crate::agents::life_fixture as lf;
use crate::agents::reconcile_tests as rt;
use crate::agents::up::tests as ut;
use serde_json::{Value, json};

const L: &str = "cccccccccccc";
const X: &str = "aaaaaaaaaaa1";
const Y: &str = "aaaaaaaaaaa2";

pub(crate) fn verbs(w: &World, v: &str) -> Vec<Vec<String>> {
    w.fakes
        .mux_calls()
        .into_iter()
        .filter(|c| c.iter().any(|a| a == v))
        .collect()
}
/// A lifecycle member `id` at the World key in its session: `rt::member` seeds generation 1 launched, claimed and
/// committed; ready and `ok` (checklist #36, N4: record, evidence and hc answers all generation 1); attachment's
/// `context` and `reason` tags set.
fn running_member(w: &World, id: &str, name: &str) -> (std::path::PathBuf, Running) {
    let f = rt::member(
        w,
        id,
        json!({"name": name, "context": "brief", "reason": "human"}),
    );
    rt::ok(&f, 1);
    let record = lf::record(&f);
    (
        f,
        Running {
            id: id.into(),
            record,
        },
    )
}
/// Every `new-window` opens pane `%2<n>` in window `@2<n>` attaching the address it was given; only the attach
/// proof (`#{pane_id}|#{==:..}`, checklist #35, N3) answers the new pane; caller and server probes are untouched.
pub(crate) fn answers_views(w: &World) {
    let n = std::rc::Rc::new(std::cell::Cell::new(0u32));
    w.fakes.mux_script("new-window", move |args| {
        n.set(n.get() + 1);
        let (i, a) = (20 + n.get(), args.last().cloned().unwrap_or_default());
        let out = format!("%{i}|@{i}|hc attach {a}{}", char::from(10u8));
        Ok(crate::mux::Output {
            code: 0,
            stdout: out,
            stderr: String::new(),
        })
    });
    w.fakes.mux_script("#{pane_id}|#{==:", |args| {
        let at = args
            .iter()
            .position(|a| a == "-t")
            .map(|i| args[i + 1].clone())
            .unwrap_or_default();
        Ok(crate::mux::Output {
            code: 0,
            stdout: format!("{at}|1{}", char::from(10u8)),
            stderr: String::new(),
        })
    });
}

// E4.4, ambiguity 12: L's pane already attaches its address (kept; a second one counted); X has none (a new view in
// the view's session). Under member locks; nothing selected, killed or sent to the duplicate.
#[test]
fn views_are_found_or_opened_in_the_view_session() {
    let w = World::human("");
    let (_, l) = running_member(&w, L, "leader");
    let (_, x) = running_member(&w, X, "x");
    let al = format!("a{L}");
    crate::agents::view_bind_tests::attach_panes(
        &w,
        &[("%4", al.as_str()), ("%5", al.as_str())],
        &[],
    );
    answers_views(&w);
    cf::online(&w);
    let core = crate::hc::connect(&w.ctx).unwrap();
    let got = reenter::views(&w.ctx, &w.caller(), &core, &[&l, &x]);
    let panes: Vec<&str> = got
        .shown
        .iter()
        .map(|v| v["pane"].as_str().unwrap())
        .collect();
    assert_eq!(panes, ["%4", "%21"], "L kept at its lowest pane; X opened");
    let windows: Vec<&str> = got
        .shown
        .iter()
        .map(|v| v["window"].as_str().unwrap())
        .collect();
    assert_eq!(
        windows,
        ["@5", "@21"],
        "each view's own window (checklist #33, N1)"
    );
    assert_eq!(
        (got.duplicates, got.error.as_deref()),
        (1, None),
        "V2: the second pane attaching L counted"
    );
    assert!(
        verbs(&w, "select-window").is_empty() && verbs(&w, "select-pane").is_empty(),
        "never selected here (row 11.1)"
    );
    assert!(
        w.fakes
            .locks()
            .iter()
            .any(|k| k.starts_with("+member|") && k.ends_with(L)),
        "{:?}",
        w.fakes.locks()
    );
    assert!(
        !w.fakes
            .mux_calls()
            .iter()
            .any(|c| c.iter().any(|a| a == "%5")),
        "the duplicate is left alone"
    );
}

// Finding 11-14 #13, #14: a session whose placement is `h` or `v`: re-entry has no pane to split from, so each view
// is a new window, never a split, and never selected.
#[test]
fn reentry_opens_new_windows_whatever_the_session_placement() {
    for placement in ["h", "v"] {
        let w = World::human("");
        w.fakes.mux_first(
            "@mux-agents-placement",
            0,
            &format!("{placement}{}", char::from(10u8)),
            "",
        );
        let (_, x) = running_member(&w, X, "x");
        answers_views(&w);
        cf::online(&w);
        let core = crate::hc::connect(&w.ctx).unwrap();
        let got = reenter::views(&w.ctx, &w.caller(), &core, &[&x]);
        assert_eq!((got.shown.len(), got.error), (1, None), "{placement}");
        assert_eq!(
            (
                verbs(&w, "new-window").len(),
                verbs(&w, "split-window").len()
            ),
            (1, 0),
            "{placement}: a window"
        );
        assert!(
            verbs(&w, "select-window").is_empty() && verbs(&w, "select-pane").is_empty(),
            "{placement}"
        );
    }
}

// E4.5, ambiguity 13, calls 28, 40: no Running member: a status window (no command) created by id, then marked by id
// (checklist #46, N15); a marked one is reused; an unmarked `status` (a crash between the two calls) is adopted.
#[test]
fn no_running_member_opens_a_status_window_once() {
    let nl = char::from(10u8);
    let w = World::human("");
    w.fakes
        .mux_rule("list-windows", 0, &format!("@1|zsh|{nl}"), "");
    w.fakes.mux_rule("new-window", 0, "@8", "");
    assert_eq!(reenter::status_window(&w.ctx, &w.caller()).unwrap(), "@8");
    let call = &verbs(&w, "new-window")[0];
    assert!(
        call.iter().any(|a| a == "status") && call.iter().any(|a| a == "$3:"),
        "{call:?}"
    );
    assert!(
        !call.iter().any(|a| a.contains("attach") || a == "hold"),
        "no command: {call:?}"
    );
    let mark = verbs(&w, crate::sidebar::STATUS);
    assert!(
        mark.iter()
            .any(|c| c.iter().any(|a| a == "@8") && c.iter().any(|a| a == "set-option")),
        "marked by id: {mark:?}"
    );
    let w = World::human("");
    w.fakes.mux_rule(
        "list-windows",
        0,
        &format!("@1|zsh|{nl}@8|renamed|1{nl}"),
        "",
    );
    assert_eq!(
        reenter::status_window(&w.ctx, &w.caller()).unwrap(),
        "@8",
        "reused"
    );
    assert!(
        verbs(&w, "new-window").is_empty(),
        "no second status window"
    );
    let w = World::human("");
    w.fakes
        .mux_rule("list-windows", 0, &format!("@1|zsh|{nl}@9|status|{nl}"), "");
    assert_eq!(
        reenter::status_window(&w.ctx, &w.caller()).unwrap(),
        "@9",
        "an unmarked status window adopted"
    );
    assert!(
        verbs(&w, "new-window").is_empty()
            && verbs(&w, "set-option")
                .iter()
                .any(|c| c.iter().any(|a| a == "@9")),
        "and marked"
    );
}

// E4.6, ambiguity 14: the leader when Running; else the earliest first start (unreadable last), ties by id.
#[test]
fn the_leader_else_the_oldest_running_member_is_chosen() {
    let at = |s: &str| Some(s.to_string());
    let both = [
        (X.to_string(), at("2026-11-16T10:00:00Z")),
        (Y.to_string(), at("2026-11-16T09:00:00Z")),
    ];
    assert_eq!(
        reenter::choose(Some(L), &[(L.into(), None), both[0].clone()]),
        Some(L.to_string()),
        "the leader"
    );
    assert_eq!(
        reenter::choose(Some(L), &both),
        Some(Y.to_string()),
        "leader not Running: the oldest"
    );
    assert_eq!(
        reenter::choose(
            None,
            &[(X.into(), None), (Y.into(), at("2026-11-16T09:00:00Z"))]
        ),
        Some(Y.to_string()),
        "unreadable last"
    );
    assert_eq!(
        reenter::choose(None, &[(Y.into(), None), (X.into(), None)]),
        Some(X.to_string()),
        "ties by id"
    );
    assert_eq!(
        reenter::choose(Some(L), &[]),
        None,
        "none: the status window"
    );
}

// E4.4 input, call 7, findings #8, #9: Running is decided by hc for the anchor session's lifecycle records with an
// active generation; the leader this call launched is Running without hc; another session's record, a void member
// and a pre-lifecycle folder are never asked about and never counted.
#[test]
fn running_members_are_the_anchor_sessions_classified_by_hc() {
    let w = World::human("");
    let (fx, _) = running_member(&w, X, "x");
    running_member(&w, Y, "y");
    running_member(&w, L, "leader");
    let other = rt::member(&w, "aaaaaaaaaaa9", json!({"name": "z", "session": "$9"}));
    rt::ok(&other, 1);
    // Checklist #36: the void member has no active generation, only an abandoned pending one.
    let void = rt::member(&w, "aaaaaaaaaaa5", json!({"name": "v"}));
    let mut r = lf::record(&void);
    r.as_object_mut().unwrap().remove("generation");
    r["pending"] = lf::pending(2);
    crate::state::fs::write_json(&void.join("record.json"), &r).unwrap();
    lf::seed_gen(&void, 2, Some("abandoned"), None);
    let pre = w
        .ctx
        .state_root
        .join(w.caller().server_key)
        .join("agents")
        .join("999999999999");
    std::fs::create_dir_all(&pre).unwrap();
    crate::state::fs::write_json(
        &pre.join("record.json"),
        &json!({"id": "999999999999", "name": "old", "session": "$3"}),
    )
    .unwrap();
    let before = std::fs::read(pre.join("record.json")).unwrap();
    cf::online(&w);
    rt::found(&w, X, 1, &ut::create::running_wire(&w, &fx, X, 1));
    rt::found(&w, Y, 1, &rt::wire_of(&w, Y, 1, lf::LOST_ARGVS[0]));
    let lazy = crate::hc::Lazy::new(&w.ctx);
    let scope = crate::agents::scope::state(&w.caller());
    let (running, members) = reenter::running(&w.ctx, &lazy, &scope, Some(L)).unwrap();
    let ids: Vec<&str> = running.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        ids,
        [X, L],
        "by id: the Running X and the launched leader; Lost Y has no view"
    );
    let lives: Vec<(String, String)> = members
        .iter()
        .map(|m| {
            (
                m["id"].as_str().unwrap().into(),
                m["life"].as_str().unwrap().into(),
            )
        })
        .collect();
    let want: Vec<(String, String)> = [(X, "running"), (Y, "lost"), (L, "running")]
        .iter()
        .map(|(i, l)| (i.to_string(), l.to_string()))
        .collect();
    assert_eq!(
        lives, want,
        "E5 members: no $9 record (call 7), no void member (finding #9)"
    );
    assert_eq!(
        std::fs::read(pre.join("record.json")).unwrap(),
        before,
        "prelife-1c G10: untouched"
    );
    w.fakes.hc_done().unwrap();
}

const K1: &str = "a1a1a1a1a1a1a1a1";

/// A person in `project.v1` whose `up` re-enters: hc reachable, the bar on, the World server proven, `new-session`
/// answering `$7 @3`, and every new view answered by `answers_views`.
fn reentry_world() -> World {
    let mut w = World::human("");
    w.ctx.cwd = w._dir.path().join("project.v1");
    std::fs::create_dir_all(&w.ctx.cwd).unwrap();
    cf::online(&w); // up's connect
    w.fakes.mux_first("@mux-agents-sidebar", 0, "on", "");
    w.fakes.mux_rule("new-session", 0, "$7 @3\n", "");
    w.fakes.mux_rule("list-sessions", 0, "", "");
    w.fakes.mux_script("up #{pid}", |_| {
        let stdout = format!("up {}\n", crate::agents::fixture::SERVER);
        Ok(crate::mux::Output {
            code: 0,
            stdout,
            stderr: String::new(),
        })
    });
    answers_views(&w);
    w
}
/// A member `id` of team `t` at K1 in session `$A`, generation 1 committed and `ok`; `fields` added.
fn at_k1(w: &World, id: &str, fields: Value) -> std::path::PathBuf {
    let mut all =
        json!({"session": "$A", "team": format!("{K1}:$A"), "context": "brief", "reason": "human"});
    for (k, v) in fields.as_object().unwrap() {
        all[k] = v.clone();
    }
    let f = rt::member_at(w, K1, id, all);
    rt::ok(&f, 1);
    f
}
fn cli(w: &World, args: &[&str]) -> (u8, Value) {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let (code, out) = crate::cli::run(&args, || Ok(&w.ctx));
    (code, serde_json::from_str(&out).unwrap())
}
/// The team an earlier `up` made at K1/$A, led by L; its view on a dead server.
fn seeded(w: &World) -> (std::path::PathBuf, std::path::PathBuf) {
    let t = ut::team_of(w);
    let fl = at_k1(w, L, json!({"name": "leader", "depth": 0, "upTeam": t}));
    let fx = at_k1(w, X, json!({"name": "x", "parentAgent": {"id": L}}));
    ut::seed_index_at(w, &t, K1, "$A", ut::dead_server());
    ut::team_record_at(w, &t, K1, "$A", L);
    (fl, fx)
}

// E4 end to end, the spec's kill-server: the index view is on a dead server; `up` from the new server reconciles,
// makes a new session, opens the views of the Running leader and member, keeps the bar on the selected (leader's)
// window, selects it and switches; reentered, not created; state stays at the anchor.
#[test]
fn up_after_a_killed_server_restores_both_views_and_the_bar() {
    let w = reentry_world();
    let (fl, fx) = seeded(&w);
    for _ in 0..2 {
        cf::online(&w); // reconcile's Lazy, then reenter's
        rt::found(&w, X, 1, &ut::create::running_wire(&w, &fx, X, 1));
        rt::found(&w, L, 1, &ut::create::running_wire(&w, &fl, L, 1));
    }
    let (code, r) = cli(&w, &["up", "-Client", "claude"]);
    assert_eq!(code, 0, "{r}");
    assert_eq!(
        (&r["reentered"], &r["created"], &r["session"]),
        (&json!(true), &json!(false), &json!("$7")),
        "{r}"
    );
    assert_eq!(
        r["leader"]["id"], L,
        "the reply carries the leader (finding #15)"
    );
    let views = r["views"].as_array().unwrap();
    assert_eq!(views.len(), 2, "both views");
    let lead = views.iter().find(|v| v["id"] == L).unwrap();
    let (window, pane) = (
        lead["window"].as_str().unwrap(),
        lead["pane"].as_str().unwrap(),
    );
    assert!(
        verbs(&w, "show-options")
            .iter()
            .any(|c| c.iter().any(|a| a == "$7")),
        "keep ran against the new session"
    );
    assert!(
        verbs(&w, "select-window")
            .iter()
            .any(|c| c.iter().any(|a| a == window)),
        "the leader's window selected"
    );
    assert!(
        verbs(&w, "select-pane")
            .iter()
            .any(|c| c.iter().any(|a| a == pane)),
        "and its pane"
    );
    assert!(
        verbs(&w, "switch-client")
            .iter()
            .any(|c| c.iter().any(|a| a.starts_with("=project_v1-"))),
        "switched"
    );
    let ix = crate::agents::up::index::read(&w.ctx, &ut::team_of(&w))
        .unwrap()
        .unwrap();
    assert_eq!(ix.anchor.unwrap().key, K1, "the anchor never moves");
    assert!(
        !w.ctx
            .state_root
            .join(w.caller().server_key)
            .join("agents")
            .exists(),
        "no member under the new key"
    );
    w.fakes.hc_done().unwrap();
}

// E4.5-6, finding #25: an Ended leader and two Running children: the oldest child (by `gens/0/launcher.json`
// `startedAt`) is selected and the client switched; no status window; the Ended leader blocks nothing.
#[test]
fn an_ended_leader_selects_the_oldest_running_child() {
    let w = reentry_world();
    let t = ut::team_of(&w);
    let fl = at_k1(&w, L, json!({"name": "leader", "depth": 0, "upTeam": t}));
    crate::state::evidence::publish(
        &crate::agents::gens::path(&fl, 1, "ended"),
        &json!({"at": "t"}),
    )
    .unwrap();
    let mut folders = Vec::new();
    for (id, started) in [(X, "2026-11-16T10:00:00Z"), (Y, "2026-11-16T09:00:00Z")] {
        let f = at_k1(
            &w,
            id,
            json!({"name": format!("m{}", &id[11..]), "parentAgent": {"id": L}}),
        );
        let at = crate::agents::gens::path(&f, 0, "launcher");
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        crate::state::fs::write_json(&at, &json!({"pid": 1, "start": 1, "startedAt": started}))
            .unwrap();
        folders.push((id, f));
    }
    ut::seed_index_at(&w, &t, K1, "$A", ut::dead_server());
    ut::team_record_at(&w, &t, K1, "$A", L);
    for _ in 0..2 {
        cf::online(&w); // reconcile, then reenter; the Ended leader is done, so hc is not asked about it
        for (id, f) in &folders {
            rt::found(&w, id, 1, &ut::create::running_wire(&w, f, id, 1));
        }
    }
    let (code, r) = cli(&w, &["up", "-Client", "claude"]);
    assert_eq!(code, 0, "{r}");
    let y = r["views"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == Y)
        .expect("Y shown");
    let y_pane = y["pane"].as_str().unwrap();
    assert!(
        verbs(&w, "select-pane")
            .iter()
            .any(|c| c.iter().any(|a| a == y_pane)),
        "the oldest child is selected"
    );
    assert!(!verbs(&w, "switch-client").is_empty(), "switched");
    assert!(
        !verbs(&w, "new-window")
            .iter()
            .any(|c| c.iter().any(|a| a == "status")),
        "no status window"
    );
    let lives: Vec<&str> = r["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["life"].as_str().unwrap())
        .collect();
    assert!(
        lives.contains(&"ended"),
        "the leader is listed Ended: {lives:?}"
    );
    w.fakes.hc_done().unwrap();
}

// Call 19, finding #26: a fresh `up` never reconciles: one hc connection (the launch's), no `restored`; its exact
// reply carries `views`, `duplicates` and `members` (finding #21).
#[test]
fn a_fresh_up_never_reconciles() {
    let w = ut::ready();
    let (code, r) = cli(&w, &["up", "-Client", "claude"]);
    assert_eq!(code, 0, "{r}");
    assert!(r.get("restored").is_none(), "no reconcile reply: {r}");
    let versions = w
        .fakes
        .hc_calls()
        .iter()
        .filter(|c| c.1[0] == "--version")
        .count();
    assert_eq!(versions, 1, "one hc::Lazy: the launch's");
    assert_eq!(
        (r["views"].as_array().unwrap().len(), &r["duplicates"]),
        (1, &json!(0))
    );
    assert_eq!(r["members"][0]["life"], "running");
}

// Call 23, finding #16: one member's view cannot be opened: `up` succeeds, the reply carries `viewError`, the
// others are shown, and the release is published once the reply reports the failure.
#[test]
fn a_member_view_failure_is_reported_and_the_others_shown() {
    let w = reentry_world();
    w.fakes.mux_first(&format!("attach a{X}"), 1, "", "no pane");
    let (fl, fx) = seeded(&w);
    for _ in 0..2 {
        cf::online(&w);
        rt::found(&w, X, 1, &ut::create::running_wire(&w, &fx, X, 1));
        rt::found(&w, L, 1, &ut::create::running_wire(&w, &fl, L, 1));
    }
    let (code, r) = cli(&w, &["up", "-Client", "claude"]);
    assert_eq!(code, 0, "{r}");
    assert!(r["viewError"].is_string(), "{r}");
    let ids: Vec<&str> = r["views"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [L], "the leader is still shown");
    assert!(
        ut::create::released(&w),
        "released once the reply reports the failure"
    );
}

// Task 12: a crash after any step is repaired by the next up (spec "Unit tests").
type Made = std::rc::Rc<std::cell::RefCell<Vec<(String, String)>>>;
/// A multiplexer that remembers the sessions `new-session` made, so `list-sessions` answers them (E3.1 by name).
fn stateful(w: &World, made: &Made) {
    let m = made.clone();
    w.fakes.mux_script("new-session", move |args| {
        let name = args
            .iter()
            .skip_while(|a| *a != "-s")
            .nth(1)
            .cloned()
            .unwrap_or_default();
        let id = format!("${}", 7 + m.borrow().len());
        m.borrow_mut().push((id.clone(), name));
        Ok(crate::mux::Output {
            code: 0,
            stdout: format!("{id} @3{}", char::from(10u8)),
            stderr: String::new(),
        })
    });
    let m = made.clone();
    w.fakes.mux_script("list-sessions", move |_| {
        let rows: String = m
            .borrow()
            .iter()
            .map(|(i, n)| format!("{i} {n}{}", char::from(10u8)))
            .collect();
        Ok(crate::mux::Output {
            code: 0,
            stdout: rows,
            stderr: String::new(),
        })
    });
}
/// The fixtures every process of a crash scenario shares: the bar off (layout is not under test), the World server
/// proven, every new view answered, launches holding, sessions answered from `made`.
fn crash_fixtures(w: &World, made: &Made) {
    w.fakes.mux_rule("@mux-agents-sidebar", 0, "off", "");
    w.fakes.mux_script("up #{pid}", |_| {
        Ok(crate::mux::Output {
            code: 0,
            stdout: format!("up {}", crate::agents::fixture::SERVER),
            stderr: String::new(),
        })
    });
    answers_views(w);
    w.launches_hold();
    stateful(w, made);
}
/// A person in `project.v1`, hc reachable for the first `up`.
fn crash_world() -> (World, Made) {
    let mut w = World::human("");
    w.ctx.cwd = w._dir.path().join("project.v1");
    std::fs::create_dir_all(&w.ctx.cwd).unwrap();
    cf::online(&w); // the first up's connect
    let made = Made::default();
    crash_fixtures(&w, &made);
    (w, made)
}
/// Findings A18, F7: a panic unwinds past `testing::locked`, so the crashed World keeps its fake locks. The next
/// `up` is a new process: a fresh World over the same state root and folder, the same sessions (`made`).
fn reopen(old: &World, made: &Made) -> World {
    let mut w = World::human("");
    w.ctx.state_root = old.ctx.state_root.clone();
    w.ctx.cwd = old.ctx.cwd.clone();
    crash_fixtures(&w, made);
    w
}
/// The non-void leaders of this folder's team under `key` (group B's `never_ran_recorded`).
fn leaders(w: &World, key: &str) -> Vec<(String, std::path::PathBuf)> {
    let t = ut::team_of(w);
    crate::agents::gens::members(&w.ctx.state_root.join(key).join("agents"))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(id, f, m)| match m {
            crate::agents::gens::Member::Life(g) if g.record()["upTeam"] == t.as_str() => {
                let s = g.status().unwrap();
                (!g.never_ran_recorded(&s).unwrap()).then_some((id, f))
            }
            _ => None,
        })
        .collect()
}
const FORWARD: [&str; 8] = [
    "allocated",
    "pending",
    "spec",
    "launch",
    "address",
    "run",
    "commit",
    "promote",
];
/// Stops the first `up` after `step`: a `forward` step by a panic in A3b-1's callback hook (the dispatcher answers
/// 1 `internal`; the files are what that step left), an E6 step by `halt_after`.
fn crash_after(step: &'static str) {
    if FORWARD.contains(&step) {
        crate::agents::core_launch::hooks::after(Some(Box::new(move |s: &str| {
            if s == step {
                panic!("crash after {step}");
            }
        })));
    } else {
        crate::agents::up::halt_after(Some(step));
    }
}
fn no_crash() {
    crate::agents::core_launch::hooks::after(None);
    crate::agents::up::halt_after(None);
}
fn address(a: &str) -> crate::error::Res<crate::spawn::Out> {
    crate::agents::core_launch::forward_tests::address(a)
}
/// Findings A19, F8: fake `hc run` runs no launcher, so its claim is seeded as the real launcher would have left
/// it: before commit (`run`, `address`) it never claimed (`abandoned`); at `commit`/`promote` it claimed `run`.
fn seed_launcher(w: &World, key: &str, step: &str) {
    let claim = match step {
        "run" | "address" => "abandoned",
        "commit" | "promote" => "run",
        _ => return,
    };
    let t = ut::team_of(w);
    for (_, f, m) in
        crate::agents::gens::members(&w.ctx.state_root.join(key).join("agents")).unwrap_or_default()
    {
        if let crate::agents::gens::Member::Life(g) = m
            && g.record()["upTeam"] == t.as_str()
        {
            let _ = crate::state::evidence::publish(
                &crate::agents::gens::claim_path(&f, 0),
                &json!(claim),
            );
        }
    }
}
/// The second `up`'s hc answers after a first `up` stopped at `step`, in real call order: its connect; settle for a
/// committed leader; reconcile and re-entry for an active leader; a run answer when the leader is (re)launched.
fn script_second_up(w: &World, step: &str, leader: Option<&(String, std::path::PathBuf)>) {
    cf::online(w);
    let ran = ["commit", "promote", "team", "presented"].contains(&step);
    match leader {
        Some((id, f)) if ran => {
            if step == "commit" {
                cf::online(w); // settle: one Running terminal, promoted
                rt::found(w, id, 0, &ut::create::running_wire(w, f, id, 0));
            }
            for _ in 0..2 {
                cf::online(w); // reconcile, then re-entry
                rt::found(w, id, 0, &ut::create::running_wire(w, f, id, 0));
            }
        }
        _ => w.fakes.hc_answer(address("t1")),
    }
}

// Spec "Initial team, a crash after each step" (calls 19, 20): E6 steps and every `forward` step; the next `up`,
// with other flags, follows the replay table from `inputs` and ends with one session, one non-void leader, one
// index, the team record and the first `up`'s options.
#[test]
fn a_crash_after_each_step_converges_to_one_team() {
    let steps = [
        "index",
        "session",
        "published",
        "allocated",
        "pending",
        "spec",
        "launch",
        "run",
        "address",
        "commit",
        "promote",
        "team",
        "presented",
    ];
    for step in steps {
        let (w, made) = crash_world();
        let key = w.caller().server_key;
        if steps.iter().position(|s| *s == step) >= steps.iter().position(|s| *s == "run") {
            w.fakes.hc_answer(address("t1"));
        }
        crash_after(step);
        let (code, first) = cli(&w, &["up", "-Client", "claude", "--", "--model", "a"]);
        no_crash();
        assert_ne!(code, 0, "{step}: stopped {first}");
        let w = reopen(&w, &made);
        seed_launcher(&w, &key, step);
        let ran = leaders(&w, &key).pop();
        script_second_up(&w, step, ran.as_ref());
        let (code, second) = cli(&w, &["up", "-Client", "claude", "--", "--model", "b"]);
        assert_eq!(code, 0, "{step}: {second}");
        assert_eq!(made.borrow().len(), 1, "{step}: one session");
        let lead = leaders(&w, &key);
        assert_eq!(lead.len(), 1, "{step}: one non-void leader");
        let rec = lf::record(&lead[0].1);
        let n: u64 = rec["generation"].as_str().unwrap().parse().unwrap();
        let spec = crate::agents::gens::read(&lead[0].1, n, "spec")
            .known()
            .unwrap()
            .unwrap()
            .to_string();
        assert!(
            spec.contains("\"a\"") && !spec.contains("\"b\""),
            "{step}: the first up's options: {spec}"
        );
        let indexes = std::fs::read_dir(w.ctx.state_root.join("up"))
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|x| x == "json")
            })
            .count();
        assert_eq!(indexes, 1, "{step}: one index");
        assert!(ut::team_file(&w).is_file(), "{step}: the team record");
        let after_team = step == "team" || step == "presented";
        assert_eq!(
            (&second["replayed"], &second["reentered"]),
            (&json!(!after_team), &json!(after_team)),
            "{step}"
        );
        assert!(!crate::agents::prelife_fixture::killed(&w), "{step}");
        w.fakes.hc_done().unwrap();
    }
}

/// Checklist #39 (N8): release-1d R1.3 for the up leader's Lost shell `t`: conclude's own `get` of the recorded
/// address, then the one relabel built from that terminal's labels (the anchor team, the leader's name).
fn kept(w: &World, t: &Value) {
    let address = t["address"].as_str().unwrap();
    cf::answer(w, &["get", address, "--json"], &t.to_string());
    let label = |k: &str| t["labels"][k].as_str().unwrap_or_default().to_string();
    let (team, name) = (
        format!("mux.kept={}", label("mux.team")),
        format!("mux.keptName={}", label("mux.name")),
    );
    let argv = [
        "label",
        address,
        "mux.managed=",
        "mux.team=",
        "mux.id=",
        "mux.gen=",
        "mux.name=",
        "mux.up=",
        &team,
        &name,
        "--json",
    ];
    cf::answer(w, &argv, "{}");
}

// Spec "Committed leader, up killed before promotion, then core loss" (and after `promote`, call 19): the next `up`
// settles or finds it active, and reconcile restores the leader from its session: the same member id.
#[test]
fn a_committed_leader_killed_before_promotion_comes_back_with_its_id() {
    for step in ["commit", "promote"] {
        let (w, made) = crash_world();
        let key = w.caller().server_key;
        w.fakes.hc_answer(address("t1"));
        crash_after(step);
        let _ = cli(&w, &["up", "-Client", "claude"]);
        no_crash();
        let w = reopen(&w, &made);
        seed_launcher(&w, &key, step);
        let (id, f) = leaders(&w, &key).pop().expect("a committed leader");
        // Checklist #39 (N8): a restoration-eligible outcome, as the launcher records it after the readiness report.
        crate::state::evidence::publish(
            &crate::agents::gens::path(&f, 0, "outcome"),
            &json!({"outcome": "ok", "at": "t"}),
        )
        .unwrap();
        let mut lost = ut::create::running_wire(&w, &f, &id, 0);
        lost["argv"] = json!(lf::LOST_ARGVS[0]);
        // Checklist #39 (N8): the shell carries the anchor session's team label and the leader's name.
        let a = crate::agents::up::index::read(&w.ctx, &ut::team_of(&w))
            .unwrap()
            .unwrap()
            .anchor
            .unwrap();
        lost["labels"]["mux.team"] = json!(format!("{}:{}", a.key, a.session));
        lost["labels"]["mux.name"] = json!("leader");
        cf::online(&w); // the second up's connect
        if step == "commit" {
            cf::online(&w); // settle: one Lost terminal: promote, then release
            rt::found(&w, &id, 0, &lost);
            kept(&w, &lost);
        }
        cf::online(&w); // reconcile's Lazy
        if step == "promote" {
            rt::found(&w, &id, 0, &lost); // row 4 Lost, outcome ok: release first
            kept(&w, &lost);
        } // commit: settle released it (cause restore): row 0 relaunches without asking hc
        w.fakes.hc_answer(address("t2")); // the relaunch as generation 1
        crate::agents::reconcile_up_tests::reentry_unlisted(&w, &id, 1); // re-entry: generation 1 unlisted
        let (code, r) = cli(&w, &["up", "-Client", "claude"]);
        let calls = lf::calls(&w);
        assert_eq!(code, 0, "{step}: {r} {calls:?}");
        assert_eq!(
            leaders(&w, &key)
                .iter()
                .map(|l| l.0.clone())
                .collect::<Vec<_>>(),
            std::slice::from_ref(&id),
            "{step}: same member id"
        );
        assert_eq!(
            lf::record(&f)["generation"],
            "1",
            "{step}: restored as generation 1: {r} {calls:?} {:?}",
            w.fakes.hc_done()
        );
        assert_eq!(
            r["leader"]["hcAddress"], "t2",
            "{step}: the reply names the restored address"
        );
        assert!(
            r["restored"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["id"] == id.as_str()),
            "{step}: reconcile ran"
        );
        assert!(ut::team_file(&w).is_file(), "{step}");
        w.fakes.hc_done().unwrap();
    }
}

// Spec "Repairer killed after dropping `pending`" (checklist #29 F18, #40 N9): a first `up` stops after its
// session; the session ends; a second `up` repairs (release durable, `pending` dropped) and stops at `repaired`; the
// third, with other flags, takes the inputs-only row and makes session and leader from the original inputs.
#[test]
fn a_repairer_killed_after_dropping_pending_leaves_the_inputs_row() {
    let (w, made) = crash_world();
    let key = w.caller().server_key;
    crash_after("session");
    let _ = cli(&w, &["up", "-Client", "claude", "--", "--model", "a"]);
    let t = ut::team_of(&w);
    let release = crate::agents::up::index::read(&w.ctx, &t)
        .unwrap()
        .unwrap()
        .pending
        .unwrap()
        .release;
    made.borrow_mut().clear(); // the session ended
    crash_after("repaired");
    cf::online(&w);
    let (code, r) = cli(&w, &["up", "-Client", "claude"]);
    no_crash();
    assert_eq!((code, r["check"].as_str()), (1, Some("halted")), "{r}");
    let ix = crate::agents::up::index::read(&w.ctx, &t).unwrap().unwrap();
    assert!(
        ix.pending.is_none() && ix.anchor.is_none() && ix.view.is_none(),
        "the inputs-only row"
    );
    assert_eq!(
        ix.releases,
        std::slice::from_ref(&release),
        "F18: the write that drops pending keeps its release durable"
    );
    cf::online(&w);
    w.fakes.hc_answer(address("t1"));
    let (code, r) = cli(&w, &["up", "-Client", "claude", "--", "--model", "b"]);
    assert_eq!(
        (code, &r["created"], &r["replayed"]),
        (0, &json!(true), &json!(true)),
        "{r}"
    );
    let ix = crate::agents::up::index::read(&w.ctx, &t).unwrap().unwrap();
    assert!(
        ix.releases.is_empty()
            && matches!(
                crate::state::evidence::read(std::path::Path::new(&release)),
                crate::state::evidence::Evidence::Present(_)
            ),
        "F18: after the next up the release is Present and cleared: {:?}",
        ix.releases
    );
    assert_eq!(made.borrow().len(), 1, "one session, from the inputs row");
    let lead = leaders(&w, &key);
    let spec = crate::agents::gens::read(&lead[0].1, 0, "spec")
        .known()
        .unwrap()
        .unwrap()
        .to_string();
    assert!(
        spec.contains("\"a\"") && !spec.contains("\"b\""),
        "the original inputs: {spec}"
    );
    w.fakes.hc_done().unwrap();
}

// Ambiguity 11, checklist #28 (F16-F17), #38 (N6-N7): a first `up` killed after `commit` with the launcher's `run`
// claim; the next `up` (a new process) finds two matching terminals: settle keeps `pending` (G3 Unknown), 1 `team`.
#[test]
fn an_unresolved_leader_launch_refuses_team() {
    let (w, made) = crash_world();
    let key = w.caller().server_key;
    w.fakes.hc_answer(address("t1"));
    crash_after("commit");
    let _ = cli(&w, &["up", "-Client", "claude"]);
    no_crash();
    let w = reopen(&w, &made);
    seed_launcher(&w, &key, "commit");
    let (id, f) = leaders(&w, &key).pop().unwrap();
    let pending = crate::agents::gens::pending(&lf::record(&f));
    assert_eq!(pending, Some(0), "the killed up left generation 0 pending");
    let wire = ut::create::running_wire(&w, &f, &id, 0);
    cf::online(&w);
    cf::online(&w); // settle
    let by_id = format!("mux.id={id}");
    cf::answer(
        &w,
        &[
            "ls",
            "--all",
            "--label",
            &by_id,
            "--label",
            "mux.gen=0",
            "--json",
        ],
        &json!({"terminals": [{"address": "t1"}, {"address": "t9"}]}).to_string(),
    );
    cf::answer(&w, &["get", "t1", "--json"], &wire.to_string());
    let mut twin = wire.clone();
    twin["address"] = json!("t9");
    cf::answer(&w, &["get", "t9", "--json"], &twin.to_string());
    let (code, r) = cli(&w, &["up", "-Client", "claude"]);
    assert_eq!(
        (code, r["check"].as_str(), r["message"].as_str()),
        (
            1,
            Some("team"),
            Some("the leader's launch is unresolved; retry")
        ),
        "{r}"
    );
    assert_eq!(
        crate::agents::gens::pending(&lf::record(&f)),
        pending,
        "pending still names the generation"
    );
    assert_eq!(
        w.fakes
            .hc_calls()
            .iter()
            .filter(|c| c.1[0] == "run")
            .count(),
        0,
        "nothing launched again"
    );
    w.fakes.hc_done().unwrap();
}

// Spec: a void leader (its client failed before commit) gets a new member id; its folder stays.
#[test]
fn a_void_leader_is_replaced_under_a_new_member_id() {
    let (w, _made) = crash_world();
    let key = w.caller().server_key;
    w.fakes.hc_answer(Ok(crate::spawn::Out {
        code: 1,
        stderr: "spawn failed".into(),
        ..Default::default()
    }));
    let _ = cli(&w, &["up", "-Client", "claude"]);
    let before: Vec<String> = std::fs::read_dir(w.ctx.state_root.join(&key).join("agents"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !before.is_empty() && leaders(&w, &key).is_empty(),
        "void: {before:?}"
    );
    cf::online(&w);
    w.fakes.hc_answer(address("t2"));
    let (code, r) = cli(&w, &["up", "-Client", "claude"]);
    assert_eq!(code, 0, "{r}");
    let now = leaders(&w, &key);
    assert_eq!(now.len(), 1);
    assert!(!before.contains(&now[0].0), "a new member id");
    assert!(
        before
            .iter()
            .all(|id| w.ctx.state_root.join(&key).join("agents").join(id).is_dir()),
        "the void folder stays"
    );
    w.fakes.hc_done().unwrap();
}

/// The caller probe answers session `session` (a later `mux_first` rule wins).
fn in_session(w: &World, session: &str) {
    w.fakes.mux_first(
        "#{pid}|#{pane_pid}|#{session_id}",
        0,
        &format!("900|100|{session}|@1|{}", char::from(10u8)),
        "",
    );
}
/// A second `up` that re-enters a team led by `id` at generation 0: its connect, reconcile's pass, re-entry's.
fn reenters(w: &World, id: &str, f: &std::path::Path) {
    crate::agents::view_bind_tests::attach_panes(w, &[("%21", "t1")], &[]);
    cf::online(w);
    for _ in 0..2 {
        cf::online(w);
        rt::found(w, id, 0, &ut::create::running_wire(w, f, id, 0));
    }
}

// Spec "up from the sessions it repairs": from a pending session (a publication crash) `send` is refused `moved`;
// `up` there repairs (E3.1 publishes it by name) and switches to it. From an alias the same refusal; `up` from the
// alias re-enters and switches to the view; no session is added.
#[test]
fn up_from_the_sessions_it_repairs_switches_there() {
    let (w, made) = crash_world();
    let key = w.caller().server_key;
    crash_after("session");
    let _ = cli(&w, &["up", "-Client", "claude"]);
    no_crash();
    let name = made.borrow()[0].1.clone();
    in_session(&w, "$7");
    w.fakes.mux_first("-t $7 #{session_name}", 0, &name, "");
    let (code, r) = cli(&w, &["send", "leader", "-Body", "hi"]);
    assert_eq!((code, r["check"].as_str()), (1, Some("moved")), "{r}");
    cf::online(&w);
    w.fakes.hc_answer(address("t1"));
    let (code, r) = cli(&w, &["up", "-Client", "claude"]);
    assert_eq!((code, &r["session"]), (0, &json!("$7")), "{r}");
    assert_eq!(made.borrow().len(), 1, "repaired, not remade");
    let target = format!("={name}");
    assert!(
        verbs(&w, "switch-client")
            .iter()
            .any(|c| c.contains(&target)),
        "switched to the repaired session"
    );
    let t = ut::team_of(&w);
    let mut ix = crate::agents::up::index::read(&w.ctx, &t).unwrap().unwrap();
    ix.aliases.push(crate::agents::up::index::View {
        session: "$3".into(),
        name: "old".into(),
        ..ix.view.clone().unwrap()
    });
    crate::agents::up::index::write(&w.ctx, &ix).unwrap();
    let (id, f) = leaders(&w, &key).pop().unwrap();
    reenters(&w, &id, &f); // queued before the caller moves to the alias: these helpers resolve the caller
    in_session(&w, "$3");
    let (code, r) = cli(&w, &["send", "leader", "-Body", "hi"]);
    assert_eq!(
        (code, r["check"].as_str()),
        (1, Some("moved")),
        "an alias: {r}"
    );
    let switched = verbs(&w, "switch-client").len();
    let (code, r) = cli(&w, &["up", "-Client", "claude"]);
    assert_eq!(
        (code, &r["reentered"]),
        (0, &json!(true)),
        "up from the alias: {r}"
    );
    assert_eq!(
        verbs(&w, "switch-client").len(),
        switched + 1,
        "it switches to the view"
    );
    assert!(verbs(&w, "switch-client").last().unwrap().contains(&target));
    assert_eq!(made.borrow().len(), 1, "no session added");
    w.fakes.hc_done().unwrap();
}

// Spec E3/E4, ambiguity 6, checklist #41 (N10): a `pending` on another server is published (then aliased when `up`
// makes the view on its own server) or released when that server lists no such session; never killed. Either way
// its release is `Present` and the outstanding list empty.
#[test]
fn a_pending_on_another_server_is_published_or_released_never_killed() {
    for listed in [true, false] {
        let (w, made) = crash_world();
        let other = "/tmp/rmux-501/other";
        w.fakes.process(crate::testing::row(5252, 1, 7));
        let s = crate::agents::up::index::Server {
            socket: other.into(),
            pid: 5252,
            start: 7,
            key: crate::agents::up::index::key_of(other, 5252, 7),
        };
        let t = ut::team_of(&w);
        let folder = crate::agents::up::identity::folder(&w.ctx).unwrap();
        let release = w.ctx.state_root.join("up").join("o.release");
        let pending = crate::agents::up::index::Pending {
            server: s.clone(),
            name: "p-other".into(),
            release: release.to_str().unwrap().into(),
        };
        let inputs = crate::agents::up::index::Inputs {
            client: "claude".into(),
            options: vec![],
            prompt: vec![],
            name: String::new(),
            folder: folder.to_str().unwrap().into(),
        };
        let ix = crate::agents::up::index::Index {
            team: t.clone(),
            inputs,
            anchor: None,
            view: None,
            aliases: vec![],
            releases: vec![],
            pending: Some(pending),
        };
        // Checklist #41 (N10): the originating `up` on that server published its binding.
        let bound = w.ctx.state_root.join(&s.key);
        std::fs::create_dir_all(&bound).unwrap();
        crate::state::evidence::publish(
            &bound.join("server.json"),
            &json!({"socket": other, "serverPid": 5252, "serverStart": 7}),
        )
        .unwrap();
        crate::agents::up::index::write(&w.ctx, &ix).unwrap();
        let rows = if listed {
            format!("$5 p-other{}", char::from(10u8))
        } else {
            String::new()
        };
        crate::agents::up::publish::test_sessions(other, Some((0, rows)));
        w.fakes.hc_answer(address("t1"));
        let (code, r) = cli(&w, &["up", "-Client", "claude"]);
        crate::agents::up::publish::test_sessions(other, None);
        assert_eq!(code, 0, "{listed}: {r}");
        let ix = crate::agents::up::index::read(&w.ctx, &t).unwrap().unwrap();
        assert_eq!(
            ix.view.unwrap().server.key,
            w.caller().server_key,
            "{listed}: the view is on the caller's server"
        );
        assert_eq!(
            ix.aliases.iter().any(|a| a.server == s),
            listed,
            "{listed}: the published one is an alias"
        );
        assert!(
            ix.releases.is_empty()
                && matches!(
                    crate::state::evidence::read(&release),
                    crate::state::evidence::Evidence::Present(_)
                ),
            "{listed}: the foreign release done"
        );
        assert_eq!(made.borrow().len(), 1);
        assert!(
            !crate::agents::prelife_fixture::killed(&w),
            "{listed}: never killed"
        );
        w.fakes.hc_done().unwrap();
    }
}

// Spec E3/E4: a second `up` adds nothing: no session, no launch, no view, the leader in the reply.
#[test]
fn a_second_up_adds_nothing() {
    let (w, made) = crash_world();
    let key = w.caller().server_key;
    w.fakes.hc_answer(address("t1"));
    let (code, first) = cli(&w, &["up", "-Client", "claude"]);
    assert_eq!(code, 0, "{first}");
    let (id, f) = leaders(&w, &key).pop().unwrap();
    reenters(&w, &id, &f);
    let windows = verbs(&w, "new-window").len();
    let (code, again) = cli(&w, &["up", "-Client", "claude"]);
    assert_eq!(
        (code, &again["reentered"], &again["created"]),
        (0, &json!(true), &json!(false)),
        "{again}"
    );
    assert_eq!(
        (made.borrow().len(), verbs(&w, "new-window").len()),
        (1, windows),
        "nothing added"
    );
    assert_eq!(
        w.fakes
            .hc_calls()
            .iter()
            .filter(|c| c.1[0] == "run")
            .count(),
        1,
        "one launch"
    );
    assert_eq!(
        (
            again["views"].as_array().unwrap().len(),
            &again["leader"]["id"]
        ),
        (1, &json!(id))
    );
    w.fakes.hc_done().unwrap();
}

// E1/E2 through the dispatcher: a torn index refuses `index` naming its path (call 2); a pre-lifecycle leader keeps
// today's refusal (prelife-1c G10). E4.2: a missing team record is repaired create-only from the leader's record;
// an unreadable leader record refuses 1 `team`.
#[test]
fn torn_index_prelife_leader_and_team_record_repair() {
    let (w, _made) = crash_world();
    let key = w.caller().server_key;
    let t = ut::team_of(&w);
    let at = crate::agents::up::index::path(&w.ctx, &t);
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::fs::write(&at, b"{").unwrap();
    let (code, r) = cli(&w, &["up", "-Client", "claude"]);
    let named = format!(
        "the team index cannot be read; repair or remove {}",
        at.display()
    );
    assert_eq!(
        (code, r["check"].as_str(), r["message"].as_str()),
        (1, Some("index"), Some(named.as_str())),
        "torn"
    );
    std::fs::remove_file(&at).unwrap();
    let pre = w
        .ctx
        .state_root
        .join(&key)
        .join("agents")
        .join("999999999999");
    std::fs::create_dir_all(&pre).unwrap();
    crate::state::fs::write_json(
        &pre.join("record.json"),
        &json!({"id": "999999999999", "name": "leader", "upTeam": t, "session": "$1", "hcAddress": "t9"}),
    )
    .unwrap();
    cf::online(&w);
    let (code, r) = cli(&w, &["up", "-Client", "claude"]);
    assert_eq!(
        (code, r["check"].as_str()),
        (2, Some("leader")),
        "prelife-1c G10: {r}"
    );
    std::fs::remove_dir_all(&pre).unwrap();
    cf::online(&w);
    w.fakes.hc_answer(address("t1"));
    let (code, first) = cli(&w, &["up", "-Client", "claude"]);
    assert_eq!(code, 0, "{first}");
    let (id, f) = leaders(&w, &key).pop().unwrap();
    std::fs::remove_file(ut::team_file(&w)).unwrap();
    reenters(&w, &id, &f);
    let (code, r) = cli(&w, &["up", "-Client", "claude"]);
    assert_eq!((code, &r["created"]), (0, &json!(true)), "{r}");
    assert_eq!(
        crate::agents::team::read(&ut::team_file(&w))
            .unwrap()
            .unwrap()
            .leader,
        id,
        "repaired"
    );
    let rec = f.join("record.json");
    std::fs::remove_file(&rec).unwrap();
    std::fs::create_dir(&rec).unwrap();
    std::fs::remove_file(ut::team_file(&w)).unwrap();
    cf::online(&w);
    let (code, r) = cli(&w, &["up", "-Client", "claude"]);
    assert_eq!(
        (code, r["check"].as_str()),
        (1, Some("team")),
        "E4.2 else 1 team: {r}"
    );
    w.fakes.hc_done().unwrap();
}
