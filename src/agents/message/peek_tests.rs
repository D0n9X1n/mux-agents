use serde_json::{Value, json};

use crate::agents::core_fixture as cf;
use crate::agents::fixture::{ID, World};
use crate::agents::tree::tests::{LEADER, OTHER, child, member_world, team_record, term};
use crate::caller::Caller;

fn run(w: &World, tokens: &[&str]) -> (u8, Value) {
    let argv: Vec<String> = tokens.iter().map(|t| t.to_string()).collect();
    let (code, out) = crate::cli::run(&argv, || Ok(&w.ctx));
    (
        code,
        serde_json::from_str(out.trim()).unwrap_or(Value::Null),
    )
}
// Member record `r`'s pane runs its recorded launch; a capture answers two lines.
fn view(w: &World, r: &Value) {
    let pane = r["pane"].as_str().unwrap();
    let mut row = crate::agents::fixture::row_of(&format!("{pane}|@5|300||"), &[]);
    row.command = r["startCommand"].as_str().unwrap().into();
    w.pane_rows(&[row]);
    w.fakes.mux_rule("show-buffer", 0, "one\ntwo\n", "");
}
// Member record `r`'s pane runs `command`.
fn rows(w: &World, r: &Value, command: &str) {
    let mut row =
        crate::agents::fixture::row_of(&format!("{}|@5|300||", r["pane"].as_str().unwrap()), &[]);
    row.command = command.into();
    w.pane_rows(&[row]);
}
fn person_team() -> World {
    let w = World::human("");
    w.state();
    team_record(&w, &[]);
    w
}
// The multiplexer calls made since `mark` (a count of `mux_calls` taken before the invocation).
fn since(w: &World, mark: usize) -> Vec<Vec<String>> {
    w.fakes.mux_calls()[mark..].to_vec()
}

// A captured view: its pane, its recorded launch command and the lines asked for.
type Seen<'a> = Option<(&'a str, &'a str, i32)>;

const HEX: &str = "<hex>";

// N4, PM call 10 (checklist 6, 7): nothing typed, focused or granted. Every multiplexer call of one `peek` (`calls`,
// that invocation's own) equals, token for token, a read spelled out here, never built by the code under test: the
// caller probe, the session's pane list and, when `seen` names a live view, its bound capture into a private buffer
// and that buffer's show and delete. Only the 32-hex list marker and buffer name vary. A `;` or any other command in
// any position fails; so does an hc `read`.
fn typed_nothing(w: &World, calls: &[Vec<String>], caller: &Caller, seen: Seen<'_>) {
    assert!(
        !w.fakes
            .hc_calls()
            .iter()
            .any(|(_, argv, _)| argv.iter().any(|s| s == "read")),
        "an hc read"
    );
    let sv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
    let probe = "#{pid}|#{pane_pid}|#{session_id}|#{window_id}|#{@agent-id}";
    let fields = [
        "#{pane_id}",
        "#{window_id}",
        "#{pane_pid}",
        "#{pane_dead}",
        "#{pane_start_command}",
        "#{@agent-id}",
        "#{@agent-name}",
    ];
    let list = format!("{HEX}{}", fields.map(|f| format!("{f}{HEX}")).concat());
    let mut allowed = vec![
        sv(&["-N", "display-message", "-p", "-t", &caller.pane, probe]),
        sv(&["-N", "list-panes", "-s", "-t", &caller.session, "-F", &list]),
    ];
    if let Some((pane, launch, lines)) = seen {
        let live =
            format!("#{{&&:#{{==:#{{pane_dead}},0}},#{{==:#{{pane_start_command}},{launch}}}}}");
        let buffer = format!("mux-agents-{HEX}");
        let capture = format!("capture-pane -J -t {pane} -b {buffer} -S -{lines}");
        allowed.push(sv(&["-N", "if-shell", "-F", "-t", pane, &live, &capture]));
        allowed.push(sv(&["-N", "show-buffer", "-b", &buffer]));
        allowed.push(sv(&["-N", "delete-buffer", "-b", &buffer]));
    }
    for call in calls {
        assert!(
            allowed.iter().any(|a| fits(a, call)),
            "not one of the reads of peek: {call:?}"
        );
    }
}

// Whether `call` is `pattern` token for token, each `<hex>` exactly 32 lowercase hex digits.
fn fits(pattern: &[String], call: &[String]) -> bool {
    pattern.len() == call.len() && pattern.iter().zip(call).all(|(p, c)| field(p, c))
}
fn field(pattern: &str, actual: &str) -> bool {
    let mut parts = pattern.split(HEX);
    let Some(mut rest) = actual.strip_prefix(parts.next().unwrap_or_default()) else {
        return false;
    };
    for part in parts {
        let Some(hex) = rest.get(..32) else {
            return false;
        };
        if !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return false;
        }
        let Some(after) = rest[32..].strip_prefix(part) else {
            return false;
        };
        rest = after;
    }
    rest.is_empty()
}

#[test]
fn a_person_peeks_and_nothing_is_typed() {
    let w = person_team();
    let r = cf::member(&w, ID, json!({}));
    view(&w, &r);
    let (c, launch) = (w.caller(), r["startCommand"].as_str().unwrap().to_string());
    let mark = w.fakes.mux_calls().len();
    let (code, got) = run(&w, &["peek", "worker"]);
    assert_eq!(
        (code, &got["pane"], &got["text"]),
        (0, &json!("%9"), &json!("one\ntwo")),
        "{got}"
    );
    let capture = w
        .fakes
        .mux_calls()
        .into_iter()
        .flatten()
        .find(|s| s.contains("capture-pane"))
        .unwrap();
    assert!(capture.contains("-S -60"), "default 60 lines: {capture}");
    typed_nothing(&w, &since(&w, mark), &c, Some(("%9", &launch, 60)));
    for lines in ["0", "2001", "x"] {
        let mark = w.fakes.mux_calls().len();
        assert_eq!(
            run(&w, &["peek", "worker", "-Lines", lines]).1["check"],
            "usage",
            "{lines}"
        );
        typed_nothing(&w, &since(&w, mark), &c, None);
    }
    let outside = World::human("");
    let oc = outside.caller();
    let mark = outside.fakes.mux_calls().len();
    let (code, r) = run(&outside, &["peek", "worker"]);
    assert_eq!(
        (code, &r["check"], &r["message"]),
        (2, &json!("team"), &json!("peek works only inside a team"))
    );
    typed_nothing(&outside, &since(&outside, mark), &oc, None);
}

// N4: a valid `-Lines` reaches the capture and trims the answer.
#[test]
fn a_valid_lines_override_reaches_the_capture() {
    for (lines, text) in [("2000", "one\ntwo"), ("1", "two")] {
        let w = person_team();
        let r = cf::member(&w, ID, json!({}));
        view(&w, &r);
        let (c, launch) = (w.caller(), r["startCommand"].as_str().unwrap().to_string());
        let mark = w.fakes.mux_calls().len();
        let (code, got) = run(&w, &["peek", "worker", "-Lines", lines]);
        assert_eq!((code, &got["text"]), (0, &json!(text)), "{lines}: {got}");
        let capture = w
            .fakes
            .mux_calls()
            .into_iter()
            .flatten()
            .find(|s| s.contains("capture-pane"))
            .unwrap();
        assert!(
            capture.ends_with(&format!("-S -{lines}")),
            "{lines}: {capture}"
        );
        typed_nothing(
            &w,
            &since(&w, mark),
            &c,
            Some(("%9", &launch, lines.parse().unwrap())),
        );
    }
}

// N4: names resolve in the caller's team only: another team's member is no agent of this team, to a person and to
// a member alike, and nothing is captured or typed.
#[test]
fn another_teams_member_is_no_agent_of_this_team() {
    let far = json!({"name": "far", "session": "$8"});
    let want = (
        2,
        json!("target"),
        json!("no agent of this team has that name"),
    );
    let w = person_team();
    cf::member(&w, "cccccccccccc", far.clone());
    let c = w.caller();
    let mark = w.fakes.mux_calls().len();
    let (code, r) = run(&w, &["peek", "far"]);
    assert_eq!(
        (code, r["check"].clone(), r["message"].clone()),
        want,
        "a person"
    );
    typed_nothing(&w, &since(&w, mark), &c, None);
    let mut m = member_world();
    cf::member(&m.w, "cccccccccccc", far);
    m.hc();
    m.rounds(3, &term(&m.r, Some(150)));
    let mark = m.w.fakes.mux_calls().len();
    let (code, r) = run(&m.w, &["peek", "far"]);
    assert_eq!(
        (code, r["check"].clone(), r["message"].clone()),
        want,
        "a member"
    );
    typed_nothing(&m.w, &since(&m.w, mark), &m.c, None);
    for w in [&w, &m.w] {
        assert!(
            !w.fakes
                .mux_calls()
                .iter()
                .flatten()
                .any(|s| s.contains("capture-pane"))
        );
    }
}

// Ambiguity 9: a sibling, a broken parent chain and a cycle all refuse with the one peek message, before any capture,
// and type nothing.
#[test]
fn a_sibling_a_broken_chain_and_a_cycle_cannot_peek() {
    for case in ["sibling", "broken", "cycle"] {
        let mut m = member_world();
        let mut other = m.record(OTHER);
        match case {
            "broken" => other["parentAgent"]["id"] = json!("dddddddddddd"),
            "cycle" => {
                cf::member(&m.w, "eeeeeeeeeeee", child("eeeeeeeeeeee", OTHER, 2));
                other["parentAgent"]["id"] = json!("eeeeeeeeeeee");
            }
            _ => {}
        }
        crate::agents::fixture::write(&m.folder(OTHER).join("record.json"), other);
        m.hc();
        m.rounds(3, &term(&m.r, Some(150)));
        let mark = m.w.fakes.mux_calls().len();
        let (code, r) = run(&m.w, &["peek", "mbb"]);
        let want = (
            2,
            json!("authority"),
            json!("only an ancestor or a person can peek mbb"),
        );
        assert_eq!(
            (code, r["check"].clone(), r["message"].clone()),
            want,
            "{case}: {r}"
        );
        typed_nothing(&m.w, &since(&m.w, mark), &m.c, None);
        assert!(
            !m.w.fakes
                .mux_calls()
                .iter()
                .flatten()
                .any(|s| s.contains("capture-pane")),
            "{case}"
        );
        m.w.fakes.hc_done().unwrap();
    }
}

// N4: an ancestor two levels up passes; a child peeking its parent (the leader) is refused; neither types.
#[test]
fn a_grandparent_peeks_and_a_child_cannot() {
    let (d, e) = ("dddddddddddd", "eeeeeeeeeeee");
    let mut m = member_world();
    cf::member(&m.w, d, child(d, ID, 2));
    let grand = cf::member(&m.w, e, child(e, d, 3));
    view(&m.w, &grand);
    let (pane, launch) = (
        grand["pane"].as_str().unwrap(),
        grand["startCommand"].as_str().unwrap(),
    );
    let leader = m.record(LEADER)["name"].as_str().unwrap().to_string();
    m.hc();
    m.rounds(3, &term(&m.r, Some(150)));
    let mark = m.w.fakes.mux_calls().len();
    let (code, r) = run(&m.w, &["peek", "mee"]);
    assert_eq!(
        (code, &r["text"]),
        (0, &json!("one\ntwo")),
        "a grandparent: {r}"
    );
    typed_nothing(&m.w, &since(&m.w, mark), &m.c, Some((pane, launch, 60)));
    m.rounds(3, &term(&m.r, Some(150)));
    let mark = m.w.fakes.mux_calls().len();
    let (code, r) = run(&m.w, &["peek", &leader]);
    let want = format!("only an ancestor or a person can peek {leader}");
    assert_eq!(
        (code, r["check"].clone(), r["message"].clone()),
        (2, json!("authority"), json!(want))
    );
    typed_nothing(&m.w, &since(&m.w, mark), &m.c, None);
    m.w.fakes.hc_done().unwrap();
}

// N4: no view, a replaced launch, a capture whose launch check fails, and the bar's stale pane refuse; the bar's
// own pane reads (ambiguity 10: `p` is `capture_member`, verb `peek`). None of them types.
#[test]
fn no_view_a_replaced_view_and_a_stale_bar_pane_refuse() {
    let focus = "the agent has no live view; run focus worker";
    for (case, want) in [
        ("none", focus),
        ("replaced", focus),
        ("raced", "the agent's pane is no longer its launch"),
        ("stale", "the agent is no longer on the pane the bar drew"),
    ] {
        let w = person_team();
        let r = cf::member(&w, ID, json!({}));
        let launch = r["startCommand"].as_str().unwrap().to_string();
        match case {
            "none" => w.panes(&[]),
            "replaced" => rows(&w, &r, "sh"),
            "raced" => {
                rows(&w, &r, &launch);
                w.fakes.mux_rule("show-buffer", 1, "", "no buffer");
            }
            _ => view(&w, &r),
        }
        let c = w.caller();
        let mark = w.fakes.mux_calls().len();
        let got = if case == "stale" {
            assert!(
                crate::agents::message::capture_member(&w.ctx, &c, ID, Some("%9"), 200).is_ok(),
                "the bar's pane"
            );
            let e = crate::agents::message::capture_member(&w.ctx, &c, ID, Some("%8"), 200)
                .unwrap_err();
            (e.code, e.check.to_string(), e.message)
        } else {
            let (code, v) = run(&w, &["peek", "worker"]);
            let text = |k: &str| v[k].as_str().unwrap_or_default().to_string();
            (code, text("check"), text("message"))
        };
        assert_eq!(got, (2, "target".to_string(), want.to_string()), "{case}");
        let seen = match case {
            "raced" => Some(("%9", launch.as_str(), 60)),
            "stale" => Some(("%9", launch.as_str(), 200)),
            _ => None,
        };
        typed_nothing(&w, &since(&w, mark), &c, seen);
    }
}
