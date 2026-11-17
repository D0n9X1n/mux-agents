//! reentry-6a E3: repair, reuse, `pending` then `new-session`, publish; never a kill (6a-R3).
use super::index::{self, Index, Inputs, Pending, Server, View};
use super::publish::{self, test_sessions};
use crate::agents::fixture::World;
use crate::caller::Anchor;

const T: &str = "1111111111111111";
const OTHER: &str = "/tmp/rmux-501/other";

fn here(w: &World) -> Server {
    index::server_of(&w.caller())
}
/// A second server, live at pid 5252 start 7 (a persistent process row; `Fakes::opens` does not repeat).
fn other(w: &World) -> Server {
    w.fakes.process(crate::testing::row(5252, 1, 7));
    Server {
        socket: OTHER.into(),
        pid: 5252,
        start: 7,
        key: index::key_of(OTHER, 5252, 7),
    }
}
fn release(w: &World, n: u8) -> String {
    w.ctx
        .state_root
        .join("up")
        .join(format!("{n}.release"))
        .to_str()
        .unwrap()
        .into()
}
fn base(view: Option<View>, pending: Option<Pending>) -> Index {
    Index {
        team: T.into(),
        inputs: Inputs {
            client: "claude".into(),
            options: vec![],
            prompt: vec![],
            name: String::new(),
            folder: "/p".into(),
        },
        anchor: None,
        view,
        pending,
        aliases: vec![],
        releases: vec![],
    }
}
/// A `list-sessions` answer: exit 0 and one `<id> <name>` line per row.
fn listing(rows: &[(&str, &str)]) -> Option<publish::Listing> {
    Some((
        0,
        rows.iter()
            .map(|(i, n)| format!("{i} {n}{}", char::from(10u8)))
            .collect(),
    ))
}
/// 6a-R3: no multiplexer call kills anything.
fn no_kills(w: &World) {
    let kills = w
        .fakes
        .mux_calls()
        .into_iter()
        .filter(|c| {
            c.iter().any(|a| {
                ["kill-session", "kill-window", "kill-pane", "kill-server"].contains(&a.as_str())
            })
        })
        .count();
    assert_eq!(kills, 0, "6a-R3: nothing killed");
}
fn released(r: &str) -> bool {
    matches!(
        crate::state::evidence::read(std::path::Path::new(r)),
        crate::state::evidence::Evidence::Present(_)
    )
}

/// E3.1: exactly one session named `pending.name` on the pending server becomes `view` (the old view an alias),
/// `pending` dropped and its release moved into `releases` in the same write (calls 25, 35), `anchor` set if absent.
/// Calls 16, 34: a server that restarted while it was listed, or another server answering, publishes nothing. An
/// unrelated session name with spaces is a valid row (call 36).
#[test]
fn a_pending_session_found_by_name_is_published() {
    let w = World::human("");
    let me = w.caller();
    let s = other(&w);
    test_sessions(OTHER, listing(&[("$5", "p-11111111-a"), ("$6", "my work")]));
    let old = View {
        server: here(&w),
        session: "$3".into(),
        name: "old".into(),
    };
    let p = Pending {
        server: s.clone(),
        name: "p-11111111-a".into(),
        release: release(&w, 1),
    };
    let mut ix = base(Some(old.clone()), Some(p.clone()));
    publish::repair(&w.ctx, &me, &mut ix).unwrap();
    assert_eq!(
        ix.releases,
        vec![release(&w, 1)],
        "call 35: its release kept durably for the end of up"
    );
    assert_eq!(
        ix.view,
        Some(View {
            server: s.clone(),
            session: "$5".into(),
            name: "p-11111111-a".into()
        })
    );
    assert_eq!(
        (ix.pending.as_ref(), ix.aliases.as_slice()),
        (None, std::slice::from_ref(&old))
    );
    assert_eq!(
        ix.anchor,
        Some(Anchor {
            key: s.key.clone(),
            session: "$5".into()
        }),
        "first publication anchors"
    );
    assert!(
        !released(&release(&w, 1)),
        "published only at the end of up"
    );
    assert_eq!(
        index::read(&w.ctx, T).unwrap(),
        Some(ix.clone()),
        "written, releases included"
    );
    w.fakes.opens(5252, &[Some(7), Some(8)]); // proven, then restarted when re-proven after the listing
    let mut again = base(None, Some(p.clone()));
    assert_eq!(
        publish::repair(&w.ctx, &me, &mut again).unwrap_err().check,
        "index",
        "call 16: re-proven"
    );
    assert_eq!(
        (again.view, again.pending.as_ref()),
        (None, Some(&p)),
        "nothing published, pending kept"
    );
    publish::test_answering(OTHER, Some(5353));
    let mut third = base(None, Some(p.clone()));
    assert_eq!(
        publish::repair(&w.ctx, &me, &mut third).unwrap_err().check,
        "index",
        "call 34: another server answered"
    );
    assert_eq!(
        (third.view, third.pending),
        (None, Some(p)),
        "nothing published"
    );
    no_kills(&w);
}

/// E3.1: none of that name: `pending` dropped, its release moved into `releases` (call 35), after the answering
/// server is proven (call 34, finding A3); several: 1 `index`, nothing killed, `pending` kept. Call 16: a failed
/// listing is never "zero sessions", a malformed row is `index`.
#[test]
fn zero_names_release_and_two_names_refuse_index() {
    let w = World::human("");
    let me = w.caller();
    let s = other(&w);
    let p = Pending {
        server: s.clone(),
        name: "p-11111111-a".into(),
        release: release(&w, 1),
    };
    test_sessions(OTHER, listing(&[("$6", "x")]));
    let mut ix = base(None, Some(p.clone()));
    publish::repair(&w.ctx, &me, &mut ix).unwrap();
    assert_eq!(
        (ix.pending.as_ref(), ix.view.as_ref(), ix.releases.clone()),
        (None, None, vec![release(&w, 1)]),
        "dropped, release kept"
    );
    assert!(
        !released(&release(&w, 1)),
        "published only at the end of up"
    );
    w.fakes.opens(5252, &[Some(7), Some(8)]);
    let mut moved = base(None, Some(p.clone()));
    assert_eq!(
        publish::repair(&w.ctx, &me, &mut moved).unwrap_err().check,
        "index",
        "call 34: the zero-match path re-proves"
    );
    assert_eq!(
        (moved.pending.as_ref(), moved.releases.len()),
        (Some(&p), 0),
        "kept, nothing moved"
    );
    let p2 = Pending {
        release: release(&w, 2),
        ..p
    };
    let failures = [
        (
            listing(&[("$5", "p-11111111-a"), ("$7", "p-11111111-a")]),
            "two of that name",
        ),
        (Some((1, String::new())), "a failed listing"),
        (
            Some((0, format!("garbage{}", char::from(10u8)))),
            "a malformed row",
        ),
    ];
    for (answer, what) in failures {
        test_sessions(OTHER, answer);
        let mut ix = base(None, Some(p2.clone()));
        assert_eq!(
            publish::repair(&w.ctx, &me, &mut ix).expect_err(what).check,
            "index",
            "{what}"
        );
        assert_eq!(
            (ix.pending.as_ref(), ix.releases.len()),
            (Some(&p2), 0),
            "{what}: kill nothing, keep it"
        );
    }
    no_kills(&w);
}

/// E3.1: a dead pending server: dropped, its release moved into `releases`, its sessions never listed; aliases on
/// proven-gone servers go, live ones stay.
#[test]
fn a_dead_pending_server_is_released_and_gone_aliases_dropped() {
    let w = World::human("");
    let me = w.caller();
    let gone = Server {
        socket: "/tmp/gone".into(),
        pid: 5151,
        start: 3,
        key: index::key_of("/tmp/gone", 5151, 3),
    };
    test_sessions("/tmp/gone", None); // listing it panics the seam
    let live = View {
        server: other(&w),
        session: "$2".into(),
        name: "a".into(),
    };
    let dead = View {
        server: gone.clone(),
        session: "$1".into(),
        name: "b".into(),
    };
    let mut ix = base(
        None,
        Some(Pending {
            server: gone,
            name: "n".into(),
            release: release(&w, 3),
        }),
    );
    ix.anchor = Some(Anchor {
        key: live.server.key.clone(),
        session: "$2".into(),
    });
    ix.aliases = vec![live.clone(), dead];
    publish::repair(&w.ctx, &me, &mut ix).unwrap();
    assert!(ix.releases == vec![release(&w, 3)] && ix.pending.is_none());
    assert_eq!(ix.aliases, vec![live], "only the proven-gone alias goes");
    no_kills(&w);
}

/// F20, calls 16, 36: every recovery transport failure is 1 `index` and leaves the durable index byte-identical:
/// another server's endpoint that cannot be built (no multiplexer image), the caller's own server's listing failing
/// in transport, and another server's answering-pid probe that cannot run.
#[test]
fn a_recovery_transport_failure_is_index_and_keeps_pending() {
    let w = World::human("");
    let me = w.caller();
    let s = other(&w);
    let p = Pending {
        server: s,
        name: "p-11111111-a".into(),
        release: release(&w, 1),
    };
    let own = Pending {
        server: here(&w),
        ..p.clone()
    };
    let unchanged = |ix: Index, what: &str| {
        index::write(&w.ctx, &ix).unwrap();
        let before = std::fs::read(index::path(&w.ctx, T)).unwrap();
        let mut probe = ix.clone();
        let e = publish::repair(&w.ctx, &me, &mut probe).unwrap_err();
        assert_eq!(e.check, "index", "{what}: {}", e.message);
        assert_eq!(probe, ix, "{what}: nothing changed");
        let after = std::fs::read(index::path(&w.ctx, T)).unwrap();
        assert!(
            before == after,
            "{what}: the durable index is byte-identical"
        );
    };
    // no seam: the real endpoint of another server; its process row has no multiplexer image
    unchanged(base(None, Some(p.clone())), "endpoint construction");
    w.fakes.mux_script("list-sessions", |_| {
        Err(crate::error::Refusal::internal("transport"))
    });
    unchanged(base(None, Some(own)), "own listing transport");
    test_sessions(OTHER, listing(&[("$5", "p-11111111-a")]));
    publish::test_answering(OTHER, None); // the real probe, which cannot build the endpoint
    unchanged(base(None, Some(p)), "answering-pid probe");
    no_kills(&w);
}

/// E5, calls 17, 25, 35: every outstanding release is published and read back `Present`, then dropped from the
/// index in one write; a torn one stays listed and refuses `index` naming it, the others still go. F21: a
/// directory where a marker belongs stays listed too.
#[test]
fn release_all_publishes_each_release_and_keeps_a_torn_one() {
    let w = World::human("");
    let torn = w.ctx.state_root.join("up").join("t.release");
    let mut ix = base(None, None);
    ix.releases = vec![
        release(&w, 1),
        torn.to_str().unwrap().into(),
        release(&w, 2),
    ];
    index::write(&w.ctx, &ix).unwrap();
    std::fs::write(&torn, b"{").unwrap();
    let e = publish::release_all(&w.ctx, &mut ix).unwrap_err();
    assert_eq!(
        (e.check, e.message),
        (
            "index",
            format!(
                "the release {} cannot be read; repair or remove it",
                torn.display()
            )
        )
    );
    assert!(
        released(&release(&w, 1)) && released(&release(&w, 2)),
        "the others published"
    );
    assert_eq!(
        ix.releases,
        vec![torn.to_str().unwrap().to_string()],
        "the torn one stays listed"
    );
    assert_eq!(
        index::read(&w.ctx, T).unwrap().unwrap().releases,
        ix.releases,
        "written"
    );
    std::fs::remove_file(&torn).unwrap();
    publish::release_all(&w.ctx, &mut ix).unwrap();
    assert!(
        ix.releases.is_empty() && released(torn.to_str().unwrap()),
        "finished by the next call"
    );
    let dir = w.ctx.state_root.join("up").join("d.release");
    std::fs::create_dir(&dir).unwrap();
    ix.releases = vec![dir.to_str().unwrap().into()];
    index::write(&w.ctx, &ix).unwrap();
    assert_eq!(
        publish::release_all(&w.ctx, &mut ix)
            .expect_err("F21: a directory marker")
            .check,
        "index",
        "F21: a directory marker"
    );
    assert_eq!(
        index::read(&w.ctx, T).unwrap().unwrap().releases,
        vec![dir.to_str().unwrap().to_string()],
        "F21: its path stays durable"
    );
}

#[cfg(test)]
mod ensure;
