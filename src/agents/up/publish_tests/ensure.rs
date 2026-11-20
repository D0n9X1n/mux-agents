//! reentry-6a E3.2-3: the view is reused, or a session is made after its pending record (Task 8).
use super::super::index::{self, View};
use super::super::publish;
use super::{T, base, here, no_kills, released};
use crate::agents::fixture::World;
use crate::caller::Anchor;

fn listed(w: &World, rows: &str) {
    w.fakes.mux_rule("list-sessions", 0, rows, "");
}
/// The World server answers `server::same`'s probe with its own pid (main's `up/tests.rs:32`; call 34).
fn own(w: &World) {
    let pid = w.caller().server_pid;
    w.fakes
        .mux_rule("up #{pid}", 0, &format!("up {pid}{}", char::from(10u8)), "");
}
fn made(w: &World) -> usize {
    w.fakes
        .mux_calls()
        .iter()
        .filter(|c| c.iter().any(|x| x == "new-session"))
        .count()
}

/// What `list-panes` answers for the view session `$3`: exit code and `<dead>|<pid>` rows.
fn panes(w: &World, code: i32, rows: &str) {
    w.fakes.mux_rule("list-panes -s -t $3 -F", code, rows, "");
}
/// A process the fake table lists as this binary's side bar, `<exe> watch`.
fn bar(w: &World, pid: u32) {
    w.fakes.process(crate::proc::Row {
        path: Some("/bin/mux-agent".into()),
        argv: Some(vec!["/bin/mux-agent".into(), "watch".into()]),
        ..crate::testing::row(pid, 1, 9)
    });
}

/// E3.2: a proven live view on the caller's server whose session still exists and holds a live view (here one pane
/// beside its side bar) is reused: no session, no write.
#[test]
fn a_live_view_on_this_server_is_reused() {
    let w = World::human("");
    let me = w.caller(); // before the index exists: after Task 3 `w.caller()` admits through it (finding A9)
    own(&w);
    listed(&w, "$3 p-11111111-v\n");
    w.fakes.process(crate::testing::row(6000, 1, 9));
    bar(&w, 6001);
    panes(&w, 0, "0|6001\n0|6000\n");
    let v = View {
        server: here(&w),
        session: "$3".into(),
        name: "p-11111111-v".into(),
    };
    let mut ix = base(Some(v.clone()), None);
    ix.anchor = Some(Anchor {
        key: "a1a1a1a1a1a1a1a1".into(),
        session: "$A".into(),
    });
    index::write(&w.ctx, &ix).unwrap();
    let got = publish::ensure(&w.ctx, &me, &mut ix).unwrap();
    assert_eq!((got.view, ix.releases.len()), (v, 0));
    assert_eq!(made(&w), 0, "reused");
    no_kills(&w);
}

/// A listed view session is reused only while it holds a live view: a pane not dead whose process the process table
/// lists, not ended, and that is not a side bar. After a core kill its `hc attach` views end at once and its bars
/// leave a moment later, so a session holding only those would vanish right after `up` answered it: a new session is
/// made, the old view an alias.
#[test]
fn a_view_session_holding_no_live_view_is_not_reused() {
    for case in [
        "dead",
        "gone",
        "ended",
        "unlisted",
        "bar",
        "empty",
        "failed",
        "transport",
        "table",
    ] {
        let w = World::human("");
        let me = w.caller();
        own(&w);
        listed(&w, "$3 p-11111111-v\n");
        match case {
            "dead" => w.fakes.process(crate::testing::row(6000, 1, 9)),
            "ended" => {
                w.fakes.process(crate::testing::row(6000, 1, 9));
                w.fakes.ended(6000);
            }
            // Windows: a handle another process holds keeps an exited process openable, so `row` still answers
            // while the process table no longer lists it.
            "unlisted" => {
                w.fakes.process(crate::testing::row(6000, 1, 9));
                w.fakes.unlist(6000);
            }
            "bar" => bar(&w, 6000),
            "table" => w.fakes.process(crate::testing::row(6000, 1, 9)),
            _ => {}
        }
        match case {
            "dead" => panes(&w, 0, "1|6000\n"),
            "empty" => panes(&w, 0, ""),
            "failed" => panes(&w, 1, ""),
            // The pane listing does not run at all: the multiplexer cannot be reached.
            "transport" => w.fakes.mux_script("list-panes -s -t $3 -F", |_| {
                Err(crate::error::Refusal::internal(
                    "the multiplexer did not answer",
                ))
            }),
            _ => panes(&w, 0, "0|6000\n"),
        }
        w.fakes.mux_rule("new-session", 0, "$7 @3\n", "");
        let v = View {
            server: here(&w),
            session: "$3".into(),
            name: "p-11111111-v".into(),
        };
        let mut ix = base(Some(v.clone()), None);
        ix.anchor = Some(Anchor {
            key: "a1a1a1a1a1a1a1a1".into(),
            session: "$A".into(),
        });
        if case == "table" {
            // After the caller and its server are resolved: only `held`'s read of the process table fails.
            w.fakes.table_fails();
        }
        let got = publish::ensure(&w.ctx, &me, &mut ix)
            .unwrap_or_else(|e| panic!("{case}: up does not abort: {e:?}"));
        assert_eq!(got.view.session, "$7", "{case}: a new session");
        assert_eq!(ix.aliases, vec![v], "{case}: the old view an alias");
        assert_eq!(made(&w), 1, "{case}: one session made");
        no_kills(&w);
    }
}

/// F4, call 34: a reuse whose server restarted while its sessions were listed is not reused: 1 `index`, no
/// session made, nothing written.
#[test]
fn a_view_whose_server_restarted_during_the_listing_is_not_reused() {
    let w = World::human("");
    let me = w.caller();
    listed(&w, "$3 p-11111111-v\n");
    own(&w);
    w.fakes.process(crate::testing::row(6000, 1, 9));
    panes(&w, 0, "0|6000\n");
    let v = View {
        server: index::server_of(&me),
        session: "$3".into(),
        name: "p-11111111-v".into(),
    };
    // after `v`: re-resolving the caller (`here`) would take these; proven live, then restarted at `answered`
    w.fakes.opens(
        me.server_pid,
        &[Some(me.server_start), Some(me.server_start + 1)],
    );
    let mut ix = base(Some(v.clone()), None);
    index::write(&w.ctx, &ix).unwrap();
    let before = std::fs::read(index::path(&w.ctx, T)).unwrap();
    let e = publish::ensure(&w.ctx, &me, &mut ix).expect_err("F4: re-proven after the listing");
    assert_eq!(e.check, "index");
    assert_eq!((ix.view, ix.pending), (Some(v), None), "nothing changed");
    assert_eq!(made(&w), 0, "no session made");
    assert!(
        before == std::fs::read(index::path(&w.ctx, T)).unwrap(),
        "nothing written"
    );
    no_kills(&w);
}

/// E3.3: `pending` is on disk before `new-session`, which runs `hold <release>`; with the reply's id: published;
/// a view whose session was killed (its id no longer listed) moves to the aliases; the anchor never changes.
#[test]
fn a_new_session_is_made_after_its_pending_record() {
    let w = World::human("");
    let me = w.caller();
    own(&w);
    listed(&w, "$1 other\n");
    let at = index::path(&w.ctx, T);
    w.fakes.mux_script("new-session", move |args| {
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&at).expect("index on disk")).unwrap();
        let name = v["pending"]["name"]
            .as_str()
            .expect("pending is durable before new-session");
        let rel = v["pending"]["release"].as_str().unwrap();
        assert!(
            args.iter().any(|a| a == name)
                && args.iter().any(|a| a == "hold")
                && args.iter().any(|a| a == rel)
        );
        Ok(crate::mux::Output {
            code: 0,
            stdout: "$7 @3\n".into(),
            stderr: String::new(),
        })
    });
    let dead = View {
        server: here(&w),
        session: "$3".into(),
        name: "killed".into(),
    };
    let anchor = Anchor {
        key: "a1a1a1a1a1a1a1a1".into(),
        session: "$A".into(),
    };
    let mut ix = base(Some(dead.clone()), None);
    ix.anchor = Some(anchor.clone());
    let got = publish::ensure(&w.ctx, &me, &mut ix).unwrap();
    assert_eq!(got.view.session, "$7");
    assert_eq!(got.view.server.key, me.server_key);
    assert_eq!(
        ix.releases.len(),
        1,
        "call 35: the release kept in the index for the end of up"
    );
    assert!(
        !released(&ix.releases[0]),
        "never released before the views open"
    );
    let back = index::read(&w.ctx, T).unwrap().unwrap();
    assert_eq!(back.releases, ix.releases, "durable with the publication");
    assert_eq!(back.view, Some(got.view));
    assert_eq!(back.pending, None);
    assert_eq!(back.aliases, vec![dead]);
    assert_eq!(back.anchor, Some(anchor), "anchor unchanged");
    no_kills(&w);
}

/// E-1 (spec), call 16: the `new-session` reply is lost; `up` lists sessions by its name. One: published with its
/// real id. None: released, then `index` (UNPROVEN). Two, or a failed listing: `index`, `pending` kept. Exactly one
/// session is made in every case (row 8.6).
#[test]
fn a_lost_reply_is_repaired_by_name() {
    for case in ["one", "zero", "two", "failed"] {
        let w = World::human("");
        let me = w.caller();
        own(&w);
        let at = index::path(&w.ctx, T);
        let answer = std::rc::Rc::new(std::cell::RefCell::new((0, String::new())));
        let a = answer.clone();
        w.fakes.mux_script("list-sessions", move |_| {
            let (code, stdout) = a.borrow().clone();
            Ok(crate::mux::Output {
                code,
                stdout,
                stderr: String::new(),
            })
        });
        let a = answer.clone();
        w.fakes.mux_script("new-session", move |_| {
            let v: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&at).unwrap()).unwrap();
            let (name, nl) = (
                v["pending"]["name"].as_str().unwrap().to_string(),
                char::from(10u8),
            );
            *a.borrow_mut() = match case {
                "one" => (0, format!("$9 {name}{nl}")),
                "zero" => (0, String::new()),
                "two" => (0, format!("$9 {name}{nl}$10 {name}{nl}")),
                _ => (1, String::new()),
            };
            Ok(crate::mux::Output {
                code: 1,
                stdout: String::new(),
                stderr: "lost".into(),
            })
        });
        let mut ix = base(None, None);
        let got = publish::ensure(&w.ctx, &me, &mut ix);
        if case == "one" {
            assert_eq!(got.unwrap().view.session, "$9", "{case}");
        } else {
            let e = got.expect_err(case);
            assert_eq!(
                (e.check, e.message.as_str()),
                ("index", publish::UNPROVEN),
                "{case}"
            );
            assert_eq!(
                ix.pending.is_some(),
                case != "zero",
                "{case}: dropped only when the listing proves none"
            );
            assert_eq!(
                ix.releases.len(),
                usize::from(case == "zero"),
                "{case}: a dropped pending keeps its release"
            );
        }
        assert_eq!(made(&w), 1, "{case}: one session made");
        no_kills(&w);
    }
}

/// Call 16: the server restarted between the caller's proof and the reply: nothing is published.
#[test]
fn a_session_made_by_a_restarted_server_is_not_published() {
    let w = World::human("");
    let me = w.caller();
    w.fakes.mux_rule("list-sessions", 0, "", "");
    w.fakes.mux_rule("new-session", 0, "$7 @3\n", "");
    w.fakes.opens(me.server_pid, &[Some(me.server_start + 1)]);
    let mut ix = base(None, None);
    let e = publish::ensure(&w.ctx, &me, &mut ix).expect_err("call 16: restarted");
    assert_eq!(e.check, "index");
    assert!(
        ix.view.is_none() && ix.anchor.is_none() && ix.pending.is_some(),
        "nothing published, pending kept"
    );
    // Call 34 (finding A2): the endpoint answers another pid, the recorded process still alive: nothing published.
    let w = World::human("");
    let me = w.caller();
    w.fakes.mux_rule(
        "up #{pid}",
        0,
        &format!("up {}{}", me.server_pid + 1, char::from(10u8)),
        "",
    );
    w.fakes.mux_rule("list-sessions", 0, "", "");
    w.fakes.mux_rule("new-session", 0, "$7 @3\n", "");
    let mut ix = base(None, None);
    assert_eq!(
        publish::ensure(&w.ctx, &me, &mut ix)
            .expect_err("another server answered")
            .check,
        "index",
        "another server answered"
    );
    assert!(
        ix.view.is_none() && ix.pending.is_some(),
        "call 34: nothing published"
    );
}
