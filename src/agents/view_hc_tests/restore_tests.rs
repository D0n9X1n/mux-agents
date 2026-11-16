//! W1, PM call 20: restore_view writes through `view_held`, for the subject generation.
use crate::agents::{
    core_fixture as cf,
    fixture::{ID, World, write},
    gens, member_target, view,
};
use crate::hc;
use crate::state::{evidence::publish, fs::read_json};
use serde_json::json;

/// A committed lifecycle member at generation 0 (`hcName` `maaaaaaaa`, launch.json published, no view.json).
fn life_member(w: &World) -> serde_json::Value {
    let r = cf::member(w, ID, json!({"lifecycle": 1, "hcName": "maaaaaaaa"}));
    let f = w.folder(ID);
    publish(
        &gens::path(&f, 0, "launch"),
        &json!({"hcName": "maaaaaaaa"}),
    )
    .unwrap();
    publish(&gens::path(&f, 0, "commit"), &json!({"committed": true})).unwrap();
    r
}

// S4, W1: a lifecycle reattach writes only view.json, naming its generation, address and launch.json's hcName;
// record.json is untouched; the member lock is taken once (by focus_identity).
#[test]
fn a_lifecycle_reattach_writes_only_its_view() {
    let w = World::human("");
    life_member(&w);
    let c = w.caller();
    let f = w.folder(ID);
    let before = std::fs::read(f.join("record.json")).unwrap();
    w.panes(&[]);
    w.launches_hold();
    w.fakes.mux_rule("@mux-agents-placement", 0, "tab", "");
    w.fakes
        .mux_rule("new-window", 0, "%19|@8|hc attach taaaaaaaaaaaa\n", "");
    w.fakes.mux_rule("#{window_active}", 0, "%19|1|1|1\n", "");
    cf::online(&w);
    let core = hc::connect(&w.ctx).unwrap();
    w.fakes.clear_log();
    assert_eq!(
        view::focus_hc(&w.ctx, &core, &c, "worker", None)
            .unwrap()
            .pane,
        "%19"
    );
    assert_eq!(
        std::fs::read(f.join("record.json")).unwrap(),
        before,
        "record.json untouched"
    );
    let want = json!({"serverKey": c.server_key, "session": c.session, "pane": "%19", "window": "@8",
        "startCommand": "hc attach taaaaaaaaaaaa", "attachProgram": core.binary().display().to_string(), "gen": 0,
        "hcAddress": "taaaaaaaaaaaa", "hcName": "maaaaaaaa"});
    assert_eq!(read_json(&f.join("view.json")).unwrap().unwrap(), want);
    let m = crate::agents::lock_key("member", &c, ID);
    let held = w
        .fakes
        .locks()
        .iter()
        .filter(|l| **l == format!("+{m}"))
        .count();
    assert_eq!(held, 1, "one member lock: {:?}", w.fakes.locks());
}

// PM call 20 (A3a 18a): a held identity's reattach writes its pending generation's view; record.json stays byte for
// byte; the next reader presents that view, so the next focus selects it rather than attaching again.
#[test]
fn a_held_reattach_writes_and_presents_its_pending_view() {
    let w = World::human("");
    let c = w.caller();
    let fields = json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd", "hcAddress": "old-address"});
    let mut r = cf::member(&w, ID, fields);
    r["pending"] = json!({"gen": 2, "hcName": "m4567cdef", "hcAddress": "new-address",
        "sessionId": "11111111-2222-4333-8444-555555555555"});
    let f = w.folder(ID);
    write(&f.join("record.json"), r);
    publish(&gens::path(&f, 2, "commit"), &json!({"committed": true})).unwrap();
    let before = std::fs::read(f.join("record.json")).unwrap();
    w.panes(&[]);
    w.launches_hold();
    w.fakes
        .mux_rule("new-window", 0, "%19|@8|hc attach new-address\n", "");
    w.fakes.mux_rule("#{window_active}", 0, "%19|1|1|1\n", "");
    cf::online(&w);
    let core = hc::connect(&w.ctx).unwrap();
    let got = view::focus_member(&w.ctx, Some(&core), &c, ID, None);
    assert!(matches!(got, Ok(view::Member::Focused(_))), "{got:?}");
    assert_eq!(
        std::fs::read(f.join("record.json")).unwrap(),
        before,
        "record.json byte-identical"
    );
    let v = read_json(&f.join("view.json")).unwrap().unwrap();
    let held = (&v["gen"], &v["hcAddress"], &v["hcName"], &v["pane"]);
    assert_eq!(
        held,
        (
            &json!(2),
            &json!("new-address"),
            &json!("m4567cdef"),
            &json!("%19")
        ),
        "the pending view"
    );
    let id = member_target::by_id(&w.ctx, &c, ID).unwrap();
    let shown = member_target::current(&w.ctx, &c, &id).unwrap();
    assert_eq!(
        (&shown["pane"], &shown["window"]),
        (&json!("%19"), &json!("@8")),
        "the held view presents"
    );
}

// a280e7 IMPORTANT 4, PM call 23: the view write fails after the attach made its pane, by an obstruction made
// inside the attach (view.json absent before; the rename onto a folder fails: 1 `internal`) or by a promotion
// landing between `current` and the write (the subject is now generation 1: 2 `target`). Only the new pane is killed.
#[test]
fn a_failed_view_write_cleans_only_the_new_pane() {
    for (case, want) in [("obstructed", (1, "internal")), ("promoted", (2, "target"))] {
        let w = World::human("");
        let r = life_member(&w);
        let c = w.caller();
        let f = w.folder(ID);
        assert!(
            !f.join("view.json").exists(),
            "{case}: the fixture starts without a view"
        );
        w.panes(&[]);
        w.launches_hold();
        let (at, rec) = (f.join("view.json"), f.join("record.json"));
        let mut promoted = r.clone();
        (promoted["generation"], promoted["hcAddress"]) = (json!("1"), json!("tother"));
        w.fakes.mux_script("new-window", move |_| {
            if case == "obstructed" {
                std::fs::create_dir(&at).unwrap();
            } else {
                crate::state::fs::write_json(&rec, &promoted).unwrap();
            }
            Ok(crate::mux::Output {
                code: 0,
                stdout: "%19|@8|hc attach taaaaaaaaaaaa\n".into(),
                stderr: String::new(),
            })
        });
        w.fakes.mux_rule("#{pane_id}|#{@agent-id}", 0, "|\n", "");
        cf::online(&w);
        let core = hc::connect(&w.ctx).unwrap();
        let e = view::focus_hc(&w.ctx, &core, &c, "worker", None).unwrap_err();
        assert_eq!((e.code, e.check), want, "{case}: {e:?}");
        let kills: Vec<_> = w
            .fakes
            .mux_calls()
            .into_iter()
            .filter(|a| a.last().is_some_and(|s| s.starts_with("kill-pane")))
            .collect();
        assert_eq!(kills.len(), 1, "{case}: {kills:?}");
        assert_eq!(kills[0].last().unwrap(), "kill-pane -t %19", "{case}");
        if case == "obstructed" {
            assert!(
                f.join("view.json").is_dir(),
                "the obstruction stands; nothing written through it"
            );
            assert_eq!(
                read_json(&f.join("record.json")).unwrap().unwrap(),
                r,
                "record.json untouched"
            );
        } else {
            assert!(
                !f.join("view.json").exists(),
                "generation 0's view never lands on generation 1"
            );
        }
    }
}

// Ambiguity 3: an identity whose generation core_identity accepts but gens::plain_gen does not ("01") is refused
// before any pane is made.
#[test]
fn a_noncanonical_generation_is_refused_before_any_pane() {
    let w = World::human("");
    cf::member(&w, ID, json!({"generation": "01", "hcName": "maaaaaaaa"}));
    let c = w.caller();
    w.panes(&[]);
    w.launches_hold();
    w.fakes
        .mux_rule("new-window", 0, "%19|@8|hc attach taaaaaaaaaaaa\n", "");
    cf::online(&w);
    let core = hc::connect(&w.ctx).unwrap();
    let e = view::focus_hc(&w.ctx, &core, &c, "worker", None).unwrap_err();
    let made = w
        .fakes
        .mux_calls()
        .iter()
        .any(|a| a[1] == "new-window" || a[1] == "split-window");
    assert!(!made, "no pane before the refusal");
    assert_eq!((e.code, e.check), (1, "internal"), "{e:?}");
}

// Checklist 16 (a3b call 40): after `focus` reattaches a detached lifecycle member, `list` and the board show the
// new pane from view.json, and record.json carries no pane, window or startCommand.
#[test]
fn a_reattached_lifecycle_member_lists_its_new_pane() {
    use crate::agents::fixture::{Listed, listed_rows};
    let w = World::human("");
    let mut r = life_member(&w);
    for k in crate::agents::team_state::VIEW_FIELDS {
        r.as_object_mut().unwrap().remove(k);
    }
    let f = w.folder(ID);
    write(&f.join("record.json"), r.clone());
    let c = w.caller();
    w.panes(&[]);
    w.launches_hold();
    w.fakes.mux_rule("@mux-agents-placement", 0, "tab", "");
    // views-6f v6f call 13: the pane runs the hc program the view records, as the multiplexer renders it.
    let attach = format!(
        "\"{}\" attach taaaaaaaaaaaa",
        w.fakes.hc_install("hc").display()
    );
    w.fakes
        .mux_rule("new-window", 0, &format!("%19|@8|{attach}\n"), "");
    w.fakes.mux_rule("#{window_active}", 0, "%19|1|1|1\n", "");
    cf::online(&w);
    let core = hc::connect(&w.ctx).unwrap();
    view::focus_hc(&w.ctx, &core, &c, "worker", None).unwrap();
    let stored = read_json(&f.join("record.json")).unwrap().unwrap();
    for k in crate::agents::team_state::VIEW_FIELDS {
        assert!(stored.get(k).is_none(), "record.json has no {k}: {stored}");
    }
    let row = Listed {
        pane: "%19".into(),
        window: "@8".into(),
        pid: "4000".into(),
        dead: false,
        command: attach,
        tag: ID.into(),
        name: "worker".into(),
    };
    w.fakes.mux_script("list-panes", move |argv| {
        Ok(crate::mux::Output {
            code: 0,
            stdout: listed_rows(argv, std::slice::from_ref(&row), "\n"),
            stderr: String::new(),
        })
    });
    cf::online(&w);
    cf::listing(&w, &crate::agents::membership::team(&c), &[]);
    cf::get(&w, "taaaaaaaaaaaa", cf::wire(&r));
    let got = crate::agents::board::board(&w.ctx, &c, false, false).unwrap();
    let a = &got["agents"][0];
    assert_eq!(
        (&a["pane"], &a["state"]),
        (&json!("%19"), &json!("running")),
        "the board shows the new pane: {got:?}"
    );
}
