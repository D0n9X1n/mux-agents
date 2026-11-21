use super::*;
use crate::agents::fixture::{ID, World};
use serde_json::json;
use std::time::Duration;

#[test]
#[cfg(feature = "test-panic")]
fn reader_answer_delay_is_test_only_and_preserves_the_board_read_time() {
    let mut w = World::human("");
    w.ctx.env = crate::ctx::Env::from_pairs([("MUX_AGENTS_TEST_READER_DELAY_MS", "450")]);
    let at = w.ctx.clock.now();
    let read = super::super::timed_test_board(Map::new());
    let original = read.at;
    let delayed = delay_read_answer(&w.ctx, Ok(read)).unwrap();
    assert_eq!(
        w.ctx.clock.now().duration_since(at).unwrap(),
        Duration::from_millis(450)
    );
    assert_eq!(
        delayed.at, original,
        "delay must not restamp the age origin"
    );
}

fn spell(changed: &str) -> Seen {
    Seen {
        id: ID.into(),
        pane: "%9".into(),
        changed: changed.into(),
    }
}

#[test]
fn reader_runs_off_the_loop_and_answers_seen_before_the_next_read() {
    let (started, starting) = channel();
    let (release, gate) = channel();
    let mut reader = Reader::start(move || {
        move |request: &Request| match request {
            Request::Seen(seen) => {
                started.send(()).unwrap();
                gate.recv().unwrap();
                Answer::Seen(seen.clone(), Ok(true))
            }
            Request::Read(generation) => {
                Answer::Read(*generation, Ok(super::super::timed_test_board(Map::new())))
            }
        }
    });
    reader.ask(Request::Seen(spell("done-at")));
    reader.ask(Request::Read(1));
    starting.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(reader.answers.try_recv().is_err(), "the loop never waits");
    release.send(()).unwrap();
    let (seen, failed) = reader.answers.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(seen, Answer::Seen(seen, Ok(true)) if seen == spell("done-at")));
    assert!(!failed);
    let (read, failed) = reader.answers.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(read, Answer::Read(1, Ok(_))));
    assert!(!failed);
    reader.finish();
}

#[test]
fn finish_stops_claims_and_waits_without_a_bound_for_the_claimed_read() {
    let (started, starting) = channel();
    let (release, gate) = channel();
    let mut reader = Reader::start(move || {
        move |request: &Request| {
            assert!(
                matches!(request, Request::Read(1)),
                "queued Seen must never run"
            );
            started.send(()).unwrap();
            gate.recv().unwrap();
            Answer::Read(1, Ok(super::super::timed_test_board(Map::new())))
        }
    });
    reader.ask(Request::Read(1));
    starting.recv_timeout(Duration::from_secs(5)).unwrap();
    reader.ask(Request::Seen(spell("done-at")));
    let queue = reader.queue.clone();
    let (finished, finish) = channel();
    let thread = std::thread::spawn(move || {
        let w = World::human("");
        let mut jobs = Jobs {
            reader: Some(reader),
            ..Jobs::default()
        };
        finished
            .send(jobs.finish(&w.ctx, &w.caller(), Duration::ZERO))
            .unwrap();
    });
    let state = queue.0.lock().unwrap();
    let (state, _) = queue
        .1
        .wait_timeout_while(state, Duration::from_secs(5), |state| !state.1)
        .unwrap();
    assert!(state.1, "finish stops the queue before joining");
    assert!(state.0.is_empty());
    drop(state);
    assert!(
        finish.try_recv().is_err(),
        "even a zero jobs bound waits for the reader"
    );
    release.send(()).unwrap();
    assert!(finish.recv_timeout(Duration::from_secs(5)).unwrap());
    thread.join().unwrap();
}

#[test]
fn three_reader_failures_fall_back_inline_and_a_successful_retry_resets_them() {
    use crate::agents::core_fixture as cf;
    for failures in [1, 3] {
        let w = World::human("");
        let caller = w.caller();
        let mut jobs = Jobs::default();
        for generation in 1..=failures {
            jobs.reader = Some(Reader::start_with(
                || |_: &Request| panic!("no worker"),
                |_| Err(std::io::Error::other("no worker")),
            ));
            jobs.read(&w.ctx, &caller);
            assert!(
                matches!(jobs.drain_reader().as_slice(), [Answer::Read(g, Err(_))] if *g == generation)
            );
            assert!(!jobs.reading());
            assert!(jobs.reader.is_none(), "next tick starts a new reader");
        }
        assert_eq!(jobs.reader_failures, failures);
        cf::online(&w);
        cf::listing(&w, &crate::agents::membership::team(&caller), &[]);
        jobs.read(&w.ctx, &caller);
        assert!(matches!(
            jobs.drain_reader().as_slice(),
            [Answer::Read(_, Ok(_))]
        ));
        assert_eq!(jobs.reader_failures, if failures == 3 { 3 } else { 0 });
        w.fakes.hc_done().unwrap();
    }
}

#[test]
fn seen_requests_use_the_reader_and_keep_the_spell_until_a_true_answer() {
    let w = World::human("");
    let (started, starting) = channel();
    let (release, gate) = channel();
    let (sent, answers) = channel();
    let mut reader = Reader::start(move || {
        move |request: &Request| {
            let Request::Seen(seen) = request else {
                panic!("only seen")
            };
            started.send(()).unwrap();
            let ok = gate.recv().unwrap();
            sent.send(()).unwrap();
            Answer::Seen(seen.clone(), ok)
        }
    });
    let mut jobs = Jobs {
        reader: Some(reader),
        ..Jobs::default()
    };
    let mut viewing = super::super::Viewing::default();
    viewing.left.push(spell("done-at"));
    viewing.mark(|pane, changed| {
        jobs.seen(
            &w.ctx,
            &w.caller(),
            &Seen {
                id: ID.into(),
                pane: pane.into(),
                changed: changed.into(),
            },
        );
        false
    });
    starting.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(jobs.drain_reader().is_empty());
    assert_eq!(viewing.left.len(), 1);
    release
        .send(Err(Refusal::internal("seen refused")))
        .unwrap();
    answers.recv_timeout(Duration::from_secs(5)).unwrap();
    reader = jobs.reader.take().unwrap();
    reader.finish();
    jobs.reader = Some(reader);
    assert!(matches!(
        jobs.drain_reader().as_slice(),
        [Answer::Seen(_, Err(_))]
    ));
    assert_eq!(viewing.left.len(), 1);
    assert!(jobs.seeing.is_empty(), "a refusal allows retry");
}

#[test]
fn a_reader_panic_in_seen_counts_toward_inline_fallback() {
    let w = World::human("");
    let mut jobs = Jobs {
        reader: Some(Reader::start(|| |_: &Request| panic!("seen panic"))),
        ..Jobs::default()
    };
    jobs.seen(&w.ctx, &w.caller(), &spell("spell"));
    let reader = jobs.reader.as_mut().unwrap();
    let answer = reader.answers.recv_timeout(Duration::from_secs(5)).unwrap();
    jobs.ready.push_back(answer);
    assert!(matches!(
        jobs.drain_reader().as_slice(),
        [Answer::Seen(_, Err(_))]
    ));
    assert_eq!(jobs.reader_failures, 1);
    assert!(jobs.reader.is_none());
}

#[test]
fn a_first_tick_seen_request_starts_its_reader_before_any_background_read() {
    let w = World::human("");
    let caller = w.caller();
    let mut jobs = Jobs::default();
    // A production bar's first board is inline, so its first queued work can be a Seen.
    jobs.start_reader(&w.ctx, &caller);
    assert!(jobs.reader.is_some());
    jobs.reader.as_mut().unwrap().finish();
}

#[test]
fn hold_read_holds_only_the_read_answer_not_a_successful_seen() {
    let mut jobs = Jobs::default();
    jobs.ready.push_back((
        Answer::Read(1, Ok(super::super::timed_test_board(Map::new()))),
        false,
    ));
    jobs.ready
        .push_back((Answer::Seen(spell("spell"), Ok(true)), false));
    HOLD_READ.set(true);
    let answers = jobs.drain_reader();
    HOLD_READ.set(false);
    assert!(matches!(answers.as_slice(), [Answer::Seen(_, Ok(true))]));
    assert!(matches!(
        jobs.drain_reader().as_slice(),
        [Answer::Read(1, Ok(_))]
    ));
}

#[test]
fn failure_collection_keeps_answers_sent_while_the_failed_reader_is_joined() {
    let (started, starting) = channel();
    let (release, gate) = channel();
    let mut reader = Reader::start(move || {
        move |request: &Request| {
            started.send(()).unwrap();
            gate.recv().unwrap();
            request.refused()
        }
    });
    reader.ask(Request::Read(1));
    starting.recv_timeout(Duration::from_secs(5)).unwrap();
    let queue = reader.queue.clone();
    let unlock = std::thread::spawn(move || {
        let state = queue.0.lock().unwrap();
        let (state, _) = queue
            .1
            .wait_timeout_while(state, Duration::from_secs(5), |state| !state.1)
            .unwrap();
        assert!(state.1);
        release.send(()).unwrap();
    });
    let mut jobs = Jobs {
        reader: Some(reader),
        reading: true,
        ..Jobs::default()
    };
    // Model a failure answer already drained when the worker is still sending its queued refusals.
    jobs.ready.push_back((
        Answer::Seen(spell("spell"), Err(Refusal::internal("reader failed"))),
        true,
    ));
    let answers = jobs.drain_reader();
    unlock.join().unwrap();
    assert!(
        answers
            .iter()
            .any(|answer| matches!(answer, Answer::Read(1, Err(_))))
    );
    assert!(!jobs.reading());
}

#[test]
fn a_reader_factory_panic_refuses_work_instead_of_leaving_it_pending() {
    let mut reader =
        Reader::start(|| -> fn(&Request) -> Answer { panic!("context construction failed") });
    reader.ask(Request::Read(1));
    assert!(matches!(
        reader.answers.recv_timeout(Duration::from_secs(1)).unwrap(),
        (Answer::Read(1, Err(_)), true)
    ));
    reader.ask(Request::Seen(spell("done-at")));
    assert!(matches!(
        reader.answers.recv_timeout(Duration::from_secs(1)).unwrap(),
        (Answer::Seen(_, Err(_)), true)
    ));
    reader.finish();
}

#[test]
fn reader_start_failure_refuses_every_request() {
    let mut reader = Reader::start_with(
        || |_: &Request| panic!("no thread may run"),
        |_| Err(std::io::Error::other("thread unavailable")),
    );
    reader.ask(Request::Read(1));
    reader.ask(Request::Seen(spell("done-at")));
    assert!(matches!(
        reader.answers.recv_timeout(Duration::from_secs(5)).unwrap(),
        (Answer::Read(1, Err(_)), true)
    ));
    assert!(matches!(
        reader.answers.recv_timeout(Duration::from_secs(5)).unwrap(),
        (Answer::Seen(_, Err(_)), true)
    ));
    reader.finish();
}

#[test]
fn reader_panic_refuses_the_request_and_every_queued_request() {
    let (started, starting) = channel();
    let (release, gate) = channel();
    let mut reader = Reader::start(move || {
        move |_: &Request| {
            started.send(()).unwrap();
            gate.recv().unwrap();
            panic!("reader panic")
        }
    });
    reader.ask(Request::Read(1));
    starting.recv_timeout(Duration::from_secs(5)).unwrap();
    reader.ask(Request::Seen(spell("done-at")));
    release.send(()).unwrap();
    let (answer, failed) = reader.answers.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(answer, Answer::Read(1, Err(_))));
    assert!(failed);
    let (answer, failed) = reader.answers.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(answer, Answer::Seen(_, Err(_))));
    assert!(failed);
    reader.ask(Request::Read(2));
    assert!(matches!(
        reader.answers.recv_timeout(Duration::from_secs(5)).unwrap(),
        (Answer::Read(2, Err(_)), true)
    ));
    reader.finish();
}

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

// A watch's last step: no new work starts, and the work running is waited for, bounded: a job released before the
// bound finishes (and the call returns true only after it ran); one still running at the bound is reported (false),
// never waited for for ever.
#[test]
fn finish_waits_for_the_running_job_within_its_bound() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    let w = World::human("");
    let (caller, mut jobs) = (w.caller(), Jobs::default());
    let (gate, wait) = channel::<()>();
    let ran = Arc::new(AtomicBool::new(false));
    let done = ran.clone();
    let target = Target::new("worker", "");
    jobs.pending.insert(target.id.clone());
    jobs.start(target.clone(), move || {
        wait.recv().unwrap();
        done.store(true, SeqCst);
        Ok(Map::new())
    });
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        gate.send(()).unwrap();
    });
    assert!(jobs.finish(&w.ctx, &caller, Duration::from_secs(5)));
    assert!(ran.load(SeqCst), "it ran before finish returned");
    assert!(
        !jobs.resume(&w.ctx, &caller, &target),
        "no new work once finishing"
    );
    release.join().unwrap();
    // One that does not finish within the bound is reported, not waited for.
    let mut jobs = Jobs::default();
    let (gate, wait) = channel::<()>();
    jobs.pending.insert(target.id.clone());
    jobs.start(target, move || {
        let _ = wait.recv();
        Ok(Map::new())
    });
    assert!(!jobs.finish(&w.ctx, &caller, Duration::from_millis(100)));
    drop(gate);
}
