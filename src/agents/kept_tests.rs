//! views-6f V4-V6: kept shells are hc labels; listed while live; closed only while hc shows them kept here.
use super::core_fixture as cf;
use super::fixture::{ID, World};
use super::kept::{self, Shell};
use super::life_fixture as lf;
use crate::spawn::Out;
use serde_json::{Value, json};

/// hc's `get` reply for a recovery shell at `address` (b-calls 43: Linux's bare `/bin/sh`), generation 0's hc name;
/// `kept` = `(team, keptName)` sets exactly the two kept labels.
fn kept_wire(address: &str, state: &str, kept: Option<(&str, &str)>) -> Value {
    let labels = match kept {
        Some((team, name)) => json!({"mux.kept": team, "mux.keptName": name}),
        None => json!({}),
    };
    let done = state == "finished";
    json!({"address": address, "name": lf::hc_name(0), "state": state,
        "reason": if done { json!("exited") } else { Value::Null },
        "exit_code": if done { json!(0) } else { Value::Null }, "argv": ["/bin/sh"], "labels": labels})
}

fn kills(w: &World) -> usize {
    w.fakes
        .hc_calls()
        .iter()
        .filter(|c| c.1.first().map(String::as_str) == Some("kill"))
        .count()
}

// V5: one row per live terminal hc shows with `mux.kept=<T>`; one that finished or was forgotten between the live
// `ls` and its `get`, and another team's, are not rows; the address stands in for an empty keptName; sorted by name.
// Two losses give two rows.
#[test]
fn the_list_is_this_teams_live_kept_shells() {
    let w = World::human("");
    let team = lf::team(&w);
    cf::online(&w);
    cf::kept_listing(&w, &team, &["k1", "k2", "k3", "k4", "k5"]);
    cf::get(&w, "k1", kept_wire("k1", "live", Some((&team, "b"))));
    cf::get(&w, "k2", kept_wire("k2", "live", Some((&team, ""))));
    cf::get(&w, "k3", kept_wire("k3", "finished", Some((&team, "c"))));
    lf::gone(&w, "k4");
    cf::get(
        &w,
        "k5",
        kept_wire("k5", "live", Some(("0123456789abcdef:$9", "d"))),
    );
    let core = crate::hc::connect(&w.ctx).unwrap();
    let got = kept::list(&core, &team).unwrap();
    let want = [("b", "k1"), ("k2", "k2")].map(|(n, a)| Shell {
        address: a.into(),
        name: n.into(),
    });
    assert_eq!(got, want);
    w.fakes.hc_done().unwrap();
    let w = World::human("");
    cf::online(&w);
    cf::kept_listing(&w, &lf::team(&w), &["k1"]);
    w.fakes.hc_expect(
        &["get", "k1", "--json"],
        Ok(Out {
            code: 1,
            stdout: String::new(),
            stderr: "boom".into(),
        }),
    );
    let core = crate::hc::connect(&w.ctx).unwrap();
    assert_eq!(
        kept::list(&core, &lf::team(&w)).map_err(|e| e.check),
        Err("hc"),
        "an unclear hc fails the list"
    );
}

// V5: `hc kill` only while the re-read shows it live with `mux.kept=<T>`; every other answer refuses, killing nothing.
#[test]
fn close_kills_only_a_live_shell_kept_by_this_team() {
    type Want = Result<(), (u8, &'static str, &'static str)>;
    type Wire = Option<(&'static str, Option<(&'static str, &'static str)>)>;
    let not_kept = "that shell is not a kept shell of this team";
    let cases: [(&str, Wire, Want); 5] = [
        ("kept here", Some(("live", Some(("T", "a")))), Ok(())),
        (
            "finished",
            Some(("finished", Some(("T", "a")))),
            Err((2, "target", "the shell has already finished")),
        ),
        (
            "another team",
            Some(("live", Some(("0123456789abcdef:$9", "a")))),
            Err((2, "target", not_kept)),
        ),
        (
            "unlabelled",
            Some(("live", None)),
            Err((2, "target", not_kept)),
        ),
        ("gone", None, Err((2, "target", "no such kept shell"))),
    ];
    for (case, wire, want) in cases {
        let w = World::human("");
        let team = lf::team(&w);
        cf::online(&w);
        match wire {
            Some((state, kept)) => {
                let kept = kept.map(|(t, n)| (if t == "T" { team.as_str() } else { t }, n));
                cf::get(&w, "k1", kept_wire("k1", state, kept));
            }
            None => lf::gone(&w, "k1"),
        }
        if want.is_ok() {
            cf::answer(&w, &["kill", "k1", "--json"], "{}");
        }
        let core = crate::hc::connect(&w.ctx).unwrap();
        let got = kept::close(&core, &team, "k1").map_err(|e| (e.code, e.check, e.message.clone()));
        let want = want.map_err(|(c, k, m)| (c, k, m.to_string()));
        assert_eq!(got, want, "{case}");
        assert_eq!(kills(&w), usize::from(want.is_ok()), "{case}: kills");
        w.fakes.hc_done().unwrap();
    }
}

// V4 (spec "Labels after release"): group B's R1.3 leaves exactly `mux.kept=<T>` and `mux.keptName=<name>`, so no
// member listing (`mux.managed=1`, `mux.team`) and no `Identity::matches` can see the shell. Passes when written:
// rows 3.7 and 3.8 make it meaningful.
#[test]
fn a_released_shell_carries_only_the_kept_labels() {
    let (w, s) = lf::world();
    let team = lf::team(&w);
    let f = lf::life(
        &w,
        json!({"generation": "0", "hcAddress": "t0", "hcName": lf::hc_name(0),
        "launcherArgv": lf::ARGV}),
    );
    lf::seed_gen(&f, 0, None, lf::committed());
    let mut before = kept_wire("t0", "live", None);
    before["labels"] = json!({"mux.managed": "1", "mux.team": team, "mux.id": ID, "mux.gen": "0",
        "mux.name": "worker", "mux.up": "1"});
    cf::online(&w);
    cf::get(&w, "t0", before.clone());
    let kept_arg = format!("mux.kept={team}");
    cf::answer(
        &w,
        &[
            "label",
            "t0",
            "mux.managed=",
            "mux.team=",
            "mux.id=",
            "mux.gen=",
            "mux.name=",
            "mux.up=",
            &kept_arg,
            "mux.keptName=worker",
            "--json",
        ],
        "{}",
    );
    super::team_state::open(&w.ctx, &s, |tx| {
        let lazy = crate::hc::Lazy::new(&w.ctx);
        super::release::release(tx, &lazy, ID, 0, "t0", super::release::Cause::Restore)
    })
    .unwrap();
    w.fakes.hc_done().unwrap();
    // The labels hc holds now: `before`'s, with the one `label` call applied (`k=` clears, `k=v` sets).
    let call = w
        .fakes
        .hc_calls()
        .into_iter()
        .find(|c| c.1[0] == "label")
        .unwrap()
        .1;
    let mut labels: std::collections::BTreeMap<String, String> = before["labels"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect();
    for pair in &call[2..call.len() - 1] {
        let (k, v) = pair.split_once('=').unwrap();
        if v.is_empty() {
            labels.remove(k);
        } else {
            labels.insert(k.into(), v.into());
        }
    }
    let want: std::collections::BTreeMap<String, String> =
        [("mux.kept", team.as_str()), ("mux.keptName", "worker")]
            .map(|(k, v)| (k.into(), v.into()))
            .into();
    assert_eq!(labels, want, "exactly the two kept labels");
    assert!(
        f.join("gens/0/released.json").is_file(),
        "the generation is done"
    );
}

// V6 (audit 1-5 F13): the only lifecycle kill is `settle::terminate`. It runs here after release, with the old
// terminal still kept: a released generation needs no hc; the restored generation's terminate kills only its own
// terminal, even when the listing wrongly returns the kept shell. Passes when written: rows 3.9, 3.12 make it
// meaningful.
#[test]
fn terminate_never_kills_a_kept_shell() {
    let (w, s) = lf::world();
    let team = lf::team(&w);
    let f = lf::life(
        &w,
        json!({"generation": "1", "hcAddress": "t1", "hcName": lf::hc_name(1),
        "launcherArgv": lf::ARGV}),
    );
    lf::seed_gen(&f, 0, None, lf::committed());
    crate::state::evidence::publish(&super::gens::path(&f, 0, "released"), &json!({"at": "x"}))
        .unwrap();
    // B1's settle reads an absent claim as abandoned (re-audit 1-3 F7): generation 1 claimed its run.
    lf::seed_gen(&f, 1, Some("run"), lf::committed());
    w.fakes.hc_install("hc");
    let stop =
        |n: u64| super::team_state::open(&w.ctx, &s, |tx| super::settle::terminate(tx, ID, n));
    stop(0).unwrap();
    assert!(
        w.fakes.hc_calls().is_empty(),
        "a released generation: no hc call, nothing killed"
    );
    // settle::terminate's script (r6b checklist 16): connection, label-filtered `ls`, a `get` of each listed
    // address, the re-read, `kill.json`, then the kill.
    cf::online(&w);
    lf::ls(&w, 1, &["t0", "t1"]);
    cf::get(&w, "t0", kept_wire("t0", "live", Some((&team, "worker"))));
    cf::get(&w, "t1", lf::running(&w, 1));
    cf::get(&w, "t1", lf::running(&w, 1));
    cf::answer(&w, &["kill", "t1", "--json"], "{}");
    stop(1).unwrap();
    let killed: Vec<String> = w
        .fakes
        .hc_calls()
        .into_iter()
        .filter(|c| c.1[0] == "kill")
        .map(|c| c.1[1].clone())
        .collect();
    assert_eq!(
        killed,
        ["t1"],
        "only the restored generation's own terminal"
    );
    w.fakes.hc_done().unwrap();
}
