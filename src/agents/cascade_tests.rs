use super::*;
use crate::agents::fixture::{ID, World};
use crate::agents::team_state::{Scope, open};

type Case = (&'static str, fn(&std::path::Path));

// C1: absent reads running; each written word reads back; anything else refuses. Unreadable never means running.
#[test]
fn desired_of_is_three_valued() {
    let w = World::human("");
    let s = Scope::of(&w.caller());
    let m = w.folder(ID);
    std::fs::create_dir_all(&m).unwrap();
    assert_eq!(desired_of(&m).unwrap(), Desired::Running, "absent");
    open(&w.ctx, &s, |tx| tx.desired(ID, false)).unwrap();
    assert_eq!(desired_of(&m).unwrap(), Desired::Stopped, "stopped");
    open(&w.ctx, &s, |tx| tx.desired(ID, true)).unwrap();
    assert_eq!(desired_of(&m).unwrap(), Desired::Running, "running");
    let cases: [Case; 3] = [
        ("torn", |m| {
            std::fs::write(m.join("desired.json"), b"{\"desi").unwrap()
        }),
        ("folder", |m| {
            std::fs::create_dir(m.join("desired.json")).unwrap()
        }),
        ("unknown word", |m| {
            std::fs::write(m.join("desired.json"), br#"{"desired":"paused"}"#).unwrap()
        }),
    ];
    for (case, plant) in cases {
        let _ = std::fs::remove_file(m.join("desired.json"));
        plant(&m);
        let want = if case == "unknown word" {
            "internal"
        } else {
            "evidence"
        };
        assert_eq!(desired_of(&m).unwrap_err().check, want, "{case}");
        let _ = std::fs::remove_dir(m.join("desired.json"));
    }
    // R5 (call 19): a linked member folder refuses through the walk; the link's target is never read.
    let target = w._dir.path().join("elsewhere");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("desired.json"), br#"{"desired":"stopped"}"#).unwrap();
    let linked = w.folder("bbbbbbbbbbbb");
    crate::testing::link_dir(&target, &linked);
    assert_eq!(
        desired_of(&linked).unwrap_err().check,
        "evidence",
        "a linked member folder"
    );
}
