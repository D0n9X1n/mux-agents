//! restore-6b unit tests (fake hc, proc, clock; real files and locks).
use super::gens::Verdict;
use super::lifecycle::Life;
use super::reconcile::{Row, Seen, classify};

fn seen(life: Option<Life>, verdict: Option<Verdict>) -> Seen {
    Seen {
        life,
        verdict,
        ..Seen::default()
    }
}
fn failed(r: &str) -> Option<Verdict> {
    Some(Verdict::Failed(r.into()))
}

/// L2: every row in spec order. Each case fails when only its rule is removed (mutation rows 1.1-1.10).
#[test]
fn every_row_in_order() {
    let lost_ok = seen(Some(Life::Lost), Some(Verdict::Ok));
    let awaiting = Seen {
        cause: Some("restore".into()),
        released: true,
        ..seen(None, Some(Verdict::Ok))
    };
    let cases: Vec<(&str, Seen, Row)> = vec![
        (
            "stopped beats awaiting",
            Seen {
                stopped: true,
                ..awaiting.clone()
            },
            Row::Stopped,
        ),
        (
            "stopped beats lost ok",
            Seen {
                stopped: true,
                ..lost_ok.clone()
            },
            Row::Stopped,
        ),
        (
            "pending is unknown",
            Seen {
                pending: true,
                ..awaiting.clone()
            },
            Row::Unknown("pending".into()),
        ),
        (
            "half release is unknown",
            Seen {
                cause: Some("restore".into()),
                ..seen(Some(Life::Lost), Some(Verdict::Ok))
            },
            Row::Unknown("release".into()),
        ),
        ("awaiting", awaiting.clone(), Row::Await),
        (
            "awaiting, outcome undecided",
            Seen {
                verdict: None,
                ..awaiting.clone()
            },
            Row::Held("outcome".into()),
        ),
        (
            "awaiting, outcome failed",
            Seen {
                verdict: failed("client exited before readiness"),
                ..awaiting.clone()
            },
            Row::Recover("client exited before readiness".into()),
        ),
        (
            "recovery",
            Seen {
                cause: Some("recovery".into()),
                ..awaiting.clone()
            },
            Row::Recover("recovery".into()),
        ),
        (
            "lost cause is ended",
            Seen {
                cause: Some("lost".into()),
                ..awaiting.clone()
            },
            Row::Ended,
        ),
        (
            "ended marker",
            Seen {
                ended: true,
                ..lost_ok.clone()
            },
            Row::Ended,
        ),
        (
            "hc unclear",
            seen(Some(Life::Unknown), None),
            Row::Unknown("hc".into()),
        ),
        (
            "no terminal read",
            seen(None, None),
            Row::Unknown("hc".into()),
        ),
        (
            "running",
            seen(Some(Life::Running), failed("terminal lost")),
            Row::Running,
        ),
        ("lost ok", lost_ok.clone(), Row::Lost),
        (
            "lost failed",
            seen(Some(Life::Lost), failed("terminal lost")),
            Row::Recover("terminal lost".into()),
        ),
        (
            "lost void",
            seen(
                Some(Life::Lost),
                Some(Verdict::Void("commit aborted".into())),
            ),
            Row::Recover("commit aborted".into()),
        ),
        (
            "lost undecided",
            seen(Some(Life::Lost), None),
            Row::Recover("outcome undecided".into()),
        ),
        ("ended life", seen(Some(Life::Ended), None), Row::Ended),
    ];
    for (name, s, want) in cases {
        assert_eq!(classify(&s), want, "{name}");
    }
}
