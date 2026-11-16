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

/// Each function whose body, with all whitespace removed (so a rustfmt-split array reads as one), passes an argument
/// array starting with `"<verb>"` (`live.run(&["resume", ..])`, `by(.., &["up", ..])`, `spawn_by`).
fn callers(src: &str, verb: &str) -> BTreeSet<String> {
    let needle = format!("&[\"{verb}\",");
    let mut bodies: Vec<(String, String)> = Vec::new();
    for line in src.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed
            .strip_prefix("fn ")
            .or_else(|| trimmed.strip_prefix("pub fn "))
        {
            bodies.push((
                rest.split('(').next().unwrap_or("").to_string(),
                String::new(),
            ));
        } else if let Some((_, body)) = bodies.last_mut() {
            body.extend(line.split_whitespace());
        }
    }
    bodies
        .into_iter()
        .filter(|(_, b)| b.contains(&needle))
        .map(|(n, _)| n)
        .collect()
}

fn resume_callers(src: &str) -> BTreeSet<String> {
    callers(src, "resume")
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
    let callers: BTreeSet<String> = SOURCES.iter().flat_map(|s| resume_callers(s)).collect();
    let helpers: Vec<_> = callers.difference(&registered).collect();
    assert!(
        helpers.is_empty(),
        "unregistered functions run resume (list the tests that call them): {helpers:?}"
    );
    let missing: Vec<_> = callers.difference(&listed).collect();
    assert!(
        missing.is_empty(),
        "registered tests running resume, not in serial_resume.txt: {missing:?}"
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
