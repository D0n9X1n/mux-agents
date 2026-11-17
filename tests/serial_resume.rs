//! occupy-3 (call 19): every registered live test that runs `resume` is in `serial_resume.txt`, and every name there is
//! a registered test. Registered: the fn after a `#[test]`, or a name in a `tests_on!(.., [..])` list (as
//! `every_mux_scenario!` registers its scenarios).
use std::collections::BTreeSet;

const SOURCES: [&str; 2] = [include_str!("live.rs"), include_str!("live_windows.rs")];

fn registered(src: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut after_test = false;
    for line in src.lines().map(str::trim) {
        if line == "#[test]" {
            after_test = true;
        } else if let Some(rest) = line.strip_prefix("fn ").filter(|_| after_test) {
            names.insert(rest.split('(').next().unwrap_or("").to_string());
            after_test = false;
        }
    }
    let flat: String = src.split_whitespace().collect();
    for call in flat.split("tests_on!(").skip(1) {
        let Some((list, _)) = call
            .split_once('[')
            .and_then(|(_, rest)| rest.split_once(']'))
        else {
            continue;
        };
        let idents = list
            .split(',')
            .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        names.extend(idents.map(String::from));
    }
    names
}

/// Each function's name and its text with all whitespace removed (so a rustfmt-split array reads as one), from its
/// declaration line on (a one-line function keeps its body, checklist 21) to the next function.
fn bodies(src: &str) -> Vec<(String, String)> {
    let mut bodies: Vec<(String, String)> = Vec::new();
    for line in src.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed
            .strip_prefix("fn ")
            .or_else(|| trimmed.strip_prefix("pub fn "))
        {
            let (name, tail) = rest.split_once('(').unwrap_or((rest, ""));
            bodies.push((name.to_string(), tail.split_whitespace().collect()));
        } else if let Some((_, body)) = bodies.last_mut() {
            body.extend(line.split_whitespace());
        }
    }
    bodies
}

/// Each function that passes an argument array starting with `"<verb>"` (`live.run(&["resume", ..])`,
/// `by(.., &["up", ..])`, `spawn_by`).
fn callers(src: &str, verb: &str) -> BTreeSet<String> {
    let needle = format!("&[\"{verb}\",");
    bodies(src)
        .into_iter()
        .filter(|(_, b)| b.contains(&needle))
        .map(|(n, _)| n)
        .collect()
}

fn resume_callers(src: &str) -> BTreeSet<String> {
    callers(src, "resume")
}

/// Whether `body` calls `name(`: the name not preceded by an identifier character (`close(` is not `lose(`).
fn calls(body: &str, name: &str) -> bool {
    let call = format!("{name}(");
    body.match_indices(&call).any(|(i, _)| {
        !body[..i]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// r6b call 8: each function that runs `up` or `restore` (with or without arguments) after a core kill (`kill_hard`,
/// directly or through a function that calls it) reconciles resumed conversations, so it runs alone too.
fn reconcile_callers(src: &str) -> BTreeSet<String> {
    let bodies = bodies(src);
    let killers: Vec<&str> = bodies
        .iter()
        .filter(|(_, b)| b.contains(".kill_hard("))
        .map(|(n, _)| n.as_str())
        .collect();
    bodies
        .iter()
        .filter(|(_, b)| b.contains(".kill_hard(") || killers.iter().any(|k| calls(b, k)))
        .filter(|(_, b)| b.contains("&[\"up\"") || b.contains("&[\"restore\""))
        .map(|(n, _)| n.clone())
        .collect()
}

/// The names `serial_resume.txt` lists (comments and blank lines skipped).
fn listed() -> BTreeSet<String> {
    include_str!("serial_resume.txt")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect()
}

#[test]
fn every_live_resume_test_is_listed() {
    let listed = listed();
    let registered: BTreeSet<String> = SOURCES.iter().flat_map(|s| registered(s)).collect();
    let callers: BTreeSet<String> = SOURCES
        .iter()
        .flat_map(|s| resume_callers(s).into_iter().chain(reconcile_callers(s)))
        .collect();
    let helpers: Vec<_> = callers.difference(&registered).collect();
    assert!(
        helpers.is_empty(),
        "unregistered functions run resume (list the tests that call them): {helpers:?}"
    );
    let missing: Vec<_> = callers.difference(&listed).collect();
    assert!(
        missing.is_empty(),
        "registered tests running resume or reconciling after a core kill, not in serial_resume.txt: {missing:?}"
    );
    let stale: Vec<_> = listed.difference(&registered).collect();
    assert!(
        stale.is_empty(),
        "listed but not a registered test: {stale:?}"
    );
}

// The parser: a rustfmt-split array is found; a scenario registered only through the macro list is registered; a
// name only in a comment, and a field read `row["resume"]`, are not.
#[test]
fn the_guard_parses_split_arrays_and_registrations() {
    let src = "    macro_rules! every_mux_scenario {\n        ($m:ident, $x:ident) => {\n            tests_on!(\n                $m,\n                $x,\n                [\n                    split_one,\n                    other,\n                ]\n            );\n        };\n    }\n    fn split_one(mux: Mux) {\n        live.run(&[\n            \"resume\",\n            \"w1\",\n        ]);\n    }\n    // commented_name\n    fn field_read() {\n        let _ = row[\"resume\"];\n    }\n    #[test]\n    fn plain() {\n        by(&live, &leader, &[\"resume\", \"b\"]);\n    }\n";
    assert_eq!(
        resume_callers(src).into_iter().collect::<Vec<_>>(),
        ["plain", "split_one"]
    );
    let reg = registered(src);
    assert!(
        reg.contains("split_one") && reg.contains("other") && reg.contains("plain"),
        "{reg:?}"
    );
    assert!(
        !reg.contains("commented_name") && !reg.contains("field_read"),
        "{reg:?}"
    );
}

// PM call 11: every registered live test that runs `up` is listed, so it runs serially.
#[test]
fn every_live_up_test_is_listed() {
    let registered: BTreeSet<String> = SOURCES.iter().flat_map(|s| registered(s)).collect();
    let ups: BTreeSet<String> = SOURCES.iter().flat_map(|s| callers(s, "up")).collect();
    let listed = listed();
    let missing: Vec<_> = ups
        .intersection(&registered)
        .filter(|n| !listed.contains(*n))
        .collect();
    assert!(
        missing.is_empty(),
        "registered live tests running up, not in serial_resume.txt: {missing:?}"
    );
}

#[test]
fn a_split_up_array_is_an_up_caller() {
    let src = r#"
fn t() {
    live.run(&[
        "up",
        "-Client",
    ]);
}
"#;
    assert_eq!(callers(src, "up"), BTreeSet::from(["t".to_string()]));
}

// r6b call 8 (checklist 21): a test that runs `up` or `restore` after a core kill (`kill_hard`, directly or through a
// function that calls it) is a reconcile caller. A one-line declaration keeps its body; `&["restore"]` with no
// arguments counts; a name only inside a longer identifier (`close(` for `lose(`) does not.
#[test]
fn a_core_kill_then_up_is_a_reconcile_caller() {
    let src = r#"
fn lose(live: &mut Live) { live.core.kill_hard(); }
#[test]
fn after_kill() { lose(&mut live); live.run(&["up", "-Client", "claude"]); }
#[test]
fn restore_bare() {
    live.core.kill_hard();
    live.run(&["restore"]);
}
#[test]
fn no_kill() { live.run(&["up", "-Client", "claude"]); }
#[test]
fn closes() { file.close(); live.run(&["up", "-Client", "claude"]); }
"#;
    assert_eq!(
        reconcile_callers(src).into_iter().collect::<Vec<_>>(),
        ["after_kill", "restore_bare"]
    );
}
