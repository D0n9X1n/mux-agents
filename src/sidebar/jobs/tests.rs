use super::*;
use crate::agents::fixture::{ID, World};
use serde_json::json;
use std::time::Duration;

// A job runs on its own thread: while it has not answered, a drain returns at once with nothing and its row stays
// pending; its answer comes on a later drain, and the row is free again.
#[test]
fn a_job_runs_off_the_loop_and_its_answer_comes_on_a_later_drain() {
    let w = World::human("");
    let (caller, mut jobs) = (w.caller(), Jobs::default());
    let (gate, wait) = channel::<()>();
    let target = Target::new("worker", "");
    jobs.pending.insert(target.id.clone());
    jobs.start(target.clone(), move || {
        wait.recv().unwrap();
        Ok(Map::new())
    });
    assert!(
        jobs.drain(&w.ctx, &caller).is_empty(),
        "the drain never waits"
    );
    assert!(jobs.pending(&target.id));
    gate.send(()).unwrap();
    let mut got = Vec::new();
    for _ in 0..500 {
        got = jobs.drain(&w.ctx, &caller);
        if !got.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(got.len(), 1);
    assert!(got[0].1.is_ok());
    assert!(!jobs.pending(&target.id));
}

// A job that panics answers a refusal; the row does not stay pending for ever.
#[test]
fn a_job_that_panics_answers_a_refusal() {
    let w = World::human("");
    let (caller, mut jobs) = (w.caller(), Jobs::default());
    jobs.start(Target::new("worker", ""), || panic!("boom"));
    let mut got = Vec::new();
    for _ in 0..500 {
        got = jobs.drain(&w.ctx, &caller);
        if !got.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let e = got.pop().expect("an answer").1.unwrap_err();
    assert_eq!(
        (e.check, e.message.as_str()),
        ("internal", "unexpected error (panic)")
    );
}

// One action per row: a second resume of a pending row starts nothing; the row is marked `resuming` on the board the
// bar draws, and only while it runs.
#[test]
fn one_action_per_row_and_its_row_is_marked_while_it_runs() {
    let w = World::human("");
    let (caller, mut jobs) = (w.caller(), Jobs::default());
    let target = Target {
        id: ID.into(),
        life: "ended".into(),
        ..Target::new("worker", "")
    };
    let board = json!({"agents": [{"id": ID, "name": "worker", "state": "stopped"},
        {"id": "bbbbbbbbbbbb", "name": "other", "state": "running"}]});
    assert_eq!(
        jobs.mark(board.as_object()),
        None,
        "nothing runs: the board as it is"
    );
    assert!(jobs.resume(&w.ctx, &caller, &target));
    assert!(
        !jobs.resume(&w.ctx, &caller, &target),
        "a second one starts nothing"
    );
    assert_eq!(jobs.deferred.len(), 1);
    let marked = jobs.mark(board.as_object()).unwrap();
    assert_eq!(marked["agents"][0]["resuming"], json!(true));
    assert!(marked["agents"][1].get("resuming").is_none());
}

// A worker's `Ctx` carries the bar's fields over the host's own providers.
#[test]
fn a_worker_ctx_carries_the_bars_fields() {
    let w = World::human("");
    let host = Seed::of(&w.ctx).host();
    assert_eq!(
        (
            &host.pid,
            &host.cwd,
            &host.home,
            &host.state_root,
            &host.exe
        ),
        (
            &w.ctx.pid,
            &w.ctx.cwd,
            &w.ctx.home,
            &w.ctx.state_root,
            &w.ctx.exe
        )
    );
    assert_eq!((host.admitted, host.tty), (w.ctx.admitted, w.ctx.tty));
    assert_eq!(host.env.get("TMUX_PANE"), w.ctx.env.get("TMUX_PANE"));
    assert_eq!(*host.member_mux.borrow(), *w.ctx.member_mux.borrow());
    assert!(host.mux.borrow().is_none());
}
