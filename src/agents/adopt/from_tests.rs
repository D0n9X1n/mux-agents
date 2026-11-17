//! adopt-3e Task 5: `adopt_from` journals, applies, commits and removes; replay converges; a blocked source says
//! why; overflow refuses first (A2-A4; AD-R2 to AD-R6; calls 7, 9, 28).
use super::tests::world;
use super::*;
use crate::agents::adopt_fixture as ax;
use crate::agents::cascade_fixture as cx;
use crate::agents::fixture::World;
use crate::agents::life_fixture as lf;
use crate::agents::team_state::Scope;
use serde_json::json;

pub(super) fn adopt_now(w: &World, id: &str) -> Adopted {
    // Hook-free: group B's hooks would settle the pending generation seeded on D.
    adopt_from(&cx::tx(w, &Scope::of(&w.caller())), id).unwrap()
}
pub(super) fn at(w: &World, id: &str) -> (Option<String>, u64) {
    (ax::parent_of(w, id), ax::depth_of(w, id))
}
pub(super) fn moved(to: &str) -> Adopted {
    Adopted::Moved(to.into())
}

// A2, A4, ambiguities 6-9: E's live and pending children move; their sub-trees' depths follow; an Ended child
// stays; the adopter's view pane is the new parentPane; nothing left to move writes nothing.
#[test]
fn live_and_pending_children_move_and_their_depths_follow() {
    use cx::{A, B, C, D};
    let (w, s) = world();
    ax::team(&w);
    ax::view_of(&w, A, "%9");
    cx::member(&w, C, Some(B), 4, json!({}));
    cx::member(&w, D, Some(ax::E), 3, json!({"pending": lf::pending(2)}));
    ax::ended(&w, D); // generation 1 done: D is live only by its pending launch
    cx::member(&w, ax::X, Some(ax::E), 3, json!({}));
    ax::ended(&w, ax::X);
    assert_eq!(adopt_now(&w, ax::E), moved(A));
    assert_eq!(at(&w, B), (Some(A.into()), 2), "live child");
    assert_eq!(at(&w, D), (Some(A.into()), 2), "pending child");
    assert_eq!(
        at(&w, C),
        (Some(B.into()), 3),
        "depth recomputed, parent kept"
    );
    assert_eq!(at(&w, ax::X), (Some(ax::E.into()), 3), "Ended child stays");
    assert_eq!(
        (
            ax::record(&w, B)["parentPane"].clone(),
            ax::record(&w, B)["parentAgent"]["pid"].clone()
        ),
        (json!("%9"), Value::Null),
        "the view pane; no pid"
    );
    assert_eq!(
        (ax::seq(&w, &s), listing(&w.ctx, &s).unwrap()),
        (1, Listing::default())
    );
    assert_eq!(adopt_now(&w, ax::E), Adopted::Nothing, "nothing left");
    assert_eq!(ax::seq(&w, &s), 1, "no second seq");
}

// prelife-1c calls 1-2, ambiguity 17: a pre-lifecycle member under E or under B is never moved and its folder stays
// byte-identical; a chain that reaches a pre-lifecycle member has no adopter and says so (call 9).
#[test]
fn a_pre_lifecycle_folder_stays_byte_identical() {
    use cx::{A, B, C, L};
    fn files(w: &World, id: &str) -> Vec<String> {
        let mut v: Vec<String> = (std::fs::read_dir(w.folder(id)).unwrap().flatten())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }
    let (w, _) = world();
    ax::team(&w);
    let (under_e, under_b) = (
        ax::pre(&w, C, Some(ax::E), 3),
        ax::pre(&w, ax::X, Some(B), 4),
    );
    assert_eq!(adopt_now(&w, ax::E), moved(A));
    assert_eq!(at(&w, B), (Some(A.into()), 2), "the lifecycle child moves");
    let bytes = |id: &str| std::fs::read(w.folder(id).join("record.json")).unwrap();
    assert_eq!(bytes(C), under_e, "pre-lifecycle child of E");
    assert_eq!(bytes(ax::X), under_b, "pre-lifecycle child of B");
    let one = vec!["record.json".to_string()];
    assert_eq!((files(&w, C), files(&w, ax::X)), (one.clone(), one));
    let (w, s) = world();
    ax::team(&w);
    let a = ax::pre(&w, A, Some(L), 1);
    assert_eq!(
        adopt_now(&w, ax::E),
        Adopted::Blocked(LEAVES),
        "A is pre-lifecycle"
    );
    assert_eq!((at(&w, B), ax::seq(&w, &s)), ((Some(ax::E.into()), 3), 0));
    let after = std::fs::read(w.folder(A).join("record.json")).unwrap();
    assert_eq!(after, a, "pre-lifecycle A untouched");
}

/// A case, its world, the reason (checklist 10: an alias, clippy `type_complexity`).
type Rows = [(&'static str, fn(&World), &'static str); 2];

// Call 9 (finding 1-8): several logical parents, or an Ended orphan whose chain leaves the tree: the reason, nothing
// written.
#[test]
fn a_blocked_source_says_why() {
    use cx::{A, B, L};
    let rows: Rows = [
        (
            "two successors",
            |w| {
                ax::successor(w, ax::A2, A, L, 1, ax::Kind::Committed);
                ax::successor(w, "aaaaaaaaaaa3", A, L, 1, ax::Kind::Committed);
            },
            SEVERAL,
        ),
        (
            "ended orphan",
            |w| {
                ax::ended(w, A);
                ax::pre(w, L, None, 0);
            },
            LEAVES,
        ),
    ];
    for (case, plant, why) in rows {
        let (w, s) = world();
        ax::team(&w);
        plant(&w);
        assert_eq!(adopt_now(&w, ax::E), Adopted::Blocked(why), "{case}");
        assert_eq!(
            (at(&w, B), ax::seq(&w, &s)),
            ((Some(ax::E.into()), 3), 0),
            "{case}: nothing written"
        );
        assert_eq!(
            listing(&w.ctx, &s).unwrap(),
            Listing::default(),
            "{case}: no journal"
        );
    }
}

// A3: two ancestors ending in either order give one adopter.
#[test]
fn two_ancestors_ending_in_either_order_give_one_adopter() {
    use cx::{A, B, L};
    for first_e in [true, false] {
        let (w, _) = world();
        ax::team_live(&w);
        let order = if first_e { [ax::E, A] } else { [A, ax::E] };
        for id in order {
            ax::ended(&w, id);
            for src in [ax::E, A] {
                let _ = adopt_now(&w, src);
            }
        }
        assert_eq!(at(&w, B), (Some(L.into()), 1), "first_e {first_e}");
    }
}

// A2: a live member is never adopted from, whatever its ancestors (adopt_from revalidates the source).
#[test]
fn a_live_member_is_never_adopted_from() {
    let (w, s) = world();
    ax::team_live(&w);
    ax::ended(&w, cx::A);
    assert_eq!(adopt_now(&w, ax::E), Adopted::Nothing);
    assert_eq!(
        (at(&w, cx::B), ax::seq(&w, &s)),
        ((Some(ax::E.into()), 3), 0)
    );
}

// A4, AD-R6: a crash after the journal, after each change, after the seq write and after removal: the journal is on
// disk exactly until removal; the next adoption's replay converges; nothing is left.
#[test]
fn a_crash_after_each_step_converges() {
    use cx::{B, C, L};
    for step in [
        "adopt-journal",
        "adopt-change-bbbbbbbbbbb1",
        "adopt-change-ccccccccccc1",
        "adopt-seq",
        "adopt-removed",
    ] {
        let (w, s) = world();
        ax::four_deep(&w);
        cx::at(step);
        let got =
            crate::agents::team_state::open(&w.ctx, &s, |tx| adopt_from(tx, ax::E)).unwrap_err();
        crate::agents::release::halt_after(None);
        assert_eq!(got.check, "halted", "{step}");
        let kept = listing(&w.ctx, &s).unwrap().journals;
        let want: Vec<u64> = if step == "adopt-removed" {
            vec![]
        } else {
            vec![1]
        };
        assert_eq!(kept, want, "{step}: journal at the halt");
        let _ = adopt_now(&w, ax::E); // its own replay (hook-free) finishes the journal first
        assert_eq!(
            (at(&w, B), at(&w, C)),
            ((Some(L.into()), 1), (Some(B.into()), 2)),
            "{step}"
        );
        assert_eq!(
            (ax::seq(&w, &s), listing(&w.ctx, &s).unwrap()),
            (1, Listing::default()),
            "{step}"
        );
        assert_eq!(
            adopt_now(&w, ax::E),
            Adopted::Nothing,
            "{step}: nothing left"
        );
    }
}

// AD-R4, calls 7, 28 (checklists 7, 9): seq and depth arithmetic is checked; overflow refuses before any write. A
// stale journal is not removed when the next seq is out of range (overflow before replay deletes anything); a
// descendant's depth that overflows refuses as the moved child's would.
#[test]
fn an_overflowing_seq_or_depth_refuses_before_any_write() {
    use cx::{A, B, C};
    for case in [
        "seq",
        "seq with a stale journal",
        "depth",
        "descendant depth",
    ] {
        let (w, s) = world();
        ax::team(&w);
        match case {
            "depth" => ax::set(&w, A, "depth", json!(u64::MAX)),
            "descendant depth" => {
                ax::set(&w, A, "depth", json!(u64::MAX - 1));
                cx::member(&w, C, Some(B), 4, json!({}));
            }
            _ => commit(&cx::tx(&w, &s), u64::MAX).unwrap(),
        }
        if case == "seq with a stale journal" {
            ax::put_journal(&w, &s, &ax::to_a(5));
        }
        let e = adopt_from(&cx::tx(&w, &s), ax::E).unwrap_err();
        assert_eq!(
            (e.check, e.message.as_str()),
            ("topology", OVERFLOW),
            "{case}"
        );
        assert_eq!(
            at(&w, B),
            (Some(ax::E.into()), 3),
            "{case}: nothing applied"
        );
        let stale = if case == "seq with a stale journal" {
            vec![5]
        } else {
            vec![]
        };
        assert_eq!(
            listing(&w.ctx, &s).unwrap(),
            Listing {
                journals: stale,
                tombstones: vec![]
            },
            "{case}: nothing published, nothing removed"
        );
    }
}

/// The pause: adoption commits B under A while the writer is stopped at its entry.
fn adopt_in_pause(ctx: &crate::ctx::Ctx, s: &Scope) {
    let tx = Tx {
        ctx,
        scope: s.clone(),
    };
    assert_eq!(adopt_from(&tx, ax::E).unwrap(), moved(cx::A));
}
/// The same, cut short after the journal; replay finishes it inside the pause.
fn halted_in_pause(ctx: &crate::ctx::Ctx, s: &Scope) {
    let tx = Tx {
        ctx,
        scope: s.clone(),
    };
    cx::at("adopt-journal");
    assert_eq!(adopt_from(&tx, ax::E).unwrap_err().check, "halted");
    crate::agents::release::halt_after(None);
    replay(&tx).unwrap();
}
fn the_view() -> Value {
    json!({"pane": "%8", "window": "@1", "startCommand": "hc attach", "gen": 1,
        "hcAddress": cx::addr(cx::B), "hcName": lf::hc_name(1)})
}
fn view_write(tx: &Tx<'_>) -> Res<()> {
    crate::agents::team_state::view(tx.ctx, &tx.scope, cx::B, &the_view())
}
fn promoted(tx: &Tx<'_>, completing: Option<Value>) -> Res<()> {
    let launch = json!({"hcName": lf::hc_name(2), "launcherArgv": lf::ARGV, "sessionId": lf::SID});
    let r = crate::agents::gens::record(&tx.folder(cx::B))
        .known()?
        .unwrap();
    let team = crate::agents::text(r.get("team"));
    let got = tx.promote(cx::B, 2, &launch, &cx::addr_at(cx::B, 2), &team, completing)?;
    assert!(got.is_some(), "promoted");
    Ok(())
}
/// Release-1d R4's write: the promotion that records `completing` (group B's `Tx::promote`).
fn release_write(tx: &Tx<'_>) -> Res<()> {
    promoted(tx, Some(json!({"gen": 1})))
}
fn promote_write(tx: &Tx<'_>) -> Res<()> {
    promoted(tx, None)
}
type Writer = (&'static str, fn(&Tx<'_>) -> Res<()>);
type Pause = (&'static str, fn(&crate::ctx::Ctx, &Scope));

// A-1, A5 (findings 1-17, 1-19): each real writer is paused at its entry, after the caller read B under E; adoption
// commits B under A inside the pause (directly, or by replay after a crash after its journal); the writer resumes.
// B's parent is A at the recomputed depth, and the writer's own fields are kept: the view descriptor exactly and
// presented, `completing`, the promoted generation.
#[test]
fn a_paused_writer_keeps_the_adopted_topology() {
    let writers: [Writer; 3] = [
        ("view", view_write),
        ("release", release_write),
        ("promote", promote_write),
    ];
    let pauses: [Pause; 2] = [("direct", adopt_in_pause), ("replayed", halted_in_pause)];
    for (name, write) in writers {
        for (how, pause) in pauses {
            let case = format!("{name} {how}");
            let (w, s) = world();
            ax::team(&w);
            if name != "view" {
                ax::set(&w, cx::B, "pending", lf::pending(2));
                lf::seed_gen(&w.folder(cx::B), 2, Some("run"), lf::committed());
            }
            let old = ax::parent_of(&w, cx::B);
            assert_eq!(
                old.as_deref(),
                Some(ax::E),
                "{case}: the caller read the old tree"
            );
            crate::agents::team_state::pause_writes(Some(pause));
            write(&cx::tx(&w, &s)).unwrap();
            crate::agents::team_state::pause_writes(None);
            assert_eq!(at(&w, cx::B), (Some(cx::A.into()), 2), "{case}");
            let r = ax::record(&w, cx::B);
            match name {
                "view" => {
                    let v = crate::state::fs::read_json(&w.folder(cx::B).join("view.json"));
                    assert_eq!(v.unwrap().unwrap(), the_view(), "{case}: the descriptor");
                    let crate::agents::gens::Member::Life(g) =
                        crate::agents::gens::member_of(&w.folder(cx::B))
                    else {
                        panic!("{case}: B is a lifecycle member")
                    };
                    let shown =
                        crate::agents::team_state::presented(&g, &g.status().unwrap()).unwrap();
                    assert_eq!(shown["pane"], "%8", "{case}: presented");
                }
                "release" => assert_eq!(
                    (&r["completing"], &r["generation"]),
                    (&json!({"gen": 1}), &json!("2")),
                    "{case}"
                ),
                _ => assert_eq!(
                    (r.get("pending"), &r["generation"]),
                    (None, &json!("2")),
                    "{case}"
                ),
            }
        }
    }
}

// A5, AD-R1 (finding 1-18): release, settle and `Tx::promote` code never names a topology field.
#[test]
fn release_and_promote_never_name_topology_fields() {
    let read =
        |f: &str| std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(f)).unwrap();
    let state = read("src/agents/team_state.rs");
    let start = state.find("pub fn promote(").expect("Tx::promote");
    let promote = &state[start..start + state[start..].find("\n    }\n").unwrap()];
    let bodies = [
        ("release.rs", read("src/agents/release.rs")),
        ("settle.rs", read("src/agents/settle.rs")),
    ];
    let mut code: Vec<(&str, String)> = (bodies.iter())
        .map(|(n, t)| (*n, t.split("mod tests").next().unwrap().to_string()))
        .collect();
    code.push(("Tx::promote", promote.to_string()));
    for (name, text) in code {
        for field in ["\"parentAgent\"", "\"parentPane\"", "\"depth\""] {
            assert!(!text.contains(field), "{name} names {field}");
        }
    }
}
