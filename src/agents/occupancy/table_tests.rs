//! occupy-3 table rules (Task 3): unreadable tables, argv, unmanaged clients, unbound rows, /proc, messages.
use super::support::held;
use super::support::*;
use super::*;

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

// H4.4: an unmanaged readable client of `kind` holds only what its argv names (H4.3) or what its own newest hook
// report names; its argv naming A proves nothing about X, and with no report X stays free. Once its hook reports X
// (keyed by its own (pid, start)), X is held as `Unmanaged`. Another kind's client holds only what its argv names.
#[test]
fn a_readable_client_of_the_kind_holds_only_what_its_argv_or_own_report_names() {
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
    assert_eq!(held(&w, X, Client::Claude), None, "no report: X is free");
    live::report(&w.ctx, Client::Claude, 7001, 1, X, "resume").unwrap();
    assert_eq!(
        held(&w, X, Client::Claude),
        Some((Rule::Unmanaged, Some(7001))),
        "its own report names X"
    );
    assert_eq!(held(&w, X, Client::Copilot), None);
    assert_eq!(held(&w, A, Client::Copilot), Some((Rule::Argv, Some(7001))));
}

// A Unix `node` whose executable path cannot be read (a Homebrew upgrade replaced it) and whose script resolves
// to no client holds nothing; one whose script cannot be resolved (client_known unknown) still holds as unbound.
#[test]
fn a_node_whose_executable_is_gone_holds_only_when_its_script_is_unknown() {
    let mcp = ["node", "/n/.bin/playwright-mcp"];
    let w = World::human("");
    w.fakes
        .process(row(Some(1), None, Some(&mcp), Some("node")));
    assert_eq!(held(&w, X, Client::Copilot), None, "an unrelated script");
    let w = World::human("");
    w.fakes
        .process(row(Some(1), None, Some(&mcp), Some("node")));
    w.fakes.client_unknown(7001);
    assert_eq!(
        held(&w, X, Client::Copilot),
        Some((Rule::Unbound, Some(7001))),
        "script unresolved"
    );
    let w = World::human("");
    w.fakes
        .process(row(Some(1), None, Some(&["claude"]), Some("claude")));
    assert_eq!(
        held(&w, X, Client::Claude),
        Some((Rule::Unbound, Some(7001))),
        "a client image still holds"
    );
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
    // An unbound row the OS names a client of `kind` from its path alone is judged as H4.4, whatever its image: by
    // its own newest report (keyed by its own (pid, start)), which here names X, so it holds X as `Unmanaged`.
    let w = World::human("");
    w.fakes.process(row(
        Some(1),
        Some("/usr/local/bin/claude"),
        None,
        Some("sleep"),
    ));
    w.fakes.client(7001, Client::Claude);
    live::report(&w.ctx, Client::Claude, 7001, 1, X, "startup").unwrap();
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

// o3 call 34 N2: a readable `node` whose client identity cannot be resolved (its script's realpath or its cwd
// unreadable) is unknown: unbound, judged by its image (`node` may be Copilot). A resolved unrelated node holds nothing.
#[test]
fn an_unresolved_node_holds_copilot_by_its_image() {
    let node = |w: &World| {
        w.fakes.process(row(
            Some(1),
            Some("/usr/bin/node"),
            Some(&["node", "app.js"]),
            Some("node"),
        ))
    };
    let w = World::human("");
    node(&w);
    assert_eq!(held(&w, X, Client::Copilot), None, "resolved: not a client");
    let w = World::human("");
    node(&w);
    w.fakes.client_unknown(7001);
    assert_eq!(
        held(&w, X, Client::Copilot),
        Some((Rule::Unbound, Some(7001)))
    );
    assert_eq!(held(&w, X, Client::Claude), None, "node is no Claude image");
}
