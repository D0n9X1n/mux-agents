use super::*;
use crate::agents::fixture::World;
use crate::parse::Client;
use crate::proc::Row;
use crate::state::root::OsKind;

// S is the fixture's SESSION_ID; X, A, B are other conversations.
const S: &str = "0f11fc2f-2032-48ef-a920-4af2a8e5af50";
const X: &str = "11111111-2222-4333-8444-555555555555";
const A: &str = "22222222-2222-4222-8222-222222222222";

/// (the rule that held, the holding pid) — PM call 2: every refusal names both.
type Who = (Rule, Option<u32>);

fn who(h: Option<Hold>) -> Option<Who> {
    h.map(|h| (h.rule, h.pid))
}

/// No exempt client; Task 4b adds the caller's (`Own`).
fn held(w: &World, s: &str, kind: Client) -> Option<Who> {
    who(held_on(&w.ctx, s, kind, OsKind::Unix, None))
}

/// Row 7001 under pid 1 with these fields.
fn row(start: Option<i64>, path: Option<&str>, argv: Option<&[&str]>, image: Option<&str>) -> Row {
    Row {
        pid: 7001,
        ppid: 1,
        start,
        path: path.map(String::from),
        argv: argv.map(|a| a.iter().map(|s| s.to_string()).collect()),
        image: image.map(String::from),
    }
}

// H4.1.
#[test]
fn an_unreadable_table_holds() {
    let w = World::human("");
    assert_eq!(
        held(&w, S, Client::Claude),
        None,
        "the fixture rows hold nothing"
    );
    w.fakes.table_fails();
    assert_eq!(held(&w, S, Client::Claude), Some((Rule::Table, None)));
    assert_eq!(held(&w, S, Client::Copilot), Some((Rule::Table, None)));
}

// H4.3: any row whose argv names S holds S, whatever its kind; a fork of S (`--fork-session`) does not.
#[test]
fn an_argv_naming_the_session_holds_whatever_its_kind() {
    let w = World::human("");
    w.fakes.process(row(
        Some(1),
        Some("/usr/bin/sh"),
        Some(&["wrapper", "--resume", S]),
        Some("sh"),
    ));
    for kind in [Client::Claude, Client::Copilot] {
        assert_eq!(
            held(&w, S, kind),
            Some((Rule::Argv, Some(7001))),
            "{kind:?}"
        );
        assert_eq!(
            held(&w, X, kind),
            None,
            "{kind:?}: a readable non-client names only S"
        );
    }
    let w = World::human("");
    w.fakes.process(row(
        Some(1),
        Some("/usr/bin/claude"),
        Some(&["claude", "--resume", S, "--fork-session", "--session-id", X]),
        Some("claude"),
    ));
    assert_eq!(held(&w, S, Client::Copilot), None, "a fork of S runs X");
    assert_eq!(held(&w, X, Client::Copilot), Some((Rule::Argv, Some(7001))));
}

// H4.4: an unmanaged readable client of `kind` holds every conversation of that kind; its argv naming A proves
// nothing about X. Another kind's client holds only what its argv names.
#[test]
fn a_readable_client_of_the_kind_holds_every_conversation() {
    let w = World::human("");
    w.fakes.process(row(
        Some(1),
        Some("/usr/bin/claude"),
        Some(&["claude", "--resume", A]),
        Some("claude"),
    ));
    w.fakes.client(7001, Client::Claude);
    assert_eq!(
        held(&w, A, Client::Claude),
        Some((Rule::Argv, Some(7001))),
        "argv first"
    );
    assert_eq!(
        held(&w, X, Client::Claude),
        Some((Rule::Unmanaged, Some(7001)))
    );
    assert_eq!(held(&w, X, Client::Copilot), None);
    assert_eq!(held(&w, A, Client::Copilot), Some((Rule::Argv, Some(7001))));
}

/// (case, start, path, argv, image, os, kind, held as unbound)
type Unbound<'a> = (
    &'a str,
    Option<i64>,
    Option<&'a str>,
    Option<&'a [&'a str]>,
    Option<&'a str>,
    OsKind,
    Client,
    bool,
);

// H4.5: a row with no start, path or argv holds, naming its pid, unless its image is readable and names no client of
// `kind`. A readable non-client row is not unbound and holds nothing.
#[test]
fn an_unbound_row_holds_unless_its_image_names_no_client() {
    use OsKind::{Unix, Windows};
    let (claude, copilot) = (Client::Claude, Client::Copilot);
    let cases: [Unbound; 16] = [
        (
            "unix claude, path and argv unreadable",
            Some(1),
            None,
            None,
            Some("claude"),
            Unix,
            claude,
            true,
        ),
        (
            "unix image unreadable",
            Some(1),
            None,
            None,
            None,
            Unix,
            claude,
            true,
        ),
        (
            "unix sshd",
            Some(1),
            None,
            None,
            Some("sshd"),
            Unix,
            claude,
            false,
        ),
        (
            "unix start unreadable, kept",
            None,
            None,
            None,
            Some("claude"),
            Unix,
            claude,
            true,
        ),
        (
            "unix start only unreadable",
            None,
            Some("/usr/bin/claude"),
            Some(&["claude"]),
            Some("claude"),
            Unix,
            claude,
            true,
        ),
        (
            "unix path only unreadable",
            Some(1),
            None,
            Some(&["claude"]),
            Some("claude"),
            Unix,
            claude,
            true,
        ),
        (
            "unix argv only unreadable",
            Some(1),
            Some("/usr/bin/node"),
            None,
            Some("node"),
            Unix,
            copilot,
            true,
        ),
        (
            "unix node, claude asked",
            Some(1),
            None,
            None,
            Some("node"),
            Unix,
            claude,
            false,
        ),
        (
            "unix Claude, case differs",
            Some(1),
            None,
            None,
            Some("Claude"),
            Unix,
            claude,
            false,
        ),
        (
            "windows claude.exe",
            None,
            None,
            None,
            Some("claude.exe"),
            Windows,
            claude,
            true,
        ),
        (
            "windows CLAUDE.EXE.old.1.2",
            None,
            None,
            None,
            Some("CLAUDE.EXE.old.1.2"),
            Windows,
            claude,
            true,
        ),
        (
            "windows node.exe, copilot",
            None,
            None,
            None,
            Some("node.exe"),
            Windows,
            copilot,
            true,
        ),
        (
            "windows node.exe, claude",
            None,
            None,
            None,
            Some("node.exe"),
            Windows,
            claude,
            false,
        ),
        (
            "windows svchost.exe",
            None,
            None,
            None,
            Some("svchost.exe"),
            Windows,
            copilot,
            false,
        ),
        (
            "windows image unreadable",
            None,
            None,
            None,
            None,
            Windows,
            claude,
            true,
        ),
        (
            "readable non-client",
            Some(1),
            Some("/usr/bin/sleep"),
            Some(&["sleep", "9"]),
            None,
            Unix,
            claude,
            false,
        ),
    ];
    for (case, start, path, argv, image, os, kind, want) in cases {
        let w = World::human("");
        w.fakes.process(row(start, path, argv, image));
        let want = want.then_some((Rule::Unbound, Some(7001)));
        assert_eq!(who(held_on(&w.ctx, X, kind, os, None)), want, "{case}");
    }
    // An unbound row the OS names a client of `kind` from its path alone holds as H4.4, whatever its image.
    let w = World::human("");
    w.fakes.process(row(
        Some(1),
        Some("/usr/local/bin/claude"),
        None,
        Some("sleep"),
    ));
    w.fakes.client(7001, Client::Claude);
    assert_eq!(
        held(&w, X, Client::Claude),
        Some((Rule::Unmanaged, Some(7001)))
    );
}

/// (case, the `/proc` options in mountinfo, or None for mountinfo that cannot be read; the rule every query gets)
type Mounts<'a> = (&'a str, Option<&'a str>, Option<Rule>);

const ROOT_MOUNT: &str = "25 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw\n";

// Call 10, checklist 3 and 4: a /proc mounted with `hidepid` (other than 0/off) or `subset=pid` hides other users'
// holders, so every query of either kind holds, naming /proc; mountinfo that cannot be read, or that names no /proc
// (`ProcView::Unreadable`), holds as `Table` through `held_on`. Without the seam (not Linux) or with a plain /proc,
// the table rules decide (the fixture rows hold nothing).
#[test]
fn a_proc_that_hides_holders_holds_every_query() {
    let proc_line = "22 28 0:21 / /proc rw,nosuid,relatime shared:13 - proc proc ";
    let cases: [Mounts; 9] = [
        ("hidepid=1", Some("rw,hidepid=1"), Some(Rule::Hidden)),
        ("hidepid=2", Some("rw,hidepid=2"), Some(Rule::Hidden)),
        (
            "hidepid=invisible",
            Some("rw,hidepid=invisible"),
            Some(Rule::Hidden),
        ),
        (
            "hidepid=noaccess",
            Some("rw,hidepid=noaccess"),
            Some(Rule::Hidden),
        ),
        ("subset=pid", Some("rw,subset=pid"), Some(Rule::Hidden)),
        ("hidepid=0", Some("rw,hidepid=0"), None),
        ("hidepid=off", Some("rw,hidepid=off"), None),
        ("plain", Some("rw"), None),
        ("mountinfo unreadable", None, Some(Rule::Table)),
    ];
    for (case, opts, want) in cases {
        for kind in [Client::Claude, Client::Copilot] {
            let w = World::human("");
            let text = opts.map(|o| format!("{ROOT_MOUNT}{proc_line}{o}\n"));
            w.fakes.mountinfo(text.as_deref());
            let got = held_on(&w.ctx, X, kind, OsKind::Unix, None);
            assert_eq!(
                got.as_ref().map(|h| (h.rule, h.pid)),
                want.map(|r| (r, None)),
                "{case} {kind:?}"
            );
            if want == Some(Rule::Hidden) {
                assert!(got.unwrap().message().contains("/proc hides"), "{case}");
            }
        }
    }
    assert_eq!(proc_view(ROOT_MOUNT), ProcView::Unreadable, "no /proc line");
    for kind in [Client::Claude, Client::Copilot] {
        let w = World::human("");
        w.fakes.mountinfo(Some(ROOT_MOUNT));
        let got = held_on(&w.ctx, X, kind, OsKind::Unix, None);
        assert_eq!(
            who(got),
            Some((Rule::Table, None)),
            "no /proc line, {kind:?}"
        );
    }
    assert_eq!(
        held(&World::human(""), X, Client::Claude),
        None,
        "no seam (not Linux): the table decides"
    );
}

// Call 20, checklist 2: any doubt about what /proc shows holds. More than one mount at /proc (in either order, whatever
// each one's options), a non-proc mount at /proc (a tmpfs over it), or `hidepid` in the mount options (field 6) rather
// than the super options: `Hidden` for every query of either kind.
#[test]
fn a_doubtful_proc_mount_holds_every_query() {
    let plain = "22 28 0:21 / /proc rw,nosuid,relatime shared:13 - proc proc rw\n";
    let hiding = "22 28 0:21 / /proc rw,nosuid,relatime shared:13 - proc proc rw,hidepid=2\n";
    let over = "31 22 0:51 / /proc rw,relatime shared:20 - proc proc rw\n";
    let tmpfs = "31 22 0:51 / /proc rw,relatime shared:20 - tmpfs tmpfs rw\n";
    let field6 = "22 28 0:21 / /proc rw,nosuid,relatime,hidepid=2 shared:13 - proc proc rw\n";
    let cases = [
        (
            "stacked: hiding, then plain",
            format!("{ROOT_MOUNT}{hiding}{over}"),
        ),
        (
            "stacked: plain, then hiding",
            format!("{ROOT_MOUNT}{over}{hiding}"),
        ),
        ("stacked: plain twice", format!("{ROOT_MOUNT}{plain}{over}")),
        ("tmpfs over proc", format!("{ROOT_MOUNT}{plain}{tmpfs}")),
        ("tmpfs alone at /proc", format!("{ROOT_MOUNT}{tmpfs}")),
        (
            "hidepid in the mount options",
            format!("{ROOT_MOUNT}{field6}"),
        ),
    ];
    for (case, text) in &cases {
        assert_eq!(proc_view(text), ProcView::Hidden, "{case}");
        for kind in [Client::Claude, Client::Copilot] {
            let w = World::human("");
            w.fakes.mountinfo(Some(text));
            let got = held_on(&w.ctx, X, kind, OsKind::Unix, None);
            assert_eq!(who(got), Some((Rule::Hidden, None)), "{case} {kind:?}");
        }
    }
    assert_eq!(
        proc_view(&format!("{ROOT_MOUNT}{plain}")),
        ProcView::Open,
        "one plain proc at /proc"
    );
}

// PM call 2: the refusal text names the pid, the image (or that it is unreadable) and the rule.
#[test]
fn a_refusal_names_the_pid_the_image_and_the_rule() {
    let w = World::human("");
    w.fakes.process(row(Some(1), None, None, Some("claude")));
    let h = held_on(&w.ctx, X, Client::Claude, OsKind::Unix, None).unwrap();
    assert_eq!(
        h.message(),
        "process 7001 (claude) may hold that conversation: it cannot be read and may be a client; if you just \
         stopped it, retry in a few seconds"
    );
    let w = World::human("");
    w.fakes.process(row(Some(1), None, None, None));
    let h = held_on(&w.ctx, X, Client::Claude, OsKind::Unix, None).unwrap();
    assert!(
        h.message()
            .starts_with("process 7001 (image unreadable) may hold"),
        "{}",
        h.message()
    );
    w.fakes.table_fails();
    let h = held_on(&w.ctx, X, Client::Claude, OsKind::Unix, None).unwrap();
    assert!(
        h.message().starts_with(
            "this machine may hold that conversation: the process table cannot be read"
        )
    );
}

#[test]
fn client_image_by_os() {
    use OsKind::{Unix, Windows};
    let yes = [
        ("claude", Unix, Client::Claude),
        ("copilot", Unix, Client::Copilot),
        ("node", Unix, Client::Copilot),
        ("claude.exe", Windows, Client::Claude),
        ("Copilot.EXE", Windows, Client::Copilot),
        ("copilot.exe.old-1-2", Windows, Client::Copilot),
        ("NODE.EXE", Windows, Client::Copilot),
    ];
    for (image, os, kind) in yes {
        assert!(client_image(image, kind, os), "{image} {os:?} {kind:?}");
    }
    let no = [
        ("claude.exe", Unix, Client::Claude),
        ("claude", Windows, Client::Claude),
        ("node", Unix, Client::Claude),
        ("node.exe", Windows, Client::Claude),
        ("claude-code", Unix, Client::Claude),
        ("copilot", Unix, Client::Claude),
    ];
    for (image, os, kind) in no {
        assert!(!client_image(image, kind, os), "{image} {os:?} {kind:?}");
    }
}

use crate::agents::gens;
use crate::proc::Boot;
use crate::state::{evidence::publish, live};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const Y: &str = "bbbbbbbbbbbb";
const B: &str = "33333333-3333-4333-8333-333333333333";
/// Mixed-case ids (hex letters), so lowercasing is observable (finding 13).
const M: &str = "ABCDEF01-2222-4333-8444-555555555555";
const N: &str = "AbCdEf02-2222-4333-8444-555555555555";

/// Lifecycle member Y of `client` at generation 1 (record `sessionId` S), its generation committed, its spec naming
/// `spec`. Nothing proves its client yet.
fn life(w: &World, client: &str, spec: &str) -> PathBuf {
    let f = w.uncommitted(
        Y,
        json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd", "client": client, "sessionId": S}),
    );
    publish(&gens::path(&f, 1, "commit"), &json!({"committed": true})).unwrap();
    publish(&gens::path(&f, 1, "spec"), &json!({"sessionId": spec})).unwrap();
    f
}

/// Generation `n` recorded launcher 31337 (start 1000, `startedAt` the fake clock's now, as the launcher writes it)
/// and client 7100 (start 1500), both listed, the client a Claude whose argv names S.
fn launched(w: &World, f: &Path, n: u64) {
    let at = crate::util::utc_o(w.ctx.clock.now());
    publish(
        &gens::path(f, n, "launcher"),
        &json!({"pid": 31337, "start": 1000, "startedAt": at}),
    )
    .unwrap();
    publish(
        &gens::path(f, n, "client"),
        &json!({"pid": 7100, "start": 1500}),
    )
    .unwrap();
    w.fakes.process(crate::testing::row(31337, 1, 1000));
    w.fakes.process(Row {
        pid: 7100,
        ppid: 31337,
        start: Some(1500),
        path: Some("/usr/bin/claude".into()),
        argv: Some(vec!["claude".into(), "--session-id".into(), S.into()]),
        image: Some("claude".into()),
    });
    w.fakes.client(7100, Client::Claude);
}

fn torn(p: &Path) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, b"{\"se").unwrap();
}

fn plant(p: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

/// (rule, pid, image, member) of a hold.
type Named = (Rule, Option<u32>, Option<String>, Option<String>);

fn named(h: Option<Hold>) -> Option<Named> {
    h.map(|h| (h.rule, h.pid, h.image, h.member))
}

/// The hold every query gets from managed client 7100 of member Y whose report cannot be used (finding 8).
fn client_evidence() -> Option<Named> {
    Some((
        Rule::Evidence,
        Some(7100),
        Some("claude".into()),
        Some(Y.into()),
    ))
}

// H2, H4.2: managed Y (its client.json matches 7100) holds only its effective session: the spec's S, then the X its
// newest hook report names. Its argv naming S adds nothing; its record's S never blocks another subject on S.
#[test]
fn a_managed_client_holds_only_its_effective_session() {
    let w = World::human("");
    let f = life(&w, "claude", S);
    launched(&w, &f, 1);
    assert_eq!(
        held(&w, S, Client::Claude),
        Some((Rule::Managed, Some(7100))),
        "no report yet: the generation's session"
    );
    let h = held_on(&w.ctx, S, Client::Claude, OsKind::Unix, None).unwrap();
    assert_eq!(
        (h.member.as_deref(), h.image.as_deref()),
        (Some(Y), Some("claude")),
        "PM call 2: member and image"
    );
    assert_eq!(
        held(&w, B, Client::Claude),
        None,
        "managed: B is free to restore"
    );
    assert_eq!(
        live::report(&w.ctx, Client::Claude, 7100, 1500, X, "resume").map_err(|r| r.check),
        Ok(1)
    );
    assert_eq!(
        held(&w, X, Client::Claude),
        Some((Rule::Managed, Some(7100))),
        "the newest report"
    );
    assert_eq!(
        held(&w, S, Client::Claude),
        None,
        "argv and record S prove nothing once the client moved"
    );
    assert_eq!(held(&w, B, Client::Claude), None);
}

// H4.4: an unmanaged live Claude on A holds X too; its registry entry naming A proves nothing. (Passes on arrival
// from Task 3; it pins that `claims` never reads an unmanaged client's entry.)
#[test]
fn an_unmanaged_client_holds_every_conversation_and_its_entry_proves_nothing() {
    let w = World::human("");
    w.fakes.process(Row {
        pid: 7200,
        ppid: 1,
        start: Some(1200),
        path: Some("/usr/bin/claude".into()),
        argv: Some(vec!["claude".into(), "--resume".into(), A.into()]),
        image: Some("claude".into()),
    });
    w.fakes.client(7200, Client::Claude);
    w.state();
    assert_eq!(
        live::report(&w.ctx, Client::Claude, 7200, 1200, A, "startup").map_err(|r| r.check),
        Ok(1)
    );
    assert_eq!(
        held(&w, X, Client::Claude),
        Some((Rule::Unmanaged, Some(7200)))
    );
    assert_eq!(held(&w, A, Client::Claude), Some((Rule::Argv, Some(7200))));
    assert_eq!(held(&w, X, Client::Copilot), None);
}

// H3, call 13: no client.json yet (pending, or the launcher paused after launcher.json): the generation reserves its
// own session, the spec's X, for every kind; the record's S is not its session and stays free, as does B.
#[test]
fn a_generation_without_its_client_reserves_its_own_session() {
    for paused in [false, true] {
        let w = World::human("");
        let f = life(&w, "claude", X);
        if paused {
            publish(
                &gens::path(&f, 1, "launcher"),
                &json!({"pid": 31337, "start": 1000}),
            )
            .unwrap();
            w.fakes.process(crate::testing::row(31337, 1, 1000));
        }
        for kind in [Client::Claude, Client::Copilot] {
            assert_eq!(
                held(&w, X, kind),
                Some((Rule::Reserved, None)),
                "paused {paused} {kind:?}: a reservation holds every kind"
            );
        }
        assert_eq!(
            held(&w, S, Client::Claude),
            None,
            "paused {paused}: the record's S is not this generation's"
        );
        assert_eq!(held(&w, B, Client::Claude), None, "paused {paused}");
    }
}

// H2 via K1a: a pid matching client.json with another start, a foreign (client-conflict) or torn client.json, or a
// done generation's client: unmanaged, so it holds every Claude conversation (H4.4). Passes on arrival from Task 3;
// it pins `bound`.
#[test]
fn an_unproven_or_done_client_is_unmanaged_and_holds() {
    for case in ["other start", "foreign", "torn client", "done"] {
        let w = World::human("");
        let f = life(&w, "claude", S);
        // `publish` is create-once: a file written first stands and `launched`'s publish answers Exists.
        match case {
            "other start" => publish(
                &gens::path(&f, 1, "client"),
                &json!({"pid": 7100, "start": 1499}),
            )
            .map(drop)
            .unwrap(),
            "foreign" => publish(
                &gens::path(&f, 1, "exit"),
                &json!({"reason": "client-conflict"}),
            )
            .map(drop)
            .unwrap(),
            "torn client" => torn(&gens::path(&f, 1, "client")),
            _ => publish(&gens::path(&f, 1, "ended"), &json!({"state": "finished"}))
                .map(drop)
                .unwrap(),
        }
        launched(&w, &f, 1);
        assert_eq!(
            held(&w, B, Client::Claude),
            Some((Rule::Unmanaged, Some(7100))),
            "{case}"
        );
    }
}

// H3, fresh pending crash: active generation 1 (open, no spec) runs S, the record's; pending generation 2 committed
// for X, its launcher killed before client.json. Each reserves its own session (call 13): S and X held; B free.
#[test]
fn a_fresh_pending_crash_reserves_the_record_and_the_generation_session() {
    let w = World::human("");
    let f = w.uncommitted(
        Y,
        json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd", "client": "claude", "sessionId": S,
            "pending": {"gen": 2, "hcName": "m4567cdef", "sessionId": X}}),
    );
    publish(&gens::path(&f, 1, "commit"), &json!({"committed": true})).unwrap();
    publish(&gens::path(&f, 2, "commit"), &json!({"committed": true})).unwrap();
    publish(&gens::path(&f, 2, "spec"), &json!({"sessionId": X})).unwrap();
    publish(
        &gens::path(&f, 2, "launcher"),
        &json!({"pid": 31337, "start": 1000}),
    )
    .unwrap();
    assert_eq!(
        held(&w, S, Client::Claude),
        Some((Rule::Reserved, None)),
        "the record's S"
    );
    assert_eq!(
        held(&w, X, Client::Claude),
        Some((Rule::Reserved, None)),
        "the generation's X"
    );
    let h = held_on(&w.ctx, X, Client::Claude, OsKind::Unix, None).unwrap();
    assert_eq!(
        h.member.as_deref(),
        Some(Y),
        "a reservation names its member (PM call 2)"
    );
    assert_eq!(held(&w, B, Client::Claude), None);
}

// R2: unreadable evidence of a member holds every conversation, for every kind: a torn record, a torn spec, a torn
// ended marker, or a pending generation with no session anywhere. (A managed client's unusable entry names that
// client instead: `an_unusable_entry_names_its_known_holder`.)
#[test]
fn unreadable_member_evidence_holds_every_conversation() {
    for case in [
        "torn record",
        "torn spec",
        "torn ended",
        "pending without a session",
    ] {
        let w = World::human("");
        let f = match case {
            "pending without a session" => w.uncommitted(
                Y,
                json!({"lifecycle": 1, "client": "claude", "pending": {"gen": 1, "hcName": "m0123abcd"}}),
            ),
            "torn spec" => {
                let f = w.uncommitted(Y, json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd", "client": "claude"}));
                torn(&gens::path(&f, 1, "spec"));
                f
            }
            _ => life(&w, "claude", S),
        };
        match case {
            "torn record" => torn(&f.join("record.json")),
            "torn ended" => torn(&gens::path(&f, 1, "ended")),
            _ => {}
        }
        for kind in [Client::Claude, Client::Copilot] {
            let h = held_on(&w.ctx, B, kind, OsKind::Unix, None).unwrap();
            assert_eq!((h.rule, h.pid), (Rule::Evidence, None), "{case} {kind:?}");
            assert_eq!(
                h.member.as_deref(),
                Some(Y),
                "{case}: the refusal names the member (PM call 2)"
            );
        }
    }
}

// Ambiguity 6: an entry of an earlier process at pid 7100 (start 999) is never read for the client started at 1500.
#[test]
fn a_reused_pid_entry_proves_nothing() {
    let w = World::human("");
    let f = life(&w, "claude", S);
    w.fakes.process(crate::testing::row(7100, 1, 999));
    assert_eq!(
        live::report(&w.ctx, Client::Claude, 7100, 999, X, "resume").map_err(|r| r.check),
        Ok(1)
    );
    w.fakes.forget(7100);
    launched(&w, &f, 1);
    assert_eq!(held(&w, X, Client::Claude), None, "the old process's entry");
    assert_eq!(
        held(&w, S, Client::Claude),
        Some((Rule::Managed, Some(7100))),
        "the generation's own session"
    );
}

// H2, loader pair (proof-0b K1a): Copilot member Y's client.json names the npm loader 7100 (`node`); its native
// child 7101 (`copilot`) is the same managed client on Unix and holds only S. On Windows there is no pair: 7101 is
// an unmanaged Copilot and holds every Copilot conversation.
#[test]
fn a_copilot_loader_and_its_native_child_hold_one_session() {
    for os in [OsKind::Unix, OsKind::Windows] {
        let w = World::human("");
        let f = life(&w, "copilot", S);
        publish(
            &gens::path(&f, 1, "launcher"),
            &json!({"pid": 31337, "start": 1000}),
        )
        .unwrap();
        publish(
            &gens::path(&f, 1, "client"),
            &json!({"pid": 7100, "start": 1500}),
        )
        .unwrap();
        w.fakes.process(crate::testing::row(31337, 1, 1000));
        let proc_row = |pid: u32, ppid: u32, file: &str| Row {
            pid,
            ppid,
            start: Some(1500 + i64::from(pid - 7100)),
            path: Some(format!("/usr/lib/{file}")),
            argv: Some(vec![file.into()]),
            image: Some(file.into()),
        };
        w.fakes.process(proc_row(7100, 31337, "node"));
        w.fakes.process(proc_row(7101, 7100, "copilot"));
        w.fakes.client(7100, Client::Copilot);
        w.fakes.client(7101, Client::Copilot);
        assert_eq!(
            who(held_on(&w.ctx, S, Client::Copilot, os, None)),
            Some((Rule::Managed, Some(7100))),
            "{os:?}"
        );
        let want = (os == OsKind::Windows).then_some((Rule::Unmanaged, Some(7101)));
        assert_eq!(
            who(held_on(&w.ctx, B, Client::Copilot, os, None)),
            want,
            "{os:?}"
        );
    }
}

// S6: a done generation (ended, released) reserves nothing; one whose `completing` names it is not done yet and
// reserves its own session X only (call 13), never the record's S.
#[test]
fn a_done_or_released_generation_reserves_nothing() {
    for (marker, completing) in [("ended", false), ("released", false), ("ended", true)] {
        let w = World::human("");
        let f = life(&w, "claude", X);
        publish(&gens::path(&f, 1, marker), &json!({})).unwrap();
        if completing {
            let mut r: Value = crate::state::fs::read_json(&f.join("record.json"))
                .unwrap()
                .unwrap();
            r["completing"] = json!({"gen": 1, "intent": "ended"});
            crate::state::fs::write_json(&f.join("record.json"), &r).unwrap();
        }
        for (s, own) in [(S, false), (X, true)] {
            let want = (completing && own).then_some((Rule::Reserved, None));
            assert_eq!(
                held(&w, s, Client::Claude),
                want,
                "{marker} completing {completing}: {s}"
            );
        }
    }
}

// Call 9 (boot reuse): an entry whose `at` is earlier than the generation's launch evidence time (`launcher.json`
// `startedAt`) comes from an earlier boot (Linux start ticks repeat): it is ignored and the client holds its
// generation's S. One at or after the launch counts, and its id is compared lowercased (finding 13).
#[test]
fn a_report_from_before_the_launch_proves_nothing() {
    let w = World::human("");
    let f = life(&w, "claude", S);
    w.state();
    live::report(&w.ctx, Client::Claude, 7100, 1500, X, "resume").unwrap(); // `at` = now
    w.fakes.advance(std::time::Duration::from_secs(60));
    launched(&w, &f, 1); // `startedAt` = now + 60 s; same pid and start
    assert_eq!(
        held(&w, S, Client::Claude),
        Some((Rule::Managed, Some(7100))),
        "earlier boot: the generation's S"
    );
    assert_eq!(held(&w, X, Client::Claude), None, "the earlier boot's X");
    live::report(&w.ctx, Client::Claude, 7100, 1500, M, "resume").unwrap(); // at the launch instant
    let m = M.to_ascii_lowercase();
    assert_eq!(
        held(&w, &m, Client::Claude),
        Some((Rule::Managed, Some(7100))),
        "not earlier: it counts"
    );
    assert_eq!(held(&w, S, Client::Claude), None);
}

// Finding 8, call 2, checklist 10: a managed client whose entry cannot be used holds every conversation as `Evidence`
// and still names its known holder (pid 7100, image claude, member Y): a torn entry, `{}`, an invalid id (finding 12),
// a valid `at` with no `sessionId`, no `at`, and a valid entry with no launch time to compare (`launcher.json` without
// `startedAt`).
#[test]
fn an_unusable_entry_names_its_known_holder() {
    let no_at = format!("{{\"sessionId\":\"{X}\",\"seq\":1}}");
    let cases: [(&str, &[u8]); 6] = [
        ("torn", b"{\"se"),
        ("empty", b"{}"),
        (
            "invalid id",
            b"{\"sessionId\":\"invalid\",\"at\":\"2099-01-01T00:00:00.0000000Z\",\"seq\":1}",
        ),
        (
            "no session id",
            b"{\"at\":\"2099-01-01T00:00:00.0000000Z\",\"seq\":1}",
        ),
        ("no at", no_at.as_bytes()),
        ("no launch time", b""),
    ];
    for (case, bytes) in cases {
        let w = World::human("");
        let f = life(&w, "claude", S);
        if case == "no launch time" {
            publish(
                &gens::path(&f, 1, "launcher"),
                &json!({"pid": 31337, "start": 1000}),
            )
            .unwrap();
        }
        launched(&w, &f, 1);
        w.state();
        let entry = live::path(&w.ctx, Client::Claude, 7100, 1500);
        if case == "no launch time" {
            live::report(&w.ctx, Client::Claude, 7100, 1500, X, "resume").unwrap();
        } else {
            plant(&entry, bytes);
        }
        for s in [S, X, B] {
            let got = named(held_on(&w.ctx, s, Client::Claude, OsKind::Unix, None));
            assert_eq!(got, client_evidence(), "{case}: {s}");
        }
    }
}

// Call 11: an unresolved orphan (a generation the record no longer names, not done; group B's `Gen::orphans()`)
// reserves its own session until it is done.
#[test]
fn an_unresolved_orphan_reserves_its_own_session() {
    let w = World::human("");
    let f = life(&w, "claude", S); // generation 1 active
    publish(&gens::path(&f, 0, "commit"), &json!({"committed": true})).unwrap();
    publish(&gens::path(&f, 0, "spec"), &json!({"sessionId": X})).unwrap();
    assert_eq!(
        held(&w, X, Client::Claude),
        Some((Rule::Reserved, None)),
        "orphan 0 reserves its X"
    );
    publish(&gens::path(&f, 0, "ended"), &json!({"state": "finished"})).unwrap();
    assert_eq!(held(&w, X, Client::Claude), None, "done: free");
}

// Checklist 5: an orphan list that cannot be read (generation 0's completion evidence torn) holds every conversation
// as evidence naming the member; it never reads as "no orphans".
#[test]
fn an_unreadable_orphan_list_holds_as_evidence() {
    let w = World::human("");
    let f = life(&w, "claude", S); // generation 1 active
    torn(&gens::path(&f, 0, "ended"));
    for kind in [Client::Claude, Client::Copilot] {
        let got = held_on(&w.ctx, X, kind, OsKind::Unix, None).map(|h| (h.rule, h.pid, h.member));
        assert_eq!(
            got,
            Some((Rule::Evidence, None, Some(Y.to_string()))),
            "{kind:?}"
        );
    }
}

// Call 14, checklist 6: a spec-less orphan never had a client (`forward` publishes `spec.json` before `hc run`), so an
// empty `gens/1` beside active generation 2 (spec X; record S) reserves nothing: S, which only a fallback to the
// record could reserve, stays free. A torn `gens/1/spec.json` still refuses `evidence`, naming Y.
#[test]
fn a_spec_less_orphan_reserves_nothing_and_a_torn_spec_refuses() {
    for case in ["empty", "torn spec"] {
        let w = World::human("");
        let f = w.uncommitted(
            Y,
            json!({"lifecycle": 1, "generation": "2", "hcName": "m0123abcd", "client": "claude", "sessionId": S}),
        );
        publish(&gens::path(&f, 2, "commit"), &json!({"committed": true})).unwrap();
        publish(&gens::path(&f, 2, "spec"), &json!({"sessionId": X})).unwrap();
        std::fs::create_dir_all(gens::dir(&f, 1)).unwrap();
        if case == "torn spec" {
            torn(&gens::path(&f, 1, "spec"));
        }
        let got = held_on(&w.ctx, S, Client::Claude, OsKind::Unix, None)
            .map(|h| (h.rule, h.pid, h.member));
        let want = (case == "torn spec").then(|| (Rule::Evidence, None, Some(Y.to_string())));
        assert_eq!(got, want, "{case}: S");
        // Generation 2's own X: reserved; with a torn orphan spec the whole member refuses.
        let rule = if case == "empty" {
            Rule::Reserved
        } else {
            Rule::Evidence
        };
        assert_eq!(held(&w, X, Client::Claude), Some((rule, None)), "{case}: X");
    }
}

// Call 13, the audit's two sibling repros: an open active generation (spec S) beside a done pending one (spec X)
// leaves X free; an open pending generation (spec X) beside a done active one (record and spec S) leaves S free.
#[test]
fn a_generation_never_reserves_a_siblings_session() {
    for (case, done, own, sibling) in [("active open", 2u64, S, X), ("pending open", 1, X, S)] {
        let w = World::human("");
        let f = w.uncommitted(
            Y,
            json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd", "client": "claude", "sessionId": S,
                "pending": {"gen": 2, "hcName": "m4567cdef", "sessionId": X}}),
        );
        for (n, s) in [(1, S), (2, X)] {
            publish(&gens::path(&f, n, "commit"), &json!({"committed": true})).unwrap();
            publish(&gens::path(&f, n, "spec"), &json!({"sessionId": s})).unwrap();
        }
        publish(
            &gens::path(&f, done, "ended"),
            &json!({"state": "finished"}),
        )
        .unwrap();
        assert_eq!(
            held(&w, own, Client::Claude),
            Some((Rule::Reserved, None)),
            "{case}: its own"
        );
        assert_eq!(
            held(&w, sibling, Client::Claude),
            None,
            "{case}: the sibling's"
        );
    }
}

// Call 13, checklist 8, the bound-pending sibling repro: active generation 1 (no client) reserves S; pending generation
// 2's spec names X and its proven client 7100 reports B. X is free (the client moved; a bound generation reserves
// nothing beside its client), B is held by the managed client, S by the reservation.
#[test]
fn a_bound_pending_generation_reserves_nothing_beside_its_client() {
    let w = World::human("");
    let f = w.uncommitted(
        Y,
        json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd", "client": "claude", "sessionId": S,
            "pending": {"gen": 2, "hcName": "m4567cdef", "sessionId": X}}),
    );
    for (n, s) in [(1, S), (2, X)] {
        publish(&gens::path(&f, n, "commit"), &json!({"committed": true})).unwrap();
        publish(&gens::path(&f, n, "spec"), &json!({"sessionId": s})).unwrap();
    }
    launched(&w, &f, 2);
    live::report(&w.ctx, Client::Claude, 7100, 1500, B, "resume").unwrap();
    assert_eq!(
        held(&w, B, Client::Claude),
        Some((Rule::Managed, Some(7100))),
        "the client's report"
    );
    assert_eq!(
        held(&w, X, Client::Claude),
        None,
        "pending's own X: its client moved"
    );
    assert_eq!(
        held(&w, S, Client::Claude),
        Some((Rule::Reserved, None)),
        "active's S"
    );
}

// Findings 11, 13: with no spec, the active generation reserves the record's `sessionId` and the pending one its own
// `pending.sessionId`; both are mixed case in the record and held lowercased.
#[test]
fn absent_specs_fall_back_to_each_generations_own_record_session() {
    let w = World::human("");
    let f = w.uncommitted(
        Y,
        json!({"lifecycle": 1, "generation": "1", "hcName": "m0123abcd", "client": "claude", "sessionId": M,
            "pending": {"gen": 2, "hcName": "m4567cdef", "sessionId": N}}),
    );
    for n in [1, 2] {
        publish(&gens::path(&f, n, "commit"), &json!({"committed": true})).unwrap();
    }
    for s in [M, N] {
        assert_eq!(
            held(&w, &s.to_ascii_lowercase(), Client::Claude),
            Some((Rule::Reserved, None)),
            "{s}"
        );
    }
    assert_eq!(held(&w, B, Client::Claude), None);
}

// Checklist 9: ids compare without case, each side tested on its own: a mixed-case spec id against a lowercase query,
// a lowercase spec id against an uppercase query, and a managed client's session against an uppercase query.
#[test]
fn mixed_case_ids_compare_without_case() {
    let lower = "abcdef03-2222-4333-8444-555555555555";
    let cases = [
        ("mixed spec, lowercase query", M, M.to_ascii_lowercase()),
        (
            "lowercase spec, uppercase query",
            lower,
            lower.to_ascii_uppercase(),
        ),
    ];
    for (case, spec, query) in cases {
        let w = World::human("");
        life(&w, "claude", spec);
        assert_eq!(
            held(&w, &query, Client::Claude),
            Some((Rule::Reserved, None)),
            "{case}"
        );
    }
    let w = World::human("");
    let f = life(&w, "claude", S);
    launched(&w, &f, 1);
    let upper = S.to_ascii_uppercase();
    assert_eq!(
        held(&w, &upper, Client::Claude),
        Some((Rule::Managed, Some(7100))),
        "managed, uppercase query"
    );
}

// Call 12: members are enumerated through the walk-checked enumeration. A file or a link (on Windows `link_dir`
// makes a symlink or a junction) at `<key>/agents` refuses as `Evidence` naming no member; no `agents`: no members.
#[test]
fn a_wrong_type_agents_folder_refuses() {
    for case in ["file", "link", "absent"] {
        let w = World::human("");
        let key = w.ctx.state_root.join("0123456789abcdef");
        std::fs::create_dir_all(&key).unwrap();
        match case {
            "file" => std::fs::write(key.join("agents"), b"x").unwrap(),
            "link" => {
                let elsewhere = w.ctx.state_root.join("elsewhere");
                std::fs::create_dir_all(elsewhere.join("agents")).unwrap();
                crate::testing::link_dir(&elsewhere.join("agents"), &key.join("agents"));
            }
            _ => {}
        }
        let got = held_on(&w.ctx, X, Client::Claude, OsKind::Unix, None)
            .map(|h| (h.rule, h.pid, h.member));
        let want = (case != "absent").then_some((Rule::Evidence, None, None));
        assert_eq!(got, want, "{case}");
    }
}

// Checklist 7 (call 16): `effective` reads the create-once `launcher.json` a second time for `startedAt`. If that read
// finds it gone or torn after `gens::client` proved the client, the client's session is unknown: every query holds as
// `Evidence`, still naming the client. (Seam: `hooks::before_launch_reread`.)
#[test]
fn a_launch_record_lost_after_the_proof_holds_as_evidence() {
    for case in ["gone", "torn"] {
        let w = World::human("");
        let f = life(&w, "claude", S);
        launched(&w, &f, 1);
        live::report(&w.ctx, Client::Claude, 7100, 1500, X, "resume").unwrap();
        assert_eq!(
            held(&w, X, Client::Claude),
            Some((Rule::Managed, Some(7100))),
            "{case}: before"
        );
        let launcher = gens::path(&f, 1, "launcher");
        let gone = case == "gone";
        hooks::before_launch_reread(Some(Box::new(move || {
            if gone {
                let _ = std::fs::remove_file(&launcher);
            } else {
                std::fs::write(&launcher, b"{\"pi").unwrap();
            }
        })));
        let got = named(held_on(&w.ctx, X, Client::Claude, OsKind::Unix, None));
        hooks::before_launch_reread(None);
        assert_eq!(got, client_evidence(), "{case}");
    }
}

// Checklist 15 (r2b calls 24, 27): an entry recorded in another boot is dead (`live::entry` answers `Ok(None)`): the
// managed client holds its generation's session, as with no entry (call 9).
#[test]
fn an_entry_from_another_boot_proves_nothing() {
    let w = World::human("");
    w.fakes.set_boot(Boot::Id("boot-a".into()));
    let f = life(&w, "claude", S);
    launched(&w, &f, 1);
    live::report(&w.ctx, Client::Claude, 7100, 1500, X, "resume").unwrap();
    assert_eq!(
        held(&w, X, Client::Claude),
        Some((Rule::Managed, Some(7100))),
        "this boot: it counts"
    );
    w.fakes.set_boot(Boot::Id("boot-b".into()));
    assert_eq!(held(&w, X, Client::Claude), None, "another boot's X");
    assert_eq!(
        held(&w, S, Client::Claude),
        Some((Rule::Managed, Some(7100))),
        "the generation's S"
    );
}

// Checklist 16 (r2b call 29): where starts are boot-relative, an entry whose boot cannot be verified (the current boot
// unreadable, or an entry recorded with no boot) is never used: every query holds as `Evidence` naming the client.
#[test]
fn an_unverifiable_boot_holds_as_evidence_naming_the_client() {
    for case in ["boot unreadable", "entry without a boot"] {
        let w = World::human("");
        if case == "boot unreadable" {
            w.fakes.set_boot(Boot::Id("boot-a".into()));
        }
        let f = life(&w, "claude", S);
        launched(&w, &f, 1);
        live::report(&w.ctx, Client::Claude, 7100, 1500, X, "resume").unwrap();
        w.fakes.set_boot(match case {
            "boot unreadable" => Boot::Unreadable,
            _ => Boot::Id("boot-a".into()),
        });
        for s in [S, X, B] {
            let got = named(held_on(&w.ctx, s, Client::Claude, OsKind::Unix, None));
            assert_eq!(got, client_evidence(), "{case}: {s}");
        }
    }
}

use crate::agents::fixture::{CLAUDE, ID, PANE};

/// `held` with an exempt client.
fn held_as(w: &World, s: &str, kind: Client, os: OsKind, own: &Own) -> Option<Who> {
    who(held_on(&w.ctx, s, kind, os, Some(own)))
}

/// `World::agent`'s pane caller, proven agent 150 (start 1500), no standing.
fn pane_own(w: &World) -> Own {
    own_of(&w.caller(), None).unwrap()
}

// Call 1, test 1: a machine whose only client is the caller's own resumes B. Without the exemption the same row holds.
#[test]
fn a_caller_alone_frees_every_other_conversation() {
    let w = World::agent("");
    let own = pane_own(&w);
    assert_eq!(
        held(&w, B, Client::Claude),
        Some((Rule::Unmanaged, Some(CLAUDE))),
        "no exemption: H4.4"
    );
    assert_eq!(held_as(&w, B, Client::Claude, OsKind::Unix, &own), None);
    assert_eq!(held_as(&w, B, Client::Copilot, OsKind::Unix, &own), None);
}

// Call 1, test 2: the caller whose argv names B blocks B (H4.3 still applies to it), and only B.
#[test]
fn the_callers_argv_naming_the_session_holds_it() {
    let w = World::agent("");
    let own = pane_own(&w);
    w.fakes.forget(CLAUDE);
    w.fakes.process(Row {
        argv: Some(vec!["claude".into(), "--resume".into(), B.into()]),
        ..crate::testing::row(CLAUDE, PANE, 1500)
    });
    assert_eq!(
        held_as(&w, B, Client::Claude, OsKind::Unix, &own),
        Some((Rule::Argv, Some(CLAUDE)))
    );
    assert_eq!(held_as(&w, X, Client::Claude, OsKind::Unix, &own), None);
}

// Call 1, test 3: a second unmanaged Claude still blocks B, beside the exempt caller.
#[test]
fn a_second_unmanaged_client_still_holds_beside_the_caller() {
    let w = World::agent("");
    let own = pane_own(&w);
    w.fakes.process(row(
        Some(1),
        Some("/usr/bin/claude"),
        Some(&["claude"]),
        Some("claude"),
    ));
    w.fakes.client(7001, Client::Claude);
    assert_eq!(
        held_as(&w, B, Client::Claude, OsKind::Unix, &own),
        Some((Rule::Unmanaged, Some(7001)))
    );
}

// Call 1: the caller's own newest entry naming B holds B (`Rule::Caller`); one naming X frees B; a torn entry holds as
// evidence (R2). Each refusal names the caller's pid (call 2).
#[test]
fn the_callers_own_newest_report_holds_its_session() {
    let cases = [
        ("names B", Some((Rule::Caller, Some(CLAUDE)))),
        ("names X", None),
        ("torn", Some((Rule::Evidence, Some(CLAUDE)))),
    ];
    for (case, want) in cases {
        let w = World::agent("");
        let own = pane_own(&w);
        w.state();
        match case {
            "torn" => torn(&live::path(&w.ctx, Client::Claude, CLAUDE, 1500)),
            _ => {
                let s = if case == "names B" { B } else { X };
                live::report(&w.ctx, Client::Claude, CLAUDE, 1500, s, "resume").unwrap();
            }
        }
        assert_eq!(
            held_as(&w, B, Client::Claude, OsKind::Unix, &own),
            want,
            "{case}"
        );
    }
}

// Call 7: `Own` is the proof's (pid, start), never the chain's. The chain names A (150); the member proof names B
// (160): only B is exempt and A still holds. A candidate or member without proof, and a person, exempt nothing. A
// listed start other than the proof's (a reused pid) is not exempt.
#[test]
fn own_is_the_proof_never_the_chain() {
    let w = World::agent("");
    let mut hc = w.caller();
    (hc.agent, hc.member) = (None, Some(ID.to_string()));
    let proof = crate::agents::tree::Member {
        id: ID.into(),
        record: json!({}),
        client: Client::Claude,
        pid: 160,
        start: 1600,
        flags: Vec::new(),
    };
    assert_eq!(
        own_of(&hc, None),
        None,
        "no proof, no exemption; the chain is not read"
    );
    assert_eq!(
        own_of(&hc, Some(&crate::agents::tree::Standing::Person)),
        None
    );
    let own = own_of(&hc, Some(&crate::agents::tree::Standing::Member(proof))).unwrap();
    assert_eq!((own.kind, own.pid, own.start), (Client::Claude, 160, 1600));
    w.fakes.process(Row {
        argv: Some(vec!["claude".into()]),
        ..crate::testing::row(160, PANE, 1600)
    });
    w.fakes.client(160, Client::Claude);
    assert_eq!(
        held_as(&w, B, Client::Claude, OsKind::Unix, &own),
        Some((Rule::Unmanaged, Some(CLAUDE))),
        "A holds"
    );
    w.fakes.forget(CLAUDE);
    assert_eq!(
        held_as(&w, B, Client::Claude, OsKind::Unix, &own),
        None,
        "B alone: exempt"
    );
    let reused = Own { start: 1601, ..own };
    assert_eq!(
        held_as(&w, B, Client::Claude, OsKind::Unix, &reused),
        Some((Rule::Unmanaged, Some(160))),
        "reused pid"
    );
}

/// Rows `node` 7100 (`loader_start`) and its native child `copilot` 7101 (start 2101), both Copilot clients.
fn copilot_pair_rows(w: &World, loader_start: Option<i64>) {
    let proc_row = |pid: u32, ppid: u32, start: Option<i64>, file: &str| Row {
        pid,
        ppid,
        start,
        path: Some(format!("/usr/lib/{file}")),
        argv: Some(vec![file.into()]),
        image: Some(file.into()),
    };
    w.fakes.process(proc_row(7100, 1, loader_start, "node"));
    w.fakes.process(proc_row(7101, 7100, Some(2101), "copilot"));
    w.fakes.client(7100, Client::Copilot);
    w.fakes.client(7101, Client::Copilot);
}

// Call 8: a Copilot caller proven as the native copilot 7101 under its npm loader 7100. On Unix `pair_parent` pairs
// them: both are exempt and the loader keys the entry; a hold names the proven 7101 (call 2). A newer process at the
// loader pid (start 9999 > 2101) is not the loader: the caller's report cannot be read, so `Evidence` on 7101. On
// Windows there is no pair: the node row is an unmanaged Copilot and holds.
#[test]
fn a_copilot_callers_loader_joins_only_by_report_2bs_rule() {
    for (os, loader_start) in [
        (OsKind::Unix, 2100),
        (OsKind::Unix, 9999),
        (OsKind::Windows, 2100),
    ] {
        let w = World::agent("");
        let own = Own {
            kind: Client::Copilot,
            pid: 7101,
            start: 2101,
        };
        copilot_pair_rows(&w, Some(loader_start));
        let case = format!("{os:?} loader start {loader_start}");
        let want = match (os, loader_start) {
            (OsKind::Windows, _) => Some((Rule::Unmanaged, Some(7100))),
            (_, 9999) => Some((Rule::Evidence, Some(7101))),
            _ => None,
        };
        assert_eq!(held_as(&w, B, Client::Copilot, os, &own), want, "{case}");
        if want.is_some() {
            continue;
        }
        w.state();
        live::report(&w.ctx, Client::Copilot, 7100, 2100, B, "resume").unwrap();
        assert_eq!(
            held_as(&w, B, Client::Copilot, os, &own),
            Some((Rule::Caller, Some(7101))),
            "{case}: keyed by the loader"
        );
    }
}

// Call 17, the auditor's mixed-read repro: the saved table lists the loader 7100 without a start (a later read could
// answer (7100, 2100)); the caller's entry under (7100, 2100) names S. The loader is taken from the saved table only:
// it joins nothing, the caller's report cannot be read, and every query holds (`Evidence`, naming 7101); never free.
// No `Os::row` read is made for 7100.
#[test]
fn a_startless_saved_loader_holds_and_is_never_read_again() {
    let w = World::agent("");
    let own = Own {
        kind: Client::Copilot,
        pid: 7101,
        start: 2101,
    };
    copilot_pair_rows(&w, None);
    w.state();
    live::report(&w.ctx, Client::Copilot, 7100, 2100, S, "startup").unwrap();
    for kind in [Client::Claude, Client::Copilot] {
        assert_eq!(
            held_as(&w, S, kind, OsKind::Unix, &own),
            Some((Rule::Evidence, Some(7101))),
            "{kind:?}"
        );
    }
    assert!(
        !w.fakes.os_calls().iter().any(|c| c == "row 7100"),
        "{:?}",
        w.fakes.os_calls()
    );
}

// Call 19, reverse pairing: the proven client is the loader 7100 (2100); on Unix its native child 7101, which
// `pair_parent` pairs back to it, is exempt too, and the loader keys the entry. On Windows nothing pairs: the native
// child is an unmanaged Copilot and holds.
#[test]
fn a_loader_callers_native_child_is_exempt() {
    for os in [OsKind::Unix, OsKind::Windows] {
        let w = World::agent("");
        let own = Own {
            kind: Client::Copilot,
            pid: 7100,
            start: 2100,
        };
        copilot_pair_rows(&w, Some(2100));
        if os == OsKind::Windows {
            assert_eq!(
                held_as(&w, B, Client::Copilot, os, &own),
                Some((Rule::Unmanaged, Some(7101)))
            );
            continue;
        }
        assert_eq!(
            held_as(&w, B, Client::Copilot, os, &own),
            None,
            "loader and its native child exempt"
        );
        w.state();
        live::report(&w.ctx, Client::Copilot, 7100, 2100, B, "resume").unwrap();
        assert_eq!(
            held_as(&w, B, Client::Copilot, os, &own),
            Some((Rule::Caller, Some(7100)))
        );
    }
}
