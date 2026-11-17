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
